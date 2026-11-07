use crate::agent::role::apply_role_to_config;
use crate::agent::role::apply_role_to_config_for_multi_agent_v2;
use crate::config::Config;
use crate::config::DEFAULT_MULTI_AGENT_V2_MIN_WAIT_TIMEOUT_MS;
use crate::config::HARD_MAX_MULTI_AGENT_V2_TIMEOUT_MS;
use crate::function_tool::FunctionCallError;
use crate::session::session::Session;
use crate::session::turn_context::TurnContext;
use crate::tools::context::FunctionToolOutput;
use crate::tools::context::ToolOutput;
use crate::tools::context::ToolPayload;
use codex_features::Feature;
use codex_models_manager::manager::RefreshStrategy;
use codex_protocol::AgentPath;
use codex_protocol::ThreadId;
use codex_protocol::crew::AgentClass;
use codex_protocol::crew::RetentionPolicy;
use codex_protocol::error::CodexErr;
use codex_protocol::error::CodexErrorDetails;
use codex_protocol::models::BaseInstructions;
use codex_protocol::models::ResponseInputItem;
use codex_protocol::openai_models::ModelPreset;
use codex_protocol::openai_models::ReasoningEffort;
use codex_protocol::openai_models::ReasoningEffortPreset;
use codex_protocol::protocol::MultiAgentVersion;
use codex_protocol::protocol::SessionSource;
use codex_protocol::protocol::SubAgentSource;
use codex_protocol::user_input::UserInput;
use serde::Serialize;
use serde_json::Value as JsonValue;

/// Minimum wait timeout to prevent tight polling loops from burning CPU.
pub(crate) const MIN_WAIT_TIMEOUT_MS: i64 = DEFAULT_MULTI_AGENT_V2_MIN_WAIT_TIMEOUT_MS;
pub(crate) const DEFAULT_WAIT_TIMEOUT_MS: i64 = 30_000;
pub(crate) const MAX_WAIT_TIMEOUT_MS: i64 = HARD_MAX_MULTI_AGENT_V2_TIMEOUT_MS;
pub(crate) const MAX_SPAWN_AGENT_MODEL_OVERRIDES: usize = 32;

pub(crate) fn function_arguments(payload: ToolPayload) -> Result<String, FunctionCallError> {
    match payload {
        ToolPayload::Function { arguments } => Ok(arguments),
        _ => Err(FunctionCallError::RespondToModel(
            "collab handler received unsupported payload".to_string(),
        )),
    }
}

pub(crate) fn tool_output_json_text<T>(value: &T, tool_name: &str) -> String
where
    T: Serialize,
{
    serde_json::to_string(value).unwrap_or_else(|err| {
        JsonValue::String(format!("failed to serialize {tool_name} result: {err}")).to_string()
    })
}

pub(crate) fn tool_output_response_item<T>(
    call_id: &str,
    payload: &ToolPayload,
    value: &T,
    success: Option<bool>,
    tool_name: &str,
) -> ResponseInputItem
where
    T: Serialize,
{
    FunctionToolOutput::from_text(tool_output_json_text(value, tool_name), success)
        .to_response_item(call_id, payload)
}

pub(crate) fn tool_output_code_mode_result<T>(value: &T, tool_name: &str) -> JsonValue
where
    T: Serialize,
{
    serde_json::to_value(value).unwrap_or_else(|err| {
        JsonValue::String(format!("failed to serialize {tool_name} result: {err}"))
    })
}

pub(crate) fn collab_spawn_error(err: CodexErr) -> FunctionCallError {
    match err.details() {
        CodexErrorDetails::UnsupportedOperation(message) if message == "thread manager dropped" => {
            FunctionCallError::RespondToModel("collab manager unavailable".to_string())
        }
        CodexErrorDetails::UnsupportedOperation(message) => {
            FunctionCallError::RespondToModel(message.clone())
        }
        _ => FunctionCallError::RespondToModel(format!("collab spawn failed: {err}")),
    }
}

