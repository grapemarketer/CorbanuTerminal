use std::collections::BTreeMap;
use std::sync::Arc;

use codex_features::Feature;
use codex_login::AuthManager;
use codex_login::CodexAuth;
use codex_mcp::ToolInfo;
use codex_model_provider::create_model_provider;
use codex_model_provider_info::AMAZON_BEDROCK_PROVIDER_ID;
use codex_model_provider_info::CLAUDE_PLAN_PROVIDER_ID;
use codex_model_provider_info::ModelProviderInfo;
use codex_model_provider_info::OPENAI_PROVIDER_ID;
use codex_model_provider_info::OPENROUTER_PROVIDER_ID;
use codex_model_provider_info::ZAI_PROVIDER_ID;
use codex_models_manager::model_info;
use codex_protocol::config_types::WebSearchMode;
use codex_protocol::dynamic_tools::DynamicToolSpec;
use codex_protocol::models::PermissionProfile;
use codex_protocol::openai_models::ApplyPatchToolType;
use codex_protocol::openai_models::ConfigShellToolType;
use codex_protocol::openai_models::InputModality;
use codex_protocol::openai_models::ToolMode;
use codex_protocol::openai_models::WebSearchToolType;
use codex_tools::DiscoverablePluginInfo;
use codex_tools::DiscoverableTool;
use codex_tools::ResponsesApiNamespaceTool;
use codex_tools::ResponsesApiTool;
use codex_tools::ToolCall as ExtensionToolCall;
use codex_tools::ToolExecutor;
use codex_tools::ToolExposure;
use codex_tools::ToolName;
use codex_tools::ToolOutput;
use codex_tools::ToolSpec;
use pretty_assertions::assert_eq;
use serde_json::json;

use crate::WaitForEnvironmentToolConfig;
use crate::config::CurrentTimeReminderConfig;
use crate::environment_selection::TurnEnvironmentState;
use crate::responses_metadata::CodexResponsesRequestKind;
use crate::session::step_context::StepContext;
use crate::session::tests::make_session_and_context;
use crate::session::tests::mcp_config_for_test;
use crate::session::turn_context::TurnContext;
use crate::tools::handlers::McpHandler;
use crate::tools::handlers::ToolSearchHandlerCache;
use crate::tools::handlers::WaitForEnvironmentHandler;
use crate::tools::handlers::multi_agents_spec::MULTI_AGENT_V1_NAMESPACE;
use crate::tools::registry::CoreToolRuntime;
use crate::tools::registry::override_tool_exposure;
use crate::tools::router::ToolRouter;
use crate::tools::router::ToolSuggestCandidates;
use crate::tools::router::ToolSuggestPresentation;
use crate::tools::spec_plan::append_source_tool_runtimes;
use crate::tools::spec_plan::build_core_tool_runtimes;

const MULTI_AGENT_V2_NAMESPACE: &str = "collaboration";

#[derive(Default)]
struct ToolPlanInputs {
    tool_runtimes: Vec<Arc<dyn CoreToolRuntime>>,
    tool_suggest_candidates: Option<ToolSuggestCandidates>,
    extension_tool_executors: Vec<Arc<dyn ToolExecutor<ExtensionToolCall>>>,
    wait_for_environment_tool_config: Option<Arc<WaitForEnvironmentToolConfig>>,
    dynamic_tools: Vec<DynamicToolSpec>,
}

struct ToolPlanProbe {
    visible_specs: Vec<ToolSpec>,
    visible_names: Vec<String>,
    namespace_functions: BTreeMap<String, Vec<String>>,
    registered_names: Vec<String>,
    exposures: BTreeMap<String, ToolExposure>,
}

impl ToolPlanProbe {
    fn from_router(router: ToolRouter) -> Self {
        let visible_specs = router.model_visible_specs();
        let visible_names = visible_specs
            .iter()
            .map(|spec| spec.name().to_string())
            .collect::<Vec<_>>();
        let namespace_functions = visible_specs
            .iter()
            .filter_map(|spec| match spec {
                ToolSpec::Namespace(namespace) => Some((
                    namespace.name.clone(),
                    namespace
                        .tools
                        .iter()
                        .map(|tool| match tool {
                            ResponsesApiNamespaceTool::Function(tool) => tool.name.clone(),
                        })
                        .collect::<Vec<_>>(),
                )),
                ToolSpec::Function(_)
                | ToolSpec::ToolSearch { .. }
                | ToolSpec::WebSearch { .. }
                | ToolSpec::Freeform(_) => None,
            })
            .collect::<BTreeMap<_, _>>();
        let registered_tool_names = router.registered_tool_names_for_test();
        let registered_names = registered_tool_names
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>();
        let exposures = registered_tool_names
            .iter()
            .filter_map(|name| {
                router
                    .tool_exposure_for_test(name)
                    .map(|exposure| (name.to_string(), exposure))
            })
            .collect::<BTreeMap<_, _>>();

        Self {
            visible_specs,
            visible_names,
            namespace_functions,
            registered_names,
            exposures,
        }
    }

    fn assert_visible_contains(&self, expected: &[&str]) {
        for name in expected {
            assert!(
                self.visible_names.iter().any(|visible| visible == name),
                "expected visible tool `{name}` in {:?}",
                self.visible_names
            );
        }
    }

    fn assert_visible_lacks(&self, expected_absent: &[&str]) {
        for name in expected_absent {
            assert!(
                !self.visible_names.iter().any(|visible| visible == name),
                "expected visible tool `{name}` to be absent from {:?}",
                self.visible_names
            );
        }
    }

    fn assert_registered_contains(&self, expected: &[&str]) {
        for name in expected {
            assert!(
                self.registered_names
                    .iter()
                    .any(|registered| registered == name),
                "expected registered tool `{name}` in {:?}",
                self.registered_names
            );
        }
    }

    fn assert_registered_lacks(&self, expected_absent: &[&str]) {
        for name in expected_absent {
            assert!(
                !self
                    .registered_names
                    .iter()
                    .any(|registered| registered == name),
                "expected registered tool `{name}` to be absent from {:?}",
                self.registered_names
            );
        }
    }

    fn namespace_function_names(&self, namespace: &str) -> &[String] {
        self.namespace_functions
            .get(namespace)
            .map_or(&[], Vec::as_slice)
    }

    fn visible_spec(&self, name: &str) -> &ToolSpec {
        self.visible_specs
            .iter()
            .find(|spec| spec.name() == name)
            .unwrap_or_else(|| panic!("expected visible spec `{name}` in {:?}", self.visible_names))
    }

    fn exposure(&self, name: &str) -> ToolExposure {
        *self
            .exposures
            .get(name)
            .unwrap_or_else(|| panic!("expected registered tool `{name}`"))
    }
}

async fn probe_with(
    configure_turn: impl FnOnce(&mut TurnContext),
    inputs: ToolPlanInputs,
) -> ToolPlanProbe {
    ToolPlanProbe::from_router(planned_router(configure_turn, inputs).await)
}

async fn planned_router(
    configure_turn: impl FnOnce(&mut TurnContext),
    inputs: ToolPlanInputs,
) -> ToolRouter {
    let (_session, mut turn) = make_session_and_context().await;
    use_openai_provider(&mut turn);
    configure_turn(&mut turn);
    let turn = Arc::new(turn);
    let step_context = StepContext::for_test(Arc::clone(&turn));
    let mut tool_runtimes = build_core_tool_runtimes(
        step_context.turn.as_ref(),
        &step_context.environments,
        step_context.mcp.as_ref(),
        inputs.tool_suggest_candidates.as_ref(),
        inputs.wait_for_environment_tool_config.as_ref(),
    );
    let hosted_specs = append_source_tool_runtimes(
        step_context.turn.as_ref(),
        &mut tool_runtimes,
        inputs.tool_runtimes,
        inputs.extension_tool_executors,
        &inputs.dynamic_tools,
    );
    ToolRouter::from_tools(
        step_context.turn.as_ref(),
        tool_runtimes,
        hosted_specs,
        &Default::default(),
    )
}

async fn probe(configure_turn: impl FnOnce(&mut TurnContext)) -> ToolPlanProbe {
    probe_with(configure_turn, ToolPlanInputs::default()).await
}

fn set_feature(turn: &mut TurnContext, feature: Feature, enabled: bool) {
    let mut config = (*turn.config).clone();
    if enabled {
        config
            .features
            .enable(feature)
            .expect("test feature should be enableable in config");
    } else {
        config
            .features
            .disable(feature)
            .expect("test feature should be disableable in config");
    }
    turn.multi_agent_version = config.multi_agent_version_from_features();
    turn.config = Arc::new(config);
}

fn set_features(turn: &mut TurnContext, features: &[Feature]) {
    for feature in features {
        set_feature(turn, *feature, /*enabled*/ true);
    }
}

fn zsh_fork_config_for_spec_plan_tests() -> codex_tools::ZshForkConfig {
    let placeholder_exe = codex_utils_absolute_path::AbsolutePathBuf::try_from(
        std::env::current_exe().expect("current exe path"),
    )
    .expect("current exe should be absolute");

    // Spec planning only checks whether the shell mode is ZshFork. These paths
    // are never executed, so use a stable absolute placeholder instead of
    // depending on packaged zsh-fork artifacts in schema tests.
    codex_tools::ZshForkConfig {
        shell_zsh_path: placeholder_exe.clone(),
        main_execve_wrapper_exe: placeholder_exe,
    }
}

fn update_config(turn: &mut TurnContext, update: impl FnOnce(&mut crate::config::Config)) {
    let mut config = (*turn.config).clone();
    update(&mut config);
    turn.config = Arc::new(config);
}

fn set_web_search_mode(turn: &mut TurnContext, mode: WebSearchMode) {
    update_config(turn, |config| {
        config
            .web_search_mode
            .set(mode)
            .expect("test web search mode should be accepted");
    });
}

fn use_chatgpt_auth(turn: &mut TurnContext) {
    use_openai_provider(turn);
    turn.auth_manager = Some(AuthManager::from_auth_for_testing(
        CodexAuth::create_dummy_chatgpt_auth_for_testing(),
    ));
    turn.provider = create_model_provider(
        turn.config.model_provider.clone(),
        turn.auth_manager.clone(),
    );
}

fn use_api_key_auth(turn: &mut TurnContext) {
    use_openai_provider(turn);
    turn.auth_manager = Some(AuthManager::from_auth_for_testing(CodexAuth::from_api_key(
        "sk-test",
    )));
    turn.provider = create_model_provider(
        turn.config.model_provider.clone(),
        turn.auth_manager.clone(),
    );
}

fn use_openai_provider(turn: &mut TurnContext) {
    let provider_info = ModelProviderInfo::create_openai_provider(/*base_url*/ None);
    update_config(turn, |config| {
        config.model_provider_id = OPENAI_PROVIDER_ID.to_string();
        config.model_provider = provider_info.clone();
    });
    turn.provider = create_model_provider(provider_info, turn.auth_manager.clone());
    turn.model_info = model_info::model_info_from_slug("gpt-5.4");
}

fn use_bedrock_provider(turn: &mut TurnContext) {
    let provider_info = ModelProviderInfo::create_amazon_bedrock_provider(/*aws*/ None);
    update_config(turn, |config| {
        config.model_provider_id = AMAZON_BEDROCK_PROVIDER_ID.to_string();
        config.model_provider = provider_info.clone();
    });
    turn.provider = create_model_provider(provider_info, turn.auth_manager.clone());
}

fn use_zai_provider(turn: &mut TurnContext) {
    let provider_info = ModelProviderInfo::create_zai_provider();
    update_config(turn, |config| {
        config.model_provider_id = ZAI_PROVIDER_ID.to_string();
        config.model_provider = provider_info.clone();
    });
    turn.provider = create_model_provider(provider_info, turn.auth_manager.clone());
}

fn use_openrouter_provider(turn: &mut TurnContext) {
    let provider_info = ModelProviderInfo::create_openrouter_provider();
    update_config(turn, |config| {
        config.model_provider_id = OPENROUTER_PROVIDER_ID.to_string();
        config.model_provider = provider_info.clone();
    });
    turn.provider = create_model_provider(provider_info, turn.auth_manager.clone());
}

fn use_claude_plan_provider(turn: &mut TurnContext) {
    let provider_info = ModelProviderInfo::create_claude_plan_provider();
    update_config(turn, |config| {
        config.model_provider_id = CLAUDE_PLAN_PROVIDER_ID.to_string();
        config.model_provider = provider_info.clone();
    });
    turn.provider = create_model_provider(provider_info, turn.auth_manager.clone());
    turn.model_info = model_info::model_info_from_slug("claude-fable-5-plan");
}

struct TestNamespaceExtensionTool {
    namespace: &'static str,
    tool_name: &'static str,
}

impl ToolExecutor<ExtensionToolCall> for TestNamespaceExtensionTool {
    fn tool_name(&self) -> ToolName {
        ToolName::namespaced(self.namespace, self.tool_name)
    }

