use super::ShellRequest;
use crate::exec::ExecCapturePolicy;
use crate::exec::ExecExpiration;
use crate::exec::cancel_when_either;
use crate::exec::is_likely_sandbox_denied;
use crate::guardian::GuardianApprovalRequest;
use crate::guardian::new_guardian_review_id;
use crate::guardian::review_approval_request;
use crate::guardian::routes_approval_to_guardian;
use crate::hook_runtime::run_permission_request_hooks;
use crate::sandboxing::ExecOptions;
use crate::sandboxing::ExecRequest;
use crate::sandboxing::SandboxPermissions;
use crate::shell::ShellType;
use crate::tools::runtimes::build_sandbox_command;
use crate::tools::runtimes::exec_env_for_sandbox_permissions;
use crate::tools::runtimes::prepend_zsh_fork_bin_to_path;
use crate::tools::sandboxing::PermissionRequestPayload;
use crate::tools::sandboxing::SandboxAttempt;
use crate::tools::sandboxing::ToolCtx;
use crate::tools::sandboxing::ToolError;
use crate::tools::sandboxing::managed_network_for_sandbox_permissions;
use crate::tools::sandboxing::sandbox_permissions_for_session;
use crate::tools::sandboxing::unsandboxed_execution_allowed;
use codex_execpolicy::Decision;
use codex_execpolicy::Evaluation;
use codex_execpolicy::MatchOptions;
use codex_execpolicy::Policy;
use codex_execpolicy::RuleMatch;
use codex_features::Feature;
use codex_hooks::PermissionRequestDecision;
use codex_protocol::config_types::WindowsSandboxLevel;
use codex_protocol::error::CodexErr;
use codex_protocol::error::SandboxErr;
use codex_protocol::exec_output::ExecToolCallOutput;
use codex_protocol::exec_output::StreamOutput;
use codex_protocol::models::AdditionalPermissionProfile;
use codex_protocol::models::PermissionProfile;
use codex_protocol::protocol::AskForApproval;
use codex_protocol::protocol::GuardianCommandSource;
use codex_protocol::protocol::NetworkPolicyRuleAction;
use codex_protocol::protocol::ReviewDecision;
use codex_sandboxing::SandboxCommand;
use codex_sandboxing::SandboxManager;
use codex_sandboxing::SandboxTransformRequest;
use codex_sandboxing::SandboxType;
use codex_sandboxing::SandboxablePreference;
use codex_sandboxing::record_filesystem_sandbox_violation;
use codex_shell_command::bash::parse_shell_lc_plain_commands;
use codex_shell_command::bash::parse_shell_lc_single_command_prefix;
use codex_shell_escalation::EscalateServer;
use codex_shell_escalation::EscalationDecision;
use codex_shell_escalation::EscalationExecution;
use codex_shell_escalation::EscalationPermissions;
use codex_shell_escalation::EscalationPolicy;
use codex_shell_escalation::EscalationPolicyFuture;
use codex_shell_escalation::EscalationSession;
use codex_shell_escalation::ExecParams;
use codex_shell_escalation::ExecResult;
use codex_shell_escalation::PreparedExec;
use codex_shell_escalation::ResolvedPermissionProfile;
use codex_shell_escalation::ShellCommandExecutor;
use codex_shell_escalation::ShellCommandExecutorFuture;
use codex_shell_escalation::Stopwatch;
use codex_utils_absolute_path::AbsolutePathBuf;
use codex_utils_path_uri::PathUri;
use std::collections::HashMap;
use std::io;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::RwLock;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

pub(crate) struct PreparedUnifiedExecZshFork {
    pub(crate) exec_request: ExecRequest,
    pub(crate) escalation_session: EscalationSession,
}

const PROMPT_CONFLICT_REASON: &str =
    "approval required by policy, but AskForApproval is set to Never";
const REJECT_SANDBOX_APPROVAL_REASON: &str =
    "approval required by policy, but AskForApproval::Granular.sandbox_approval is false";
const REJECT_RULES_APPROVAL_REASON: &str =
    "approval required by policy rule, but AskForApproval::Granular.rules is false";
fn approval_sandbox_permissions(
    sandbox_permissions: SandboxPermissions,
    additional_permissions_preapproved: bool,
) -> SandboxPermissions {
    if additional_permissions_preapproved
        && matches!(
            sandbox_permissions,
            SandboxPermissions::WithAdditionalPermissions
        )
    {
        SandboxPermissions::UseDefault
    } else {
        sandbox_permissions
    }
}

