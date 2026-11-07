/*
Module: orchestrator

Central place for approvals + sandbox selection + retry semantics. Drives a
simple sequence for any ToolRuntime: approval → select sandbox → attempt →
retry with an escalated sandbox strategy on denial (no re‑approval thanks to
caching).
*/
use super::approvals::ApprovalResolutionSource;
use super::approvals::ApprovalReviewer;
use super::approvals::DeclineRecord;
use super::approvals::resolve_tool_apporval;
use crate::network_policy_decision::network_approval_context_from_payload;
use crate::tools::flat_tool_name;
use crate::tools::network_approval::ActiveNetworkApproval;
use crate::tools::network_approval::DeferredNetworkApproval;
use crate::tools::network_approval::NetworkApprovalMode;
use crate::tools::network_approval::begin_network_approval;
use crate::tools::network_approval::finish_deferred_network_approval;
use crate::tools::network_approval::finish_immediate_network_approval;
use crate::tools::sandboxing::ApprovalAction;
use crate::tools::sandboxing::ApprovalCtx;
use crate::tools::sandboxing::ExecApprovalRequirement;
use crate::tools::sandboxing::SandboxAttempt;
use crate::tools::sandboxing::SandboxOverride;
use crate::tools::sandboxing::ToolCtx;
use crate::tools::sandboxing::ToolError;
use crate::tools::sandboxing::ToolRuntime;
use crate::tools::sandboxing::default_exec_approval_requirement;
use crate::tools::sandboxing::sandbox_override_for_first_attempt;
use crate::tools::sandboxing::unsandboxed_execution_allowed;
use codex_otel::ToolDecisionSource;
use codex_protocol::config_types::WindowsSandboxLevel;
use codex_protocol::error::CodexErrorDetails;
use codex_protocol::error::SandboxErr;
use codex_protocol::exec_output::ExecToolCallOutput;
use codex_protocol::protocol::AskForApproval;
use codex_protocol::protocol::ReviewDecision;
use codex_sandboxing::SandboxManager;
use codex_sandboxing::SandboxType;
use codex_utils_path_uri::PathUri;
use std::time::Instant;

pub(crate) struct ToolOrchestrator {
    sandbox: SandboxManager,
    handler_basis: Option<HandlerBasis>,
}

/// #1057: the policy a tool's handler read its `request_permissions` grants
/// (and made its exec-policy decision) under, taken just before the grants
/// were read, so a transition committed before the run reached the
/// orchestrator is noticed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct HandlerBasis {
    marker: crate::state::AuthorityMarker,
    aggressive: bool,
}

impl HandlerBasis {
    pub(crate) fn now(session: &crate::session::session::Session) -> Self {
        Self {
            marker: session.services.authority_marker(),
            aggressive: session.services.model_client().live_level_is_aggressive(),
        }
    }
}

pub(crate) struct OrchestratorRunResult<Out> {
    pub output: Out,
    pub deferred_network_approval: Option<DeferredNetworkApproval>,
}

impl ToolOrchestrator {
    pub fn new() -> Self {
        Self {
            sandbox: SandboxManager::new(),
            handler_basis: None,
        }
    }

    /// #1057: the run was prepared by a handler under `basis`.
    pub(crate) fn with_handler_basis(mut self, basis: Option<HandlerBasis>) -> Self {
        self.handler_basis = basis;
        self
    }

    async fn run_attempt<Rq, Out, T>(
        tool: &mut T,
        req: &Rq,
        tool_ctx: &ToolCtx,
        attempt: &SandboxAttempt<'_>,
        managed_network_active: bool,
    ) -> (Result<Out, ToolError>, Option<DeferredNetworkApproval>)
    where
        T: ToolRuntime<Rq, Out>,
    {
        let network_approval = match begin_network_approval(
            &tool_ctx.session,
            &tool_ctx.turn.sub_id,
            managed_network_active,
            tool.network_approval_spec(req, tool_ctx),
        )
        .await
        {
            Ok(network_approval) => network_approval,
            Err(err) => return (Err(err), None),
        };

        let attempt_tool_ctx = ToolCtx {
            session: tool_ctx.session.clone(),
            turn: tool_ctx.turn.clone(),
            call_id: tool_ctx.call_id.clone(),
            tool_name: tool_ctx.tool_name.clone(),
        };
        let attempt_with_network_approval = SandboxAttempt {
            sandbox: attempt.sandbox,
            sandbox_requested: attempt.sandbox_requested,
            permissions: attempt.permissions,
            exec_server_permissions: attempt.exec_server_permissions,
            enforce_managed_network: attempt.enforce_managed_network,
            manager: attempt.manager,
            sandbox_cwd: attempt.sandbox_cwd,
            workspace_roots: attempt.workspace_roots,
            codex_linux_sandbox_exe: attempt.codex_linux_sandbox_exe,
            use_legacy_landlock: attempt.use_legacy_landlock,
            windows_sandbox_level: attempt.windows_sandbox_level,
            windows_sandbox_private_desktop: attempt.windows_sandbox_private_desktop,
            network_denial_cancellation_token: network_approval
                .as_ref()
                .map(ActiveNetworkApproval::cancellation_token),
            network_proxy: network_approval
                .as_ref()
                .map(ActiveNetworkApproval::execution_proxy),
        };
        let unlabelled_mark = codex_windows_sandbox::unlabelled_roots_mark();
        let run_result = tool
            .run(req, &attempt_with_network_approval, &attempt_tool_ctx)
            .await;
        warn_unlabelled_writable_roots(tool, req, tool_ctx, attempt, unlabelled_mark).await;

        let Some(network_approval) = network_approval else {
            return (run_result, None);
        };

        match network_approval.mode() {
            NetworkApprovalMode::Immediate => {
                let finalize_result =
                    finish_immediate_network_approval(&tool_ctx.session, network_approval).await;
                if let Err(err) = finalize_result {
                    return (Err(err), None);
                }
                (run_result, None)
            }
            NetworkApprovalMode::Deferred => {
                let deferred = network_approval.into_deferred();
                if run_result.is_err() {
                    let finalize_result =
                        finish_deferred_network_approval(&tool_ctx.session, deferred).await;
                    if let Err(err) = finalize_result {
                        return (Err(err), None);
                    }
                    return (run_result, None);
                }
                (run_result, deferred)
            }
        }
    }

    /// Approves, selects the sandbox and runs the tool. D50 (#1001): when a
    /// security transition is committed while a run is being approved (up to
    /// its first attempt), the answer was for a stale prompt: nothing runs on
    /// it, and the run is decided again under the new level, the person asked
    /// again (as network access is, #945). Where the transition switched
    /// Aggressive on, the run's exec-policy decision (made by the tool's
    /// handler) is stale too, so it is refused instead: a new call is decided
    /// under Aggressive. A retry outside the sandbox approved (or due) before
    /// a transition is not run. #1057: a run whose handler read its grants
    /// before a transition is handled the same way before anyone is asked,
    /// as if its first round had gone stale.
    pub async fn run<Rq, Out, T>(
        &mut self,
        tool: &mut T,
        req: &Rq,
        tool_ctx: &ToolCtx,
        turn_ctx: &crate::session::turn_context::TurnContext,
        approval_policy: AskForApproval,
    ) -> Result<OrchestratorRunResult<Out>, ToolError>
    where
        T: ToolRuntime<Rq, Out>,
    {
        let aggressive_at_start = match self.handler_basis {
            Some(basis) => basis.aggressive,
            None => tool_ctx
                .session
                .services
                .model_client()
                .live_level_is_aggressive(),
        };
        // The first round is decided under the handler's policy, so a
        // transition committed from here on is noticed by it too. A basis
        // without a marker (the policy not initialized then; not in real
        // sessions) notices nothing, as before.
        let mut first_under = self.handler_basis.and_then(|basis| basis.marker);
        if tool_ctx.session.services.transition_since(first_under) {
            tracing::info!(
                target: "codex_core::security::transition",
                call_id = %tool_ctx.call_id,
                "security transition after the handler read its grants"
            );
            stale_refusal(
                tool,
                req,
                tool_ctx,
                aggressive_at_start,
                /*before_decided*/ true,
            )
            .await?;
            first_under = None;
        }
        let mut asked_again = false;
        for _ in 0..MAX_STALE_APPROVAL_ROUNDS {
            // D50: the policy this round is decided under.
            let decided_under = first_under
                .take()
                .or_else(|| tool_ctx.session.services.authority_marker());
            match Box::pin(self.run_round(
                tool,
                req,
                tool_ctx,
                turn_ctx,
                approval_policy,
                asked_again,
                decided_under,
            ))
            .await?
            {
                Round::Done(result) => return Ok(result),
                Round::Stale { person_asked } => {
                    tracing::info!(
                        target: "codex_core::security::transition",
                        call_id = %tool_ctx.call_id,
                        "approval answered after a security transition; not used"
                    );
                    stale_refusal(
                        tool,
                        req,
                        tool_ctx,
                        aggressive_at_start,
                        /*before_decided*/ false,
                    )
                    .await?;
                    // The note is for a person who saw the earlier prompt.
                    asked_again = person_asked;
                }
            }
        }
        warn(tool_ctx, STALE_APPROVAL_REFUSAL).await;
        Err(ToolError::Rejected(STALE_APPROVAL_REFUSAL.to_string()))
    }

