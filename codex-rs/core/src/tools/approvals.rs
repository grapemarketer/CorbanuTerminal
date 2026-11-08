//! Central approval policy-stage execution and reviewer routing.

use crate::guardian::guardian_timeout_message;
use crate::guardian::new_guardian_review_id;
use crate::guardian::review_approval_request;
use crate::guardian::routes_approval_to_guardian_with_reviewer;
use crate::hook_runtime::run_permission_request_hooks;
use crate::sandboxing::SandboxPermissions;
use crate::session::turn_context::TurnContext;
use crate::tools::flat_tool_name;
use crate::tools::sandboxing::ApprovalCtx;
use crate::tools::sandboxing::ToolCtx;
use crate::tools::sandboxing::ToolError;
use crate::tools::sandboxing::ToolRuntime;
use codex_config::types::ApprovalsReviewer;
use codex_hooks::PermissionRequestDecision;
use codex_otel::ToolDecisionSource;
use codex_protocol::models::AdditionalPermissionProfile;
use codex_protocol::protocol::NetworkPolicyRuleAction;
use codex_protocol::protocol::ReviewDecision;
use codex_utils_absolute_path::AbsolutePathBuf;
use codex_utils_path_uri::PathUri;

#[derive(Clone, Debug, PartialEq)]
pub(crate) enum ApprovalAction {
    Shell {
        id: String,
        environment_id: String,
        command: Vec<String>,
        cwd: PathUri,
        sandbox_permissions: SandboxPermissions,
        additional_permissions: Option<AdditionalPermissionProfile>,
        justification: Option<String>,
    },
    ExecCommand {
        id: String,
        environment_id: String,
        command: Vec<String>,
        cwd: PathUri,
        sandbox_permissions: SandboxPermissions,
        additional_permissions: Option<AdditionalPermissionProfile>,
        justification: Option<String>,
        tty: bool,
    },
    ApplyPatch {
        id: String,
        environment_id: String,
        cwd: PathUri,
        files: Vec<PathUri>,
        patch: String,
    },
}

impl ApprovalAction {
    fn into_guardian_request(self) -> std::io::Result<crate::guardian::GuardianApprovalRequest> {
        Ok(match self {
            Self::Shell {
                id,
                environment_id,
                command,
                cwd,
                sandbox_permissions,
                additional_permissions,
                justification,
            } => crate::guardian::GuardianApprovalRequest::Shell {
                id,
                command,
                cwd: guardian_cwd(&environment_id, cwd)?,
                sandbox_permissions,
                additional_permissions,
                justification,
            },
            Self::ExecCommand {
                id,
                environment_id,
                command,
                cwd,
                sandbox_permissions,
                additional_permissions,
                justification,
                tty,
            } => crate::guardian::GuardianApprovalRequest::ExecCommand {
                id,
                command,
                cwd: guardian_cwd(&environment_id, cwd)?,
                sandbox_permissions,
                additional_permissions,
                justification,
                tty,
            },
            Self::ApplyPatch {
                id,
                environment_id,
                cwd,
                files,
                patch,
            } => crate::guardian::GuardianApprovalRequest::ApplyPatch {
                id,
                cwd: guardian_cwd(&environment_id, cwd)?,
                files: files
                    .into_iter()
                    .map(|path| path.to_abs_path())
                    .collect::<std::io::Result<Vec<_>>>()?,
                patch,
            },
        })
    }
}