pub(super) async fn try_run_zsh_fork(
    req: &ShellRequest,
    attempt: &SandboxAttempt<'_>,
    ctx: &ToolCtx,
    command: &[String],
) -> Result<Option<ExecToolCallOutput>, ToolError> {
    let Some(shell_zsh_path) = ctx.session.services.shell_zsh_path.as_ref() else {
        tracing::warn!("ZshFork backend specified, but shell_zsh_path is not configured.");
        return Ok(None);
    };
    if !ctx.session.features().enabled(Feature::ShellZshFork) {
        tracing::warn!("ZshFork backend specified, but ShellZshFork feature is not enabled.");
        return Ok(None);
    }
    if !matches!(ctx.session.user_shell().shell_type, ShellType::Zsh) {
        tracing::warn!("ZshFork backend specified, but user shell is not Zsh.");
        return Ok(None);
    }

    let (attempt_file_system_sandbox_policy, _) = attempt.permissions.to_runtime_permissions();
    let sandbox_permissions = sandbox_permissions_for_session(
        req.sandbox_permissions,
        &attempt_file_system_sandbox_policy,
        &ctx.session,
    );
    let req = &ShellRequest {
        sandbox_permissions,
        ..req.clone()
    };
    let mut env = exec_env_for_sandbox_permissions(&req.env, req.sandbox_permissions);
    prepend_zsh_fork_bin_to_path(&mut env, shell_zsh_path);
    // D447: as `ShellRuntime::run` does for the other backends; the snapshot
    // overrides are already in `command`.
    if crate::security::git_ext_guard::applies(&ctx.session) {
        crate::security::git_ext_guard::apply(&mut env, &mut std::collections::HashMap::new());
    }
    let command =
        build_sandbox_command(command, &req.cwd, &env, req.additional_permissions.clone())?;
    let options = ExecOptions {
        expiration: req.timeout_ms.into(),
        capture_policy: ExecCapturePolicy::ShellTool,
    };
    let sandbox_exec_request = attempt
        .env_for(
            command,
            options,
            managed_network_for_sandbox_permissions(req.network.as_ref(), req.sandbox_permissions),
            Some(&req.turn_environment.environment_id),
        )
        .map_err(ToolError::Codex)?;
    let crate::sandboxing::ExecRequest {
        command,
        cwd: sandbox_cwd,
        env: sandbox_env,
        exec_server_env_config: _,
        network: sandbox_network,
        network_environment_id,
        expiration: _sandbox_expiration,
        capture_policy: _capture_policy,
        sandbox,
        windows_sandbox_policy_cwd: sandbox_policy_cwd,
        windows_sandbox_workspace_roots,
        windows_sandbox_level,
        windows_sandbox_private_desktop: _windows_sandbox_private_desktop,
        permission_profile,
        windows_sandbox_filesystem_overrides: _windows_sandbox_filesystem_overrides,
        arg0,
        exec_server_sandbox: _,
        exec_server_enforce_managed_network: _,
        exec_server_managed_network: _,
        exec_server_network_proxy: _,
    } = sandbox_exec_request;
    let ParsedShellCommand { script, login, .. } = extract_shell_script(&command)?;
    let effective_timeout = Duration::from_millis(
        req.timeout_ms
            .unwrap_or(crate::exec::DEFAULT_EXEC_COMMAND_TIMEOUT_MS),
    );
    let live_aggressive = ctx
        .session
        .services
        .model_client()
        .live_level_is_aggressive();
    let exec_policy = Arc::new(RwLock::new(
        ctx.session
            .services
            .exec_policy
            .current_for(live_aggressive)
            .as_ref()
            .clone(),
    ));
    // TODO(anp): Keep PathUri through the shell escalation boundary.
    let sandbox_cwd = sandbox_cwd
        .to_abs_path()
        .map_err(|err| ToolError::Rejected(err.to_string()))?;
    // TODO(anp): Keep PathUri through the shell sandbox policy boundary.
    let sandbox_policy_cwd = sandbox_policy_cwd
        .to_abs_path()
        .map_err(|err| ToolError::Rejected(err.to_string()))?;
    let command_executor = CoreShellCommandExecutor {
        command,
        cwd: sandbox_cwd,
        permission_profile,
        sandbox,
        env: sandbox_env,
        network: sandbox_network,
        network_environment_id,
        windows_sandbox_level,
        arg0,
        sandbox_policy_cwd,
        windows_sandbox_workspace_roots,
        codex_linux_sandbox_exe: ctx.turn.config.codex_linux_sandbox_exe.clone(),
        use_legacy_landlock: ctx.turn.config.features.use_legacy_landlock(),
    };
    let main_execve_wrapper_exe = ctx
        .session
        .services
        .main_execve_wrapper_exe
        .clone()
        .ok_or_else(|| {
            ToolError::Rejected(
                "zsh fork feature enabled, but execve wrapper is not configured".to_string(),
            )
        })?;
    let exec_params = ExecParams {
        command: script,
        workdir: req.cwd.to_string_lossy().to_string(),
        timeout_ms: Some(effective_timeout.as_millis() as u64),
        login: Some(login),
    };

    // Note that Stopwatch starts immediately upon creation, so currently we try
    // to minimize the time between creating the Stopwatch and starting the
    // escalation server.
    let stopwatch = Stopwatch::new(effective_timeout);
    let mut cancel_token = stopwatch.cancellation_token();
    if let Some(cancellation) = attempt.network_denial_cancellation_token.clone() {
        cancel_token = cancel_when_either(cancel_token, cancellation);
    }
    let approval_sandbox_permissions = approval_sandbox_permissions(
        req.sandbox_permissions,
        req.additional_permissions_preapproved,
    );
    let escalation_policy = CoreShellActionProvider {
        policy: Arc::clone(&exec_policy),
        live_aggressive,
        strict_rules: ctx
            .session
            .services
            .exec_policy
            .strict_rules_for(live_aggressive),
        session: Arc::clone(&ctx.session),
        turn: Arc::clone(&ctx.turn),
        call_id: ctx.call_id.clone(),
        environment_id: req.turn_environment.environment_id.clone(),
        tool_name: GuardianCommandSource::Shell,
        approval_policy: ctx.turn.approval_policy.value(),
        permission_profile: command_executor.permission_profile.clone(),
        sandbox_permissions: req.sandbox_permissions,
        approval_sandbox_permissions,
        prompt_permissions: req.additional_permissions.clone(),
        stopwatch: stopwatch.clone(),
    };

    let escalate_server = EscalateServer::new(
        shell_zsh_path.clone(),
        main_execve_wrapper_exe,
        escalation_policy,
    );

    let exec_result = escalate_server
        .exec(exec_params, cancel_token, Arc::new(command_executor))
        .await
        .map_err(|err| ToolError::Rejected(err.to_string()))?;

    map_exec_result(attempt.sandbox, exec_result).map(Some)
}