    /// One decision and run; [`Round::Stale`] when a security transition was
    /// committed before its first attempt started. `decided_under`: the
    /// policy marker this round is decided under (D50, #1057).
    #[expect(clippy::too_many_arguments)]
    async fn run_round<Rq, Out, T>(
        &mut self,
        tool: &mut T,
        req: &Rq,
        tool_ctx: &ToolCtx,
        turn_ctx: &crate::session::turn_context::TurnContext,
        approval_policy: AskForApproval,
        asked_again: bool,
        decided_under: crate::state::AuthorityMarker,
    ) -> Result<Round<Out>, ToolError>
    where
        T: ToolRuntime<Rq, Out>,
    {
        let otel = turn_ctx.session_telemetry.clone();
        let otel_tn = flat_tool_name(&tool_ctx.tool_name).into_owned();
        let otel_ci = &tool_ctx.call_id;
        let strict_auto_review = tool_ctx.session.strict_auto_review_enabled_for_turn().await;
        // 1) Approval
        let mut already_approved = false;
        // Issue #513: who approved this run, if anyone was asked.
        let mut approved_by = None;

        let workspace_roots = tool.workspace_roots(req);
        let permission_profile = turn_ctx.config.permissions.permission_profile();
        let materialized_workspace_roots = workspace_roots
            .iter()
            .filter_map(|workspace_root| workspace_root.to_abs_path().ok())
            .collect::<Vec<_>>();
        let permissions = permission_profile
            .clone()
            .materialize_project_roots_with_workspace_roots(&materialized_workspace_roots);
        let file_system_sandbox_policy = permissions.file_system_sandbox_policy();
        let requirement = tool.exec_approval_requirement(req).unwrap_or_else(|| {
            default_exec_approval_requirement(approval_policy, &file_system_sandbox_policy)
        });
        // PF-23-S02: under Aggressive, the grant digest of this exact command.
        // PF-25-S01: while its approval is open, the human may grant it once
        // in the TUI; the offer ends with the approval.
        let grant_operation = aggressive_grant_operation(tool, req, tool_ctx);
        // #623: under Aggressive, a command that creates a link to the
        // project's `.codex` or `.agents` (or Corbanu's own files) is refused
        // before anyone is asked, so nothing is created; it says why. Shell
        // and exec_command requests carry their command here; a new runtime
        // whose `approval_action` has none is not checked.
        if !matches!(requirement, ExecApprovalRequirement::Forbidden { .. })
            && let Some((_, Some((command, cwd)))) = grant_operation.as_ref()
            && let Some(refusal) = crate::security::protected_links::refusal(
                &tool_ctx.session,
                turn_ctx,
                command,
                std::path::Path::new(cwd),
            )
            .await
        {
            return Err(ToolError::Rejected(refusal));
        }
        // #739 (option B, security sign-off): under Permissive with protected
        // launch armed, a request to run outside the sandbox is refused before
        // anyone is asked, since the launch contract would refuse the run. A
        // forbidden request keeps its own refusal; where denied reads already
        // keep the run in the sandbox, nothing is refused.
        let permissive_launch =
            crate::security::protected_additional::permissive_launch_keeps_sandbox(
                &tool_ctx.session,
            );
        if permissive_launch
            && !matches!(requirement, ExecApprovalRequirement::Forbidden { .. })
            && unsandboxed_execution_allowed(&file_system_sandbox_policy)
            && tool
                .sandbox_permissions(req)
                .requires_escalated_permissions()
        {
            // The person sees it too, not only the model (an exec_command
            // also gets a "Refused: <this reason>" cell since #873).
            warn(
                tool_ctx,
                crate::security::protected_additional::PROTECTED_LAUNCH_NO_ESCALATION,
            )
            .await;
            return Err(ToolError::Rejected(
                crate::security::protected_additional::PROTECTED_LAUNCH_NO_ESCALATION.to_string(),
            ));
        }
        // Registered just before the user (not an automatic reviewer) is
        // asked. A cached approval or a permission hook may still answer
        // without them; then no review is shown and nothing is confirmed.
        let mut grant_offer = None;
        // #747 (D3): under Moderate a run never leaves the sandbox.
        let moderate_keeps_sandbox =
            crate::security::protected_additional::moderate_keeps_sandbox(&tool_ctx.session);
        // Issue #513: under Moderate, the persistence file a person's approval
        // of this request would let it write, for the approval prompt.
        let mut persistence_note = None;
        // PF-30-S03: a protected action after untrusted content needs fresh,
        // exact human approval, whatever the requirement above would allow.
        let post_taint = post_taint_action(tool, req, tool_ctx, &requirement).await;
        // D27 (a), D48 (c): the taint generation at which the person was
        // warned, in the prompt they answer, that approving lets this run
        // create user namespaces (only a run whose prompt showed it may).
        let mut user_namespaces_warned_at = None;
        // Issue #245: additional permissions never reopen a protected path.
        // Refused before anyone is asked where no approval can lift the
        // rules for this run; checked again below once the rules are known.
        // Issue #513: nor a runtime entry of the Corbanu home (`tmp` holds
        // the sandbox launchers) for writing, even where an approval lifts
        // the other rules. Other write protections wait for the second
        // check: an Aggressive grant confirmed in the approval, or under
        // Moderate a person's fresh approval of this command, may lift them;
        // under Moderate they count here already where no person will decide
        // this run (the automatic reviewer, or nobody, is asked).
        // A forbidden request keeps its own refusal; an external sandbox
        // takes no protected-path rules and ignores additional permissions.
        if let Some(state) = tool_ctx
            .session
            .services
            .model_client()
            .post_taint_state()
            .filter(crate::security::tainted_action::PostTaintState::protected_paths_apply)
            && !matches!(requirement, ExecApprovalRequirement::Forbidden { .. })
            && !matches!(
                permission_profile,
                codex_protocol::models::PermissionProfile::External { .. }
            )
            && let Some(additional) = tool.additional_permissions(req)
        {
            use crate::security::protected_additional;
            let human_route = post_taint.is_some()
                || moderate_escalation_before_taint(tool, req, tool_ctx, &requirement);
            let reads = !(state.moderate_bound() && human_route);
            let person_may_decide = human_route
                || (matches!(requirement, ExecApprovalRequirement::NeedsApproval { .. })
                    && !strict_auto_review
                    && ApprovalReviewer::for_turn(turn_ctx) == ApprovalReviewer::User);
            let writes = protected_additional::ReadOnlyScope::before_approval(
                state.level,
                person_may_decide,
            );
            #[allow(deprecated)]
            let turn_cwd = turn_ctx.cwd.clone();
            let cwds = resolution_folders(tool, req, &turn_cwd, &materialized_workspace_roots);
            let codex_home = tool_ctx.turn.config.codex_home.to_path_buf();
            let sqlite_home = tool_ctx.turn.config.sqlite.home().to_path_buf();
            let (roots, profile, additional) = (
                materialized_workspace_roots.clone(),
                permissions.clone(),
                additional.clone(),
            );
            // Issue #524: building the denials reads folders; off the async
            // thread. A failed check refuses (fail closed).
            // Where a person decides, say which persistence file approving
            // opens (the second check refuses it for any other approver).
            let note_persistence = state.level == codex_security_policy::SecurityLevel::Moderate
                && !human_route
                && person_may_decide;
            let checked = tokio::task::spawn_blocking(move || {
                let denials = crate::security::protected_surface::ReadDenials::for_turn(
                    &codex_home,
                    &sqlite_home,
                    &turn_cwd,
                    &roots,
                    &profile,
                );
                let protected = denials
                    .apply(&profile)
                    .map(|protected| protected.file_system_sandbox_policy());
                let check = protected_additional::Check {
                    denials: &denials,
                    protected: protected.as_ref(),
                    reads,
                    writes,
                };
                let cwds: Vec<&std::path::Path> = cwds.iter().map(AsRef::as_ref).collect();
                let liftable = note_persistence
                    .then(|| {
                        protected_additional::opened_read_only_path(
                            &denials,
                            protected.as_ref(),
                            &additional,
                            &cwds,
                            protected_additional::ReadOnlyScope::Kept,
                        )
                    })
                    .flatten();
                (
                    protected_additional::opened(check, &additional, &cwds),
                    liftable,
                )
            })
            .await
            .unwrap_or((
                Some(crate::security::protected_additional::Opened::Unchecked),
                None,
            ));
            let (opened, liftable) = checked;
            refuse_opened(opened, state.level, tool_ctx).await?;
            persistence_note = liftable
                .map(|path| protected_additional::persistence_note(&path.display().to_string()));
        }
        // #943 item 17: under Moderate or Aggressive, a repository this
        // session hasn't looked at yet, whose git folder this run may write,
        // gets its first look before anyone decides the run: what was left
        // there before (before Corbanu started, say) is told first and holds
        // the approval (`git_refresh`). Judged before the protected-file rules
        // apply (they only narrow what the run may write), and anywhere the
        // run may leave the sandbox.
        let git_first_look_failed =
            match crate::security::protected_additional::protected_level(&tool_ctx.session) {
                Some(level)
                    if level != codex_security_policy::SecurityLevel::Permissive
                        && !matches!(requirement, ExecApprovalRequirement::Forbidden { .. }) =>
                {
                    let effective =
                        codex_sandboxing::policy_transforms::effective_file_system_sandbox_policy(
                            &file_system_sandbox_policy,
                            tool.additional_permissions(req),
                        );
                    // Where the rules apply and no approval lifts them (Aggressive),
                    // their denied reads keep the run in the sandbox (#540); under
                    // Moderate it never leaves it (#747).
                    let rules_kept = tool_ctx
                        .session
                        .services
                        .model_client()
                        .post_taint_state()
                        .is_some_and(|state| {
                            state.protected_paths_apply() && !state.moderate_bound()
                        });
                    let may_leave = !moderate_keeps_sandbox
                        && !rules_kept
                        && ((tool
                            .sandbox_permissions(req)
                            .requires_escalated_permissions()
                            && matches!(
                                sandbox_override_for_first_attempt(
                                    tool.sandbox_permissions(req),
                                    &requirement,
                                    &file_system_sandbox_policy,
                                ),
                                SandboxOverride::BypassSandboxFirstAttempt
                            ))
                            || unsandboxed_execution_allowed(&file_system_sandbox_policy));
                    let unsandboxed = may_leave
                        || self.sandbox.select_initial(
                            &permissions,
                            tool.sandbox_preference(),
                            turn_ctx.windows_sandbox_level,
                            turn_ctx.network.is_some(),
                        ) == SandboxType::None;
                    GitCheck::first_look(
                        tool_ctx,
                        level,
                        git_check_folders(tool, req, turn_ctx, &materialized_workspace_roots),
                        tool.command_cwd(req).map(|cwd| cwd.to_path_buf()),
                        effective,
                        unsandboxed,
                    )
                    .await
                    .is_err()
                }
                _ => false,
            };
        if let Some(action) = &post_taint {
            if let Some(refusal) = action.refused_up_front() {
                post_taint_outcome(
                    action,
                    tool_ctx,
                    "refused_kill_switch",
                    /*waited*/ None,
                );
                return Err(ToolError::Rejected(refusal));
            }
            if approval_policy == AskForApproval::Never {
                post_taint_outcome(
                    action,
                    tool_ctx,
                    "refused_approvals_off",
                    /*waited*/ None,
                );
                return Err(ToolError::Rejected(action.approvals_off_rejection()));
            }
            let protected_read_note =
                protected_read_note(grant_operation.as_ref(), tool_ctx, turn_ctx).await;
            // #841: what approving cannot unlock under Moderate.
            let kept_read_note =
                moderate_kept_read_note(tool, req, tool_ctx, turn_ctx, action).await;
            // #747: and that it stays in the sandbox under Moderate.
            let stays_sandboxed = moderate_sandbox_note(tool, req, moderate_keeps_sandbox);
            // D48 (c): a known tool's request to run outside the sandbox may
            // create user namespaces if approved here, with the warning;
            // #826: elsewhere, that what needs one fails on Linux.
            let warning = tool
                .sandbox_permissions(req)
                .requires_escalated_permissions()
                .then(|| {
                    crate::security::user_namespaces::one_run_warning(
                        &tool_ctx.session,
                        turn_ctx,
                        &|| {
                            runs_known_tool(
                                tool,
                                req,
                                tool_ctx,
                                turn_ctx,
                                &file_system_sandbox_policy,
                            )
                        },
                    )
                })
                .flatten();
            let user_namespaces_note = match warning {
                Some((warning, at)) => {
                    user_namespaces_warned_at = Some(at);
                    Some(warning)
                }
                None => user_namespaces_note(tool, req, tool_ctx, turn_ctx).map(|(note, _)| note),
            };
            let approval_ctx = ApprovalCtx {
                session: &tool_ctx.session,
                turn: &tool_ctx.turn,
                call_id: &tool_ctx.call_id,
                retry_reason: Some(
                    [
                        Some(action.approval_reason()),
                        protected_read_note.clone(),
                        kept_read_note,
                        stays_sandboxed.map(str::to_string),
                        user_namespaces_note,
                    ]
                    .into_iter()
                    .flatten()
                    .collect::<Vec<_>>()
                    .join(" "),
                ),
                network_approval_context: None,
                fresh_human_authority: true,
            };
            let asked = Instant::now();
            // #442: no grant for an action whose reads stay denied anyway.
            if !action.reads_stay_denied() && protected_read_note.is_none() {
                grant_offer = aggressive_grant_offer(grant_operation.as_ref(), tool_ctx);
            }
            let mut pending = crate::security::inspection::PendingProtectedAction::new(
                tool_ctx.session.thread_id(),
                action.kind,
            );
            let decision = resolve_tool_apporval(
                tool,
                req,
                tool_ctx.call_id.as_str(),
                approval_ctx,
                tool_ctx,
                ApprovalReviewer::User,
                &otel,
                DeclineRecord::ByCaller,
                asked_again,
            )
            .await;
            pending.answered();
            // D50: approved before a security transition; decided again
            // (the recheck below would refuse it).
            if decision.is_ok() && tool_ctx.session.services.transition_since(decided_under) {
                post_taint_outcome(action, tool_ctx, "stale", Some(asked.elapsed()));
                return Ok(Round::Stale { person_asked: true });
            }
            post_taint_outcome(
                action,
                tool_ctx,
                if decision.is_ok() {
                    "approved"
                } else {
                    "declined"
                },
                Some(asked.elapsed()),
            );
            approved_by = Some(decision?);
            post_taint_recheck(action, tool_ctx)?;
            already_approved = true;
        }
        // Issue #239: under Moderate before untrusted content, a command that
        // asks to run outside the sandbox needs the human (no cache, hook or
        // automatic reviewer), also where it would run unasked; that answer
        // lifts the protected-path rules. With approvals off it stays refused.
        let asks_sandbox_approval = match approval_policy {
            AskForApproval::Never => false,
            AskForApproval::Granular(granular) => granular.allows_sandbox_approval(),
            AskForApproval::UnlessTrusted | AskForApproval::OnRequest => true,
        };
        let ask_human_to_escalate = post_taint.is_none()
            && !matches!(requirement, ExecApprovalRequirement::Forbidden { .. })
            && asks_sandbox_approval
            && moderate_escalation_before_taint(tool, req, tool_ctx, &requirement);
        // D48 (c) (Travis, 2026-10-20, security sign-off): after untrusted
        // content under Moderate on Linux, a request to run outside the
        // sandbox whose program is a known tool (unshare, bwrap, podman,
        // buildah, flatpak, nix) is asked about afresh, every time, by the
        // person (no approval cache, hook, automatic reviewer or saved rule),
        // with D27's warning: approving lets that one run create user
        // namespaces. It lifts nothing else: the protected-file rules stay.
        let after_untrusted_warning = if post_taint.is_none()
            && !ask_human_to_escalate
            && !matches!(
                requirement,
                ExecApprovalRequirement::Forbidden { .. }
                    | ExecApprovalRequirement::Skip {
                        bypass_sandbox: true,
                        ..
                    }
            )
            && asks_sandbox_approval
            && tool
                .sandbox_permissions(req)
                .requires_escalated_permissions()
        {
            crate::security::user_namespaces::one_run_warning(&tool_ctx.session, turn_ctx, &|| {
                runs_known_tool(tool, req, tool_ctx, turn_ctx, &file_system_sandbox_policy)
            })
            .filter(|(_, at)| *at > 0)
        } else {
            None
        };
        let mut escalation_approved_by_human = false;
        match &requirement {
            _ if post_taint.is_some() => {}
            _ if ask_human_to_escalate => {
                // #747: and it stays inside the sandbox.
                let note = crate::security::protected_additional::moderate_escalation_note();
                let mut approval_ctx = ApprovalCtx {
                    session: &tool_ctx.session,
                    turn: &tool_ctx.turn,
                    call_id: &tool_ctx.call_id,
                    retry_reason: None,
                    network_approval_context: None,
                    fresh_human_authority: true,
                };
                // Keep why the command runs ahead of the note.
                let why = escalation_why(tool, req, &requirement, &approval_ctx);
                // D27 (a): on Linux, approving also lets this one run create
                // user namespaces, and the person is warned (only a run whose
                // prompt showed it may); elsewhere (#826), that what needs one
                // fails. Before untrusted content only: if some arrived
                // since this was decided, no warning.
                let warning = crate::security::user_namespaces::one_run_warning(
                    &tool_ctx.session,
                    turn_ctx,
                    &|| false,
                )
                .map(|(warning, at)| {
                    user_namespaces_warned_at = Some(at);
                    warning
                });
                let note = match warning.or_else(|| {
                    user_namespaces_note(tool, req, tool_ctx, turn_ctx).map(|(note, _)| note)
                }) {
                    Some(user_namespaces) => format!("{note} {user_namespaces}"),
                    None => note,
                };
                approval_ctx.retry_reason = Some(match why {
                    Some(why) => format!("{why} {note}"),
                    None => note,
                });
                approved_by = Some(
                    resolve_tool_apporval(
                        tool,
                        req,
                        tool_ctx.call_id.as_str(),
                        approval_ctx,
                        tool_ctx,
                        ApprovalReviewer::User,
                        &otel,
                        DeclineRecord::Here,
                        asked_again,
                    )
                    .await?,
                );
                already_approved = true;
                escalation_approved_by_human = true;
            }
            _ if after_untrusted_warning.is_some() => {
                // Set: the guard holds.
                let (warning, at) = after_untrusted_warning.clone().unwrap_or_default();
                let approval_ctx = ApprovalCtx {
                    session: &tool_ctx.session,
                    turn: &tool_ctx.turn,
                    call_id: &tool_ctx.call_id,
                    retry_reason: None,
                    network_approval_context: None,
                    fresh_human_authority: true,
                };
                // Why the command runs, what approving may let it write
                // (#513), that it stays in the sandbox (#747), the warning.
                let retry_reason = [
                    escalation_why(tool, req, &requirement, &approval_ctx),
                    persistence_note.clone(),
                    moderate_sandbox_note(tool, req, moderate_keeps_sandbox).map(str::to_string),
                    Some(warning),
                ]
                .into_iter()
                .flatten()
                .collect::<Vec<_>>()
                .join(" ");
                approved_by = Some(
                    resolve_tool_apporval(
                        tool,
                        req,
                        tool_ctx.call_id.as_str(),
                        ApprovalCtx {
                            retry_reason: Some(retry_reason),
                            ..approval_ctx
                        },
                        tool_ctx,
                        ApprovalReviewer::User,
                        &otel,
                        DeclineRecord::Here,
                        asked_again,
                    )
                    .await?,
                );
                already_approved = true;
                user_namespaces_warned_at = Some(at);
            }
            ExecApprovalRequirement::Skip { .. } => {
                if strict_auto_review {
                    let approval_ctx = ApprovalCtx {
                        session: &tool_ctx.session,
                        turn: &tool_ctx.turn,
                        call_id: &tool_ctx.call_id,
                        retry_reason: None,
                        network_approval_context: None,
                        fresh_human_authority: false,
                    };
                    approved_by = Some(
                        resolve_tool_apporval(
                            tool,
                            req,
                            tool_ctx.call_id.as_str(),
                            approval_ctx,
                            tool_ctx,
                            ApprovalReviewer::Guardian,
                            &otel,
                            DeclineRecord::Here,
                            asked_again,
                        )
                        .await?,
                    );
                    already_approved = true;
                } else {
                    otel.tool_decision(
                        &otel_tn,
                        otel_ci,
                        &ReviewDecision::Approved,
                        ToolDecisionSource::Config,
                    );
                }
            }
            ExecApprovalRequirement::Forbidden { reason } => {
                return Err(ToolError::Rejected(reason.clone()));
            }
            ExecApprovalRequirement::NeedsApproval { reason, .. } => {
                let reviewer = if strict_auto_review {
                    ApprovalReviewer::Guardian
                } else {
                    ApprovalReviewer::for_turn(turn_ctx)
                };
                let protected_read_note = if reviewer == ApprovalReviewer::User {
                    protected_read_note(grant_operation.as_ref(), tool_ctx, turn_ctx).await
                } else {
                    None
                };
                // #747: a request to run outside the sandbox stays in it
                // under Moderate; the person is told before approving.
                let stays_sandboxed =
                    moderate_sandbox_note(tool, req, moderate_keeps_sandbox).map(str::to_string);
                // #826: that what needs a user namespace fails on Linux.
                let (user_namespaces_note, why) =
                    match user_namespaces_note(tool, req, tool_ctx, turn_ctx)
                        .filter(|_| reviewer == ApprovalReviewer::User)
                    {
                        // Keep why the command runs (the model's justification)
                        // ahead of the note when there is no other reason.
                        Some((note, justification)) => {
                            (Some(note), justification.filter(|_| reason.is_none()))
                        }
                        None => (None, None),
                    };
                let approval_ctx = ApprovalCtx {
                    session: &tool_ctx.session,
                    turn: &tool_ctx.turn,
                    call_id: &tool_ctx.call_id,
                    retry_reason: [
                        reason.as_ref(),
                        why.as_ref(),
                        protected_read_note.as_ref(),
                        persistence_note
                            .as_ref()
                            .filter(|_| reviewer == ApprovalReviewer::User),
                        stays_sandboxed.as_ref(),
                        user_namespaces_note.as_ref(),
                    ]
                    .into_iter()
                    .flatten()
                    .cloned()
                    .reduce(|joined, part| format!("{joined} {part}")),
                    network_approval_context: None,
                    fresh_human_authority: false,
                };
                if reviewer == ApprovalReviewer::User && protected_read_note.is_none() {
                    grant_offer = aggressive_grant_offer(grant_operation.as_ref(), tool_ctx);
                }
                approved_by = Some(
                    resolve_tool_apporval(
                        tool,
                        req,
                        tool_ctx.call_id.as_str(),
                        approval_ctx,
                        tool_ctx,
                        reviewer,
                        &otel,
                        DeclineRecord::Here,
                        asked_again,
                    )
                    .await?,
                );
                already_approved = true;
            }
        }

        // The approval came back approved (a refusal returned above): the
        // grant the person confirmed for it, if any (PF-25-S01).
        let confirmed_grant = grant_offer
            .as_ref()
            .and_then(crate::security::grant_offer::OfferGuard::take_confirmed);
        drop(grant_offer);
        // D50: approved (or decided) before a security transition: nothing
        // derived from the approval is used (checked again just before the
        // first attempt).
        if tool_ctx.session.services.transition_since(decided_under) {
            return Ok(Round::Stale {
                person_asked: approved_by == Some(ApprovalResolutionSource::User),
            });
        }

        // PF-23-S01 slice 3: after untrusted content the sandbox itself denies
        // credential and Corbanu home reads, whatever the command text says.
        // What stays readable comes from the turn, never from the command's
        // own (model-chosen) working folder.
        #[allow(deprecated)]
        let turn_cwd = turn_ctx.cwd.clone();
        // The taint generation a human approval of this run was given under.
        let human_approved_at = match &post_taint {
            Some(action) => Some(action.state.taint_generation),
            None => escalation_approved_by_human.then_some(0),
        };
        // #874 (D6): the git folders this session watches, looked at again
        // before deciding whether an approval lifts the rules (text typed
        // into a running shell or a process left running may have changed
        // them). #943 item 11: the folder the command runs in (its
        // `workdir`) counts too.
        let mut git_folders =
            resolution_folders(tool, req, &turn_cwd, &materialized_workspace_roots);
        git_folders.extend(tool.command_cwd(req));
        let mut git_held = git_refresh(tool_ctx, git_folders).await;
        // A failed first look (above) holds it too.
        if git_first_look_failed {
            git_held.push("(the check failed)".to_string());
        }
        let mut git_held_back = false;
        let denied = post_taint_read_denials(
            tool_ctx,
            !git_held.is_empty(),
            &mut git_held_back,
            human_approved_at,
            grant_operation
                .as_ref()
                .map(|(operation, _)| operation.as_str()),
            confirmed_grant,
            turn_cwd,
            &materialized_workspace_roots,
            permission_profile,
            &permissions,
        );
        // D48 (c): a person's approval of a known tool's command after
        // untrusted content lifts no rule but would allow user namespaces:
        // held back too, and the person is told why (#943 item 18: only the
        // namespaces, where no rule would have been lifted).
        let rules_held_back = git_held_back;
        if !git_held.is_empty()
            && user_namespaces_warned_at.is_some()
            && approved_by == Some(ApprovalResolutionSource::User)
        {
            git_held_back = true;
        }
        if git_held_back {
            let withheld = if rules_held_back {
                Withheld::Rules
            } else {
                Withheld::Namespaces
            };
            warn(tool_ctx, &git_held_note(&git_held, withheld)).await;
        }
        // A person's approval of this command when asked about it now: it
        // counts for nothing below while the git check's findings stand
        // (#874), like for the rules above.
        let approved_by_person_now =
            approved_by == Some(ApprovalResolutionSource::User) && git_held.is_empty();
        // #826: Core's level, where the protected-path rules apply.
        let protected_level = denied.as_ref().map(|(_, _, _, level)| *level);
        let (denied_exec_server_permissions, permissions) = match denied {
            Some((exec_server, materialized, denials, level)) => {
                // Issue #245: the rules apply to this run, so its additional
                // permissions may not reopen what they deny.
                // Issue #513: nor make a read-only path writable: the
                // runtime entries always; under Aggressive every one a grant
                // for this run did not lift; under Moderate every one unless
                // a person approved this command when asked about it now (not
                // the automatic reviewer, a hook, the approval cache or a
                // grant: Travis, 2026-10-11).
                if let Some(additional) = tool.additional_permissions(req) {
                    #[allow(deprecated)]
                    let turn_cwd = turn_ctx.cwd.clone();
                    let cwds =
                        resolution_folders(tool, req, &turn_cwd, &materialized_workspace_roots);
                    let protected = materialized.file_system_sandbox_policy();
                    let check = crate::security::protected_additional::Check {
                        denials: &denials,
                        protected: Some(&protected),
                        reads: true,
                        writes:
                            crate::security::protected_additional::ReadOnlyScope::after_approval(
                                level,
                                approved_by_person_now,
                            ),
                    };
                    let cwds: Vec<&std::path::Path> = cwds.iter().map(AsRef::as_ref).collect();
                    let opened =
                        crate::security::protected_additional::opened(check, additional, &cwds);
                    // #943 (#1503 review): where the run is refused only
                    // because the findings discount the person's approval,
                    // the refusal alone would read as if none was given: the
                    // person is told why (unless the rules note above was).
                    if opened.is_some()
                        && !rules_held_back
                        && !git_held.is_empty()
                        && approved_by == Some(ApprovalResolutionSource::User)
                    {
                        let counted = crate::security::protected_additional::Check {
                            writes:
                                crate::security::protected_additional::ReadOnlyScope::after_approval(
                                    level, /*by_person*/ true,
                                ),
                            ..check
                        };
                        if crate::security::protected_additional::opened(counted, additional, &cwds)
                            .is_none()
                        {
                            warn(tool_ctx, &git_held_note(&git_held, Withheld::Writes)).await;
                        }
                    }
                    refuse_opened(opened, level, tool_ctx).await?;
                }
                (Some(exec_server), materialized)
            }
            None => (None, permissions),
        };
        // #826 (Travis, 2026-10-15): under Moderate and Aggressive (also
        // where only the launch enforces Aggressive) agent commands may not
        // create user namespaces, in which they could open what the Linux
        // sandbox hides and defeat nested-launch detection. No grant lifts
        // it. Permissive is unchanged.
        // D27 (a) (Travis, 2026-10-19, security sign-off): but for one run
        // under Moderate whose request to run outside the sandbox a person
        // approved when asked now, before untrusted content, after the
        // prompt warned them. D48 (c) (2026-10-20): after untrusted content
        // likewise, for a known tool's command, while no more untrusted
        // content arrived since the warning. Never a remembered answer;
        // never under Aggressive.
        let launch_enforces_aggressive = turn_ctx.config.permissions.launch_enforces_aggressive();
        let user_namespaces_allowed = crate::security::user_namespaces::allows_one_run(
            &tool_ctx.session,
            protected_level,
            launch_enforces_aggressive,
            user_namespaces_warned_at.is_some() && approved_by_person_now,
            user_namespaces_warned_at,
        );
        let (denied_exec_server_permissions, permissions) = if !user_namespaces_allowed
            && crate::security::user_namespaces::rule_applies(
                protected_level,
                launch_enforces_aggressive,
            ) {
            (
                Some(deny_user_namespaces(
                    denied_exec_server_permissions
                        .as_ref()
                        .unwrap_or(permission_profile),
                )),
                deny_user_namespaces(&permissions),
            )
        } else {
            (denied_exec_server_permissions, permissions)
        };
        let permission_profile = denied_exec_server_permissions
            .as_ref()
            .unwrap_or(permission_profile);
        let file_system_sandbox_policy = permissions.file_system_sandbox_policy();

        // Issue #540: where the protected-path rules apply, a run leaves the
        // sandbox only where a person approves running it outside: a request
        // to (`require_escalated`, which needs a person's approval under
        // these levels), or the retry after a sandbox denial of a protected
        // action (asked afresh, checked below). Never through an exec-policy
        // allow rule: in a run whose rules a person lifted, only the sandbox
        // keeps the Corbanu home's runtime entries read-only. (Elsewhere the
        // rules' read denials already keep such runs sandboxed.)
        // #747 (D3): under Moderate nothing leaves the sandbox at all (read
        // again: a level confirmed while the person was asked counts).
        // #739: nor under Permissive with protected launch armed, whose launch
        // contract refuses every run outside the sandbox.
        let permissive_launch = permissive_launch
            || crate::security::protected_additional::permissive_launch_keeps_sandbox(
                &tool_ctx.session,
            );
        let always_sandboxed = permissive_launch
            || moderate_keeps_sandbox
            || crate::security::protected_additional::moderate_keeps_sandbox(&tool_ctx.session);
        let keeps_sandbox = always_sandboxed
            || (crate::security::protected_additional::protected_level(&tool_ctx.session)
                .is_some()
                && !tool
                    .sandbox_permissions(req)
                    .requires_escalated_permissions());
        let policy_allows_no_sandbox =
            !always_sandboxed && unsandboxed_execution_allowed(&file_system_sandbox_policy);

        // 2) First attempt under the selected sandbox.
        let unkept_override = sandbox_override_for_first_attempt(
            tool.sandbox_permissions(req),
            &requirement,
            &file_system_sandbox_policy,
        );
        let sandbox_override = if keeps_sandbox {
            SandboxOverride::NoOverride
        } else {
            unkept_override
        };
        // #739: a remembered approval or allow rule that would skip the
        // sandbox is not applied silently under protected launch: the run
        // stays in it, and the person is told why (once per session).
        if permissive_launch
            && matches!(unkept_override, SandboxOverride::BypassSandboxFirstAttempt)
            && !tool
                .sandbox_permissions(req)
                .requires_escalated_permissions()
            && crate::security::protected_additional::first_rule_note(tool_ctx.session.thread_id())
        {
            warn(
                tool_ctx,
                crate::security::protected_additional::PROTECTED_LAUNCH_RULE_STAYS_SANDBOXED,
            )
            .await;
        }
        let managed_network_active = turn_ctx.network.is_some();
        let sandbox_preference = tool.sandbox_preference();
        let sandbox_requested = match sandbox_override {
            SandboxOverride::BypassSandboxFirstAttempt => false,
            SandboxOverride::NoOverride => self.sandbox.should_sandbox(
                &permissions,
                sandbox_preference,
                managed_network_active,
            ),
        };
        let initial_sandbox = if sandbox_requested {
            self.sandbox.select_initial(
                &permissions,
                sandbox_preference,
                turn_ctx.windows_sandbox_level,
                managed_network_active,
            )
        } else {
            SandboxType::None
        };

        // Platform-specific flag gating is handled by SandboxManager::select_initial.
        let use_legacy_landlock = turn_ctx.config.features.use_legacy_landlock();
        #[allow(deprecated)]
        let sandbox_policy_cwd = tool
            .sandbox_cwd(req)
            .cloned()
            .unwrap_or_else(|| PathUri::from_abs_path(&turn_ctx.cwd));
        let initial_attempt = SandboxAttempt {
            sandbox: initial_sandbox,
            sandbox_requested,
            permissions: &permissions,
            exec_server_permissions: permission_profile,
            enforce_managed_network: managed_network_active,
            manager: &self.sandbox,
            sandbox_cwd: &sandbox_policy_cwd,
            workspace_roots,
            codex_linux_sandbox_exe: turn_ctx.config.codex_linux_sandbox_exe.as_ref(),
            use_legacy_landlock,
            windows_sandbox_level: turn_ctx.windows_sandbox_level,
            windows_sandbox_private_desktop: turn_ctx
                .config
                .permissions
                .windows_sandbox_private_desktop,
            network_denial_cancellation_token: None,
            network_proxy: None,
        };

        // Issue #535: a writable root this run adds (or the session has)
        // that is a link into another root's read-only path is left out of
        // the sandbox; explain it before the run.
        if initial_sandbox != SandboxType::None
            && let Ok(policy_cwd) = sandbox_policy_cwd.to_abs_path()
        {
            let effective =
                codex_sandboxing::policy_transforms::effective_file_system_sandbox_policy(
                    &file_system_sandbox_policy,
                    tool.additional_permissions(req),
                );
            for message in crate::tools::writable_root_notices::excluded_writable_link_warnings(
                tool_ctx.session.thread_id(),
                &effective,
                policy_cwd.as_path(),
            ) {
                tool_ctx
                    .session
                    .send_event(
                        &tool_ctx.turn,
                        codex_protocol::protocol::EventMsg::Warning(
                            codex_protocol::protocol::WarningEvent { message },
                        ),
                    )
                    .await;
            }
        }
        // #874 (D6): under Moderate or Aggressive, where this run (or its
        // retry outside the sandbox) may write a project's git folder, look
        // at it before (once per session) and after each attempt.
        let git_check =
            match crate::security::protected_additional::protected_level(&tool_ctx.session) {
                Some(level) if level != codex_security_policy::SecurityLevel::Permissive => {
                    let folders =
                        git_check_folders(tool, req, turn_ctx, &materialized_workspace_roots);
                    let effective =
                        codex_sandboxing::policy_transforms::effective_file_system_sandbox_policy(
                            &file_system_sandbox_policy,
                            tool.additional_permissions(req),
                        );
                    let unsandboxed =
                        initial_sandbox == SandboxType::None || policy_allows_no_sandbox;
                    // #943 item 11: and the folder the command runs in (its
                    // `workdir`), which may be another repository.
                    let command_cwd = tool.command_cwd(req).map(|cwd| cwd.to_path_buf());
                    GitCheck::begin(
                        tool_ctx,
                        level,
                        folders,
                        command_cwd,
                        effective,
                        unsandboxed,
                    )
                    .await
                }
                _ => None,
            };
        // D50: approved (or decided) before a security transition: nothing
        // runs on it; decided again under the new level. Checked after the
        // git folder's first look (#874), which may wait.
        if tool_ctx.session.services.transition_since(decided_under) {
            return Ok(Round::Stale {
                person_asked: approved_by == Some(ApprovalResolutionSource::User),
            });
        }
        // #1048: only a round that reaches its first attempt gets an
        // allowed-run note. A stale round never ran; its note must not hide
        // a later denial for the same call. A sandbox/network retry reuses
        // these permissions and this one note.
        if user_namespaces_allowed {
            crate::security::user_namespaces::note_allowed_run(
                tool_ctx.session.thread_id(),
                &turn_ctx.sub_id,
                &tool_ctx.call_id,
            );
            tracing::info!(
                target: "codex_core::security::tainted_action",
                call_id = %tool_ctx.call_id,
                "user namespaces allowed for this run by the person's approval of its request to run outside the sandbox (Moderate)"
            );
        }
        let initial_attempt_start = Instant::now();
        let (first_result, first_deferred_network_approval) = Self::run_attempt(
            tool,
            req,
            tool_ctx,
            &initial_attempt,
            managed_network_active,
        )
        .await;
        if let Some(check) = &git_check {
            check.finish(tool_ctx).await;
        }
        let initial_duration = initial_attempt_start.elapsed();
        let result = match first_result {
            Ok(out) => {
                // We have a successful initial result
                Ok(OrchestratorRunResult {
                    output: out,
                    deferred_network_approval: first_deferred_network_approval,
                })
            }
            Err(ToolError::Codex(err)) => {
                let CodexErrorDetails::Sandbox(SandboxErr::Denied {
                    output,
                    network_policy_decision,
                }) = err.details()
                else {
                    let err = ToolError::Codex(err);
                    if let Some(outcome) = sandbox_outcome_from_tool_error(&err) {
                        otel.sandbox_outcome(
                            &otel_tn,
                            otel_ci,
                            outcome,
                            initial_duration,
                            /*escalated_duration*/ None,
                        );
                    }
                    return Err(err);
                };
                let network_approval_context = if managed_network_active {
                    network_policy_decision
                        .as_ref()
                        .and_then(network_approval_context_from_payload)
                } else {
                    None
                };
                if network_policy_decision.is_some() && network_approval_context.is_none() {
                    otel.sandbox_outcome(
                        &otel_tn,
                        otel_ci,
                        "denied",
                        initial_duration,
                        /*escalated_duration*/ None,
                    );
                    return Err(ToolError::Codex(err));
                }
                if !tool.escalate_on_failure() {
                    otel.sandbox_outcome(
                        &otel_tn,
                        otel_ci,
                        "denied",
                        initial_duration,
                        /*escalated_duration*/ None,
                    );
                    return Err(ToolError::Codex(err));
                }
                let sandbox_startup_failure =
                    is_sandbox_startup_failure(initial_sandbox, output.as_ref());
                // #874: nor while the git check's findings are still there
                // (an approval doesn't lift the rules then); the person is
                // told why.
                let unsandboxed_allowed = retry_may_leave_sandbox(
                    policy_allows_no_sandbox,
                    keeps_sandbox,
                    post_taint.is_some(),
                    network_approval_context.is_some(),
                );
                // #943 item 3: never after the note above: the rules it kept
                // deny reads, which keep the run in the sandbox (an external
                // sandbox, which takes no rules, never reaches this retry),
                // and under Moderate (D48) it never leaves it.
                if unsandboxed_allowed && !git_held.is_empty() {
                    warn(tool_ctx, &git_held_note(&git_held, Withheld::Rules)).await;
                }
                let unsandboxed_allowed = unsandboxed_allowed && git_held.is_empty();
                // Under `Never` or `OnRequest`, do not retry without sandbox;
                // surface a concise sandbox denial that preserves the original
                // output. Exception: if the platform sandbox itself failed to
                // start after an explicit OnRequest approval, the approval is
                // enough to retry unsandboxed when doing so would not drop
                // denied-read enforcement, nor (#540) leave a kept sandbox
                // outside a protected action's fresh approval
                // (`retry_may_leave_sandbox`).
                if !tool.wants_no_sandbox_approval(approval_policy) {
                    let allow_on_request_network_prompt =
                        matches!(approval_policy, AskForApproval::OnRequest)
                            && network_approval_context.is_some()
                            && matches!(
                                default_exec_approval_requirement(
                                    approval_policy,
                                    &file_system_sandbox_policy
                                ),
                                ExecApprovalRequirement::NeedsApproval { .. }
                            );
                    let allow_on_request_startup_retry = allow_on_request_sandbox_startup_retry(
                        approval_policy,
                        already_approved,
                        sandbox_startup_failure,
                    );
                    if !allow_on_request_network_prompt && !allow_on_request_startup_retry {
                        otel.sandbox_outcome(
                            &otel_tn,
                            otel_ci,
                            "denied",
                            initial_duration,
                            /*escalated_duration*/ None,
                        );
                        return Err(ToolError::Codex(err));
                    }
                }
                if !unsandboxed_allowed && network_approval_context.is_none() {
                    // #739: where only protected launch keeps this run in the
                    // sandbox, say why no retry outside it is offered.
                    if permissive_launch
                        && unsandboxed_execution_allowed(&file_system_sandbox_policy)
                    {
                        warn(
                            tool_ctx,
                            crate::security::protected_additional::PROTECTED_LAUNCH_NO_RETRY,
                        )
                        .await;
                    }
                    otel.sandbox_outcome(
                        &otel_tn,
                        otel_ci,
                        "denied",
                        initial_duration,
                        /*escalated_duration*/ None,
                    );
                    return Err(ToolError::Codex(err));
                }
                let retry_reason =
                    if let Some(network_approval_context) = network_approval_context.as_ref() {
                        format!(
                            "Network access to \"{}\" is blocked by policy.",
                            network_approval_context.host
                        )
                    } else if sandbox_startup_failure {
                        "sandbox failed to start; retry without sandbox?".to_string()
                    } else {
                        build_denial_reason_from_output(output.as_ref())
                    };

                // Strict auto-review approval covers the sandboxed attempt only;
                // retrying without the sandbox requires a fresh guardian review.
                // A post-taint approval covered the sandboxed attempt only.
                let bypass_retry_approval = !strict_auto_review
                    && post_taint.is_none()
                    && (tool.should_bypass_approval(approval_policy, already_approved)
                        || allow_on_request_sandbox_startup_retry(
                            approval_policy,
                            already_approved,
                            sandbox_startup_failure,
                        ))
                    && network_approval_context.is_none();
                // D50: a retry due on an approval from before a security
                // transition (one committed while the first attempt ran) is
                // not asked about: it would not be run.
                if tool_ctx.session.services.transition_since(decided_under) {
                    warn(tool_ctx, STALE_RETRY_NOTE).await;
                    otel.sandbox_outcome(
                        &otel_tn,
                        otel_ci,
                        "denied",
                        initial_duration,
                        /*escalated_duration*/ None,
                    );
                    return Err(ToolError::Codex(err));
                }
                if !bypass_retry_approval {
                    let approval_ctx = ApprovalCtx {
                        session: &tool_ctx.session,
                        turn: &tool_ctx.turn,
                        call_id: &tool_ctx.call_id,
                        retry_reason: Some(match &post_taint {
                            Some(action) => format!("{} {retry_reason}", action.approval_reason()),
                            None => retry_reason,
                        }),
                        network_approval_context: network_approval_context.clone(),
                        fresh_human_authority: post_taint.is_some(),
                    };

                    let permission_request_run_id = format!("{}:retry", tool_ctx.call_id);
                    let retry_approved_by = resolve_tool_apporval(
                        tool,
                        req,
                        &permission_request_run_id,
                        approval_ctx,
                        tool_ctx,
                        if strict_auto_review {
                            ApprovalReviewer::Guardian
                        } else {
                            ApprovalReviewer::for_turn(turn_ctx)
                        },
                        &otel,
                        DeclineRecord::Here,
                        /*asked_again*/ false,
                    )
                    .await?;
                    if let Some(action) = &post_taint {
                        post_taint_recheck(action, tool_ctx)?;
                    }
                    // Issue #540: leaving the sandbox here takes a person.
                    if keeps_sandbox
                        && unsandboxed_allowed
                        && retry_approved_by != ApprovalResolutionSource::User
                    {
                        return Err(ToolError::Rejected(
                            "running outside the sandbox needs a person's approval here"
                                .to_string(),
                        ));
                    }
                }

                // D50: nor a retry approved (or due on an earlier approval)
                // before a security transition. The first attempt already
                // ran, so the run is not decided again: the sandbox denial
                // stands and the person is told why there was no retry.
                if tool_ctx.session.services.transition_since(decided_under) {
                    warn(tool_ctx, STALE_RETRY_NOTE).await;
                    otel.sandbox_outcome(
                        &otel_tn,
                        otel_ci,
                        "denied",
                        initial_duration,
                        /*escalated_duration*/ None,
                    );
                    return Err(ToolError::Codex(err));
                }

                let retry_sandbox_requested = !unsandboxed_allowed
                    && self.sandbox.should_sandbox(
                        &permissions,
                        sandbox_preference,
                        managed_network_active,
                    );
                let retry_sandbox = if retry_sandbox_requested {
                    self.sandbox.select_initial(
                        &permissions,
                        sandbox_preference,
                        turn_ctx.windows_sandbox_level,
                        managed_network_active,
                    )
                } else {
                    SandboxType::None
                };
                let retry_codex_linux_sandbox_exe = if unsandboxed_allowed {
                    None
                } else {
                    turn_ctx.config.codex_linux_sandbox_exe.as_ref()
                };
                let retry_attempt = SandboxAttempt {
                    sandbox: retry_sandbox,
                    sandbox_requested: retry_sandbox_requested,
                    permissions: &permissions,
                    exec_server_permissions: permission_profile,
                    enforce_managed_network: managed_network_active,
                    manager: &self.sandbox,
                    sandbox_cwd: &sandbox_policy_cwd,
                    workspace_roots,
                    codex_linux_sandbox_exe: retry_codex_linux_sandbox_exe,
                    use_legacy_landlock,
                    windows_sandbox_level: turn_ctx.windows_sandbox_level,
                    windows_sandbox_private_desktop: turn_ctx
                        .config
                        .permissions
                        .windows_sandbox_private_desktop,
                    network_denial_cancellation_token: None,
                    network_proxy: None,
                };

                // Second attempt.
                let escalated_attempt_start = Instant::now();
                let (retry_result, retry_deferred_network_approval) =
                    Self::run_attempt(tool, req, tool_ctx, &retry_attempt, managed_network_active)
                        .await;
                if let Some(check) = &git_check {
                    check.finish(tool_ctx).await;
                }
                let escalated_duration = escalated_attempt_start.elapsed();
                match retry_result {
                    Ok(output) => {
                        otel.sandbox_outcome(
                            &otel_tn,
                            otel_ci,
                            "escalated",
                            initial_duration,
                            Some(escalated_duration),
                        );
                        Ok(OrchestratorRunResult {
                            output,
                            deferred_network_approval: retry_deferred_network_approval,
                        })
                    }
                    Err(err) => {
                        if let Some(outcome) = sandbox_outcome_from_tool_error(&err) {
                            otel.sandbox_outcome(
                                &otel_tn,
                                otel_ci,
                                outcome,
                                initial_duration,
                                Some(escalated_duration),
                            );
                        }
                        Err(err)
                    }
                }
            }
            Err(err) => {
                if let Some(outcome) = sandbox_outcome_from_tool_error(&err) {
                    otel.sandbox_outcome(
                        &otel_tn,
                        otel_ci,
                        outcome,
                        initial_duration,
                        /*escalated_duration*/ None,
                    );
                }
                Err(err)
            }
        };
        result.map(Round::Done)
    }
}

