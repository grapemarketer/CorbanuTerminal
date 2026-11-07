use super::*;

#[path = "multi_agents_source_identity_route_tests.rs"]
mod source_identity_route_tests;
use crate::StartThreadOptions;
use crate::ThreadManager;
use crate::config::AgentRoleConfig;
use crate::config::DEFAULT_AGENT_MAX_DEPTH;
use crate::function_tool::FunctionCallError;
use crate::init_state_db;
use crate::local_agent_graph_store_from_state_db;
use crate::session::step_context::StepContext;
use crate::session::tests::make_session_and_context;
use crate::session::turn_context::TurnContext;
use crate::session_prefix::format_inter_agent_completion_message;
use crate::thread_manager::thread_store_from_config;
use crate::tools::context::ToolOutput;
use crate::tools::handlers::multi_agents_v2::FollowupTaskHandler as FollowupTaskHandlerV2;
use crate::tools::handlers::multi_agents_v2::InterruptAgentHandler;
use crate::tools::handlers::multi_agents_v2::ListAgentsHandler as ListAgentsHandlerV2;
use crate::tools::handlers::multi_agents_v2::PlaintextFollowupTaskHandler;
use crate::tools::handlers::multi_agents_v2::PlaintextSendMessageHandler;
use crate::tools::handlers::multi_agents_v2::PlaintextSpawnAgentHandler;
use crate::tools::handlers::multi_agents_v2::SendMessageHandler as SendMessageHandlerV2;
use crate::tools::handlers::multi_agents_v2::SpawnAgentHandler as SpawnAgentHandlerV2;
use crate::tools::handlers::multi_agents_v2::WaitAgentHandler as WaitAgentHandlerV2;
use crate::tools::handlers::multi_agents_v2::message_tool::MessageToolResult;
use crate::turn_diff_tracker::TurnDiffTracker;
use codex_extension_api::ExtensionDataInit;
use codex_extension_api::empty_extension_registry;
use codex_features::Feature;
use codex_login::AuthManager;
use codex_login::CodexAuth;
use codex_model_provider::create_model_provider;
use codex_model_provider_info::CLAUDE_FABLE_5_PLAN_MODEL;
use codex_model_provider_info::CLAUDE_PLAN_MODEL;
use codex_model_provider_info::CLAUDE_PLAN_PROVIDER_ID;
use codex_model_provider_info::KIMI_CODE_K3_MODEL;
use codex_model_provider_info::KIMI_CODE_PROVIDER_ID;
use codex_model_provider_info::ModelProviderInfo;
use codex_model_provider_info::OPENAI_PROVIDER_ID;
use codex_model_provider_info::OPENROUTER_PROVIDER_ID;
use codex_model_provider_info::ZAI_PROVIDER_ID;
use codex_model_provider_info::built_in_model_providers;
use codex_protocol::AgentPath;
use codex_protocol::ThreadId;
use codex_protocol::config_types::ApprovalsReviewer;
use codex_protocol::config_types::ServiceTier;
use codex_protocol::config_types::ShellEnvironmentPolicy;
use codex_protocol::models::BaseInstructions;
use codex_protocol::models::ContentItem;
use codex_protocol::models::FunctionCallOutputBody;
use codex_protocol::models::PermissionProfile;
use codex_protocol::models::ResponseInputItem;
use codex_protocol::models::ResponseItem;
use codex_protocol::models::SandboxEnforcement;
use codex_protocol::openai_models::ReasoningEffort;
use codex_protocol::protocol::AgentStatus;
use codex_protocol::protocol::AskForApproval;
use codex_protocol::protocol::EventMsg;
use codex_protocol::protocol::FileSystemAccessMode;
use codex_protocol::protocol::FileSystemPath;
use codex_protocol::protocol::FileSystemSandboxEntry;
use codex_protocol::protocol::FileSystemSandboxPolicy;
use codex_protocol::protocol::InitialHistory;
use codex_protocol::protocol::InterAgentCommunication;
use codex_protocol::protocol::MultiAgentVersion;
use codex_protocol::protocol::NetworkSandboxPolicy;
use codex_protocol::protocol::Op;
use codex_protocol::protocol::RolloutItem;
use codex_protocol::protocol::SandboxPolicy;
use codex_protocol::protocol::SessionSource;
use codex_protocol::protocol::SubAgentSource;
use codex_protocol::protocol::ThreadSource;
use codex_protocol::protocol::TurnAbortReason;
use codex_protocol::protocol::TurnAbortedEvent;
use codex_protocol::protocol::TurnCompleteEvent;
use codex_protocol::user_input::UserInput;
use codex_state::DirectionalThreadSpawnEdgeStatus;
use core_test_support::TempDirExt;
use pretty_assertions::assert_eq;
use serde::Deserialize;
use serde_json::json;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::Mutex;
use tokio::time::timeout;
use tokio_util::sync::CancellationToken;

fn model_facing_error_message(error: &FunctionCallError) -> Option<&str> {
    match error {
        FunctionCallError::RespondToModel(message)
        | FunctionCallError::MalformedToolCall { message, .. } => Some(message),
        FunctionCallError::Fatal(_) => None,
    }
}

fn invocation(
    session: Arc<crate::session::session::Session>,
    turn: Arc<TurnContext>,
    tool_name: &str,
    payload: ToolPayload,
) -> ToolInvocation {
    let step_context = StepContext::for_test(Arc::clone(&turn));
    ToolInvocation {
        session,
        step_context,
        turn,
        cancellation_token: CancellationToken::new(),
        tracker: Arc::new(Mutex::new(TurnDiffTracker::default())),
        call_id: "call-1".to_string(),
        tool_name: codex_tools::ToolName::plain(tool_name),
        source: crate::tools::context::ToolCallSource::Direct,
        payload,
    }
}

fn function_payload(args: serde_json::Value) -> ToolPayload {
    ToolPayload::Function {
        arguments: args.to_string(),
    }
}

fn assert_wait_v2_result(
    result: &crate::tools::handlers::multi_agents_v2::wait::WaitAgentResult,
    message: &str,
    timed_out: bool,
) {
    assert_eq!(result.message, message);
    assert_eq!(result.timed_out, timed_out);
}

fn assert_wait_v2_agents_include_task(
    result: &crate::tools::handlers::multi_agents_v2::wait::WaitAgentResult,
    task_name: &str,
) {
    assert!(
        result
            .agents
            .iter()
            .any(|agent| agent.agent_name.ends_with(task_name)
                || agent
                    .last_task_message
                    .as_deref()
                    .is_some_and(|message| message.contains(task_name))),
        "wait_agent result should include task {task_name:?}"
    );
}

fn parse_agent_id(id: &str) -> ThreadId {
    ThreadId::from_string(id).expect("agent id should be valid")
}

fn assert_nickname_from_role_pool(
    config: &crate::config::Config,
    nickname: Option<&str>,
    role_name: &str,
) {
    let nickname = nickname.expect("spawned agent should have a nickname");
    let candidates = crate::agent::role::agent_nickname_candidates(config, Some(role_name));
    assert!(
        candidates.iter().any(|candidate| candidate == nickname),
        "nickname {nickname:?} should come from the {role_name:?} candidate pool {candidates:?}"
    );
}

fn thread_manager() -> ThreadManager {
    ThreadManager::with_models_provider_for_tests(
        CodexAuth::from_api_key("dummy"),
        built_in_model_providers(/* openai_base_url */ /*openai_base_url*/ None)["openai"].clone(),
    )
}

async fn install_live_root(
    session: &mut crate::session::session::Session,
    turn: &TurnContext,
) -> ThreadManager {
    let manager = thread_manager();
    let root = manager
        .start_thread((*turn.config).clone())
        .await
        .expect("root thread should start");
    session.services.agent_control = manager.agent_control();
    session.thread_id = root.thread_id;
    manager
}

fn set_turn_to_openai_provider(turn: &mut TurnContext) {
    let provider_info =
        built_in_model_providers(/* openai_base_url */ /*openai_base_url*/ None)
            [OPENAI_PROVIDER_ID]
            .clone();
    let mut config = (*turn.config).clone();
    config.model_provider_id = OPENAI_PROVIDER_ID.to_string();
    config.model_provider = provider_info.clone();
    turn.provider = create_model_provider(provider_info, turn.auth_manager.clone());
    turn.config = Arc::new(config);
}

async fn with_spawn_eligible_runtime(
    session: &crate::session::session::Session,
    turn: TurnContext,
) -> TurnContext {
    let mut turn = turn
        .with_model(
            "gpt-5.6-terra".to_string(),
            &session.services.models_manager,
        )
        .await;
    set_turn_to_openai_provider(&mut turn);
    turn
}

async fn install_role_with_model_override(turn: &mut TurnContext) -> String {
    let role_name = "fork-context-role".to_string();
    tokio::fs::create_dir_all(&turn.config.codex_home)
        .await
        .expect("codex home should be created");
    let role_config_path = turn
        .config
        .codex_home
        .as_path()
        .join("fork-context-role.toml");
    tokio::fs::write(
        &role_config_path,
        r#"model = "gpt-5.6-sol"
model_provider = "openai"
model_reasoning_effort = "low"
"#,
    )
    .await
    .expect("role config should be written");

    let mut config = (*turn.config).clone();
    config.agent_roles.insert(
        role_name.clone(),
        AgentRoleConfig {
            description: Some("Role with model overrides".to_string()),
            config_file: Some(role_config_path),
            nickname_candidates: None,
        },
    );
    turn.config = Arc::new(config);

    role_name
}

fn set_turn_config(turn: &mut TurnContext, config: crate::config::Config) {
    turn.multi_agent_version = config.multi_agent_version_from_features();
    turn.config = Arc::new(config);
}

#[tokio::test]
async fn spawn_model_switch_cannot_route_around_the_provider_allowlist() {
    let (session, mut turn) = make_session_and_context().await;
    set_turn_to_openai_provider(&mut turn);

    // A bare `model` with no `model_provider` still selects a provider: the catalog
    // correction re-routes the child onto whichever provider owns that model. Whatever
    // the outcome, the child must never end up on a provider the operator did not
    // authorize.
    let allowlist = vec!["claude-plan".to_string(), "openai".to_string()];
    for model in [
        "claude-fable-5",
        "claude-opus-5",
        "x-ai/grok-4.5",
        "k3",
        "glm-5.3",
    ] {
        let mut config = (*turn.config).clone();
        config.agent_provider_allowlist = Some(allowlist.clone());
        let parent_provider = config.model_provider_id.clone();

        let result = apply_requested_spawn_agent_model_overrides(
            &session,
            &turn,
            &mut config,
            Some(model),
            /*requested_reasoning_effort*/ None,
        )
        .await;

        match result {
            Ok(()) => assert!(
                allowlist.contains(&config.model_provider_id),
                "model `{model}` left the child on unauthorized provider `{}`",
                config.model_provider_id
            ),
            Err(FunctionCallError::RespondToModel(message)) => assert_eq!(
                config.model_provider_id, parent_provider,
                "refused switch for `{model}` must not mutate the provider: {message}"
            ),
            Err(other) => panic!("unexpected error kind for `{model}`: {other:?}"),
        }
    }
}

#[tokio::test]
async fn spawn_provider_authorization_is_operator_policy_in_core() {
    use crate::tools::handlers::multi_agents_common::ensure_spawn_provider_authorized;

    let (_session, turn) = make_session_and_context().await;
    let mut config = (*turn.config).clone();
    config.agent_provider_allowlist = None;
    // Unset stays unrestricted.
    for provider in ["anthropic", "openrouter", "kimi-code"] {
        ensure_spawn_provider_authorized(&config, provider)
            .unwrap_or_else(|err| panic!("{provider} should be unrestricted: {err:?}"));
    }

    config.agent_provider_allowlist = Some(vec!["claude-plan".to_string(), "openai".to_string()]);
    for provider in ["claude-plan", "openai"] {
        ensure_spawn_provider_authorized(&config, provider)
            .unwrap_or_else(|err| panic!("{provider} should be authorized: {err:?}"));
    }
    // The whole unauthorized class is refused, not one reported example.
    for provider in ["anthropic", "openrouter", "kimi-code", "zai", "vercel"] {
        let err = ensure_spawn_provider_authorized(&config, provider)
            .expect_err("unauthorized provider must be refused before child creation");
        let FunctionCallError::RespondToModel(message) = err else {
            panic!("expected a model-visible refusal for {provider}");
        };
        assert!(
            message.contains("agents.provider_allowlist") && message.contains(provider),
            "refusal for {provider} should name the provider and the policy: {message}"
        );
    }
}

#[tokio::test]
async fn spawn_runtime_validation_does_not_invent_a_catalogue_blocklist() {
    use crate::tools::handlers::multi_agents_common::ensure_spawn_runtime_eligible;

    let (session, turn) = make_session_and_context().await;
    let mut config = (*turn.config).clone();

    config.model = Some("gpt-5.5".to_string());
    config.model_provider_id = OPENAI_PROVIDER_ID.to_string();
    ensure_spawn_runtime_eligible(&session, &config)
        .await
        .expect("hidden catalogue models remain usable when explicitly configured");

    config.model = Some("unlisted-model-without-policy".to_string());
    ensure_spawn_runtime_eligible(&session, &config)
        .await
        .expect("custom configured models do not require a bundled catalogue row");

    config.model = Some("gpt-5.6-sol".to_string());
    config.model_provider_id = KIMI_CODE_PROVIDER_ID.to_string();
    ensure_spawn_runtime_eligible(&session, &config)
        .await
        .expect("a configured provider is not overridden by another provider's catalogue row");

    config.model = None;
    let missing_model = ensure_spawn_runtime_eligible(&session, &config)
        .await
        .expect_err("a child still requires a resolved model");
    assert!(
        matches!(
            &missing_model,
            FunctionCallError::RespondToModel(message) if message.contains("resolve the child model")
        ),
        "unexpected missing-model error: {missing_model:?}"
    );

    config.model = Some("custom-model".to_string());
    config.model_provider_id = "not-configured".to_string();
    let missing_provider = ensure_spawn_runtime_eligible(&session, &config)
        .await
        .expect_err("an unknown provider must not reach child creation");
    assert!(matches!(
        &missing_provider,
        FunctionCallError::RespondToModel(message) if message.contains("is not configured")
    ));
}

#[tokio::test]
async fn multi_agent_v2_inherited_runtime_must_clear_provider_policy() {
    let (mut session, mut turn) = make_session_and_context().await;
    let provider_info =
        built_in_model_providers(/* openai_base_url */ /*openai_base_url*/ None)
            [KIMI_CODE_PROVIDER_ID]
            .clone();
    let mut config = (*turn.config).clone();
    config.model_provider_id = KIMI_CODE_PROVIDER_ID.to_string();
    config.model_provider = provider_info.clone();
    config.agent_provider_allowlist = Some(vec![OPENAI_PROVIDER_ID.to_string()]);
    config
        .features
        .enable(Feature::MultiAgentV2)
        .expect("test config should allow feature update");
    turn.provider = create_model_provider(provider_info, turn.auth_manager.clone());
    set_turn_config(&mut turn, config);

    let manager = thread_manager();
    let root = manager
        .start_thread((*turn.config).clone())
        .await
        .expect("root thread should start");
    session.services.agent_control = manager.agent_control();
    session.thread_id = root.thread_id;

    let result = SpawnAgentHandlerV2::default()
        .handle(invocation(
            Arc::new(session),
            Arc::new(turn),
            "spawn_agent",
            function_payload(json!({
                "message": "review this repository",
                "task_name": "reviewer",
                "fork_turns": "none"
            })),
        ))
        .await;
    let Err(err) = result else {
        panic!("inherited providers must not bypass the allowlist");
    };

    assert_eq!(
        err,
        FunctionCallError::RespondToModel(
            "Provider `kimi-code` is not authorized for spawned agents. Authorized providers: openai. This is operator policy set in `agents.provider_allowlist`; it cannot be changed from a task and must not be worked around by selecting a different model that routes to an unauthorized provider. Do not spawn a substitute runtime in this turn; report the refusal and obtain the user's explicit consent before trying a fallback.".to_string()
        )
    );
}

async fn register_v2_wait_child(
    session: &mut crate::session::session::Session,
    turn: &TurnContext,
) -> ThreadManager {
    let manager = thread_manager();
    let root = manager
        .start_thread((*turn.config).clone())
        .await
        .expect("root thread should start");
    session.services.agent_control = manager.agent_control();
    session.thread_id = root.thread_id;
    let child = session
        .services
        .agent_control
        .spawn_agent_with_metadata(
            (*turn.config).clone(),
            vec![UserInput::Text {
                text: "wait-test child".to_string(),
                text_elements: Vec::new(),
            }],
            Some(SessionSource::SubAgent(SubAgentSource::ThreadSpawn {
                parent_thread_id: root.thread_id,
                depth: 1,
                agent_path: Some(AgentPath::try_from("/root/worker").expect("agent path")),
                agent_nickname: Some("Worker".to_string()),
                agent_role: Some("worker".to_string()),
                agent_class: None,
            })),
            crate::agent::control::SpawnAgentOptions::default(),
        )
        .await
        .expect("wait-test child should spawn");
    let mut status_rx = session
        .services
        .agent_control
        .subscribe_status(child.thread_id)
        .await
        .expect("wait-test child status subscription");
    loop {
        if matches!(
            &*status_rx.borrow(),
            AgentStatus::Completed(_)
                | AgentStatus::Errored(_)
                | AgentStatus::Interrupted
                | AgentStatus::Shutdown
        ) {
            break;
        }
        status_rx
            .changed()
            .await
            .expect("wait-test child should reach a terminal setup state");
    }
    manager
}

fn expect_text_output<T>(output: T) -> (String, Option<bool>)
where
    T: ToolOutput,
{
    let response = output.to_response_item(
        "call-1",
        &ToolPayload::Function {
            arguments: "{}".to_string(),
        },
    );
    match response {
        ResponseInputItem::FunctionCallOutput { output, .. }
        | ResponseInputItem::CustomToolCallOutput { output, .. } => {
            let content = match output.body {
                FunctionCallOutputBody::Text(text) => text,
                FunctionCallOutputBody::ContentItems(items) => {
                    codex_protocol::models::function_call_output_content_items_to_text(&items)
                        .unwrap_or_default()
                }
            };
            (content, output.success)
        }
        other => panic!("expected function output, got {other:?}"),
    }
}

#[derive(Debug, Deserialize)]
struct ListAgentsResult {
    agents: Vec<ListedAgentResult>,
}

#[derive(Debug, Deserialize)]
struct ListedAgentResult {
    agent_name: String,
    agent_nickname: Option<String>,
    agent_role: Option<String>,
    agent_status: serde_json::Value,
    last_task_message: Option<String>,
    last_result_message: Option<String>,
}

#[derive(Debug, Deserialize)]
struct InterruptAgentResult {
    previous_status: AgentStatus,
}

#[tokio::test]
async fn handler_rejects_non_function_payloads() {
    let (session, turn) = make_session_and_context().await;
    let invocation = invocation(
        Arc::new(session),
        Arc::new(turn),
        "spawn_agent",
        ToolPayload::Custom {
            input: "hello".to_string(),
        },
    );
    let Err(err) = SpawnAgentHandler::default().handle(invocation).await else {
        panic!("payload should be rejected");
    };
    assert_eq!(
        err,
        FunctionCallError::RespondToModel(
            "collab handler received unsupported payload".to_string()
        )
    );
}

#[tokio::test]
async fn spawn_agent_rejects_empty_message() {
    let (session, turn) = make_session_and_context().await;
    let invocation = invocation(
        Arc::new(session),
        Arc::new(turn),
        "spawn_agent",
        function_payload(json!({"message": "   "})),
    );
    let Err(err) = SpawnAgentHandler::default().handle(invocation).await else {
        panic!("empty message should be rejected");
    };
    assert_eq!(
        err,
        FunctionCallError::RespondToModel("Empty message can't be sent to an agent".to_string())
    );
}

#[tokio::test]
async fn spawn_agent_rejects_when_message_and_items_are_both_set() {
    let (session, turn) = make_session_and_context().await;
    let invocation = invocation(
        Arc::new(session),
        Arc::new(turn),
        "spawn_agent",
        function_payload(json!({
            "message": "hello",
            "items": [{"type": "mention", "name": "drive", "path": "app://drive"}]
        })),
    );
    let Err(err) = SpawnAgentHandler::default().handle(invocation).await else {
        panic!("message+items should be rejected");
    };
    assert_eq!(
        err,
        FunctionCallError::RespondToModel(
            "Provide either message or items, but not both".to_string()
        )
    );
}

#[tokio::test]
async fn spawn_agent_uses_explorer_role_and_preserves_approval_policy() {
    #[derive(Debug, Deserialize)]
    struct SpawnAgentResult {
        agent_id: String,
        nickname: Option<String>,
    }

    let (mut session, turn) = make_session_and_context().await;
    let mut turn = with_spawn_eligible_runtime(&session, turn).await;
    let mut config = (*turn.config).clone();
    config
        .permissions
        .approval_policy
        .set(AskForApproval::OnRequest)
        .expect("approval policy should be set");
    turn.approval_policy
        .set(AskForApproval::OnRequest)
        .expect("approval policy should be set");
    turn.config = Arc::new(config);
    let manager = install_live_root(&mut session, &turn).await;

    let invocation = invocation(
        Arc::new(session),
        Arc::new(turn),
        "spawn_agent",
        function_payload(json!({
            "message": "inspect this repo",
            "agent_type": "explorer"
        })),
    );
    let output = SpawnAgentHandler::default()
        .handle(invocation)
        .await
        .expect("spawn_agent should succeed");
    let (content, _) = expect_text_output(output);
    let result: SpawnAgentResult =
        serde_json::from_str(&content).expect("spawn_agent result should be json");
    let agent_id = parse_agent_id(&result.agent_id);
    assert!(
        result
            .nickname
            .as_deref()
            .is_some_and(|nickname| !nickname.is_empty())
    );
    let snapshot = manager
        .get_thread(agent_id)
        .await
        .expect("spawned agent thread should exist")
        .config_snapshot()
        .await;
    assert_eq!(snapshot.approval_policy, AskForApproval::OnRequest);
    assert_eq!(snapshot.model_provider_id, "openai");
}

#[tokio::test]
async fn spawn_agent_fork_context_rejects_agent_type_override() {
    let (mut session, mut turn) = make_session_and_context().await;
    let role_name = install_role_with_model_override(&mut turn).await;
    let manager = thread_manager();
    let root = manager
        .start_thread(StartThreadOptions::new((*turn.config).clone()))
        .await
        .expect("root thread should start");
    session.services.agent_control = manager.agent_control();
    session.thread_id = root.thread_id;
    let err = SpawnAgentHandler::default()
        .handle(invocation(
            Arc::new(session),
            Arc::new(turn),
            "spawn_agent",
            function_payload(json!({
                "message": "inspect this repo",
                "agent_type": role_name,
                "fork_context": true
            })),
        ))
        .await
        .err()
        .expect("fork_context should reject agent_type overrides");

    assert_eq!(
        err,
        FunctionCallError::RespondToModel(
            "Full-history forked agents inherit the parent agent type; omit agent_type, or spawn without a full-history fork.".to_string(),
        )
    );
}

#[tokio::test]
async fn spawn_agent_rejects_cross_provider_model_override_for_zai() {
    let (session, mut turn) = make_session_and_context().await;
    let provider_info =
        built_in_model_providers(/* openai_base_url */ /*openai_base_url*/ None)[ZAI_PROVIDER_ID]
            .clone();
    let mut config = (*turn.config).clone();
    config.model = Some("glm-5.2".to_string());
    config.model_provider_id = ZAI_PROVIDER_ID.to_string();
    config.model_provider = provider_info.clone();
    turn.model_info.slug = "glm-5.2".to_string();
    turn.provider = create_model_provider(provider_info, turn.auth_manager.clone());
    turn.config = Arc::new(config);

    let err = SpawnAgentHandler::default()
        .handle(invocation(
            Arc::new(session),
            Arc::new(turn),
            "spawn_agent",
            function_payload(json!({
                "message": "inspect this repo",
                "model": "z-ai/glm-5.2"
            })),
        ))
        .await
        .err()
        .expect("Z.AI spawn should reject cross-provider model overrides");

    match err {
        FunctionCallError::RespondToModel(message) => {
            assert!(message.contains("provider `zai` model `glm-5.2`"));
            assert!(message.contains("model `z-ai/glm-5.2`"));
            assert!(message.contains("Subagents inherit the parent provider"));
        }
        other => panic!("expected RespondToModel, got {other:?}"),
    }
}

#[tokio::test]
async fn spawn_agent_explicit_runtime_supports_required_multimodel_pairs() {
    let (session, turn) = make_session_and_context().await;
    let providers = built_in_model_providers(/*openai_base_url*/ None);
    let expected = [
        (CLAUDE_PLAN_PROVIDER_ID, CLAUDE_PLAN_MODEL),
        (CLAUDE_PLAN_PROVIDER_ID, CLAUDE_FABLE_5_PLAN_MODEL),
        (OPENROUTER_PROVIDER_ID, "x-ai/grok-4.5"),
        (KIMI_CODE_PROVIDER_ID, KIMI_CODE_K3_MODEL),
        (ZAI_PROVIDER_ID, "glm-5.3"),
    ];

    for (provider, model) in expected {
        let mut child_config = (*turn.config).clone();
        apply_requested_spawn_agent_runtime_overrides(
            &session,
            &turn,
            &mut child_config,
            Some(provider),
            Some(model),
            /*requested_reasoning_effort*/ None,
        )
        .await
        .unwrap_or_else(|err| panic!("runtime {provider}/{model} should resolve: {err:?}"));

        assert_eq!(child_config.model_provider_id, provider);
        assert_eq!(
            &child_config.model_provider,
            providers
                .get(provider)
                .unwrap_or_else(|| panic!("provider {provider} should be built in"))
        );
        assert_eq!(child_config.model.as_deref(), Some(model));
    }
}

#[tokio::test]
async fn spawn_agent_explicit_runtime_rejects_incomplete_or_invalid_pairs() {
    let (session, turn) = make_session_and_context().await;
    let mut child_config = (*turn.config).clone();

    let missing_model = apply_requested_spawn_agent_runtime_overrides(
        &session,
        &turn,
        &mut child_config,
        Some(KIMI_CODE_PROVIDER_ID),
        /*requested_model*/ None,
        /*requested_reasoning_effort*/ None,
    )
    .await
    .expect_err("explicit provider without model should fail");
    assert_eq!(
        missing_model,
        FunctionCallError::RespondToModel(
            "spawn_agent requires `model` when `model_provider` is set; set both fields or omit both to inherit the parent runtime.".to_string()
        )
    );

    let invalid_pair = apply_requested_spawn_agent_runtime_overrides(
        &session,
        &turn,
        &mut child_config,
        Some(KIMI_CODE_PROVIDER_ID),
        Some("x-ai/grok-4.5"),
        /*requested_reasoning_effort*/ None,
    )
    .await
    .expect_err("provider/model mismatch should fail");
    assert_eq!(
        invalid_pair,
        FunctionCallError::RespondToModel(
            "Model `x-ai/grok-4.5` is not valid for provider `kimi-code`.".to_string()
        )
    );
}

#[tokio::test]
async fn spawn_agent_accepts_exact_configured_model_even_when_catalog_hidden() {
    let (mut session, mut turn) = make_session_and_context().await;
    let provider_info =
        built_in_model_providers(/* openai_base_url */ /*openai_base_url*/ None)[ZAI_PROVIDER_ID]
            .clone();
    let mut config = (*turn.config).clone();
    config.model = Some("glm-5.2".to_string());
    config.model_provider_id = ZAI_PROVIDER_ID.to_string();
    config.model_provider = provider_info.clone();
    turn.model_info.slug = "glm-5.2".to_string();
    turn.provider = create_model_provider(provider_info, turn.auth_manager.clone());
    turn.config = Arc::new(config);

    let manager = thread_manager();
    let root = manager
        .start_thread((*turn.config).clone())
        .await
        .expect("root thread should start");
    session.services.agent_control = manager.agent_control();
    session.thread_id = root.thread_id;

    let output = SpawnAgentHandler::default()
        .handle(invocation(
            Arc::new(session),
            Arc::new(turn),
            "spawn_agent",
            function_payload(json!({
                "message": "inspect this repo",
                "model": "gpt-5.5"
            })),
        ))
        .await
        .expect("an exact configured runtime must not be rejected by hidden catalogue policy");
    let (content, success) = expect_text_output(output);
    let result: serde_json::Value =
        serde_json::from_str(&content).expect("spawn_agent result should be json");
    let child_id = parse_agent_id(
        result["agent_id"]
            .as_str()
            .expect("spawn_agent should return the child id"),
    );
    let snapshot = manager
        .get_thread(child_id)
        .await
        .expect("spawned child should exist")
        .config_snapshot()
        .await;
    assert_eq!(snapshot.model_provider_id, OPENAI_PROVIDER_ID);
    assert_eq!(snapshot.model, "gpt-5.5");
    assert_eq!(success, Some(true));
}