pub(crate) async fn prepare_unified_exec_zsh_fork(
    req: &crate::tools::runtimes::unified_exec::UnifiedExecRequest,
    _attempt: &SandboxAttempt<'_>,
    ctx: &ToolCtx,
    exec_request: ExecRequest,
    shell_zsh_path: &std::path::Path,
    main_execve_wrapper_exe: &std::path::Path,
) -> Result<Option<PreparedUnifiedExecZshFork>, ToolError> {
    let parsed = match extract_shell_script(&exec_request.command) {
        Ok(parsed) => parsed,
        Err(err) => {
            tracing::warn!("ZshFork unified exec fallback: {err:?}");
            return Ok(None);
        }
    };
    if parsed.program != shell_zsh_path.to_string_lossy() {
        tracing::warn!(
            "ZshFork backend specified, but unified exec command targets `{}` instead of `{}`.",
            parsed.program,
            shell_zsh_path.display(),
        );
        return Ok(None);
    }

    let live_aggressive = ctx
        .session
        .services
        .model_client()
        .live_level_is_aggressive();
    let exec_policy = Arc::new(RwLock::new(
        ctx.session
            .services
            .exec_policy
            .current_for(live_aggressive)
            .as_ref()
            .clone(),
    ));
    // TODO(anp): Keep PathUri through the zsh-fork executor boundary.
    let cwd = exec_request
        .cwd
        .to_abs_path()
        .map_err(|err| ToolError::Rejected(err.to_string()))?;
    // TODO(anp): Keep PathUri through the zsh-fork sandbox policy boundary.
    let sandbox_policy_cwd = exec_request
        .windows_sandbox_policy_cwd
        .to_abs_path()
        .map_err(|err| ToolError::Rejected(err.to_string()))?;
    let command_executor = CoreShellCommandExecutor {
        command: exec_request.command.clone(),
        cwd,
        permission_profile: exec_request.permission_profile.clone(),
        sandbox: exec_request.sandbox,
        env: exec_request.env.clone(),
        network: exec_request.network.clone(),
        network_environment_id: exec_request.network_environment_id.clone(),
        windows_sandbox_level: exec_request.windows_sandbox_level,
        arg0: exec_request.arg0.clone(),
        sandbox_policy_cwd,
        windows_sandbox_workspace_roots: exec_request.windows_sandbox_workspace_roots.clone(),
        codex_linux_sandbox_exe: ctx.turn.config.codex_linux_sandbox_exe.clone(),
        use_legacy_landlock: ctx.turn.config.features.use_legacy_landlock(),
    };
    let escalation_policy = CoreShellActionProvider {
        policy: Arc::clone(&exec_policy),
        live_aggressive,
        strict_rules: ctx
            .session
            .services
            .exec_policy
            .strict_rules_for(live_aggressive),
        session: Arc::clone(&ctx.session),
        turn: Arc::clone(&ctx.turn),
        call_id: ctx.call_id.clone(),
        environment_id: req.turn_environment.environment_id.clone(),
        tool_name: GuardianCommandSource::UnifiedExec,
        approval_policy: ctx.turn.approval_policy.value(),
        permission_profile: exec_request.permission_profile.clone(),
        sandbox_permissions: req.sandbox_permissions,
        approval_sandbox_permissions: approval_sandbox_permissions(
            req.sandbox_permissions,
            req.additional_permissions_preapproved,
        ),
        prompt_permissions: req.additional_permissions.clone(),
        stopwatch: Stopwatch::unlimited(),
    };

    let escalate_server = EscalateServer::new(
        shell_zsh_path.to_path_buf(),
        main_execve_wrapper_exe.to_path_buf(),
        escalation_policy,
    );
    let escalation_session = escalate_server
        .start_session(CancellationToken::new(), Arc::new(command_executor))
        .map_err(|err| ToolError::Rejected(err.to_string()))?;
    let mut exec_request = exec_request;
    exec_request.env.extend(escalation_session.env().clone());
    Ok(Some(PreparedUnifiedExecZshFork {
        exec_request,
        escalation_session,
    }))
}

struct CoreShellActionProvider {
    policy: Arc<RwLock<Policy>>,
    /// Whether `policy` is the Aggressive one (#626: remembered approvals
    /// suspended), so a prompt says which were not applied.
    live_aggressive: bool,
    /// See `ExecPolicyManager::load`.
    strict_rules: bool,
    session: Arc<crate::session::session::Session>,
    turn: Arc<crate::session::turn_context::TurnContext>,
    call_id: String,
    environment_id: String,
    tool_name: GuardianCommandSource,
    approval_policy: AskForApproval,
    permission_profile: PermissionProfile,
    sandbox_permissions: SandboxPermissions,
    approval_sandbox_permissions: SandboxPermissions,
    prompt_permissions: Option<AdditionalPermissionProfile>,
    stopwatch: Stopwatch,
}

#[allow(clippy::large_enum_variant)]
enum DecisionSource {
    PrefixRule,
    /// Often, this is `is_safe_command()`.
    UnmatchedCommandFallback,
}

fn execve_prompt_is_rejected_by_policy(
    approval_policy: AskForApproval,
    decision_source: &DecisionSource,
) -> Option<&'static str> {
    match (approval_policy, decision_source) {
        (AskForApproval::Never, _) => Some(PROMPT_CONFLICT_REASON),
        (AskForApproval::Granular(granular_config), DecisionSource::PrefixRule)
            if !granular_config.allows_rules_approval() =>
        {
            Some(REJECT_RULES_APPROVAL_REASON)
        }
        (AskForApproval::Granular(granular_config), DecisionSource::UnmatchedCommandFallback)
            if !granular_config.allows_sandbox_approval() =>
        {
            Some(REJECT_SANDBOX_APPROVAL_REASON)
        }
        _ => None,
    }
}