/// #874, #837 (Travis, 2026-10-16, D6): the repositories whose git folder a
/// run may write under Moderate or Aggressive (`security::git_postcheck`).
struct GitCheck {
    repos: Vec<crate::security::git_postcheck::Repo>,
    level: codex_security_policy::SecurityLevel,
}

impl GitCheck {
    /// The repositories of `folders` and of `command_cwd` (the folder the
    /// command runs in, #943 item 11) whose git folder the run may write
    /// (through `policy`, or anywhere where it may run unsandboxed), with
    /// this session's first look at each taken.
    async fn begin(
        tool_ctx: &ToolCtx,
        level: codex_security_policy::SecurityLevel,
        folders: Vec<std::path::PathBuf>,
        command_cwd: Option<std::path::PathBuf>,
        policy: codex_protocol::permissions::FileSystemSandboxPolicy,
        unsandboxed: bool,
    ) -> Option<Self> {
        let repos = Self::first_look(tool_ctx, level, folders, command_cwd, policy, unsandboxed)
            .await
            .ok()?;
        (!repos.is_empty()).then_some(Self { repos, level })
    }

    /// Those repositories, with this session's first look at each taken
    /// where it has none yet; `Err` (the person told) where the look failed.
    async fn first_look(
        tool_ctx: &ToolCtx,
        level: codex_security_policy::SecurityLevel,
        folders: Vec<std::path::PathBuf>,
        command_cwd: Option<std::path::PathBuf>,
        policy: codex_protocol::permissions::FileSystemSandboxPolicy,
        unsandboxed: bool,
    ) -> Result<Vec<crate::security::git_postcheck::Repo>, ()> {
        use crate::security::git_postcheck;
        let thread = tool_ctx.session.thread_id();
        let Some(cwd) = folders.first().cloned() else {
            return Ok(Vec::new());
        };
        let level_name = format!("{level:?}");
        // Issue #524: reading folders stays off the async thread.
        let looked = tokio::task::spawn_blocking(move || {
            let folders: Vec<&std::path::Path> = folders.iter().map(AsRef::as_ref).collect();
            let repos: Vec<_> = git_postcheck::repos_and(&folders, command_cwd.as_deref())
                .into_iter()
                .filter(|repo| {
                    unsandboxed
                        || repo
                            .probes()
                            .iter()
                            .any(|probe| policy.can_write_effective_path_with_cwd(probe, &cwd))
                })
                .collect();
            let message = git_postcheck::begin(thread, &repos, &level_name);
            (repos, message)
        })
        .await;
        // A failed look (its task panicked) leaves nothing to compare with:
        // the person is told, and the next command looks again.
        let Ok((repos, message)) = looked else {
            warn(tool_ctx, GIT_CHECK_FAILED).await;
            return Err(());
        };
        // What a session's first look finds there already (planted before,
        // or by a process left running) is told too.
        if let Some(message) = message {
            warn(tool_ctx, &message).await;
        }
        Ok(repos)
    }