    fn spec(&self) -> ToolSpec {
        ToolSpec::Namespace(codex_tools::ResponsesApiNamespace {
            name: self.namespace.to_string(),
            description: "Test namespace.".to_string(),
            tools: vec![ResponsesApiNamespaceTool::Function(ResponsesApiTool {
                name: self.tool_name.to_string(),
                description: "Test namespace tool.".to_string(),
                strict: false,
                defer_loading: None,
                parameters: codex_tools::JsonSchema::default(),
                output_schema: None,
            })],
        })
    }

    fn handle(&self, _call: ExtensionToolCall) -> codex_tools::ToolExecutorFuture<'_> {
        Box::pin(async {
            Ok(Box::new(codex_tools::JsonToolOutput::new(json!({}))) as Box<dyn ToolOutput>)
        })
    }
}

struct DeferredExtensionTool;

impl ToolExecutor<ExtensionToolCall> for DeferredExtensionTool {
    fn tool_name(&self) -> ToolName {
        ToolName::plain("extension_echo")
    }

    fn spec(&self) -> ToolSpec {
        ToolSpec::Function(ResponsesApiTool {
            name: "extension_echo".to_string(),
            description: "Echoes arguments through an extension tool.".to_string(),
            strict: true,
            defer_loading: None,
            parameters: codex_tools::JsonSchema::object(
                BTreeMap::from([(
                    "message".to_string(),
                    codex_tools::JsonSchema::string(/*description*/ None),
                )]),
                Some(vec!["message".to_string()]),
                Some(false.into()),
            ),
            output_schema: None,
        })
    }

    fn exposure(&self) -> ToolExposure {
        ToolExposure::Deferred
    }

    fn handle(&self, _call: ExtensionToolCall) -> codex_tools::ToolExecutorFuture<'_> {
        Box::pin(async { panic!("spec planning should not execute extension tools") })
    }
}

fn duplicate_primary_environment(turn: &mut TurnContext) {
    let mut second_environment = turn
        .environments
        .primary()
        .expect("primary environment")
        .clone();
    second_environment.environment_id = "secondary".to_string();
    turn.environments
        .environments
        .push(TurnEnvironmentState::Ready(second_environment));
}

fn mcp_tool(server: &str, namespace: &str, name: &str) -> ToolInfo {
    ToolInfo {
        server_name: server.to_string(),
        supports_parallel_tool_calls: false,
        server_origin: None,
        callable_name: name.to_string(),
        callable_namespace: namespace.to_string(),
        namespace_description: Some(format!("Tools from {server}.")),
        tool: rmcp::model::Tool::new(
            name.to_string(),
            format!("{name} test tool"),
            Arc::new(rmcp::model::object(json!({
                "type": "object",
                "properties": {},
                "additionalProperties": false,
            }))),
        ),
        openai_file_input_optional_fields: Default::default(),
        connector_id: None,
        connector_name: None,
        plugin_display_names: Vec::new(),
    }
}

fn mcp_runtime(
    server: &str,
    namespace: &str,
    name: &str,
    exposure: ToolExposure,
) -> Arc<dyn CoreToolRuntime> {
    let handler: Arc<dyn CoreToolRuntime> = Arc::new(
        McpHandler::new(mcp_tool(server, namespace, name)).expect("MCP tool spec should build"),
    );
    override_tool_exposure(handler, exposure)
}

fn dynamic_tool(namespace: Option<&str>, name: &str, defer_loading: bool) -> DynamicToolSpec {
    let function = codex_protocol::dynamic_tools::DynamicToolFunctionSpec {
        name: name.to_string(),
        description: format!("{name} dynamic tool"),
        input_schema: json!({
            "type": "object",
            "properties": {},
            "additionalProperties": false,
        }),
        defer_loading,
    };
    match namespace {
        Some(namespace) => {
            DynamicToolSpec::Namespace(codex_protocol::dynamic_tools::DynamicToolNamespaceSpec {
                name: namespace.to_string(),
                description: format!("{namespace} dynamic tools"),
                tools: vec![
                    codex_protocol::dynamic_tools::DynamicToolNamespaceTool::Function(function),
                ],
            })
        }
        None => DynamicToolSpec::Function(function),
    }
}

fn plugin_candidates(presentation: ToolSuggestPresentation) -> ToolSuggestCandidates {
    ToolSuggestCandidates {
        tools: vec![DiscoverableTool::Plugin(Box::new(DiscoverablePluginInfo {
            id: "github@openai-curated-remote".to_string(),
            remote_plugin_id: None,
            name: "GitHub".to_string(),
            description: Some("Work with GitHub repositories".to_string()),
            has_skills: true,
            mcp_server_names: Vec::new(),
            app_connector_ids: Vec::new(),
        }))],
        presentation,
    }
}

fn has_parameter(spec: &ToolSpec, parameter_name: &str) -> bool {
    serde_json::to_value(spec)
        .expect("tool spec should serialize")
        .pointer(&format!("/parameters/properties/{parameter_name}"))
        .is_some()
}

fn apply_patch_accepts_environment_id(spec: &ToolSpec) -> bool {
    match spec {
        ToolSpec::Freeform(tool) if tool.name == "apply_patch" => {
            tool.format.definition.contains("Environment ID")
        }
        _ => false,
    }
}

#[tokio::test]
async fn wait_for_environment_requires_feature_and_uses_host_config_when_present() {
    const TOOL_DESCRIPTION: &str = "Host-provided wait tool description";
    const ENVIRONMENT_ID_DESCRIPTION: &str = "Host-provided environment ID description";

    for deferred_executor_enabled in [false, true] {
        for config_present in [false, true] {
            let wait_for_environment_tool_config = config_present.then(|| {
                Arc::new(WaitForEnvironmentToolConfig {
                    tool_description: TOOL_DESCRIPTION.to_string(),
                    environment_id_description: ENVIRONMENT_ID_DESCRIPTION.to_string(),
                })
            });
            let plan = probe_with(
                |turn| {
                    set_feature(turn, Feature::DeferredExecutor, deferred_executor_enabled);
                },
                ToolPlanInputs {
                    wait_for_environment_tool_config,
                    ..ToolPlanInputs::default()
                },
            )
            .await;

            if deferred_executor_enabled {
                plan.assert_visible_contains(&["wait_for_environment"]);
                plan.assert_registered_contains(&["wait_for_environment"]);
                if !config_present {
                    assert_eq!(
                        plan.visible_spec("wait_for_environment"),
                        &WaitForEnvironmentHandler::default().spec()
                    );
                    continue;
                }
                let ToolSpec::Function(ResponsesApiTool {
                    description,
                    parameters,
                    ..
                }) = plan.visible_spec("wait_for_environment")
                else {
                    panic!("expected wait_for_environment function spec");
                };
                assert_eq!(description, TOOL_DESCRIPTION);
                assert_eq!(
                    parameters
                        .properties
                        .as_ref()
                        .and_then(|properties| properties.get("environment_id"))
                        .and_then(|schema| schema.description.as_deref()),
                    Some(ENVIRONMENT_ID_DESCRIPTION)
                );
            } else {
                plan.assert_visible_lacks(&["wait_for_environment"]);
                plan.assert_registered_lacks(&["wait_for_environment"]);
            }
        }
    }
}

#[tokio::test]
async fn wait_for_environment_falls_back_for_oversized_host_configuration() {
    const MAX_COMBINED_DESCRIPTION_BYTES: usize = 1_024;

    for (tool_description, environment_id_description) in [
        (
            "x".repeat(MAX_COMBINED_DESCRIPTION_BYTES + 1),
            String::new(),
        ),
        (
            String::new(),
            "x".repeat(MAX_COMBINED_DESCRIPTION_BYTES + 1),
        ),
        ("x".repeat(512), "x".repeat(513)),
        // The descriptions fit the aggregate input cap, but the complete serialized schema does
        // not fit its model-context cap once the surrounding tool definition is included.
        ("x".repeat(500), "x".repeat(500)),
    ] {
        let configured_tool_description = tool_description.clone();
        let configured_environment_id_description = environment_id_description.clone();
        let plan = probe_with(
            |turn| {
                set_feature(turn, Feature::DeferredExecutor, /*enabled*/ true);
            },
            ToolPlanInputs {
                wait_for_environment_tool_config: Some(Arc::new(WaitForEnvironmentToolConfig {
                    tool_description,
                    environment_id_description,
                })),
                ..ToolPlanInputs::default()
            },
        )
        .await;

        plan.assert_visible_contains(&["wait_for_environment"]);
        plan.assert_registered_contains(&["wait_for_environment"]);
        let ToolSpec::Function(ResponsesApiTool {
            description,
            parameters,
            ..
        }) = plan.visible_spec("wait_for_environment")
        else {
            panic!("expected wait_for_environment function spec");
        };
        let environment_id_description = parameters
            .properties
            .as_ref()
            .and_then(|properties| properties.get("environment_id"))
            .and_then(|schema| schema.description.as_deref())
            .expect("environment_id description should be present");
        assert_ne!(description, &configured_tool_description);
        assert_ne!(
            environment_id_description,
            configured_environment_id_description
        );
        assert!(
            serde_json::to_vec(plan.visible_spec("wait_for_environment"))
                .expect("tool spec should serialize")
                .len()
                <= 1_000
        );
    }
}

#[tokio::test]
async fn request_user_input_tool_respects_experimental_config_gate() {
    let enabled = probe(|_| {}).await;
    enabled.assert_visible_contains(&["request_user_input"]);
    enabled.assert_registered_contains(&["request_user_input"]);
    assert_eq!(
        enabled.exposure("request_user_input"),
        ToolExposure::DirectModelOnly
    );

    let disabled = probe(|turn| {
        update_config(turn, |config| {
            config.experimental_request_user_input_enabled = false;
        });
    })
    .await;
    disabled.assert_visible_lacks(&["request_user_input"]);
    disabled.assert_registered_lacks(&["request_user_input"]);
}

#[tokio::test]
async fn update_plan_tool_respects_config_gate() {
    let enabled = probe(|_| {}).await;
    enabled.assert_visible_contains(&["update_plan"]);
    enabled.assert_registered_contains(&["update_plan"]);

    let disabled = probe(|turn| {
        update_config(turn, |config| {
            config.update_plan_enabled = false;
        });
    })
    .await;
    disabled.assert_visible_lacks(&["update_plan"]);
    disabled.assert_registered_lacks(&["update_plan"]);
}

#[tokio::test]
async fn request_user_input_stays_direct_in_code_mode_only() {
    let plan = probe(|turn| {
        set_features(turn, &[Feature::CodeMode, Feature::CodeModeOnly]);
    })
    .await;

    plan.assert_visible_contains(&[
        "request_user_input",
        codex_code_mode::PUBLIC_TOOL_NAME,
        codex_code_mode::WAIT_TOOL_NAME,
    ]);
    plan.assert_registered_contains(&["request_user_input"]);
    assert_eq!(
        plan.exposure("request_user_input"),
        ToolExposure::DirectModelOnly
    );

    let ToolSpec::Freeform(exec) = plan.visible_spec(codex_code_mode::PUBLIC_TOOL_NAME) else {
        panic!("expected code mode exec tool");
    };
    assert!(!exec.description.contains("request_user_input"));
}

#[tokio::test]
async fn shell_family_registers_visible_unified_exec_and_hidden_legacy_shell() {
    let plan = probe(|turn| {
        set_features(turn, &[Feature::ShellTool, Feature::UnifiedExec]);
        set_feature(turn, Feature::ShellZshFork, /*enabled*/ false);
        turn.model_info.shell_type = ConfigShellToolType::ShellCommand;
    })
    .await;

    plan.assert_visible_contains(&["exec_command", "write_stdin"]);
    plan.assert_visible_lacks(&["shell_command"]);
    plan.assert_registered_contains(&["exec_command", "write_stdin", "shell_command"]);
    assert_eq!(plan.exposure("shell_command"), ToolExposure::Hidden);
    assert!(has_parameter(plan.visible_spec("exec_command"), "shell"));
}

#[tokio::test]
async fn shell_zsh_fork_stays_standalone_until_unified_exec_composition_is_enabled() {
    let standalone = probe(|turn| {
        set_features(turn, &[Feature::ShellTool, Feature::UnifiedExec]);
        set_feature(turn, Feature::ShellZshFork, /*enabled*/ true);
        set_feature(turn, Feature::UnifiedExecZshFork, /*enabled*/ false);
        turn.model_info.shell_type = ConfigShellToolType::ShellCommand;
    })
    .await;

    standalone.assert_visible_contains(&["shell_command"]);
    standalone.assert_visible_lacks(&["exec_command", "write_stdin"]);
    standalone.assert_registered_contains(&["shell_command"]);
    standalone.assert_registered_lacks(&["exec_command", "write_stdin"]);

    let composed = probe(|turn| {
        set_features(
            turn,
            &[
                Feature::ShellTool,
                Feature::UnifiedExec,
                Feature::ShellZshFork,
                Feature::UnifiedExecZshFork,
            ],
        );
        turn.model_info.shell_type = ConfigShellToolType::ShellCommand;
    })
    .await;

    if codex_utils_pty::conpty_supported() {
        composed.assert_visible_contains(&["exec_command", "write_stdin"]);
        composed.assert_visible_lacks(&["shell_command"]);
        composed.assert_registered_contains(&["exec_command", "write_stdin", "shell_command"]);
        assert_eq!(composed.exposure("shell_command"), ToolExposure::Hidden);
    } else {
        composed.assert_visible_contains(&["shell_command"]);
        composed.assert_visible_lacks(&["exec_command", "write_stdin"]);
    }
}