#[tokio::test]
async fn spawn_agent_rejects_unmapped_model_override_for_zai() {
    let (session, mut turn) = make_session_and_context().await;
    let provider_info =
        built_in_model_providers(/* openai_base_url */ /*openai_base_url*/ None)[ZAI_PROVIDER_ID]
            .clone();
    let mut config = (*turn.config).clone();
    config.model = Some("glm-5.2".to_string());
    config.model_provider_id = ZAI_PROVIDER_ID.to_string();
    config.model_provider = provider_info.clone();
    turn.model_info.slug = "glm-5.2".to_string();
    turn.provider = create_model_provider(provider_info, turn.auth_manager.clone());
    turn.config = Arc::new(config);

    let err = SpawnAgentHandler::default()
        .handle(invocation(
            Arc::new(session),
            Arc::new(turn),
            "spawn_agent",
            function_payload(json!({
                "message": "inspect this repo",
                "model": "some-unknown-model"
            })),
        ))
        .await
        .err()
        .expect("Z.AI spawn should reject unmapped model overrides");

    match err {
        FunctionCallError::RespondToModel(message) => {
            assert!(message.contains("provider `zai` model `glm-5.2`"));
            assert!(message.contains("model `some-unknown-model`"));
            assert!(message.contains("Subagents inherit the parent provider"));
        }
        other => panic!("expected RespondToModel, got {other:?}"),
    }
}

#[tokio::test]
async fn multi_agent_v2_spawn_fork_turns_all_rejects_agent_type_override() {
    let (mut session, mut turn) = make_session_and_context().await;
    let role_name = install_role_with_model_override(&mut turn).await;
    let manager = thread_manager();
    let root = manager
        .start_thread(StartThreadOptions::new((*turn.config).clone()))
        .await
        .expect("root thread should start");
    session.services.agent_control = manager.agent_control();
    session.thread_id = root.thread_id;
    let mut config = (*turn.config).clone();
    config
        .features
        .enable(Feature::MultiAgentV2)
        .expect("test config should allow feature update");
    let turn = TurnContext {
        config: Arc::new(config),
        multi_agent_version: codex_protocol::protocol::MultiAgentVersion::V2,
        ..turn
    };

    let err = SpawnAgentHandlerV2::default()
        .handle(invocation(
            Arc::new(session),
            Arc::new(turn),
            "spawn_agent",
            function_payload(json!({
                "message": "inspect this repo",
                "task_name": "fork_context_v2",
                "agent_type": role_name,
                "fork_turns": "all"
            })),
        ))
        .await
        .err()
        .expect("fork_turns=all should reject agent_type overrides");

    assert_eq!(
        err,
        FunctionCallError::RespondToModel(
            "Full-history forked agents inherit the parent agent type; omit agent_type, or spawn without a full-history fork.".to_string(),
        )
    );
}

#[tokio::test]
async fn multi_agent_v2_selects_child_models_with_different_engine_defaults() {
    let (session, mut turn) = make_session_and_context().await;
    set_turn_to_openai_provider(&mut turn);
    let mut config = (*turn.config).clone();
    config
        .features
        .enable(Feature::MultiAgentV2)
        .expect("test config should allow feature update");
    set_turn_config(&mut turn, config);

    for (model, provider) in [
        ("gpt-5.4", OPENAI_PROVIDER_ID),
        ("k3", KIMI_CODE_PROVIDER_ID),
    ] {
        let mut child =
            build_agent_spawn_config(&BaseInstructions::default(), &turn).expect("child config");
        apply_requested_spawn_agent_model_overrides(
            &session,
            &turn,
            &mut child,
            Some(model),
            /*requested_reasoning_effort*/ None,
        )
        .await
        .expect("model selection is independent of the model's engine default");
        assert_eq!(
            (
                child.model.as_deref(),
                child.model_provider_id.as_str(),
                child.multi_agent_version_for_model(/*model_multi_agent_version*/ None)
            ),
            (Some(model), provider, MultiAgentVersion::V2)
        );
    }
}

#[tokio::test]
async fn build_agent_config_inherits_model_selected_v2_for_spawn_and_resume() {
    let (_session, mut turn) = make_session_and_context().await;
    let mut parent = (*turn.config).clone();
    parent
        .features
        .disable(Feature::MultiAgentV2)
        .expect("feature update");
    turn.config = Arc::new(parent);
    // The parent's model, not an explicit feature flag, selected the running engine.
    turn.multi_agent_version = MultiAgentVersion::V2;
    let spawn =
        build_agent_spawn_config(&BaseInstructions::default(), &turn).expect("spawn config");
    let resume = build_agent_resume_config(&turn).expect("resume config");
    for preference in [
        None,
        Some(MultiAgentVersion::V1),
        Some(MultiAgentVersion::V2),
    ] {
        assert_eq!(
            (
                spawn.multi_agent_version_for_model(preference),
                resume.multi_agent_version_for_model(preference)
            ),
            (MultiAgentVersion::V2, MultiAgentVersion::V2)
        );
    }
    assert!(!turn.config.features.enabled(Feature::MultiAgentV2));
}

#[tokio::test]
async fn spawn_agent_service_tier_override_validates_the_effective_child_model() {
    #[derive(Debug, Deserialize)]
    struct SpawnAgentResult {
        agent_id: String,
    }

    {
        let (mut session, mut turn) = make_session_and_context().await;
        set_turn_to_openai_provider(&mut turn);
        let manager = thread_manager();
        let root = manager
            .start_thread(StartThreadOptions::new((*turn.config).clone()))
            .await
            .expect("root thread should start");
        session.services.agent_control = manager.agent_control();
        session.thread_id = root.thread_id;

        let output = SpawnAgentHandler::default()
            .handle(invocation(
                Arc::new(session),
                Arc::new(turn),
                "spawn_agent",
                function_payload(json!({
                    "message": "inspect this repo",
                    "model": "gpt-5.6-sol",
                    "service_tier": ServiceTier::Fast.request_value()
                })),
            ))
            .await
            .expect("spawn_agent should accept a supported explicit service tier");
        let (content, _) = expect_text_output(output);
        let result: SpawnAgentResult =
            serde_json::from_str(&content).expect("spawn_agent result should be json");
        let snapshot = manager
            .get_thread(parse_agent_id(&result.agent_id))
            .await
            .expect("spawned agent thread should exist")
            .config_snapshot()
            .await;

        assert_eq!(
            snapshot.service_tier,
            Some(ServiceTier::Fast.request_value().to_string())
        );
    }

    {
        let (session, mut turn) = make_session_and_context().await;
        set_turn_to_openai_provider(&mut turn);
        let err = SpawnAgentHandler::default()
            .handle(invocation(
                Arc::new(session),
                Arc::new(turn),
                "spawn_agent",
                function_payload(json!({
                    "message": "inspect this repo",
                    "model": "gpt-5.6-sol",
                    "service_tier": "turbo"
                })),
            ))
            .await
            .err()
            .expect("unknown service tier should be rejected");

        assert_eq!(
            err,
            FunctionCallError::RespondToModel(
                "Service tier `turbo` is not supported for model `gpt-5.6-sol`. Supported service tiers: priority"
                    .to_string()
            )
        );
    }

    {
        let (session, mut turn) = make_session_and_context().await;
        set_turn_to_openai_provider(&mut turn);
        let err = SpawnAgentHandler::default()
            .handle(invocation(
                Arc::new(session),
                Arc::new(turn),
                "spawn_agent",
                function_payload(json!({
                    "message": "inspect this repo",
                    "model": "gpt-5.4-mini",
                    "service_tier": ServiceTier::Fast.request_value()
                })),
            ))
            .await
            .err()
            .expect("tier unsupported by the final child model should be rejected");

        assert_eq!(
            err,
            FunctionCallError::RespondToModel(
                "Service tier `priority` is not supported for model `gpt-5.4-mini`. Supported service tiers: none"
                    .to_string()
            )
        );
    }
}

#[tokio::test]
async fn spawn_agent_service_tier_inheritance_preserves_supported_or_configured_tiers() {
    #[derive(Debug, Deserialize)]
    struct SpawnAgentResult {
        agent_id: String,
    }

    {
        let (mut session, turn) = make_session_and_context().await;
        let mut turn = turn
            .with_model("gpt-5.6-sol".to_string(), &session.services.models_manager)
            .await;
        set_turn_to_openai_provider(&mut turn);
        let mut config = (*turn.config).clone();
        config.service_tier = Some(ServiceTier::Fast.request_value().to_string());
        turn.config = Arc::new(config);
        let manager = thread_manager();
        let root = manager
            .start_thread(StartThreadOptions::new((*turn.config).clone()))
            .await
            .expect("root thread should start");
        session.services.agent_control = manager.agent_control();
        session.thread_id = root.thread_id;

        let output = SpawnAgentHandler::default()
            .handle(invocation(
                Arc::new(session),
                Arc::new(turn),
                "spawn_agent",
                function_payload(json!({"message": "inspect this repo"})),
            ))
            .await
            .expect("spawn_agent should inherit a supported parent service tier");
        let (content, _) = expect_text_output(output);
        let result: SpawnAgentResult =
            serde_json::from_str(&content).expect("spawn_agent result should be json");
        let snapshot = manager
            .get_thread(parse_agent_id(&result.agent_id))
            .await
            .expect("spawned agent thread should exist")
            .config_snapshot()
            .await;

        assert_eq!(
            snapshot.service_tier,
            Some(ServiceTier::Fast.request_value().to_string())
        );
    }

    {
        let (mut session, turn) = make_session_and_context().await;
        let mut turn = turn
            .with_model("gpt-5.6-sol".to_string(), &session.services.models_manager)
            .await;
        set_turn_to_openai_provider(&mut turn);
        let mut config = (*turn.config).clone();
        config.service_tier = Some(ServiceTier::Fast.request_value().to_string());
        turn.config = Arc::new(config);
        let manager = thread_manager();
        let root = manager
            .start_thread(StartThreadOptions::new((*turn.config).clone()))
            .await
            .expect("root thread should start");
        session.services.agent_control = manager.agent_control();
        session.thread_id = root.thread_id;

        let output = SpawnAgentHandler::default()
            .handle(invocation(
                Arc::new(session),
                Arc::new(turn),
                "spawn_agent",
                function_payload(json!({
                    "message": "inspect this repo",
                    "model": "gpt-5.4-mini"
                })),
            ))
            .await
            .expect("spawn_agent should clear unsupported inherited service tier");
        let (content, _) = expect_text_output(output);
        let result: SpawnAgentResult =
            serde_json::from_str(&content).expect("spawn_agent result should be json");
        let snapshot = manager
            .get_thread(parse_agent_id(&result.agent_id))
            .await
            .expect("spawned agent thread should exist")
            .config_snapshot()
            .await;

        assert_eq!(snapshot.service_tier, None);
    }

    {
        let (mut session, mut turn) = make_session_and_context().await;
        set_turn_to_openai_provider(&mut turn);
        tokio::fs::create_dir_all(&turn.config.codex_home)
            .await
            .expect("codex home should be created");
        let role_config_path = turn
            .config
            .codex_home
            .as_path()
            .join("service-tier-role.toml");
        tokio::fs::write(
            &role_config_path,
            r#"model = "gpt-5.6-sol"
service_tier = "priority"
"#,
        )
        .await
        .expect("role config should be written");

        let role_name = "service-tier-role".to_string();
        let mut config = (*turn.config).clone();
        config.agent_roles.insert(
            role_name.clone(),
            AgentRoleConfig {
                description: Some("Role with a child service tier".to_string()),
                config_file: Some(role_config_path),
                nickname_candidates: None,
            },
        );
        turn.config = Arc::new(config);
        let manager = thread_manager();
        let root = manager
            .start_thread(StartThreadOptions::new((*turn.config).clone()))
            .await
            .expect("root thread should start");
        session.services.agent_control = manager.agent_control();
        session.thread_id = root.thread_id;

        let output = SpawnAgentHandler::default()
            .handle(invocation(
                Arc::new(session),
                Arc::new(turn),
                "spawn_agent",
                function_payload(json!({
                    "message": "inspect this repo",
                    "agent_type": role_name
                })),
            ))
            .await
            .expect("spawn_agent should preserve the child role service tier");
        let (content, _) = expect_text_output(output);
        let result: SpawnAgentResult =
            serde_json::from_str(&content).expect("spawn_agent result should be json");
        let snapshot = manager
            .get_thread(parse_agent_id(&result.agent_id))
            .await
            .expect("spawned agent thread should exist")
            .config_snapshot()
            .await;

        assert_eq!(
            snapshot.service_tier,
            Some(ServiceTier::Fast.request_value().to_string())
        );
    }
}

#[tokio::test]
async fn spawn_agent_role_service_tier_falls_back_to_supported_parent_tier() {
    #[derive(Debug, Deserialize)]
    struct SpawnAgentResult {
        agent_id: String,
    }

    let (mut session, turn) = make_session_and_context().await;
    let mut turn = turn
        .with_model("gpt-5.6-sol".to_string(), &session.services.models_manager)
        .await;
    set_turn_to_openai_provider(&mut turn);
    tokio::fs::create_dir_all(&turn.config.codex_home)
        .await
        .expect("codex home should be created");
    let role_config_path = turn.config.codex_home.as_path().join("tiered-role.toml");
    tokio::fs::write(
        &role_config_path,
        r#"model = "gpt-5.6-sol"
service_tier = "turbo"
"#,
    )
    .await
    .expect("role config should be written");

    let role_name = "tiered-role".to_string();
    let mut config = (*turn.config).clone();
    config.service_tier = Some(ServiceTier::Fast.request_value().to_string());
    config.agent_roles.insert(
        role_name.clone(),
        AgentRoleConfig {
            description: Some("Role with an unsupported child tier".to_string()),
            config_file: Some(role_config_path),
            nickname_candidates: None,
        },
    );
    turn.config = Arc::new(config);
    let manager = thread_manager();
    let root = manager
        .start_thread(StartThreadOptions::new((*turn.config).clone()))
        .await
        .expect("root thread should start");
    session.services.agent_control = manager.agent_control();
    session.thread_id = root.thread_id;

    let output = SpawnAgentHandler::default()
        .handle(invocation(
            Arc::new(session),
            Arc::new(turn),
            "spawn_agent",
            function_payload(json!({
                "message": "inspect this repo",
                "agent_type": role_name
            })),
        ))
        .await
        .expect("spawn_agent should fall back to the supported parent tier");
    let (content, _) = expect_text_output(output);
    let result: SpawnAgentResult =
        serde_json::from_str(&content).expect("spawn_agent result should be json");
    let snapshot = manager
        .get_thread(parse_agent_id(&result.agent_id))
        .await
        .expect("spawned agent thread should exist")
        .config_snapshot()
        .await;

    assert_eq!(
        snapshot.service_tier,
        Some(ServiceTier::Fast.request_value().to_string())
    );
}

#[tokio::test]
async fn spawn_agent_role_service_tier_does_not_hide_invalid_spawn_request() {
    let (session, mut turn) = make_session_and_context().await;
    set_turn_to_openai_provider(&mut turn);
    tokio::fs::create_dir_all(&turn.config.codex_home)
        .await
        .expect("codex home should be created");
    let role_config_path = turn.config.codex_home.as_path().join("tiered-role.toml");
    tokio::fs::write(
        &role_config_path,
        r#"model = "gpt-5.6-sol"
service_tier = "priority"
"#,
    )
    .await
    .expect("role config should be written");

    let role_name = "tiered-role".to_string();
    let mut config = (*turn.config).clone();
    config.agent_roles.insert(
        role_name.clone(),
        AgentRoleConfig {
            description: Some("Role with a supported child tier".to_string()),
            config_file: Some(role_config_path),
            nickname_candidates: None,
        },
    );
    turn.config = Arc::new(config);

    let result = SpawnAgentHandler::default()
        .handle(invocation(
            Arc::new(session),
            Arc::new(turn),
            "spawn_agent",
            function_payload(json!({
                "message": "inspect this repo",
                "agent_type": role_name,
                "service_tier": "turbo"
            })),
        ))
        .await;

    assert_eq!(
        result.err(),
        Some(FunctionCallError::RespondToModel(
            "Service tier `turbo` is not supported for model `gpt-5.6-sol`. Supported service tiers: priority"
                .to_string()
        ))
    );
}

#[tokio::test]
async fn spawn_agent_full_history_fork_accepts_explicit_service_tier() {
    #[derive(Debug, Deserialize)]
    struct SpawnAgentResult {
        agent_id: String,
    }

    let (mut session, turn) = make_session_and_context().await;
    let mut turn = turn
        .with_model("gpt-5.6-sol".to_string(), &session.services.models_manager)
        .await;
    set_turn_to_openai_provider(&mut turn);
    let manager = thread_manager();
    let root = manager
        .start_thread(StartThreadOptions::new((*turn.config).clone()))
        .await
        .expect("root thread should start");
    session.services.agent_control = manager.agent_control();
    session.thread_id = root.thread_id;

    let output = SpawnAgentHandler::default()
        .handle(invocation(
            Arc::new(session),
            Arc::new(turn),
            "spawn_agent",
            function_payload(json!({
                "message": "inspect this repo",
                "fork_context": true,
                "service_tier": ServiceTier::Fast.request_value()
            })),
        ))
        .await
        .expect("full-history fork should accept explicit service tier");
    let (content, _) = expect_text_output(output);
    let result: SpawnAgentResult =
        serde_json::from_str(&content).expect("spawn_agent result should be json");
    let snapshot = manager
        .get_thread(parse_agent_id(&result.agent_id))
        .await
        .expect("spawned agent thread should exist")
        .config_snapshot()
        .await;

    assert_eq!(
        snapshot.service_tier,
        Some(ServiceTier::Fast.request_value().to_string())
    );
}

#[tokio::test]
async fn multi_agent_v2_full_history_fork_accepts_explicit_service_tier() {
    #[derive(Debug, Deserialize)]
    struct SpawnAgentResult {
        task_name: String,
    }

    let (mut session, turn) = make_session_and_context().await;
    let mut turn = turn
        .with_model("gpt-5.6-sol".to_string(), &session.services.models_manager)
        .await;
    set_turn_to_openai_provider(&mut turn);
    let mut config = (*turn.config).clone();
    config
        .features
        .enable(Feature::MultiAgentV2)
        .expect("test config should allow feature update");
    set_turn_config(&mut turn, config);
    let manager = thread_manager();
    let root = manager
        .start_thread(StartThreadOptions::new((*turn.config).clone()))
        .await
        .expect("root thread should start");
    session.services.agent_control = manager.agent_control();
    session.thread_id = root.thread_id;
    let session = Arc::new(session);
    let turn = Arc::new(turn);

    let output = SpawnAgentHandlerV2::default()
        .handle(invocation(
            session.clone(),
            turn.clone(),
            "spawn_agent",
            function_payload(json!({
                "message": "inspect this repo",
                "task_name": "fork_with_tier",
                "service_tier": ServiceTier::Fast.request_value()
            })),
        ))
        .await
        .expect("multi-agent v2 full-history fork should accept explicit service tier");
    let (content, _) = expect_text_output(output);
    let result: SpawnAgentResult =
        serde_json::from_str(&content).expect("spawn_agent result should be json");
    let child_thread_id = session
        .services
        .agent_control
        .resolve_agent_reference(
            session.thread_id,
            &turn.session_source,
            result.task_name.as_str(),
        )
        .await
        .expect("spawned task name should resolve");
    let snapshot = manager
        .get_thread(child_thread_id)
        .await
        .expect("spawned agent thread should exist")
        .config_snapshot()
        .await;

    assert_eq!(
        snapshot.service_tier,
        Some(ServiceTier::Fast.request_value().to_string())
    );
}

#[tokio::test]
async fn multi_agent_v2_spawn_partial_fork_turns_allows_agent_type_override() {
    let (mut session, mut turn) = make_session_and_context().await;
    let role_name = install_role_with_model_override(&mut turn).await;
    let manager = thread_manager();
    let root = manager
        .start_thread(StartThreadOptions::new((*turn.config).clone()))
        .await
        .expect("root thread should start");
    session.services.agent_control = manager.agent_control();
    session.thread_id = root.thread_id;
    let mut config = (*turn.config).clone();
    config
        .features
        .enable(Feature::MultiAgentV2)
        .expect("test config should allow feature update");
    let turn = TurnContext {
        config: Arc::new(config),
        multi_agent_version: codex_protocol::protocol::MultiAgentVersion::V2,
        ..turn
    };

    let output = SpawnAgentHandlerV2::default()
        .handle(invocation(
            Arc::new(session),
            Arc::new(turn),
            "spawn_agent",
            function_payload(json!({
                "message": "inspect this repo",
                "task_name": "partial_fork",
                "agent_type": role_name,
                "fork_turns": "1"
            })),
        ))
        .await
        .expect("partial fork should allow agent_type overrides");
    let (content, _) = expect_text_output(output);
    let result: serde_json::Value =
        serde_json::from_str(&content).expect("spawn_agent result should be json");
    assert_eq!(result["task_name"], "/root/partial_fork");
    let agent_id = manager
        .captured_ops()
        .into_iter()
        .map(|(thread_id, _)| thread_id)
        .find(|thread_id| *thread_id != root.thread_id)
        .expect("spawned agent should receive an op");
    let snapshot = manager
        .get_thread(agent_id)
        .await
        .expect("spawned agent thread should exist")
        .config_snapshot()
        .await;

    assert_eq!(snapshot.model, "gpt-5.6-sol");
    assert_eq!(snapshot.model_provider_id, "openai");
    assert_eq!(snapshot.reasoning_effort, Some(ReasoningEffort::Low));
}

#[tokio::test]
async fn spawn_agent_returns_agent_id_without_task_name() {
    let (mut session, turn) = make_session_and_context().await;
    let turn = with_spawn_eligible_runtime(&session, turn).await;
    let _manager = install_live_root(&mut session, &turn).await;

    let output = SpawnAgentHandler::default()
        .handle(invocation(
            Arc::new(session),
            Arc::new(turn),
            "spawn_agent",
            function_payload(json!({
                "message": "inspect this repo"
            })),
        ))
        .await
        .expect("spawn_agent should succeed");
    let (content, success) = expect_text_output(output);
    let result: serde_json::Value =
        serde_json::from_str(&content).expect("spawn_agent result should be json");

    assert!(result["agent_id"].is_string());
    assert!(result.get("task_name").is_none());
    assert!(result.get("nickname").is_some());
    assert_eq!(success, Some(true));
}

#[tokio::test]
async fn multi_agent_v2_spawn_explicit_runtime_wins_over_role_runtime() {
    let (mut session, mut turn) = make_session_and_context().await;
    let role_name = install_role_with_model_override(&mut turn).await;
    let manager = thread_manager();
    let root = manager
        .start_thread((*turn.config).clone())
        .await
        .expect("root thread should start");
    session.services.agent_control = manager.agent_control();
    session.thread_id = root.thread_id;
    let mut config = (*turn.config).clone();
    config
        .features
        .enable(Feature::MultiAgentV2)
        .expect("test config should allow feature update");
    set_turn_config(&mut turn, config);
    let session = Arc::new(session);
    let turn = Arc::new(turn);

    let output = PlaintextSpawnAgentHandler::default()
        .handle(invocation(
            session.clone(),
            turn.clone(),
            "spawn_agent_plaintext",
            function_payload(json!({
                "message": "inspect this repo",
                "task_name": "kimi_worker",
                "agent_type": role_name,
                "model_provider": KIMI_CODE_PROVIDER_ID,
                "model": KIMI_CODE_K3_MODEL,
                "fork_turns": "none"
            })),
        ))
        .await
        .expect("explicit runtime should override role runtime");
    let (content, _) = expect_text_output(output);
    let result: serde_json::Value =
        serde_json::from_str(&content).expect("spawn_agent result should be json");
    assert_eq!(result["model_provider"], KIMI_CODE_PROVIDER_ID);
    assert_eq!(result["model"], KIMI_CODE_K3_MODEL);
    let child_thread_id = session
        .services
        .agent_control
        .resolve_agent_reference(
            session.thread_id,
            &turn.session_source,
            result["task_name"]
                .as_str()
                .expect("task_name should be returned"),
        )
        .await
        .expect("spawned task name should resolve");
    let snapshot = manager
        .get_thread(child_thread_id)
        .await
        .expect("spawned agent thread should exist")
        .config_snapshot()
        .await;

    assert_eq!(snapshot.model_provider_id, KIMI_CODE_PROVIDER_ID);
    assert_eq!(snapshot.model, KIMI_CODE_K3_MODEL);
}

#[tokio::test]
async fn multi_agent_v2_spawn_requires_task_name() {
    let (mut session, mut turn) = make_session_and_context().await;
    let manager = thread_manager();
    let root = manager
        .start_thread(StartThreadOptions::new((*turn.config).clone()))
        .await
        .expect("root thread should start");
    session.services.agent_control = manager.agent_control();
    session.thread_id = root.thread_id;
    let mut config = (*turn.config).clone();
    config
        .features
        .enable(Feature::MultiAgentV2)
        .expect("test config should allow feature update");
    set_turn_config(&mut turn, config);

    let invocation = invocation(
        Arc::new(session),
        Arc::new(turn),
        "spawn_agent",
        function_payload(json!({
            "message": "inspect this repo"
        })),
    );
    let Err(err) = SpawnAgentHandlerV2::default().handle(invocation).await else {
        panic!("missing task_name should be rejected");
    };
    let Some(message) = model_facing_error_message(&err) else {
        panic!("missing task_name should surface as a model-facing error");
    };
    assert!(message.contains("missing field `task_name`"));
}

#[tokio::test]
async fn multi_agent_v2_spawn_rejects_legacy_items_field() {
    let (mut session, mut turn) = make_session_and_context().await;
    let manager = thread_manager();
    let root = manager
        .start_thread(StartThreadOptions::new((*turn.config).clone()))
        .await
        .expect("root thread should start");
    session.services.agent_control = manager.agent_control();
    session.thread_id = root.thread_id;
    let mut config = (*turn.config).clone();
    config
        .features
        .enable(Feature::MultiAgentV2)
        .expect("test config should allow feature update");
    set_turn_config(&mut turn, config);

    let invocation = invocation(
        Arc::new(session),
        Arc::new(turn),
        "spawn_agent",
        function_payload(json!({
            "message": "inspect this repo",
            "items": [{"type": "text", "text": "inspect this repo"}],
            "task_name": "worker"
        })),
    );
    let Err(err) = SpawnAgentHandlerV2::default().handle(invocation).await else {
        panic!("legacy items field should be rejected");
    };
    let Some(message) = model_facing_error_message(&err) else {
        panic!("legacy items field should surface as a model-facing error");
    };
    assert!(message.contains("unknown field `items`"));
}

#[tokio::test]
async fn spawn_agent_errors_when_manager_dropped() {
    let (session, turn) = make_session_and_context().await;
    let turn = with_spawn_eligible_runtime(&session, turn).await;
    let invocation = invocation(
        Arc::new(session),
        Arc::new(turn),
        "spawn_agent",
        function_payload(json!({"message": "hello"})),
    );
    let Err(err) = SpawnAgentHandler::default().handle(invocation).await else {
        panic!("spawn should fail without a manager");
    };
    assert_eq!(
        err,
        FunctionCallError::RespondToModel("collab manager unavailable".to_string())
    );
}

