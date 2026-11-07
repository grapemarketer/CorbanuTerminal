//! PF-23-S01: the post-taint check for routes outside the shared approval
//! seam. The same rule as PF-30-S03: a protected action after untrusted
//! content needs a fresh approval from the human, bound to the taint and the
//! security policy it was given under; the kill switch and
//! `approval_policy = never` refuse it. Hooks, remembered approvals and the
//! automatic reviewer are never asked.

use super::Coverage;
use super::ToolOrigin;
use super::coverage;
use crate::security::tainted_action::PostTaintAction;
use crate::security::tainted_action::ProtectedActionKind;
use crate::session::session::Session;
use crate::session::turn_context::TurnContext;
use crate::tools::context::ToolInvocation;
use codex_protocol::protocol::AskForApproval;
use codex_protocol::request_user_input::RequestUserInputArgs;
use codex_protocol::request_user_input::RequestUserInputQuestion;
use codex_protocol::request_user_input::RequestUserInputQuestionOption;
use std::time::Duration;
use std::time::Instant;

/// How long classifying one action may take; a stall fails closed.
const CLASSIFY_TIMEOUT: Duration = Duration::from_secs(3);
/// Not the MCP approval prefix: delegated MCP approval questions can be
/// answered by the automatic reviewer, these never are.
const QUESTION_PREFIX: &str = "security_post_taint";
const ALLOW: &str = "Allow once";
const CANCEL: &str = "Cancel";

/// Routes with their own post-taint check; stable names for the audit log.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Route {
    McpTool,
    WriteStdin,
    UnclassifiedTool,
    AgentLaunch,
    PolicyRequest,
}

impl Route {
    fn as_str(self) -> &'static str {
        match self {
            Self::McpTool => "mcp_tool",
            Self::WriteStdin => "write_stdin",
            Self::UnclassifiedTool => "unclassified_tool",
            Self::AgentLaunch => "agent_launch",
            Self::PolicyRequest => "policy_request",
        }
    }
}

pub(crate) enum Admission {
    /// Not protected, or post-taint checks do not apply.
    Clear,
    /// Refused without asking (kill switch, approvals off).
    Refused(String),
    /// Ask the human, then call [`HumanCheck::resolve`] with the answer.
    AskHuman(HumanCheck),
}

pub(crate) struct HumanCheck {
    action: PostTaintAction,
    route: Route,
    call_id: String,
    asked: Instant,
    /// PF-41-S01: an unanswered check is recorded as declined.
    pending: crate::security::inspection::PendingProtectedAction,
}

impl HumanCheck {
    pub(crate) fn reason(&self) -> String {
        self.action.approval_reason()
    }

    /// Record the answer; an approval holds only if the taint and policy are
    /// still the ones the human saw.
    pub(crate) fn resolve(mut self, session: &Session, approved: bool) -> Result<(), String> {
        self.pending.answered();
        let outcome = if approved { "approved" } else { "declined" };
        log(
            session.thread_id(),
            self.route,
            &self.action,
            &self.call_id,
            outcome,
            Some(self.asked.elapsed()),
        );
        if !approved {
            return Err(format!(
                "Not run: you declined {} after untrusted content entered the session.",
                self.action.kind.describe()
            ));
        }
        let now = session.services.model_client().post_taint_state();
        self.action.recheck(now.as_ref()).inspect_err(|_| {
            log(
                session.thread_id(),
                self.route,
                &self.action,
                &self.call_id,
                "refused_stale",
                /*waited*/ None,
            );
        })
    }
}

/// Decide whether `route` may go ahead. `classify` runs only once the
/// session holds untrusted content under a protected level, off the async
/// workers and bounded in time.
pub(crate) async fn admit<F>(
    session: &Session,
    approval_policy: AskForApproval,
    route: Route,
    call_id: &str,
    classify: F,
) -> Admission
where
    F: FnOnce() -> Option<ProtectedActionKind> + Send + 'static,
{
    let Some(state) = session
        .services
        .model_client()
        .post_taint_state()
        .filter(|state| state.taint_generation > 0)
    else {
        return Admission::Clear;
    };
    let started = Instant::now();
    let classified =
        tokio::time::timeout(CLASSIFY_TIMEOUT, tokio::task::spawn_blocking(classify)).await;
    let kind = match classified {
        Ok(Ok(Some(kind))) => kind,
        Ok(Ok(None)) => return Admission::Clear,
        Ok(Err(_)) | Err(_) => ProtectedActionKind::UnseenCode,
    };
    let action = PostTaintAction { kind, state };
    tracing::info!(
        target: "codex_core::security::tainted_action",
        route = route.as_str(),
        kind = ?action.kind,
        taint_generation = action.state.taint_generation,
        classify_us = started.elapsed().as_micros() as u64,
        call_id,
        "post-taint protected action needs fresh human approval"
    );
    if let Some(refusal) = action.refused_up_front() {
        log(
            session.thread_id(),
            route,
            &action,
            call_id,
            "refused_kill_switch",
            /*waited*/ None,
        );
        return Admission::Refused(refusal);
    }
    if approval_policy == AskForApproval::Never {
        log(
            session.thread_id(),
            route,
            &action,
            call_id,
            "refused_approvals_off",
            /*waited*/ None,
        );
        return Admission::Refused(action.approvals_off_rejection());
    }
    Admission::AskHuman(HumanCheck {
        pending: crate::security::inspection::PendingProtectedAction::new(
            session.thread_id(),
            action.kind,
        ),
        action,
        route,
        call_id: call_id.to_string(),
        asked: Instant::now(),
    })
}