#[tokio::test]
async fn zsh_fork_unified_exec_hides_shell_parameter() {
    if !codex_utils_pty::conpty_supported() {
        return;
    }

    let plan = probe(|turn| {
        set_features(
            turn,
            &[
                Feature::ShellTool,
                Feature::UnifiedExec,
                Feature::ShellZshFork,
                Feature::UnifiedExecZshFork,
            ],
        );
        turn.unified_exec_shell_mode =
            codex_tools::UnifiedExecShellMode::ZshFork(zsh_fork_config_for_spec_plan_tests());
    })
    .await;

    plan.assert_visible_contains(&["exec_command", "write_stdin"]);
    assert!(!has_parameter(plan.visible_spec("exec_command"), "shell"));
}

#[tokio::test]
async fn zsh_fork_unified_exec_keeps_shell_parameter_when_remote_environment_available() {
    if !codex_utils_pty::conpty_supported() {
        return;
    }

    let plan = probe(|turn| {
        set_features(
            turn,
            &[
                Feature::ShellTool,
                Feature::UnifiedExec,
                Feature::ShellZshFork,
                Feature::UnifiedExecZshFork,
            ],
        );
        turn.unified_exec_shell_mode =
            codex_tools::UnifiedExecShellMode::ZshFork(zsh_fork_config_for_spec_plan_tests());
        let remote_cwd = turn
            .environments
            .primary()
            .expect("primary environment")
            .cwd()
            .clone();
        turn.environments
            .environments
            .push(TurnEnvironmentState::Ready(
                crate::session::turn_context::TurnEnvironment::new(
                    "remote".to_string(),
                    Arc::new(
                        codex_exec_server::Environment::create_for_tests(Some(
                            "ws://127.0.0.1:1/remote-exec-server".to_string(),
                        ))
                        .expect("remote test environment"),
                    ),
                    remote_cwd,
                    Vec::new(),
                    /*shell*/ None,
                ),
            ));
    })
    .await;

    plan.assert_visible_contains(&["exec_command", "write_stdin"]);
    assert!(has_parameter(plan.visible_spec("exec_command"), "shell"));
    assert!(has_parameter(
        plan.visible_spec("exec_command"),
        "environment_id"
    ));
}

#[tokio::test]
async fn environment_count_controls_environment_backed_tools() {
    let no_environment = probe(|turn| {
        turn.environments.environments.clear();
        set_feature(turn, Feature::ShellTool, /*enabled*/ true);
        set_feature(turn, Feature::RequestPermissionsTool, /*enabled*/ true);
        turn.model_info.apply_patch_tool_type = Some(ApplyPatchToolType::Freeform);
    })
    .await;
    no_environment.assert_visible_lacks(&[
        "shell_command",
        "exec_command",
        "apply_patch",
        "view_image",
        "request_permissions",
    ]);
    no_environment.assert_registered_lacks(&[
        "shell_command",
        "exec_command",
        "apply_patch",
        "view_image",
        "request_permissions",
    ]);

    let multiple_environments = probe(|turn| {
        duplicate_primary_environment(turn);
        turn.permission_profile = PermissionProfile::workspace_write();
        set_feature(turn, Feature::ShellTool, /*enabled*/ true);
        set_feature(turn, Feature::UnifiedExec, /*enabled*/ true);
        set_feature(turn, Feature::RequestPermissionsTool, /*enabled*/ true);
        turn.model_info.apply_patch_tool_type = Some(ApplyPatchToolType::Freeform);
    })
    .await;
    multiple_environments.assert_visible_contains(&[
        "exec_command",
        "apply_patch",
        "view_image",
        "request_permissions",
    ]);
    assert!(has_parameter(
        multiple_environments.visible_spec("exec_command"),
        "environment_id"
    ));
    assert!(apply_patch_accepts_environment_id(
        multiple_environments.visible_spec("apply_patch")
    ));
    assert!(has_parameter(
        multiple_environments.visible_spec("view_image"),
        "environment_id"
    ));
}

#[tokio::test]
async fn environment_tools_follow_the_step_context() {
    let (_session, mut turn) = make_session_and_context().await;
    set_feature(&mut turn, Feature::UnifiedExec, /*enabled*/ true);
    turn.model_info.apply_patch_tool_type = Some(ApplyPatchToolType::Freeform);
    turn.permission_profile = PermissionProfile::workspace_write();

    let environments = turn.environments.clone();
    turn.environments.environments.clear();
    let turn = Arc::new(turn);
    let mcp = Arc::new(codex_mcp::McpBinding::empty(mcp_config_for_test(
        &turn.config,
    )));

    let plan = ToolPlanProbe::from_router(ToolRouter::from_tools(
        turn.as_ref(),
        build_core_tool_runtimes(
            turn.as_ref(),
            &environments,
            mcp.as_ref(),
            /*tool_suggest_candidates*/ None,
            /*wait_for_environment_tool_config*/ None,
        ),
        super::hosted_model_tool_specs(turn.as_ref(), &[]),
        &Default::default(),
    ));

    plan.assert_visible_contains(&["exec_command", "apply_patch", "view_image"]);
}

#[tokio::test]
async fn sleep_tool_follows_current_time_config() {
    let disabled = probe(|turn| {
        set_feature(turn, Feature::CurrentTimeReminder, /*enabled*/ true);
    })
    .await;
    assert_eq!(disabled.namespace_function_names("clock"), ["curr_time"]);

    let enabled = probe(|turn| {
        set_feature(turn, Feature::CurrentTimeReminder, /*enabled*/ true);
        let mut config = (*turn.config).clone();
        config.current_time_reminder = Some(CurrentTimeReminderConfig {
            sleep_tool: true,
            ..CurrentTimeReminderConfig::default()
        });
        turn.config = Arc::new(config);
    })
    .await;
    assert_eq!(
        enabled.namespace_function_names("clock"),
        ["curr_time", "sleep"]
    );
}

#[tokio::test]
async fn released_sleep_tool_flag_still_exposes_sleep_without_time_reminders() {
    let plan = probe(|turn| {
        set_feature(turn, Feature::SleepTool, /*enabled*/ true);
    })
    .await;

    assert_eq!(plan.namespace_function_names("clock"), ["sleep"]);
}

#[tokio::test]
async fn sleep_tool_stays_direct_and_outside_code_mode() {
    for code_mode_only in [false, true] {
        let plan = probe(|turn| {
            set_features(
                turn,
                &[
                    Feature::CodeMode,
                    Feature::CurrentTimeReminder,
                    Feature::MultiAgentV2,
                ],
            );
            if code_mode_only {
                set_feature(turn, Feature::CodeModeOnly, /*enabled*/ true);
            }
            update_config(turn, |config| {
                config.current_time_reminder = Some(CurrentTimeReminderConfig {
                    sleep_tool: true,
                    ..CurrentTimeReminderConfig::default()
                });
                config.multi_agent_v2.wait_agent_enabled = false;
            });
        })
        .await;

        assert!(
            plan.namespace_function_names("clock")
                .iter()
                .any(|name| name == "sleep")
        );
        let sleep_tool_name = ToolName::namespaced("clock", "sleep").to_string();
        let wait_agent_tool_name =
            ToolName::namespaced(MULTI_AGENT_V2_NAMESPACE, "wait_agent").to_string();
        assert_eq!(
            plan.exposure(&sleep_tool_name),
            ToolExposure::DirectModelOnly
        );
        plan.assert_registered_lacks(&[wait_agent_tool_name.as_str()]);

        let ToolSpec::Freeform(exec) = plan.visible_spec(codex_code_mode::PUBLIC_TOOL_NAME) else {
            panic!("expected code mode exec tool");
        };
        if code_mode_only {
            assert!(exec.description.contains("clock__curr_time"));
        }
        assert!(!exec.description.contains("clock__sleep"));
    }
}

#[tokio::test]
async fn mcp_and_tool_search_follow_direct_and_deferred_tool_exposure() {
    let direct_mcp = probe_with(
        |_| {},
        ToolPlanInputs {
            tool_runtimes: vec![mcp_runtime(
                "direct",
                "mcp__direct",
                "lookup",
                ToolExposure::Direct,
            )],
            ..ToolPlanInputs::default()
        },
    )
    .await;
    assert_eq!(
        direct_mcp.namespace_function_names("mcp__direct"),
        &["lookup".to_string()]
    );

    let searchable_mcp = ToolPlanInputs {
        tool_runtimes: vec![mcp_runtime(
            "searchable",
            "mcp__searchable",
            "lookup",
            ToolExposure::Deferred,
        )],
        ..ToolPlanInputs::default()
    };

    let missing_model_capability = probe_with(
        |turn| {
            turn.model_info.supports_search_tool = false;
        },
        ToolPlanInputs {
            tool_runtimes: searchable_mcp.tool_runtimes.clone(),
            ..ToolPlanInputs::default()
        },
    )
    .await;
    missing_model_capability.assert_visible_lacks(&["tool_search"]);

    let missing_deferred_tools = probe(|turn| {
        set_feature(turn, Feature::Collab, /*enabled*/ false);
        turn.model_info.supports_search_tool = true;
    })
    .await;
    missing_deferred_tools.assert_visible_lacks(&["tool_search"]);
    missing_deferred_tools.assert_visible_lacks(&[
        "list_mcp_resources",
        "list_mcp_resource_templates",
        "read_mcp_resource",
    ]);

    let bedrock_namespace_capability = probe_with(
        |turn| {
            turn.model_info.supports_search_tool = true;
            use_bedrock_provider(turn);
        },
        ToolPlanInputs {
            tool_runtimes: searchable_mcp.tool_runtimes.clone(),
            ..ToolPlanInputs::default()
        },
    )
    .await;
    bedrock_namespace_capability.assert_visible_contains(&["tool_search"]);

    let enabled = probe_with(
        |turn| {
            turn.model_info.supports_search_tool = true;
        },
        searchable_mcp,
    )
    .await;
    enabled.assert_visible_contains(&["tool_search"]);
    enabled.assert_registered_contains(&[
        "tool_search",
        &ToolName::namespaced("mcp__searchable", "lookup").to_string(),
    ]);
}

#[tokio::test]
async fn unified_tool_runtimes_preserve_source_order_and_collision_priority() {
    let plan = probe_with(
        |turn| {
            set_features(turn, &[Feature::ShellTool, Feature::UnifiedExec]);
            set_feature(turn, Feature::ShellZshFork, /*enabled*/ false);
            turn.model_info.shell_type = ConfigShellToolType::ShellCommand;
        },
        ToolPlanInputs {
            tool_runtimes: vec![mcp_runtime(
                "registry",
                "mcp__registry",
                "lookup",
                ToolExposure::Direct,
            )],
            extension_tool_executors: vec![
                Arc::new(TestNamespaceExtensionTool {
                    namespace: "mcp__registry",
                    tool_name: "lookup",
                }),
                Arc::new(TestNamespaceExtensionTool {
                    namespace: "registry_extension",
                    tool_name: "lookup",
                }),
            ],
            dynamic_tools: vec![dynamic_tool(
                Some("registry_dynamic"),
                "lookup",
                /*defer_loading*/ false,
            )],
            ..ToolPlanInputs::default()
        },
    )
    .await;

    let expected_source_order = [
        "exec_command",
        "mcp__registry",
        "registry_extension",
        "registry_dynamic",
    ];
    let source_order = plan
        .visible_names
        .iter()
        .map(String::as_str)
        .filter(|name| expected_source_order.contains(name))
        .collect::<Vec<_>>();
    assert_eq!(source_order, expected_source_order);

    let mcp_tool_name = ToolName::namespaced("mcp__registry", "lookup").to_string();
    let extension_tool_name = ToolName::namespaced("registry_extension", "lookup").to_string();
    let dynamic_tool_name = ToolName::namespaced("registry_dynamic", "lookup").to_string();
    plan.assert_registered_contains(&[
        "exec_command",
        &mcp_tool_name,
        &extension_tool_name,
        &dynamic_tool_name,
    ]);
    assert_eq!(
        plan.namespace_function_names("mcp__registry"),
        &["lookup".to_string()]
    );

    let ToolSpec::Namespace(namespace) = plan.visible_spec("mcp__registry") else {
        panic!("expected the MCP namespace to stay visible");
    };
    let [ResponsesApiNamespaceTool::Function(tool)] = namespace.tools.as_slice() else {
        panic!("expected exactly one MCP tool after the extension collision");
    };
    assert_eq!(tool.description, "lookup test tool");
}