impl CoreShellActionProvider {
    fn decision_driven_by_policy(matched_rules: &[RuleMatch], decision: Decision) -> bool {
        matched_rules.iter().any(|rule_match| {
            !matches!(rule_match, RuleMatch::HeuristicsRuleMatch { .. })
                && rule_match.decision() == decision
        })
    }

    fn shell_request_escalation_execution(
        sandbox_permissions: SandboxPermissions,
        permission_profile: &PermissionProfile,
        additional_permissions: Option<&AdditionalPermissionProfile>,
    ) -> EscalationExecution {
        match sandbox_permissions {
            SandboxPermissions::UseDefault => EscalationExecution::TurnDefault,
            SandboxPermissions::RequireEscalated => {
                if unsandboxed_execution_allowed(&permission_profile.file_system_sandbox_policy()) {
                    EscalationExecution::Unsandboxed
                } else {
                    EscalationExecution::TurnDefault
                }
            }
            SandboxPermissions::WithAdditionalPermissions => additional_permissions
                .map(|_| {
                    // Shell request additional permissions were already normalized and
                    // merged into the first-attempt sandbox policy.
                    EscalationExecution::Permissions(
                        EscalationPermissions::ResolvedPermissionProfile(
                            ResolvedPermissionProfile {
                                permission_profile: permission_profile.clone(),
                            },
                        ),
                    )
                })
                .unwrap_or(EscalationExecution::TurnDefault),
        }
    }

    async fn prompt(
        &self,
        program: &AbsolutePathBuf,
        argv: &[String],
        workdir: &AbsolutePathBuf,
        stopwatch: &Stopwatch,
        additional_permissions: Option<AdditionalPermissionProfile>,
        reason: Option<String>,
    ) -> anyhow::Result<ReviewDecision> {
        let command = join_program_and_argv(program, argv);
        let workdir = workdir.clone();
        let session = self.session.clone();
        let turn = self.turn.clone();
        let call_id = self.call_id.clone();
        let approval_id = Some(Uuid::new_v4().to_string());
        let environment_id = Some(self.environment_id.clone());
        let source = self.tool_name;
        let guardian_review_id = routes_approval_to_guardian(&turn).then(new_guardian_review_id);
        Ok(stopwatch
            .pause_for(async move {
                // 1) Run PermissionRequest hooks
                let permission_request = PermissionRequestPayload::bash(
                    codex_shell_command::parse_command::shlex_join(&command),
                    /*description*/ None,
                );
                let effective_approval_id = approval_id.clone().unwrap_or_else(|| call_id.clone());
                match run_permission_request_hooks(
                    &session,
                    &turn,
                    &effective_approval_id,
                    permission_request,
                )
                .await
                {
                    Some(PermissionRequestDecision::Allow) => {
                        return ReviewDecision::Approved;
                    }
                    Some(PermissionRequestDecision::Deny { message }) => {
                        return ReviewDecision::denied(message);
                    }
                    None => {}
                }

                // 2) Route to Guardian if configured
                if let Some(review_id) = guardian_review_id {
                    return review_approval_request(
                        &session,
                        &turn,
                        review_id,
                        GuardianApprovalRequest::Execve {
                            id: call_id.clone(),
                            source,
                            program: program.to_string_lossy().into_owned(),
                            argv: argv.to_vec(),
                            cwd: workdir.clone(),
                            additional_permissions,
                        },
                        /*retry_reason*/ None,
                    )
                    .await;
                }

                // 3) Fall back to regular user prompt
                session
                    .request_command_approval(
                        &turn,
                        call_id,
                        approval_id,
                        environment_id,
                        command,
                        workdir.clone(),
                        reason,
                        /*network_approval_context*/ None,
                        /*proposed_execpolicy_amendment*/ None,
                        additional_permissions,
                        Some(vec![ReviewDecision::Approved, ReviewDecision::Abort]),
                        /*plugin_attribution_override*/ None,
                    )
                    .await
            })
            .await)
    }

    #[allow(clippy::too_many_arguments)]
    async fn process_decision(
        &self,
        decision: Decision,
        needs_escalation: bool,
        program: &AbsolutePathBuf,
        argv: &[String],
        workdir: &AbsolutePathBuf,
        prompt_permissions: Option<AdditionalPermissionProfile>,
        escalation_execution: EscalationExecution,
        decision_source: DecisionSource,
        suspended_note: Option<String>,
        asked_again: bool,
    ) -> anyhow::Result<EscalationDecision> {
        let action = match decision {
            Decision::Forbidden => {
                EscalationDecision::deny(Some("Execution forbidden by policy".to_string()))
            }
            Decision::Prompt => {
                if execve_prompt_is_rejected_by_policy(self.approval_policy, &decision_source)
                    .is_some()
                {
                    EscalationDecision::deny(Some(match suspended_note {
                        Some(note) => format!("Execution forbidden by policy. {note}"),
                        None => "Execution forbidden by policy".to_string(),
                    }))
                } else {
                    // D50: a prompt asked again says why.
                    let reason = if asked_again {
                        Some(match suspended_note {
                            Some(note) => {
                                format!("{} {note}", crate::tools::approvals::ASKED_AGAIN_NOTE)
                            }
                            None => crate::tools::approvals::ASKED_AGAIN_NOTE.to_string(),
                        })
                    } else {
                        suspended_note
                    };
                    let decision = self
                        .prompt(
                            program,
                            argv,
                            workdir,
                            &self.stopwatch,
                            prompt_permissions,
                            reason,
                        )
                        .await?;
                    match decision {
                        ReviewDecision::Approved
                        | ReviewDecision::ApprovedForSession
                        | ReviewDecision::ApprovedExecpolicyAmendment { .. } => {
                            if needs_escalation {
                                EscalationDecision::escalate(escalation_execution.clone())
                            } else {
                                EscalationDecision::run()
                            }
                        }
                        ReviewDecision::NetworkPolicyAmendment {
                            network_policy_amendment,
                        } => match network_policy_amendment.action {
                            NetworkPolicyRuleAction::Allow => {
                                if needs_escalation {
                                    EscalationDecision::escalate(escalation_execution.clone())
                                } else {
                                    EscalationDecision::run()
                                }
                            }
                            NetworkPolicyRuleAction::Deny => {
                                EscalationDecision::deny(Some("User denied execution".to_string()))
                            }
                        },
                        ReviewDecision::Denied { rejection } => {
                            EscalationDecision::deny(Some(rejection))
                        }
                        ReviewDecision::TimedOut => EscalationDecision::deny(Some(
                            crate::guardian::guardian_timeout_message(),
                        )),
                        ReviewDecision::Abort => {
                            EscalationDecision::deny(Some("User cancelled execution".to_string()))
                        }
                    }
                }
            }
            Decision::Allow => {
                if needs_escalation {
                    EscalationDecision::escalate(escalation_execution)
                } else {
                    EscalationDecision::run()
                }
            }
        };
        tracing::debug!(
            "Policy decision for command {program:?} is {decision:?}, leading to escalation action {action:?}",
        );
        Ok(action)
    }
}