/// Ask the human a yes/no question; anything but "Allow once" is a no.
pub(crate) async fn ask_human(
    session: &Session,
    turn: &TurnContext,
    call_id: &str,
    question: String,
) -> bool {
    ask_human_answer(session, turn, call_id, question).await == Some(true)
}

/// The id of the question [`ask_human`] asks for `call_id`.
pub(crate) fn question_id(call_id: &str) -> String {
    format!("{QUESTION_PREFIX}_{call_id}")
}

/// [`ask_human`], telling an answer (`Some(true)` for "Allow once",
/// `Some(false)` for any other) from no answer at all (`None`): the client
/// cannot ask, such as `corbanu exec`, or the question was dismissed.
pub(crate) async fn ask_human_answer(
    session: &Session,
    turn: &TurnContext,
    call_id: &str,
    question: String,
) -> Option<bool> {
    let id = question_id(call_id);
    let option = |label: &str, description: &str| RequestUserInputQuestionOption {
        label: label.to_string(),
        description: description.to_string(),
    };
    let args = RequestUserInputArgs {
        questions: vec![RequestUserInputQuestion {
            id: id.clone(),
            header: "Security level".to_string(),
            question,
            is_other: false,
            is_secret: false,
            options: Some(vec![
                option(ALLOW, "Run this one call; ask again next time."),
                option(CANCEL, "Do not run it."),
            ]),
        }],
        auto_resolution_ms: None,
    };
    session
        .request_user_input(turn, call_id.to_string(), args)
        .await
        .and_then(|mut response| response.answers.remove(&id))
        .map(|answer| answer.answers == [ALLOW])
}

/// PF-23-S01 dispatch-boundary check: an unclassified route, starting an
/// agent (D320), or a policy request an automatic reviewer would answer,
/// needs the human after untrusted content.
pub(crate) async fn check_dispatch(
    invocation: &ToolInvocation,
    origin: ToolOrigin,
) -> Result<(), String> {
    let (route, kind) = match coverage(origin, &invocation.tool_name) {
        Coverage::Unclassified => (
            Route::UnclassifiedTool,
            ProtectedActionKind::UnclassifiedTool,
        ),
        Coverage::AgentLaunch => (Route::AgentLaunch, ProtectedActionKind::AgentLaunch),
        Coverage::PolicyRequest
            if invocation.tool_name.name != "request_permissions"
                || crate::guardian::routes_approval_to_guardian(&invocation.turn) =>
        {
            (Route::PolicyRequest, ProtectedActionKind::SecurityPolicy)
        }
        Coverage::PolicyRequest
        | Coverage::ApprovalSeam
        | Coverage::OwnCheck
        | Coverage::NestedCallsOnly
        | Coverage::ChildAgent
        | Coverage::NoProtectedEffect => return Ok(()),
    };
    let admission = admit(
        &invocation.session,
        invocation.turn.approval_policy.value(),
        route,
        &invocation.call_id,
        move || Some(kind),
    )
    .await;
    let check = match admission {
        Admission::Clear => return Ok(()),
        Admission::Refused(refusal) => return Err(refusal),
        Admission::AskHuman(check) => check,
    };
    let question = format!("Allow `{}`? {}", invocation.tool_name, check.reason());
    let approved = ask_human(
        &invocation.session,
        &invocation.turn,
        &invocation.call_id,
        question,
    )
    .await;
    check.resolve(&invocation.session, approved)
}

/// One line per post-taint decision (PF-26 counts these); never arguments.
fn log(
    thread: codex_protocol::ThreadId,
    route: Route,
    action: &PostTaintAction,
    call_id: &str,
    outcome: &'static str,
    waited: Option<Duration>,
) {
    crate::security::inspection::record_protected_action(Some(thread), action.kind, outcome);
    tracing::info!(
        target: "codex_core::security::tainted_action",
        route = route.as_str(),
        kind = ?action.kind,
        taint_generation = action.state.taint_generation,
        outcome,
        waited_ms = waited.map(|waited| waited.as_millis() as u64),
        call_id,
        "post-taint decision"
    );
}