#[tokio::test]
async fn code_mode_uses_the_first_normalized_tool_identity() {
    for (code_mode_only, winner_exposure, shadow_is_deferred) in [
        (false, ToolExposure::Direct, true),
        (false, ToolExposure::Deferred, false),
        (true, ToolExposure::Direct, true),
        (true, ToolExposure::Deferred, false),
    ] {
        let mut metadata_state = None;
        let plan = probe_with(
            |turn| {
                set_feature(turn, Feature::CodeMode, /*enabled*/ true);
                if code_mode_only {
                    set_feature(turn, Feature::CodeModeOnly, /*enabled*/ true);
                }
                turn.model_info.supports_search_tool = true;
                turn.model_info.use_responses_lite = true;
                metadata_state = Some(Arc::clone(&turn.turn_metadata_state));
            },
            ToolPlanInputs {
                tool_runtimes: vec![mcp_runtime(
                    "winner",
                    "normalized-alias",
                    "lookup",
                    winner_exposure,
                )],
                dynamic_tools: vec![dynamic_tool(
                    Some("normalized_alias"),
                    "lookup",
                    shadow_is_deferred,
                )],
                ..ToolPlanInputs::default()
            },
        )
        .await;

        let winner_name = ToolName::namespaced("normalized-alias", "lookup");
        let shadow_name = ToolName::namespaced("normalized_alias", "lookup");
        plan.assert_registered_contains(&[&winner_name.to_string(), &shadow_name.to_string()]);
        assert_eq!(plan.exposure(&winner_name.to_string()), winner_exposure);
        assert_eq!(
            plan.exposure(&shadow_name.to_string()),
            if shadow_is_deferred {
                ToolExposure::Deferred
            } else {
                ToolExposure::Direct
            },
        );

        let metadata = metadata_state
            .expect("tool planning should capture the turn metadata")
            .to_responses_metadata(
                "installation".to_string(),
                "window".to_string(),
                CodexResponsesRequestKind::Turn,
            );
        let code_mode_tool_names = metadata
            .code_mode_tool_names
            .as_ref()
            .expect("Responses Lite should receive the supported code-mode tools");
        assert_eq!(
            code_mode_tool_names.get("normalized_alias__lookup"),
            Some(&winner_name),
        );
        assert!(!code_mode_tool_names.contains_key(codex_tools::TOOL_SEARCH_TOOL_NAME));

        if !code_mode_only && !shadow_is_deferred {
            let ToolSpec::Namespace(namespace) = plan.visible_spec("normalized_alias") else {
                panic!("expected the shadowed dynamic tool to remain directly visible");
            };
            let [ResponsesApiNamespaceTool::Function(shadow)] = namespace.tools.as_slice() else {
                panic!("expected exactly one shadowed dynamic tool");
            };
            assert_eq!(shadow.description, "lookup dynamic tool");
        }

        let ToolSpec::Freeform(exec) = plan.visible_spec(codex_code_mode::PUBLIC_TOOL_NAME) else {
            panic!("expected code mode exec tool");
        };
        assert!(!exec.description.contains("lookup dynamic tool"));
        assert_eq!(
            exec.description.contains("lookup test tool"),
            code_mode_only && winner_exposure == ToolExposure::Direct,
        );
    }
}

#[tokio::test]
async fn deferred_extension_tools_are_discoverable_with_tool_search() {
    let plan = probe_with(
        |turn| {
            turn.model_info.supports_search_tool = true;
        },
        ToolPlanInputs {
            extension_tool_executors: vec![Arc::new(DeferredExtensionTool)],
            ..ToolPlanInputs::default()
        },
    )
    .await;

    plan.assert_visible_contains(&["tool_search"]);
    plan.assert_visible_lacks(&["extension_echo"]);
    plan.assert_registered_contains(&["extension_echo"]);
    assert_eq!(plan.exposure("extension_echo"), ToolExposure::Deferred);
}

#[tokio::test]
async fn tool_search_cache_rebuilds_when_deferred_sources_change() {
    let cache = ToolSearchHandlerCache::default();

    let (_session, mut first_turn) = make_session_and_context().await;
    use_openai_provider(&mut first_turn);
    first_turn.model_info.supports_search_tool = true;
    let first_turn = Arc::new(first_turn);
    let first_step_context = StepContext::for_test(Arc::clone(&first_turn));
    let mut first_tool_runtimes = build_core_tool_runtimes(
        first_step_context.turn.as_ref(),
        &first_step_context.environments,
        first_step_context.mcp.as_ref(),
        /*tool_suggest_candidates*/ None,
        /*wait_for_environment_tool_config*/ None,
    );
    first_tool_runtimes.push(mcp_runtime(
        "first",
        "mcp__first",
        "lookup",
        ToolExposure::Deferred,
    ));
    let first_router = ToolRouter::from_tools(
        first_step_context.turn.as_ref(),
        first_tool_runtimes,
        super::hosted_model_tool_specs(first_step_context.turn.as_ref(), &[]),
        &cache,
    );
    let first_plan = ToolPlanProbe::from_router(first_router);

    let (_session, mut second_turn) = make_session_and_context().await;
    use_openai_provider(&mut second_turn);
    second_turn.model_info.supports_search_tool = true;
    let second_turn = Arc::new(second_turn);
    let second_step_context = StepContext::for_test(Arc::clone(&second_turn));
    let mut second_tool_runtimes = build_core_tool_runtimes(
        second_step_context.turn.as_ref(),
        &second_step_context.environments,
        second_step_context.mcp.as_ref(),
        /*tool_suggest_candidates*/ None,
        /*wait_for_environment_tool_config*/ None,
    );
    second_tool_runtimes.push(mcp_runtime(
        "second",
        "mcp__second",
        "lookup",
        ToolExposure::Deferred,
    ));
    let second_router = ToolRouter::from_tools(
        second_step_context.turn.as_ref(),
        second_tool_runtimes,
        super::hosted_model_tool_specs(second_step_context.turn.as_ref(), &[]),
        &cache,
    );
    let second_plan = ToolPlanProbe::from_router(second_router);

    let ToolSpec::ToolSearch {
        description: first_description,
        ..
    } = first_plan.visible_spec("tool_search")
    else {
        panic!("expected first tool_search spec");
    };
    assert!(first_description.contains("- first: Tools from first."));
    assert!(!first_description.contains("- second: Tools from second."));

    let ToolSpec::ToolSearch {
        description: second_description,
        ..
    } = second_plan.visible_spec("tool_search")
    else {
        panic!("expected second tool_search spec");
    };
    assert!(second_description.contains("- second: Tools from second."));
    assert!(!second_description.contains("- first: Tools from first."));
}

#[tokio::test]
async fn tool_search_cache_rebuilds_when_deferred_world_state_changes() {
    let cache = ToolSearchHandlerCache::default();

    for world_state_enabled in [false, true, false] {
        let (_session, mut turn) = make_session_and_context().await;
        turn.model_info.supports_search_tool = true;
        set_feature(
            &mut turn,
            Feature::DeferredToolWorldState,
            world_state_enabled,
        );
        let turn = Arc::new(turn);
        let step_context = StepContext::for_test(Arc::clone(&turn));
        let mut tool_runtimes = build_core_tool_runtimes(
            step_context.turn.as_ref(),
            &step_context.environments,
            step_context.mcp.as_ref(),
            /*tool_suggest_candidates*/ None,
            /*wait_for_environment_tool_config*/ None,
        );
        tool_runtimes.push(mcp_runtime(
            "calendar",
            "mcp__calendar",
            "lookup",
            ToolExposure::Deferred,
        ));
        let router = ToolRouter::from_tools(
            step_context.turn.as_ref(),
            tool_runtimes,
            super::hosted_model_tool_specs(step_context.turn.as_ref(), &[]),
            &cache,
        );
        let plan = ToolPlanProbe::from_router(router);
        let ToolSpec::ToolSearch { description, .. } = plan.visible_spec("tool_search") else {
            panic!("expected visible tool_search spec");
        };

        assert_eq!(
            description.contains("- calendar: Tools from calendar."),
            !world_state_enabled,
            "tool search cache should follow the deferred world-state feature"
        );
    }
}

#[tokio::test]
async fn request_plugin_install_requires_all_discovery_features() {
    for disabled_feature in [Feature::ToolSuggest, Feature::Apps, Feature::Plugins] {
        let plan = probe_with(
            |turn| {
                set_features(
                    turn,
                    &[Feature::ToolSuggest, Feature::Apps, Feature::Plugins],
                );
                set_feature(turn, disabled_feature, /*enabled*/ false);
            },
            ToolPlanInputs {
                tool_suggest_candidates: Some(plugin_candidates(ToolSuggestPresentation::ListTool)),
                ..ToolPlanInputs::default()
            },
        )
        .await;
        plan.assert_visible_lacks(&[
            "list_available_plugins_to_install",
            "request_plugin_install",
        ]);
    }

    for tool_suggest_candidates in [
        None,
        Some(ToolSuggestCandidates {
            tools: Vec::new(),
            presentation: ToolSuggestPresentation::RecommendationContext,
        }),
    ] {
        let plan = probe_with(
            |turn| {
                set_features(
                    turn,
                    &[Feature::ToolSuggest, Feature::Apps, Feature::Plugins],
                );
            },
            ToolPlanInputs {
                tool_suggest_candidates,
                ..ToolPlanInputs::default()
            },
        )
        .await;
        plan.assert_visible_lacks(&[
            "list_available_plugins_to_install",
            "request_plugin_install",
        ]);
    }

    let enabled = probe_with(
        |turn| {
            set_features(
                turn,
                &[Feature::ToolSuggest, Feature::Apps, Feature::Plugins],
            );
        },
        ToolPlanInputs {
            tool_suggest_candidates: Some(plugin_candidates(ToolSuggestPresentation::ListTool)),
            ..ToolPlanInputs::default()
        },
    )
    .await;
    enabled.assert_visible_contains(&[
        "list_available_plugins_to_install",
        "request_plugin_install",
    ]);
}

#[tokio::test]
async fn request_plugin_install_stays_visible_without_tool_search() {
    let plan = probe_with(
        |turn| {
            turn.model_info.supports_search_tool = false;
            set_features(
                turn,
                &[Feature::ToolSuggest, Feature::Apps, Feature::Plugins],
            );
        },
        ToolPlanInputs {
            tool_suggest_candidates: Some(plugin_candidates(ToolSuggestPresentation::ListTool)),
            ..ToolPlanInputs::default()
        },
    )
    .await;

    plan.assert_visible_contains(&[
        "list_available_plugins_to_install",
        "request_plugin_install",
    ]);
    plan.assert_visible_lacks(&["tool_search"]);
}

#[tokio::test]
async fn request_plugin_install_description_requires_exhausting_tool_search() {
    let plan = probe_with(
        |turn| {
            set_features(
                turn,
                &[Feature::ToolSuggest, Feature::Apps, Feature::Plugins],
            );
        },
        ToolPlanInputs {
            tool_suggest_candidates: Some(plugin_candidates(
                ToolSuggestPresentation::RecommendationContext,
            )),
            ..ToolPlanInputs::default()
        },
    )
    .await;

    let request_spec = plan.visible_spec("request_plugin_install");
    let ToolSpec::Function(ResponsesApiTool {
        description: request_description,
        ..
    }) = request_spec
    else {
        panic!("expected request_plugin_install function spec");
    };
    assert!(request_description.contains("listed in `<recommended_plugins>`"));
    assert!(request_description.contains("explicitly asks to use a specific plugin"));
    assert!(request_description.contains("Tool search has already been exhausted"));
    assert!(!request_description.contains("`tool_search`"));
    assert!(request_description.contains("DO NOT call this tool in parallel with other tools"));
    assert!(!request_description.contains("list_available_plugins_to_install"));
    assert!(!request_description.contains("github"));
    assert!(has_parameter(request_spec, "plugin_id"));
    assert!(has_parameter(request_spec, "suggest_reason"));
    assert!(!has_parameter(request_spec, "tool_id"));
    assert!(!has_parameter(request_spec, "tool_type"));
    assert!(!has_parameter(request_spec, "action_type"));
    plan.assert_visible_lacks(&["list_available_plugins_to_install"]);
    plan.assert_registered_lacks(&["list_available_plugins_to_install"]);
}