pub(crate) fn collab_agent_error(agent_id: ThreadId, err: CodexErr) -> FunctionCallError {
    match err.details() {
        CodexErrorDetails::ThreadNotFound(id) => {
            FunctionCallError::RespondToModel(format!("agent with id {id} not found"))
        }
        CodexErrorDetails::InternalAgentDied => {
            FunctionCallError::RespondToModel(format!("agent with id {agent_id} is closed"))
        }
        CodexErrorDetails::UnsupportedOperation(_) => {
            FunctionCallError::RespondToModel("collab manager unavailable".to_string())
        }
        _ => FunctionCallError::RespondToModel(format!("collab tool failed: {err}")),
    }
}

pub(crate) fn thread_spawn_source(
    parent_thread_id: ThreadId,
    parent_session_source: &SessionSource,
    depth: i32,
    agent_role: Option<&str>,
    task_name: Option<String>,
    assignment_id: Option<String>,
) -> Result<SessionSource, FunctionCallError> {
    let agent_path = task_name
        .as_deref()
        .map(|task_name| {
            parent_session_source
                .get_agent_path()
                .unwrap_or_else(AgentPath::root)
                .join(task_name)
                .map_err(FunctionCallError::RespondToModel)
        })
        .transpose()?;
    Ok(SessionSource::SubAgent(SubAgentSource::ThreadSpawn {
        parent_thread_id,
        depth,
        agent_path,
        agent_nickname: None,
        agent_role: agent_role.map(str::to_string),
        agent_class: Some(AgentClass::EphemeralTask {
            assignment_id: assignment_id
                .unwrap_or_else(|| format!("thread-spawn:{}", ThreadId::new())),
            retention: RetentionPolicy::Retain,
        }),
    }))
}

pub(crate) fn parse_collab_input(
    message: Option<String>,
    items: Option<Vec<UserInput>>,
) -> Result<Vec<UserInput>, FunctionCallError> {
    match (message, items) {
        (Some(_), Some(_)) => Err(FunctionCallError::RespondToModel(
            "Provide either message or items, but not both".to_string(),
        )),
        (None, None) => Err(FunctionCallError::RespondToModel(
            "Provide one of: message or items".to_string(),
        )),
        (Some(message), None) => {
            if message.trim().is_empty() {
                return Err(FunctionCallError::RespondToModel(
                    "Empty message can't be sent to an agent".to_string(),
                ));
            }
            Ok(vec![UserInput::Text {
                text: message,
                text_elements: Vec::new(),
            }])
        }
        (None, Some(items)) => {
            if items.is_empty() {
                return Err(FunctionCallError::RespondToModel(
                    "Items can't be empty".to_string(),
                ));
            }
            Ok(items)
        }
    }
}

/// Builds the base config snapshot for a newly spawned sub-agent.
///
/// The returned config starts from the parent's effective config and then refreshes the
/// runtime-owned fields carried on `turn`, including model selection, reasoning settings,
/// approval policy, sandbox, and cwd. Role-specific overrides are layered after this step;
/// skipping this helper and cloning stale config state directly can send the child agent out with
/// the wrong provider or runtime policy.
pub(crate) fn build_agent_spawn_config(
    base_instructions: &BaseInstructions,
    turn: &TurnContext,
) -> Result<Config, FunctionCallError> {
    let mut config = build_agent_shared_config(turn)?;
    config.base_instructions = Some(base_instructions.text.clone());
    Ok(config)
}

pub(crate) fn build_agent_resume_config(turn: &TurnContext) -> Result<Config, FunctionCallError> {
    let mut config = build_agent_shared_config(turn)?;
    // For resume, keep base instructions sourced from rollout/session metadata.
    config.base_instructions = None;
    Ok(config)
}

fn build_agent_shared_config(turn: &TurnContext) -> Result<Config, FunctionCallError> {
    let base_config = turn.config.clone();
    let mut config = (*base_config).clone();
    config.model = Some(turn.model_info.slug.clone());
    config.model_provider = turn.provider.info().clone();
    config.model_reasoning_effort = turn
        .reasoning_effort
        .clone()
        .or_else(|| turn.model_info.default_reasoning_level.clone());
    config.model_reasoning_summary = Some(turn.reasoning_summary);
    config.developer_instructions = turn.developer_instructions.clone();
    if turn.multi_agent_version == MultiAgentVersion::V2
        && let Some(developer_instructions) = turn
            .config
            .multi_agent_v2
            .subagent_developer_instructions
            .clone()
    {
        config.developer_instructions = Some(developer_instructions);
    }
    apply_spawn_agent_runtime_overrides(&mut config, turn)?;

    Ok(config)
}