/// D50 (#1001): how often an intercepted exec whose approval went stale is
/// decided again.
const MAX_STALE_EXEC_APPROVAL_ROUNDS: usize = 3;

// Shell-wrapper parsing is weaker than direct exec interception because it can
// only see the script text, not the final resolved executable path. Keep it
// disabled by default so path-sensitive rules rely on the later authoritative
// execve interception.
const ENABLE_INTERCEPTED_EXEC_POLICY_SHELL_WRAPPER_PARSING: bool = false;

impl CoreShellActionProvider {
    /// D50 (#1001): an exec approved (or decided) before a security
    /// transition was committed is decided again, the person asked again, and
    /// refused when that keeps happening, or at once when the transition
    /// switched Aggressive on (this shell's rules are for its start level).
    async fn determine_fresh_action(
        &self,
        program: &AbsolutePathBuf,
        argv: &[String],
        workdir: &AbsolutePathBuf,
    ) -> anyhow::Result<EscalationDecision> {
        for round in 1..=MAX_STALE_EXEC_APPROVAL_ROUNDS {
            let decided_under = self.session.services.authority_marker();
            let action = self
                .determine_action(program, argv, workdir, /*asked_again*/ round > 1)
                .await?;
            if matches!(action, EscalationDecision::Deny { .. })
                || !self.session.services.transition_since(decided_under)
            {
                return Ok(action);
            }
            tracing::info!(
                target: "codex_core::security::transition",
                "intercepted exec approved before a security transition; not used"
            );
            // The rules this shell checks execs against were chosen for the
            // level when it started; under Aggressive they are stale.
            if self
                .session
                .services
                .model_client()
                .live_level_is_aggressive()
                != self.live_aggressive
            {
                return Ok(EscalationDecision::deny(Some(
                    crate::tools::orchestrator::STALE_AGGRESSIVE_REFUSAL.to_string(),
                )));
            }
            // D51: nor with permissions read before it (grants end there).
            if self.prompt_permissions.is_some() {
                return Ok(EscalationDecision::deny(Some(
                    crate::tools::orchestrator::STALE_PERMISSIONS_REFUSAL.to_string(),
                )));
            }
        }
        Ok(EscalationDecision::deny(Some(
            crate::tools::orchestrator::STALE_APPROVAL_REFUSAL.to_string(),
        )))
    }

    async fn determine_action(
        &self,
        program: &AbsolutePathBuf,
        argv: &[String],
        workdir: &AbsolutePathBuf,
        asked_again: bool,
    ) -> anyhow::Result<EscalationDecision> {
        tracing::debug!(
            "Determining escalation action for command {program:?} with args {argv:?} in {workdir:?}"
        );
        // #623: under Aggressive, no program a script starts may create a
        // link to the project's `.codex` or `.agents` (or Corbanu's files).
        if let Some(refusal) = crate::security::protected_links::refusal(
            &self.session,
            &self.turn,
            &join_program_and_argv(program, argv),
            workdir.as_path(),
        )
        .await
        {
            return Ok(EscalationDecision::deny(Some(refusal)));
        }

        let policy_context = || InterceptedExecPolicyContext {
            approval_policy: self.approval_policy,
            permission_profile: self.permission_profile.clone(),
            windows_sandbox_level: self.turn.windows_sandbox_level,
            sandbox_permissions: self.approval_sandbox_permissions,
            enable_shell_wrapper_parsing: ENABLE_INTERCEPTED_EXEC_POLICY_SHELL_WRAPPER_PARSING,
        };
        let evaluation = {
            let policy = self.policy.read().await;
            let mut evaluation =
                evaluate_intercepted_exec_policy(&policy, program, argv, policy_context());
            if self.strict_rules {
                crate::exec_policy::apply_strict_forbidden_matches(
                    &mut evaluation,
                    &policy,
                    &join_program_and_argv(program, argv),
                );
            }
            evaluation
        };
        // #626: as for a shell command, a prompt under Aggressive names the
        // remembered approvals that would have run this program unasked.
        let suspended_note = (self.live_aggressive && evaluation.decision == Decision::Prompt)
            .then(|| {
                self.session
                    .services
                    .exec_policy
                    .aggressive_suspended_note(|policy| {
                        evaluate_intercepted_exec_policy(policy, program, argv, policy_context())
                    })
            })
            .flatten();
        // When true, means the Evaluation was due to *.rules, not the
        // fallback function.
        let decision_driven_by_policy =
            Self::decision_driven_by_policy(&evaluation.matched_rules, evaluation.decision);
        // Issue #540: where the protected-path rules apply, an intercepted
        // exec leaves the sandbox only within a request a person approved to
        // run outside it, never through an allow rule (as in the
        // orchestrator). #747: under Moderate, never; #739: nor under
        // Permissive with protected launch armed, which refuses such a run
        // (unlike the orchestrator, no note says so: zsh-fork is off by
        // default).
        let always_sandboxed =
            crate::security::protected_additional::keeps_sandbox_always(&self.session);
        let unsandboxed_allowed =
            unsandboxed_execution_allowed(&self.permission_profile.file_system_sandbox_policy())
                && !always_sandboxed
                && (self.sandbox_permissions == SandboxPermissions::RequireEscalated
                    || crate::security::protected_additional::protected_level(&self.session)
                        .is_none());
        let needs_escalation = match self.sandbox_permissions {
            SandboxPermissions::UseDefault => unsandboxed_allowed && decision_driven_by_policy,
            SandboxPermissions::RequireEscalated => unsandboxed_allowed,
            SandboxPermissions::WithAdditionalPermissions => true,
        };

        let decision_source = if decision_driven_by_policy {
            DecisionSource::PrefixRule
        } else {
            DecisionSource::UnmatchedCommandFallback
        };
        let escalation_execution = match decision_source {
            DecisionSource::PrefixRule if unsandboxed_allowed => EscalationExecution::Unsandboxed,
            DecisionSource::PrefixRule => EscalationExecution::TurnDefault,
            DecisionSource::UnmatchedCommandFallback => Self::shell_request_escalation_execution(
                if always_sandboxed
                    && self.sandbox_permissions == SandboxPermissions::RequireEscalated
                {
                    SandboxPermissions::UseDefault
                } else {
                    self.sandbox_permissions
                },
                &self.permission_profile,
                self.prompt_permissions.as_ref(),
            ),
        };
        self.process_decision(
            evaluation.decision,
            needs_escalation,
            program,
            argv,
            workdir,
            self.prompt_permissions.clone(),
            escalation_execution,
            decision_source,
            suspended_note,
            asked_again,
        )
        .await
    }
}