#[tokio::test]
async fn code_mode_only_exposes_code_executor_and_hides_nested_tools() {
    let input = ToolPlanInputs {
        dynamic_tools: vec![dynamic_tool(
            Some("codex_app"),
            "lookup",
            /*defer_loading*/ false,
        )],
        ..ToolPlanInputs::default()
    };
    let plain = probe_with(|_| {}, input).await;
    assert_eq!(
        plain.namespace_function_names("codex_app"),
        &["lookup".to_string()]
    );
    plain.assert_visible_lacks(&[
        codex_code_mode::PUBLIC_TOOL_NAME,
        codex_code_mode::WAIT_TOOL_NAME,
    ]);

    let code_mode_only = probe_with(
        |turn| {
            set_features(turn, &[Feature::CodeMode, Feature::CodeModeOnly]);
        },
        ToolPlanInputs {
            dynamic_tools: vec![dynamic_tool(
                Some("codex_app"),
                "lookup",
                /*defer_loading*/ false,
            )],
            ..ToolPlanInputs::default()
        },
    )
    .await;
    code_mode_only.assert_visible_contains(&[
        codex_code_mode::PUBLIC_TOOL_NAME,
        codex_code_mode::WAIT_TOOL_NAME,
    ]);
    assert_eq!(
        code_mode_only.namespace_function_names("codex_app"),
        Vec::<String>::new().as_slice()
    );
}

#[tokio::test]
async fn code_mode_buffered_exec_updates_exec_description() {
    let plan = probe(|turn| {
        set_features(turn, &[Feature::CodeMode, Feature::CodeModeBufferedExec]);
    })
    .await;

    let ToolSpec::Freeform(exec) = plan.visible_spec(codex_code_mode::PUBLIC_TOOL_NAME) else {
        panic!("expected code mode exec tool");
    };
    assert!(exec.description.contains("Defaults to 30000 ms."));
    assert!(!exec.description.contains("Defaults to 10000 ms."));
}

#[tokio::test]
async fn code_mode_only_exposes_configured_dynamic_namespace_directly() {
    let plan = probe_with(
        |turn| {
            set_features(turn, &[Feature::CodeMode, Feature::CodeModeOnly]);
            turn.model_info.supports_search_tool = true;
            update_config(turn, |config| {
                config.code_mode.direct_only_tool_namespaces = vec!["direct_only".to_string()];
            });
        },
        ToolPlanInputs {
            dynamic_tools: vec![dynamic_tool(
                Some("direct_only"),
                "lookup",
                /*defer_loading*/ true,
            )],
            ..ToolPlanInputs::default()
        },
    )
    .await;

    plan.assert_visible_contains(&[
        codex_code_mode::PUBLIC_TOOL_NAME,
        codex_code_mode::WAIT_TOOL_NAME,
        "direct_only",
    ]);
    plan.assert_visible_lacks(&["tool_search"]);
    assert_eq!(
        plan.exposure(&ToolName::namespaced("direct_only", "lookup").to_string()),
        ToolExposure::DirectModelOnly
    );
    let ToolSpec::Namespace(namespace) = plan.visible_spec("direct_only") else {
        panic!("expected direct-only namespace spec");
    };
    let ResponsesApiNamespaceTool::Function(tool) = &namespace.tools[0];
    assert_eq!(tool.defer_loading, None);
    let ToolSpec::Freeform(exec) = plan.visible_spec(codex_code_mode::PUBLIC_TOOL_NAME) else {
        panic!("expected code mode exec tool");
    };
    assert!(!exec.description.contains("direct_only_lookup(args:"));
}

#[tokio::test]
async fn excluded_deferred_namespaces_do_not_enable_nested_tool_guidance() {
    let plan = probe_with(
        |turn| {
            set_features(turn, &[Feature::CodeMode, Feature::CodeModeOnly]);
            set_feature(turn, Feature::Collab, /*enabled*/ false);
            turn.model_info.supports_search_tool = true;
            update_config(turn, |config| {
                config.code_mode.excluded_tool_namespaces = vec!["excluded".to_string()];
            });
        },
        ToolPlanInputs {
            dynamic_tools: vec![dynamic_tool(
                Some("excluded"),
                "lookup",
                /*defer_loading*/ true,
            )],
            ..ToolPlanInputs::default()
        },
    )
    .await;

    let ToolSpec::Freeform(exec) = plan.visible_spec(codex_code_mode::PUBLIC_TOOL_NAME) else {
        panic!("expected code mode exec tool");
    };
    assert!(
        !exec
            .description
            .contains("Some deferred nested tools may be omitted")
    );
    plan.assert_registered_contains(&[
        &ToolName::namespaced("excluded", "lookup").to_string(),
        "tool_search",
    ]);
}

#[tokio::test]
async fn multi_agent_feature_selects_one_agent_tool_family() {
    let v1 = probe(|turn| {
        use_openai_provider(turn);
        set_feature(turn, Feature::Collab, /*enabled*/ true);
        set_feature(turn, Feature::MultiAgentV2, /*enabled*/ false);
    })
    .await;
    v1.assert_visible_contains(&[MULTI_AGENT_V1_NAMESPACE]);
    v1.assert_visible_lacks(&[
        "spawn_agent",
        "send_input",
        "resume_agent",
        "wait_agent",
        "close_agent",
        "interrupt_agent",
        "send_message",
        "followup_task",
        "assign_task",
        "list_agents",
    ]);
    assert_eq!(
        v1.namespace_function_names(MULTI_AGENT_V1_NAMESPACE),
        &[
            "close_agent".to_string(),
            "resume_agent".to_string(),
            "send_input".to_string(),
            "spawn_agent".to_string(),
            "wait_agent".to_string(),
        ]
    );
    let ToolSpec::Namespace(namespace) = v1.visible_spec(MULTI_AGENT_V1_NAMESPACE) else {
        panic!("expected v1 multi-agent namespace");
    };
    let Some(ResponsesApiNamespaceTool::Function(spawn_agent)) =
        namespace.tools.iter().find(|tool| {
            matches!(
                tool,
                ResponsesApiNamespaceTool::Function(tool) if tool.name == "spawn_agent"
            )
        })
    else {
        panic!("expected v1 spawn_agent function");
    };
    let properties = spawn_agent
        .parameters
        .properties
        .as_ref()
        .expect("spawn_agent should use object params");
    for property in ["model", "reasoning_effort", "service_tier"] {
        assert!(
            properties.contains_key(property),
            "expected v1 spawn_agent to expose `{property}`"
        );
    }
    assert!(
        properties.contains_key("agent_type"),
        "PF Terminal's built-in hierarchy roles must remain selectable in the native v1 surface"
    );

    let v2 = probe(|turn| {
        set_feature(turn, Feature::MultiAgentV2, /*enabled*/ true);
        update_config(turn, |config| {
            config.multi_agent_v2.max_concurrent_threads_per_session = 17;
            config.multi_agent_v2.expose_spawn_agent_model_overrides = true;
        });
    })
    .await;
    v2.assert_visible_contains(&[MULTI_AGENT_V2_NAMESPACE]);
    v2.assert_visible_lacks(&[
        "spawn_agent",
        "send_message",
        "followup_task",
        "wait_agent",
        "interrupt_agent",
        "list_agents",
        "send_input",
        "resume_agent",
        "assign_task",
        "close_agent",
    ]);
    for tool_name in [
        "spawn_agent",
        "send_message",
        "followup_task",
        "wait_agent",
        "interrupt_agent",
        "list_agents",
    ] {
        assert!(
            v2.namespace_function_names(MULTI_AGENT_V2_NAMESPACE)
                .iter()
                .any(|name| name == tool_name),
            "expected {tool_name} in {MULTI_AGENT_V2_NAMESPACE} namespace"
        );
    }
    let ToolSpec::Namespace(namespace) = v2.visible_spec(MULTI_AGENT_V2_NAMESPACE) else {
        panic!("expected {MULTI_AGENT_V2_NAMESPACE} namespace");
    };
    let Some(ResponsesApiNamespaceTool::Function(spawn_agent)) =
        namespace.tools.iter().find(|tool| {
            matches!(
                tool,
                ResponsesApiNamespaceTool::Function(tool) if tool.name == "spawn_agent"
            )
        })
    else {
        panic!("expected spawn_agent in {MULTI_AGENT_V2_NAMESPACE} namespace");
    };
    let spawn_agent_properties = spawn_agent
        .parameters
        .properties
        .as_ref()
        .expect("spawn_agent should use object params");
    for property in [
        "agent_type",
        "model_provider",
        "model",
        "reasoning_effort",
        "service_tier",
    ] {
        assert!(
            !spawn_agent_properties.contains_key(property),
            "OpenAI's reserved collaboration schema must hide `{property}`"
        );
    }
    for property in ["message", "task_name", "fork_turns"] {
        assert!(spawn_agent_properties.contains_key(property));
    }
    let spawn_agent_description = spawn_agent.description.as_str();
    assert!(!spawn_agent_description.contains("max_concurrent_threads_per_session"));
    assert!(spawn_agent_description.contains(
        "Note that passing `fork_turns=\"none\"` will not pass any surrounding context to the spawned subagent"
    ));

    let direct_model_only = probe(|turn| {
        set_features(
            turn,
            &[
                Feature::CodeMode,
                Feature::CodeModeOnly,
                Feature::MultiAgentV2,
            ],
        );
        update_config(turn, |config| {
            config.multi_agent_v2.non_code_mode_only = true;
        });
    })
    .await;
    direct_model_only.assert_visible_contains(&[MULTI_AGENT_V2_NAMESPACE]);
    direct_model_only.assert_visible_lacks(&["spawn_agent", "send_message", "wait_agent"]);
    assert_eq!(
        direct_model_only
            .exposure(&ToolName::namespaced(MULTI_AGENT_V2_NAMESPACE, "spawn_agent").to_string()),
        ToolExposure::DirectModelOnly
    );
}

#[tokio::test]
async fn multi_agent_v2_advertises_cross_provider_catalog_from_third_party_parent() {
    let v2 = probe(|turn| {
        use_openrouter_provider(turn);
        turn.available_models = crate::test_support::all_model_presets().clone();
        set_feature(turn, Feature::MultiAgentV2, /*enabled*/ true);
        update_config(turn, |config| {
            config.agent_provider_allowlist = Some(vec![
                OPENROUTER_PROVIDER_ID.to_string(),
                OPENAI_PROVIDER_ID.to_string(),
            ]);
        });
    })
    .await;
    let v2_description = match v2.visible_spec("spawn_agent") {
        ToolSpec::Function(tool) => tool.description.as_str(),
        other => panic!("expected v2 spawn_agent function spec, got {other:?}"),
    };
    assert!(
        v2_description.contains("`openai` / `gpt-5.6-sol`"),
        "v2 managers on third-party providers need the authorized cross-provider catalog"
    );
    assert!(
        v2_description.contains(
            "`openai` / `gpt-6-astra`; explicit-choice only; allocation economics unavailable;"
        ),
        "explicit model discovery must not require automatic-allocation economics"
    );

    let v1 = probe(|turn| {
        use_openrouter_provider(turn);
        turn.available_models = crate::test_support::all_model_presets().clone();
        set_feature(turn, Feature::Collab, /*enabled*/ true);
        set_feature(turn, Feature::MultiAgentV2, /*enabled*/ false);
    })
    .await;
    let v1_description = match v1.visible_spec("spawn_agent_v1") {
        ToolSpec::Function(tool) => tool.description.as_str(),
        other => panic!("expected flattened v1 spawn_agent function spec, got {other:?}"),
    };
    assert!(
        !v1_description.contains("`openai` / `gpt-5.6-sol`"),
        "v1 must not advertise provider/model pairs its model-only schema cannot select"
    );
}

#[tokio::test]
async fn multi_agent_v2_flattens_collaboration_tools_for_anthropic_messages() {
    let plan = probe(|turn| {
        use_claude_plan_provider(turn);
        set_feature(turn, Feature::MultiAgentV2, /*enabled*/ true);
    })
    .await;

    let collaboration_tools = [
        "spawn_agent",
        "send_message",
        "followup_task",
        "wait_agent",
        "interrupt_agent",
        "list_agents",
    ];
    plan.assert_visible_contains(&collaboration_tools);
    plan.assert_registered_contains(&collaboration_tools);
    plan.assert_visible_lacks(&[MULTI_AGENT_V2_NAMESPACE]);
    for tool_name in collaboration_tools {
        assert_eq!(plan.exposure(tool_name), ToolExposure::DirectModelOnly);
        let ToolSpec::Function(tool) = plan.visible_spec(tool_name) else {
            panic!("expected `{tool_name}` to be an Anthropic-compatible function tool");
        };
        assert_eq!(tool.name, tool_name);
    }
}

#[tokio::test]
async fn multi_agent_v2_message_schemas_are_provider_neutral_plaintext() {
    let plan = probe(|turn| {
        use_bedrock_provider(turn);
        set_feature(turn, Feature::MultiAgentV2, /*enabled*/ true);
    })
    .await;
    let ToolSpec::Namespace(namespace) = plan.visible_spec(MULTI_AGENT_V2_NAMESPACE) else {
        panic!("expected {MULTI_AGENT_V2_NAMESPACE} namespace");
    };
    for tool_name in ["spawn_agent", "send_message", "followup_task"] {
        let Some(ResponsesApiNamespaceTool::Function(tool)) = namespace.tools.iter().find(|tool| {
            matches!(
                tool,
                ResponsesApiNamespaceTool::Function(tool) if tool.name == tool_name
            )
        }) else {
            panic!("expected {tool_name} in {MULTI_AGENT_V2_NAMESPACE} namespace");
        };
        let properties = tool
            .parameters
            .properties
            .as_ref()
            .expect("tool should use object params");
        assert_eq!(
            properties
                .get("message")
                .and_then(|schema| schema.encrypted),
            None
        );
    }
}

