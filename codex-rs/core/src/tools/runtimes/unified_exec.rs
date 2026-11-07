/*
Runtime: unified exec

Handles approval + sandbox orchestration for unified exec requests, delegating to
the process manager to spawn PTYs once an ExecRequest is prepared.
*/
use crate::command_canonicalization::canonicalize_command_for_approval;
use crate::exec::ExecCapturePolicy;
use crate::exec::ExecExpiration;
use crate::guardian::GUARDIAN_REVIEW_TIMEOUT;
use crate::guardian::GuardianNetworkAccessTrigger;
use crate::guardian::routes_approval_to_guardian;
use crate::sandboxing::ExecOptions;
use crate::sandboxing::ExecServerEnvConfig;
use crate::sandboxing::SandboxPermissions;
use crate::session::turn_context::TurnEnvironment;
use crate::shell::ShellType;
use crate::tools::flat_tool_name;
use crate::tools::network_approval::NetworkApprovalMode;
use crate::tools::network_approval::NetworkApprovalSpec;
use crate::tools::runtimes::RuntimePathPrepends;
#[cfg(unix)]
use crate::tools::runtimes::apply_zsh_fork_path_prepend;
use crate::tools::runtimes::disable_powershell_profile_for_elevated_windows_sandbox;
use crate::tools::runtimes::exec_env_for_sandbox_permissions;
use crate::tools::runtimes::maybe_wrap_shell_lc_with_snapshot;
use crate::tools::runtimes::shell::zsh_fork_backend;
use crate::tools::sandboxing::Approvable;
use crate::tools::sandboxing::ApprovalAction;
use crate::tools::sandboxing::ApprovalCtx;
use crate::tools::sandboxing::ExecApprovalRequirement;
use crate::tools::sandboxing::PermissionRequestPayload;
use crate::tools::sandboxing::SandboxAttempt;
use crate::tools::sandboxing::Sandboxable;
use crate::tools::sandboxing::ToolCtx;
use crate::tools::sandboxing::ToolError;
use crate::tools::sandboxing::ToolRuntime;
use crate::tools::sandboxing::managed_network_for_sandbox_permissions;
use crate::tools::sandboxing::sandbox_permissions_for_session;
use crate::tools::sandboxing::with_cached_approval;
use crate::unified_exec::NoopSpawnLifecycle;
use crate::unified_exec::UnifiedExecError;
use crate::unified_exec::UnifiedExecProcess;
use crate::unified_exec::UnifiedExecProcessManager;
use codex_network_proxy::ManagedNetworkSandboxContext;
use codex_network_proxy::NetworkMode;
use codex_network_proxy::NetworkProxy;
use codex_protocol::error::CodexErr;
use codex_protocol::error::SandboxErr;
use codex_protocol::models::AdditionalPermissionProfile;
use codex_protocol::protocol::NetworkPolicyRuleAction;
use codex_protocol::protocol::ReviewDecision;
use codex_sandboxing::SandboxCommand;
use codex_sandboxing::SandboxablePreference;
use codex_shell_command::powershell::prefix_powershell_script_with_utf8;
use codex_tools::UnifiedExecShellMode;
use codex_utils_absolute_path::AbsolutePathBuf;
use codex_utils_path_uri::PathUri;
use futures::future::BoxFuture;
use std::collections::HashMap;
use std::io;
use std::time::Duration;
use tokio_util::sync::CancellationToken;
use tracing::error;

// Allow 5s for Guardian cleanup and 5s for controller processing after review.
const REMOTE_NETWORK_POLICY_DECISION_MARGIN: Duration = Duration::from_secs(10);
/// How long a remote network request may wait for the user to answer its
/// prompt on this machine (#722, D246). Unanswered, the request is refused
/// and the command ends as failed.
const REMOTE_NETWORK_USER_DECISION_TIMEOUT: Duration = Duration::from_secs(10 * 60);

/// Budget for this machine to decide one remote network request, or `None`
/// to leave the decision to the remote proxy (#722, D246).
///
/// When the proxy has a policy decider (managed network requirements), a
/// remote allowlist miss is routed here, so the user gets the same prompt and
/// failure reporting as locally. Under Guardian that held before. Without a
/// decider the remote proxy still refuses it silently (fail closed). Remote
/// "limited" mode is not routed for the user: it keeps failing closed on the
/// remote side, as before.
fn remote_network_policy_decision_timeout(
    routes_to_guardian: bool,
    mode: NetworkMode,
    hook_timeout: Duration,
) -> Option<Duration> {
    let review_timeout = if routes_to_guardian {
        GUARDIAN_REVIEW_TIMEOUT
    } else if mode == NetworkMode::Full {
        REMOTE_NETWORK_USER_DECISION_TIMEOUT
    } else {
        return None;
    };
    Some(
        hook_timeout
            .saturating_add(review_timeout)
            .saturating_add(REMOTE_NETWORK_POLICY_DECISION_MARGIN),
    )
}