    /// After an attempt: tell the person what is new there.
    async fn finish(&self, tool_ctx: &ToolCtx) {
        let thread = tool_ctx.session.thread_id();
        let level = format!("{:?}", self.level);
        let repos = self.repos.clone();
        let message = tokio::task::spawn_blocking(move || {
            crate::security::git_postcheck::finish(thread, &repos, &level)
        })
        .await
        .unwrap_or_else(|_| Some(GIT_CHECK_FAILED.to_string()));
        if let Some(message) = message {
            tracing::warn!(
                target: "codex_core::security::git_postcheck",
                call_id = %tool_ctx.call_id,
                "{message}"
            );
            warn(tool_ctx, &message).await;
        }
    }
}

use crate::security::git_postcheck::CHECK_FAILED as GIT_CHECK_FAILED;

/// #874 (D6): under Moderate or Aggressive, the git folders this session
/// watches looked at again; what holds the rules for a command in `folders`
/// (fail closed when the look fails). The person is told what is new.
async fn git_refresh(
    tool_ctx: &ToolCtx,
    folders: Vec<codex_utils_absolute_path::AbsolutePathBuf>,
) -> Vec<String> {
    let Some(level) = crate::security::protected_additional::protected_level(&tool_ctx.session)
        .filter(|level| *level != codex_security_policy::SecurityLevel::Permissive)
    else {
        return Vec::new();
    };
    let thread = tool_ctx.session.thread_id();
    let level = format!("{level:?}");
    let (message, held) = tokio::task::spawn_blocking(move || {
        let folders: Vec<&std::path::Path> = folders.iter().map(AsRef::as_ref).collect();
        crate::security::git_postcheck::recheck(thread, &level, &folders)
    })
    .await
    // A failed look holds this command (not other sessions: a task ended
    // by a shutdown fails too).
    .unwrap_or_else(|_| {
        (
            Some(GIT_CHECK_FAILED.to_string()),
            vec!["(the check failed)".to_string()],
        )
    });
    if let Some(message) = message {
        warn(tool_ctx, &message).await;
    }
    held
}

