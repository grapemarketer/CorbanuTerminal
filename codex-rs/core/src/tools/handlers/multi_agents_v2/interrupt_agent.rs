use super::*;
use crate::agent_communication::AgentCommunicationContext;
use crate::agent_communication::AgentCommunicationKind;
use crate::tools::handlers::multi_agents_spec::create_interrupt_agent_tool_v2;
use codex_protocol::error::CodexErrorDetails;
use codex_protocol::models::ResponseItemMetadata;
use codex_tools::ToolSpec;

pub(crate) struct Handler;

/// What an interrupt does to an agent that isn't running (D404).
const NOT_RUNNING_EFFECT: &str =
    "not running: nothing was interrupted and the agent was not started";

impl ToolExecutor<ToolInvocation> for Handler {
    fn tool_name(&self) -> ToolName {
        ToolName::plain("interrupt_agent")
    }

    fn spec(&self) -> ToolSpec {
        create_interrupt_agent_tool_v2()
    }

    fn handle(&self, invocation: ToolInvocation) -> codex_tools::ToolExecutorFuture<'_> {
        Box::pin(async move {
            handle_interrupt_agent(invocation)
                .await
                .map(boxed_tool_output)
        })
    }
}

async fn handle_interrupt_agent(
    invocation: ToolInvocation,
) -> Result<InterruptAgentResult, FunctionCallError> {
    let ToolInvocation {
        session,
        turn,
        payload,
        call_id,
        ..
    } = invocation;
    ensure_manager_tool_allowed(&turn, "interrupt_agent")?;
    let arguments = function_arguments(payload)?;
    let args: InterruptAgentArgs = parse_arguments(&arguments)?;
    // OpenAI's reserved collaboration schema exposes only `target`. Keep the richer PF reason
    // outside that wire contract and synthesize a truthful audit reason when the native call does
    // not provide one. Provider-neutral callers may still supply the explicit reason exposed by
    // their superset schema.
    let reason = args
        .reason
        .as_deref()
        .map(str::trim)
        .filter(|reason| !reason.is_empty())
        .unwrap_or("Native collaboration interrupt requested");
    let superseding_task = args
        .superseding_task
        .map(|task| task.trim().to_string())
        .filter(|task| !task.is_empty());
    let explicit_agent_path = AgentPath::try_from(args.target.trim()).ok();
    if explicit_agent_path.as_ref().is_some_and(AgentPath::is_root) {
        return Err(FunctionCallError::RespondToModel(
            "root is not a spawned agent".to_string(),
        ));
    }
    let agent_id = resolve_agent_target(&session, &turn, &args.target).await?;
    if session.services.agent_control.is_root_thread(agent_id) {
        return Err(FunctionCallError::RespondToModel(
            "root is not a spawned agent".to_string(),
        ));
    }
    let receiver_agent = match explicit_agent_path.as_ref() {
        Some(agent_path) => session
            .services
            .agent_control
            .get_agent_metadata_for_path(agent_path)
            .filter(|metadata| metadata.agent_id == Some(agent_id))
            .ok_or_else(|| {
                FunctionCallError::RespondToModel(format!(
                    "live agent path `{agent_path}` no longer matches its resolved thread"
                ))
            })?,
        None => session
            .services
            .agent_control
            .ensure_agent_known(agent_id)
            .map_err(|err| collab_agent_error(agent_id, err))?,
    };
    if agent_id == session.thread_id {
        return Err(FunctionCallError::RespondToModel(
            "an agent cannot interrupt itself; return your result and let the parent interrupt you if needed"
                .to_string(),
        ));
    }
    let receiver_agent_path = receiver_agent.agent_path.clone().ok_or_else(|| {
        FunctionCallError::RespondToModel("target agent is missing an agent_path".to_string())
    })?;
    let status = session.services.agent_control.get_status(agent_id).await;
    let actor_path = turn
        .session_source
        .get_agent_path()
        .unwrap_or_else(AgentPath::root);
    let actor = match (
        turn.session_source.get_nickname(),
        turn.session_source.get_agent_role(),
    ) {
        (Some(nickname), Some(role)) => format!("{nickname} [{role}] · {actor_path}"),
        (Some(nickname), None) => format!("{nickname} · {actor_path}"),
        (None, _) => actor_path.to_string(),
    };
    let target = match (
        receiver_agent.agent_nickname.as_deref(),
        receiver_agent.agent_role.as_deref(),
    ) {
        (Some(nickname), Some(role)) => format!("{nickname} [{role}] · {receiver_agent_path}"),
        (Some(nickname), None) => format!("{nickname} · {receiver_agent_path}"),
        (None, _) => receiver_agent_path.to_string(),
    };
    let not_running = InterruptAgentResult {
        previous_status: status.clone(),
        actor: actor.clone(),
        target: target.clone(),
        reason: reason.to_string(),
        superseding_task: superseding_task.clone(),
        process_effect: NOT_RUNNING_EFFECT.to_string(),
    };
    // D404 (#1796): an agent without a running turn is neither interrupted
    // nor woken (the audit message below would start a turn): the call only
    // reports its status, at every security level.
    if status != AgentStatus::Running {
        return Ok(not_running);
    }
    let process_effect = "model turn aborted; active turn-owned tool processes receive SIGTERM cleanup before abort; durable unified-exec background processes are preserved and remain inspectable in /ps".to_string();
    let audit_copy = format!(
        "Actor: {actor}\nTarget: {target}\nReason: {reason}\nSuperseding task/dispatch: {}\nProcess effect: {process_effect}",
        superseding_task.as_deref().unwrap_or("none")
    );
    match session
        .services
        .agent_control
        .interrupt_agent(agent_id)
        .await
    {
        Ok(_) => {}
        // Its runtime is gone, so it no longer runs: don't wake it either.
        Err(err)
            if matches!(
                err.details(),
                CodexErrorDetails::ThreadNotFound(_) | CodexErrorDetails::InternalAgentDied
            ) =>
        {
            return Ok(not_running);
        }
        Err(err) => return Err(collab_agent_error(agent_id, err)),
    }
    // The runtime owns this plaintext audit record. Keep it typed as a control request so the
    // target pane and durable mailbox retain the full actor/reason/process tuple.
    let mut communication = InterAgentCommunication::new(
        actor_path,
        receiver_agent_path.clone(),
        Vec::new(),
        format!("CONTROL EVENT — INTERRUPT\n{audit_copy}"),
        /*trigger_turn*/ true,
    )
    .with_kind(codex_protocol::protocol::AgentMessageKind::ControlRequest);
    communication
        .metadata
        .get_or_insert_with(ResponseItemMetadata::default)
        .source_call_id = Some(call_id.clone());
    let context =
        AgentCommunicationContext::new(AgentCommunicationKind::Message, session.thread_id);
    session
        .services
        .agent_control
        .send_inter_agent_communication(agent_id, communication, context, Some(turn.sub_id.clone()))
        .await
        .map_err(|err| collab_agent_error(agent_id, err))?;
    emit_sub_agent_activity(
        &session,
        &turn,
        SubAgentActivityItem {
            id: call_id,
            agent_thread_id: agent_id,
            agent_path: receiver_agent_path,
            kind: SubAgentActivityKind::Interrupted,
        },
    )
    .await;

    Ok(InterruptAgentResult {
        previous_status: status,
        actor,
        target,
        reason: reason.to_string(),
        superseding_task,
        process_effect,
    })
}