pub(crate) fn reject_full_fork_agent_type_override(
    agent_type: Option<&str>,
) -> Result<(), FunctionCallError> {
    if agent_type.is_some() {
        return Err(FunctionCallError::RespondToModel(
            "Full-history forked agents inherit the parent agent type; omit agent_type, or spawn without a full-history fork.".to_string(),
        ));
    }
    Ok(())
}

/// Copies runtime-only turn state onto a child config before it is handed to `AgentControl`.
///
/// These values are chosen by the live turn rather than persisted config, so leaving them stale
/// can make a child agent disagree with its parent about approval policy, cwd, or sandboxing.
pub(crate) fn apply_spawn_agent_runtime_overrides(
    config: &mut Config,
    turn: &TurnContext,
) -> Result<(), FunctionCallError> {
    // V2's agent paths and mailboxes belong to the running engine, not the selected
    // model. Reapply after role loading too: roles can rebuild the config layers.
    if turn.multi_agent_version == MultiAgentVersion::V2 {
        config
            .features
            .enable(Feature::MultiAgentV2)
            .map_err(|err| {
                FunctionCallError::RespondToModel(format!(
                    "could not preserve the parent agent runtime: {err}"
                ))
            })?;
    }
    config
        .permissions
        .approval_policy
        .set(turn.approval_policy.value())
        .map_err(|err| {
            FunctionCallError::RespondToModel(format!("approval_policy is invalid: {err}"))
        })?;
    config.approvals_reviewer = turn.config.approvals_reviewer;
    #[allow(deprecated)]
    let turn_cwd = turn.cwd.clone();
    config.cwd = turn_cwd;
    config
        .permissions
        .set_permission_profile(turn.permission_profile())
        .map_err(|err| {
            FunctionCallError::RespondToModel(format!("permission_profile is invalid: {err}"))
        })?;
    Ok(())
}

/// Operator spend policy for model-driven agent creation.
///
/// `agents.provider_allowlist` is authorization, not preference. A model selects a
/// runtime; only the operator authorizes one. This is enforced in core because the
/// model-facing spawn path never crosses the TUI, and it is neutral to provider,
/// model, and role name.
pub(crate) fn ensure_spawn_provider_authorized(
    config: &Config,
    provider_id: &str,
) -> Result<(), FunctionCallError> {
    let Some(allowlist) = config.agent_provider_allowlist.as_ref() else {
        return Ok(());
    };
    if allowlist.iter().any(|allowed| allowed == provider_id) {
        return Ok(());
    }
    Err(FunctionCallError::RespondToModel(format!(
        "Provider `{provider_id}` is not authorized for spawned agents. \
Authorized providers: {}. This is operator policy set in `agents.provider_allowlist`; \
it cannot be changed from a task and must not be worked around by selecting a different \
model that routes to an unauthorized provider. Do not spawn a substitute runtime in this \
turn; report the refusal and obtain the user's explicit consent before trying a fallback.",
        allowlist.join(", ")
    )))
}