/// What an approval or grant didn't do while the git check's findings stand
/// (#874).
#[derive(Clone, Copy)]
enum Withheld {
    /// Lift the protected-file rules.
    Rules,
    /// Let the command create user namespaces only (a D48 approval, which
    /// lifts no rule).
    Namespaces,
    /// Let the command's additional permissions write to a read-only path
    /// (#513, Moderate).
    Writes,
}

/// Why an approval or grant didn't do `withheld` (#874).
fn git_held_note(held: &[String], withheld: Withheld) -> String {
    const SHOWN: usize = 20;
    let mut shown = held
        .iter()
        .take(SHOWN)
        .cloned()
        .collect::<Vec<_>>()
        .join(", ");
    if held.len() > SHOWN {
        shown.push_str(&format!(" and {} more", held.len() - SHOWN));
    }
    let withheld = match withheld {
        Withheld::Rules => {
            "doesn't lift the protected-file rules for this command (nor run it outside the \
             sandbox, nor let it create user namespaces)"
        }
        Withheld::Namespaces => "doesn't let this command create user namespaces",
        Withheld::Writes => {
            "doesn't let this command's additional permissions write to read-only paths"
        }
    };
    format!(
        "The approval {withheld} while the project's git folder still holds what Corbanu \
         reported: {shown}."
    )
}

/// One round of [`ToolOrchestrator::run`].
enum Round<Out> {
    Done(OrchestratorRunResult<Out>),
    /// Approved (or decided) before a security transition (D50).
    /// `person_asked`: a person answered the stale prompt.
    Stale {
        person_asked: bool,
    },
}

/// D50 (#1001): `Err` when a run whose approval (or, #1057, whose handler's
/// grant read) went stale must not be decided again: the transition switched
/// Aggressive on, so its exec-policy decision is stale, or (D51) it carries
/// permissions read from grants the transition ended. A new call reads them
/// again. `before_decided`: the transition came before anyone was asked.
async fn stale_refusal<Rq, Out, T>(
    tool: &T,
    req: &Rq,
    tool_ctx: &ToolCtx,
    aggressive_at_start: bool,
    before_decided: bool,
) -> Result<(), ToolError>
where
    T: ToolRuntime<Rq, Out>,
{
    let refusal = if tool_ctx
        .session
        .services
        .model_client()
        .live_level_is_aggressive()
        != aggressive_at_start
    {
        if before_decided {
            PREPARED_BEFORE_AGGRESSIVE_REFUSAL
        } else {
            STALE_AGGRESSIVE_REFUSAL
        }
    } else if tool.additional_permissions(req).is_some() {
        if before_decided {
            PREPARED_PERMISSIONS_REFUSAL
        } else {
            STALE_PERMISSIONS_REFUSAL
        }
    } else {
        return Ok(());
    };
    warn(tool_ctx, refusal).await;
    Err(ToolError::Rejected(refusal.to_string()))
}