#[tokio::test]
async fn multi_agent_v2_spawn_returns_path_and_send_message_accepts_relative_path() {
    #[derive(Debug, Deserialize)]
    struct SpawnAgentResult {
        task_name: String,
        nickname: Option<String>,
        model_provider: String,
        model: String,
        reasoning_effort: Option<ReasoningEffort>,
        service_tier: Option<String>,
    }

    let (mut session, turn) = make_session_and_context().await;
    let mut turn = turn
        .with_model("gpt-5.6-sol".to_string(), &session.services.models_manager)
        .await;
    set_turn_to_openai_provider(&mut turn);
    let manager = thread_manager();
    let root = manager
        .start_thread(StartThreadOptions::new((*turn.config).clone()))
        .await
        .expect("root thread should start");
    session.services.agent_control = manager.agent_control();
    session.thread_id = root.thread_id;
    let mut config = (*turn.config).clone();
    config
        .features
        .enable(Feature::MultiAgentV2)
        .expect("test config should allow feature update");
    set_turn_config(&mut turn, config);

    let session = Arc::new(session);
    let turn = Arc::new(turn);
    let spawn_output = SpawnAgentHandlerV2::default()
        .handle(invocation(
            session.clone(),
            turn.clone(),
            "spawn_agent",
            function_payload(json!({
                "message": "encrypted-spawn-message",
                "task_name": "test_process"
            })),
        ))
        .await
        .expect("spawn_agent should succeed");
    let (content, _) = expect_text_output(spawn_output);
    let spawn_result: SpawnAgentResult =
        serde_json::from_str(&content).expect("spawn result should parse");
    assert_eq!(spawn_result.task_name, "/root/test_process");
    assert!(
        spawn_result
            .nickname
            .as_deref()
            .is_some_and(|nickname| !nickname.is_empty())
    );
    assert_eq!(spawn_result.model_provider, OPENAI_PROVIDER_ID);
    assert_eq!(spawn_result.model, turn.model_info.slug);
    assert_eq!(
        spawn_result.reasoning_effort,
        turn.reasoning_effort
            .clone()
            .or_else(|| turn.model_info.default_reasoning_level.clone())
    );
    assert_eq!(spawn_result.service_tier, turn.config.service_tier);

    let child_thread_id = session
        .services
        .agent_control
        .resolve_agent_reference(session.thread_id, &turn.session_source, "test_process")
        .await
        .expect("relative path should resolve");
    let child_snapshot = manager
        .get_thread(child_thread_id)
        .await
        .expect("child thread should exist")
        .config_snapshot()
        .await;
    assert_eq!(
        child_snapshot.session_source.get_agent_path().as_deref(),
        Some("/root/test_process")
    );
    assert!(manager.captured_ops().iter().any(|(id, op)| {
        *id == child_thread_id
            && matches!(
                op,
                Op::InterAgentCommunication { communication }
                    if communication.author == AgentPath::root()
                        && communication.recipient.as_str() == "/root/test_process"
                        && communication.other_recipients.is_empty()
                        && communication.content.is_empty()
                        && communication.encrypted_content.as_deref() == Some("encrypted-spawn-message")
                        && communication.trigger_turn
            )
    }));

    let send_output = SendMessageHandlerV2
        .handle(invocation(
            session.clone(),
            turn.clone(),
            "send_message",
            function_payload(json!({
                "target": "test_process",
                "message": "encrypted-send-message"
            })),
        ))
        .await
        .expect("send_message should accept v2 path");
    let (content, success) = expect_text_output(send_output);
    assert_eq!(success, Some(true));
    let send_result: MessageToolResult =
        serde_json::from_str(&content).expect("send_message result should parse");
    assert!(!send_result.message_id.is_empty());
    assert_eq!(send_result.target_thread_id, child_thread_id.to_string());
    assert_eq!(send_result.agent_path, "/root/test_process");
    assert_eq!(send_result.delivery, "queued");
    assert!(!send_result.triggered_turn);
    assert_eq!(
        session
            .services
            .agent_control
            .get_agent_metadata(child_thread_id)
            .and_then(|metadata| metadata.last_task_message),
        None,
        "OpenAI's opaque collaboration payload must not be exposed as a task preview"
    );

    assert!(manager.captured_ops().iter().any(|(id, op)| {
        *id == child_thread_id
            && matches!(
                op,
                Op::InterAgentCommunication { communication }
                    if communication.author == AgentPath::root()
                        && communication.recipient.as_str() == "/root/test_process"
                        && communication.other_recipients.is_empty()
                        && communication.content.is_empty()
                        && communication.encrypted_content.as_deref() == Some("encrypted-send-message")
                        && !communication.trigger_turn
            )
    }));
}

#[tokio::test]
async fn openai_encrypted_message_fails_closed_then_plaintext_adapter_reaches_external_agent() {
    let (mut session, turn) = make_session_and_context().await;
    let mut turn = turn
        .with_model("gpt-5.6-sol".to_string(), &session.services.models_manager)
        .await;
    set_turn_to_openai_provider(&mut turn);
    let mut config = (*turn.config).clone();
    config
        .features
        .enable(Feature::MultiAgentV2)
        .expect("test config should allow feature update");
    set_turn_config(&mut turn, config);
    let manager = thread_manager();
    let root = manager
        .start_thread(StartThreadOptions::new((*turn.config).clone()))
        .await
        .expect("root thread should start");
    session.services.agent_control = manager.agent_control();
    session.thread_id = root.thread_id;

    let external_path = AgentPath::try_from("/root/external_worker").expect("agent path");
    let mut external_config = (*turn.config).clone();
    external_config.model_provider_id = OPENROUTER_PROVIDER_ID.to_string();
    external_config.model_provider =
        external_config.model_providers[OPENROUTER_PROVIDER_ID].clone();
    external_config.model = Some("x-ai/grok-4.5".to_string());
    let external = session
        .services
        .agent_control
        .spawn_agent_with_metadata(
            external_config,
            Vec::new(),
            Some(SessionSource::SubAgent(SubAgentSource::ThreadSpawn {
                parent_thread_id: root.thread_id,
                depth: 1,
                agent_path: Some(external_path.clone()),
                agent_nickname: Some("Krimp".to_string()),
                agent_role: Some("orc".to_string()),
                agent_class: None,
            })),
            crate::agent::control::SpawnAgentOptions::default(),
        )
        .await
        .expect("external worker should register");
    let session = Arc::new(session);
    let turn = Arc::new(turn);
    let message = "reply exactly CROSS_PROVIDER_OK";

    let send_err = SendMessageHandlerV2
        .handle(invocation(
            session.clone(),
            turn.clone(),
            "send_message",
            function_payload(json!({
                "target": external_path,
                "message": message
            })),
        ))
        .await
        .err()
        .expect("encrypted native send must fail before external admission");
    assert!(
        model_facing_error_message(&send_err)
            .is_some_and(|error| error.contains("No message was admitted"))
    );
    let followup_err = FollowupTaskHandlerV2
        .handle(invocation(
            session.clone(),
            turn.clone(),
            "followup_task",
            function_payload(json!({
                "target": external_path,
                "message": message
            })),
        ))
        .await
        .err()
        .expect("encrypted native follow-up must fail before external admission");
    assert!(
        model_facing_error_message(&followup_err)
            .is_some_and(|error| error.contains("No message was admitted"))
    );
    assert!(!manager.captured_ops().iter().any(|(thread_id, op)| {
        *thread_id == external.thread_id
            && matches!(
                op,
                Op::InterAgentCommunication { communication }
                    if communication.content.contains(message)
                        || communication.encrypted_content.as_deref() == Some(message)
            )
    }));

    PlaintextSendMessageHandler
        .handle(invocation(
            session.clone(),
            turn.clone(),
            "send_message_plaintext",
            function_payload(json!({
                "target": external_path,
                "message": "queued external context"
            })),
        ))
        .await
        .expect("plaintext send adapter should queue external context");
    PlaintextFollowupTaskHandler
        .handle(invocation(
            session,
            turn,
            "followup_task_plaintext",
            function_payload(json!({
                "target": external_path,
                "message": message
            })),
        ))
        .await
        .expect("plaintext follow-up adapter should trigger external work");

    assert!(manager.captured_ops().iter().any(|(thread_id, op)| {
        *thread_id == external.thread_id
            && matches!(
                op,
                Op::InterAgentCommunication { communication }
                    if communication.content.contains("queued external context")
                        && communication.encrypted_content.is_none()
                        && !communication.trigger_turn
            )
    }));
    assert!(manager.captured_ops().iter().any(|(thread_id, op)| {
        *thread_id == external.thread_id
            && matches!(
                op,
                Op::InterAgentCommunication { communication }
                    if communication.content.contains(message)
                        && communication.encrypted_content.is_none()
                        && communication.trigger_turn
            )
    }));
}

#[tokio::test]
async fn openai_cross_provider_spawn_fails_closed_then_plaintext_adapter_preserves_assignment() {
    #[derive(Debug, Deserialize)]
    struct SpawnResult {
        task_name: String,
        model_provider: String,
    }

    let (mut session, turn) = make_session_and_context().await;
    let mut turn = turn
        .with_model("gpt-5.6-sol".to_string(), &session.services.models_manager)
        .await;
    set_turn_to_openai_provider(&mut turn);
    let mut config = (*turn.config).clone();
    config
        .features
        .enable(Feature::MultiAgentV2)
        .expect("test config should allow feature update");
    set_turn_config(&mut turn, config);
    let manager = thread_manager();
    let root = manager
        .start_thread(StartThreadOptions::new((*turn.config).clone()))
        .await
        .expect("root thread should start");
    session.services.agent_control = manager.agent_control();
    session.thread_id = root.thread_id;
    let session = Arc::new(session);
    let turn = Arc::new(turn);
    let arguments = json!({
        "message": "reply exactly EXTERNAL_SPAWN_OK",
        "task_name": "external_worker",
        "model_provider": OPENROUTER_PROVIDER_ID,
        "model": "x-ai/grok-4.5",
        "fork_turns": "none"
    });

    let err = SpawnAgentHandlerV2::default()
        .handle(invocation(
            session.clone(),
            turn.clone(),
            "spawn_agent",
            function_payload(arguments.clone()),
        ))
        .await
        .err()
        .expect("encrypted native spawn must fail before cross-provider mutation");
    assert!(model_facing_error_message(&err).is_some_and(|error| {
        error.contains("spawn_agent_plaintext") && error.contains("No message was admitted")
    }));
    assert_eq!(manager.list_thread_ids().await, vec![root.thread_id]);

    let output = PlaintextSpawnAgentHandler::default()
        .handle(invocation(
            session,
            turn,
            "spawn_agent_plaintext",
            function_payload(arguments),
        ))
        .await
        .expect("plaintext spawn adapter should create the external worker");
    let (content, _) = expect_text_output(output);
    let result: SpawnResult = serde_json::from_str(&content).expect("spawn result should parse");
    assert_eq!(result.task_name, "/root/external_worker");
    assert_eq!(result.model_provider, OPENROUTER_PROVIDER_ID);
    let external_id = manager
        .list_thread_ids()
        .await
        .into_iter()
        .find(|thread_id| *thread_id != root.thread_id)
        .expect("external worker thread");
    assert!(manager.captured_ops().iter().any(|(thread_id, op)| {
        *thread_id == external_id
            && matches!(
                op,
                Op::InterAgentCommunication { communication }
                    if communication.content.contains("reply exactly EXTERNAL_SPAWN_OK")
                        && communication.encrypted_content.is_none()
                        && communication.trigger_turn
            )
    }));
}

#[tokio::test]
async fn multi_agent_v2_spawn_rejects_legacy_fork_context() {
    let (mut session, mut turn) = make_session_and_context().await;
    let manager = thread_manager();
    let root = manager
        .start_thread(StartThreadOptions::new((*turn.config).clone()))
        .await
        .expect("root thread should start");
    session.services.agent_control = manager.agent_control();
    session.thread_id = root.thread_id;
    let mut config = (*turn.config).clone();
    config
        .features
        .enable(Feature::MultiAgentV2)
        .expect("test config should allow feature update");
    set_turn_config(&mut turn, config);

    let err = SpawnAgentHandlerV2::default()
        .handle(invocation(
            Arc::new(session),
            Arc::new(turn),
            "spawn_agent",
            function_payload(json!({
                "message": "inspect this repo",
                "task_name": "worker",
                "fork_context": true
            })),
        ))
        .await
        .err()
        .expect("legacy fork_context should be rejected");

    assert_eq!(
        err,
        FunctionCallError::RespondToModel(
            "fork_context is not supported in MultiAgentV2; use fork_turns instead".to_string()
        )
    );
}

#[tokio::test]
async fn multi_agent_v2_spawn_rejects_invalid_fork_turns_string() {
    let (mut session, mut turn) = make_session_and_context().await;
    let manager = thread_manager();
    let root = manager
        .start_thread(StartThreadOptions::new((*turn.config).clone()))
        .await
        .expect("root thread should start");
    session.services.agent_control = manager.agent_control();
    session.thread_id = root.thread_id;
    let mut config = (*turn.config).clone();
    config
        .features
        .enable(Feature::MultiAgentV2)
        .expect("test config should allow feature update");
    set_turn_config(&mut turn, config);

    let err = SpawnAgentHandlerV2::default()
        .handle(invocation(
            Arc::new(session),
            Arc::new(turn),
            "spawn_agent",
            function_payload(json!({
                "message": "inspect this repo",
                "task_name": "worker",
                "fork_turns": "banana"
            })),
        ))
        .await
        .err()
        .expect("invalid fork_turns should be rejected");

    assert_eq!(
        err,
        FunctionCallError::RespondToModel(
            "fork_turns must be `none`, `all`, or a positive integer string".to_string()
        )
    );
}

#[tokio::test]
async fn multi_agent_v2_spawn_rejects_zero_fork_turns() {
    let (mut session, mut turn) = make_session_and_context().await;
    let manager = thread_manager();
    let root = manager
        .start_thread(StartThreadOptions::new((*turn.config).clone()))
        .await
        .expect("root thread should start");
    session.services.agent_control = manager.agent_control();
    session.thread_id = root.thread_id;
    let mut config = (*turn.config).clone();
    config
        .features
        .enable(Feature::MultiAgentV2)
        .expect("test config should allow feature update");
    set_turn_config(&mut turn, config);

    let err = SpawnAgentHandlerV2::default()
        .handle(invocation(
            Arc::new(session),
            Arc::new(turn),
            "spawn_agent",
            function_payload(json!({
                "message": "inspect this repo",
                "task_name": "worker",
                "fork_turns": "0"
            })),
        ))
        .await
        .err()
        .expect("zero turn count should be rejected");

    assert_eq!(
        err,
        FunctionCallError::RespondToModel(
            "fork_turns must be `none`, `all`, or a positive integer string".to_string()
        )
    );
}

#[tokio::test]
async fn multi_agent_v2_send_message_accepts_root_target_from_child() {
    let (mut session, mut turn) = make_session_and_context().await;
    set_turn_to_openai_provider(&mut turn);
    let manager = thread_manager();
    let mut config = (*turn.config).clone();
    config
        .features
        .enable(Feature::MultiAgentV2)
        .expect("test config should allow feature update");
    set_turn_config(&mut turn, config);
    let root = manager
        .start_thread(StartThreadOptions::new((*turn.config).clone()))
        .await
        .expect("root thread should start");
    session.services.agent_control = manager.agent_control();
    session.thread_id = root.thread_id;

    let child_path = AgentPath::try_from("/root/worker").expect("agent path");
    let child_thread_id = session
        .services
        .agent_control
        .spawn_agent_with_metadata(
            (*turn.config).clone(),
            vec![UserInput::Text {
                text: "inspect this repo".to_string(),
                text_elements: Vec::new(),
            }],
            Some(SessionSource::SubAgent(SubAgentSource::ThreadSpawn {
                parent_thread_id: root.thread_id,
                depth: 1,
                agent_path: Some(child_path.clone()),
                agent_nickname: None,
                agent_role: None,
                agent_class: None,
            })),
            crate::agent::control::SpawnAgentOptions::default(),
        )
        .await
        .expect("worker spawn should succeed")
        .thread_id;
    session.thread_id = child_thread_id;
    turn.session_source = SessionSource::SubAgent(SubAgentSource::ThreadSpawn {
        parent_thread_id: root.thread_id,
        depth: 1,
        agent_path: Some(child_path.clone()),
        agent_nickname: None,
        agent_role: None,
        agent_class: None,
    });

    SendMessageHandlerV2
        .handle(invocation(
            Arc::new(session),
            Arc::new(turn),
            "send_message",
            function_payload(json!({
                "target": "/root",
                "message": "encrypted-done"
            })),
        ))
        .await
        .expect("send_message should accept the root agent path");

    assert!(manager.captured_ops().iter().any(|(id, op)| {
        *id == root.thread_id
            && matches!(
                op,
                Op::InterAgentCommunication { communication }
                    if communication.author == child_path
                        && communication.recipient == AgentPath::root()
                        && communication.other_recipients.is_empty()
                        && communication.content.is_empty()
                        && communication.encrypted_content.as_deref() == Some("encrypted-done")
                        && !communication.trigger_turn
            )
    }));
}

#[tokio::test]
async fn multi_agent_v2_followup_task_rejects_root_target_from_child() {
    let (mut session, mut turn) = make_session_and_context().await;
    let manager = thread_manager();
    let mut config = (*turn.config).clone();
    config
        .features
        .enable(Feature::MultiAgentV2)
        .expect("test config should allow feature update");
    set_turn_config(&mut turn, config);
    let root = manager
        .start_thread(StartThreadOptions::new((*turn.config).clone()))
        .await
        .expect("root thread should start");
    session.services.agent_control = manager.agent_control();
    session.thread_id = root.thread_id;

    let child_path = AgentPath::try_from("/root/worker").expect("agent path");
    let child_thread_id = session
        .services
        .agent_control
        .spawn_agent_with_metadata(
            (*turn.config).clone(),
            vec![UserInput::Text {
                text: "inspect this repo".to_string(),
                text_elements: Vec::new(),
            }],
            Some(SessionSource::SubAgent(SubAgentSource::ThreadSpawn {
                parent_thread_id: root.thread_id,
                depth: 1,
                agent_path: Some(child_path.clone()),
                agent_nickname: None,
                agent_role: None,
                agent_class: None,
            })),
            crate::agent::control::SpawnAgentOptions::default(),
        )
        .await
        .expect("worker spawn should succeed")
        .thread_id;
    session.thread_id = child_thread_id;
    turn.session_source = SessionSource::SubAgent(SubAgentSource::ThreadSpawn {
        parent_thread_id: root.thread_id,
        depth: 1,
        agent_path: Some(child_path),
        agent_nickname: None,
        agent_role: None,
        agent_class: None,
    });

    let Err(err) = FollowupTaskHandlerV2
        .handle(invocation(
            Arc::new(session),
            Arc::new(turn),
            "followup_task",
            function_payload(json!({
                "target": "/root",
                "message": "run this",
            })),
        ))
        .await
    else {
        panic!("followup_task should reject the root target");
    };

    assert_eq!(
        err,
        FunctionCallError::RespondToModel(
            "Follow-up tasks can't target the root agent".to_string()
        )
    );
    let root_ops = manager
        .captured_ops()
        .into_iter()
        .filter_map(|(id, op)| (id == root.thread_id).then_some(op))
        .collect::<Vec<_>>();
    assert!(!root_ops.iter().any(|op| matches!(op, Op::Interrupt)));
    assert!(
        !root_ops
            .iter()
            .any(|op| matches!(op, Op::InterAgentCommunication { .. }))
    );
}

#[tokio::test]
async fn multi_agent_v2_list_agents_returns_completed_status() {
    let (mut session, mut turn) = make_session_and_context().await;
    let manager = thread_manager();
    let root = manager
        .start_thread(StartThreadOptions::new((*turn.config).clone()))
        .await
        .expect("root thread should start");
    session.services.agent_control = manager.agent_control();
    session.thread_id = root.thread_id;
    let mut config = (*turn.config).clone();
    let _ = config.features.enable(Feature::MultiAgentV2);
    set_turn_config(&mut turn, config);

    let session = Arc::new(session);
    let turn = Arc::new(turn);
    let spawn_output = SpawnAgentHandlerV2::default()
        .handle(invocation(
            session.clone(),
            turn.clone(),
            "spawn_agent",
            function_payload(json!({
                "message": "inspect this repo",
                "task_name": "worker"
            })),
        ))
        .await
        .expect("spawn_agent should succeed");
    let _ = expect_text_output(spawn_output);

    let agent_id = session
        .services
        .agent_control
        .resolve_agent_reference(session.thread_id, &turn.session_source, "worker")
        .await
        .expect("worker path should resolve");
    let child_thread = manager
        .get_thread(agent_id)
        .await
        .expect("child thread should exist");
    let child_turn = child_thread.session.new_default_turn().await;
    child_thread
        .session
        .send_event(
            child_turn.as_ref(),
            EventMsg::TurnComplete(TurnCompleteEvent {
                turn_id: child_turn.sub_id.clone(),
                started_at: None,
                last_agent_message: Some("done".to_string()),
                error: None,
                completed_at: None,
                duration_ms: None,
                time_to_first_token_ms: None,
            }),
        )
        .await;

    let output = ListAgentsHandlerV2
        .handle(invocation(
            session.clone(),
            turn.clone(),
            "list_agents",
            function_payload(json!({})),
        ))
        .await
        .expect("list_agents should succeed");
    let (content, success) = expect_text_output(output);
    let result: ListAgentsResult =
        serde_json::from_str(&content).expect("list_agents result should be json");

    let agent_names = result
        .agents
        .iter()
        .map(|agent| agent.agent_name.as_str())
        .collect::<Vec<_>>();
    assert_eq!(agent_names, vec!["/root", "/root/worker"]);
    let worker = result
        .agents
        .iter()
        .find(|agent| agent.agent_name == "/root/worker")
        .expect("worker agent should be listed");
    assert_eq!(worker.agent_status, json!({"completed": "done"}));
    assert_eq!(success, Some(true));
}

#[tokio::test]
async fn multi_agent_v2_list_agents_filters_by_relative_path_prefix() {
    let (mut session, mut turn) = make_session_and_context().await;
    let manager = thread_manager();
    let mut config = (*turn.config).clone();
    let _ = config.features.enable(Feature::MultiAgentV2);
    set_turn_config(&mut turn, config.clone());
    let root = manager
        .start_thread(StartThreadOptions::new((*turn.config).clone()))
        .await
        .expect("root thread should start");
    session.services.agent_control = manager.agent_control();
    session.thread_id = root.thread_id;

    let researcher_path = AgentPath::from_string("/root/researcher".to_string()).expect("path");
    let worker_path = AgentPath::from_string("/root/researcher/worker".to_string()).expect("path");
    let researcher = session
        .services
        .agent_control
        .spawn_agent_with_metadata(
            config.clone(),
            vec![UserInput::Text {
                text: "research".to_string(),
                text_elements: Vec::new(),
            }],
            Some(SessionSource::SubAgent(SubAgentSource::ThreadSpawn {
                parent_thread_id: root.thread_id,
                depth: 1,
                agent_path: Some(researcher_path.clone()),
                agent_nickname: None,
                agent_role: None,
                agent_class: None,
            })),
            crate::agent::control::SpawnAgentOptions::default(),
        )
        .await
        .expect("researcher agent should spawn");
    session
        .services
        .agent_control
        .spawn_agent_with_metadata(
            config,
            vec![UserInput::Text {
                text: "build".to_string(),
                text_elements: Vec::new(),
            }],
            Some(SessionSource::SubAgent(SubAgentSource::ThreadSpawn {
                parent_thread_id: researcher.thread_id,
                depth: 2,
                agent_path: Some(worker_path.clone()),
                agent_nickname: None,
                agent_role: None,
                agent_class: None,
            })),
            crate::agent::control::SpawnAgentOptions::default(),
        )
        .await
        .expect("worker agent should spawn");

    turn.session_source = SessionSource::SubAgent(SubAgentSource::ThreadSpawn {
        parent_thread_id: root.thread_id,
        depth: 1,
        agent_path: Some(researcher_path),
        agent_nickname: None,
        agent_role: None,
        agent_class: None,
    });

    let output = ListAgentsHandlerV2
        .handle(invocation(
            Arc::new(session),
            Arc::new(turn),
            "list_agents",
            function_payload(json!({
                "path_prefix": "worker"
            })),
        ))
        .await
        .expect("list_agents should succeed");
    let (content, _) = expect_text_output(output);
    let result: ListAgentsResult =
        serde_json::from_str(&content).expect("list_agents result should be json");

    assert_eq!(result.agents.len(), 1);
    assert_eq!(result.agents[0].agent_name, worker_path.as_str());
}

#[tokio::test]
async fn multi_agent_v2_list_agents_omits_closed_agents() {
    let (mut session, mut turn) = make_session_and_context().await;
    let manager = thread_manager();
    let root = manager
        .start_thread(StartThreadOptions::new((*turn.config).clone()))
        .await
        .expect("root thread should start");
    session.services.agent_control = manager.agent_control();
    session.thread_id = root.thread_id;
    let mut config = (*turn.config).clone();
    let _ = config.features.enable(Feature::MultiAgentV2);
    set_turn_config(&mut turn, config);

    let session = Arc::new(session);
    let turn = Arc::new(turn);
    let spawn_output = SpawnAgentHandlerV2::default()
        .handle(invocation(
            session.clone(),
            turn.clone(),
            "spawn_agent",
            function_payload(json!({
                "message": "inspect this repo",
                "task_name": "worker"
            })),
        ))
        .await
        .expect("spawn_agent should succeed");
    let _ = expect_text_output(spawn_output);

    let agent_id = session
        .services
        .agent_control
        .resolve_agent_reference(session.thread_id, &turn.session_source, "worker")
        .await
        .expect("worker path should resolve");
    session
        .services
        .agent_control
        .close_agent(agent_id)
        .await
        .expect("close_agent should succeed");

    let output = ListAgentsHandlerV2
        .handle(invocation(
            session.clone(),
            turn.clone(),
            "list_agents",
            function_payload(json!({})),
        ))
        .await
        .expect("list_agents should succeed");
    let (content, _) = expect_text_output(output);
    let result: ListAgentsResult =
        serde_json::from_str(&content).expect("list_agents result should be json");

    assert_eq!(result.agents.len(), 1);
    assert_eq!(result.agents[0].agent_name, "/root");
}

#[tokio::test]
async fn multi_agent_v2_list_agents_keeps_interrupted_resident_agents() {
    let (mut session, mut turn) = make_session_and_context().await;
    let manager = thread_manager();
    let root = manager
        .start_thread(StartThreadOptions::new((*turn.config).clone()))
        .await
        .expect("root thread should start");
    session.services.agent_control = manager.agent_control();
    session.thread_id = root.thread_id;
    let mut config = (*turn.config).clone();
    let _ = config.features.enable(Feature::MultiAgentV2);
    set_turn_config(&mut turn, config);

    let session = Arc::new(session);
    let turn = Arc::new(turn);
    let spawn_output = SpawnAgentHandlerV2::default()
        .handle(invocation(
            session.clone(),
            turn.clone(),
            "spawn_agent",
            function_payload(json!({
                "message": "inspect this repo",
                "task_name": "worker"
            })),
        ))
        .await
        .expect("spawn_agent should succeed");
    let _ = expect_text_output(spawn_output);

    let agent_id = session
        .services
        .agent_control
        .resolve_agent_reference(session.thread_id, &turn.session_source, "worker")
        .await
        .expect("worker path should resolve");
    let agent_path = session
        .services
        .agent_control
        .get_agent_metadata(agent_id)
        .expect("worker metadata should exist")
        .agent_path
        .expect("worker path should exist");
    let interrupt_output = InterruptAgentHandler
        .handle(invocation(
            session.clone(),
            turn.clone(),
            "interrupt_agent",
            function_payload(json!({"target": "worker", "reason": "review correction"})),
        ))
        .await
        .expect("interrupt_agent should succeed");
    let _ = expect_text_output(interrupt_output);

    let output = ListAgentsHandlerV2
        .handle(invocation(
            session.clone(),
            turn.clone(),
            "list_agents",
            function_payload(json!({})),
        ))
        .await
        .expect("list_agents should succeed");
    let (content, _) = expect_text_output(output);
    let result: ListAgentsResult =
        serde_json::from_str(&content).expect("list_agents result should be json");

    assert_eq!(result.agents.len(), 2);
    assert_eq!(result.agents[0].agent_name, "/root");
    assert_eq!(result.agents[1].agent_name, agent_path.as_str());
}