/// Validate a fully resolved child runtime without inventing an operator policy.
///
/// Catalogue visibility and lifecycle metadata control picker presentation and automatic
/// recommendations. They must not become a hidden spawn blocklist: an exact runtime configured by
/// the operator remains usable even when it is hidden, custom, or represented by another
/// provider's catalogue row. Only an explicit `agents.provider_allowlist` is spend authorization.
pub(crate) async fn ensure_spawn_runtime_eligible(
    session: &Session,
    config: &Config,
) -> Result<(), FunctionCallError> {
    let model = config.model.as_deref().ok_or_else(|| {
        FunctionCallError::RespondToModel(
            "spawn_agent could not resolve the child model".to_string(),
        )
    })?;
    if !config
        .model_providers
        .contains_key(&config.model_provider_id)
    {
        return Err(FunctionCallError::RespondToModel(format!(
            "Runtime provider `{}` is not configured for spawned agents.",
            config.model_provider_id
        )));
    }
    let model_info = crate::model_info_for_provider::model_info_for_config(
        &session.services.models_manager,
        model,
        config,
    )
    .await;
    if model_info
        .orchestration
        .as_ref()
        .is_some_and(|metadata| metadata.provider_id() == config.model_provider_id)
        && let Some(reasoning_effort) = config.model_reasoning_effort.as_ref()
        && !model_info.supported_reasoning_levels.is_empty()
    {
        validate_spawn_agent_reasoning_effort(
            model,
            &model_info.supported_reasoning_levels,
            reasoning_effort,
        )?;
    }
    Ok(())
}

pub(crate) async fn apply_requested_spawn_agent_model_overrides(
    session: &Session,
    turn: &TurnContext,
    config: &mut Config,
    requested_model: Option<&str>,
    requested_reasoning_effort: Option<ReasoningEffort>,
) -> Result<(), FunctionCallError> {
    let requested_model = requested_model.or(turn.config.agent_default_subagent_model.as_deref());
    let requested_reasoning_effort = requested_reasoning_effort
        .or_else(|| turn.config.agent_default_subagent_reasoning_effort.clone());
    if requested_model.is_none() && requested_reasoning_effort.is_none() {
        return Ok(());
    }

    if let Some(requested_model) = requested_model {
        if requested_model == turn.model_info.slug {
            if let Some(reasoning_effort) = requested_reasoning_effort {
                validate_spawn_agent_reasoning_effort(
                    &turn.model_info.slug,
                    &turn.model_info.supported_reasoning_levels,
                    &reasoning_effort,
                )?;
                config.model_reasoning_effort = Some(reasoning_effort);
            }
            return Ok(());
        }
        reject_spawn_agent_model_switch_for_third_party_provider(turn, config, requested_model)?;
        let available_models = session
            .services
            .models_manager
            .list_models(RefreshStrategy::Offline, config.http_client_factory())
            .await;
        let selected_model_name = find_spawn_agent_model_name(&available_models, requested_model)?;

        config.model = Some(selected_model_name.clone());
        // A model switch must not silently keep the parent's provider when that provider cannot
        // serve the selected model — the child's first turn would 400/404 with "Unknown model".
        if let Some(corrected) = codex_model_provider_info::corrected_catalog_provider(
            &selected_model_name,
            &config.model_provider_id,
        ) && let Some(info) = config.model_providers.get(corrected)
        {
            // A model switch can re-route the child onto another provider. That is still
            // a provider selection and must clear the same operator policy as an explicit
            // one. Authorize before mutating, so a refused switch cannot leave the child
            // pointed at an unauthorized provider.
            ensure_spawn_provider_authorized(config, corrected)?;
            tracing::warn!(
                model = %selected_model_name,
                parent_provider = %config.model_provider_id,
                corrected_provider = corrected,
                "correcting inherited provider for spawn_agent model switch"
            );
            let info = info
                .clone()
                .with_account_from(&config.model_provider)
                .map_err(FunctionCallError::RespondToModel)?;
            config.model_provider_id = corrected.to_string();
            config.model_provider = info;
        }
        // The child's provider is final now (#572: Bedrock metadata).
        let selected_model_info = crate::model_info_for_provider::model_info_for_config(
            &session.services.models_manager,
            &selected_model_name,
            config,
        )
        .await;
        if let Some(reasoning_effort) = requested_reasoning_effort {
            validate_spawn_agent_reasoning_effort(
                &selected_model_name,
                &selected_model_info.supported_reasoning_levels,
                &reasoning_effort,
            )?;
            config.model_reasoning_effort = Some(reasoning_effort);
        } else {
            config.model_reasoning_effort = selected_model_info.default_reasoning_level;
        }

        return Ok(());
    }

    if let Some(reasoning_effort) = requested_reasoning_effort {
        validate_spawn_agent_reasoning_effort(
            &turn.model_info.slug,
            &turn.model_info.supported_reasoning_levels,
            &reasoning_effort,
        )?;
        config.model_reasoning_effort = Some(reasoning_effort);
    }

    Ok(())
}