/// D50 (#1001): how often a run whose approval went stale is decided again.
const MAX_STALE_APPROVAL_ROUNDS: usize = 3;

/// D50: why a run approved before a transition that switched Aggressive on
/// did not run.
pub(crate) const STALE_AGGRESSIVE_REFUSAL: &str = "Not run: the security policy changed while \
     this was being approved (security level Aggressive was switched on), so the approval was \
     not used. Ask again if it is still needed; it will be decided under Aggressive.";

/// D50, D51: why a run that carried additional permissions (granted ones
/// included) was not decided again after a transition.
pub(crate) const STALE_PERMISSIONS_REFUSAL: &str = "Not run: the security policy changed while \
     this was being approved, and the permissions it asked for or was granted were for the \
     earlier policy, so the approval was not used. Ask again if it is still needed.";

/// #1057: why a run prepared before Aggressive was switched on did not run.
pub(crate) const PREPARED_BEFORE_AGGRESSIVE_REFUSAL: &str = "Not run: security level \
     Aggressive was switched on while this was being prepared, before it was decided. Ask \
     again if it is still needed; it will be decided under Aggressive.";

/// #1057: why a run carrying permissions read before a transition did not
/// run.
pub(crate) const PREPARED_PERMISSIONS_REFUSAL: &str = "Not run: the security policy changed \
     while this was being prepared, and the permissions it asked for or was granted were for \
     the earlier policy. Ask again if it is still needed.";

/// D50: why a retry outside the sandbox did not happen.
const STALE_RETRY_NOTE: &str = "The security level changed while this command ran or its \
     retry outside the sandbox was being approved, so it was not retried. Run it again to have \
     it decided under the new level.";

/// D50: why a run whose approval went stale every time did not run.
pub(crate) const STALE_APPROVAL_REFUSAL: &str = "The security level changed while this \
     approval was open, each time it was asked, so it was not used and nothing ran. Ask \
     again if it is still needed.";

/// Issue #540: whether the retry after a sandbox denial may run without a
/// sandbox. A run that keeps its sandbox (`keeps_sandbox`: the protected-path
/// rules apply and it did not ask to run outside) leaves it only for a
/// protected action's retry, which a person approves afresh, and never for
/// a network approval: that person allowed a host, not an unsandboxed run.
fn retry_may_leave_sandbox(
    policy_allows: bool,
    keeps_sandbox: bool,
    protected_action: bool,
    network_retry: bool,
) -> bool {
    policy_allows && (!keeps_sandbox || (protected_action && !network_retry))
}

fn sandbox_outcome_from_tool_error(err: &ToolError) -> Option<&'static str> {
    match err {
        ToolError::Codex(err) => match err.details() {
            CodexErrorDetails::Sandbox(SandboxErr::Denied { .. }) => Some("denied"),
            CodexErrorDetails::Sandbox(SandboxErr::Timeout { .. }) => Some("timed_out"),
            CodexErrorDetails::Sandbox(SandboxErr::Signal(_)) => Some("signal"),
            _ => None,
        },
        ToolError::Rejected(_) | ToolError::Declined(_) => None,
    }
}

/// PF-30-S03: how long classifying one post-taint action may take.
const POST_TAINT_CLASSIFY_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(3);

/// PF-30-S03: the protected action this request performs, when post-taint
/// checks apply and the session already holds content without standing.
async fn post_taint_action<Rq, Out, T>(
    tool: &T,
    req: &Rq,
    tool_ctx: &ToolCtx,
    requirement: &ExecApprovalRequirement,
) -> Option<crate::security::tainted_action::PostTaintAction>
where
    T: ToolRuntime<Rq, Out>,
{
    if matches!(requirement, ExecApprovalRequirement::Forbidden { .. }) {
        return None;
    }
    let state = tool_ctx
        .session
        .services
        .model_client()
        .post_taint_state()
        .filter(|state| state.taint_generation > 0)?;
    let ctx = ApprovalCtx {
        session: &tool_ctx.session,
        turn: &tool_ctx.turn,
        call_id: &tool_ctx.call_id,
        retry_reason: None,
        network_approval_context: None,
        fresh_human_authority: true,
    };
    // An action the host cannot describe cannot be shown to be ordinary.
    let started = Instant::now();
    let kind = match tool.approval_action(req, &ctx) {
        Ok(action) => {
            // Classification may look at the filesystem: keep it off the
            // async workers and bounded in time; a stall fails closed.
            let codex_home = tool_ctx.turn.config.codex_home.to_path_buf();
            // D85: the zsh exec bridge runs any shell tool `-c` script under
            // zsh, whatever shell the argv names.
            let features = tool_ctx.session.features();
            let as_named = !features.enabled(codex_features::Feature::ShellZshFork)
                && !features.enabled(codex_features::Feature::UnifiedExecZshFork);
            let classified = tokio::time::timeout(
                POST_TAINT_CLASSIFY_TIMEOUT,
                tokio::task::spawn_blocking(move || {
                    if as_named {
                        crate::security::tainted_action::classify_as_named(&action, &codex_home)
                    } else {
                        crate::security::tainted_action::classify(&action, &codex_home)
                    }
                }),
            )
            .await;
            match classified {
                Ok(Ok(kind)) => kind?,
                Ok(Err(_)) | Err(_) => {
                    crate::security::tainted_action::ProtectedActionKind::UnseenCode
                }
            }
        }
        Err(_) => crate::security::tainted_action::ProtectedActionKind::UnseenCode,
    };
    tracing::info!(
        target: "codex_core::security::tainted_action",
        kind = ?kind,
        taint_generation = state.taint_generation,
        classify_us = started.elapsed().as_micros() as u64,
        call_id = %tool_ctx.call_id,
        "post-taint protected action needs fresh human approval"
    );
    Some(crate::security::tainted_action::PostTaintAction { kind, state })
}

/// #734: after a run in the non-admin Windows sandbox, tells the person
/// (once per thread) about each writable root of the run that it couldn't
/// label Low during the run (since `mark`), so left read-only to its
/// commands. Display only.
async fn warn_unlabelled_writable_roots<Rq, Out, T>(
    tool: &T,
    req: &Rq,
    tool_ctx: &ToolCtx,
    attempt: &SandboxAttempt<'_>,
    mark: u64,
) where
    T: ToolRuntime<Rq, Out>,
{
    // The elevated backend labels nothing; a run it took (by level, or for
    // the network proxy) records no failure after `mark` but another
    // thread's concurrent run might, so it is left out where known.
    if attempt.sandbox != SandboxType::WindowsRestrictedToken
        || attempt.windows_sandbox_level != WindowsSandboxLevel::RestrictedToken
    {
        return;
    }
    let failed = codex_windows_sandbox::unlabelled_roots_since(mark);
    if failed.is_empty() {
        return;
    }
    let Ok(cwd) = attempt.sandbox_cwd.to_abs_path() else {
        return;
    };
    let Ok(workspace_roots) = attempt
        .workspace_roots
        .iter()
        .map(PathUri::to_abs_path)
        .collect::<std::io::Result<Vec<_>>>()
    else {
        return;
    };
    let effective = codex_sandboxing::policy_transforms::effective_file_system_sandbox_policy(
        &attempt.permissions.file_system_sandbox_policy(),
        tool.additional_permissions(req),
    );
    let writable =
        codex_windows_sandbox::legacy_writable_roots(&effective, &workspace_roots, cwd.as_path());
    for message in crate::tools::writable_root_notices::unlabelled_writable_root_warnings(
        tool_ctx.session.thread_id(),
        &failed,
        &writable,
    ) {
        warn(tool_ctx, &message).await;
    }
}

/// #739: tells the person `message` (a warning in the transcript).
async fn warn(tool_ctx: &ToolCtx, message: &str) {
    tool_ctx
        .session
        .send_event(
            &tool_ctx.turn,
            codex_protocol::protocol::EventMsg::Warning(codex_protocol::protocol::WarningEvent {
                message: message.to_string(),
            }),
        )
        .await;
}

/// #747 (D3): under Moderate, what a person is told before approving a
/// request to run outside the sandbox: it stays in it. `None` elsewhere. (An
/// approved patch outside the writable folders is applied inside the sandbox
/// with that one extra write, under every level.)
fn moderate_sandbox_note<Rq, Out, T>(
    tool: &T,
    req: &Rq,
    moderate_keeps_sandbox: bool,
) -> Option<&'static str>
where
    T: ToolRuntime<Rq, Out>,
{
    (moderate_keeps_sandbox
        && tool
            .sandbox_permissions(req)
            .requires_escalated_permissions())
    .then_some(crate::security::protected_additional::MODERATE_STAYS_SANDBOXED)
}

/// Issue #239: under Moderate before untrusted content, whether this command
/// asks to run outside the sandbox (a human approval then lifts the
/// protected-path rules for it, as after untrusted content).
/// #840 (D6): not where a saved "don't ask again" rule allows it: the rule
/// skips the prompt and the command runs inside the sandbox with the rules
/// kept, as any command that does not ask to leave it.
fn moderate_escalation_before_taint<Rq, Out, T>(
    tool: &T,
    req: &Rq,
    tool_ctx: &ToolCtx,
    requirement: &ExecApprovalRequirement,
) -> bool
where
    T: ToolRuntime<Rq, Out>,
{
    !matches!(
        requirement,
        ExecApprovalRequirement::Skip {
            bypass_sandbox: true,
            ..
        }
    ) && tool
        .sandbox_permissions(req)
        .requires_escalated_permissions()
        && tool_ctx
            .session
            .services
            .model_client()
            .post_taint_state()
            .is_some_and(|state| state.taint_generation == 0 && state.moderate_bound())
}

/// Why a command that asks to run outside the sandbox runs, for its prompt:
/// the requirement's reason, else the model's justification.
fn escalation_why<Rq, Out, T>(
    tool: &T,
    req: &Rq,
    requirement: &ExecApprovalRequirement,
    approval_ctx: &ApprovalCtx<'_>,
) -> Option<String>
where
    T: ToolRuntime<Rq, Out>,
{
    match requirement {
        ExecApprovalRequirement::NeedsApproval {
            reason: Some(reason),
            ..
        } => Some(reason.clone()),
        _ => match tool.approval_action(req, approval_ctx) {
            Ok(
                ApprovalAction::Shell { justification, .. }
                | ApprovalAction::ExecCommand { justification, .. },
            ) => justification,
            _ => None,
        },
    }
}

/// D48 (c): whether the command a shell request runs (as its prompt shows
/// it) is a known tool's, its programs looked up through the `PATH` it runs
/// with and checked against what this run's sandbox (with its additional
/// permissions) can write.
fn runs_known_tool<Rq, Out, T>(
    tool: &T,
    req: &Rq,
    tool_ctx: &ToolCtx,
    turn_ctx: &crate::session::turn_context::TurnContext,
    file_system_sandbox_policy: &codex_protocol::permissions::FileSystemSandboxPolicy,
) -> bool
where
    T: ToolRuntime<Rq, Out>,
{
    let ctx = ApprovalCtx {
        session: &tool_ctx.session,
        turn: &tool_ctx.turn,
        call_id: &tool_ctx.call_id,
        retry_reason: None,
        network_approval_context: None,
        fresh_human_authority: true,
    };
    let command = match tool.approval_action(req, &ctx) {
        Ok(ApprovalAction::Shell { command, .. } | ApprovalAction::ExecCommand { command, .. }) => {
            command
        }
        _ => return false,
    };
    #[allow(deprecated)]
    let cwd = tool
        .sandbox_cwd(req)
        .and_then(|cwd| cwd.to_abs_path().ok())
        .unwrap_or_else(|| turn_ctx.cwd.clone());
    let policy = codex_sandboxing::policy_transforms::effective_file_system_sandbox_policy(
        file_system_sandbox_policy,
        tool.additional_permissions(req),
    );
    // Also where it is reached through a link (`~/bin -> ~/project/bin`).
    let writable = |path: &std::path::Path| {
        policy.can_write_path_with_cwd(path, cwd.as_path())
            || std::fs::canonicalize(path)
                .is_ok_and(|real| policy.can_write_path_with_cwd(&real, cwd.as_path()))
    };
    // As the shell's search decides it (`access(X_OK)`), not by mode bits.
    let executable = |file: &std::path::Path| {
        #[cfg(unix)]
        let runs = {
            use std::os::unix::ffi::OsStrExt;
            std::ffi::CString::new(file.as_os_str().as_bytes()).is_ok_and(|file| {
                // SAFETY: `file` is a valid NUL-terminated string that
                // outlives the call; `access` only reads it.
                unsafe { libc::access(file.as_ptr(), libc::X_OK) == 0 }
            })
        };
        #[cfg(not(unix))]
        let runs = true;
        runs && std::fs::metadata(file).is_ok_and(|meta| meta.is_file())
    };
    crate::security::user_namespaces::known_tool(
        &command,
        &crate::security::user_namespaces::Lookup {
            path: tool.local_search_path(req),
            writable: &writable,
            executable: &executable,
        },
    )
    .is_some()
}