#[tokio::test]
async fn multi_agent_v2_send_message_rejects_legacy_items_field() {
    let (mut session, mut turn) = make_session_and_context().await;
    let manager = thread_manager();
    let root = manager
        .start_thread(StartThreadOptions::new((*turn.config).clone()))
        .await
        .expect("root thread should start");
    session.services.agent_control = manager.agent_control();
    session.thread_id = root.thread_id;
    let mut config = turn.config.as_ref().clone();
    let _ = config.features.enable(Feature::MultiAgentV2);
    set_turn_config(&mut turn, config);
    let session = Arc::new(session);
    let turn = Arc::new(turn);

    SpawnAgentHandlerV2::default()
        .handle(invocation(
            session.clone(),
            turn.clone(),
            "spawn_agent",
            function_payload(json!({
                "message": "boot worker",
                "task_name": "worker"
            })),
        ))
        .await
        .expect("spawn worker");
    let agent_id = session
        .services
        .agent_control
        .resolve_agent_reference(session.thread_id, &turn.session_source, "worker")
        .await
        .expect("worker should resolve");
    let invocation = invocation(
        session,
        turn,
        "send_message",
        function_payload(json!({
            "target": agent_id.to_string(),
            "items": [
                {"type": "mention", "name": "drive", "path": "app://google_drive"},
                {"type": "text", "text": "read the folder"}
            ]
        })),
    );

    let Err(err) = SendMessageHandlerV2.handle(invocation).await else {
        panic!("legacy items field should be rejected in v2");
    };
    let Some(message) = model_facing_error_message(&err) else {
        panic!("legacy items field should surface as a model-facing error");
    };
    assert!(message.contains("unknown field `items`"));
}

#[tokio::test]
async fn multi_agent_v2_send_message_rejects_interrupt_parameter() {
    let (mut session, mut turn) = make_session_and_context().await;
    let manager = thread_manager();
    let root = manager
        .start_thread(StartThreadOptions::new((*turn.config).clone()))
        .await
        .expect("root thread should start");
    session.services.agent_control = manager.agent_control();
    session.thread_id = root.thread_id;
    let mut config = turn.config.as_ref().clone();
    let _ = config.features.enable(Feature::MultiAgentV2);
    set_turn_config(&mut turn, config);
    let session = Arc::new(session);
    let turn = Arc::new(turn);

    SpawnAgentHandlerV2::default()
        .handle(invocation(
            session.clone(),
            turn.clone(),
            "spawn_agent",
            function_payload(json!({
                "message": "boot worker",
                "task_name": "worker"
            })),
        ))
        .await
        .expect("spawn worker");
    let agent_id = session
        .services
        .agent_control
        .resolve_agent_reference(session.thread_id, &turn.session_source, "worker")
        .await
        .expect("worker should resolve");

    let invocation = invocation(
        session,
        turn,
        "send_message",
        function_payload(json!({
            "target": agent_id.to_string(),
            "message": "continue",
            "interrupt": true
        })),
    );

    let Err(err) = SendMessageHandlerV2.handle(invocation).await else {
        panic!("send_message interrupt parameter should be rejected");
    };
    let Some(message) = model_facing_error_message(&err) else {
        panic!("expected model-facing parse error");
    };
    assert!(message.contains("unknown field `interrupt`"));

    let ops = manager.captured_ops();
    let ops_for_agent: Vec<&Op> = ops
        .iter()
        .filter_map(|(id, op)| (*id == agent_id).then_some(op))
        .collect();
    assert!(!ops_for_agent.iter().any(|op| matches!(op, Op::Interrupt)));
    assert!(!ops_for_agent.iter().any(|op| matches!(
        op,
        Op::InterAgentCommunication { communication }
            if communication.author == AgentPath::root()
                && communication.recipient.as_str() == "/root/worker"
                && communication.other_recipients.is_empty()
                && communication.content == "continue"
                && communication.encrypted_content.is_none()
                && !communication.trigger_turn
    )));
}