#[tokio::test]
async fn multi_agent_v2_openai_plan_uses_reserved_collaboration_schemas() {
    let plan = probe(|turn| {
        use_openai_provider(turn);
        set_feature(turn, Feature::MultiAgentV2, /*enabled*/ true);
    })
    .await;
    let ToolSpec::Namespace(namespace) = plan.visible_spec(MULTI_AGENT_V2_NAMESPACE) else {
        panic!("expected {MULTI_AGENT_V2_NAMESPACE} namespace");
    };
    let function = |name: &str| {
        namespace
            .tools
            .iter()
            .find_map(|tool| match tool {
                ResponsesApiNamespaceTool::Function(tool) if tool.name == name => Some(tool),
                _ => None,
            })
            .unwrap_or_else(|| panic!("expected {name} in {MULTI_AGENT_V2_NAMESPACE}"))
    };

    for tool_name in ["spawn_agent", "send_message", "followup_task"] {
        let properties = function(tool_name)
            .parameters
            .properties
            .as_ref()
            .expect("collaboration tool should use object params");
        assert_eq!(
            properties
                .get("message")
                .and_then(|schema| schema.encrypted),
            Some(true),
            "{tool_name} must retain OpenAI's reserved encrypted-message annotation"
        );
    }
    for adapter_name in [
        "spawn_agent_plaintext",
        "send_message_plaintext",
        "followup_task_plaintext",
    ] {
        let ToolSpec::Function(adapter) = plan.visible_spec(adapter_name) else {
            panic!("expected {adapter_name} as a provider adapter function");
        };
        let properties = adapter
            .parameters
            .properties
            .as_ref()
            .expect("adapter should use object params");
        assert_eq!(
            properties
                .get("message")
                .and_then(|schema| schema.encrypted),
            None,
            "{adapter_name} must provide plaintext cross-provider transport"
        );
        assert!(adapter.description.contains("Plaintext adapter"));
        assert!(
            adapter
                .description
                .contains("It supports OpenAI recipients too.")
        );
    }
    assert_eq!(
        function("spawn_agent")
            .parameters
            .properties
            .as_ref()
            .expect("spawn params")
            .keys()
            .cloned()
            .collect::<Vec<_>>(),
        vec![
            "fork_turns".to_string(),
            "message".to_string(),
            "task_name".to_string()
        ]
    );
    assert_eq!(
        function("spawn_agent")
            .output_schema
            .as_ref()
            .expect("spawn output schema")["required"],
        serde_json::json!(["task_name"])
    );
    assert_eq!(
        function("interrupt_agent")
            .parameters
            .properties
            .as_ref()
            .expect("interrupt params")
            .keys()
            .cloned()
            .collect::<Vec<_>>(),
        vec!["target".to_string()]
    );
    assert_eq!(
        function("wait_agent")
            .output_schema
            .as_ref()
            .expect("wait output schema")["required"],
        serde_json::json!(["message", "timed_out"])
    );
    assert_eq!(
        function("list_agents")
            .output_schema
            .as_ref()
            .expect("list output schema")["properties"]["agents"]["items"]["required"],
        serde_json::json!(["agent_name", "agent_status"])
    );
}

#[tokio::test]
async fn multi_agent_v2_can_disable_wait_agent() {
    let plan = probe(|turn| {
        set_feature(turn, Feature::MultiAgentV2, /*enabled*/ true);
        update_config(turn, |config| {
            config.multi_agent_v2.wait_agent_enabled = false;
        });
    })
    .await;

    assert_eq!(
        plan.namespace_function_names(MULTI_AGENT_V2_NAMESPACE),
        &[
            "followup_task".to_string(),
            "interrupt_agent".to_string(),
            "list_agents".to_string(),
            "send_message".to_string(),
            "spawn_agent".to_string(),
        ]
    );
    plan.assert_visible_lacks(&["clock"]);
    plan.assert_registered_lacks(&["collaboration.wait_agent", "clock.sleep"]);
}

#[tokio::test]
async fn tool_mode_selector_overrides_feature_flags() {
    let direct = probe(|turn| {
        set_features(turn, &[Feature::CodeMode, Feature::CodeModeOnly]);
        turn.model_info.tool_mode = Some(ToolMode::Direct);
    })
    .await;
    direct.assert_visible_lacks(&[
        codex_code_mode::PUBLIC_TOOL_NAME,
        codex_code_mode::WAIT_TOOL_NAME,
    ]);
}

#[tokio::test]
async fn v1_multi_agent_tools_defer_when_tool_search_available() {
    let plan = probe(|turn| {
        use_openai_provider(turn);
        turn.model_info.supports_search_tool = true;
        set_feature(turn, Feature::Collab, /*enabled*/ true);
        set_feature(turn, Feature::MultiAgentV2, /*enabled*/ false);
    })
    .await;

    plan.assert_visible_contains(&["tool_search"]);
    plan.assert_visible_lacks(&[
        "spawn_agent",
        "send_input",
        "resume_agent",
        "wait_agent",
        "close_agent",
        "interrupt_agent",
    ]);
    for tool_name in [
        "spawn_agent",
        "send_input",
        "resume_agent",
        "wait_agent",
        "close_agent",
    ] {
        let namespaced_tool_name = ToolName::namespaced(MULTI_AGENT_V1_NAMESPACE, tool_name);
        let namespaced_tool_name = namespaced_tool_name.to_string();
        assert!(
            plan.registered_names.contains(&namespaced_tool_name),
            "expected namespaced runtime for {tool_name}"
        );
        assert!(
            !plan
                .registered_names
                .contains(&ToolName::plain(tool_name).to_string()),
            "expected no plain runtime for deferred {tool_name}"
        );
        assert_eq!(plan.exposure(&namespaced_tool_name), ToolExposure::Deferred);
    }
    let ToolSpec::ToolSearch { description, .. } = plan.visible_spec("tool_search") else {
        panic!("expected visible tool_search spec");
    };
    assert!(description.contains("- Multi-agent tools: Spawn and manage sub-agents."));
}

fn flat_call(name: &str) -> crate::tools::router::ToolCall {
    crate::tools::router::ToolCall {
        tool_name: ToolName::plain(name),
        call_id: "call-1".to_string(),
        payload: crate::tools::context::ToolPayload::Function {
            arguments: "{}".to_string(),
        },
        encrypted_function_args: None,
    }
}

async fn router_with(
    configure_turn: fn(&mut TurnContext),
    tool_runtimes: Vec<Arc<dyn CoreToolRuntime>>,
) -> ToolRouter {
    let (_session, mut turn) = make_session_and_context().await;
    configure_turn(&mut turn);
    ToolRouter::from_tools(&turn, tool_runtimes, Vec::new(), &Default::default())
}

async fn mcp_router(configure_turn: fn(&mut TurnContext)) -> ToolRouter {
    router_with(
        configure_turn,
        vec![
            mcp_runtime("orders", "mcp__orders", "lookup", ToolExposure::Direct),
            mcp_runtime("orders", "mcp__orders", "cancel", ToolExposure::Direct),
        ],
    )
    .await
}

/// A custom provider on the Chat Completions wire (no provider-specific branch).
fn use_generic_chat_provider(turn: &mut TurnContext) {
    let provider_info = ModelProviderInfo {
        wire_api: codex_model_provider_info::WireApi::Chat,
        ..ModelProviderInfo::create_openai_provider(Some("http://localhost:1/v1".to_string()))
    };
    update_config(turn, |config| {
        config.model_provider_id = "custom-chat".to_string();
        config.model_provider = provider_info.clone();
    });
    turn.provider = create_model_provider(provider_info, turn.auth_manager.clone());
}

fn plain_dynamic_runtime(name: &str) -> Arc<dyn CoreToolRuntime> {
    let DynamicToolSpec::Function(function) =
        dynamic_tool(/*namespace*/ None, name, /*defer_loading*/ false)
    else {
        unreachable!("plain dynamic tools are functions");
    };
    Arc::new(
        crate::tools::handlers::DynamicToolHandler::new(&function)
            .expect("dynamic tool spec should build"),
    )
}

#[tokio::test]
async fn flat_mcp_names_never_shadow_registered_plain_tools() {
    let router = router_with(
        use_zai_provider,
        vec![
            plain_dynamic_runtime("mcp__orders__lookup"),
            mcp_runtime("orders", "mcp__orders", "lookup", ToolExposure::Direct),
        ],
    )
    .await;
    let specs = router.model_visible_specs();
    let flat_names: Vec<&str> = specs
        .iter()
        .map(ToolSpec::name)
        .filter(|name| name.starts_with("mcp__orders__lookup"))
        .collect();
    assert_eq!(flat_names.len(), 2, "{flat_names:?}");
    let hashed = flat_names
        .iter()
        .find(|name| **name != "mcp__orders__lookup")
        .expect("the MCP tool gets a hashed name");

    // The plain name still reaches the plain tool; the hashed one reaches MCP.
    let plain = router.resolve_flat_tool_call(flat_call("mcp__orders__lookup"));
    assert_eq!(plain.tool_name, ToolName::plain("mcp__orders__lookup"));
    let mcp = router.resolve_flat_tool_call(flat_call(hashed));
    assert_eq!(mcp.tool_name, ToolName::namespaced("mcp__orders", "lookup"));
}

#[tokio::test]
async fn flattened_mcp_tools_stay_within_the_tool_count_limit() {
    // Registered in reverse name order.
    let tool_runtimes = (0..200)
        .rev()
        .map(|index| {
            mcp_runtime(
                "bulk",
                "mcp__bulk",
                &format!("tool_{index:03}"),
                ToolExposure::Direct,
            )
        })
        .collect();
    let router = router_with(use_zai_provider, tool_runtimes).await;
    let specs = router.model_visible_specs();
    assert_eq!(specs.len(), 128);
    // The kept tools are the first ones by name, independent of registration order.
    assert!(
        specs
            .iter()
            .any(|spec| spec.name() == "mcp__bulk__tool_000")
    );
    assert!(
        !specs
            .iter()
            .any(|spec| spec.name() == "mcp__bulk__tool_199")
    );
    // #688: the left-out tools are reported (by the turn, once per session).
    let omitted = router.omitted_tools();
    assert_eq!(omitted.len(), 200 - 128);
    assert_eq!(
        omitted.first(),
        Some(&ToolName::namespaced("mcp__bulk", "tool_128"))
    );
    assert_eq!(
        omitted.last(),
        Some(&ToolName::namespaced("mcp__bulk", "tool_199"))
    );
}

#[tokio::test]
async fn nothing_is_reported_omitted_within_the_tool_count_limit() {
    let router = mcp_router(use_zai_provider).await;
    assert_eq!(router.omitted_tools(), &[] as &[ToolName]);
}

#[tokio::test]
async fn mcp_tools_are_flattened_and_resolved_on_wires_without_namespace_tools() {
    for configure_turn in [
        use_zai_provider as fn(&mut TurnContext),
        use_openrouter_provider,
        use_claude_plan_provider,
        use_generic_chat_provider,
    ] {
        let router = mcp_router(configure_turn).await;
        let specs = router.model_visible_specs();
        assert!(
            !specs
                .iter()
                .any(|spec| matches!(spec, ToolSpec::Namespace(_))),
            "namespaces must not reach a wire without namespace tools: {specs:?}"
        );
        for (flat, name) in [
            ("mcp__orders__lookup", "lookup"),
            ("mcp__orders__cancel", "cancel"),
        ] {
            let Some(ToolSpec::Function(tool)) = specs.iter().find(|spec| spec.name() == flat)
            else {
                panic!("expected flat MCP function `{flat}` in {specs:?}");
            };
            // The namespace is not sent, so the tool carries the server's description.
            assert_eq!(
                tool.description,
                format!("{name} test tool\n\nTools from orders.")
            );
            assert_eq!(tool.defer_loading, None);

            let resolved = router.resolve_flat_tool_call(flat_call(flat));
            assert_eq!(
                resolved.tool_name,
                ToolName::namespaced("mcp__orders", name)
            );
            assert!(router.tool_runtime(&resolved).is_some());
        }

        let unknown = router.resolve_flat_tool_call(flat_call("mcp__orders__missing"));
        assert_eq!(unknown.tool_name, ToolName::plain("mcp__orders__missing"));
    }
}

