use super::TurnInput;
use super::tests::make_session_and_context;
use crate::state::ActiveTurn;
use codex_protocol::AgentPath;
use codex_protocol::protocol::InterAgentCommunication;
use futures::FutureExt;
use pretty_assertions::assert_eq;
use std::sync::Arc;

#[tokio::test]
async fn pending_mailbox_wake_checks_idle_before_preparing_a_turn() {
    for busy in [true, false] {
        let (session, _turn_context) = make_session_and_context().await;
        let session = Arc::new(session);
        let active_turn = busy.then(ActiveTurn::default);
        let original_turn_state = active_turn
            .as_ref()
            .map(|turn| Arc::clone(&turn.turn_state));
        *session.active_turn.lock().await = active_turn;
        let mail = InterAgentCommunication::new(
            AgentPath::root(),
            AgentPath::try_from("/root/worker").expect("agent path"),
            Vec::new(),
            "continue the queued work".to_string(),
            /*trigger_turn*/ true,
        );
        session
            .input_queue
            .enqueue_mailbox_communication(mail.clone(), Some("parent-turn".to_string()))
            .await;

        // Turn preparation first locks session state. Keep it unavailable so one poll proves
        // the busy wake returns before preparation (and therefore before the auth read), while
        // the idle control still reaches preparation. This needs no timeout or native vault.
        let outcome = {
            let _state = session.state.lock().await;
            tokio::task::unconstrained(
                session.maybe_start_turn_for_pending_work_with_sub_id("mailbox-wake".to_string()),
            )
            .now_or_never()
        };
        assert_eq!(outcome, busy.then_some(()));

        {
            let active_turn = session.active_turn.lock().await;
            match (active_turn.as_ref(), original_turn_state.as_ref()) {
                (Some(turn), Some(original)) => assert!(Arc::ptr_eq(&turn.turn_state, original)),
                (None, None) => {}
                _ => panic!("the wake changed the active turn before preparing a new one"),
            }
        }
        assert_eq!(
            session.input_queue.drain_mailbox_input_items().await,
            (
                vec![TurnInput::InterAgentCommunication(mail)],
                Some("parent-turn".to_string()),
            )
        );
    }
}