pub(crate) async fn apply_requested_spawn_agent_runtime_overrides(
    session: &Session,
    turn: &TurnContext,
    config: &mut Config,
    requested_provider: Option<&str>,
    requested_model: Option<&str>,
    requested_reasoning_effort: Option<ReasoningEffort>,
) -> Result<(), FunctionCallError> {
    let requested_provider = requested_provider
        .map(str::trim)
        .filter(|provider| !provider.is_empty());
    if requested_provider.is_none() {
        return apply_requested_spawn_agent_model_overrides(
            session,
            turn,
            config,
            requested_model,
            requested_reasoning_effort,
        )
        .await;
    }

    let Some(requested_provider) = requested_provider else {
        return Ok(());
    };
    let requested_model = requested_model
        .map(str::trim)
        .filter(|model| !model.is_empty())
        .ok_or_else(|| {
            FunctionCallError::RespondToModel(
                "spawn_agent requires `model` when `model_provider` is set; set both fields or omit both to inherit the parent runtime.".to_string(),
            )
        })?;
    let provider = config
        .model_providers
        .get(requested_provider)
        .cloned()
        .ok_or_else(|| {
            FunctionCallError::RespondToModel(format!(
                "Unknown model provider `{requested_provider}` for spawn_agent."
            ))
        })?;
    ensure_spawn_provider_authorized(config, requested_provider)?;
    let resolved_model = codex_model_provider_info::resolve_model_for_provider(
        Some(requested_model.to_string()),
        requested_provider,
    );
    if resolved_model.as_deref() != Some(requested_model) {
        return Err(FunctionCallError::RespondToModel(format!(
            "Model `{requested_model}` is not valid for provider `{requested_provider}`."
        )));
    }

    config.model_provider_id = requested_provider.to_string();
    config.model_provider = provider;
    config.model = Some(requested_model.to_string());

    let selected_model_info = crate::model_info_for_provider::model_info_for_config(
        &session.services.models_manager,
        requested_model,
        config,
    )
    .await;
    if let Some(reasoning_effort) = requested_reasoning_effort {
        validate_spawn_agent_reasoning_effort(
            requested_model,
            &selected_model_info.supported_reasoning_levels,
            &reasoning_effort,
        )?;
        config.model_reasoning_effort = Some(reasoning_effort);
    } else {
        config.model_reasoning_effort = selected_model_info.default_reasoning_level;
    }

    Ok(())
}