/// Request payload used by the unified-exec runtime after approvals and
/// sandbox preferences have been resolved for the current turn.
#[derive(Clone, Debug)]
pub struct UnifiedExecRequest {
    pub command: Vec<String>,
    pub shell_type: ShellType,
    pub hook_command: String,
    pub process_id: i32,
    pub cwd: PathUri,
    pub sandbox_cwd: PathUri,
    pub turn_environment: TurnEnvironment,
    pub env: HashMap<String, String>,
    pub exec_server_env_config: Option<ExecServerEnvConfig>,
    pub explicit_env_overrides: HashMap<String, String>,
    pub network: Option<NetworkProxy>,
    pub tty: bool,
    pub sandbox_permissions: SandboxPermissions,
    pub additional_permissions: Option<AdditionalPermissionProfile>,
    #[cfg(unix)]
    pub additional_permissions_preapproved: bool,
    pub justification: Option<String>,
    pub exec_approval_requirement: ExecApprovalRequirement,
}

/// Cache key for approval decisions that can be reused across equivalent
/// unified-exec launches.
#[derive(serde::Serialize, Clone, Debug, Eq, PartialEq, Hash)]
pub struct UnifiedExecApprovalKey {
    pub environment_id: String,
    pub command: Vec<String>,
    pub cwd: PathUri,
    pub tty: bool,
    pub sandbox_permissions: SandboxPermissions,
    pub additional_permissions: Option<AdditionalPermissionProfile>,
}

/// Runtime adapter that keeps policy and sandbox orchestration on the
/// unified-exec side while delegating process startup to the manager.
pub struct UnifiedExecRuntime<'a> {
    manager: &'a UnifiedExecProcessManager,
    shell_mode: UnifiedExecShellMode,
    /// #466: whether the command is shown as running because of a review
    /// that approved it. A review shows it (the app server starts its item);
    /// a refusal ends it there. An approved one must end even when the
    /// command then fails to start.
    shown_by_review: bool,
    /// #873: whether the last start attempt itself (not a policy check
    /// before it) failed with `ToolError::Rejected`: a setup or spawn
    /// failure, shown as failed rather than as refused by policy.
    start_rejected: bool,
}

fn unified_exec_options(
    network_denial_cancellation_token: Option<CancellationToken>,
) -> ExecOptions {
    let mut expiration = ExecExpiration::DefaultTimeout;
    if let Some(cancellation) = network_denial_cancellation_token {
        expiration = expiration.with_cancellation(cancellation);
    }
    ExecOptions {
        expiration,
        capture_policy: ExecCapturePolicy::ShellTool,
    }
}

fn build_unified_exec_sandbox_command(
    command: &[String],
    cwd: &PathUri,
    env: &HashMap<String, String>,
    managed_network: Option<ManagedNetworkSandboxContext>,
    additional_permissions: Option<AdditionalPermissionProfile>,
) -> Result<SandboxCommand, ToolError> {
    let (program, args) = command
        .split_first()
        .ok_or_else(|| ToolError::Rejected("command args are empty".to_string()))?;
    Ok(SandboxCommand {
        program: program.clone().into(),
        args: args.to_vec(),
        cwd: cwd.clone(),
        env: env.clone(),
        managed_network,
        additional_permissions,
    })
}

impl<'a> UnifiedExecRuntime<'a> {
    /// Creates a runtime bound to the shared unified-exec process manager.
    pub fn new(manager: &'a UnifiedExecProcessManager, shell_mode: UnifiedExecShellMode) -> Self {
        Self {
            manager,
            shell_mode,
            shown_by_review: false,
            start_rejected: false,
        }
    }

    /// Whether the last review of this command (the person's or the
    /// automatic reviewer's) approved it, leaving it shown as running. A
    /// permission hook shows nothing and leaves this as it was; a cached
    /// session approval counts as approved (the command then ends on screen
    /// even if no prompt showed it).
    pub(crate) fn shown_by_review(&self) -> bool {
        self.shown_by_review
    }

    /// #873: whether a `ToolError::Rejected` from the orchestrator came from
    /// the start attempt (setup or spawn failure) rather than from a policy
    /// refusal before it.
    pub(crate) fn start_rejected(&self) -> bool {
        self.start_rejected
    }
}

