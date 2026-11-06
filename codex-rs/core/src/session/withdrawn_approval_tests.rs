//! #722 (D279): a command approval whose caller stops waiting (a remote
//! network request that timed out) tells clients to close its prompt; an
//! answered one does not.

use super::*;
use crate::session::tests::make_session_and_context_with_rx;
use crate::state::ActiveTurn;
use pretty_assertions::assert_eq;
use std::time::Duration;

fn ask(
    session: &Arc<Session>,
    turn_context: &Arc<TurnContext>,
    call_id: &'static str,
) -> tokio::task::JoinHandle<ReviewDecision> {
    let session = Arc::clone(session);
    let turn_context = Arc::clone(turn_context);
    tokio::spawn(async move {
        #[allow(deprecated)]
        let cwd = turn_context.cwd.clone();
        session
            .request_command_approval(
                turn_context.as_ref(),
                call_id.to_string(),
                Some(format!("{call_id}#approval")),
                /*environment_id*/ None,
                vec!["curl".to_string(), "https://example.com".to_string()],
                cwd,
                /*reason*/ None,
                /*network_approval_context*/ None,
                /*proposed_execpolicy_amendment*/ None,
                /*additional_permissions*/ None,
                /*available_decisions*/ None,
                /*plugin_attribution_override*/ None,
            )
            .await
    })
}

async fn next_approval_event(events: &async_channel::Receiver<Event>) -> EventMsg {
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let event = events.recv().await.expect("event channel open");
            if matches!(
                event.msg,
                EventMsg::ExecApprovalRequest(_) | EventMsg::ExecApprovalRequestWithdrawn(_)
            ) {
                return event.msg;
            }
        }
    })
    .await
    .expect("an approval event")
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn issue_722_dropped_approval_withdraws_its_prompt() {
    let (session, turn_context, events) = make_session_and_context_with_rx().await;
    *session.active_turn.lock().await = Some(ActiveTurn::default());

    let approval = ask(&session, &turn_context, "call-722-timeout");
    let EventMsg::ExecApprovalRequest(prompt) = next_approval_event(&events).await else {
        panic!("the prompt is shown first");
    };
    // The remote decision timed out: its waiter is dropped unanswered.
    approval.abort();
    let _ = approval.await;

    let EventMsg::ExecApprovalRequestWithdrawn(withdrawn) = next_approval_event(&events).await
    else {
        panic!("the prompt is withdrawn");
    };
    assert_eq!(
        withdrawn,
        ExecApprovalRequestWithdrawnEvent {
            call_id: prompt.call_id.clone(),
            approval_id: prompt.approval_id.clone(),
            turn_id: turn_context.sub_id.clone(),
        }
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn issue_722_answered_approval_is_not_withdrawn() {
    let (session, turn_context, events) = make_session_and_context_with_rx().await;
    *session.active_turn.lock().await = Some(ActiveTurn::default());

    let approval = ask(&session, &turn_context, "call-722-answered");
    let EventMsg::ExecApprovalRequest(prompt) = next_approval_event(&events).await else {
        panic!("the prompt is shown first");
    };
    handlers::exec_approval(
        &session,
        prompt.effective_approval_id(),
        /*turn_id*/ None,
        ReviewDecision::Approved,
    )
    .await;
    assert_eq!(approval.await.unwrap(), ReviewDecision::Approved);

    let withdrawn = std::iter::from_fn(|| events.try_recv().ok())
        .any(|event| matches!(event.msg, EventMsg::ExecApprovalRequestWithdrawn(_)));
    assert!(!withdrawn, "an answered prompt is not withdrawn");
}