#[tokio::test]
async fn multi_agent_v2_followup_task_accepts_durable_thread_target_and_notifies_parent() {
    let (mut session, mut turn) = make_session_and_context().await;
    let manager = thread_manager();
    let mut config = turn.config.as_ref().clone();
    let _ = config.features.enable(Feature::MultiAgentV2);
    set_turn_config(&mut turn, config);
    let root = manager
        .start_thread(StartThreadOptions::new((*turn.config).clone()))
        .await
        .expect("root thread should start");
    // Production spawn_agent calls happen after the parent turn has resolved
    // and stored its runtime; mirror that before using the synthetic handler.
    root.thread.session.new_default_turn().await;
    session.services.agent_control = manager.agent_control();
    session.thread_id = root.thread_id;
    let session = Arc::new(session);
    let turn = Arc::new(turn);

    SpawnAgentHandlerV2::default()
        .handle(invocation(
            session.clone(),
            turn.clone(),
            "spawn_agent",
            function_payload(json!({
                "message": "boot worker",
                "task_name": "worker"
            })),
        ))
        .await
        .expect("spawn worker");
    let agent_id = session
        .services
        .agent_control
        .resolve_agent_reference(session.thread_id, &turn.session_source, "worker")
        .await
        .expect("worker should resolve");
    let thread = manager
        .get_thread(agent_id)
        .await
        .expect("worker thread should exist");
    let worker_path = AgentPath::try_from("/root/worker").expect("worker path");

    let first_turn = thread.session.new_default_turn().await;
    first_turn
        .parent_completion_expected
        .store(true, std::sync::atomic::Ordering::Release);
    thread
        .session
        .send_event(
            first_turn.as_ref(),
            EventMsg::TurnComplete(TurnCompleteEvent {
                turn_id: first_turn.sub_id.clone(),
                started_at: None,
                last_agent_message: Some("first done".to_string()),
                error: None,
                completed_at: None,
                duration_ms: None,
                time_to_first_token_ms: None,
            }),
        )
        .await;

    FollowupTaskHandlerV2
        .handle(invocation(
            session,
            turn,
            "followup_task",
            function_payload(json!({
                "target": format!("thread:{agent_id}"),
                "message": "continue",
            })),
        ))
        .await
        .expect("followup_task should succeed");

    assert!(manager.captured_ops().iter().any(|(id, op)| {
        *id == agent_id
            && matches!(
                op,
                Op::InterAgentCommunication { communication }
                    if communication.author == AgentPath::root()
                        && communication.recipient == worker_path
                        && communication.content.is_empty()
                        && communication.encrypted_content.as_deref() == Some("continue")
                        && communication.trigger_turn
            )
    }));

    let second_turn = thread.session.new_default_turn().await;
    second_turn
        .parent_completion_expected
        .store(true, std::sync::atomic::Ordering::Release);
    thread
        .session
        .send_event(
            second_turn.as_ref(),
            EventMsg::TurnComplete(TurnCompleteEvent {
                turn_id: second_turn.sub_id.clone(),
                started_at: None,
                last_agent_message: Some("second done".to_string()),
                error: None,
                completed_at: None,
                duration_ms: None,
                time_to_first_token_ms: None,
            }),
        )
        .await;

    let first_notification = format_inter_agent_completion_message(
        AgentPath::root(),
        worker_path.clone(),
        &AgentStatus::Completed(Some("first done".to_string())),
    )
    .expect("completed status should render");
    let second_notification = format_inter_agent_completion_message(
        AgentPath::root(),
        worker_path.clone(),
        &AgentStatus::Completed(Some("second done".to_string())),
    )
    .expect("completed status should render");

    let notifications = timeout(Duration::from_secs(5), async {
        loop {
            let notifications = manager
                .captured_ops()
                .into_iter()
                .filter_map(|(id, op)| {
                    (id == root.thread_id)
                        .then_some(op)
                        .and_then(|op| match op {
                            Op::InterAgentCommunication { communication }
                                if communication.author == worker_path
                                    && communication.recipient == AgentPath::root()
                                    && communication.other_recipients.is_empty()
                                    && !communication.trigger_turn =>
                            {
                                Some(communication.content)
                            }
                            _ => None,
                        })
                })
                .collect::<Vec<_>>();
            let first_count = notifications
                .iter()
                .filter(|message| **message == first_notification)
                .count();
            let second_count = notifications
                .iter()
                .filter(|message| **message == second_notification)
                .count();
            if first_count == 1 && second_count == 1 {
                break notifications;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("parent should receive one completion notification per child turn");

    assert_eq!(notifications.len(), 2);
}

#[tokio::test]
async fn multi_agent_v2_direct_operator_child_turn_does_not_notify_parent() {
    let (mut session, mut turn) = make_session_and_context().await;
    let manager = thread_manager();
    let mut config = turn.config.as_ref().clone();
    let _ = config.features.enable(Feature::MultiAgentV2);
    set_turn_config(&mut turn, config);
    let root = manager
        .start_thread(StartThreadOptions::new((*turn.config).clone()))
        .await
        .expect("root thread should start");
    root.thread.session.new_default_turn().await;
    session.services.agent_control = manager.agent_control();
    session.thread_id = root.thread_id;
    let session = Arc::new(session);
    let turn = Arc::new(turn);

    SpawnAgentHandlerV2::default()
        .handle(invocation(
            session.clone(),
            turn.clone(),
            "spawn_agent",
            function_payload(json!({
                "message": "boot worker",
                "task_name": "worker"
            })),
        ))
        .await
        .expect("spawn worker");
    let agent_id = session
        .services
        .agent_control
        .resolve_agent_reference(session.thread_id, &turn.session_source, "worker")
        .await
        .expect("worker should resolve");
    let thread = manager
        .get_thread(agent_id)
        .await
        .expect("worker thread should exist");
    let direct_turn = thread.session.new_default_turn().await;

    thread
        .session
        .send_event(
            direct_turn.as_ref(),
            EventMsg::TurnComplete(TurnCompleteEvent {
                turn_id: direct_turn.sub_id.clone(),
                started_at: None,
                last_agent_message: Some("operator task done".to_string()),
                error: None,
                completed_at: None,
                duration_ms: None,
                time_to_first_token_ms: None,
            }),
        )
        .await;

    tokio::time::sleep(Duration::from_millis(100)).await;
    assert!(!manager.captured_ops().iter().any(|(id, op)| {
        *id == root.thread_id
            && matches!(
                op,
                Op::InterAgentCommunication { communication }
                    if communication.author.as_str() == "/root/worker"
                        && communication.recipient == AgentPath::root()
                        && communication.content.contains("operator task done")
            )
    }));
}

#[tokio::test]
async fn direct_spawn_troll_can_followup_task_two_named_orc_children() {
    let (mut session, mut turn) = make_session_and_context().await;
    let manager = thread_manager();
    let mut config = (*turn.config).clone();
    config
        .features
        .enable(Feature::MultiAgentV2)
        .expect("test config should allow feature update");
    set_turn_config(&mut turn, config);

    let root = manager
        .start_thread((*turn.config).clone())
        .await
        .expect("root thread should start");
    let troll_path = AgentPath::root()
        .join("troll_burzum")
        .expect("test troll path should be valid");
    let first_orc_path = troll_path
        .join("orc_snaga")
        .expect("test first orc path should be valid");
    let second_orc_path = troll_path
        .join("orc_ghash")
        .expect("test second orc path should be valid");

    let troll = manager
        .start_thread_with_options(StartThreadOptions {
            config: (*turn.config).clone(),
            allow_provider_model_fallback: false,
            history_mode: None,
            initial_history: InitialHistory::New,
            session_source: Some(SessionSource::SubAgent(SubAgentSource::ThreadSpawn {
                parent_thread_id: root.thread_id,
                depth: 1,
                agent_path: Some(troll_path.clone()),
                agent_nickname: Some("Burzum".to_string()),
                agent_role: Some("troll".to_string()),
                agent_class: None,
            })),
            thread_source: Some(ThreadSource::Subagent),
            dynamic_tools: Vec::new(),
            metrics_service_name: None,
            parent_trace: None,
            environments: Some(Vec::new()),
            thread_extension_init: ExtensionDataInit::default(),
            supports_openai_form_elicitation: false,
        })
        .await
        .expect("direct troll pane should start");

    let first_orc = manager
        .start_thread_with_options(StartThreadOptions {
            config: (*turn.config).clone(),
            allow_provider_model_fallback: false,
            history_mode: None,
            initial_history: InitialHistory::New,
            session_source: Some(SessionSource::SubAgent(SubAgentSource::ThreadSpawn {
                parent_thread_id: troll.thread_id,
                depth: 2,
                agent_path: Some(first_orc_path.clone()),
                agent_nickname: Some("Snaga".to_string()),
                agent_role: Some("orc".to_string()),
                agent_class: None,
            })),
            thread_source: Some(ThreadSource::Subagent),
            dynamic_tools: Vec::new(),
            metrics_service_name: None,
            parent_trace: None,
            environments: Some(Vec::new()),
            thread_extension_init: ExtensionDataInit::default(),
            supports_openai_form_elicitation: false,
        })
        .await
        .expect("direct first orc pane should start");

    let second_orc = manager
        .start_thread_with_options(StartThreadOptions {
            config: (*turn.config).clone(),
            allow_provider_model_fallback: false,
            history_mode: None,
            initial_history: InitialHistory::New,
            session_source: Some(SessionSource::SubAgent(SubAgentSource::ThreadSpawn {
                parent_thread_id: troll.thread_id,
                depth: 2,
                agent_path: Some(second_orc_path.clone()),
                agent_nickname: Some("Ghash".to_string()),
                agent_role: Some("orc".to_string()),
                agent_class: None,
            })),
            thread_source: Some(ThreadSource::Subagent),
            dynamic_tools: Vec::new(),
            metrics_service_name: None,
            parent_trace: None,
            environments: Some(Vec::new()),
            thread_extension_init: ExtensionDataInit::default(),
            supports_openai_form_elicitation: false,
        })
        .await
        .expect("direct second orc pane should start");

    let troll_thread = manager
        .get_thread(troll.thread_id)
        .await
        .expect("troll thread should exist");
    let troll_snapshot = troll_thread.config_snapshot().await;
    session.services.agent_control = troll_thread.codex.session.services.agent_control.clone();
    session.thread_id = troll.thread_id;
    turn.session_source = troll_snapshot.session_source;
    let session = Arc::new(session);
    let turn = Arc::new(turn);
    let first_delegated_message = "Build the animation shell for the crypto formula demo.";
    let second_delegated_message = "Write the cryptographic formula copy and adversarial checks.";

    let first_output = FollowupTaskHandlerV2
        .handle(invocation(
            session.clone(),
            turn.clone(),
            "followup_task",
            function_payload(json!({
                "target": "orc_snaga",
                "message": first_delegated_message
            })),
        ))
        .await
        .expect("direct-spawn Troll should be able to target first named Orc child");
    let (content, success) = expect_text_output(first_output);
    assert_eq!(success, Some(true));
    let first_delivery: MessageToolResult =
        serde_json::from_str(&content).expect("first followup_task result should parse");
    assert_eq!(
        first_delivery.target_thread_id,
        first_orc.thread_id.to_string()
    );
    assert_eq!(first_delivery.agent_path, first_orc_path.as_str());
    assert_eq!(first_delivery.agent_nickname.as_deref(), Some("Snaga"));
    assert_eq!(first_delivery.agent_role.as_deref(), Some("orc"));
    assert_eq!(first_delivery.delivery, "followup_task_sent");
    assert!(first_delivery.triggered_turn);

    let second_output = FollowupTaskHandlerV2
        .handle(invocation(
            session.clone(),
            turn.clone(),
            "followup_task",
            function_payload(json!({
                "target": "orc_ghash",
                "message": second_delegated_message
            })),
        ))
        .await
        .expect("direct-spawn Troll should be able to target second named Orc child");
    let (content, success) = expect_text_output(second_output);
    assert_eq!(success, Some(true));
    let second_delivery: MessageToolResult =
        serde_json::from_str(&content).expect("second followup_task result should parse");
    assert_eq!(
        second_delivery.target_thread_id,
        second_orc.thread_id.to_string()
    );
    assert_eq!(second_delivery.agent_path, second_orc_path.as_str());
    assert_eq!(second_delivery.agent_nickname.as_deref(), Some("Ghash"));
    assert_eq!(second_delivery.agent_role.as_deref(), Some("orc"));
    assert_eq!(second_delivery.delivery, "followup_task_sent");
    assert!(second_delivery.triggered_turn);

    assert!(manager.captured_ops().iter().any(|(id, op)| {
        *id == first_orc.thread_id
            && matches!(
                op,
                Op::InterAgentCommunication { communication }
                    if communication.author == troll_path
                        && communication.recipient == first_orc_path
                        && communication.other_recipients.is_empty()
                        && communication.content.is_empty()
                        && communication.encrypted_content.as_deref()
                            == Some(first_delegated_message)
                        && communication
                            .metadata
                            .as_ref()
                            .and_then(|metadata| metadata.source_call_id.as_deref())
                            == Some("call-1")
                        && communication.trigger_turn
            )
    }));
    assert!(manager.captured_ops().iter().any(|(id, op)| {
        *id == second_orc.thread_id
            && matches!(
                op,
                Op::InterAgentCommunication { communication }
                    if communication.author == troll_path
                        && communication.recipient == second_orc_path
                        && communication.other_recipients.is_empty()
                        && communication.content.is_empty()
                        && communication.encrypted_content.as_deref()
                            == Some(second_delegated_message)
                        && communication
                            .metadata
                            .as_ref()
                            .and_then(|metadata| metadata.source_call_id.as_deref())
                            == Some("call-1")
                        && communication.trigger_turn
            )
    }));

    let captured_ops_before_capacity_queue = manager.captured_ops().len();
    let max_threads = turn
        .config
        .effective_agent_max_threads(MultiAgentVersion::V2)
        .expect("MultiAgentV2 should have a bounded turn capacity");
    let execution_guards = (0..max_threads)
        .map(|_| {
            session
                .services
                .agent_control
                .execution_guard(MultiAgentVersion::V2, &turn.session_source)
                .expect("a Troll turn should consume MultiAgentV2 capacity")
        })
        .collect::<Vec<_>>();
    let capacity_output = FollowupTaskHandlerV2
        .handle(invocation(
            session.clone(),
            turn.clone(),
            "followup_task",
            function_payload(json!({
                "target": "orc_snaga",
                "message": "Start another task while every execution slot is occupied."
            })),
        ))
        .await
        .expect("capacity pressure should not reject mailbox admission");
    let (capacity_message, success) = expect_text_output(capacity_output);
    assert_eq!(success, Some(true));
    let queued_delivery: MessageToolResult =
        serde_json::from_str(&capacity_message).expect("queued delivery result");
    assert_eq!(queued_delivery.delivery, "followup_task_sent");
    assert!(queued_delivery.triggered_turn);
    assert_eq!(
        manager.captured_ops().len(),
        captured_ops_before_capacity_queue + 1
    );
    drop(execution_guards);

    session.services.agent_control.record_agent_result_status(
        first_orc.thread_id,
        &AgentStatus::Completed(Some("animation shell complete".to_string())),
    );
    session.services.agent_control.record_agent_result_status(
        second_orc.thread_id,
        &AgentStatus::Completed(Some("formula checks complete".to_string())),
    );

    let output = ListAgentsHandlerV2
        .handle(invocation(
            session.clone(),
            turn.clone(),
            "list_agents",
            function_payload(json!({})),
        ))
        .await
        .expect("Troll should be able to list managed Orc status");
    let (content, success) = expect_text_output(output);
    assert_eq!(success, Some(true));
    let result: ListAgentsResult =
        serde_json::from_str(&content).expect("list_agents result should parse");
    assert!(result.agents.iter().any(|agent| {
        agent.agent_name == first_orc_path.as_str()
            && agent.agent_nickname.as_deref() == Some("Snaga")
            && agent.agent_role.as_deref() == Some("orc")
            && agent.last_task_message.is_none()
            && agent.last_result_message.as_deref() == Some("animation shell complete")
    }));
    assert!(result.agents.iter().any(|agent| {
        agent.agent_name == second_orc_path.as_str()
            && agent.agent_nickname.as_deref() == Some("Ghash")
            && agent.agent_role.as_deref() == Some("orc")
            && agent.last_task_message.is_none()
            && agent.last_result_message.as_deref() == Some("formula checks complete")
    }));
    let context = session
        .services
        .agent_control
        .format_environment_context_subagents(troll.thread_id)
        .await;
    assert!(
        context.contains("orc_snaga: Snaga"),
        "Troll context should name the first Orc, got {context:?}"
    );
    assert!(!context.contains("Start another task while every execution slot is occupied."));
    assert!(
        context.contains("animation shell complete"),
        "Troll context should expose the first Orc result, got {context:?}"
    );
    assert!(
        context.contains("orc_ghash: Ghash"),
        "Troll context should name the second Orc, got {context:?}"
    );
    assert!(!context.contains(second_delegated_message));
    assert!(
        context.contains("formula checks complete"),
        "Troll context should expose the second Orc result, got {context:?}"
    );

    let first_improvement_message =
        "Improve the animation shell with concrete file paths and reduced-motion handling.";
    let second_improvement_message =
        "Improve the formula review with adversarial edge cases and verification evidence.";
    FollowupTaskHandlerV2
        .handle(invocation(
            session.clone(),
            turn.clone(),
            "followup_task",
            function_payload(json!({
                "target": "Snaga [orc]",
                "message": first_improvement_message
            })),
        ))
        .await
        .expect("Troll should be able to force rework from first named Orc");
    FollowupTaskHandlerV2
        .handle(invocation(
            session.clone(),
            turn.clone(),
            "followup_task",
            function_payload(json!({
                "target": second_orc.thread_id.to_string(),
                "message": second_improvement_message
            })),
        ))
        .await
        .expect("Troll should be able to force rework from second named Orc");

    assert!(manager.captured_ops().iter().any(|(id, op)| {
        *id == first_orc.thread_id
            && matches!(
                op,
                Op::InterAgentCommunication { communication }
                    if communication.author == troll_path
                        && communication.recipient == first_orc_path
                        && communication.content.is_empty()
                        && communication.encrypted_content.as_deref()
                            == Some(first_improvement_message)
                        && communication.trigger_turn
            )
    }));
    assert!(manager.captured_ops().iter().any(|(id, op)| {
        *id == second_orc.thread_id
            && matches!(
                op,
                Op::InterAgentCommunication { communication }
                    if communication.author == troll_path
                        && communication.recipient == second_orc_path
                        && communication.content.is_empty()
                        && communication.encrypted_content.as_deref()
                            == Some(second_improvement_message)
                        && communication.trigger_turn
            )
    }));

    let output = ListAgentsHandlerV2
        .handle(invocation(
            session.clone(),
            turn.clone(),
            "list_agents",
            function_payload(json!({})),
        ))
        .await
        .expect("Troll should be able to inspect post-review Orc state");
    let (content, success) = expect_text_output(output);
    assert_eq!(success, Some(true));
    let result: ListAgentsResult =
        serde_json::from_str(&content).expect("post-review list_agents result should parse");
    assert!(result.agents.iter().any(|agent| {
        agent.agent_name == first_orc_path.as_str()
            && agent.agent_nickname.as_deref() == Some("Snaga")
            && agent.last_task_message.is_none()
            && agent.last_result_message.as_deref() == Some("animation shell complete")
    }));
    assert!(result.agents.iter().any(|agent| {
        agent.agent_name == second_orc_path.as_str()
            && agent.agent_nickname.as_deref() == Some("Ghash")
            && agent.last_task_message.is_none()
            && agent.last_result_message.as_deref() == Some("formula checks complete")
    }));
    let context = session
        .services
        .agent_control
        .format_environment_context_subagents(troll.thread_id)
        .await;
    assert!(!context.contains(first_improvement_message));
    assert!(!context.contains(second_improvement_message));
    assert!(
        context.contains("animation shell complete") && context.contains("formula checks complete"),
        "results should remain readable without exposing encrypted task text, got {context:?}"
    );
}

#[tokio::test]
async fn multi_agent_v2_followup_task_rejects_legacy_items_field() {
    let (mut session, mut turn) = make_session_and_context().await;
    let manager = thread_manager();
    let root = manager
        .start_thread(StartThreadOptions::new((*turn.config).clone()))
        .await
        .expect("root thread should start");
    session.services.agent_control = manager.agent_control();
    session.thread_id = root.thread_id;
    let mut config = turn.config.as_ref().clone();
    let _ = config.features.enable(Feature::MultiAgentV2);
    set_turn_config(&mut turn, config);
    let session = Arc::new(session);
    let turn = Arc::new(turn);

    SpawnAgentHandlerV2::default()
        .handle(invocation(
            session.clone(),
            turn.clone(),
            "spawn_agent",
            function_payload(json!({
                "message": "boot worker",
                "task_name": "worker"
            })),
        ))
        .await
        .expect("spawn worker");
    let agent_id = session
        .services
        .agent_control
        .resolve_agent_reference(session.thread_id, &turn.session_source, "worker")
        .await
        .expect("worker should resolve");
    let invocation = invocation(
        session,
        turn,
        "followup_task",
        function_payload(json!({
            "target": agent_id.to_string(),
            "items": [{"type": "text", "text": "continue"}],
        })),
    );

    let Err(err) = FollowupTaskHandlerV2.handle(invocation).await else {
        panic!("legacy items field should be rejected in v2");
    };
    let Some(message) = model_facing_error_message(&err) else {
        panic!("legacy items field should surface as a model-facing error");
    };
    assert!(message.contains("unknown field `items`"));
}

#[tokio::test]
async fn multi_agent_v2_interrupted_turn_does_not_notify_parent() {
    let (mut session, mut turn) = make_session_and_context().await;
    let manager = thread_manager();
    let root = manager
        .start_thread(StartThreadOptions::new((*turn.config).clone()))
        .await
        .expect("root thread should start");
    session.services.agent_control = manager.agent_control();
    session.thread_id = root.thread_id;
    let mut config = turn.config.as_ref().clone();
    let _ = config.features.enable(Feature::MultiAgentV2);
    set_turn_config(&mut turn, config);
    let session = Arc::new(session);
    let turn = Arc::new(turn);

    SpawnAgentHandlerV2::default()
        .handle(invocation(
            session.clone(),
            turn.clone(),
            "spawn_agent",
            function_payload(json!({
                "message": "boot worker",
                "task_name": "worker"
            })),
        ))
        .await
        .expect("spawn worker");
    let agent_id = session
        .services
        .agent_control
        .resolve_agent_reference(session.thread_id, &turn.session_source, "worker")
        .await
        .expect("worker should resolve");
    let thread = manager
        .get_thread(agent_id)
        .await
        .expect("worker thread should exist");

    let aborted_turn = thread.session.new_default_turn().await;
    thread
        .session
        .send_event(
            aborted_turn.as_ref(),
            EventMsg::TurnAborted(TurnAbortedEvent {
                turn_id: Some(aborted_turn.sub_id.clone()),
                started_at: None,
                reason: TurnAbortReason::Interrupted,
                completed_at: None,
                duration_ms: None,
            }),
        )
        .await;

    let notifications = manager
        .captured_ops()
        .into_iter()
        .filter_map(|(id, op)| {
            (id == root.thread_id)
                .then_some(op)
                .and_then(|op| match op {
                    Op::InterAgentCommunication { communication }
                        if communication.author.as_str() == "/root/worker"
                            && communication.recipient == AgentPath::root()
                            && communication.other_recipients.is_empty()
                            && !communication.trigger_turn =>
                    {
                        Some(communication.content)
                    }
                    _ => None,
                })
        })
        .collect::<Vec<_>>();

    assert_eq!(notifications, Vec::<String>::new());
}

#[tokio::test]
async fn multi_agent_v2_spawn_omits_agent_id_when_named() {
    let (mut session, mut turn) = make_session_and_context().await;
    let expected_model_provider = turn.config.model_provider_id.clone();
    let expected_model = turn.model_info.slug.clone();
    let manager = thread_manager();
    let root = manager
        .start_thread(StartThreadOptions::new((*turn.config).clone()))
        .await
        .expect("root thread should start");
    session.services.agent_control = manager.agent_control();
    session.thread_id = root.thread_id;
    let mut config = (*turn.config).clone();
    config
        .features
        .enable(Feature::MultiAgentV2)
        .expect("test config should allow feature update");
    set_turn_config(&mut turn, config);

    let output = SpawnAgentHandlerV2::default()
        .handle(invocation(
            Arc::new(session),
            Arc::new(turn),
            "spawn_agent",
            function_payload(json!({
                "message": "inspect this repo",
                "task_name": "test_process"
            })),
        ))
        .await
        .expect("spawn_agent should succeed");
    let (content, success) = expect_text_output(output);
    let result: serde_json::Value =
        serde_json::from_str(&content).expect("spawn_agent result should be json");
    assert_eq!(result["model_provider"], expected_model_provider);
    assert_eq!(result["model"], expected_model);

    assert!(result.get("agent_id").is_none());
    assert_eq!(result["task_name"], "/root/test_process");
    assert!(
        result
            .get("nickname")
            .and_then(serde_json::Value::as_str)
            .is_some_and(|nickname| !nickname.is_empty())
    );
    assert_eq!(success, Some(true));
}

#[tokio::test]
async fn multi_agent_v2_spawn_surfaces_task_name_validation_errors() {
    let (mut session, mut turn) = make_session_and_context().await;
    let manager = thread_manager();
    let root = manager
        .start_thread(StartThreadOptions::new((*turn.config).clone()))
        .await
        .expect("root thread should start");
    session.services.agent_control = manager.agent_control();
    session.thread_id = root.thread_id;
    let mut config = (*turn.config).clone();
    config
        .features
        .enable(Feature::MultiAgentV2)
        .expect("test config should allow feature update");
    set_turn_config(&mut turn, config);

    let invocation = invocation(
        Arc::new(session),
        Arc::new(turn),
        "spawn_agent",
        function_payload(json!({
            "message": "inspect this repo",
            "task_name": "BadName"
        })),
    );
    let Err(err) = SpawnAgentHandlerV2::default().handle(invocation).await else {
        panic!("invalid agent name should be rejected");
    };
    assert_eq!(
        err,
        FunctionCallError::RespondToModel(
            "agent_name must use only lowercase letters, digits, and underscores".to_string()
        )
    );
}

#[tokio::test]
async fn spawn_agent_reapplies_runtime_sandbox_after_role_config() {
    #[derive(Debug, Deserialize)]
    struct SpawnAgentResult {
        agent_id: String,
        nickname: Option<String>,
    }

    let (mut session, turn) = make_session_and_context().await;
    let mut turn = with_spawn_eligible_runtime(&session, turn).await;
    let expected_sandbox = turn.config.legacy_sandbox_policy();
    #[allow(deprecated)]
    let mut expected_file_system_sandbox_policy =
        FileSystemSandboxPolicy::from_legacy_sandbox_policy_for_cwd(&expected_sandbox, &turn.cwd);
    expected_file_system_sandbox_policy
        .entries
        .push(FileSystemSandboxEntry {
            path: FileSystemPath::GlobPattern {
                scan: None,
                pattern: "**/.env".to_string(),
            },
            access: FileSystemAccessMode::Deny,
            missing_path_behavior: None,
        });
    let expected_network_sandbox_policy = NetworkSandboxPolicy::from(&expected_sandbox);
    let expected_permission_profile = PermissionProfile::from_runtime_permissions_with_enforcement(
        SandboxEnforcement::from_legacy_sandbox_policy(&expected_sandbox),
        &expected_file_system_sandbox_policy,
        expected_network_sandbox_policy,
    );
    turn.approval_policy
        .set(AskForApproval::OnRequest)
        .expect("approval policy should be set");
    let mut config = (*turn.config).clone();
    config.approvals_reviewer = ApprovalsReviewer::AutoReview;
    set_turn_config(&mut turn, config);
    turn.permission_profile = expected_permission_profile.clone();
    assert_ne!(
        expected_permission_profile,
        turn.config.permissions.effective_permission_profile(),
        "test requires a runtime profile override that differs from base config"
    );
    let manager = install_live_root(&mut session, &turn).await;

    let invocation = invocation(
        Arc::new(session),
        Arc::new(turn),
        "spawn_agent",
        function_payload(json!({
            "message": "await this command",
            "agent_type": "explorer"
        })),
    );
    let output = SpawnAgentHandler::default()
        .handle(invocation)
        .await
        .expect("spawn_agent should succeed");
    let (content, _) = expect_text_output(output);
    let result: SpawnAgentResult =
        serde_json::from_str(&content).expect("spawn_agent result should be json");
    let agent_id = parse_agent_id(&result.agent_id);
    assert!(
        result
            .nickname
            .as_deref()
            .is_some_and(|nickname| !nickname.is_empty())
    );

    let snapshot = manager
        .get_thread(agent_id)
        .await
        .expect("spawned agent thread should exist")
        .config_snapshot()
        .await;
    assert_eq!(snapshot.sandbox_policy(), expected_sandbox);
    assert_eq!(snapshot.approval_policy, AskForApproval::OnRequest);
    assert_eq!(snapshot.approvals_reviewer, ApprovalsReviewer::AutoReview);
    assert_eq!(snapshot.permission_profile, expected_permission_profile);
    let child_thread = manager
        .get_thread(agent_id)
        .await
        .expect("spawned agent thread should exist");
    let child_turn = child_thread.session.new_default_turn().await;
    assert_eq!(
        child_turn.file_system_sandbox_policy(),
        expected_file_system_sandbox_policy
    );
    assert_eq!(
        child_turn.network_sandbox_policy(),
        expected_network_sandbox_policy
    );
    assert_eq!(child_turn.permission_profile(), expected_permission_profile);
}

#[tokio::test]
async fn spawn_agent_rejects_when_depth_limit_exceeded() {
    let (mut session, mut turn) = make_session_and_context().await;
    let manager = thread_manager();
    session.services.agent_control = manager.agent_control();

    let max_depth = turn.config.agent_max_depth;
    turn.session_source = SessionSource::SubAgent(SubAgentSource::ThreadSpawn {
        parent_thread_id: session.thread_id,
        depth: max_depth,
        agent_path: None,
        agent_nickname: None,
        agent_role: None,
        agent_class: None,
    });

    let invocation = invocation(
        Arc::new(session),
        Arc::new(turn),
        "spawn_agent",
        function_payload(json!({"message": "hello"})),
    );
    let Err(err) = SpawnAgentHandler::default().handle(invocation).await else {
        panic!("spawn should fail when depth limit exceeded");
    };
    assert_eq!(
        err,
        FunctionCallError::RespondToModel(
            "Agent depth limit reached. Solve the task yourself.".to_string()
        )
    );
}

#[tokio::test]
async fn spawn_agent_allows_depth_up_to_configured_max_depth() {
    #[derive(Debug, Deserialize)]
    struct SpawnAgentResult {
        agent_id: String,
        nickname: Option<String>,
    }

    let (mut session, turn) = make_session_and_context().await;
    let mut turn = with_spawn_eligible_runtime(&session, turn).await;
    let mut config = (*turn.config).clone();
    config.agent_max_depth = DEFAULT_AGENT_MAX_DEPTH + 1;
    turn.config = Arc::new(config);
    let manager = thread_manager();
    let root = manager
        .start_thread((*turn.config).clone())
        .await
        .expect("root thread should start");
    let parent_source = SessionSource::SubAgent(SubAgentSource::ThreadSpawn {
        parent_thread_id: root.thread_id,
        depth: DEFAULT_AGENT_MAX_DEPTH,
        agent_path: None,
        agent_nickname: None,
        agent_role: None,
        agent_class: None,
    });
    let parent = manager
        .start_thread_with_options(StartThreadOptions {
            config: (*turn.config).clone(),
            allow_provider_model_fallback: false,
            history_mode: None,
            initial_history: InitialHistory::New,
            session_source: Some(parent_source.clone()),
            thread_source: Some(ThreadSource::Subagent),
            dynamic_tools: Vec::new(),
            metrics_service_name: None,
            parent_trace: None,
            environments: Some(Vec::new()),
            thread_extension_init: ExtensionDataInit::default(),
            supports_openai_form_elicitation: false,
        })
        .await
        .expect("live parent at the configured depth should start");
    // Starting a sub-agent thread directly binds it to its live parent's policy,
    // the same binding a live spawn receives.
    root.thread
        .session
        .services
        .agent_control
        .effective_security_policy()
        .snapshot_for_agent(parent.thread_id)
        .expect("direct fixture parent should receive the same policy binding as a live spawn");
    session.services.agent_control = manager.agent_control();
    session.thread_id = parent.thread_id;
    turn.session_source = parent_source;

    let invocation = invocation(
        Arc::new(session),
        Arc::new(turn),
        "spawn_agent",
        function_payload(json!({"message": "hello"})),
    );
    let output = SpawnAgentHandler::default()
        .handle(invocation)
        .await
        .expect("spawn should succeed within configured depth");
    let (content, success) = expect_text_output(output);
    let result: SpawnAgentResult =
        serde_json::from_str(&content).expect("spawn_agent result should be json");
    assert!(!result.agent_id.is_empty());
    assert!(
        result
            .nickname
            .as_deref()
            .is_some_and(|nickname| !nickname.is_empty())
    );
    assert_eq!(success, Some(true));
}

#[tokio::test]
async fn spawn_agent_allows_root_to_spawn_troll() {
    #[derive(Debug, Deserialize)]
    struct SpawnAgentResult {
        agent_id: String,
        nickname: Option<String>,
    }

    let (mut session, turn) = make_session_and_context().await;
    let turn = with_spawn_eligible_runtime(&session, turn).await;
    let config = (*turn.config).clone();
    let expected_model = turn.model_info.slug.clone();
    let expected_provider = turn.config.model_provider_id.clone();
    let manager = install_live_root(&mut session, &turn).await;

    let invocation = invocation(
        Arc::new(session),
        Arc::new(turn),
        "spawn_agent",
        function_payload(json!({
            "message": "supervise this implementation",
            "agent_type": "troll"
        })),
    );
    let output = SpawnAgentHandler::default()
        .handle(invocation)
        .await
        .expect("root should spawn troll");
    let (content, success) = expect_text_output(output);
    let result: SpawnAgentResult =
        serde_json::from_str(&content).expect("spawn_agent result should be json");
    assert_nickname_from_role_pool(&config, result.nickname.as_deref(), "troll");
    let agent_id = parse_agent_id(&result.agent_id);
    let snapshot = manager
        .get_thread(agent_id)
        .await
        .expect("spawned troll should exist")
        .config_snapshot()
        .await;
    assert_eq!(
        snapshot.session_source.get_agent_role().as_deref(),
        Some("troll")
    );
    assert_eq!(snapshot.model, expected_model);
    assert_eq!(snapshot.model_provider_id, expected_provider);
    assert_eq!(snapshot.session_source.get_nickname(), result.nickname);
    assert_eq!(success, Some(true));
}

#[test]
fn spawn_agent_allows_troll_to_spawn_orc() {
    std::thread::Builder::new()
        .name("spawn_agent_allows_troll_to_spawn_orc".to_string())
        .stack_size(64 * 1024 * 1024)
        .spawn(|| {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("test runtime should build");
            runtime.block_on(async {
                #[derive(Debug, Deserialize)]
                struct SpawnAgentResult {
                    agent_id: String,
                    nickname: Option<String>,
                }

                let (mut session, turn) = make_session_and_context().await;
                let mut turn = with_spawn_eligible_runtime(&session, turn).await;
                let mut turn_config = (*turn.config).clone();
                turn_config.agent_max_depth = 2;
                turn.config = Arc::new(turn_config);
                let config = (*turn.config).clone();
                let expected_model = turn.model_info.slug.clone();
                let expected_provider = turn.config.model_provider_id.clone();
                let manager = install_live_root(&mut session, &turn).await;

                let troll_output = SpawnAgentHandler::default()
                    .handle(invocation(
                        Arc::new(session),
                        Arc::new(turn),
                        "spawn_agent",
                        function_payload(json!({
                            "message": "supervise this implementation",
                            "agent_type": "troll"
                        })),
                    ))
                    .await
                    .expect("root should spawn troll");
                let (content, _) = expect_text_output(troll_output);
                let troll_result: SpawnAgentResult =
                    serde_json::from_str(&content).expect("spawn_agent result should be json");
                assert_nickname_from_role_pool(&config, troll_result.nickname.as_deref(), "troll");
                let troll_id = parse_agent_id(&troll_result.agent_id);
                let troll_snapshot = manager
                    .get_thread(troll_id)
                    .await
                    .expect("spawned troll should exist")
                    .config_snapshot()
                    .await;

                assert_eq!(
                    troll_snapshot.session_source.get_agent_role().as_deref(),
                    Some("troll")
                );
                assert_eq!(troll_snapshot.model, expected_model);
                assert_eq!(troll_snapshot.model_provider_id, expected_provider);
                assert_eq!(
                    troll_snapshot.session_source.get_nickname(),
                    troll_result.nickname
                );
                let troll_thread = manager
                    .get_thread(troll_id)
                    .await
                    .expect("spawned troll should exist");
                let troll_turn = troll_thread.codex.session.new_default_turn().await;
                let first_orc_output = SpawnAgentHandler::default()
                    .handle(invocation(
                        Arc::clone(&troll_thread.codex.session),
                        troll_turn,
                        "spawn_agent",
                        function_payload(json!({
                            "message": "implement the assigned fix and report evidence",
                            "agent_type": "orc"
                        })),
                    ))
                    .await
                    .expect("troll should spawn first orc");
                let (content, success) = expect_text_output(first_orc_output);
                let first_orc_result: SpawnAgentResult =
                    serde_json::from_str(&content).expect("spawn_agent result should be json");
                assert_nickname_from_role_pool(
                    &config,
                    first_orc_result.nickname.as_deref(),
                    "orc",
                );
                let first_orc_id = parse_agent_id(&first_orc_result.agent_id);
                let first_orc_snapshot = manager
                    .get_thread(first_orc_id)
                    .await
                    .expect("spawned orc should exist")
                    .config_snapshot()
                    .await;
                assert_eq!(
                    first_orc_snapshot
                        .session_source
                        .get_agent_role()
                        .as_deref(),
                    Some("orc")
                );
                assert_eq!(first_orc_snapshot.model, expected_model);
                assert_eq!(first_orc_snapshot.model_provider_id, expected_provider);
                assert_eq!(success, Some(true));

                let troll_turn = troll_thread.codex.session.new_default_turn().await;
                let second_orc_output = SpawnAgentHandler::default()
                    .handle(invocation(
                        Arc::clone(&troll_thread.codex.session),
                        troll_turn,
                        "spawn_agent",
                        function_payload(json!({
                            "message": "implement the second assigned fix and report evidence",
                            "agent_type": "orc"
                        })),
                    ))
                    .await
                    .expect("troll should spawn second orc");
                let (content, success) = expect_text_output(second_orc_output);
                let second_orc_result: SpawnAgentResult =
                    serde_json::from_str(&content).expect("spawn_agent result should be json");
                assert_nickname_from_role_pool(
                    &config,
                    second_orc_result.nickname.as_deref(),
                    "orc",
                );
                assert_ne!(first_orc_result.nickname, second_orc_result.nickname);
                let second_orc_id = parse_agent_id(&second_orc_result.agent_id);
                let second_orc_snapshot = manager
                    .get_thread(second_orc_id)
                    .await
                    .expect("second spawned orc should exist")
                    .config_snapshot()
                    .await;
                assert_eq!(
                    second_orc_snapshot
                        .session_source
                        .get_agent_role()
                        .as_deref(),
                    Some("orc")
                );
                assert_eq!(second_orc_snapshot.model, expected_model);
                assert_eq!(second_orc_snapshot.model_provider_id, expected_provider);
                assert_eq!(success, Some(true));
            });
        })
        .expect("test thread should spawn")
        .join()
        .expect("test thread should finish");
}

#[tokio::test]
async fn multi_agent_v2_spawn_agent_ignores_configured_max_depth() {
    #[derive(Debug, Deserialize)]
    struct SpawnAgentResult {
        task_name: String,
        nickname: Option<String>,
    }

    let (mut session, mut turn) = make_session_and_context().await;
    let manager = thread_manager();
    let mut config = (*turn.config).clone();
    config.agent_max_depth = 1;
    config
        .features
        .enable(Feature::MultiAgentV2)
        .expect("test config should allow feature update");
    let root = manager
        .start_thread(StartThreadOptions::new(config.clone()))
        .await
        .expect("root thread should start");
    let control = manager.agent_control();
    let parent_path = AgentPath::try_from("/root/parent").expect("agent path");
    let parent = control
        .spawn_agent_with_metadata(
            config.clone(),
            vec![UserInput::Text {
                text: "parent task".to_string(),
                text_elements: Vec::new(),
            }],
            Some(SessionSource::SubAgent(SubAgentSource::ThreadSpawn {
                parent_thread_id: root.thread_id,
                depth: 1,
                agent_path: Some(parent_path.clone()),
                agent_nickname: None,
                agent_role: None,
                agent_class: None,
            })),
            crate::agent::control::SpawnAgentOptions {
                parent_thread_id: Some(root.thread_id),
                ..Default::default()
            },
        )
        .await
        .expect("parent worker should spawn");
    session.services.agent_control = control;
    session.thread_id = parent.thread_id;
    set_turn_config(&mut turn, config);
    turn.session_source = SessionSource::SubAgent(SubAgentSource::ThreadSpawn {
        parent_thread_id: root.thread_id,
        depth: 1,
        agent_path: Some(parent_path),
        agent_nickname: None,
        agent_role: None,
        agent_class: None,
    });

    let invocation = invocation(
        Arc::new(session),
        Arc::new(turn),
        "spawn_agent",
        function_payload(json!({
            "message": "hello",
            "task_name": "child",
            "fork_turns": "none"
        })),
    );
    let output = SpawnAgentHandlerV2::default()
        .handle(invocation)
        .await
        .expect("multi-agent v2 spawn should ignore max depth");
    let (content, success) = expect_text_output(output);
    let result: SpawnAgentResult =
        serde_json::from_str(&content).expect("spawn_agent result should be json");
    assert_eq!(result.task_name, "/root/parent/child");
    assert!(
        result
            .nickname
            .as_deref()
            .is_some_and(|nickname| !nickname.is_empty())
    );
    assert_eq!(success, Some(true));
}

#[tokio::test]
async fn send_input_rejects_empty_message() {
    let (session, turn) = make_session_and_context().await;
    let invocation = invocation(
        Arc::new(session),
        Arc::new(turn),
        "send_input",
        function_payload(json!({"target": ThreadId::new().to_string(), "message": ""})),
    );
    let Err(err) = SendInputHandler.handle(invocation).await else {
        panic!("empty message should be rejected");
    };
    assert_eq!(
        err,
        FunctionCallError::RespondToModel("Empty message can't be sent to an agent".to_string())
    );
}

#[tokio::test]
async fn send_input_rejects_when_message_and_items_are_both_set() {
    let (session, turn) = make_session_and_context().await;
    let invocation = invocation(
        Arc::new(session),
        Arc::new(turn),
        "send_input",
        function_payload(json!({
            "target": ThreadId::new().to_string(),
            "message": "hello",
            "items": [{"type": "mention", "name": "drive", "path": "app://drive"}]
        })),
    );
    let Err(err) = SendInputHandler.handle(invocation).await else {
        panic!("message+items should be rejected");
    };
    assert_eq!(
        err,
        FunctionCallError::RespondToModel(
            "Provide either message or items, but not both".to_string()
        )
    );
}

#[tokio::test]
async fn send_input_rejects_invalid_id() {
    let (session, turn) = make_session_and_context().await;
    let invocation = invocation(
        Arc::new(session),
        Arc::new(turn),
        "send_input",
        function_payload(json!({"target": "not-a-uuid", "message": "hi"})),
    );
    let Err(err) = SendInputHandler.handle(invocation).await else {
        panic!("invalid id should be rejected");
    };
    let FunctionCallError::RespondToModel(msg) = err else {
        panic!("expected respond-to-model error");
    };
    assert!(msg.starts_with("invalid agent id not-a-uuid:"));
}

#[tokio::test]
async fn send_input_reports_missing_agent() {
    let (mut session, turn) = make_session_and_context().await;
    let manager = thread_manager();
    session.services.agent_control = manager.agent_control();
    let agent_id = ThreadId::new();
    let invocation = invocation(
        Arc::new(session),
        Arc::new(turn),
        "send_input",
        function_payload(json!({"target": agent_id.to_string(), "message": "hi"})),
    );
    let Err(err) = SendInputHandler.handle(invocation).await else {
        panic!("missing agent should be reported");
    };
    assert_eq!(
        err,
        FunctionCallError::RespondToModel(format!("agent with id {agent_id} not found"))
    );
}

#[tokio::test]
async fn send_input_interrupts_before_prompt() {
    let (mut session, turn) = make_session_and_context().await;
    let manager = thread_manager();
    session.services.agent_control = manager.agent_control();
    let config = turn.config.as_ref().clone();
    let thread = manager
        .start_thread(StartThreadOptions::new(config.clone()))
        .await
        .expect("start thread");
    let agent_id = thread.thread_id;
    let invocation = invocation(
        Arc::new(session),
        Arc::new(turn),
        "send_input",
        function_payload(json!({
            "target": agent_id.to_string(),
            "message": "hi",
            "interrupt": true
        })),
    );
    SendInputHandler
        .handle(invocation)
        .await
        .expect("send_input should succeed");

    let ops = manager.captured_ops();
    let ops_for_agent: Vec<&Op> = ops
        .iter()
        .filter_map(|(id, op)| (*id == agent_id).then_some(op))
        .collect();
    assert_eq!(ops_for_agent.len(), 2);
    assert!(matches!(ops_for_agent[0], Op::Interrupt));
    assert!(matches!(ops_for_agent[1], Op::UserInput { .. }));

    let _ = thread
        .thread
        .submit(Op::Shutdown {})
        .await
        .expect("shutdown should submit");
}

#[tokio::test]
async fn send_input_accepts_structured_items() {
    let (mut session, turn) = make_session_and_context().await;
    let manager = thread_manager();
    session.services.agent_control = manager.agent_control();
    let config = turn.config.as_ref().clone();
    let thread = manager
        .start_thread(StartThreadOptions::new(config.clone()))
        .await
        .expect("start thread");
    let agent_id = thread.thread_id;
    let invocation = invocation(
        Arc::new(session),
        Arc::new(turn),
        "send_input",
        function_payload(json!({
            "target": agent_id.to_string(),
            "items": [
                {"type": "mention", "name": "drive", "path": "app://google_drive"},
                {"type": "text", "text": "read the folder"}
            ]
        })),
    );
    SendInputHandler
        .handle(invocation)
        .await
        .expect("send_input should succeed");

    let expected = Op::UserInput {
        items: vec![
            UserInput::Mention {
                name: "drive".to_string(),
                path: "app://google_drive".to_string(),
            },
            UserInput::Text {
                text: "read the folder".to_string(),
                text_elements: Vec::new(),
            },
        ],
        final_output_json_schema: None,
        responsesapi_client_metadata: None,
        additional_context: Default::default(),
        thread_settings: Default::default(),
    };
    let captured = manager
        .captured_ops()
        .into_iter()
        .find(|(id, op)| *id == agent_id && *op == expected);
    assert_eq!(captured, Some((agent_id, expected)));

    let _ = thread
        .thread
        .submit(Op::Shutdown {})
        .await
        .expect("shutdown should submit");
}

#[tokio::test]
async fn resume_agent_rejects_invalid_id() {
    let (session, turn) = make_session_and_context().await;
    let invocation = invocation(
        Arc::new(session),
        Arc::new(turn),
        "resume_agent",
        function_payload(json!({"id": "not-a-uuid"})),
    );
    let Err(err) = ResumeAgentHandler.handle(invocation).await else {
        panic!("invalid id should be rejected");
    };
    let FunctionCallError::RespondToModel(msg) = err else {
        panic!("expected respond-to-model error");
    };
    assert!(msg.starts_with("invalid agent id not-a-uuid:"));
}

#[tokio::test]
async fn resume_agent_reports_missing_agent() {
    let (mut session, turn) = make_session_and_context().await;
    let manager = thread_manager();
    session.services.agent_control = manager.agent_control();
    let agent_id = ThreadId::new();
    let invocation = invocation(
        Arc::new(session),
        Arc::new(turn),
        "resume_agent",
        function_payload(json!({"id": agent_id.to_string()})),
    );
    let Err(err) = ResumeAgentHandler.handle(invocation).await else {
        panic!("missing agent should be reported");
    };
    assert_eq!(
        err,
        FunctionCallError::RespondToModel(format!("agent with id {agent_id} not found"))
    );
}

#[tokio::test]
async fn resume_agent_noops_for_active_agent() {
    let (mut session, turn) = make_session_and_context().await;
    let manager = thread_manager();
    session.services.agent_control = manager.agent_control();
    let config = turn.config.as_ref().clone();
    let thread = manager
        .start_thread(StartThreadOptions::new(config.clone()))
        .await
        .expect("start thread");
    let agent_id = thread.thread_id;
    let status_before = manager.agent_control().get_status(agent_id).await;
    let invocation = invocation(
        Arc::new(session),
        Arc::new(turn),
        "resume_agent",
        function_payload(json!({"id": agent_id.to_string()})),
    );

    let output = ResumeAgentHandler
        .handle(invocation)
        .await
        .expect("resume_agent should succeed");
    let (content, success) = expect_text_output(output);
    let result: resume_agent::ResumeAgentResult =
        serde_json::from_str(&content).expect("resume_agent result should be json");
    assert_eq!(result.status, status_before);
    assert_eq!(success, Some(true));

    let thread_ids = manager.list_thread_ids().await;
    assert_eq!(thread_ids, vec![agent_id]);

    let _ = thread
        .thread
        .submit(Op::Shutdown {})
        .await
        .expect("shutdown should submit");
}

#[tokio::test]
async fn resume_agent_restores_closed_agent_and_accepts_send_input() {
    let (mut session, turn) = make_session_and_context().await;
    let manager = thread_manager();
    session.services.agent_control = manager.agent_control();
    let config = turn.config.as_ref().clone();
    let thread = manager
        .resume_thread_with_history(
            config.clone(),
            InitialHistory::Forked(vec![RolloutItem::ResponseItem(ResponseItem::Message {
                id: None,
                role: "user".to_string(),
                content: vec![ContentItem::InputText {
                    text: "materialized".to_string(),
                }],
                phase: None,
                internal_chat_message_metadata_passthrough: None,
            })]),
            AuthManager::from_auth_for_testing(CodexAuth::from_api_key("dummy")),
            /*parent_trace*/ None,
            /*supports_openai_form_elicitation*/ false,
        )
        .await
        .expect("start thread");
    let agent_id = thread.thread_id;
    let _ = manager
        .agent_control()
        .shutdown_live_agent(agent_id)
        .await
        .expect("shutdown agent");
    assert_eq!(
        manager.agent_control().get_status(agent_id).await,
        AgentStatus::NotFound
    );
    let session = Arc::new(session);
    let turn = Arc::new(turn);

    let resume_invocation = invocation(
        session.clone(),
        turn.clone(),
        "resume_agent",
        function_payload(json!({"id": agent_id.to_string()})),
    );
    let output = ResumeAgentHandler
        .handle(resume_invocation)
        .await
        .expect("resume_agent should succeed");
    let (content, success) = expect_text_output(output);
    let result: resume_agent::ResumeAgentResult =
        serde_json::from_str(&content).expect("resume_agent result should be json");
    assert_ne!(result.status, AgentStatus::NotFound);
    assert_eq!(success, Some(true));

    let send_invocation = invocation(
        session,
        turn,
        "send_input",
        function_payload(json!({"target": agent_id.to_string(), "message": "hello"})),
    );
    let output = SendInputHandler
        .handle(send_invocation)
        .await
        .expect("send_input should succeed after resume");
    let (content, success) = expect_text_output(output);
    let result: serde_json::Value =
        serde_json::from_str(&content).expect("send_input result should be json");
    let submission_id = result
        .get("submission_id")
        .and_then(|value| value.as_str())
        .unwrap_or_default();
    assert!(!submission_id.is_empty());
    assert_eq!(success, Some(true));

    let _ = manager
        .agent_control()
        .shutdown_live_agent(agent_id)
        .await
        .expect("shutdown resumed agent");
}

/// #1074: resuming a closed agent goes through the #689 check: on a
/// connection whose default key is nowhere while a sibling connection with
/// the same key variable runs on a named account, it is refused before it
/// starts; with a stored default key it resumes.
#[tokio::test]
async fn resume_on_a_connection_without_its_own_credential_is_refused() {
    const KEY: &str = "CORBANU_TEST_1074_ZAI_KEY";
    let (mut session, mut turn) = make_session_and_context().await;
    let manager = thread_manager();
    session.services.agent_control = manager.agent_control();
    let thread = manager
        .resume_thread_with_history(
            turn.config.as_ref().clone(),
            InitialHistory::Forked(vec![RolloutItem::ResponseItem(ResponseItem::Message {
                id: None,
                role: "user".to_string(),
                content: vec![ContentItem::InputText {
                    text: "materialized".to_string(),
                }],
                phase: None,
                internal_chat_message_metadata_passthrough: None,
            })]),
            AuthManager::from_auth_for_testing(CodexAuth::from_api_key("dummy")),
            /*parent_trace*/ None,
            /*supports_openai_form_elicitation*/ false,
        )
        .await
        .expect("start thread");
    let agent_id = thread.thread_id;
    let _ = manager
        .agent_control()
        .shutdown_live_agent(agent_id)
        .await
        .expect("shutdown agent");

    // The parent runs `zai-anthropic` on its default key; `zai`, which shares
    // the key variable, runs on account `work`.
    let mut config = (*turn.config).clone();
    let mut zai = ModelProviderInfo::create_zai_provider();
    zai.env_key = Some(KEY.to_string());
    crate::config::stamp_provider_account(ZAI_PROVIDER_ID, &mut zai, "work").expect("stamp");
    let mut anthropic = ModelProviderInfo::create_zai_anthropic_provider();
    anthropic.env_key = Some(KEY.to_string());
    config
        .model_providers
        .insert(ZAI_PROVIDER_ID.to_string(), zai);
    config
        .model_providers
        .insert("zai-anthropic".to_string(), anthropic.clone());
    config.model_provider_id = "zai-anthropic".to_string();
    config.model_provider = anthropic.clone();
    turn.provider = create_model_provider(anthropic, turn.auth_manager.clone());
    let codex_home = config.codex_home.clone();
    set_turn_config(&mut turn, config);
    let session = Arc::new(session);
    let turn = Arc::new(turn);
    let resume = || {
        ResumeAgentHandler.handle(invocation(
            session.clone(),
            turn.clone(),
            "resume_agent",
            function_payload(json!({"id": agent_id.to_string()})),
        ))
    };

    let Err(error) = resume().await else {
        panic!("resume_agent should be refused");
    };
    assert_eq!(
        error,
        FunctionCallError::RespondToModel(format!(
            "Resuming this agent: `zai-anthropic` would run on its default account, which has \
             no credential (`{KEY}` is not set and no default key is stored); accounts are \
             selected per connection, and account `work` is selected for `zai` only. Ask the \
             user to add a default `{KEY}` (`/providers`) before resuming it, or do the work \
             without this agent. Do not retry on another account without the user's consent."
        ))
    );
    assert_eq!(
        manager.agent_control().get_status(agent_id).await,
        AgentStatus::NotFound
    );

    // With a stored default key it resumes.
    codex_login::login_with_provider_api_key(
        &codex_home,
        KEY,
        "canary-1074-default",
        codex_login::AuthCredentialsStoreMode::File,
        codex_login::AuthKeyringBackendKind::default(),
    )
    .expect("store default key");
    let output = resume().await.expect("resume_agent should succeed");
    let (content, _) = expect_text_output(output);
    let result: resume_agent::ResumeAgentResult =
        serde_json::from_str(&content).expect("resume_agent result should be json");
    assert_ne!(result.status, AgentStatus::NotFound);
    let _ = manager
        .agent_control()
        .shutdown_live_agent(agent_id)
        .await
        .expect("shutdown resumed agent");
}

#[tokio::test]
async fn resume_agent_rejects_when_depth_limit_exceeded() {
    let (mut session, mut turn) = make_session_and_context().await;
    let manager = thread_manager();
    session.services.agent_control = manager.agent_control();

    let max_depth = turn.config.agent_max_depth;
    turn.session_source = SessionSource::SubAgent(SubAgentSource::ThreadSpawn {
        parent_thread_id: session.thread_id,
        depth: max_depth,
        agent_path: None,
        agent_nickname: None,
        agent_role: None,
        agent_class: None,
    });

    let invocation = invocation(
        Arc::new(session),
        Arc::new(turn),
        "resume_agent",
        function_payload(json!({"id": ThreadId::new().to_string()})),
    );
    let Err(err) = ResumeAgentHandler.handle(invocation).await else {
        panic!("resume should fail when depth limit exceeded");
    };
    assert_eq!(
        err,
        FunctionCallError::RespondToModel(
            "Agent depth limit reached. Solve the task yourself.".to_string()
        )
    );
}

#[tokio::test]
async fn wait_agent_rejects_non_positive_timeout() {
    let (session, turn) = make_session_and_context().await;
    let invocation = invocation(
        Arc::new(session),
        Arc::new(turn),
        "wait_agent",
        function_payload(json!({
            "targets": [ThreadId::new().to_string()],
            "timeout_ms": 0
        })),
    );
    let Err(err) = WaitAgentHandler::default().handle(invocation).await else {
        panic!("non-positive timeout should be rejected");
    };
    assert_eq!(
        err,
        FunctionCallError::RespondToModel("timeout_ms must be greater than zero".to_string())
    );
}

#[tokio::test]
async fn wait_agent_rejects_invalid_target() {
    let (session, turn) = make_session_and_context().await;
    let invocation = invocation(
        Arc::new(session),
        Arc::new(turn),
        "wait_agent",
        function_payload(json!({"targets": ["invalid"]})),
    );
    let Err(err) = WaitAgentHandler::default().handle(invocation).await else {
        panic!("invalid id should be rejected");
    };
    let FunctionCallError::RespondToModel(msg) = err else {
        panic!("expected respond-to-model error");
    };
    assert!(msg.starts_with("invalid agent id invalid:"));
}

#[tokio::test]
async fn wait_agent_rejects_empty_targets() {
    let (session, turn) = make_session_and_context().await;
    let invocation = invocation(
        Arc::new(session),
        Arc::new(turn),
        "wait_agent",
        function_payload(json!({"targets": []})),
    );
    let Err(err) = WaitAgentHandler::default().handle(invocation).await else {
        panic!("empty ids should be rejected");
    };
    assert_eq!(
        err,
        FunctionCallError::RespondToModel("agent ids must be non-empty".to_string())
    );
}

#[tokio::test]
async fn multi_agent_v2_wait_agent_accepts_timeout_only_argument() {
    let (mut session, mut turn) = make_session_and_context().await;
    let manager = thread_manager();
    let root = manager
        .start_thread(StartThreadOptions::new((*turn.config).clone()))
        .await
        .expect("root thread should start");
    session.services.agent_control = manager.agent_control();
    session.thread_id = root.thread_id;
    let mut config = (*turn.config).clone();
    config
        .features
        .enable(Feature::MultiAgentV2)
        .expect("test config should allow feature update");
    set_turn_config(&mut turn, config);
    let session = Arc::new(session);
    let turn = Arc::new(turn);

    SpawnAgentHandlerV2::default()
        .handle(invocation(
            session.clone(),
            turn.clone(),
            "spawn_agent",
            function_payload(json!({
                "message": "boot worker",
                "task_name": "worker"
            })),
        ))
        .await
        .expect("spawn worker");
    let agent_id = session
        .services
        .agent_control
        .resolve_agent_reference(session.thread_id, &turn.session_source, "worker")
        .await
        .expect("worker should resolve");
    let worker_path = session
        .services
        .agent_control
        .get_agent_metadata(agent_id)
        .expect("worker metadata")
        .agent_path
        .expect("worker path");

    let wait_task = tokio::spawn({
        let session = session.clone();
        let turn = turn.clone();
        async move {
            WaitAgentHandlerV2::default()
                .handle(invocation(
                    session,
                    turn,
                    "wait_agent",
                    function_payload(json!({"timeout_ms": 10_000})),
                ))
                .await
        }
    });
    tokio::task::yield_now().await;

    session
        .input_queue
        .enqueue_mailbox_communication(
            InterAgentCommunication::new(
                worker_path,
                AgentPath::root(),
                Vec::new(),
                "hello from worker".to_string(),
                /*trigger_turn*/ false,
            ),
            /*parent_turn_id*/ None,
        )
        .await;

    let output = wait_task
        .await
        .expect("wait task should join")
        .expect("timeout-only args should be accepted in v2 mode");
    let (content, success) = expect_text_output(output);
    let result: crate::tools::handlers::multi_agents_v2::wait::WaitAgentResult =
        serde_json::from_str(&content).expect("wait_agent result should be json");
    assert_wait_v2_result(&result, "Wait completed.", /*timed_out*/ false);
    assert_wait_v2_agents_include_task(&result, "worker");
    assert_eq!(success, None);
}

#[tokio::test]
async fn multi_agent_v2_wait_agent_rejects_agent_without_eligible_children() {
    let (mut session, mut turn) = make_session_and_context().await;
    let manager = thread_manager();
    let root = manager
        .start_thread((*turn.config).clone())
        .await
        .expect("root thread should start");
    session.services.agent_control = manager.agent_control();
    session.thread_id = root.thread_id;
    let mut config = (*turn.config).clone();
    config
        .features
        .enable(Feature::MultiAgentV2)
        .expect("test config should allow feature update");
    config.multi_agent_v2.min_wait_timeout_ms = 0;
    config.multi_agent_v2.max_wait_timeout_ms = 0;
    config.multi_agent_v2.default_wait_timeout_ms = 0;
    set_turn_config(&mut turn, config);

    let err = WaitAgentHandlerV2::default()
        .handle(invocation(
            Arc::new(session),
            Arc::new(turn),
            "wait_agent",
            function_payload(json!({})),
        ))
        .await
        .err()
        .expect("a childless agent must not enter a meaningless wait");
    assert_eq!(
        err,
        FunctionCallError::RespondToModel(
            "wait_agent rejected: this agent has no eligible child agents; return the result to its parent instead"
                .to_string()
        )
    );
}

#[tokio::test]
async fn multi_agent_v2_wait_agent_rejects_timeout_below_configured_min() {
    let (session, mut turn) = make_session_and_context().await;
    let mut config = (*turn.config).clone();
    config
        .features
        .enable(Feature::MultiAgentV2)
        .expect("test config should allow feature update");
    config.multi_agent_v2.min_wait_timeout_ms = 50;
    config.multi_agent_v2.max_wait_timeout_ms = 1_000;
    config.multi_agent_v2.default_wait_timeout_ms = 50;
    set_turn_config(&mut turn, config);

    let Err(err) = WaitAgentHandlerV2::default()
        .handle(invocation(
            Arc::new(session),
            Arc::new(turn),
            "wait_agent",
            function_payload(json!({"timeout_ms": 1})),
        ))
        .await
    else {
        panic!("timeout below configured minimum should be rejected");
    };
    assert_eq!(
        err,
        FunctionCallError::RespondToModel("timeout_ms must be at least 50".to_string())
    );
}

#[tokio::test]
async fn multi_agent_v2_wait_agent_accepts_explicit_timeout_at_configured_min() {
    let (mut session, mut turn) = make_session_and_context().await;
    let mut config = (*turn.config).clone();
    config
        .features
        .enable(Feature::MultiAgentV2)
        .expect("test config should allow feature update");
    config.multi_agent_v2.min_wait_timeout_ms = 1;
    config.multi_agent_v2.max_wait_timeout_ms = 1_000;
    config.multi_agent_v2.default_wait_timeout_ms = 50;
    set_turn_config(&mut turn, config);
    let _manager = register_v2_wait_child(&mut session, &turn).await;

    let output = WaitAgentHandlerV2::default()
        .handle(invocation(
            Arc::new(session),
            Arc::new(turn),
            "wait_agent",
            function_payload(json!({"timeout_ms": 1})),
        ))
        .await
        .expect("wait_agent should succeed");
    let (content, success) = expect_text_output(output);
    let result: crate::tools::handlers::multi_agents_v2::wait::WaitAgentResult =
        serde_json::from_str(&content).expect("wait_agent result should be json");
    assert_wait_v2_result(&result, "Wait timed out.", /*timed_out*/ true);
    assert_eq!(success, None);
}

#[tokio::test]
async fn multi_agent_v2_wait_agent_uses_configured_default_timeout() {
    let (mut session, mut turn) = make_session_and_context().await;
    let mut config = (*turn.config).clone();
    config
        .features
        .enable(Feature::MultiAgentV2)
        .expect("test config should allow feature update");
    config.multi_agent_v2.min_wait_timeout_ms = 1;
    config.multi_agent_v2.max_wait_timeout_ms = 1_000;
    config.multi_agent_v2.default_wait_timeout_ms = 50;
    set_turn_config(&mut turn, config);
    let _manager = register_v2_wait_child(&mut session, &turn).await;
    let session = Arc::new(session);
    let turn = Arc::new(turn);

    let early = timeout(
        Duration::from_millis(/*millis*/ 20),
        WaitAgentHandlerV2::default().handle(invocation(
            session.clone(),
            turn.clone(),
            "wait_agent",
            function_payload(json!({})),
        )),
    )
    .await;
    assert!(
        early.is_err(),
        "wait_agent should not return before the configured default timeout"
    );

    let output = timeout(
        Duration::from_secs(/*secs*/ 1),
        WaitAgentHandlerV2::default().handle(invocation(
            session,
            turn,
            "wait_agent",
            function_payload(json!({})),
        )),
    )
    .await
    .expect("configured default should be shorter than the test timeout")
    .expect("wait_agent should succeed");
    let (content, success) = expect_text_output(output);
    let result: crate::tools::handlers::multi_agents_v2::wait::WaitAgentResult =
        serde_json::from_str(&content).expect("wait_agent result should be json");
    assert_wait_v2_result(&result, "Wait timed out.", /*timed_out*/ true);
    assert_eq!(success, None);
}

#[tokio::test]
async fn multi_agent_v2_wait_agent_allows_zero_configured_timeout() {
    let (mut session, mut turn) = make_session_and_context().await;
    let mut config = (*turn.config).clone();
    config
        .features
        .enable(Feature::MultiAgentV2)
        .expect("test config should allow feature update");
    config.multi_agent_v2.min_wait_timeout_ms = 0;
    config.multi_agent_v2.max_wait_timeout_ms = 0;
    config.multi_agent_v2.default_wait_timeout_ms = 0;
    set_turn_config(&mut turn, config);
    let _manager = register_v2_wait_child(&mut session, &turn).await;
    let session = Arc::new(session);
    let turn = Arc::new(turn);

    let output = timeout(
        Duration::from_secs(/*secs*/ 1),
        WaitAgentHandlerV2::default().handle(invocation(
            session,
            turn,
            "wait_agent",
            function_payload(json!({})),
        )),
    )
    .await
    .expect("zero timeout should complete immediately")
    .expect("wait_agent should succeed");
    let (content, success) = expect_text_output(output);
    let result: crate::tools::handlers::multi_agents_v2::wait::WaitAgentResult =
        serde_json::from_str(&content).expect("wait_agent result should be json");
    assert_wait_v2_result(&result, "Wait timed out.", /*timed_out*/ true);
    assert_eq!(success, None);
}

#[tokio::test]
async fn multi_agent_v2_wait_watchdog_escalates_and_blocks_polling_loop() {
    let (mut session, mut turn) = make_session_and_context().await;
    let mut config = (*turn.config).clone();
    config
        .features
        .enable(Feature::MultiAgentV2)
        .expect("test config should allow feature update");
    config.multi_agent_v2.min_wait_timeout_ms = 0;
    config.multi_agent_v2.max_wait_timeout_ms = 0;
    config.multi_agent_v2.default_wait_timeout_ms = 0;
    set_turn_config(&mut turn, config);
    let _manager = register_v2_wait_child(&mut session, &turn).await;
    let session = Arc::new(session);
    let turn = Arc::new(turn);
    let handler = WaitAgentHandlerV2::default();

    let mut third_result = None;
    for _ in 0..3 {
        let output = handler
            .handle(invocation(
                session.clone(),
                turn.clone(),
                "wait_agent",
                function_payload(json!({})),
            ))
            .await
            .expect("first three waits return structured status");
        let (content, _) = expect_text_output(output);
        third_result = Some(
            serde_json::from_str::<crate::tools::handlers::multi_agents_v2::wait::WaitAgentResult>(
                &content,
            )
            .expect("wait result should parse"),
        );
    }

    let third_result = third_result.expect("third result");
    assert_eq!(third_result.consecutive_empty_waits, 3);
    assert!(third_result.watchdog_escalated);
    assert!(third_result.message.contains("WATCHDOG ESCALATION"));

    let err = handler
        .handle(invocation(
            session,
            turn,
            "wait_agent",
            function_payload(json!({})),
        ))
        .await
        .err()
        .expect("watchdog must break the polling loop");
    assert_eq!(
        err,
        FunctionCallError::RespondToModel(
            "wait_agent watchdog is open after 3 consecutive empty waits; polling is blocked until new work starts"
                .to_string()
        )
    );
}

#[tokio::test]
async fn multi_agent_v2_wait_agent_rejects_timeout_above_configured_max() {
    let (session, mut turn) = make_session_and_context().await;
    let mut config = (*turn.config).clone();
    config
        .features
        .enable(Feature::MultiAgentV2)
        .expect("test config should allow feature update");
    config.multi_agent_v2.min_wait_timeout_ms = 1;
    config.multi_agent_v2.max_wait_timeout_ms = 50;
    config.multi_agent_v2.default_wait_timeout_ms = 1;
    set_turn_config(&mut turn, config);

    let Err(err) = WaitAgentHandlerV2::default()
        .handle(invocation(
            Arc::new(session),
            Arc::new(turn),
            "wait_agent",
            function_payload(json!({"timeout_ms": 500})),
        ))
        .await
    else {
        panic!("timeout above configured maximum should be rejected");
    };
    assert_eq!(
        err,
        FunctionCallError::RespondToModel("timeout_ms must be at most 50".to_string())
    );
}

#[tokio::test]
async fn multi_agent_v2_wait_agent_accepts_explicit_timeout_at_configured_max() {
    let (mut session, mut turn) = make_session_and_context().await;
    let mut config = (*turn.config).clone();
    config
        .features
        .enable(Feature::MultiAgentV2)
        .expect("test config should allow feature update");
    config.multi_agent_v2.min_wait_timeout_ms = 1;
    config.multi_agent_v2.max_wait_timeout_ms = 1;
    config.multi_agent_v2.default_wait_timeout_ms = 1;
    set_turn_config(&mut turn, config);
    let _manager = register_v2_wait_child(&mut session, &turn).await;

    let output = WaitAgentHandlerV2::default()
        .handle(invocation(
            Arc::new(session),
            Arc::new(turn),
            "wait_agent",
            function_payload(json!({"timeout_ms": 1})),
        ))
        .await
        .expect("wait_agent should succeed");
    let (content, success) = expect_text_output(output);
    let result: crate::tools::handlers::multi_agents_v2::wait::WaitAgentResult =
        serde_json::from_str(&content).expect("wait_agent result should be json");
    assert_wait_v2_result(&result, "Wait timed out.", /*timed_out*/ true);
    assert_eq!(success, None);
}

#[tokio::test]
async fn wait_agent_returns_not_found_for_missing_agents() {
    let (mut session, turn) = make_session_and_context().await;
    let manager = thread_manager();
    session.services.agent_control = manager.agent_control();
    let id_a = ThreadId::new();
    let id_b = ThreadId::new();
    let invocation = invocation(
        Arc::new(session),
        Arc::new(turn),
        "wait_agent",
        function_payload(json!({
            "targets": [id_a.to_string(), id_b.to_string()],
            "timeout_ms": 10_000
        })),
    );
    let output = WaitAgentHandler::default()
        .handle(invocation)
        .await
        .expect("wait_agent should succeed");
    let (content, success) = expect_text_output(output);
    let result: wait::WaitAgentResult =
        serde_json::from_str(&content).expect("wait_agent result should be json");
    assert_eq!(
        result,
        wait::WaitAgentResult {
            status: HashMap::from([
                (id_a.to_string(), AgentStatus::NotFound),
                (id_b.to_string(), AgentStatus::NotFound),
            ]),
            timed_out: false
        }
    );
    assert_eq!(success, None);
}

#[tokio::test]
async fn wait_agent_times_out_when_status_is_not_final() {
    let (mut session, turn) = make_session_and_context().await;
    let manager = thread_manager();
    session.services.agent_control = manager.agent_control();
    let config = turn.config.as_ref().clone();
    let thread = manager
        .start_thread(StartThreadOptions::new(config.clone()))
        .await
        .expect("start thread");
    let agent_id = thread.thread_id;
    let invocation = invocation(
        Arc::new(session),
        Arc::new(turn),
        "wait_agent",
        function_payload(json!({
            "targets": [agent_id.to_string()],
            "timeout_ms": MIN_WAIT_TIMEOUT_MS
        })),
    );
    let output = WaitAgentHandler::default()
        .handle(invocation)
        .await
        .expect("wait_agent should succeed");
    let (content, success) = expect_text_output(output);
    let result: wait::WaitAgentResult =
        serde_json::from_str(&content).expect("wait_agent result should be json");
    assert_eq!(
        result,
        wait::WaitAgentResult {
            status: HashMap::new(),
            timed_out: true
        }
    );
    assert_eq!(success, None);

    let _ = thread
        .thread
        .submit(Op::Shutdown {})
        .await
        .expect("shutdown should submit");
}

#[tokio::test]
async fn wait_agent_clamps_short_timeouts_to_minimum() {
    let (mut session, turn) = make_session_and_context().await;
    let manager = thread_manager();
    session.services.agent_control = manager.agent_control();
    let config = turn.config.as_ref().clone();
    let thread = manager
        .start_thread(StartThreadOptions::new(config.clone()))
        .await
        .expect("start thread");
    let agent_id = thread.thread_id;
    let invocation = invocation(
        Arc::new(session),
        Arc::new(turn),
        "wait_agent",
        function_payload(json!({
            "targets": [agent_id.to_string()],
            "timeout_ms": 10
        })),
    );

    let early = timeout(
        Duration::from_millis(50),
        WaitAgentHandler::default().handle(invocation),
    )
    .await;
    assert!(
        early.is_err(),
        "wait_agent should not return before the minimum timeout clamp"
    );

    let _ = thread
        .thread
        .submit(Op::Shutdown {})
        .await
        .expect("shutdown should submit");
}

#[tokio::test]
async fn wait_agent_returns_final_status_without_timeout() {
    let (mut session, turn) = make_session_and_context().await;
    let manager = thread_manager();
    session.services.agent_control = manager.agent_control();
    let config = turn.config.as_ref().clone();
    let thread = manager
        .start_thread(StartThreadOptions::new(config.clone()))
        .await
        .expect("start thread");
    let agent_id = thread.thread_id;
    let mut status_rx = manager
        .agent_control()
        .subscribe_status(agent_id)
        .await
        .expect("subscribe should succeed");

    let _ = thread
        .thread
        .submit(Op::Shutdown {})
        .await
        .expect("shutdown should submit");
    let _ = timeout(Duration::from_secs(1), status_rx.changed())
        .await
        .expect("shutdown status should arrive");

    let invocation = invocation(
        Arc::new(session),
        Arc::new(turn),
        "wait_agent",
        function_payload(json!({
            "targets": [agent_id.to_string()],
            "timeout_ms": 10_000
        })),
    );
    let output = WaitAgentHandler::default()
        .handle(invocation)
        .await
        .expect("wait_agent should succeed");
    let (content, success) = expect_text_output(output);
    let result: wait::WaitAgentResult =
        serde_json::from_str(&content).expect("wait_agent result should be json");
    assert_eq!(
        result,
        wait::WaitAgentResult {
            status: HashMap::from([(agent_id.to_string(), AgentStatus::Shutdown)]),
            timed_out: false
        }
    );
    assert_eq!(success, None);
}

#[tokio::test]
async fn multi_agent_v2_wait_agent_returns_summary_for_mailbox_activity() {
    let (mut session, mut turn) = make_session_and_context().await;
    let manager = thread_manager();
    let root = manager
        .start_thread(StartThreadOptions::new((*turn.config).clone()))
        .await
        .expect("root thread should start");
    session.services.agent_control = manager.agent_control();
    session.thread_id = root.thread_id;
    let mut config = (*turn.config).clone();
    config
        .features
        .enable(Feature::MultiAgentV2)
        .expect("test config should allow feature update");
    set_turn_config(&mut turn, config);

    let session = Arc::new(session);
    let turn = Arc::new(turn);
    let spawn_output = SpawnAgentHandlerV2::default()
        .handle(invocation(
            session.clone(),
            turn.clone(),
            "spawn_agent",
            function_payload(json!({
                "message": "inspect this repo",
                "task_name": "test_process"
            })),
        ))
        .await
        .expect("spawn_agent should succeed");
    let _ = expect_text_output(spawn_output);

    let agent_id = session
        .services
        .agent_control
        .resolve_agent_reference(session.thread_id, &turn.session_source, "test_process")
        .await
        .expect("relative path should resolve");
    let worker_path = session
        .services
        .agent_control
        .get_agent_metadata(agent_id)
        .expect("worker metadata")
        .agent_path
        .expect("worker path");
    let wait_task = tokio::spawn({
        let session = session.clone();
        let turn = turn.clone();
        async move {
            WaitAgentHandlerV2::default()
                .handle(invocation(
                    session,
                    turn,
                    "wait_agent",
                    function_payload(json!({"timeout_ms": 10_000})),
                ))
                .await
        }
    });
    tokio::task::yield_now().await;

    session
        .input_queue
        .enqueue_mailbox_communication(
            InterAgentCommunication::new(
                worker_path,
                AgentPath::root(),
                Vec::new(),
                "completed".to_string(),
                /*trigger_turn*/ false,
            ),
            /*parent_turn_id*/ None,
        )
        .await;

    let wait_output = wait_task
        .await
        .expect("wait task should join")
        .expect("wait_agent should succeed");
    let (content, success) = expect_text_output(wait_output);
    let result: crate::tools::handlers::multi_agents_v2::wait::WaitAgentResult =
        serde_json::from_str(&content).expect("wait_agent result should be json");
    assert_wait_v2_result(&result, "Wait completed.", /*timed_out*/ false);
    assert_wait_v2_agents_include_task(&result, "test_process");
    assert_eq!(success, None);
}

#[tokio::test]
async fn multi_agent_v2_wait_agent_returns_for_already_queued_mail() {
    let (mut session, mut turn) = make_session_and_context().await;
    let manager = thread_manager();
    let root = manager
        .start_thread(StartThreadOptions::new((*turn.config).clone()))
        .await
        .expect("root thread should start");
    session.services.agent_control = manager.agent_control();
    session.thread_id = root.thread_id;
    let mut config = (*turn.config).clone();
    config
        .features
        .enable(Feature::MultiAgentV2)
        .expect("test config should allow feature update");
    set_turn_config(&mut turn, config);
    let session = Arc::new(session);
    let turn = Arc::new(turn);

    SpawnAgentHandlerV2::default()
        .handle(invocation(
            session.clone(),
            turn.clone(),
            "spawn_agent",
            function_payload(json!({
                "message": "boot worker",
                "task_name": "worker"
            })),
        ))
        .await
        .expect("spawn worker");
    let agent_id = session
        .services
        .agent_control
        .resolve_agent_reference(session.thread_id, &turn.session_source, "worker")
        .await
        .expect("worker should resolve");
    let worker_path = session
        .services
        .agent_control
        .get_agent_metadata(agent_id)
        .expect("worker metadata")
        .agent_path
        .expect("worker path");

    session
        .input_queue
        .enqueue_mailbox_communication(
            InterAgentCommunication::new(
                worker_path,
                AgentPath::root(),
                Vec::new(),
                "already queued".to_string(),
                /*trigger_turn*/ false,
            ),
            /*parent_turn_id*/ None,
        )
        .await;

    let output = timeout(
        Duration::from_millis(500),
        WaitAgentHandlerV2::default().handle(invocation(
            session,
            turn,
            "wait_agent",
            function_payload(json!({"timeout_ms": 10_000})),
        )),
    )
    .await
    .expect("already queued mail should complete wait_agent immediately")
    .expect("wait_agent should succeed");
    let (content, success) = expect_text_output(output);
    let result: crate::tools::handlers::multi_agents_v2::wait::WaitAgentResult =
        serde_json::from_str(&content).expect("wait_agent result should be json");
    assert_wait_v2_result(&result, "Wait completed.", /*timed_out*/ false);
    assert_wait_v2_agents_include_task(&result, "worker");
    assert_eq!(success, None);
}

#[tokio::test]
async fn multi_agent_v2_wait_agent_wakes_on_any_mailbox_notification() {
    let (mut session, mut turn) = make_session_and_context().await;
    let manager = thread_manager();
    let root = manager
        .start_thread(StartThreadOptions::new((*turn.config).clone()))
        .await
        .expect("root thread should start");
    session.services.agent_control = manager.agent_control();
    session.thread_id = root.thread_id;
    let mut config = (*turn.config).clone();
    config
        .features
        .enable(Feature::MultiAgentV2)
        .expect("test config should allow feature update");
    set_turn_config(&mut turn, config);
    let session = Arc::new(session);
    let turn = Arc::new(turn);

    for task_name in ["worker_a", "worker_b"] {
        SpawnAgentHandlerV2::default()
            .handle(invocation(
                session.clone(),
                turn.clone(),
                "spawn_agent",
                function_payload(json!({
                    "message": format!("boot {task_name}"),
                    "task_name": task_name
                })),
            ))
            .await
            .expect("spawn worker");
    }
    let worker_b_id = session
        .services
        .agent_control
        .resolve_agent_reference(session.thread_id, &turn.session_source, "worker_b")
        .await
        .expect("worker_b should resolve");
    let worker_b_path = session
        .services
        .agent_control
        .get_agent_metadata(worker_b_id)
        .expect("worker_b metadata")
        .agent_path
        .expect("worker_b path");

    let wait_task = tokio::spawn({
        let session = session.clone();
        let turn = turn.clone();
        async move {
            WaitAgentHandlerV2::default()
                .handle(invocation(
                    session,
                    turn,
                    "wait_agent",
                    function_payload(json!({"timeout_ms": 10_000})),
                ))
                .await
        }
    });
    tokio::task::yield_now().await;

    session
        .input_queue
        .enqueue_mailbox_communication(
            InterAgentCommunication::new(
                worker_b_path,
                AgentPath::root(),
                Vec::new(),
                "from worker b".to_string(),
                /*trigger_turn*/ false,
            ),
            /*parent_turn_id*/ None,
        )
        .await;

    let output = wait_task
        .await
        .expect("wait task should join")
        .expect("wait_agent should succeed");
    let (content, success) = expect_text_output(output);
    let result: crate::tools::handlers::multi_agents_v2::wait::WaitAgentResult =
        serde_json::from_str(&content).expect("wait_agent result should be json");
    assert_wait_v2_result(&result, "Wait completed.", /*timed_out*/ false);
    assert_wait_v2_agents_include_task(&result, "worker_b");
    assert_eq!(success, None);
}

#[tokio::test]
async fn multi_agent_v2_wait_agent_does_not_return_completed_content() {
    let (mut session, mut turn) = make_session_and_context().await;
    let manager = thread_manager();
    let root = manager
        .start_thread(StartThreadOptions::new((*turn.config).clone()))
        .await
        .expect("root thread should start");
    session.services.agent_control = manager.agent_control();
    session.thread_id = root.thread_id;
    let mut config = (*turn.config).clone();
    config
        .features
        .enable(Feature::MultiAgentV2)
        .expect("test config should allow feature update");
    set_turn_config(&mut turn, config);
    let session = Arc::new(session);
    let turn = Arc::new(turn);

    SpawnAgentHandlerV2::default()
        .handle(invocation(
            session.clone(),
            turn.clone(),
            "spawn_agent",
            function_payload(json!({
                "message": "boot worker",
                "task_name": "worker"
            })),
        ))
        .await
        .expect("spawn worker");
    let agent_id = session
        .services
        .agent_control
        .resolve_agent_reference(session.thread_id, &turn.session_source, "worker")
        .await
        .expect("worker should resolve");
    let worker_path = session
        .services
        .agent_control
        .get_agent_metadata(agent_id)
        .expect("worker metadata")
        .agent_path
        .expect("worker path");
    let wait_task = tokio::spawn({
        let session = session.clone();
        let turn = turn.clone();
        async move {
            WaitAgentHandlerV2::default()
                .handle(invocation(
                    session,
                    turn,
                    "wait_agent",
                    function_payload(json!({"timeout_ms": 10_000})),
                ))
                .await
        }
    });
    tokio::task::yield_now().await;

    session
        .input_queue
        .enqueue_mailbox_communication(
            InterAgentCommunication::new(
                worker_path,
                AgentPath::root(),
                Vec::new(),
                "sensitive child output".to_string(),
                /*trigger_turn*/ false,
            ),
            /*parent_turn_id*/ None,
        )
        .await;

    let output = wait_task
        .await
        .expect("wait task should join")
        .expect("wait_agent should succeed");
    let (content, success) = expect_text_output(output);
    let result: crate::tools::handlers::multi_agents_v2::wait::WaitAgentResult =
        serde_json::from_str(&content).expect("wait_agent result should be json");
    assert_wait_v2_result(&result, "Wait completed.", /*timed_out*/ false);
    assert_wait_v2_agents_include_task(&result, "worker");
    assert!(!content.contains("sensitive child output"));
    assert_eq!(success, None);
}

#[tokio::test]
async fn multi_agent_v2_interrupt_agent_accepts_task_name_target() {
    let (mut session, mut turn) = make_session_and_context().await;
    let manager = thread_manager();
    let root = manager
        .start_thread(StartThreadOptions::new((*turn.config).clone()))
        .await
        .expect("root thread should start");
    session.services.agent_control = manager.agent_control();
    session.thread_id = root.thread_id;
    let mut config = (*turn.config).clone();
    config
        .features
        .enable(Feature::MultiAgentV2)
        .expect("test config should allow feature update");
    set_turn_config(&mut turn, config);

    let session = Arc::new(session);
    let turn = Arc::new(turn);
    SpawnAgentHandlerV2::default()
        .handle(invocation(
            session.clone(),
            turn.clone(),
            "spawn_agent",
            function_payload(json!({
                "message": "inspect this repo",
                "task_name": "worker"
            })),
        ))
        .await
        .expect("spawn_agent should succeed");

    let agent_id = session
        .services
        .agent_control
        .resolve_agent_reference(session.thread_id, &turn.session_source, "worker")
        .await
        .expect("worker path should resolve");
    let worker_thread = manager
        .get_thread(agent_id)
        .await
        .expect("worker thread should be resident");
    let worker_session = worker_thread.session.clone();
    SpawnAgentHandlerV2::default()
        .handle(invocation(
            worker_session.clone(),
            worker_session.new_default_turn().await,
            "spawn_agent",
            function_payload(json!({
                "message": "inspect a child task",
                "task_name": "child"
            })),
        ))
        .await
        .expect("child spawn should succeed");
    let child_id = session
        .services
        .agent_control
        .resolve_agent_reference(session.thread_id, &turn.session_source, "worker/child")
        .await
        .expect("child path should resolve");

    let output = InterruptAgentHandler
        .handle(invocation(
            session.clone(),
            turn.clone(),
            "interrupt_agent",
            function_payload(json!({"target": "worker", "reason": "review correction"})),
        ))
        .await
        .expect("interrupt_agent should succeed for v2 task names");
    let (content, success) = expect_text_output(output);
    let result: InterruptAgentResult =
        serde_json::from_str(&content).expect("interrupt_agent result should be json");
    assert_ne!(result.previous_status, AgentStatus::NotFound);
    assert_eq!(success, Some(true));
    assert_eq!(
        session
            .services
            .agent_control
            .resolve_agent_reference(session.thread_id, &turn.session_source, "worker")
            .await
            .expect("worker path should remain resolvable"),
        agent_id
    );
    manager
        .get_thread(agent_id)
        .await
        .expect("worker should remain resident");
    manager
        .get_thread(child_id)
        .await
        .expect("child should remain resident");
    let ops = manager.captured_ops();
    assert!(
        ops.iter()
            .any(|(thread_id, op)| *thread_id == agent_id && matches!(op, Op::Interrupt))
    );
    assert!(!ops.iter().any(|(thread_id, op)| {
        (*thread_id == agent_id || *thread_id == child_id) && matches!(op, Op::Shutdown)
    }));
    assert!(
        !ops.iter()
            .any(|(thread_id, op)| *thread_id == child_id && matches!(op, Op::Interrupt))
    );
}

#[tokio::test]
async fn multi_agent_v2_interrupt_shutdown_agent_is_idempotent_and_preserves_identity() {
    let (mut session, mut turn) = make_session_and_context().await;
    let manager = thread_manager();
    let root = manager
        .start_thread(StartThreadOptions::new((*turn.config).clone()))
        .await
        .expect("root thread should start");
    session.services.agent_control = manager.agent_control();
    session.thread_id = root.thread_id;
    let mut config = (*turn.config).clone();
    config
        .features
        .enable(Feature::MultiAgentV2)
        .expect("test config should allow feature update");
    set_turn_config(&mut turn, config);

    let session = Arc::new(session);
    let turn = Arc::new(turn);
    SpawnAgentHandlerV2::default()
        .handle(invocation(
            session.clone(),
            turn.clone(),
            "spawn_agent",
            function_payload(json!({
                "message": "inspect this repo",
                "task_name": "worker"
            })),
        ))
        .await
        .expect("spawn_agent should succeed");
    let agent_id = session
        .services
        .agent_control
        .resolve_agent_reference(session.thread_id, &turn.session_source, "worker")
        .await
        .expect("worker path should resolve");
    let worker = manager
        .get_thread(agent_id)
        .await
        .expect("worker should be resident");
    worker
        .submit(Op::Shutdown {})
        .await
        .expect("worker shutdown should submit");
    worker.wait_until_terminated().await;
    assert_eq!(
        session.services.agent_control.get_status(agent_id).await,
        AgentStatus::Shutdown
    );
    let audit_count = manager
        .captured_ops()
        .iter()
        .filter(|(thread_id, op)| {
            *thread_id == agent_id && matches!(op, Op::InterAgentCommunication { .. })
        })
        .count();

    for _ in 0..2 {
        let output = InterruptAgentHandler
            .handle(invocation(
                session.clone(),
                turn.clone(),
                "interrupt_agent",
                function_payload(json!({"target": "worker", "reason": "review correction"})),
            ))
            .await
            .expect("interrupting a shutdown agent should be idempotent");
        let (content, success) = expect_text_output(output);
        let result: InterruptAgentResult =
            serde_json::from_str(&content).expect("interrupt_agent result should be json");
        assert_eq!(result.previous_status, AgentStatus::Shutdown);
        assert_eq!(success, Some(true));
    }

    assert_eq!(
        manager
            .captured_ops()
            .iter()
            .filter(|(thread_id, op)| {
                *thread_id == agent_id && matches!(op, Op::InterAgentCommunication { .. })
            })
            .count(),
        audit_count
    );
    assert!(
        session
            .services
            .agent_control
            .get_agent_metadata(agent_id)
            .is_some()
    );
}

#[tokio::test]
async fn multi_agent_v2_interrupt_agent_accepts_canonical_child_path() {
    let (mut session, mut turn) = make_session_and_context().await;
    let manager = thread_manager();
    let root = manager
        .start_thread((*turn.config).clone())
        .await
        .expect("root thread should start");
    session.services.agent_control = manager.agent_control();
    session.thread_id = root.thread_id;
    let mut config = (*turn.config).clone();
    config
        .features
        .enable(Feature::MultiAgentV2)
        .expect("test config should allow feature update");
    set_turn_config(&mut turn, config);

    let session = Arc::new(session);
    let turn = Arc::new(turn);
    SpawnAgentHandlerV2::default()
        .handle(invocation(
            session.clone(),
            turn.clone(),
            "spawn_agent",
            function_payload(json!({
                "message": "inspect this repo",
                "task_name": "worker"
            })),
        ))
        .await
        .expect("worker spawn should succeed");
    let worker_id = session
        .services
        .agent_control
        .resolve_agent_reference(session.thread_id, &turn.session_source, "worker")
        .await
        .expect("worker path should resolve");
    let worker_thread = manager
        .get_thread(worker_id)
        .await
        .expect("worker thread should be resident");
    let worker_session = worker_thread.codex.session.clone();
    SpawnAgentHandlerV2::default()
        .handle(invocation(
            worker_session.clone(),
            worker_session.new_default_turn().await,
            "spawn_agent",
            function_payload(json!({
                "message": "inspect a child task",
                "task_name": "child"
            })),
        ))
        .await
        .expect("child spawn should succeed");
    let child_id = session
        .services
        .agent_control
        .resolve_agent_reference(session.thread_id, &turn.session_source, "worker/child")
        .await
        .expect("child path should resolve");

    let output = InterruptAgentHandler
        .handle(invocation(
            session,
            turn,
            "interrupt_agent",
            function_payload(json!({
                "target": "/root/worker/child",
                "reason": "correct the child capture",
                "superseding_task": "record all eight party directions"
            })),
        ))
        .await
        .expect("interrupt_agent should accept the canonical child path from list_agents");
    let (content, success) = expect_text_output(output);
    let result: InterruptAgentResult =
        serde_json::from_str(&content).expect("canonical interrupt result should be json");
    assert_ne!(result.previous_status, AgentStatus::NotFound);
    assert_eq!(success, Some(true));
    assert!(
        manager
            .captured_ops()
            .iter()
            .any(|(thread_id, op)| { *thread_id == child_id && matches!(op, Op::Interrupt) })
    );
    assert!(
        !manager
            .captured_ops()
            .iter()
            .any(|(thread_id, op)| { *thread_id == worker_id && matches!(op, Op::Interrupt) })
    );
}

#[tokio::test]
async fn multi_agent_v2_interrupt_requires_visible_reason_and_superseding_task() {
    let (mut session, mut turn) = make_session_and_context().await;
    let manager = thread_manager();
    let root = manager
        .start_thread((*turn.config).clone())
        .await
        .expect("root thread should start");
    session.services.agent_control = manager.agent_control();
    session.thread_id = root.thread_id;
    let mut config = (*turn.config).clone();
    config
        .features
        .enable(Feature::MultiAgentV2)
        .expect("test config should allow feature update");
    set_turn_config(&mut turn, config);
    let session = Arc::new(session);
    let turn = Arc::new(turn);

    SpawnAgentHandlerV2::default()
        .handle(invocation(
            session.clone(),
            turn.clone(),
            "spawn_agent",
            function_payload(json!({
                "message": "run the long verification",
                "task_name": "worker"
            })),
        ))
        .await
        .expect("spawn worker");

    let output = InterruptAgentHandler
        .handle(invocation(
            session,
            turn,
            "interrupt_agent",
            function_payload(json!({
                "target": "worker",
                "reason": "verification scope changed after review",
                "superseding_task": "audit only the committed candidate"
            })),
        ))
        .await
        .expect("attributed interrupt should succeed");
    let (content, success) = expect_text_output(output);
    assert!(content.contains("verification scope changed after review"));
    assert!(content.contains("audit only the committed candidate"));
    assert_eq!(success, Some(true));
}

#[tokio::test]
async fn multi_agent_v2_interrupt_target_receives_full_control_and_process_tuple() {
    let (mut session, mut turn) = make_session_and_context().await;
    let manager = thread_manager();
    let root = manager
        .start_thread((*turn.config).clone())
        .await
        .expect("root thread should start");
    session.services.agent_control = manager.agent_control();
    session.thread_id = root.thread_id;
    let mut config = (*turn.config).clone();
    config
        .features
        .enable(Feature::MultiAgentV2)
        .expect("test config should allow feature update");
    set_turn_config(&mut turn, config);
    turn.session_source = SessionSource::Cli;
    let session = Arc::new(session);
    let turn = Arc::new(turn);

    SpawnAgentHandlerV2::default()
        .handle(invocation(
            session.clone(),
            turn.clone(),
            "spawn_agent",
            function_payload(json!({
                "message": "run the long verification",
                "task_name": "worker"
            })),
        ))
        .await
        .expect("spawn worker");
    let worker_id = session
        .services
        .agent_control
        .resolve_agent_reference(session.thread_id, &turn.session_source, "worker")
        .await
        .expect("worker path should resolve");
    let worker_nickname = session
        .services
        .agent_control
        .get_agent_metadata(worker_id)
        .expect("worker metadata")
        .agent_nickname
        .expect("spawned worker nickname");

    InterruptAgentHandler
        .handle(invocation(
            session,
            turn,
            "interrupt_agent",
            function_payload(json!({
                "target": "worker",
                "reason": "verification scope changed after review",
                "superseding_task": "dispatch #22: audit only the committed candidate"
            })),
        ))
        .await
        .expect("attributed interrupt should succeed");

    let target_audit =
        manager
            .captured_ops()
            .into_iter()
            .find_map(|(thread_id, op)| match op {
                Op::InterAgentCommunication { communication }
                    if thread_id == worker_id
                        && (communication.content.contains("CONTROL EVENT — INTERRUPT")
                            || communication.encrypted_content.as_deref().is_some_and(
                                |content| content.contains("CONTROL EVENT — INTERRUPT"),
                            )) =>
                {
                    Some(communication)
                }
                _ => None,
            })
            .expect("target pane must receive the interrupt audit");
    let expected = format!(
        "Actor: /root\nTarget: {worker_nickname} · /root/worker\nReason: verification scope changed after review\nSuperseding task/dispatch: dispatch #22: audit only the committed candidate\nProcess effect: model turn aborted; active turn-owned tool processes receive SIGTERM cleanup before abort; durable unified-exec background processes are preserved and remain inspectable in /ps"
    );
    assert!(
        target_audit.encrypted_content.is_none(),
        "runtime-generated plaintext must not be mislabeled as encrypted model content: {target_audit:?}"
    );
    assert!(
        target_audit.content.contains(&expected),
        "target interrupt audit did not contain the full tuple: {target_audit:?}"
    );
}

#[tokio::test]
async fn multi_agent_v2_interrupt_agent_accepts_unloaded_task_name_target() {
    let (mut session, mut turn) = make_session_and_context().await;
    let mut config = (*turn.config).clone();
    config.multi_agent_v2.max_concurrent_threads_per_session = 2;
    config
        .features
        .enable(Feature::MultiAgentV2)
        .expect("test config should allow feature update");
    config
        .features
        .enable(Feature::Sqlite)
        .expect("test config should allow sqlite");
    let state_db = init_state_db(&config)
        .await
        .expect("sqlite state db should initialize");
    let manager = ThreadManager::with_models_provider_home_and_state_for_tests(
        CodexAuth::from_api_key("dummy"),
        config.model_provider.clone(),
        config.codex_home.to_path_buf(),
        Arc::new(codex_exec_server::EnvironmentManager::default_for_tests()),
        Some(state_db.clone()),
    );
    let root = manager
        .start_thread(StartThreadOptions::new(config.clone()))
        .await
        .expect("root thread should start");
    session.services.agent_control = manager.agent_control();
    session.thread_id = root.thread_id;
    set_turn_config(&mut turn, config.clone());

    let session = Arc::new(session);
    let turn = Arc::new(turn);
    SpawnAgentHandlerV2::default()
        .handle(invocation(
            session.clone(),
            turn.clone(),
            "spawn_agent",
            function_payload(json!({
                "message": "inspect this repo",
                "task_name": "worker"
            })),
        ))
        .await
        .expect("spawn_agent should succeed");

    let agent_id = session
        .services
        .agent_control
        .resolve_agent_reference(session.thread_id, &turn.session_source, "worker")
        .await
        .expect("worker path should resolve");
    let stale_thread = manager
        .remove_thread(&agent_id)
        .await
        .expect("worker thread should be loaded before removal");
    stale_thread
        .submit(Op::Shutdown {})
        .await
        .expect("removed worker thread should still accept shutdown");
    stale_thread.wait_until_terminated().await;

    let output = InterruptAgentHandler
        .handle(invocation(
            session.clone(),
            turn.clone(),
            "interrupt_agent",
            function_payload(json!({"target": "worker", "reason": "review correction"})),
        ))
        .await
        .expect("interrupt_agent should accept unloaded v2 task names");
    let (content, success) = expect_text_output(output);
    let result: InterruptAgentResult =
        serde_json::from_str(&content).expect("interrupt_agent result should be json");
    assert_eq!(result.previous_status, AgentStatus::Unloaded);
    assert_eq!(success, Some(true));

    let open_children = state_db
        .list_thread_spawn_children_with_status(
            root.thread_id,
            DirectionalThreadSpawnEdgeStatus::Open,
        )
        .await
        .expect("open children should load");
    assert_eq!(open_children, vec![agent_id]);
    let closed_children = state_db
        .list_thread_spawn_children_with_status(
            root.thread_id,
            DirectionalThreadSpawnEdgeStatus::Closed,
        )
        .await
        .expect("closed children should load");
    assert_eq!(closed_children, Vec::<ThreadId>::new());

    let output = ListAgentsHandlerV2
        .handle(invocation(
            session.clone(),
            turn.clone(),
            "list_agents",
            function_payload(json!({})),
        ))
        .await
        .expect("list_agents should succeed");
    let (content, _) = expect_text_output(output);
    let result: ListAgentsResult =
        serde_json::from_str(&content).expect("list_agents result should be json");
    assert_eq!(result.agents.len(), 2);
    assert_eq!(result.agents[0].agent_name, "/root");
    let unloaded_worker = result
        .agents
        .iter()
        .find(|agent| agent.agent_name == "/root/worker")
        .expect("unloaded worker should remain visible");
    assert_eq!(unloaded_worker.agent_status, json!("unloaded"));
}

#[tokio::test]
async fn multi_agent_v2_interrupt_agent_rejects_root_target_and_id() {
    let (mut session, mut turn) = make_session_and_context().await;
    let manager = thread_manager();
    let root = manager
        .start_thread(StartThreadOptions::new((*turn.config).clone()))
        .await
        .expect("root thread should start");
    session.services.agent_control = manager.agent_control();
    session.thread_id = root.thread_id;
    let mut config = (*turn.config).clone();
    config
        .features
        .enable(Feature::MultiAgentV2)
        .expect("test config should allow feature update");
    set_turn_config(&mut turn, config);

    let session = Arc::new(session);
    let turn = Arc::new(turn);
    let root_path_error = InterruptAgentHandler
        .handle(invocation(
            session.clone(),
            turn.clone(),
            "interrupt_agent",
            function_payload(json!({"target": "/root", "reason": "invalid root test"})),
        ))
        .await
        .err()
        .expect("interrupt_agent should reject the root path");
    assert_eq!(
        root_path_error,
        FunctionCallError::RespondToModel("root is not a spawned agent".to_string())
    );

    let root_id_error = InterruptAgentHandler
        .handle(invocation(
            session,
            turn,
            "interrupt_agent",
            function_payload(
                json!({"target": root.thread_id.to_string(), "reason": "invalid root test"}),
            ),
        ))
        .await
        .err()
        .expect("interrupt_agent should reject the root thread id");
    assert_eq!(
        root_id_error,
        FunctionCallError::RespondToModel("root is not a spawned agent".to_string())
    );
}

#[tokio::test]
async fn multi_agent_v2_interrupt_agent_rejects_self_target_by_id() {
    let (mut session, mut turn) = make_session_and_context().await;
    let manager = thread_manager();
    let mut config = (*turn.config).clone();
    config
        .features
        .enable(Feature::MultiAgentV2)
        .expect("test config should allow feature update");
    set_turn_config(&mut turn, config);
    let root = manager
        .start_thread(StartThreadOptions::new((*turn.config).clone()))
        .await
        .expect("root thread should start");
    session.services.agent_control = manager.agent_control();
    session.thread_id = root.thread_id;

    let child_path = AgentPath::try_from("/root/worker").expect("agent path");
    let child_thread_id = session
        .services
        .agent_control
        .spawn_agent_with_metadata(
            (*turn.config).clone(),
            vec![UserInput::Text {
                text: "inspect this repo".to_string(),
                text_elements: Vec::new(),
            }],
            Some(SessionSource::SubAgent(SubAgentSource::ThreadSpawn {
                parent_thread_id: root.thread_id,
                depth: 1,
                agent_path: Some(child_path.clone()),
                agent_nickname: None,
                agent_role: None,
                agent_class: None,
            })),
            crate::agent::control::SpawnAgentOptions::default(),
        )
        .await
        .expect("worker spawn should succeed")
        .thread_id;
    session.thread_id = child_thread_id;
    turn.session_source = SessionSource::SubAgent(SubAgentSource::ThreadSpawn {
        parent_thread_id: root.thread_id,
        depth: 1,
        agent_path: Some(child_path),
        agent_nickname: None,
        agent_role: None,
        agent_class: None,
    });

    let err = InterruptAgentHandler
        .handle(invocation(
            Arc::new(session),
            Arc::new(turn),
            "interrupt_agent",
            function_payload(
                json!({"target": child_thread_id.to_string(), "reason": "invalid self test"}),
            ),
        ))
        .await
        .err()
        .expect("interrupt_agent should reject self-target by id");
    assert_eq!(
        err,
        FunctionCallError::RespondToModel(
            "an agent cannot interrupt itself; return your result and let the parent interrupt you if needed"
                .to_string()
        )
    );
}

#[tokio::test]
async fn multi_agent_v2_interrupt_agent_rejects_self_target_by_task_name() {
    let (mut session, mut turn) = make_session_and_context().await;
    let manager = thread_manager();
    let mut config = (*turn.config).clone();
    config
        .features
        .enable(Feature::MultiAgentV2)
        .expect("test config should allow feature update");
    set_turn_config(&mut turn, config);
    let root = manager
        .start_thread(StartThreadOptions::new((*turn.config).clone()))
        .await
        .expect("root thread should start");
    session.services.agent_control = manager.agent_control();
    session.thread_id = root.thread_id;

    let child_path = AgentPath::try_from("/root/worker").expect("agent path");
    let child_thread_id = session
        .services
        .agent_control
        .spawn_agent_with_metadata(
            (*turn.config).clone(),
            vec![UserInput::Text {
                text: "inspect this repo".to_string(),
                text_elements: Vec::new(),
            }],
            Some(SessionSource::SubAgent(SubAgentSource::ThreadSpawn {
                parent_thread_id: root.thread_id,
                depth: 1,
                agent_path: Some(child_path.clone()),
                agent_nickname: None,
                agent_role: None,
                agent_class: None,
            })),
            crate::agent::control::SpawnAgentOptions::default(),
        )
        .await
        .expect("worker spawn should succeed")
        .thread_id;
    session.thread_id = child_thread_id;
    turn.session_source = SessionSource::SubAgent(SubAgentSource::ThreadSpawn {
        parent_thread_id: root.thread_id,
        depth: 1,
        agent_path: Some(child_path.clone()),
        agent_nickname: None,
        agent_role: None,
        agent_class: None,
    });

    let err = InterruptAgentHandler
        .handle(invocation(
            Arc::new(session),
            Arc::new(turn),
            "interrupt_agent",
            function_payload(
                json!({"target": child_path.to_string(), "reason": "invalid self test"}),
            ),
        ))
        .await
        .err()
        .expect("interrupt_agent should reject self-target by task name");
    assert_eq!(
        err,
        FunctionCallError::RespondToModel(
            "an agent cannot interrupt itself; return your result and let the parent interrupt you if needed"
                .to_string()
        )
    );
}

#[tokio::test]
async fn close_agent_submits_shutdown_and_returns_previous_status() {
    let (mut session, turn) = make_session_and_context().await;
    let manager = thread_manager();
    session.services.agent_control = manager.agent_control();
    let config = turn.config.as_ref().clone();
    let thread = manager
        .start_thread(StartThreadOptions::new(config.clone()))
        .await
        .expect("start thread");
    let agent_id = thread.thread_id;
    let status_before = manager.agent_control().get_status(agent_id).await;

    let invocation = invocation(
        Arc::new(session),
        Arc::new(turn),
        "close_agent",
        function_payload(json!({"target": agent_id.to_string()})),
    );
    let output = CloseAgentHandler
        .handle(invocation)
        .await
        .expect("close_agent should succeed");
    let (content, success) = expect_text_output(output);
    let result: close_agent::CloseAgentResult =
        serde_json::from_str(&content).expect("close_agent result should be json");
    assert_eq!(result.previous_status, status_before);
    assert_eq!(success, Some(true));

    let ops = manager.captured_ops();
    let submitted_shutdown = ops
        .iter()
        .any(|(id, op)| *id == agent_id && matches!(op, Op::Shutdown));
    assert_eq!(submitted_shutdown, true);

    let status_after = manager.agent_control().get_status(agent_id).await;
    assert_eq!(status_after, AgentStatus::NotFound);
}

#[tokio::test]
async fn tool_handlers_cascade_close_and_resume_and_keep_explicitly_closed_subtrees_closed() {
    let (_session, turn) = make_session_and_context().await;
    let mut config = turn.config.as_ref().clone();
    config.agent_max_depth = 3;
    config
        .features
        .enable(Feature::Sqlite)
        .expect("test config should allow sqlite");
    let state_db = init_state_db(&config).await;
    let auth_manager = AuthManager::from_auth_for_testing(CodexAuth::from_api_key("dummy"));
    let manager = ThreadManager::new(
        &config,
        auth_manager.clone(),
        crate::thread_manager::build_models_manager(&config, auth_manager),
        crate::CodexAppsToolsCache::default(),
        SessionSource::Exec,
        Arc::new(codex_exec_server::EnvironmentManager::default_for_tests()),
        empty_extension_registry(),
        Arc::new(crate::test_support::EmptyUserInstructionsProvider),
        /*analytics_events_client*/ None,
        thread_store_from_config(&config, state_db.clone()),
        local_agent_graph_store_from_state_db(state_db.as_ref()),
        "11111111-1111-4111-8111-111111111111".to_string(),
        /*attestation_provider*/ None,
        /*external_time_provider*/ None,
    );

    let parent = manager
        .start_thread(StartThreadOptions::new(config.clone()))
        .await
        .expect("parent thread should start");
    let parent_thread_id = parent.thread_id;
    let parent_session = parent.thread.session.clone();

    let child_turn = parent_session.new_default_turn().await;
    let child_spawn_output = SpawnAgentHandler::default()
        .handle(invocation(
            parent_session.clone(),
            child_turn,
            "spawn_agent",
            function_payload(json!({"message": "hello child"})),
        ))
        .await
        .expect("child spawn should succeed");
    let (child_content, child_success) = expect_text_output(child_spawn_output);
    let child_result: serde_json::Value =
        serde_json::from_str(&child_content).expect("child spawn result should be json");
    let child_thread_id = parse_agent_id(
        child_result
            .get("agent_id")
            .and_then(serde_json::Value::as_str)
            .expect("child spawn result should include agent_id"),
    );
    assert_eq!(child_success, Some(true));

    let child_thread = manager
        .get_thread(child_thread_id)
        .await
        .expect("child thread should exist");
    let child_session = child_thread.session.clone();
    let grandchild_spawn_output = SpawnAgentHandler::default()
        .handle(invocation(
            child_session.clone(),
            child_session.new_default_turn().await,
            "spawn_agent",
            function_payload(json!({"message": "hello grandchild"})),
        ))
        .await
        .expect("grandchild spawn should succeed");
    let (grandchild_content, grandchild_success) = expect_text_output(grandchild_spawn_output);
    let grandchild_result: serde_json::Value =
        serde_json::from_str(&grandchild_content).expect("grandchild spawn result should be json");
    let grandchild_thread_id = parse_agent_id(
        grandchild_result
            .get("agent_id")
            .and_then(serde_json::Value::as_str)
            .expect("grandchild spawn result should include agent_id"),
    );
    assert_eq!(grandchild_success, Some(true));

    let close_output = CloseAgentHandler
        .handle(invocation(
            parent_session.clone(),
            parent_session.new_default_turn().await,
            "close_agent",
            function_payload(json!({"target": child_thread_id.to_string()})),
        ))
        .await
        .expect("close_agent should close the child subtree");
    let (close_content, close_success) = expect_text_output(close_output);
    let close_result: close_agent::CloseAgentResult =
        serde_json::from_str(&close_content).expect("close_agent result should be json");
    assert_ne!(close_result.previous_status, AgentStatus::NotFound);
    assert_eq!(close_success, Some(true));
    assert_eq!(
        manager.agent_control().get_status(child_thread_id).await,
        AgentStatus::NotFound
    );
    assert_eq!(
        manager
            .agent_control()
            .get_status(grandchild_thread_id)
            .await,
        AgentStatus::NotFound
    );

    let child_resume_output = ResumeAgentHandler
        .handle(invocation(
            parent_session.clone(),
            parent_session.new_default_turn().await,
            "resume_agent",
            function_payload(json!({"id": child_thread_id.to_string()})),
        ))
        .await
        .expect("resume_agent should reopen the child subtree");
    let (child_resume_content, child_resume_success) = expect_text_output(child_resume_output);
    let child_resume_result: resume_agent::ResumeAgentResult =
        serde_json::from_str(&child_resume_content).expect("resume result should be json");
    assert_ne!(child_resume_result.status, AgentStatus::NotFound);
    assert_eq!(child_resume_success, Some(true));
    assert_ne!(
        manager.agent_control().get_status(child_thread_id).await,
        AgentStatus::NotFound
    );
    assert_ne!(
        manager
            .agent_control()
            .get_status(grandchild_thread_id)
            .await,
        AgentStatus::NotFound
    );

    let close_again_output = CloseAgentHandler
        .handle(invocation(
            parent_session.clone(),
            parent_session.new_default_turn().await,
            "close_agent",
            function_payload(json!({"target": child_thread_id.to_string()})),
        ))
        .await
        .expect("close_agent should be repeatable for the child subtree");
    let (close_again_content, close_again_success) = expect_text_output(close_again_output);
    let close_again_result: close_agent::CloseAgentResult =
        serde_json::from_str(&close_again_content)
            .expect("second close_agent result should be json");
    assert_ne!(close_again_result.previous_status, AgentStatus::NotFound);
    assert_eq!(close_again_success, Some(true));
    assert_eq!(
        manager.agent_control().get_status(child_thread_id).await,
        AgentStatus::NotFound
    );
    assert_eq!(
        manager
            .agent_control()
            .get_status(grandchild_thread_id)
            .await,
        AgentStatus::NotFound
    );

    let operator = manager
        .start_thread(StartThreadOptions::new(config.clone()))
        .await
        .expect("operator thread should start");
    let operator_session = operator.thread.session.clone();
    let _ = manager
        .agent_control()
        .shutdown_live_agent(parent_thread_id)
        .await
        .expect("parent shutdown should succeed");
    assert_eq!(
        manager.agent_control().get_status(parent_thread_id).await,
        AgentStatus::NotFound
    );

    let parent_resume_output = ResumeAgentHandler
        .handle(invocation(
            operator_session,
            operator.thread.session.new_default_turn().await,
            "resume_agent",
            function_payload(json!({"id": parent_thread_id.to_string()})),
        ))
        .await
        .expect("resume_agent should reopen the parent thread");
    let (parent_resume_content, parent_resume_success) = expect_text_output(parent_resume_output);
    let parent_resume_result: resume_agent::ResumeAgentResult =
        serde_json::from_str(&parent_resume_content).expect("parent resume result should be json");
    assert_ne!(parent_resume_result.status, AgentStatus::NotFound);
    assert_eq!(parent_resume_success, Some(true));
    assert_ne!(
        manager.agent_control().get_status(parent_thread_id).await,
        AgentStatus::NotFound
    );
    assert_eq!(
        manager.agent_control().get_status(child_thread_id).await,
        AgentStatus::NotFound
    );
    assert_eq!(
        manager
            .agent_control()
            .get_status(grandchild_thread_id)
            .await,
        AgentStatus::NotFound
    );

    let shutdown_report = manager
        .shutdown_all_threads_bounded(Duration::from_secs(5))
        .await;
    assert_eq!(shutdown_report.submit_failed, Vec::<ThreadId>::new());
    assert_eq!(shutdown_report.timed_out, Vec::<ThreadId>::new());
}

#[tokio::test]
async fn build_agent_spawn_config_uses_turn_context_values() {
    fn pick_allowed_sandbox_policy(
        permissions: &crate::config::Permissions,
        base: SandboxPolicy,
        cwd: &std::path::Path,
    ) -> SandboxPolicy {
        let candidates = [
            SandboxPolicy::new_read_only_policy(),
            SandboxPolicy::new_workspace_write_policy(),
            SandboxPolicy::DangerFullAccess,
        ];
        candidates
            .into_iter()
            .find(|candidate| {
                if *candidate == base {
                    return false;
                }
                permissions
                    .can_set_legacy_sandbox_policy(candidate, cwd)
                    .is_ok()
            })
            .unwrap_or(base)
    }

    let (_session, mut turn) = make_session_and_context().await;
    let base_instructions = BaseInstructions {
        text: "base".to_string(),
    };
    turn.developer_instructions = Some("dev".to_string());
    let mut config = (*turn.config).clone();
    config.compact_prompt = Some("compact".to_string());
    config.permissions.shell_environment_policy = ShellEnvironmentPolicy {
        use_profile: true,
        ..ShellEnvironmentPolicy::default()
    };
    config.codex_linux_sandbox_exe = Some(PathBuf::from("/bin/echo"));
    turn.config = Arc::new(config);
    let temp_dir = tempfile::tempdir().expect("temp dir");
    #[allow(deprecated)]
    {
        turn.cwd = temp_dir.abs();
    }
    #[allow(deprecated)]
    let turn_cwd = turn.cwd.clone();
    let sandbox_policy = pick_allowed_sandbox_policy(
        &turn.config.permissions,
        turn.config.legacy_sandbox_policy(),
        turn_cwd.as_path(),
    );
    let file_system_sandbox_policy =
        FileSystemSandboxPolicy::from_legacy_sandbox_policy_for_cwd(&sandbox_policy, &turn_cwd);
    let network_sandbox_policy = NetworkSandboxPolicy::from(&sandbox_policy);
    let permission_profile = PermissionProfile::from_runtime_permissions_with_enforcement(
        SandboxEnforcement::from_legacy_sandbox_policy(&sandbox_policy),
        &file_system_sandbox_policy,
        network_sandbox_policy,
    );
    turn.permission_profile = permission_profile.clone();
    turn.approval_policy
        .set(AskForApproval::OnRequest)
        .expect("approval policy set");

    let config = build_agent_spawn_config(&base_instructions, &turn).expect("spawn config");
    let mut expected = (*turn.config).clone();
    expected.base_instructions = Some(base_instructions.text);
    expected.model = Some(turn.model_info.slug.clone());
    expected.model_provider = turn.provider.info().clone();
    expected.model_reasoning_effort = turn.reasoning_effort.clone();
    expected.model_reasoning_summary = Some(turn.reasoning_summary);
    expected.developer_instructions = turn.developer_instructions.clone();
    #[allow(deprecated)]
    {
        expected.cwd = turn.cwd.clone();
    }
    expected
        .permissions
        .approval_policy
        .set(AskForApproval::OnRequest)
        .expect("approval policy set");
    expected
        .permissions
        .set_permission_profile(permission_profile)
        .expect("permission profile set");
    assert_eq!(config, expected);
}

#[tokio::test]
async fn build_agent_resume_config_clears_base_instructions() {
    let (_session, mut turn) = make_session_and_context().await;
    let mut base_config = (*turn.config).clone();
    base_config.base_instructions = Some("caller-base".to_string());
    turn.config = Arc::new(base_config);
    turn.approval_policy
        .set(AskForApproval::OnRequest)
        .expect("approval policy set");

    let config = build_agent_resume_config(&turn).expect("resume config");

    let mut expected = (*turn.config).clone();
    expected.base_instructions = None;
    expected.model = Some(turn.model_info.slug.clone());
    expected.model_provider = turn.provider.info().clone();
    expected.model_reasoning_effort = turn.reasoning_effort.clone();
    expected.model_reasoning_summary = Some(turn.reasoning_summary);
    expected.developer_instructions = turn.developer_instructions.clone();
    #[allow(deprecated)]
    {
        expected.cwd = turn.cwd.clone();
    }
    expected
        .permissions
        .approval_policy
        .set(AskForApproval::OnRequest)
        .expect("approval policy set");
    expected
        .permissions
        .set_permission_profile(turn.permission_profile())
        .expect("permission profile set");
    assert_eq!(config, expected);
}

/// PF-84-S03: a parent on Z.AI (on `parent_account` when set) spawns a child
/// with `extra` spawn arguments. Returns the spawn result and the accounts of
/// the spawned children (`<provider>:<name>`).
async fn spawn_with_account(
    named_accounts: bool,
    configure: impl FnOnce(&mut crate::config::Config, &mut TurnContext),
    parent_account: Option<&str>,
    extra: serde_json::Value,
) -> (Result<(), FunctionCallError>, Vec<Option<String>>) {
    let (result, children, _asked) = spawn_with_account_answering(
        named_accounts,
        configure,
        parent_account,
        extra,
        /*answer*/ None,
        /*raised_level*/ None,
    )
    .await;
    (result, children)
}

/// [`spawn_with_account`] with a turn that can ask the human: a security
/// question is answered with `answer` (`Allow once`, `Cancel`, or empty for no
/// answer). Also returns whether a question was asked.
async fn spawn_with_account_answering(
    named_accounts: bool,
    configure: impl FnOnce(&mut crate::config::Config, &mut TurnContext),
    parent_account: Option<&str>,
    extra: serde_json::Value,
    answer: Option<&str>,
    raised_level: Option<codex_security_policy::SecurityLevel>,
) -> (Result<(), FunctionCallError>, Vec<Option<String>>, bool) {
    let (mut session, mut turn) = make_session_and_context().await;
    let mut provider_info = ModelProviderInfo::create_zai_provider();
    provider_info.account =
        parent_account.map(|name| codex_model_provider_info::NamedProviderAccount {
            provider_id: "zai".to_string(),
            name: name.to_string(),
        });
    let mut config = (*turn.config).clone();
    config.model_provider_id = "zai".to_string();
    config.model_provider = provider_info;
    config
        .model_providers
        .insert("zai".to_string(), ModelProviderInfo::create_zai_provider());
    config
        .features
        .enable(Feature::MultiAgentV2)
        .expect("test config should allow feature update");
    if named_accounts {
        config
            .features
            .enable(Feature::NamedAccounts)
            .expect("test config should allow feature update");
    }
    codex_vault::Vault::new(config.codex_home.to_path_buf())
        .write_provider_account(
            "zai",
            &codex_vault::ProviderAccountName::parse("work").expect("name"),
            codex_vault::ProviderAccountKind::ApiKey,
            "canary-spawn-work",
        )
        .expect("write account");
    configure(&mut config, &mut turn);
    turn.provider = create_model_provider(config.model_provider.clone(), turn.auth_manager.clone());
    set_turn_config(&mut turn, config);
    let manager = thread_manager();
    let root = manager
        .start_thread((*turn.config).clone())
        .await
        .expect("root thread should start");
    // The root's own control plane carries its security policy binding.
    session.services.agent_control = root.thread.session.services.agent_control.clone();
    session.thread_id = root.thread_id;
    if let Some(level) = raised_level {
        let controller = session
            .services
            .agent_control
            .trusted_security_controller()
            .expect("root security controller");
        let change = controller
            .confirm_level_change(level, codex_security_policy::RevocationState::new())
            .expect("confirm level change");
        controller
            .apply_confirmed_change(change)
            .expect("apply level change");
    }
    let mut arguments = json!({
        "message": "say pong",
        "task_name": "worker",
        "fork_turns": "none",
    });
    for (key, value) in extra.as_object().expect("object").clone() {
        arguments[key] = value;
    }
    if answer.is_some() {
        *session.active_turn.lock().await = Some(crate::state::ActiveTurn::default());
    }
    let session = Arc::new(session);
    let sub_id = turn.sub_id.clone();
    let handler = SpawnAgentHandlerV2::default();
    let call = handler.handle(invocation(
        Arc::clone(&session),
        Arc::new(turn),
        "spawn_agent",
        function_payload(arguments),
    ));
    tokio::pin!(call);
    let mut asked = false;
    let result = tokio::time::timeout(std::time::Duration::from_secs(30), async {
        loop {
            tokio::select! {
                result = &mut call => break result,
                () = tokio::time::sleep(std::time::Duration::from_millis(10)),
                    if answer.is_some() && !asked =>
                {
                    let turn_state = session
                        .active_turn
                        .lock()
                        .await
                        .as_ref()
                        .map(|active| Arc::clone(&active.turn_state));
                    let pending = match turn_state {
                        Some(turn_state) => turn_state.lock().await.remove_pending_user_input(&sub_id),
                        None => None,
                    };
                    if let (Some(pending), Some(answer)) = (pending, answer) {
                        asked = true;
                        // An empty answer stands for a client that cannot ask
                        // (`corbanu exec` rejects the request with no answers).
                        let answers = if answer.is_empty() {
                            std::collections::HashMap::new()
                        } else {
                            std::collections::HashMap::from([(
                                crate::security::protected_surface::question_id("call-1"),
                                codex_protocol::request_user_input::RequestUserInputAnswer {
                                    answers: vec![answer.to_string()],
                                },
                            )])
                        };
                        let response =
                            codex_protocol::request_user_input::RequestUserInputResponse { answers };
                        let _ = pending.send(response);
                    }
                }
            }
        }
    })
    .await
    .expect("spawn must not wait for an unanswered approval")
    .map(drop);
    let mut children = Vec::new();
    for thread_id in manager.list_thread_ids().await {
        if thread_id != root.thread_id {
            let thread = manager.get_thread(thread_id).await.expect("child thread");
            children.push(thread.config_snapshot().await.provider_account);
        }
    }
    (result, children, asked)
}

#[tokio::test]
async fn spawn_account_needs_the_named_accounts_feature() {
    let (result, _) = spawn_with_account(
        /*named_accounts*/ false,
        |_, _| {},
        /*parent_account*/ None,
        json!({"account": "work"}),
    )
    .await;
    assert_eq!(
        result,
        Err(FunctionCallError::RespondToModel(
            "spawn_agent `account` needs the `named_accounts` feature; omit it to use the parent's account.".to_string()
        ))
    );
}

#[tokio::test]
async fn spawn_account_must_be_configured_for_the_child_provider() {
    let (result, _) = spawn_with_account(
        /*named_accounts*/ true,
        |_, _| {},
        /*parent_account*/ None,
        json!({"account": "gone"}),
    )
    .await;
    assert_eq!(
        result,
        Err(FunctionCallError::RespondToModel(
            "account `gone` of provider `zai` is not configured; add it with `corbanu account add zai gone` or pick another with `--account` (see `corbanu account list`). Configured accounts of `zai`: `default`, work. Do not retry on another account without the user's consent.".to_string()
        ))
    );
}

#[tokio::test]
async fn aggressive_spawn_account_switch_needs_an_approval() {
    let (result, children) = spawn_with_account(
        /*named_accounts*/ true,
        |config, turn| {
            config.security_level = codex_security_policy::SecurityLevel::Aggressive;
            turn.approval_policy
                .set(AskForApproval::Never)
                .expect("approval policy should be set");
        },
        /*parent_account*/ None,
        json!({"account": "work"}),
    )
    .await;
    assert_eq!(children, Vec::<Option<String>>::new());
    assert_eq!(
        result,
        Err(FunctionCallError::RespondToModel(
            "Running a spawned agent on account `work` of `zai` needs the user's approval under the Aggressive security level, and approvals are off.".to_string()
        ))
    );
}

/// #428 (Travis 2026-10-11, option 1): the session's permissions as an
/// Aggressive launch of Corbanu Terminal builds them while Core's level stays
/// Permissive. `profile` selects the Aggressive permission profile; `marker`
/// sets the origin marker. Both are set by every such launch: the states the
/// TUI shows as "Aggressive is enforced; choose a level in /security" (level
/// file missing, Aggressive rules present), "Aggressive is enforced, but the
/// protected boundary is unverified" (saved without a preflight) and "Active
/// in this session: Aggressive" (Permissive saved for the next start).
async fn tui_aggressive_permissions(
    codex_home: &std::path::Path,
    profile: bool,
    marker: bool,
) -> crate::config::Permissions {
    let profile_id = codex_security_level::level::AGGRESSIVE_PROFILE_ID;
    let mut overrides = Vec::new();
    if profile {
        overrides.push((
            format!("permissions.{profile_id}"),
            toml::Value::Table(toml::toml! {
                extends = ":workspace"
                [network]
                enabled = false
            }),
        ));
        overrides.push((
            "default_permissions".to_string(),
            toml::Value::String(profile_id.to_string()),
        ));
    }
    if marker {
        overrides.push((
            format!(
                "shell_environment_policy.set.{}",
                codex_security_level::nested::ORIGIN_ENV
            ),
            toml::Value::String(codex_home.to_string_lossy().into_owned()),
        ));
    }
    let config = crate::config::ConfigBuilder::without_managed_config_for_tests()
        .codex_home(codex_home.to_path_buf())
        .harness_overrides(crate::session::tests::kept_test_cwd_overrides())
        .cli_overrides(overrides)
        .build()
        .await
        .expect("config with the Aggressive launch overrides");
    assert_eq!(
        (
            config.security_level,
            config.permissions.launch_enforces_aggressive()
        ),
        (
            codex_security_policy::SecurityLevel::Permissive,
            profile || marker
        )
    );
    config.permissions
}

#[tokio::test]
async fn tui_aggressive_spawn_account_switch_is_refused_with_approvals_off() {
    for (profile, marker) in [(true, true), (true, false), (false, true)] {
        let home = tempfile::tempdir().expect("home");
        let permissions = tui_aggressive_permissions(home.path(), profile, marker).await;
        let (result, children) = spawn_with_account(
            /*named_accounts*/ true,
            |config, turn| {
                config.permissions = permissions;
                turn.approval_policy
                    .set(AskForApproval::Never)
                    .expect("approval policy should be set");
            },
            /*parent_account*/ None,
            json!({"account": "work"}),
        )
        .await;
        assert_eq!(
            (result, children),
            (
                Err(FunctionCallError::RespondToModel(
                    "Running a spawned agent on account `work` of `zai` needs the user's approval under the Aggressive security level, and approvals are off.".to_string()
                )),
                Vec::new()
            ),
            "profile={profile} marker={marker}"
        );
    }
}

#[tokio::test]
async fn tui_aggressive_spawn_account_switch_asks_the_human() {
    for (answer, expected) in [
        (
            "Cancel",
            (
                Err(FunctionCallError::RespondToModel(
                    "The user declined running the spawned agent on account `work` of `zai`. Do not retry on another account without the user's consent.".to_string(),
                )),
                Vec::new(),
                true,
            ),
        ),
        (
            "",
            (
                Err(FunctionCallError::RespondToModel(
                    "Running a spawned agent on account `work` of `zai` needs the user's approval under the Aggressive security level, and no answer was given (this session cannot ask, or the question was dismissed). Do not retry on another account without the user's consent.".to_string(),
                )),
                Vec::new(),
                true,
            ),
        ),
        ("Allow once", (Ok(()), vec![Some("zai:work".to_string())], true)),
    ] {
        let home = tempfile::tempdir().expect("home");
        let permissions = tui_aggressive_permissions(home.path(), /*profile*/ true, /*marker*/ true).await;
        let outcome = spawn_with_account_answering(
            /*named_accounts*/ true,
            |config, turn| {
                config.permissions = permissions;
                turn.approval_policy
                    .set(AskForApproval::UnlessTrusted)
                    .expect("approval policy should be set");
            },
            /*parent_account*/ None,
            json!({"account": "work"}),
            Some(answer),
            /*raised_level*/ None,
        )
        .await;
        assert_eq!(outcome, expected, "{answer}");
    }
}

#[tokio::test]
async fn permissive_spawn_account_switch_asks_nothing_even_when_it_could() {
    let (result, children, asked) = spawn_with_account_answering(
        /*named_accounts*/ true,
        |_, turn| {
            turn.approval_policy
                .set(AskForApproval::UnlessTrusted)
                .expect("approval policy should be set");
        },
        /*parent_account*/ None,
        json!({"account": "work"}),
        Some("Cancel"),
        /*raised_level*/ None,
    )
    .await;
    assert_eq!(
        (result, children, asked),
        (Ok(()), vec![Some("zai:work".to_string())], false)
    );
}

/// PF-84-S03 follow-up: D3 checks the level in force at spawn time, so a
/// session started Permissive and raised to Aggressive mid-session needs the
/// approval too.
#[tokio::test]
async fn spawn_account_switch_after_a_mid_session_raise_needs_an_approval() {
    let (result, children, _asked) = spawn_with_account_answering(
        /*named_accounts*/ true,
        |config, turn| {
            config.security_level = codex_security_policy::SecurityLevel::Permissive;
            turn.approval_policy
                .set(AskForApproval::Never)
                .expect("approval policy should be set");
        },
        /*parent_account*/ None,
        json!({"account": "work"}),
        /*answer*/ None,
        Some(codex_security_policy::SecurityLevel::Aggressive),
    )
    .await;
    assert_eq!(children, Vec::<Option<String>>::new());
    assert_eq!(
        result,
        Err(FunctionCallError::RespondToModel(
            "Running a spawned agent on account `work` of `zai` needs the user's approval under the Aggressive security level, and approvals are off.".to_string()
        ))
    );
}

#[tokio::test]
async fn spawn_account_switch_needs_no_prompt_below_aggressive() {
    let (result, children) = spawn_with_account(
        /*named_accounts*/ true,
        |_, _| {},
        /*parent_account*/ None,
        json!({"account": "work"}),
    )
    .await;
    assert_eq!(
        (result, children),
        (Ok(()), vec![Some("zai:work".to_string())])
    );
}

/// #689: a parent on Z.AI account `work` spawns on the Claude-style
/// connection, which runs on the default account; with no default key that
/// child would fail at its first request, so it is refused before it starts.
/// Explicit `account: default` on the parent's own connection is refused the
/// same way. With a default key the spawn goes ahead. The key variable is
/// unique to this test so the ambient environment cannot supply it.
#[tokio::test]
async fn spawn_on_a_connection_without_its_own_credential_is_refused() {
    const KEY: &str = "CORBANU_TEST_689_ZAI_KEY";
    let use_test_key = |config: &mut crate::config::Config, _: &mut TurnContext| {
        for id in [ZAI_PROVIDER_ID, "zai-anthropic"] {
            let mut provider = if id == ZAI_PROVIDER_ID {
                ModelProviderInfo::create_zai_provider()
            } else {
                ModelProviderInfo::create_zai_anthropic_provider()
            };
            provider.env_key = Some(KEY.to_string());
            config.model_providers.insert(id.to_string(), provider);
        }
        config.model_provider.env_key = Some(KEY.to_string());
    };
    let refusal = |child: &str| {
        format!(
            "spawn_agent: `{child}` would run on its default account, which has no credential \
             (`{KEY}` is not set and no default key is stored); accounts are selected per \
             connection, and the parent runs on `zai` account `work`. Spawn without the \
             `model_provider`, `account` or `agent_type` that chose this connection and account \
             to run on the parent's, or ask the user to add a default `{KEY}` (`/providers`). \
             Do not retry on another account without the user's consent."
        )
    };
    for (extra, child) in [
        (
            json!({"model_provider": "zai-anthropic", "model": "glm-5.2"}),
            "zai-anthropic",
        ),
        (json!({"account": "default"}), ZAI_PROVIDER_ID),
    ] {
        let (result, children) = spawn_with_account(
            /*named_accounts*/ true,
            use_test_key,
            /*parent_account*/ Some("work"),
            extra.clone(),
        )
        .await;
        assert_eq!(
            (result, children),
            (
                Err(FunctionCallError::RespondToModel(refusal(child))),
                Vec::new()
            ),
            "{extra}"
        );
    }
    // The parent's own connection and account still spawn.
    let (result, children) = spawn_with_account(
        /*named_accounts*/ true,
        use_test_key,
        /*parent_account*/ Some("work"),
        json!({}),
    )
    .await;
    assert_eq!(
        (result, children),
        (Ok(()), vec![Some("zai:work".to_string())])
    );
    // With a stored default key the Claude-style connection spawns.
    let (result, children) = spawn_with_account(
        /*named_accounts*/ true,
        |config, turn| {
            use_test_key(config, turn);
            codex_login::login_with_provider_api_key(
                &config.codex_home,
                KEY,
                "canary-689-default",
                codex_login::AuthCredentialsStoreMode::File,
                codex_login::AuthKeyringBackendKind::default(),
            )
            .expect("store default key");
        },
        /*parent_account*/ Some("work"),
        json!({"model_provider": "zai-anthropic", "model": "glm-5.2"}),
    )
    .await;
    assert_eq!(
        (result, children),
        (Ok(()), vec![Some("zai-anthropic:default".to_string())])
    );
}

#[tokio::test]
async fn spawned_children_inherit_the_parent_account_even_with_a_role() {
    for extra in [json!({}), json!({"agent_type": "explorer"})] {
        let (result, children) = spawn_with_account(
            /*named_accounts*/ true,
            |_, _| {},
            /*parent_account*/ Some("work"),
            extra.clone(),
        )
        .await;
        assert_eq!(
            (result, children),
            (Ok(()), vec![Some("zai:work".to_string())]),
            "{extra}"
        );
    }
}