/// #547: every grouped tool, not only MCP tools, is offered under a flat name
/// on wires without namespaces and routed back to its namespaced runtime.
#[tokio::test]
async fn non_mcp_namespaced_tools_are_flattened_and_resolved_without_namespace_tools() {
    for configure_turn in [
        use_zai_provider as fn(&mut TurnContext),
        use_claude_plan_provider,
        use_generic_chat_provider,
    ] {
        let router = planned_router(
            |turn| {
                configure_turn(turn);
                set_feature(turn, Feature::SleepTool, /*enabled*/ true);
            },
            ToolPlanInputs {
                extension_tool_executors: vec![Arc::new(TestNamespaceExtensionTool {
                    namespace: "registry_extension",
                    tool_name: "lookup",
                })],
                dynamic_tools: vec![dynamic_tool(
                    Some("registry_dynamic"),
                    "lookup",
                    /*defer_loading*/ false,
                )],
                ..ToolPlanInputs::default()
            },
        )
        .await;
        let specs = router.model_visible_specs();
        assert!(
            !specs
                .iter()
                .any(|spec| matches!(spec, ToolSpec::Namespace(_))),
            "{specs:?}"
        );
        for (flat, namespace, name, namespace_description) in [
            (
                "clock__sleep",
                "clock",
                "sleep",
                "Tools for reading and waiting on time.",
            ),
            (
                "registry_extension__lookup",
                "registry_extension",
                "lookup",
                "Test namespace.",
            ),
            (
                "registry_dynamic__lookup",
                "registry_dynamic",
                "lookup",
                "registry_dynamic dynamic tools",
            ),
        ] {
            let Some(ToolSpec::Function(tool)) = specs.iter().find(|spec| spec.name() == flat)
            else {
                panic!("expected flat function `{flat}` in {specs:?}");
            };
            assert!(
                tool.description.ends_with(namespace_description),
                "{flat}: {}",
                tool.description
            );
            let resolved = router.resolve_flat_tool_call(flat_call(flat));
            assert_eq!(resolved.tool_name, ToolName::namespaced(namespace, name));
            assert!(router.tool_runtime(&resolved).is_some(), "{flat}");
        }
    }
}

/// #547: V1 sub-agent tools reach every provider without namespace tools (a
/// custom Chat Completions provider, Claude on the Anthropic wire), as plain
/// aliases, and are not offered twice.
#[tokio::test]
async fn v1_multi_agent_tools_reach_every_provider_without_namespace_tools() {
    for configure_turn in [
        use_generic_chat_provider as fn(&mut TurnContext),
        use_claude_plan_provider,
        use_zai_provider,
    ] {
        let plan = probe(|turn| {
            configure_turn(turn);
            set_feature(turn, Feature::Collab, /*enabled*/ true);
            set_feature(turn, Feature::MultiAgentV2, /*enabled*/ false);
        })
        .await;
        plan.assert_visible_contains(&[
            "spawn_agent_v1",
            "send_input_v1",
            "wait_agent_v1",
            "close_agent_v1",
            "resume_agent_v1",
        ]);
        plan.assert_visible_lacks(&[
            MULTI_AGENT_V1_NAMESPACE,
            "spawn_agent",
            "multi_agent_v1__spawn_agent",
            "multi_agent_v1__resume_agent",
        ]);
    }
}

/// Built-in grouped tools are kept before MCP tools when the tool-count limit
/// leaves room for only some of them, even when their names sort after the
/// MCP tools'.
#[tokio::test]
async fn flattened_builtin_tools_keep_their_place_before_mcp_tools() {
    let tool_runtimes = (0..200)
        .map(|index| {
            mcp_runtime(
                "bulk",
                "mcp__bulk",
                &format!("tool_{index:03}"),
                ToolExposure::Direct,
            )
        })
        .collect();
    let router = planned_router(
        use_zai_provider,
        ToolPlanInputs {
            tool_runtimes,
            extension_tool_executors: vec![Arc::new(TestNamespaceExtensionTool {
                namespace: "zz_extension",
                tool_name: "lookup",
            })],
            ..ToolPlanInputs::default()
        },
    )
    .await;
    let specs = router.model_visible_specs();
    assert_eq!(specs.len(), 128);
    assert!(
        specs
            .iter()
            .any(|spec| spec.name() == "zz_extension__lookup")
    );
    assert!(
        specs
            .iter()
            .any(|spec| spec.name() == "mcp__bulk__tool_000")
    );
}

/// Memory reads stay off wires without namespaces until sending stored
/// memories to those providers is decided (#547).
#[tokio::test]
async fn memory_tools_are_not_offered_without_namespace_tools() {
    let router = planned_router(
        use_generic_chat_provider,
        ToolPlanInputs {
            extension_tool_executors: vec![Arc::new(TestNamespaceExtensionTool {
                namespace: "memories",
                tool_name: "read",
            })],
            ..ToolPlanInputs::default()
        },
    )
    .await;
    assert!(
        !router
            .model_visible_specs()
            .iter()
            .any(|spec| spec.name().starts_with("memories")),
    );
}

#[tokio::test]
async fn mcp_tools_stay_namespaced_on_the_responses_wire() {
    let router = mcp_router(use_openai_provider).await;
    let specs = router.model_visible_specs();
    let Some(ToolSpec::Namespace(namespace)) =
        specs.iter().find(|spec| spec.name() == "mcp__orders")
    else {
        panic!("expected MCP namespace in {specs:?}");
    };
    assert_eq!(namespace.tools.len(), 2);
    assert!(
        !specs
            .iter()
            .any(|spec| spec.name() == "mcp__orders__lookup"),
        "flat names are only for wires without namespaces"
    );
    let call = router.resolve_flat_tool_call(flat_call("mcp__orders__lookup"));
    assert_eq!(call.tool_name, ToolName::plain("mcp__orders__lookup"));
}

#[tokio::test]
async fn v1_multi_agent_tools_flatten_for_openrouter_without_namespace_tools() {
    let plan = probe(|turn| {
        use_openrouter_provider(turn);
        set_feature(turn, Feature::Collab, /*enabled*/ true);
        set_feature(turn, Feature::MultiAgentV2, /*enabled*/ false);
    })
    .await;

    plan.assert_visible_contains(&[
        "spawn_agent_v1",
        "send_input_v1",
        "wait_agent_v1",
        "close_agent_v1",
    ]);
    plan.assert_visible_lacks(&[MULTI_AGENT_V1_NAMESPACE, "spawn_agent"]);
    plan.assert_registered_contains(&[
        "spawn_agent_v1",
        "send_input_v1",
        "wait_agent_v1",
        "close_agent_v1",
        &ToolName::namespaced(MULTI_AGENT_V1_NAMESPACE, "spawn_agent").to_string(),
    ]);
    for tool_name in [
        "spawn_agent_v1",
        "send_input_v1",
        "wait_agent_v1",
        "close_agent_v1",
    ] {
        assert_eq!(plan.exposure(tool_name), ToolExposure::Direct);
        let ToolSpec::Function(tool) = plan.visible_spec(tool_name) else {
            panic!("expected {tool_name} to be exposed as a plain function");
        };
        assert_eq!(tool.name, tool_name);
        assert!(
            tool.description
                .contains("Compatibility alias for V1 subagent support")
        );
    }
}

#[tokio::test]
async fn multi_agent_v2_can_use_configured_tool_namespace() {
    let namespaced = probe(|turn| {
        use_openai_provider(turn);
        set_feature(turn, Feature::MultiAgentV2, /*enabled*/ true);
        update_config(turn, |config| {
            config.multi_agent_v2.tool_namespace = Some("agents".to_string());
        });
    })
    .await;

    namespaced.assert_visible_contains(&["agents"]);
    namespaced.assert_visible_lacks(&["assign_task"]);
    assert!(
        !namespaced
            .registered_names
            .contains(&ToolName::namespaced("agents", "assign_task").to_string()),
        "expected no namespaced runtime for assign_task"
    );
    assert!(
        !namespaced
            .namespace_function_names("agents")
            .iter()
            .any(|name| name == "assign_task"),
        "expected assign_task to be absent from agents namespace"
    );
    for tool_name in [
        "spawn_agent",
        "send_message",
        "followup_task",
        "wait_agent",
        "interrupt_agent",
        "list_agents",
    ] {
        namespaced.assert_visible_lacks(&[tool_name]);
        assert!(
            namespaced
                .registered_names
                .contains(&ToolName::namespaced("agents", tool_name).to_string()),
            "expected namespaced runtime for {tool_name}"
        );
        assert!(
            !namespaced
                .registered_names
                .contains(&ToolName::plain(tool_name).to_string()),
            "expected no plain runtime for {tool_name}"
        );
        assert!(
            namespaced
                .namespace_function_names("agents")
                .iter()
                .any(|name| name == tool_name),
            "expected {tool_name} in agents namespace"
        );
    }
}

#[tokio::test]
async fn multi_agent_v2_namespace_is_supported_by_bedrock_provider() {
    let plan = probe(|turn| {
        set_feature(turn, Feature::MultiAgentV2, /*enabled*/ true);
        update_config(turn, |config| {
            config.multi_agent_v2.tool_namespace = Some("agents".to_string());
        });
        use_bedrock_provider(turn);
    })
    .await;

    plan.assert_visible_contains(&["agents"]);
    plan.assert_visible_lacks(&["spawn_agent", "send_message", "list_agents"]);
    assert!(
        !plan
            .registered_names
            .contains(&ToolName::plain("spawn_agent").to_string())
    );
    assert!(
        plan.registered_names
            .contains(&ToolName::namespaced("agents", "spawn_agent").to_string())
    );
}

#[tokio::test]
async fn code_mode_only_can_expose_namespaced_multi_agent_v2_as_normal_tools() {
    let plan = probe(|turn| {
        set_features(
            turn,
            &[
                Feature::CodeMode,
                Feature::CodeModeOnly,
                Feature::MultiAgentV2,
            ],
        );
        update_config(turn, |config| {
            config.multi_agent_v2.non_code_mode_only = true;
            config.multi_agent_v2.tool_namespace = Some("agents".to_string());
        });
    })
    .await;

    assert_eq!(
        plan.visible_names,
        vec![
            "exec",
            "wait",
            "request_user_input",
            "agents",
            "spawn_agent_plaintext",
            "send_message_plaintext",
            "followup_task_plaintext",
            // Hosted Responses tool.
            "web_search",
        ]
    );
    assert!(
        !plan
            .namespace_function_names("agents")
            .iter()
            .any(|name| name == "assign_task"),
        "expected assign_task to be absent from agents namespace"
    );
    for tool_name in [
        "spawn_agent",
        "send_message",
        "followup_task",
        "wait_agent",
        "interrupt_agent",
        "list_agents",
    ] {
        assert!(
            plan.namespace_function_names("agents")
                .iter()
                .any(|name| name == tool_name),
            "expected {tool_name} in agents namespace"
        );
    }
}

#[tokio::test]
async fn hosted_web_search_fallback_follows_winning_browser_runtime() {
    let plan = probe_with(
        |turn| {
            set_feature(turn, Feature::StandaloneWebSearch, /*enabled*/ true);
            set_web_search_mode(turn, WebSearchMode::Live);
        },
        ToolPlanInputs {
            tool_runtimes: vec![mcp_runtime(
                "browser_collision",
                "web",
                "run",
                ToolExposure::Direct,
            )],
            extension_tool_executors: vec![Arc::new(TestNamespaceExtensionTool {
                namespace: "web",
                tool_name: "run",
            })],
            ..ToolPlanInputs::default()
        },
    )
    .await;

    let ToolSpec::Namespace(namespace) = plan.visible_spec("web") else {
        panic!("expected the winning browser namespace");
    };
    assert_eq!(namespace.description, "Tools from browser_collision.");
    plan.assert_visible_contains(&["web_search"]);
}

#[tokio::test]
async fn standalone_image_generation_is_offered_on_an_openai_api_key() {
    let image_generation_tool = Arc::new(TestNamespaceExtensionTool {
        namespace: "image_gen",
        tool_name: "imagegen",
    });
    // An OpenAI API key on the built-in provider reaches the public Images
    // API with that key, so the tool is offered there too.
    let api_key = probe_with(
        |turn| {
            use_api_key_auth(turn);
            turn.model_info.input_modalities = vec![InputModality::Image];
        },
        ToolPlanInputs {
            extension_tool_executors: vec![image_generation_tool.clone()],
            ..Default::default()
        },
    )
    .await;
    api_key.assert_visible_contains(&["image_gen"]);

    // Another provider's API key is not an OpenAI key: no Images API to reach.
    let other_provider_api_key = probe_with(
        |turn| {
            use_api_key_auth(turn);
            let provider_info = ModelProviderInfo {
                name: "Custom".to_string(),
                ..ModelProviderInfo::create_openai_provider(Some(
                    "https://example.invalid/v1".to_string(),
                ))
            };
            update_config(turn, |config| {
                config.model_provider_id = "custom".to_string();
                config.model_provider = provider_info.clone();
            });
            turn.provider = create_model_provider(provider_info, turn.auth_manager.clone());
            turn.model_info.input_modalities = vec![InputModality::Image];
        },
        ToolPlanInputs {
            extension_tool_executors: vec![image_generation_tool.clone()],
            ..Default::default()
        },
    )
    .await;
    other_provider_api_key.assert_visible_lacks(&["image_gen"]);

    // A custom provider that only borrows the name is not the built-in one.
    let named_openai = probe_with(
        |turn| {
            use_api_key_auth(turn);
            let provider_info = ModelProviderInfo::create_openai_provider(Some(
                "https://proxy.example.invalid/v1".to_string(),
            ));
            update_config(turn, |config| {
                config.model_provider_id = "my-proxy".to_string();
                config.model_provider = provider_info.clone();
            });
            turn.provider = create_model_provider(provider_info, turn.auth_manager.clone());
            turn.model_info.input_modalities = vec![InputModality::Image];
        },
        ToolPlanInputs {
            extension_tool_executors: vec![image_generation_tool],
            ..Default::default()
        },
    )
    .await;
    named_openai.assert_visible_lacks(&["image_gen"]);
}

