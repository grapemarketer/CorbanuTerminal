use std::path::Path;

use codex_app_server_protocol::ServerNotification;
use codex_core::config::Config;
use codex_protocol::protocol::SessionConfiguredEvent;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CodexStatus {
    Running,
    InitiateShutdown,
}

pub(crate) trait EventProcessor {
    /// Print summary of effective configuration and user prompt.
    fn print_config_summary(
        &mut self,
        config: &Config,
        prompt: &str,
        session_configured: &SessionConfiguredEvent,
    );

    /// Handle a single typed app-server notification emitted by the agent.
    fn process_server_notification(&mut self, notification: ServerNotification) -> CodexStatus;

    /// Observe accounting from other threads without rendering their content
    /// or letting their completion terminate the selected turn.
    fn observe_unselected_notification(
        &mut self,
        _notification: &ServerNotification,
        _selected_thread_id: &str,
    ) {
    }

    fn register_usage_thread(&mut self, _thread_id: &str, _parent_thread_id: &str) {}

    fn invalidate_usage(&mut self) {}

    fn usage_is_valid(&self) -> bool {
        true
    }

    fn mark_forked_usage_thread(&mut self, _thread_id: &str) {}

    /// Handle a local exec warning that is not represented as an app-server notification.
    fn process_warning(&mut self, message: String) -> CodexStatus;

    fn print_final_output(&mut self) {}

    /// PF-84 (#480): the account (`<provider>:<name>`) a resumed thread runs
    /// on, which can differ from the one this exec's config selects.
    fn set_provider_account(&mut self, _config: &Config, _account: &str) {}
}

pub(crate) fn handle_last_message(last_agent_message: Option<&str>, output_file: &Path) {
    let message = last_agent_message.unwrap_or_default();
    write_last_message_file(message, Some(output_file));
    if last_agent_message.is_none() {
        eprintln!(
            "Warning: no last agent message; wrote empty content to {}",
            output_file.display()
        );
    }
}

fn write_last_message_file(contents: &str, last_message_path: Option<&Path>) {
    if let Some(path) = last_message_path
        && let Err(e) = std::fs::write(path, contents)
    {
        eprintln!("Failed to write last message file {path:?}: {e}");
    }
}