/// The command and folder a grant offer shows (PF-25-S01).
type GrantShown = Option<(Vec<String>, String)>;

/// PF-23-S02: under Aggressive, the grant operation naming this exact
/// command or patch in its folder, and for a command what a grant offer
/// shows (PF-25-S01). `None` under any other level.
fn aggressive_grant_operation<Rq, Out, T>(
    tool: &T,
    req: &Rq,
    tool_ctx: &ToolCtx,
) -> Option<(String, GrantShown)>
where
    T: ToolRuntime<Rq, Out>,
{
    use crate::security::aggressive::command_operation;
    tool_ctx
        .session
        .services
        .model_client()
        .post_taint_state()
        .filter(|state| state.level == codex_security_policy::SecurityLevel::Aggressive)?;
    let ctx = ApprovalCtx {
        session: &tool_ctx.session,
        turn: &tool_ctx.turn,
        call_id: &tool_ctx.call_id,
        retry_reason: None,
        network_approval_context: None,
        fresh_human_authority: true,
    };
    Some(match tool.approval_action(req, &ctx).ok()? {
        ApprovalAction::Shell {
            command,
            cwd,
            sandbox_permissions,
            additional_permissions,
            ..
        }
        | ApprovalAction::ExecCommand {
            command,
            cwd,
            sandbox_permissions,
            additional_permissions,
            ..
        } => {
            let shown_cwd = cwd.to_path_buf().display().to_string();
            let cwd = cwd.to_string();
            let permissions = format!("{sandbox_permissions:?} {additional_permissions:?}");
            let mut parts = vec!["command", cwd.as_str(), permissions.as_str()];
            parts.extend(command.iter().map(String::as_str));
            let operation = command_operation(&parts);
            (operation, Some((command, shown_cwd)))
        }
        ApprovalAction::ApplyPatch { cwd, patch, .. } => (
            command_operation(&["patch", &cwd.to_string(), &patch]),
            None,
        ),
    })
}

/// #442: under Aggressive, a command that names a protected path (credential
/// files, the Corbanu home) is never granted: no grant is offered for it, and
/// its approval prompt carries this explanation. Looked up only where a
/// person is about to be asked.
async fn protected_read_note(
    operation: Option<&(String, GrantShown)>,
    tool_ctx: &ToolCtx,
    turn_ctx: &crate::session::turn_context::TurnContext,
) -> Option<String> {
    let (_, Some((command, cwd))) = operation? else {
        return None;
    };
    crate::security::protected_reads::protected_note_in_command(
        &tool_ctx.session,
        turn_ctx,
        command,
        std::path::Path::new(cwd),
    )
    .await
    .map(|found| found.explanation())
}

/// #826 follow-up: for a person asked to approve a command that runs a
/// program needing a user namespace (`unshare`, `bwrap`, rootless Podman,
/// …) where agent commands cannot create one, the note that it fails and
/// that no approval lifts the rule, with the model's justification for the
/// command (a prompt's reason replaces the justification).
fn user_namespaces_note<Rq, Out, T>(
    tool: &T,
    req: &Rq,
    tool_ctx: &ToolCtx,
    turn_ctx: &crate::session::turn_context::TurnContext,
) -> Option<(String, Option<String>)>
where
    T: ToolRuntime<Rq, Out>,
{
    if !cfg!(target_os = "linux") {
        return None;
    }
    let ctx = ApprovalCtx {
        session: &tool_ctx.session,
        turn: &tool_ctx.turn,
        call_id: &tool_ctx.call_id,
        retry_reason: None,
        network_approval_context: None,
        fresh_human_authority: false,
    };
    let (command, justification) = match tool.approval_action(req, &ctx).ok()? {
        ApprovalAction::Shell {
            command,
            justification,
            ..
        }
        | ApprovalAction::ExecCommand {
            command,
            justification,
            ..
        } => (command, justification),
        _ => return None,
    };
    crate::security::user_namespaces::approval_note(&tool_ctx.session, turn_ctx, &command)
        .map(|note| (note, justification))
}

/// #841, #1077: under Moderate, for a protected action after untrusted
/// content that reads a file the secretless launch contract keeps unreadable
/// at every level (a CLI token, the keychains, or Corbanu's sign-in, vault,
/// config or history): approving cannot unlock it, and the person is told
/// which file and what it is before answering.
async fn moderate_kept_read_note<Rq, Out, T>(
    tool: &T,
    req: &Rq,
    tool_ctx: &ToolCtx,
    turn_ctx: &crate::session::turn_context::TurnContext,
    action: &crate::security::tainted_action::PostTaintAction,
) -> Option<String>
where
    T: ToolRuntime<Rq, Out>,
{
    use crate::security::tainted_action::ProtectedActionKind;
    if !action.state.moderate_bound()
        || !matches!(
            action.kind,
            ProtectedActionKind::SecurityPolicy
                | ProtectedActionKind::Vault
                | ProtectedActionKind::Credentials
        )
    {
        return None;
    }
    let ctx = ApprovalCtx {
        session: &tool_ctx.session,
        turn: &tool_ctx.turn,
        call_id: &tool_ctx.call_id,
        retry_reason: None,
        network_approval_context: None,
        fresh_human_authority: true,
    };
    let (command, cwd) = match tool.approval_action(req, &ctx).ok()? {
        ApprovalAction::Shell { command, cwd, .. }
        | ApprovalAction::ExecCommand { command, cwd, .. } => (command, cwd),
        _ => return None,
    };
    let cwd = cwd.to_abs_path().ok()?;
    crate::security::protected_reads::moderate_kept_read_note_for_command(
        &tool_ctx.session,
        turn_ctx,
        &command,
        cwd.as_path(),
    )
    .await
}

/// PF-25-S01: offer the human a grant for this exact command while its
/// approval is open (only under a live Aggressive policy).
fn aggressive_grant_offer(
    operation: Option<&(String, GrantShown)>,
    tool_ctx: &ToolCtx,
) -> Option<crate::security::grant_offer::OfferGuard> {
    let (operation, Some((command, cwd))) = operation? else {
        return None;
    };
    let state = tool_ctx
        .session
        .services
        .model_client()
        .post_taint_state()?;
    Some(crate::security::grant_offer::offer_for_approval(
        &tool_ctx.session,
        &state,
        &tool_ctx.call_id,
        operation.clone(),
        command.clone(),
        cwd.clone(),
    ))
}

/// PF-23-S01 slice 3 / PF-23-S02 / issue #239: the exec-server and
/// materialized profiles with credential and Corbanu home reads denied and
/// persistence files made read-only under Moderate or Aggressive, from the
/// start of the session (`PostTaintState::protected_paths_apply`).
///
/// Under Moderate a fresh human approval lifts the rules for this run: after
/// untrusted content, of this exact protected command; before it, of this
/// command's request to run outside the sandbox. The Corbanu home's runtime
/// entries stay read-only even then (#540). `human_approved_at` is the
/// taint generation that approval was given under; new taint since voids it.
/// Under Aggressive an approval never does; a matching human grant for this
/// exact command (`security::aggressive`) lifts the write protections only:
/// reads of credential files and the Corbanu home stay denied (#442).
/// Lifting only leaves these rules out: every denial of the profile itself
/// stays. An external sandbox cannot take the
/// rules and keeps its own.
#[allow(clippy::too_many_arguments)]
fn post_taint_read_denials(
    tool_ctx: &ToolCtx,
    git_held: bool,
    git_held_back: &mut bool,
    human_approved_at: Option<u64>,
    grant_operation: Option<&str>,
    confirmed_grant: Option<crate::security::grant_offer::Confirmed>,
    cwd: codex_utils_absolute_path::AbsolutePathBuf,
    workspace_roots: &[codex_utils_absolute_path::AbsolutePathBuf],
    exec_server: &codex_protocol::models::PermissionProfile,
    materialized: &codex_protocol::models::PermissionProfile,
) -> Option<(
    codex_protocol::models::PermissionProfile,
    codex_protocol::models::PermissionProfile,
    crate::security::protected_surface::ReadDenials,
    codex_security_policy::SecurityLevel,
)> {
    use crate::security::aggressive;
    use crate::security::protected_surface::ReadDenials;
    let thread = tool_ctx.session.thread_id();
    let state = tool_ctx
        .session
        .services
        .model_client()
        .post_taint_state()
        .filter(crate::security::tainted_action::PostTaintState::protected_paths_apply)?;
    // #874 (D6): while something the post-command check found in a git
    // folder is still there (`git_refresh`), no approval or grant lifts the
    // rules: an approved `git commit` could write through a planted link
    // into a protected file.
    *git_held_back = git_held
        && (state.human_approval_lifts_rules(human_approved_at) || confirmed_grant.is_some());
    if *git_held_back {
        tracing::info!(
            target: "codex_core::security::tainted_action",
            call_id = %tool_ctx.call_id,
            "protected-path rules kept: the git check's findings are still there"
        );
    }
    if !git_held && state.human_approval_lifts_rules(human_approved_at) {
        // Issue #540: except the Corbanu home's runtime entries, which stay
        // read-only in the command's sandbox (under Moderate every command
        // keeps one, #747).
        let mut denials = ReadDenials::for_turn(
            tool_ctx.turn.config.codex_home.as_path(),
            tool_ctx.turn.config.sqlite.home(),
            &cwd,
            workspace_roots,
            materialized,
        )
        .runtime_only();
        // #766: the approval lifts no rule a nested `corbanu exec` relies on.
        if !tool_ctx
            .turn
            .config
            .permissions
            .launch_enforces_aggressive()
        {
            denials.mark_moderate(tool_ctx.turn.config.codex_home.as_path());
        } else {
            ReadDenials::keep_moderate_marker_readable(tool_ctx.turn.config.codex_home.as_path());
        }
        tracing::info!(
            target: "codex_core::security::tainted_action",
            taint_generation = state.taint_generation,
            read_only = denials.read_only_paths().count(),
            call_id = %tool_ctx.call_id,
            "protected-path rules lifted by the human approval of this command, but for the runtime entries"
        );
        let (exec_server, materialized) =
            (denials.apply(exec_server)?, denials.apply(materialized)?);
        return Some((exec_server, materialized, denials, state.level));
    }
    // #442: a grant lifts the write protections only. Reads of credential
    // files and the Corbanu home are never granted under Aggressive.
    let mut granted = false;
    if let (false, Some(operation), Some(confirmed)) = (git_held, grant_operation, confirmed_grant)
    {
        match crate::security::grant_offer::apply(
            thread,
            &state,
            confirmed,
            operation,
            aggressive::now_unix_seconds(),
        ) {
            Ok(grant_id) => {
                tracing::info!(
                    target: "codex_core::security::tainted_action",
                    grant_id = grant_id.as_str(),
                    call_id = %tool_ctx.call_id,
                    "write protections lifted by the grant the human confirmed for this command; protected reads stay denied"
                );
                granted = true;
            }
            Err(reason) => tracing::warn!(
                target: "codex_core::security::tainted_action",
                call_id = %tool_ctx.call_id,
                reason,
                "the confirmed grant does not apply; the rules stay"
            ),
        }
    }
    if !granted
        && !git_held
        && let Some(operation) = grant_operation
        && let Some(grant_id) = aggressive::admit(
            thread,
            &state,
            aggressive::Surface::UnprotectedCommand,
            operation,
            aggressive::now_unix_seconds(),
        )
    {
        tracing::info!(
            target: "codex_core::security::tainted_action",
            grant_id = grant_id.as_str(),
            call_id = %tool_ctx.call_id,
            "write protections lifted by an Aggressive grant for this command; protected reads stay denied"
        );
        granted = true;
    }
    let mut denials = ReadDenials::for_turn(
        tool_ctx.turn.config.codex_home.as_path(),
        tool_ctx.turn.config.sqlite.home(),
        &cwd,
        workspace_roots,
        materialized,
    );
    if granted {
        denials.lift_write_rules();
    }
    // #506, #758: no grant lifts these either.
    if state.level == codex_security_policy::SecurityLevel::Aggressive {
        denials.deny_home_listing();
        denials.deny_keychain_services();
    }
    // #766: `corbanu exec` started by this command runs with Moderate; not
    // where the launch enforces Aggressive, whose own nested-launch rules
    // apply (its level file may be unreadable under Core's Moderate rules).
    if state.level == codex_security_policy::SecurityLevel::Moderate
        && !tool_ctx
            .turn
            .config
            .permissions
            .launch_enforces_aggressive()
    {
        denials.mark_moderate(tool_ctx.turn.config.codex_home.as_path());
    } else {
        ReadDenials::keep_moderate_marker_readable(tool_ctx.turn.config.codex_home.as_path());
    }
    let (Some(exec_server), Some(materialized)) =
        (denials.apply(exec_server), denials.apply(materialized))
    else {
        tracing::warn!(
            target: "codex_core::security::tainted_action",
            call_id = %tool_ctx.call_id,
            "protected-path rules cannot be added to an external sandbox"
        );
        return None;
    };
    tracing::info!(
        target: "codex_core::security::tainted_action",
        taint_generation = state.taint_generation,
        denied = denials.paths().count(),
        read_only = denials.read_only_paths().count(),
        skipped = denials.skipped.len(),
        call_id = %tool_ctx.call_id,
        "protected-path rules applied"
    );
    Some((exec_server, materialized, denials, state.level))
}