impl CoreToolRuntime for Handler {
    fn matches_kind(&self, payload: &ToolPayload) -> bool {
        matches!(payload, ToolPayload::Function { .. })
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct InterruptAgentArgs {
    target: String,
    reason: Option<String>,
    superseding_task: Option<String>,
}

#[derive(Debug, Deserialize, Serialize)]
pub(crate) struct InterruptAgentResult {
    pub(crate) previous_status: AgentStatus,
    actor: String,
    target: String,
    reason: String,
    superseding_task: Option<String>,
    process_effect: String,
}

impl ToolOutput for InterruptAgentResult {
    fn log_preview(&self) -> String {
        tool_output_json_text(self, "interrupt_agent")
    }

    fn success_for_logging(&self) -> bool {
        true
    }

    fn to_response_item(&self, call_id: &str, payload: &ToolPayload) -> ResponseInputItem {
        tool_output_response_item(call_id, payload, self, Some(true), "interrupt_agent")
    }

    fn code_mode_result(&self, _payload: &ToolPayload) -> JsonValue {
        tool_output_code_mode_result(self, "interrupt_agent")
    }
}

#[cfg(test)]
mod tests {
    use super::InterruptAgentArgs;

    #[test]
    fn accepts_openai_reserved_target_only_arguments() {
        let args: InterruptAgentArgs =
            serde_json::from_str(r#"{"target":"/root/worker"}"#).expect("target-only args");

        assert_eq!(args.target, "/root/worker");
        assert_eq!(args.reason, None);
        assert_eq!(args.superseding_task, None);
    }

    #[test]
    fn retains_provider_neutral_interrupt_metadata() {
        let args: InterruptAgentArgs = serde_json::from_str(
            r#"{"target":"/root/worker","reason":"new priority","superseding_task":"task-2"}"#,
        )
        .expect("provider-neutral args");

        assert_eq!(args.reason.as_deref(), Some("new priority"));
        assert_eq!(args.superseding_task.as_deref(), Some("task-2"));
    }
}
