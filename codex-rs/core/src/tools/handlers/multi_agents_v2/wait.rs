use super::*;
use crate::agent::control::ListedAgent;
use crate::session::InputQueueActivity;
use crate::tools::handlers::multi_agents_spec::WaitAgentTimeoutOptions;
use crate::tools::handlers::multi_agents_spec::create_wait_agent_tool_v2;
use codex_protocol::ThreadId;
use codex_tools::ToolSpec;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;
use tokio::time::Instant;
use tokio::time::timeout_at;

pub(crate) struct Handler {
    options: WaitAgentTimeoutOptions,
    empty_waits_by_thread: Arc<tokio::sync::Mutex<HashMap<ThreadId, u8>>>,
}

impl Default for Handler {
    fn default() -> Self {
        Self {
            options: WaitAgentTimeoutOptions::default(),
            empty_waits_by_thread: Arc::new(tokio::sync::Mutex::new(HashMap::new())),
        }
    }
}

impl Handler {
    pub(crate) fn new(options: WaitAgentTimeoutOptions) -> Self {
        Self {
            options,
            empty_waits_by_thread: Arc::new(tokio::sync::Mutex::new(HashMap::new())),
        }
    }
}

impl ToolExecutor<ToolInvocation> for Handler {
    fn tool_name(&self) -> ToolName {
        ToolName::plain("wait_agent")
    }

    fn spec(&self) -> ToolSpec {
        create_wait_agent_tool_v2(self.options)
    }

    fn handle(&self, invocation: ToolInvocation) -> codex_tools::ToolExecutorFuture<'_> {
        Box::pin(self.handle_call(invocation))
    }
}

impl Handler {
    async fn handle_call(
        &self,
        invocation: ToolInvocation,
    ) -> Result<Box<dyn crate::tools::context::ToolOutput>, FunctionCallError> {
        let ToolInvocation {
            session,
            turn,
            payload,
            call_id,
            ..
        } = invocation;
        ensure_manager_tool_allowed(&turn, "wait_agent")?;
        let arguments = function_arguments(payload)?;
        let args: WaitArgs = parse_arguments(&arguments)?;
        let min_timeout_ms = turn.config.multi_agent_v2.min_wait_timeout_ms;
        let max_timeout_ms = turn.config.multi_agent_v2.max_wait_timeout_ms;
        let default_timeout_ms = turn.config.multi_agent_v2.default_wait_timeout_ms;
        let timeout_ms = match args.timeout_ms {
            Some(ms) if ms < min_timeout_ms => {
                return Err(FunctionCallError::RespondToModel(format!(
                    "timeout_ms must be at least {min_timeout_ms}"
                )));
            }
            Some(ms) if ms > max_timeout_ms => {
                return Err(FunctionCallError::RespondToModel(format!(
                    "timeout_ms must be at most {max_timeout_ms}"
                )));
            }
            Some(ms) => ms,
            None => default_timeout_ms,
        };

        let current_path = turn
            .session_source
            .get_agent_path()
            .unwrap_or_else(AgentPath::root);
        let descendant_prefix = format!("{}/", current_path.as_str().trim_end_matches('/'));
        let agents = session
            .services
            .agent_control
            .list_agents(&turn.session_source, /*path_prefix*/ None)
            .await
            .unwrap_or_default();
        if !agents.iter().any(|agent| {
            agent.agent_name.starts_with(&descendant_prefix)
                && !matches!(
                    agent.agent_status,
                    AgentStatus::Shutdown | AgentStatus::NotFound
                )
        }) {
            return Err(FunctionCallError::RespondToModel(
                "wait_agent rejected: this agent has no eligible child agents; return the result to its parent instead"
                    .to_string(),
            ));
        }
        if self
            .empty_waits_by_thread
            .lock()
            .await
            .get(&session.thread_id)
            .is_some_and(|count| *count >= 3)
        {
            return Err(FunctionCallError::RespondToModel(
                "wait_agent watchdog is open after 3 consecutive empty waits; polling is blocked until new work starts"
                    .to_string(),
            ));
        }

        let turn_state = session
            .input_queue
            .turn_state_for_sub_id(&session.active_turn, &turn.sub_id)
            .await;
        let (mut activity_rx, pending_activity) = session
            .input_queue
            .subscribe_activity(turn_state.as_deref())
            .await;

        session
            .emit_turn_item_started(
                &turn,
                &TurnItem::CollabAgentToolCall(CollabAgentToolCallItem {
                    id: call_id.clone(),
                    tool: CollabAgentTool::Wait,
                    status: CollabAgentToolCallStatus::InProgress,
                    sender_thread_id: session.thread_id,
                    receiver_thread_ids: Vec::new(),
                    receiver_agents: Vec::new(),
                    prompt: None,
                    model: None,
                    reasoning_effort: None,
                    provider_account: None,
                    agents_states: Default::default(),
                }),
            )
            .await;

        let deadline = Instant::now() + Duration::from_millis(timeout_ms as u64);
        let outcome = wait_for_activity(&mut activity_rx, pending_activity, deadline).await;
        let consecutive_empty_waits = {
            let mut counts = self.empty_waits_by_thread.lock().await;
            let count = counts.entry(session.thread_id).or_default();
            if outcome == WaitOutcome::TimedOut {
                *count = count.saturating_add(1);
            } else {
                *count = 0;
            }
            *count
        };
        let watchdog_escalated = consecutive_empty_waits >= 3;
        let mut agents = session
            .services
            .agent_control
            .list_agents(&turn.session_source, /*path_prefix*/ None)
            .await
            .unwrap_or_default();
        // #1701: report only the caller's own agents (and theirs), not other
        // loaded threads and their final messages.
        let own_agents = session
            .services
            .agent_control
            .spawn_descendant_agent_names(session.thread_id)
            .await
            .unwrap_or_default();
        agents.retain(|agent| own_agents.contains(&agent.agent_name));
        let result = WaitAgentResult::from_outcome(
            outcome,
            agents,
            current_path.to_string(),
            consecutive_empty_waits,
            watchdog_escalated,
        );

        if watchdog_escalated
            && let Some(parent_thread_id) = turn.parent_thread_id
            && let Some(parent) = session
                .services
                .agent_control
                .get_agent_metadata(parent_thread_id)
            && let Some(parent_path) = parent.agent_path
        {
            let communication = InterAgentCommunication::new(
                current_path.clone(),
                parent_path,
                Vec::new(),
                format!(
                    "WATCHDOG ESCALATION — {current_path} reached {consecutive_empty_waits} consecutive empty waits. Automatic polling is now blocked; dispatch real work or end the manager turn."
                ),
                /*trigger_turn*/ true,
            )
            .with_kind(codex_protocol::protocol::AgentMessageKind::ControlRequest);
            session
                .services
                .agent_control
                .send_persisted_inter_agent_communication(
                    parent_thread_id,
                    communication,
                    /*parent_turn_id*/ None,
                )
                .await
                .map_err(|err| collab_agent_error(parent_thread_id, err))?;
        }

        session
            .emit_turn_item_completed(
                &turn,
                TurnItem::CollabAgentToolCall(CollabAgentToolCallItem {
                    id: call_id,
                    tool: CollabAgentTool::Wait,
                    status: CollabAgentToolCallStatus::Completed,
                    sender_thread_id: session.thread_id,
                    receiver_thread_ids: Vec::new(),
                    receiver_agents: Vec::new(),
                    prompt: None,
                    model: None,
                    reasoning_effort: None,
                    provider_account: None,
                    agents_states: HashMap::new(),
                }),
            )
            .await;

        Ok(boxed_tool_output(result))
    }
}