#[tokio::test]
async fn hosted_web_search_and_standalone_image_generation_follow_runtime_gates() {
    let image_generation_tool = Arc::new(TestNamespaceExtensionTool {
        namespace: "image_gen",
        tool_name: "imagegen",
    });
    let image_generation = probe_with(
        |turn| {
            use_chatgpt_auth(turn);
            turn.model_info.input_modalities = vec![InputModality::Image];
        },
        ToolPlanInputs {
            extension_tool_executors: vec![image_generation_tool.clone()],
            ..Default::default()
        },
    )
    .await;
    image_generation.assert_visible_contains(&["image_gen"]);

    let extension_disabled = probe_with(
        |turn| {
            use_chatgpt_auth(turn);
            set_feature(turn, Feature::ImageGeneration, /*enabled*/ false);
            turn.model_info.input_modalities = vec![InputModality::Image];
        },
        ToolPlanInputs {
            extension_tool_executors: vec![image_generation_tool.clone()],
            ..Default::default()
        },
    )
    .await;
    extension_disabled.assert_visible_lacks(&["image_gen"]);

    let text_only_model = probe_with(
        |turn| {
            use_chatgpt_auth(turn);
            turn.model_info.input_modalities = vec![];
        },
        ToolPlanInputs {
            extension_tool_executors: vec![image_generation_tool.clone()],
            ..Default::default()
        },
    )
    .await;
    text_only_model.assert_visible_lacks(&["image_gen"]);

    let unsupported_provider = probe_with(
        |turn| {
            use_bedrock_provider(turn);
            turn.model_info.input_modalities = vec![InputModality::Image];
        },
        ToolPlanInputs {
            extension_tool_executors: vec![image_generation_tool],
            ..Default::default()
        },
    )
    .await;
    unsupported_provider.assert_visible_lacks(&["image_gen"]);

    let live_web_search = probe(|turn| {
        use_openai_provider(turn);
        set_web_search_mode(turn, WebSearchMode::Live);
        turn.model_info.web_search_tool_type = WebSearchToolType::TextAndImage;
    })
    .await;
    assert_eq!(
        live_web_search.visible_spec("web_search"),
        &ToolSpec::WebSearch {
            external_web_access: Some(true),
            indexed_web_access: None,
            filters: None,
            user_location: None,
            search_context_size: None,
            search_content_types: Some(vec!["text".to_string(), "image".to_string()]),
        }
    );

    let zai_native_web_search = probe(|turn| {
        use_zai_provider(turn);
        set_web_search_mode(turn, WebSearchMode::Live);
        turn.model_info.supports_search_tool = false;
    })
    .await;
    zai_native_web_search.assert_visible_contains(&["web_search"]);

    let zai_native_web_search_for_responses_lite = probe(|turn| {
        use_zai_provider(turn);
        set_web_search_mode(turn, WebSearchMode::Live);
        turn.model_info.supports_search_tool = false;
        turn.model_info.use_responses_lite = true;
    })
    .await;
    zai_native_web_search_for_responses_lite.assert_visible_lacks(&["web_search"]);

    let openrouter_server_web_search_for_chat = probe(|turn| {
        use_openrouter_provider(turn);
        set_web_search_mode(turn, WebSearchMode::Live);
        turn.model_info.supports_search_tool = false;
        turn.model_info.use_responses_lite = true;
    })
    .await;
    openrouter_server_web_search_for_chat.assert_visible_lacks(&["web_search"]);

    let code_mode_only = probe(|turn| {
        use_chatgpt_auth(turn);
        set_features(turn, &[Feature::CodeModeOnly, Feature::MultiAgentV2]);
        set_web_search_mode(turn, WebSearchMode::Live);
        turn.model_info.input_modalities = vec![InputModality::Image];
    })
    .await;
    assert_eq!(
        code_mode_only.visible_names,
        vec![
            // Code-mode entrypoints.
            codex_code_mode::PUBLIC_TOOL_NAME,
            codex_code_mode::WAIT_TOOL_NAME,
            "request_user_input",
            // Multi-agent v2 tools.
            MULTI_AGENT_V2_NAMESPACE,
            "spawn_agent_plaintext",
            "send_message_plaintext",
            "followup_task_plaintext",
            // Hosted Responses tools.
            "web_search",
        ]
    );

    let standalone_web_search_without_web_run = probe(|turn| {
        use_openai_provider(turn);
        set_feature(turn, Feature::StandaloneWebSearch, /*enabled*/ true);
        set_web_search_mode(turn, WebSearchMode::Live);
    })
    .await;
    standalone_web_search_without_web_run.assert_visible_contains(&["web_search"]);

    let standalone_web_search_with_dynamic_web_run = probe_with(
        |turn| {
            set_feature(turn, Feature::StandaloneWebSearch, /*enabled*/ true);
            set_web_search_mode(turn, WebSearchMode::Live);
        },
        ToolPlanInputs {
            dynamic_tools: vec![dynamic_tool(
                Some("web"),
                "run",
                /*defer_loading*/ false,
            )],
            ..Default::default()
        },
    )
    .await;
    standalone_web_search_with_dynamic_web_run.assert_visible_contains(&["web", "web_search"]);

    let standalone_web_search_with_mcp_web_run = probe_with(
        |turn| {
            set_feature(turn, Feature::StandaloneWebSearch, /*enabled*/ true);
            set_web_search_mode(turn, WebSearchMode::Live);
        },
        ToolPlanInputs {
            tool_runtimes: vec![mcp_runtime(
                "web_server",
                "web",
                "run",
                ToolExposure::Direct,
            )],
            ..Default::default()
        },
    )
    .await;
    standalone_web_search_with_mcp_web_run.assert_visible_contains(&["web", "web_search"]);

    let standalone_web_search = probe_with(
        |turn| {
            use_openai_provider(turn);
            set_feature(turn, Feature::StandaloneWebSearch, /*enabled*/ true);
            set_web_search_mode(turn, WebSearchMode::Live);
        },
        ToolPlanInputs {
            extension_tool_executors: vec![Arc::new(TestNamespaceExtensionTool {
                namespace: "web",
                tool_name: "run",
            })],
            ..Default::default()
        },
    )
    .await;
    standalone_web_search.assert_visible_lacks(&["web_search"]);

    let unsupported_provider = probe(|turn| {
        set_web_search_mode(turn, WebSearchMode::Live);
        use_bedrock_provider(turn);
    })
    .await;
    unsupported_provider.assert_visible_lacks(&["web_search"]);
}

// Issue #820: real tool planning and serde, with disposable config and no provider call.
fn issue820_use_custom_provider(turn: &mut TurnContext, base_url: &str, wire_api: &str) {
    let provider_info: ModelProviderInfo = toml::from_str(&format!(
        r#"
name = "Custom official endpoint"
base_url = "{base_url}"
env_key = "ISSUE820_SYNTHETIC_UNSET_KEY"
wire_api = "{wire_api}"
billing = "pay_per_use"
"#
    ))
    .expect("synthetic provider config");
    // Wire-schema compatibility must not relabel this provider or enable built-in auth.
    assert!(!provider_info.is_openai());
    assert!(!provider_info.requires_openai_auth);
    update_config(turn, |config| {
        config.model_provider_id = "myopenai".to_string();
        config.model_provider = provider_info.clone();
    });
    turn.provider = create_model_provider(provider_info, turn.auth_manager.clone());
}

#[tokio::test]
async fn issue820_custom_official_responses_tools_match_builtin() {
    let builtin = probe(|turn| {
        set_feature(turn, Feature::MultiAgentV2, /*enabled*/ true);
    })
    .await;
    let expected = serde_json::to_value(builtin.visible_spec(MULTI_AGENT_V2_NAMESPACE))
        .expect("built-in collaboration serialization");
    for base_url in [
        "https://api.openai.com/v1",
        "https://api.openai.com/v1/",
        "https://api.openai.com:443/v1",
        "https://API.OPENAI.COM/v1",
        // #863 item 3: the fully qualified host and OpenAI's regional domains.
        "https://api.openai.com./v1",
        "https://eu.api.openai.com/v1",
        "https://jp.api.openai.com/v1/",
    ] {
        let custom = probe(|turn| {
            issue820_use_custom_provider(turn, base_url, "responses");
            set_feature(turn, Feature::MultiAgentV2, /*enabled*/ true);
        })
        .await;
        let captured = serde_json::to_value(custom.visible_spec(MULTI_AGENT_V2_NAMESPACE))
            .expect("custom collaboration serialization");
        assert_eq!(captured, expected, "reserved tools diverged for {base_url}");
    }
}

#[tokio::test]
async fn issue820_custom_official_responses_register_plaintext_adapters() {
    let custom = probe(|turn| {
        issue820_use_custom_provider(turn, "https://api.openai.com/v1", "responses");
        set_feature(turn, Feature::MultiAgentV2, /*enabled*/ true);
    })
    .await;
    for name in [
        "spawn_agent_plaintext",
        "send_message_plaintext",
        "followup_task_plaintext",
    ] {
        custom.assert_registered_contains(&[name]);
        let captured =
            serde_json::to_value(custom.visible_spec(name)).expect("adapter serialization");
        assert_eq!(
            captured.pointer("/parameters/properties/message/encrypted"),
            None
        );
    }
}

#[tokio::test]
async fn issue820_other_responses_endpoints_keep_provider_neutral_tools() {
    for base_url in [
        "https://openrouter.ai/api/v1",
        "https://example.openai.azure.com/openai/v1",
        "https://api.openai.com.example.test/v1",
        "https://xx.api.openai.com/v1",
        "https://eu.api.openai.com:8443/v1",
        "https://api.openai.com@provider.example/v1",
        "https://provider.example/api.openai.com/v1",
        "http://api.openai.com/v1",
        "https://api.openai.com:8443/v1",
        "https://api.openai.com/proxy/v1",
        "https://api.openai.com/v1?endpoint=other",
        "https://api.openai.com/v1#other",
        "not-a-url",
    ] {
        let plan = probe(|turn| {
            issue820_use_custom_provider(turn, base_url, "responses");
            set_feature(turn, Feature::MultiAgentV2, /*enabled*/ true);
        })
        .await;
        let ToolSpec::Namespace(namespace) = plan.visible_spec(MULTI_AGENT_V2_NAMESPACE) else {
            panic!("expected collaboration namespace for {base_url}");
        };
        for name in ["spawn_agent", "send_message", "followup_task"] {
            let function = namespace
                .tools
                .iter()
                .find_map(|tool| match tool {
                    ResponsesApiNamespaceTool::Function(tool) if tool.name == name => Some(tool),
                    _ => None,
                })
                .expect("provider-neutral collaboration function");
            assert_eq!(
                function
                    .parameters
                    .properties
                    .as_ref()
                    .expect("object params")["message"]
                    .encrypted,
                None,
                "{base_url} must retain plaintext {name}"
            );
        }
        plan.assert_registered_lacks(&[
            "spawn_agent_plaintext",
            "send_message_plaintext",
            "followup_task_plaintext",
        ]);
    }
}

#[tokio::test]
async fn issue820_chat_wire_keeps_plaintext_and_no_reserved_adapters() {
    let plan = probe(|turn| {
        issue820_use_custom_provider(turn, "https://api.openai.com/v1", "chat");
        set_feature(turn, Feature::MultiAgentV2, /*enabled*/ true);
    })
    .await;
    plan.assert_registered_lacks(&[
        "spawn_agent_plaintext",
        "send_message_plaintext",
        "followup_task_plaintext",
    ]);
    // Chat has no namespaces: the v2 handler is registered as a plain function.
    let captured = serde_json::to_value(plan.visible_spec("followup_task"))
        .expect("flat chat tool serialization");
    assert_eq!(
        captured.pointer("/parameters/properties/message/encrypted"),
        None
    );
}

#[tokio::test]
async fn issue820_disabled_v2_does_not_add_reserved_adapters() {
    let plan = probe(|turn| {
        issue820_use_custom_provider(turn, "https://api.openai.com/v1", "responses");
        set_feature(turn, Feature::MultiAgentV2, /*enabled*/ false);
    })
    .await;
    plan.assert_visible_lacks(&[MULTI_AGENT_V2_NAMESPACE]);
    plan.assert_registered_lacks(&[
        "spawn_agent_plaintext",
        "send_message_plaintext",
        "followup_task_plaintext",
    ]);
}