impl Sandboxable for UnifiedExecRuntime<'_> {
    fn sandbox_preference(&self) -> SandboxablePreference {
        SandboxablePreference::Auto
    }

    fn escalate_on_failure(&self) -> bool {
        true
    }
}

impl Approvable<UnifiedExecRequest> for UnifiedExecRuntime<'_> {
    type ApprovalKey = UnifiedExecApprovalKey;

    fn approval_keys(&self, req: &UnifiedExecRequest) -> Vec<Self::ApprovalKey> {
        vec![UnifiedExecApprovalKey {
            environment_id: req.turn_environment.environment_id.clone(),
            command: canonicalize_command_for_approval(&req.command),
            cwd: req.cwd.clone(),
            tty: req.tty,
            sandbox_permissions: req.sandbox_permissions,
            additional_permissions: req.additional_permissions.clone(),
        }]
    }

    fn note_review_decision(&mut self, decision: &ReviewDecision, ctx: &ApprovalCtx<'_>) {
        // A person's network review neither shows nor ends the command.
        // (Unified exec never retries with a network context today, so an
        // automatic review never gets one either.)
        if ctx.network_approval_context.is_some() {
            return;
        }
        self.shown_by_review = match decision {
            ReviewDecision::Denied { .. } | ReviewDecision::TimedOut | ReviewDecision::Abort => {
                false
            }
            ReviewDecision::NetworkPolicyAmendment {
                network_policy_amendment,
            } => network_policy_amendment.action != NetworkPolicyRuleAction::Deny,
            ReviewDecision::Approved
            | ReviewDecision::ApprovedExecpolicyAmendment { .. }
            | ReviewDecision::ApprovedForSession => true,
        };
    }

    fn start_approval_async<'b>(
        &'b mut self,
        req: &'b UnifiedExecRequest,
        ctx: ApprovalCtx<'b>,
    ) -> BoxFuture<'b, ReviewDecision> {
        let keys = self.approval_keys(req);
        let session = ctx.session;
        let turn = ctx.turn;
        let call_id = ctx.call_id.to_string();
        let command = req.command.clone();
        let environment_id = Some(req.turn_environment.environment_id.clone());
        let reason = ctx
            .retry_reason
            .clone()
            .or_else(|| req.justification.clone());
        Box::pin(async move {
            let native_cwd = match req.cwd.to_abs_path() {
                Ok(c) => c,
                Err(e) => {
                    // TODO(anp) make sandboxing work for foreign OSes, in the meantime this should
                    // be impossible for single-OS app-servers
                    error!(cwd = %req.cwd, ?e, "got non-native path in start_approval_async");
                    return ReviewDecision::Abort;
                }
            };
            let fresh = ctx.fresh_human_authority;
            with_cached_approval(
                &session.services,
                "unified_exec",
                keys,
                fresh,
                || async move {
                    let available_decisions = None;
                    session
                        .request_command_approval(
                            turn,
                            call_id,
                            /*approval_id*/ None,
                            environment_id,
                            command,
                            native_cwd,
                            reason,
                            ctx.network_approval_context.clone(),
                            // A post-taint approval covers this action only:
                            // no "don't ask again" rule (PF-30-S03).
                            req.exec_approval_requirement
                                .proposed_execpolicy_amendment()
                                .filter(|_| !fresh)
                                .cloned(),
                            req.additional_permissions.clone(),
                            available_decisions,
                            /*plugin_attribution_override*/ None,
                        )
                        .await
                },
            )
            .await
        })
    }

    fn approval_action(
        &self,
        req: &UnifiedExecRequest,
        ctx: &ApprovalCtx<'_>,
    ) -> std::io::Result<ApprovalAction> {
        Ok(ApprovalAction::ExecCommand {
            id: ctx.call_id.to_string(),
            environment_id: req.turn_environment.environment_id.clone(),
            command: req.command.clone(),
            cwd: req.cwd.clone(),
            sandbox_permissions: req.sandbox_permissions,
            additional_permissions: req.additional_permissions.clone(),
            justification: req.justification.clone(),
            tty: req.tty,
        })
    }

    fn exec_approval_requirement(
        &self,
        req: &UnifiedExecRequest,
    ) -> Option<ExecApprovalRequirement> {
        Some(req.exec_approval_requirement.clone())
    }

    fn permission_request_payload(
        &self,
        req: &UnifiedExecRequest,
    ) -> Option<PermissionRequestPayload> {
        Some(PermissionRequestPayload::bash(
            req.hook_command.clone(),
            req.justification.clone(),
        ))
    }

    fn sandbox_permissions(&self, req: &UnifiedExecRequest) -> SandboxPermissions {
        req.sandbox_permissions
    }

    fn additional_permissions<'r>(
        &self,
        req: &'r UnifiedExecRequest,
    ) -> Option<&'r codex_protocol::models::AdditionalPermissionProfile> {
        req.additional_permissions.as_ref()
    }
}