impl EscalationPolicy for CoreShellActionProvider {
    fn determine_action<'a>(
        &'a self,
        program: &'a AbsolutePathBuf,
        argv: &'a [String],
        workdir: &'a AbsolutePathBuf,
    ) -> EscalationPolicyFuture<'a> {
        Box::pin(CoreShellActionProvider::determine_fresh_action(
            self, program, argv, workdir,
        ))
    }
}

fn evaluate_intercepted_exec_policy(
    policy: &Policy,
    program: &AbsolutePathBuf,
    argv: &[String],
    context: InterceptedExecPolicyContext,
) -> Evaluation {
    let InterceptedExecPolicyContext {
        approval_policy,
        permission_profile,
        windows_sandbox_level,
        sandbox_permissions,
        enable_shell_wrapper_parsing,
    } = context;
    let CandidateCommands {
        commands,
        used_complex_parsing,
    } = if enable_shell_wrapper_parsing {
        // In this codepath, the first argument in `commands` could be a bare
        // name like `find` instead of an absolute path like `/usr/bin/find`.
        // It could also be a shell built-in like `echo`.
        commands_for_intercepted_exec_policy(program, argv)
    } else {
        // In this codepath, `commands` has a single entry where the program
        // is always an absolute path.
        CandidateCommands {
            commands: vec![join_program_and_argv(program, argv)],
            used_complex_parsing: false,
        }
    };

    let fallback = |cmd: &[String]| {
        crate::exec_policy::render_decision_for_unmatched_command(
            cmd,
            crate::exec_policy::UnmatchedCommandContext {
                approval_policy,
                permission_profile: &permission_profile,
                windows_sandbox_level,
                sandbox_permissions,
                used_complex_parsing,
                command_origin: crate::exec_policy::ExecPolicyCommandOrigin::Generic,
            },
        )
    };

    policy.check_multiple_with_options(
        commands.iter(),
        &fallback,
        &MatchOptions {
            resolve_host_executables: true,
        },
    )
}

#[derive(Clone)]
struct InterceptedExecPolicyContext {
    approval_policy: AskForApproval,
    permission_profile: PermissionProfile,
    windows_sandbox_level: WindowsSandboxLevel,
    sandbox_permissions: SandboxPermissions,
    enable_shell_wrapper_parsing: bool,
}

struct CandidateCommands {
    commands: Vec<Vec<String>>,
    used_complex_parsing: bool,
}

fn commands_for_intercepted_exec_policy(
    program: &AbsolutePathBuf,
    argv: &[String],
) -> CandidateCommands {
    if let [_, flag, script] = argv {
        let shell_command = [
            program.to_string_lossy().to_string(),
            flag.clone(),
            script.clone(),
        ];
        if let Some(commands) = parse_shell_lc_plain_commands(&shell_command) {
            return CandidateCommands {
                commands,
                used_complex_parsing: false,
            };
        }
        if let Some(single_command) = parse_shell_lc_single_command_prefix(&shell_command) {
            return CandidateCommands {
                commands: vec![single_command],
                used_complex_parsing: true,
            };
        }
    }

    CandidateCommands {
        commands: vec![join_program_and_argv(program, argv)],
        used_complex_parsing: false,
    }
}

struct CoreShellCommandExecutor {
    command: Vec<String>,
    cwd: AbsolutePathBuf,
    permission_profile: PermissionProfile,
    sandbox: SandboxType,
    env: HashMap<String, String>,
    network: Option<codex_network_proxy::NetworkProxy>,
    network_environment_id: Option<String>,
    windows_sandbox_level: WindowsSandboxLevel,
    arg0: Option<String>,
    sandbox_policy_cwd: AbsolutePathBuf,
    windows_sandbox_workspace_roots: Vec<AbsolutePathBuf>,
    codex_linux_sandbox_exe: Option<PathBuf>,
    use_legacy_landlock: bool,
}