pub(crate) async fn apply_spawn_agent_service_tier(
    session: &Session,
    config: &mut Config,
    parent_service_tier: Option<&str>,
    requested_service_tier: Option<&str>,
) -> Result<(), FunctionCallError> {
    let candidate_service_tiers = [
        config.service_tier.clone(),
        requested_service_tier.map(str::to_string),
        parent_service_tier.map(str::to_string),
    ];
    if candidate_service_tiers.iter().all(Option::is_none) {
        config.service_tier = None;
        return Ok(());
    }

    let model = config.model.clone().ok_or_else(|| {
        FunctionCallError::RespondToModel(
            "spawn_agent could not resolve the child model for service tier validation".to_string(),
        )
    })?;
    let model_info = crate::model_info_for_provider::model_info_for_config(
        &session.services.models_manager,
        model.as_str(),
        config,
    )
    .await;

    if let Some(requested_service_tier) = requested_service_tier
        && !model_info.supports_service_tier(requested_service_tier)
    {
        let supported_service_tiers = if model_info.service_tiers.is_empty() {
            "none".to_string()
        } else {
            model_info
                .service_tiers
                .iter()
                .map(|tier| tier.id.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        };
        return Err(FunctionCallError::RespondToModel(format!(
            "Service tier `{requested_service_tier}` is not supported for model `{model}`. Supported service tiers: {supported_service_tiers}"
        )));
    }

    config.service_tier =
        candidate_service_tiers
            .into_iter()
            .flatten()
            .find(|candidate_service_tier| {
                model_info.supports_service_tier(candidate_service_tier.as_str())
            });
    Ok(())
}

pub(crate) async fn apply_spawn_agent_role(
    session: &Session,
    config: &mut Config,
    role_name: Option<&str>,
) -> Result<(), FunctionCallError> {
    if session.multi_agent_version() == Some(MultiAgentVersion::V2) {
        apply_role_to_config_for_multi_agent_v2(config, role_name)
            .await
            .map_err(FunctionCallError::RespondToModel)?;
    } else {
        apply_role_to_config(config, role_name)
            .await
            .map_err(FunctionCallError::RespondToModel)?;
    }
    Ok(())
}

fn find_spawn_agent_model_name(
    available_models: &[ModelPreset],
    requested_model: &str,
) -> Result<String, FunctionCallError> {
    available_models
        .iter()
        .find(|model| model.model == requested_model)
        .map(|model| model.model.clone())
        .ok_or_else(|| {
            let available = available_models
                .iter()
                .filter(|model| model.show_in_picker)
                .take(MAX_SPAWN_AGENT_MODEL_OVERRIDES)
                .map(|model| model.model.as_str())
                .collect::<Vec<_>>()
                .join(", ");
            FunctionCallError::RespondToModel(format!(
                "Unknown model `{requested_model}` for spawn_agent. Available models: {available}"
            ))
        })
}

fn reject_spawn_agent_model_switch_for_third_party_provider(
    turn: &TurnContext,
    child_config: &Config,
    requested_model: &str,
) -> Result<(), FunctionCallError> {
    let provider_info = turn.provider.info();
    if !(provider_info.is_ambient()
        || provider_info.is_kimi_code()
        || provider_info.is_zai()
        || provider_info.is_openrouter()
        || provider_info.is_baseten()
        || provider_info.is_vercel())
    {
        return Ok(());
    }
    if let Some(corrected) = codex_model_provider_info::corrected_catalog_provider(
        requested_model,
        &turn.config.model_provider_id,
    ) && child_config.model_providers.contains_key(corrected)
    {
        return Ok(());
    }

    Err(FunctionCallError::RespondToModel(format!(
        "spawn_agent cannot switch from provider `{}` model `{}` to model `{requested_model}`. Subagents inherit the parent provider; omit model to inherit `{}`, or switch the parent session provider/model before spawning.",
        turn.config.model_provider_id, turn.model_info.slug, turn.model_info.slug
    )))
}

fn validate_spawn_agent_reasoning_effort(
    model: &str,
    supported_reasoning_levels: &[ReasoningEffortPreset],
    requested_reasoning_effort: &ReasoningEffort,
) -> Result<(), FunctionCallError> {
    if supported_reasoning_levels
        .iter()
        .any(|preset| &preset.effort == requested_reasoning_effort)
    {
        return Ok(());
    }

    let supported = supported_reasoning_levels
        .iter()
        .map(|preset| preset.effort.to_string())
        .collect::<Vec<_>>()
        .join(", ");
    Err(FunctionCallError::RespondToModel(format!(
        "Reasoning effort `{requested_reasoning_effort}` is not supported for model `{model}`. Supported reasoning efforts: {supported}"
    )))
}

/// #1756 (as #1701 for `wait_agent`): agent tools act only on the caller's
/// own agents, spawned by it or by its agents. A loaded thread the caller
/// did not spawn is refused; an id that is not loaded is left to the tool
/// (it reports it as not found). The registry not being readable refuses.
pub(crate) async fn ensure_own_agent(
    session: &Session,
    tool: &str,
    agent_id: ThreadId,
) -> Result<(), FunctionCallError> {
    if session
        .services
        .agent_control
        .is_foreign_loaded_thread(session.thread_id, agent_id)
        .await
        .unwrap_or(true)
    {
        return Err(FunctionCallError::RespondToModel(format!(
            "{tool} can only target agents spawned by this agent or by its sub-agents; `{agent_id}` is not one of them"
        )));
    }
    Ok(())
}
