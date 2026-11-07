use super::AgentControl;
use codex_protocol::ThreadId;
use codex_protocol::error::Result as CodexResult;
use codex_protocol::protocol::Event;
use codex_protocol::protocol::EventMsg;
use codex_protocol::protocol::SessionSource;
use codex_protocol::protocol::WarningEvent;

impl AgentControl {
    /// Shows a held child's warning on its live parent's client channel without
    /// starting a parent turn, changing agent status, or injecting model input.
    pub(crate) async fn warn_parent_of_held_mailbox(
        &self,
        child_thread_id: ThreadId,
        session_source: &SessionSource,
        message: String,
    ) -> CodexResult<()> {
        let Some(parent_thread_id) = session_source.parent_thread_id() else {
            return Ok(());
        };
        let state = self.upgrade()?;
        let parent = state.get_thread(parent_thread_id).await?;
        let child = session_source
            .get_agent_path()
            .map(|path| path.to_string())
            .unwrap_or_else(|| child_thread_id.to_string());
        parent
            .session
            .send_event_raw(Event {
                id: String::new(),
                msg: EventMsg::Warning(WarningEvent {
                    message: format!("Agent {child}: {message}"),
                }),
            })
            .await;
        Ok(())
    }
}