struct PrepareSandboxedExecParams<'a> {
    command: Vec<String>,
    workdir: &'a AbsolutePathBuf,
    env: HashMap<String, String>,
    permission_profile: &'a PermissionProfile,
    additional_permissions: Option<AdditionalPermissionProfile>,
}

impl ShellCommandExecutor for CoreShellCommandExecutor {
    fn run(
        &self,
        _command: Vec<String>,
        _cwd: PathBuf,
        env_overlay: HashMap<String, String>,
        cancel_rx: CancellationToken,
        after_spawn: Option<Box<dyn FnOnce() + Send>>,
    ) -> ShellCommandExecutorFuture<'_, ExecResult> {
        Box::pin(CoreShellCommandExecutor::run(
            self,
            env_overlay,
            cancel_rx,
            after_spawn,
        ))
    }

    fn prepare_escalated_exec<'a>(
        &'a self,
        program: &'a AbsolutePathBuf,
        argv: &'a [String],
        workdir: &'a AbsolutePathBuf,
        env: HashMap<String, String>,
        execution: EscalationExecution,
    ) -> ShellCommandExecutorFuture<'a, PreparedExec> {
        Box::pin(CoreShellCommandExecutor::prepare_escalated_exec(
            self, program, argv, workdir, env, execution,
        ))
    }
}

impl CoreShellCommandExecutor {
    async fn run(
        &self,
        env_overlay: HashMap<String, String>,
        cancel_rx: CancellationToken,
        after_spawn: Option<Box<dyn FnOnce() + Send>>,
    ) -> anyhow::Result<ExecResult> {
        let mut exec_env = self.env.clone();
        // `env_overlay` comes from `EscalationSession::env()`, so merge only the
        // wrapper/socket variables into the base shell environment.
        for var in ["CODEX_ESCALATE_SOCKET", "EXEC_WRAPPER"] {
            if let Some(value) = env_overlay.get(var) {
                exec_env.insert(var.to_string(), value.clone());
            }
        }

        let result = crate::sandboxing::execute_exec_request_with_after_spawn(
            crate::sandboxing::ExecRequest {
                command: self.command.clone(),
                cwd: self.cwd.clone().into(),
                env: exec_env,
                exec_server_env_config: None,
                network: self.network.clone(),
                network_environment_id: self.network_environment_id.clone(),
                expiration: ExecExpiration::Cancellation(cancel_rx),
                capture_policy: ExecCapturePolicy::ShellTool,
                sandbox: self.sandbox,
                windows_sandbox_policy_cwd: self.sandbox_policy_cwd.clone().into(),
                windows_sandbox_workspace_roots: self.windows_sandbox_workspace_roots.clone(),
                windows_sandbox_level: self.windows_sandbox_level,
                windows_sandbox_private_desktop: false,
                permission_profile: self.permission_profile.clone(),
                windows_sandbox_filesystem_overrides: None,
                arg0: self.arg0.clone(),
                exec_server_sandbox: None,
                exec_server_enforce_managed_network: false,
                exec_server_managed_network: None,
                exec_server_network_proxy: None,
            },
            /*stdout_stream*/ None,
            after_spawn,
        )
        .await?;

        Ok(ExecResult {
            exit_code: result.exit_code,
            stdout: result.stdout.text,
            stderr: result.stderr.text,
            output: result.aggregated_output.text,
            duration: result.duration,
            timed_out: result.timed_out,
        })
    }

    async fn prepare_escalated_exec(
        &self,
        program: &AbsolutePathBuf,
        argv: &[String],
        workdir: &AbsolutePathBuf,
        env: HashMap<String, String>,
        execution: EscalationExecution,
    ) -> anyhow::Result<PreparedExec> {
        let command = join_program_and_argv(program, argv);
        let Some(first_arg) = argv.first() else {
            return Err(anyhow::anyhow!(
                "intercepted exec request must contain argv[0]"
            ));
        };

        if matches!(execution, EscalationExecution::Unsandboxed)
            && crate::security::launch_contract::active().is_some()
        {
            return Err(anyhow::anyhow!(
                crate::security::launch_contract::LaunchDenied::Unsandboxed
            ));
        }
        let prepared = match execution {
            EscalationExecution::Unsandboxed => PreparedExec {
                command,
                cwd: workdir.to_path_buf(),
                env: exec_env_for_sandbox_permissions(&env, SandboxPermissions::RequireEscalated),
                arg0: Some(first_arg.clone()),
            },
            EscalationExecution::TurnDefault => {
                self.prepare_sandboxed_exec(PrepareSandboxedExecParams {
                    command,
                    workdir,
                    env,
                    permission_profile: &self.permission_profile,
                    additional_permissions: None,
                })?
            }
            EscalationExecution::Permissions(
                EscalationPermissions::AdditionalPermissionProfile(permission_profile),
            ) => {
                // Merge additive permissions into the existing turn/request sandbox policy.
                self.prepare_sandboxed_exec(PrepareSandboxedExecParams {
                    command,
                    workdir,
                    env,
                    permission_profile: &self.permission_profile,
                    additional_permissions: Some(permission_profile),
                })?
            }
            EscalationExecution::Permissions(EscalationPermissions::ResolvedPermissionProfile(
                permissions,
            )) => {
                // Use a fully specified permission profile instead of merging into the turn policy.
                self.prepare_sandboxed_exec(PrepareSandboxedExecParams {
                    command,
                    workdir,
                    env,
                    permission_profile: &permissions.permission_profile,
                    additional_permissions: None,
                })?
            }
        };

        Ok(prepared)
    }