/// #826: `profile` with user namespaces denied to its commands. A profile
/// without a restricted filesystem policy (no sandbox, or an external one)
/// cannot carry the rule and is returned as it is.
fn deny_user_namespaces(
    profile: &codex_protocol::models::PermissionProfile,
) -> codex_protocol::models::PermissionProfile {
    use codex_protocol::permissions::FileSystemSandboxEntry;
    use codex_protocol::permissions::FileSystemSandboxKind;
    let (mut file_system, network) = profile.to_runtime_permissions();
    if file_system.kind != FileSystemSandboxKind::Restricted || file_system.denies_user_namespaces()
    {
        return profile.clone();
    }
    file_system
        .entries
        .push(FileSystemSandboxEntry::deny_user_namespaces());
    codex_protocol::models::PermissionProfile::from_runtime_permissions_with_enforcement(
        profile.enforcement(),
        &file_system,
        network,
    )
}

/// #874: the folders whose repositories the git check watches for a run:
/// the sandbox's working folder first (the probes resolve against it, as the
/// sandbox does), the turn's folder and the workspace roots.
fn git_check_folders<Rq, Out, T>(
    tool: &T,
    req: &Rq,
    turn_ctx: &crate::session::turn_context::TurnContext,
    workspace_roots: &[codex_utils_absolute_path::AbsolutePathBuf],
) -> Vec<std::path::PathBuf>
where
    T: ToolRuntime<Rq, Out>,
{
    #[allow(deprecated)]
    let turn_cwd = turn_ctx.cwd.to_path_buf();
    let mut folders = Vec::new();
    match tool.sandbox_cwd(req).map(PathUri::to_abs_path) {
        Some(Ok(cwd)) => folders.push(cwd.to_path_buf()),
        Some(Err(_)) => {}
        None => folders.push(turn_cwd.clone()),
    }
    folders.push(turn_cwd);
    folders.extend(
        workspace_roots
            .iter()
            .map(codex_utils_absolute_path::AbsolutePathBuf::to_path_buf),
    );
    folders
}

/// Issue #245: every folder the sandbox may resolve a relative or
/// project-root additional permission against: the run's own folder, the
/// turn's folder and the workspace roots.
fn resolution_folders<Rq, Out, T>(
    tool: &T,
    req: &Rq,
    turn_cwd: &codex_utils_absolute_path::AbsolutePathBuf,
    workspace_roots: &[codex_utils_absolute_path::AbsolutePathBuf],
) -> Vec<codex_utils_absolute_path::AbsolutePathBuf>
where
    T: ToolRuntime<Rq, Out>,
{
    let mut folders: Vec<_> = tool
        .sandbox_cwd(req)
        .and_then(|cwd| cwd.to_abs_path().ok())
        .into_iter()
        .collect();
    folders.push(turn_cwd.clone());
    folders.extend(workspace_roots.iter().cloned());
    folders.dedup();
    folders
}

/// Issues #245 and #513: refuses a run whose additional permissions would
/// open a protected path or reopen a read-only one, and explains why to the
/// person and the model.
async fn refuse_opened(
    opened: Option<crate::security::protected_additional::Opened>,
    level: codex_security_policy::SecurityLevel,
    tool_ctx: &ToolCtx,
) -> Result<(), ToolError> {
    let Some(opened) = opened else {
        return Ok(());
    };
    if opened == crate::security::protected_additional::Opened::Unchecked {
        tracing::warn!(
            target: "codex_core::security::tainted_action",
            call_id = %tool_ctx.call_id,
            "additional permissions could not be checked against the protected paths; refused"
        );
    } else {
        tracing::warn!(
            target: "codex_core::security::tainted_action",
            call_id = %tool_ctx.call_id,
            "additional permissions would open a protected path; refused"
        );
    }
    let refusal = opened.refusal(level);
    crate::security::protected_reads::notify(&tool_ctx.session, &tool_ctx.turn, refusal.clone())
        .await;
    Err(ToolError::Rejected(refusal))
}

/// PF-30-S03: refuse when the taint or the effective policy changed while
/// the human was deciding; they approved under what they saw.
fn post_taint_recheck(
    action: &crate::security::tainted_action::PostTaintAction,
    tool_ctx: &ToolCtx,
) -> Result<(), ToolError> {
    let now = tool_ctx.session.services.model_client().post_taint_state();
    action.recheck(now.as_ref()).map_err(|refusal| {
        post_taint_outcome(action, tool_ctx, "refused_stale", /*waited*/ None);
        ToolError::Rejected(refusal)
    })
}

/// PF-30-S03 / PF-26: one log line per post-taint decision, with how long
/// the human took, so research workflows can count prompts and their cost.
fn post_taint_outcome(
    action: &crate::security::tainted_action::PostTaintAction,
    tool_ctx: &ToolCtx,
    outcome: &'static str,
    waited: Option<std::time::Duration>,
) {
    crate::security::inspection::record_protected_action(
        Some(tool_ctx.session.thread_id()),
        action.kind,
        outcome,
    );
    tracing::info!(
        target: "codex_core::security::tainted_action",
        kind = ?action.kind,
        taint_generation = action.state.taint_generation,
        outcome,
        waited_ms = waited.map(|waited| waited.as_millis() as u64),
        call_id = %tool_ctx.call_id,
        "post-taint decision"
    );
}

fn build_denial_reason_from_output(_output: &ExecToolCallOutput) -> String {
    // Keep approval reason terse and stable for UX/tests, but accept the
    // output so we can evolve heuristics later without touching call sites.
    "command failed; retry without sandbox?".to_string()
}

fn allow_on_request_sandbox_startup_retry(
    approval_policy: AskForApproval,
    already_approved: bool,
    sandbox_startup_failure: bool,
) -> bool {
    matches!(approval_policy, AskForApproval::OnRequest)
        && already_approved
        && sandbox_startup_failure
}

fn is_sandbox_startup_failure(sandbox: SandboxType, output: &ExecToolCallOutput) -> bool {
    if sandbox != SandboxType::LinuxSeccomp {
        return false;
    }
    if !output.stdout.text.trim().is_empty() {
        return false;
    }

    let text =
        format!("{}\n{}", output.stderr.text, output.aggregated_output.text).to_ascii_lowercase();

    const STARTUP_FAILURE_MARKERS: &[&str] = &[
        "bubblewrap is unavailable",
        "failed to exec bundled bubblewrap",
        "failed to exec system bubblewrap",
        "loopback: failed rtm_newaddr",
        "loopback: failed rtm_newlink",
        "no permissions to create a new namespace",
        "setting up uid map: permission denied",
        "creating new namespace failed",
        "cannot create user namespace",
    ];

    STARTUP_FAILURE_MARKERS
        .iter()
        .any(|marker| text.contains(marker))
}

#[cfg(test)]
mod tests {
    use super::*;
    use codex_protocol::exec_output::StreamOutput;

    /// Issue #540: a run that keeps its sandbox leaves it on retry only for
    /// a protected action a person approves afresh, never for a network
    /// approval; other runs are unchanged.
    #[test]
    fn issue_540_retry_leaves_the_sandbox_only_where_a_person_approves_it() {
        // (policy allows, keeps sandbox, protected action, network) -> leaves
        for (policy, keeps, protected, network, leaves) in [
            (true, false, false, false, true),
            (true, false, false, true, true),
            (false, false, true, false, false),
            (true, true, false, false, false),
            (true, true, true, false, true),
            (true, true, true, true, false),
            (true, true, false, true, false),
            (false, true, true, false, false),
        ] {
            assert_eq!(
                retry_may_leave_sandbox(policy, keeps, protected, network),
                leaves,
                "{policy} {keeps} {protected} {network}"
            );
        }
    }

    /// #826: the rule joins a sandboxed profile once and changes nothing
    /// else; a profile without Corbanu's sandbox cannot carry it.
    #[test]
    fn issue_826_user_namespace_rule_joins_sandboxed_profiles_only() {
        use codex_protocol::models::PermissionProfile;
        use codex_protocol::permissions::NetworkSandboxPolicy;
        let workspace = PermissionProfile::workspace_write_with(
            &[],
            NetworkSandboxPolicy::Restricted,
            /*exclude_tmpdir_env_var*/ true,
            /*exclude_slash_tmp*/ true,
        );
        let denied = deny_user_namespaces(&workspace);
        let (policy, network) = denied.to_runtime_permissions();
        let (before, before_network) = workspace.to_runtime_permissions();
        assert!(policy.denies_user_namespaces());
        assert!(!before.denies_user_namespaces());
        assert_eq!(network, before_network);
        assert_eq!(denied.enforcement(), workspace.enforcement());
        assert_eq!(policy.entries.len(), before.entries.len() + 1);
        assert_eq!(deny_user_namespaces(&denied), denied);
        for profile in [
            PermissionProfile::Disabled,
            PermissionProfile::External {
                network: NetworkSandboxPolicy::Restricted,
            },
        ] {
            assert_eq!(deny_user_namespaces(&profile), profile);
        }
    }

    fn denied_output(text: &str) -> ExecToolCallOutput {
        output_with_streams("", text)
    }

    fn output_with_streams(stdout: &str, stderr: &str) -> ExecToolCallOutput {
        ExecToolCallOutput {
            exit_code: 1,
            stdout: StreamOutput::new(stdout.to_string()),
            stderr: StreamOutput::new(stderr.to_string()),
            aggregated_output: StreamOutput::new(format!("{stdout}\n{stderr}")),
            ..Default::default()
        }
    }

    #[test]
    fn linux_sandbox_startup_failure_is_classified_narrowly() {
        assert!(is_sandbox_startup_failure(
            SandboxType::LinuxSeccomp,
            &denied_output("bwrap: loopback: Failed RTM_NEWADDR: Operation not permitted"),
        ));
        assert!(is_sandbox_startup_failure(
            SandboxType::LinuxSeccomp,
            &denied_output("bubblewrap is unavailable: no system bwrap was found"),
        ));
        // #563: the helper's refusal when no bubblewrap is found.
        #[cfg(target_os = "linux")]
        assert!(is_sandbox_startup_failure(
            SandboxType::LinuxSeccomp,
            &denied_output(codex_sandboxing::BWRAP_UNAVAILABLE_MESSAGE),
        ));
        assert!(!is_sandbox_startup_failure(
            SandboxType::LinuxSeccomp,
            &denied_output("sandbox fails to start"),
        ));
        assert!(!is_sandbox_startup_failure(
            SandboxType::LinuxSeccomp,
            &output_with_streams(
                "command ran before failing",
                "bwrap: loopback: Failed RTM_NEWADDR: Operation not permitted",
            ),
        ));
        assert!(!is_sandbox_startup_failure(
            SandboxType::LinuxSeccomp,
            &denied_output("bash: ./script.sh: Permission denied"),
        ));
        assert!(!is_sandbox_startup_failure(
            SandboxType::None,
            &denied_output("bwrap: loopback: Failed RTM_NEWADDR: Operation not permitted"),
        ));
    }

    #[test]
    fn on_request_sandbox_startup_retry_requires_prior_approval() {
        assert!(allow_on_request_sandbox_startup_retry(
            AskForApproval::OnRequest,
            /*already_approved*/ true,
            /*sandbox_startup_failure*/ true,
        ));
        assert!(!allow_on_request_sandbox_startup_retry(
            AskForApproval::OnRequest,
            /*already_approved*/ false,
            /*sandbox_startup_failure*/ true,
        ));
        assert!(!allow_on_request_sandbox_startup_retry(
            AskForApproval::Never,
            /*already_approved*/ true,
            /*sandbox_startup_failure*/ true,
        ));
        assert!(!allow_on_request_sandbox_startup_retry(
            AskForApproval::OnRequest,
            /*already_approved*/ true,
            /*sandbox_startup_failure*/ false,
        ));
    }
}

#[cfg(test)]
#[path = "orchestrator_stale_tests.rs"]
mod stale_tests;

#[cfg(test)]
#[cfg(target_os = "linux")]
#[path = "orchestrator_namespace_note_tests.rs"]
mod namespace_note_tests;