impl CoreToolRuntime for Handler {
    fn repeated_identical_calls_are_polling(&self) -> bool {
        true
    }
    fn matches_kind(&self, payload: &ToolPayload) -> bool {
        matches!(payload, ToolPayload::Function { .. })
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct WaitArgs {
    timeout_ms: Option<i64>,
}

#[derive(Debug, Deserialize, Serialize, PartialEq, Eq)]
pub(crate) struct WaitAgentResult {
    pub(crate) message: String,
    pub(crate) timed_out: bool,
    pub(crate) waiting_for: String,
    pub(crate) wake_conditions: String,
    pub(crate) consecutive_empty_waits: u8,
    pub(crate) watchdog_escalated: bool,
    pub(crate) agents: Vec<ListedAgent>,
}

impl WaitAgentResult {
    fn from_outcome(
        outcome: WaitOutcome,
        agents: Vec<ListedAgent>,
        waiting_for: String,
        consecutive_empty_waits: u8,
        watchdog_escalated: bool,
    ) -> Self {
        let message = match outcome {
            WaitOutcome::MailboxActivity => "Wait completed.",
            WaitOutcome::Steered => "Wait interrupted by new input.",
            WaitOutcome::TimedOut if watchdog_escalated => {
                "WATCHDOG ESCALATION: 3 consecutive empty waits; further polling is blocked."
            }
            WaitOutcome::TimedOut => "Wait timed out.",
        };
        Self {
            message: message.to_string(),
            timed_out: outcome == WaitOutcome::TimedOut,
            waiting_for,
            wake_conditions:
                "child completion or message, follow-up task, human steering, or timeout"
                    .to_string(),
            consecutive_empty_waits,
            watchdog_escalated,
            agents,
        }
    }
}

impl ToolOutput for WaitAgentResult {
    fn log_preview(&self) -> String {
        tool_output_json_text(self, "wait_agent")
    }

    fn success_for_logging(&self) -> bool {
        true
    }

    fn to_response_item(&self, call_id: &str, payload: &ToolPayload) -> ResponseInputItem {
        tool_output_response_item(call_id, payload, self, /*success*/ None, "wait_agent")
    }

    fn code_mode_result(&self, _payload: &ToolPayload) -> JsonValue {
        tool_output_code_mode_result(self, "wait_agent")
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum WaitOutcome {
    MailboxActivity,
    Steered,
    TimedOut,
}

async fn wait_for_activity(
    activity_rx: &mut tokio::sync::watch::Receiver<InputQueueActivity>,
    pending_activity: Option<InputQueueActivity>,
    deadline: Instant,
) -> WaitOutcome {
    if let Some(activity) = pending_activity {
        return match activity {
            InputQueueActivity::Mailbox => WaitOutcome::MailboxActivity,
            InputQueueActivity::Steer => WaitOutcome::Steered,
        };
    }
    match timeout_at(deadline, activity_rx.changed()).await {
        Ok(Ok(())) => match *activity_rx.borrow_and_update() {
            InputQueueActivity::Mailbox => WaitOutcome::MailboxActivity,
            InputQueueActivity::Steer => WaitOutcome::Steered,
        },
        Ok(Err(_)) | Err(_) => WaitOutcome::TimedOut,
    }
}