    #[allow(clippy::too_many_arguments)]
    fn prepare_sandboxed_exec(
        &self,
        params: PrepareSandboxedExecParams<'_>,
    ) -> anyhow::Result<PreparedExec> {
        let PrepareSandboxedExecParams {
            command,
            workdir,
            env,
            permission_profile,
            additional_permissions,
        } = params;
        let (program, args) = command
            .split_first()
            .ok_or_else(|| anyhow::anyhow!("prepared command must not be empty"))?;
        let sandbox_manager = SandboxManager::new();
        let sandbox = sandbox_manager.select_initial(
            permission_profile,
            SandboxablePreference::Auto,
            self.windows_sandbox_level,
            self.network.is_some(),
        );
        let cwd = PathUri::from_abs_path(workdir);
        let sandbox_policy_cwd = PathUri::from_abs_path(&self.sandbox_policy_cwd);
        let mut command = SandboxCommand {
            program: program.clone().into(),
            args: args.to_vec(),
            cwd,
            env,
            managed_network: None,
            additional_permissions,
        };
        // PF-27-S02: intercepted execs get the same protected launch contract.
        let protected = match crate::security::launch_contract::active() {
            Some(contract) => Some(contract.protect_launch(
                sandbox,
                sandbox != SandboxType::None,
                /*exec_server*/ false,
                /*windows_elevated*/ false,
                &mut command,
                permission_profile,
                self.sandbox_policy_cwd.as_path(),
            )?),
            None => None,
        };
        let permission_profile = protected.as_ref().unwrap_or(permission_profile);
        let options = ExecOptions {
            expiration: ExecExpiration::DefaultTimeout,
            capture_policy: ExecCapturePolicy::ShellTool,
        };
        let exec_request = sandbox_manager.transform(SandboxTransformRequest {
            command,
            permissions: permission_profile,
            sandbox,
            enforce_managed_network: self.network.is_some(),
            environment_id: self.network_environment_id.as_deref(),
            network: self.network.as_ref(),
            sandbox_policy_cwd: &sandbox_policy_cwd,
            codex_linux_sandbox_exe: self.codex_linux_sandbox_exe.as_deref(),
            use_legacy_landlock: self.use_legacy_landlock,
            windows_sandbox_level: self.windows_sandbox_level,
            windows_sandbox_private_desktop: false,
        })?;
        let mut exec_request = crate::sandboxing::ExecRequest::from_sandbox_exec_request(
            exec_request,
            options,
            self.windows_sandbox_workspace_roots.clone(),
        );
        if let Some(network) = exec_request.network.as_ref() {
            network
                .apply_to_env_for_optional_environment(
                    &mut exec_request.env,
                    self.network_environment_id.as_deref(),
                )
                .map_err(|err| {
                    let environment_id =
                        self.network_environment_id.as_deref().unwrap_or("default");
                    CodexErr::Io(io::Error::other(format!(
                        "failed to prepare network proxy for environment `{environment_id}`: {err}"
                    )))
                })?;
        }

        Ok(PreparedExec {
            command: exec_request.command,
            // TODO(anp): Keep PathUri through the execve-wrapper boundary.
            cwd: exec_request.cwd.to_abs_path()?.to_path_buf(),
            env: exec_request.env,
            arg0: exec_request.arg0,
        })
    }
}

#[derive(Debug, Eq, PartialEq)]
struct ParsedShellCommand {
    program: String,
    script: String,
    login: bool,
}

fn extract_shell_script(command: &[String]) -> Result<ParsedShellCommand, ToolError> {
    // Commands reaching zsh-fork can be wrapped by environment/sandbox helpers, so
    // we search for the first `-c`/`-lc` triple anywhere in the argv rather
    // than assuming it is the first positional form.
    if let Some((program, script, login)) = command.windows(3).find_map(|parts| match parts {
        [program, flag, script] if flag == "-c" => {
            Some((program.to_owned(), script.to_owned(), false))
        }
        [program, flag, script] if flag == "-lc" => {
            Some((program.to_owned(), script.to_owned(), true))
        }
        _ => None,
    }) {
        return Ok(ParsedShellCommand {
            program,
            script,
            login,
        });
    }

    Err(ToolError::Rejected(
        "unexpected shell command format for zsh-fork execution".to_string(),
    ))
}

fn map_exec_result(
    sandbox: SandboxType,
    result: ExecResult,
) -> Result<ExecToolCallOutput, ToolError> {
    let output = ExecToolCallOutput {
        exit_code: result.exit_code,
        stdout: StreamOutput::new(result.stdout.clone()),
        stderr: StreamOutput::new(result.stderr.clone()),
        aggregated_output: StreamOutput::new(result.output.clone()),
        duration: result.duration,
        timed_out: result.timed_out,
    };

    if result.timed_out {
        return Err(ToolError::Codex(CodexErr::Sandbox(SandboxErr::Timeout {
            output: Box::new(output),
        })));
    }

    if is_likely_sandbox_denied(sandbox, &output) {
        record_filesystem_sandbox_violation(sandbox, &output);
        return Err(ToolError::Codex(CodexErr::Sandbox(SandboxErr::Denied {
            output: Box::new(output),
            network_policy_decision: None,
        })));
    }

    Ok(output)
}

/// Convert an intercepted exec `(program, argv)` into a command vector suitable
/// for display and policy parsing.
///
/// The intercepted `argv` includes `argv[0]`, but once we have normalized the
/// executable path in `program`, we should replace the original `argv[0]`
/// rather than duplicating it as an apparent user argument.
fn join_program_and_argv(program: &AbsolutePathBuf, argv: &[String]) -> Vec<String> {
    std::iter::once(program.to_string_lossy().to_string())
        .chain(argv.iter().skip(1).cloned())
        .collect::<Vec<_>>()
}

#[cfg(test)]
#[path = "unix_escalation_tests.rs"]
mod tests;