fn guardian_cwd(environment_id: &str, cwd: PathUri) -> std::io::Result<AbsolutePathBuf> {
    match cwd.to_abs_path() {
        Ok(cwd) => Ok(cwd),
        Err(err) if environment_id != codex_exec_server::LOCAL_ENVIRONMENT_ID => Err(err),
        // A `\\localhost` UNC path: `to_file_path` would panic (#1863).
        Err(err) if cwd.to_url().host_str() == Some("") => Err(err),
        Err(_) => {
            let cwd_display = cwd.to_string();
            let path = cwd.to_url().to_file_path().map_err(|()| {
                std::io::Error::new(
                    std::io::ErrorKind::InvalidInput,
                    format!("local cwd URI `{cwd_display}` is not a host-native path"),
                )
            })?;
            AbsolutePathBuf::from_absolute_path_checked(path).map_err(|err| {
                std::io::Error::new(
                    std::io::ErrorKind::InvalidInput,
                    format!("local cwd URI `{cwd_display}` is not absolute: {err}"),
                )
            })
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum ApprovalReviewer {
    Guardian,
    User,
}

impl ApprovalReviewer {
    pub(super) fn for_turn(turn: &TurnContext) -> Self {
        Self::for_reviewer(turn, turn.config.approvals_reviewer)
    }

    fn for_reviewer(turn: &TurnContext, reviewer: ApprovalsReviewer) -> Self {
        if routes_approval_to_guardian_with_reviewer(turn, reviewer) {
            Self::Guardian
        } else {
            Self::User
        }
    }
}

/// Who answered an approval.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum ApprovalResolutionSource {
    Hook,
    Guardian,
    /// A person, asked about this request now.
    User,
    /// Answered without asking anyone: an earlier "don't ask again" answer
    /// (the approval cache) or a stored permission grant (issue #513).
    Remembered,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct ApprovalResolution {
    decision: ReviewDecision,
    source: ApprovalResolutionSource,
}

impl ApprovalResolution {
    fn into_tool_result(self) -> Result<ReviewDecision, ToolError> {
        let source = self.source;
        match self.decision {
            ReviewDecision::NetworkPolicyAmendment {
                network_policy_amendment,
            } if network_policy_amendment.action == NetworkPolicyRuleAction::Deny => {
                let rejection = match source {
                    ApprovalResolutionSource::Hook => "rejected by configuration",
                    ApprovalResolutionSource::Guardian => {
                        "automatic approval review denied the action"
                    }
                    ApprovalResolutionSource::User | ApprovalResolutionSource::Remembered => {
                        "rejected by user"
                    }
                };
                Err(ToolError::Declined(rejection.to_string()))
            }
            ReviewDecision::Denied { rejection } => Err(ToolError::Declined(rejection)),
            ReviewDecision::TimedOut => Err(ToolError::Declined(guardian_timeout_message())),
            ReviewDecision::Abort => {
                Err(ToolError::Declined("approval request aborted".to_string()))
            }
            decision => Ok(decision),
        }
    }
}

/// Who lists a refused approval under the inspector's recent denials (#437).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum DeclineRecord {
    /// This function does.
    Here,
    /// The caller records the outcome itself (protected actions after
    /// untrusted content).
    ByCaller,
}

#[allow(clippy::too_many_arguments)]
pub(super) async fn resolve_tool_apporval<Rq, Out, T>(
    tool: &mut T,
    req: &Rq,
    permission_request_run_id: &str,
    ctx: ApprovalCtx<'_>,
    tool_ctx: &ToolCtx,
    reviewer: ApprovalReviewer,
    otel: &codex_otel::SessionTelemetry,
    decline_record: DeclineRecord,
    asked_again: bool,
) -> Result<ApprovalResolutionSource, ToolError>
where
    T: ToolRuntime<Rq, Out>,
{
    let what = approval_kind(&flat_tool_name(&tool_ctx.tool_name));
    let thread = tool_ctx.session.thread_id();
    // An interrupted wait is "declined by you" only when the person was
    // asked (not a hook or the automatic reviewer).
    let asks_person = ctx.fresh_human_authority || reviewer == ApprovalReviewer::User;
    // D50: only a person is told why they are asked again.
    let ctx = if asked_again && asks_person {
        asked_again_ctx(tool, req, ctx)
    } else {
        ctx
    };
    let mut pending = (decline_record == DeclineRecord::Here)
        .then(|| crate::security::inspection::PendingApproval::new(thread, what, asks_person));
    let resolution = resolve(
        tool,
        req,
        permission_request_run_id,
        ctx,
        tool_ctx,
        reviewer,
        otel,
    )
    .await;
    if let Some(pending) = pending.as_mut() {
        pending.answered();
        if let Some(outcome) = decline_outcome(&resolution) {
            crate::security::inspection::record_declined_approval(thread, what, outcome);
        }
    }
    let source = resolution.source;
    resolution.into_tool_result().map(|_| source)
}

/// D50 (#1001): the note an approval asked again carries, because the one
/// shown before a security transition went stale.
pub(crate) const ASKED_AGAIN_NOTE: &str = "The security level changed while the earlier \
     prompt was open, so it is asked again under the new level.";

/// D50: `ctx` with [`ASKED_AGAIN_NOTE`] ahead of its reason, or of the
/// model's justification when it has none.
fn asked_again_ctx<'a, Rq, Out, T>(tool: &T, req: &Rq, ctx: ApprovalCtx<'a>) -> ApprovalCtx<'a>
where
    T: ToolRuntime<Rq, Out>,
{
    let why = ctx
        .retry_reason
        .clone()
        .or_else(|| match tool.approval_action(req, &ctx) {
            Ok(
                ApprovalAction::Shell { justification, .. }
                | ApprovalAction::ExecCommand { justification, .. },
            ) => justification,
            _ => None,
        });
    ApprovalCtx {
        retry_reason: Some(match why {
            Some(why) => format!("{ASKED_AGAIN_NOTE} {why}"),
            None => ASKED_AGAIN_NOTE.to_string(),
        }),
        ..ctx
    }
}

/// What the inspector says about a refused approval, or `None` when it was
/// approved.
fn decline_outcome(resolution: &ApprovalResolution) -> Option<&'static str> {
    let refused = match &resolution.decision {
        ReviewDecision::NetworkPolicyAmendment {
            network_policy_amendment,
        } => network_policy_amendment.action == NetworkPolicyRuleAction::Deny,
        ReviewDecision::Denied { .. } | ReviewDecision::Abort => true,
        ReviewDecision::TimedOut => return Some("not answered in time"),
        _ => false,
    };
    refused.then_some(match resolution.source {
        ApprovalResolutionSource::User | ApprovalResolutionSource::Remembered => "declined by you",
        ApprovalResolutionSource::Hook => "declined by a permission hook",
        ApprovalResolutionSource::Guardian => "declined by the automatic reviewer",
    })
}