impl<'a> ToolRuntime<UnifiedExecRequest, UnifiedExecProcess> for UnifiedExecRuntime<'a> {
    fn workspace_roots<'b>(&self, req: &'b UnifiedExecRequest) -> &'b [PathUri] {
        req.turn_environment.workspace_roots()
    }

    fn sandbox_cwd<'b>(&self, req: &'b UnifiedExecRequest) -> Option<&'b PathUri> {
        Some(&req.sandbox_cwd)
    }

    fn command_cwd(&self, req: &UnifiedExecRequest) -> Option<AbsolutePathBuf> {
        req.cwd.to_abs_path().ok()
    }

    fn local_search_path<'b>(&self, req: &'b UnifiedExecRequest) -> Option<&'b str> {
        (!req.turn_environment.environment.is_remote())
            .then(|| req.env.get("PATH").map(String::as_str))
            .flatten()
    }

    fn network_approval_spec(
        &self,
        req: &UnifiedExecRequest,
        ctx: &ToolCtx,
    ) -> Option<NetworkApprovalSpec> {
        let file_system_sandbox_policy = ctx.turn.file_system_sandbox_policy();
        let sandbox_permissions = sandbox_permissions_for_session(
            req.sandbox_permissions,
            &file_system_sandbox_policy,
            &ctx.session,
        );
        let network =
            managed_network_for_sandbox_permissions(req.network.as_ref(), sandbox_permissions)?;
        Some(NetworkApprovalSpec {
            network: Some(network.clone()),
            mode: NetworkApprovalMode::Deferred,
            trigger: GuardianNetworkAccessTrigger {
                call_id: ctx.call_id.clone(),
                tool_name: flat_tool_name(&ctx.tool_name).into_owned(),
                command: req.command.clone(),
                cwd: req.cwd.to_abs_path().ok()?,
                sandbox_permissions: req.sandbox_permissions,
                additional_permissions: req.additional_permissions.clone(),
                justification: req.justification.clone(),
                tty: Some(req.tty),
            },
            command: req.hook_command.clone(),
            environment_id: req.turn_environment.environment_id.clone(),
        })
    }

    async fn run(
        &mut self,
        req: &UnifiedExecRequest,
        attempt: &SandboxAttempt<'_>,
        ctx: &ToolCtx,
    ) -> Result<UnifiedExecProcess, ToolError> {
        let result = self.start_process(req, attempt, ctx).await;
        self.start_rejected = matches!(result, Err(ToolError::Rejected(_)));
        result
    }
}