fn approval_kind(tool_name: &str) -> &'static str {
    match tool_name {
        "exec_command" | "shell" | "shell_command" | "local_shell" | "container.exec" => {
            "command approval"
        }
        "apply_patch" => "file edit approval",
        _ => "tool approval",
    }
}

async fn resolve<Rq, Out, T>(
    tool: &mut T,
    req: &Rq,
    permission_request_run_id: &str,
    ctx: ApprovalCtx<'_>,
    tool_ctx: &ToolCtx,
    reviewer: ApprovalReviewer,
    otel: &codex_otel::SessionTelemetry,
) -> ApprovalResolution
where
    T: ToolRuntime<Rq, Out>,
{
    if let Some(permission_request) = tool.permission_request_payload(req) {
        match run_permission_request_hooks(
            ctx.session,
            ctx.turn,
            permission_request_run_id,
            permission_request,
        )
        .await
        {
            // A hook can still refuse a post-taint action, but only the human
            // can allow it (PF-30-S03).
            Some(PermissionRequestDecision::Allow) if ctx.fresh_human_authority => {}
            Some(PermissionRequestDecision::Allow) => {
                let resolution = ApprovalResolution {
                    decision: ReviewDecision::Approved,
                    source: ApprovalResolutionSource::Hook,
                };
                record_resolution(otel, tool_ctx, &resolution);
                return resolution;
            }
            Some(PermissionRequestDecision::Deny { message }) => {
                let resolution = ApprovalResolution {
                    decision: ReviewDecision::denied(message),
                    source: ApprovalResolutionSource::Hook,
                };
                record_resolution(otel, tool_ctx, &resolution);
                return resolution;
            }
            None => {}
        }
    }

    let reviewer = if ctx.fresh_human_authority {
        ApprovalReviewer::User
    } else {
        reviewer
    };
    let resolution = match reviewer {
        ApprovalReviewer::Guardian => {
            let review_id = new_guardian_review_id();
            let action = match tool
                .approval_action(req, &ctx)
                .and_then(ApprovalAction::into_guardian_request)
            {
                Ok(action) => action,
                Err(err) => {
                    tracing::error!(%err, "failed to build automatic approval action");
                    let resolution = ApprovalResolution {
                        decision: ReviewDecision::denied(
                            "automatic approval review could not prepare the action",
                        ),
                        source: ApprovalResolutionSource::Guardian,
                    };
                    record_resolution(otel, tool_ctx, &resolution);
                    return resolution;
                }
            };
            review_approval_request(
                ctx.session,
                ctx.turn,
                review_id,
                action,
                ctx.retry_reason.clone(),
            )
            .await
        }
        ApprovalReviewer::User => {
            let (decision, asked) = crate::tools::sandboxing::asking_person(|| {
                tool.start_approval_async(req, ctx.clone())
            })
            .await;
            let source = if asked {
                ApprovalResolutionSource::User
            } else {
                ApprovalResolutionSource::Remembered
            };
            tool.note_review_decision(&decision, &ctx);
            let resolution = ApprovalResolution { decision, source };
            record_resolution(otel, tool_ctx, &resolution);
            return resolution;
        }
    };
    tool.note_review_decision(&resolution, &ctx);
    let source = ApprovalResolutionSource::Guardian;
    let resolution = ApprovalResolution {
        decision: resolution,
        source,
    };
    record_resolution(otel, tool_ctx, &resolution);
    resolution
}

fn record_resolution(
    otel: &codex_otel::SessionTelemetry,
    tool_ctx: &ToolCtx,
    resolution: &ApprovalResolution,
) {
    let source = match resolution.source {
        ApprovalResolutionSource::Hook => ToolDecisionSource::Config,
        ApprovalResolutionSource::Guardian => ToolDecisionSource::AutomatedReviewer,
        ApprovalResolutionSource::User | ApprovalResolutionSource::Remembered => {
            ToolDecisionSource::User
        }
    };
    let tool_name = flat_tool_name(&tool_ctx.tool_name);
    otel.tool_decision(
        tool_name.as_ref(),
        &tool_ctx.call_id,
        &resolution.decision,
        source,
    );
}

#[cfg(all(test, unix))]
#[path = "approvals_tests.rs"]
mod tests;

#[cfg(all(test, windows))]
#[path = "approvals_windows_tests.rs"]
mod windows_tests;

#[cfg(test)]
#[path = "refusal_diagnostics_tests.rs"]
mod refusal_diagnostics_tests;