impl UnifiedExecRuntime<'_> {
    async fn start_process(
        &mut self,
        req: &UnifiedExecRequest,
        attempt: &SandboxAttempt<'_>,
        ctx: &ToolCtx,
    ) -> Result<UnifiedExecProcess, ToolError> {
        let base_command = &req.command;
        let windows_sandbox_proxy_settings_mode = ctx.session.windows_sandbox_proxy_settings_mode;
        let session_shell = ctx.session.user_shell();
        let shell = req
            .turn_environment
            .shell
            .as_ref()
            .unwrap_or(session_shell.as_ref());
        let environment_is_remote = req.turn_environment.environment.is_remote();
        let shell_snapshot_location = if environment_is_remote {
            None
        } else {
            // TODO(anp): Make shell snapshot lookup accept PathUri.
            let native_cwd = req
                .cwd
                .to_abs_path()
                .map_err(|err| ToolError::Rejected(err.to_string()))?;
            req.turn_environment.shell_snapshot(&native_cwd)
        };
        let (file_system_sandbox_policy, _) = attempt.permissions.to_runtime_permissions();
        // PF-23-S02: remember whether this process can read credentials, from
        // the sandbox that really runs it, for typing into it later.
        {
            // Both a Corbanu store and a user credential folder must be
            // unreadable (a profile may deny one on its own).
            let markers = [
                Some(ctx.turn.config.codex_home.join("auth.json").into_path_buf()),
                dirs::home_dir().map(|home| home.join(".ssh")),
            ];
            let cwd = attempt.sandbox_cwd.to_abs_path().ok();
            let confined = !environment_is_remote
                && attempt.sandbox != codex_sandboxing::SandboxType::None
                && cwd.is_some_and(|cwd| {
                    markers.iter().all(|marker| {
                        marker.as_ref().is_some_and(|marker| {
                            !file_system_sandbox_policy
                                .can_read_path_with_cwd(marker, cwd.as_path())
                        })
                    })
                });
            crate::security::protected_surface::confined::note_start(
                ctx.session.thread_id(),
                req.process_id,
                confined,
            );
        }
        let launch_sandbox_permissions = sandbox_permissions_for_session(
            req.sandbox_permissions,
            &file_system_sandbox_policy,
            &ctx.session,
        );
        let managed_network = attempt.network_proxy(managed_network_for_sandbox_permissions(
            req.network.as_ref(),
            launch_sandbox_permissions,
        ));
        let env = exec_env_for_sandbox_permissions(&req.env, launch_sandbox_permissions);
        let (env, managed_network_context, network_proxy_launch) = match managed_network {
            Some(network) if environment_is_remote => {
                let mut launch = network.remote_launch_config().await.map_err(|err| {
                    ToolError::Codex(CodexErr::Io(io::Error::other(err.to_string())))
                })?;
                if network.remote_policy_decider().is_some()
                    && let Some(timeout) = remote_network_policy_decision_timeout(
                        routes_approval_to_guardian(&ctx.turn),
                        launch.proxy.mode,
                        ctx.session.hooks().max_permission_request_timeout(),
                    )
                {
                    launch.policy_decision_timeout_ms =
                        Some(u64::try_from(timeout.as_millis()).map_err(|_| {
                            ToolError::Rejected(
                                "remote network policy decision timeout exceeds protocol limit"
                                    .to_string(),
                            )
                        })?);
                }
                if !launch.proxy.enabled {
                    (env, None, None)
                } else {
                    let environment_info =
                        req.turn_environment
                            .environment
                            .info()
                            .await
                            .map_err(|err| {
                                ToolError::Codex(CodexErr::Io(io::Error::other(format!(
                                    "failed to query exec-server capabilities: {err}"
                                ))))
                            })?;
                    if !environment_info.capabilities.network_proxy_launch {
                        return Err(ToolError::Rejected(
                            "selected exec-server does not support executor-local network proxy launches"
                                .to_string(),
                        ));
                    }
                    (env, None, Some(launch))
                }
            }
            Some(network) => {
                let prepared = network
                    .prepare_for_optional_environment(
                        env,
                        Some(&req.turn_environment.environment_id),
                    )
                    .map_err(|err| {
                        ToolError::Codex(CodexErr::Io(io::Error::other(format!(
                            "failed to prepare network proxy for environment `{}`: {err}",
                            req.turn_environment.environment_id
                        ))))
                    })?;
                (prepared.env, Some(prepared.sandbox_context), None)
            }
            None => (env, None, None),
        };
        let explicit_env_overrides = req.explicit_env_overrides.clone();
        #[cfg(unix)]
        let mut env = env;
        #[cfg(unix)]
        let runtime_path_prepends = {
            let mut runtime_path_prepends = RuntimePathPrepends::default();
            if !environment_is_remote {
                crate::tools::runtimes::apply_package_path_prepend(
                    &mut env,
                    &mut runtime_path_prepends,
                );
            }
            if let UnifiedExecShellMode::ZshFork(zsh_fork_config) = &self.shell_mode {
                apply_zsh_fork_path_prepend(
                    &mut env,
                    &mut runtime_path_prepends,
                    zsh_fork_config.shell_zsh_path.as_path(),
                );
            }
            runtime_path_prepends
        };
        #[cfg(not(unix))]
        let runtime_path_prepends = RuntimePathPrepends::default();
        let command = if environment_is_remote {
            base_command.to_vec()
        } else {
            maybe_wrap_shell_lc_with_snapshot(
                base_command,
                shell,
                shell_snapshot_location.as_ref(),
                &explicit_env_overrides,
                &env,
                &runtime_path_prepends,
            )
        };
        let command = disable_powershell_profile_for_elevated_windows_sandbox(
            &command,
            Some(&req.shell_type),
            attempt.sandbox,
            attempt.windows_sandbox_level,
        );
        let command = if matches!(req.shell_type, ShellType::PowerShell) {
            prefix_powershell_script_with_utf8(&command)
        } else {
            command
        };

        if let UnifiedExecShellMode::ZshFork(zsh_fork_config) = &self.shell_mode {
            let command = build_unified_exec_sandbox_command(
                &command,
                &req.cwd,
                &env,
                managed_network_context.clone(),
                req.additional_permissions.clone(),
            )
            .map_err(|error| match error {
                ToolError::Rejected(_) | ToolError::Declined(_) => {
                    ToolError::Rejected("missing command line for PTY".to_string())
                }
                error @ ToolError::Codex(_) => error,
            })?;
            let options = unified_exec_options(attempt.network_denial_cancellation_token.clone());
            let mut exec_env = attempt
                .env_for(
                    command,
                    options,
                    managed_network,
                    Some(&req.turn_environment.environment_id),
                )
                .map_err(ToolError::Codex)?;
            exec_env.exec_server_env_config = req.exec_server_env_config.clone();
            match zsh_fork_backend::maybe_prepare_unified_exec(
                req,
                attempt,
                ctx,
                exec_env,
                zsh_fork_config,
            )
            .await?
            {
                Some(prepared) => {
                    if req.turn_environment.environment.is_remote() {
                        return Err(ToolError::Rejected(
                            "unified_exec zsh-fork is not supported for remote environments"
                                .to_string(),
                        ));
                    }
                    return self
                        .manager
                        .open_session_with_prepared_exec_env(
                            req.process_id,
                            &prepared.exec_request,
                            windows_sandbox_proxy_settings_mode,
                            /*network_policy_decider*/ None,
                            req.tty,
                            prepared.spawn_lifecycle,
                            req.turn_environment.environment.as_ref(),
                        )
                        .await
                        .map_err(|err| match err {
                            UnifiedExecError::SandboxDenied { output, .. } => {
                                ToolError::Codex(CodexErr::Sandbox(SandboxErr::Denied {
                                    output: Box::new(output),
                                    network_policy_decision: None,
                                }))
                            }
                            other => ToolError::Rejected(other.to_string()),
                        });
                }
                None => {
                    tracing::warn!(
                        "UnifiedExec ZshFork backend specified, but conditions for using it were not met, falling back to direct execution",
                    );
                }
            }
        }
        let command = build_unified_exec_sandbox_command(
            &command,
            &req.cwd,
            &env,
            managed_network_context,
            req.additional_permissions.clone(),
        )
        .map_err(|error| match error {
            ToolError::Rejected(_) | ToolError::Declined(_) => {
                ToolError::Rejected("missing command line for PTY".to_string())
            }
            error @ ToolError::Codex(_) => error,
        })?;
        let options = unified_exec_options(attempt.network_denial_cancellation_token.clone());
        self.manager
            .open_session_with_exec_env(
                req.process_id,
                command,
                options,
                attempt,
                managed_network,
                network_proxy_launch,
                /*environment_id*/ Some(&req.turn_environment.environment_id),
                req.exec_server_env_config.clone(),
                windows_sandbox_proxy_settings_mode,
                req.tty,
                Box::new(NoopSpawnLifecycle),
                req.turn_environment.environment.as_ref(),
            )
            .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::exec::DEFAULT_EXEC_COMMAND_TIMEOUT_MS;
    use crate::tools::sandboxing::ToolRuntime;
    use codex_exec_server::Environment;
    use codex_exec_server::LOCAL_ENVIRONMENT_ID;
    use codex_tools::ZshForkConfig;
    use codex_utils_absolute_path::AbsolutePathBuf;
    use codex_utils_path_uri::PathUri;
    use std::sync::Arc;
    use std::time::Duration;
    use tempfile::tempdir;

    fn test_turn_environment(cwd: PathUri) -> TurnEnvironment {
        TurnEnvironment::new(
            LOCAL_ENVIRONMENT_ID.to_string(),
            Arc::new(Environment::default_for_tests()),
            cwd,
            Vec::new(),
            /*shell*/ None,
        )
    }

    /// #722 (D246): remote network decisions reach this machine for the
    /// user too, except in limited mode, which keeps failing closed remotely.
    #[test]
    fn remote_network_decisions_route_here_except_user_limited_mode() {
        let hooks = Duration::from_secs(7);
        let margin = REMOTE_NETWORK_POLICY_DECISION_MARGIN;
        assert_eq!(
            remote_network_policy_decision_timeout(
                /*routes_to_guardian*/ false,
                NetworkMode::Full,
                hooks,
            ),
            Some(hooks + REMOTE_NETWORK_USER_DECISION_TIMEOUT + margin)
        );
        assert_eq!(
            remote_network_policy_decision_timeout(
                /*routes_to_guardian*/ false,
                NetworkMode::Limited,
                hooks,
            ),
            None
        );
        for mode in [NetworkMode::Full, NetworkMode::Limited] {
            assert_eq!(
                remote_network_policy_decision_timeout(
                    /*routes_to_guardian*/ true, mode, hooks
                ),
                Some(hooks + GUARDIAN_REVIEW_TIMEOUT + margin)
            );
        }
    }

    #[test]
    fn unified_exec_options_combines_default_timeout_with_network_denial_cancellation() {
        let cancellation = CancellationToken::new();
        let options = unified_exec_options(Some(cancellation.clone()));

        assert_eq!(options.capture_policy, ExecCapturePolicy::ShellTool);
        match options.expiration {
            ExecExpiration::TimeoutOrCancellation {
                timeout,
                cancellation: actual,
            } => {
                assert_eq!(
                    timeout,
                    Duration::from_millis(DEFAULT_EXEC_COMMAND_TIMEOUT_MS)
                );
                cancellation.cancel();
                assert!(actual.is_cancelled());
            }
            other => panic!("expected timeout-or-cancellation expiration, got {other:?}"),
        }
    }

    /// #466: the last review of the command decides whether Core must end
    /// it; a network review neither shows nor ends it.
    #[tokio::test]
    async fn the_last_command_review_decides_whether_core_ends_it() {
        use codex_protocol::approvals::NetworkApprovalContext;
        use codex_protocol::approvals::NetworkApprovalProtocol;
        use codex_protocol::approvals::NetworkPolicyAmendment;
        let (session, turn, _rx) = crate::session::tests::make_session_and_context_with_rx().await;
        let ctx = |network: bool| ApprovalCtx {
            session: &session,
            turn: &turn,
            call_id: "call-466",
            retry_reason: None,
            network_approval_context: network.then(|| NetworkApprovalContext {
                host: "example.com".to_string(),
                protocol: NetworkApprovalProtocol::Https,
            }),
            fresh_human_authority: false,
        };
        let amendment = |action| ReviewDecision::NetworkPolicyAmendment {
            network_policy_amendment: NetworkPolicyAmendment {
                host: "example.com".to_string(),
                action,
            },
        };
        let manager = UnifiedExecProcessManager::default();
        let mut runtime = UnifiedExecRuntime::new(&manager, UnifiedExecShellMode::Direct);
        assert!(!runtime.shown_by_review());
        for (decision, shown) in [
            (ReviewDecision::Approved, true),
            (ReviewDecision::denied("no"), false),
            (ReviewDecision::ApprovedForSession, true),
            (ReviewDecision::TimedOut, false),
            (amendment(NetworkPolicyRuleAction::Allow), true),
            (ReviewDecision::Abort, false),
            (amendment(NetworkPolicyRuleAction::Deny), false),
        ] {
            runtime.note_review_decision(&decision, &ctx(/*network*/ false));
            assert_eq!(runtime.shown_by_review(), shown, "{decision:?}");
        }
        // A network review after an approval leaves the command shown.
        runtime.note_review_decision(&ReviewDecision::Approved, &ctx(/*network*/ false));
        runtime.note_review_decision(&ReviewDecision::denied("no"), &ctx(/*network*/ true));
        assert!(runtime.shown_by_review());
    }

    #[tokio::test]
    async fn approval_key_includes_environment_id() {
        let manager = UnifiedExecProcessManager::default();
        let runtime = UnifiedExecRuntime::new(&manager, UnifiedExecShellMode::Direct);
        let mut request = test_request(
            SandboxPermissions::UseDefault,
            ExecApprovalRequirement::Skip {
                bypass_sandbox: false,
                proposed_execpolicy_amendment: None,
            },
        );
        request.turn_environment.environment_id = "remote".to_string();
        let original_key = runtime.approval_keys(&request);
        request.turn_environment.environment_id = "other".to_string();
        let other_key = runtime.approval_keys(&request);

        assert_ne!(original_key, other_key);
    }

    #[tokio::test]
    async fn unified_exec_uses_the_trusted_sandbox_cwd() {
        let cwd_dir = tempdir().expect("create process temp dir");
        let sandbox_dir = tempdir().expect("create sandbox temp dir");
        let cwd =
            AbsolutePathBuf::try_from(cwd_dir.path().to_path_buf()).expect("absolute temp dir");
        let sandbox_cwd = AbsolutePathBuf::try_from(sandbox_dir.path().to_path_buf())
            .expect("absolute sandbox temp dir");
        let manager = UnifiedExecProcessManager::default();
        let runtime = UnifiedExecRuntime::new(&manager, UnifiedExecShellMode::Direct);
        let request = UnifiedExecRequest {
            command: vec!["pwd".to_string()],
            shell_type: ShellType::Sh,
            hook_command: "pwd".to_string(),
            process_id: 1000,
            cwd: cwd.into(),
            sandbox_cwd: sandbox_cwd.clone().into(),
            turn_environment: test_turn_environment(sandbox_cwd.clone().into()),
            env: HashMap::new(),
            exec_server_env_config: None,
            explicit_env_overrides: HashMap::new(),
            network: None,
            tty: false,
            sandbox_permissions: SandboxPermissions::UseDefault,
            additional_permissions: None,
            #[cfg(unix)]
            additional_permissions_preapproved: false,
            justification: None,
            exec_approval_requirement: ExecApprovalRequirement::Skip {
                bypass_sandbox: false,
                proposed_execpolicy_amendment: None,
            },
        };

        assert_eq!(
            runtime.sandbox_cwd(&request),
            Some(&PathUri::from_abs_path(&sandbox_cwd))
        );
    }

    #[tokio::test]
    async fn zsh_fork_first_attempt_preserves_parent_sandbox_override() {
        let manager = UnifiedExecProcessManager::default();
        let request = test_request(
            SandboxPermissions::RequireEscalated,
            ExecApprovalRequirement::NeedsApproval {
                reason: None,
                proposed_execpolicy_amendment: None,
            },
        );
        let direct_runtime = UnifiedExecRuntime::new(&manager, UnifiedExecShellMode::Direct);
        let zsh_fork_runtime = UnifiedExecRuntime::new(&manager, zsh_fork_mode());

        assert_eq!(
            direct_runtime.sandbox_permissions(&request),
            SandboxPermissions::RequireEscalated,
            "direct unified exec should preserve a parent require_escalated request"
        );
        assert_eq!(
            zsh_fork_runtime.sandbox_permissions(&request),
            SandboxPermissions::RequireEscalated,
            "zsh-fork unified exec should preserve the same parent require_escalated request"
        );
    }

    #[tokio::test]
    async fn zsh_fork_first_attempt_preserves_additional_permissions_request() {
        let manager = UnifiedExecProcessManager::default();
        let request = test_request(
            SandboxPermissions::WithAdditionalPermissions,
            ExecApprovalRequirement::NeedsApproval {
                reason: None,
                proposed_execpolicy_amendment: None,
            },
        );
        let zsh_fork_runtime = UnifiedExecRuntime::new(&manager, zsh_fork_mode());

        assert_eq!(
            zsh_fork_runtime.sandbox_permissions(&request),
            SandboxPermissions::WithAdditionalPermissions,
            "zsh-fork unified exec should keep bounded additional-permissions requests sandboxed"
        );
    }

    #[tokio::test]
    async fn zsh_fork_execpolicy_allow_preserves_parent_sandbox_override() {
        let manager = UnifiedExecProcessManager::default();
        let request = test_request(
            SandboxPermissions::UseDefault,
            ExecApprovalRequirement::Skip {
                bypass_sandbox: true,
                proposed_execpolicy_amendment: None,
            },
        );
        let runtime = UnifiedExecRuntime::new(&manager, zsh_fork_mode());

        assert_eq!(
            runtime.exec_approval_requirement(&request),
            Some(ExecApprovalRequirement::Skip {
                bypass_sandbox: true,
                proposed_execpolicy_amendment: None,
            }),
            "zsh-fork unified exec should preserve exec-policy allow decisions that bypass the sandbox"
        );
    }

    fn test_request(
        sandbox_permissions: SandboxPermissions,
        exec_approval_requirement: ExecApprovalRequirement,
    ) -> UnifiedExecRequest {
        let cwd = AbsolutePathBuf::try_from(std::env::current_dir().unwrap())
            .expect("current dir is absolute");
        UnifiedExecRequest {
            command: vec!["zsh".to_string(), "-c".to_string(), "echo hi".to_string()],
            shell_type: ShellType::Zsh,
            hook_command: "echo hi".to_string(),
            process_id: 1000,
            cwd: cwd.clone().into(),
            sandbox_cwd: cwd.clone().into(),
            turn_environment: test_turn_environment(cwd.into()),
            env: HashMap::new(),
            exec_server_env_config: None,
            explicit_env_overrides: HashMap::new(),
            network: None,
            tty: false,
            sandbox_permissions,
            additional_permissions: None,
            #[cfg(unix)]
            additional_permissions_preapproved: false,
            justification: None,
            exec_approval_requirement,
        }
    }

    fn zsh_fork_mode() -> UnifiedExecShellMode {
        let cwd = std::env::current_dir().expect("read current dir");
        UnifiedExecShellMode::ZshFork(ZshForkConfig {
            shell_zsh_path: AbsolutePathBuf::try_from(cwd.join("zsh")).expect("absolute zsh path"),
            main_execve_wrapper_exe: AbsolutePathBuf::try_from(cwd.join("execve-wrapper"))
                .expect("absolute wrapper path"),
        })
    }
}
