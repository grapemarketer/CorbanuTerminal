use crate::agent::exceeds_thread_spawn_depth_limit;
use crate::agent::next_thread_spawn_depth;
use crate::connectors::AppInfo;
use crate::environment_selection::TurnEnvironmentSnapshot;
use crate::mcp_tool_exposure::build_mcp_tool_runtimes;
use crate::security::protected_surface::ToolOrigin;
use crate::session::session::Session;
use crate::session::turn_context::TurnContext;
use crate::tools::code_mode::default_exec_yield_time_override_ms;
use crate::tools::code_mode::execute_spec::create_code_mode_tool;
use crate::tools::collaboration_wire::uses_openai_reserved_collaboration_schema;
use crate::tools::context::ToolInvocation;
use crate::tools::effective_tool_mode;
use crate::tools::flat_tool_names::FlatToolNames;
use crate::tools::flat_tool_names::flatten_namespaced_tools;
use crate::tools::handlers::ApplyPatchHandler;
use crate::tools::handlers::CodeModeExecuteHandler;
use crate::tools::handlers::CodeModeWaitHandler;
use crate::tools::handlers::CurrentTimeHandler;
use crate::tools::handlers::DynamicToolHandler;
use crate::tools::handlers::ExecCommandHandler;
use crate::tools::handlers::ExecCommandHandlerOptions;
use crate::tools::handlers::GetContextRemainingHandler;
use crate::tools::handlers::ListAvailablePluginsToInstallHandler;
use crate::tools::handlers::ListMcpResourceTemplatesHandler;
use crate::tools::handlers::ListMcpResourcesHandler;
use crate::tools::handlers::NewContextWindowHandler;
use crate::tools::handlers::PlanHandler;
use crate::tools::handlers::ReadMcpResourceHandler;
use crate::tools::handlers::RequestPermissionsHandler;
use crate::tools::handlers::RequestPluginInstallHandler;
use crate::tools::handlers::RequestUserInputHandler;
use crate::tools::handlers::ShellCommandHandler;
use crate::tools::handlers::ShellCommandHandlerOptions;
use crate::tools::handlers::SleepHandler;
use crate::tools::handlers::StructuredEditHandler;
use crate::tools::handlers::StructuredWriteHandler;
use crate::tools::handlers::TestSyncHandler;
use crate::tools::handlers::ToolSearchHandlerCache;
use crate::tools::handlers::ViewImageHandler;
use crate::tools::handlers::WaitForEnvironmentHandler;
use crate::tools::handlers::WriteStdinHandler;
use crate::tools::handlers::extension_tools::ExtensionToolAdapter;
use crate::tools::handlers::multi_agents::CloseAgentHandler;
use crate::tools::handlers::multi_agents::ResumeAgentHandler;
use crate::tools::handlers::multi_agents::SendInputHandler;
use crate::tools::handlers::multi_agents::SpawnAgentHandler;
use crate::tools::handlers::multi_agents::WaitAgentHandler;
use crate::tools::handlers::multi_agents_common::DEFAULT_WAIT_TIMEOUT_MS;
use crate::tools::handlers::multi_agents_common::MAX_WAIT_TIMEOUT_MS;
use crate::tools::handlers::multi_agents_common::MIN_WAIT_TIMEOUT_MS;
use crate::tools::handlers::multi_agents_spec::MULTI_AGENT_V1_NAMESPACE;
use crate::tools::handlers::multi_agents_spec::SpawnAgentRuntime;
use crate::tools::handlers::multi_agents_spec::SpawnAgentToolOptions;
use crate::tools::handlers::multi_agents_spec::WaitAgentTimeoutOptions;
use crate::tools::handlers::multi_agents_spec::apply_openai_reserved_collaboration_schema;
use crate::tools::handlers::multi_agents_v2::FollowupTaskHandler as FollowupTaskHandlerV2;
use crate::tools::handlers::multi_agents_v2::InterruptAgentHandler;
use crate::tools::handlers::multi_agents_v2::ListAgentsHandler as ListAgentsHandlerV2;
use crate::tools::handlers::multi_agents_v2::PlaintextFollowupTaskHandler;
use crate::tools::handlers::multi_agents_v2::PlaintextSendMessageHandler;
use crate::tools::handlers::multi_agents_v2::PlaintextSpawnAgentHandler;
use crate::tools::handlers::multi_agents_v2::SendMessageHandler as SendMessageHandlerV2;
use crate::tools::handlers::multi_agents_v2::SpawnAgentHandler as SpawnAgentHandlerV2;
use crate::tools::handlers::multi_agents_v2::WaitAgentHandler as WaitAgentHandlerV2;
use crate::tools::handlers::native_structured_edit_protocol_enabled;
use crate::tools::handlers::tool_search_spec::ToolSearchSourceListing;
use crate::tools::handlers::view_image_spec::ViewImageToolOptions;
use crate::tools::hosted_spec::WebSearchToolOptions;
use crate::tools::hosted_spec::create_web_search_tool;
use crate::tools::registry::CoreToolRuntime;
use crate::tools::registry::ToolExposure;
use crate::tools::registry::ToolRegistry;
use crate::tools::registry::override_tool_exposure;
use crate::tools::router::ToolRouter;
use codex_extension_api::ExtensionData;
use codex_features::Feature;
use codex_login::AuthManager;
use codex_model_provider_info::OPENAI_PROVIDER_ID;
use codex_protocol::account::PlanType;
use codex_protocol::auth::AuthMode;
use codex_protocol::config_types::WebSearchMode;
use codex_protocol::dynamic_tools::DynamicToolNamespaceTool;
use codex_protocol::dynamic_tools::DynamicToolSpec;
use codex_protocol::openai_models::ConfigShellToolType;
use codex_protocol::openai_models::InputModality;
use codex_protocol::openai_models::ModelPreset;
use codex_protocol::openai_models::ToolMode;
use codex_protocol::permissions::FileSystemSandboxKind;
use codex_protocol::protocol::MultiAgentVersion;
use codex_tools::ResponsesApiNamespace;
use codex_tools::ResponsesApiNamespaceTool;
use codex_tools::ResponsesApiTool;
use codex_tools::TOOL_SEARCH_TOOL_NAME;
use codex_tools::ToolCall as ExtensionToolCall;
use codex_tools::ToolEnvironmentMode;
use codex_tools::ToolExecutor;
use codex_tools::ToolName;
use codex_tools::ToolSearchInfo;
use codex_tools::ToolSpec;
use codex_tools::UnifiedExecShellMode;
use codex_tools::can_request_original_image_detail;
use codex_tools::collect_code_mode_exec_prompt_tool_definitions;
use codex_tools::collect_request_plugin_install_entries;
use codex_tools::default_namespace_description;
use codex_tools::request_user_input_available_modes;
use codex_tools::shell_command_backend_for_features;
use codex_tools::shell_type_for_model_and_features;
use codex_utils_string::take_bytes_at_char_boundary;
use futures::future::BoxFuture;
use std::collections::BTreeMap;
use std::collections::btree_map::Entry;
use std::sync::Arc;
use tracing::instrument;
use tracing::warn;

/// Most function tools one request may carry on wires without namespaces: the
/// smallest documented limit among them (OpenAI Chat Completions rejects more
/// than 128 tools).
pub(crate) const MAX_TOOLS_WITHOUT_NAMESPACES: usize = 128;
/// The share of a namespace's description each of its flat tools repeats.
const MAX_FLAT_NAMESPACE_DESCRIPTION_BYTES: usize = 300;
/// `ext/memories` tool namespace.
const MEMORY_TOOLS_NAMESPACE: &str = "memories";
const MULTI_AGENT_V2_NAMESPACE_DESCRIPTION: &str = "Tools for spawning and managing sub-agents.";
const IMAGE_GEN_NAMESPACE: &str = "image_gen";
const IMAGEGEN_TOOL_NAME: &str = "imagegen";

#[derive(Clone, Copy)]
struct CoreToolPlanContext<'a> {
    turn_context: &'a TurnContext,
    environments: &'a TurnEnvironmentSnapshot,
    mcp: &'a codex_mcp::McpBinding,
    tool_suggest_candidates: Option<&'a crate::tools::router::ToolSuggestCandidates>,
    wait_for_environment_tool_config: Option<&'a Arc<crate::WaitForEnvironmentToolConfig>>,
    default_agent_type_description: &'a str,
    wait_agent_timeouts: WaitAgentTimeoutOptions,
}

#[instrument(level = "trace", skip_all)]
pub(crate) fn build_tool_router(
    session: &Session,
    turn_context: &TurnContext,
    environments: &TurnEnvironmentSnapshot,
    mcp: &codex_mcp::McpBinding,
    connectors: Option<&[AppInfo]>,
    step_store: &ExtensionData,
    tool_suggest_candidates: Option<&crate::tools::router::ToolSuggestCandidates>,
) -> ToolRouter {
    let default_agent_type_description =
        crate::agent::role::spawn_tool_spec::build(&std::collections::BTreeMap::new());
    let wait_for_environment_tool_config = session
        .services
        .thread_extension_data
        .get::<crate::WaitForEnvironmentToolConfig>();
    let context = CoreToolPlanContext {
        turn_context,
        environments,
        mcp,
        tool_suggest_candidates,
        wait_for_environment_tool_config: wait_for_environment_tool_config.as_ref(),
        default_agent_type_description: &default_agent_type_description,
        wait_agent_timeouts: wait_agent_timeout_options(turn_context),
    };
    let mut registry = ToolRegistry::default();
    add_core_tool_sources(&context, &mut registry);

    let hosted_specs = if crate::guardian::is_guardian_reviewer_source(&turn_context.session_source)
    {
        Vec::new()
    } else {
        for runtime in build_mcp_tool_runtimes(
            mcp.tools(),
            connectors,
            &turn_context.config,
            search_tool_enabled(turn_context),
        ) {
            registry.register_external(runtime);
        }
        let standalone_web_search_tool = append_extension_tool_executors(
            turn_context,
            extension_tool_executors(session, step_store),
            &mut registry,
        );
        append_dynamic_tool_runtimes(&turn_context.dynamic_tools, &mut registry);
        hosted_model_tool_specs(turn_context, standalone_web_search_tool.as_slice())
    };

    finalize_tool_router(
        turn_context,
        registry,
        hosted_specs,
        &session.services.tool_search_handler_cache,
    )
}

#[cfg(test)]
#[instrument(level = "trace", skip_all)]
pub(crate) fn build_core_tool_runtimes(
    turn_context: &TurnContext,
    environments: &TurnEnvironmentSnapshot,
    mcp: &codex_mcp::McpBinding,
    tool_suggest_candidates: Option<&crate::tools::router::ToolSuggestCandidates>,
    wait_for_environment_tool_config: Option<&Arc<crate::WaitForEnvironmentToolConfig>>,
) -> Vec<Arc<dyn CoreToolRuntime>> {
    let default_agent_type_description =
        crate::agent::role::spawn_tool_spec::build(&std::collections::BTreeMap::new());
    let context = CoreToolPlanContext {
        turn_context,
        environments,
        mcp,
        tool_suggest_candidates,
        wait_for_environment_tool_config,
        default_agent_type_description: &default_agent_type_description,
        wait_agent_timeouts: wait_agent_timeout_options(turn_context),
    };
    let mut registry = ToolRegistry::default();
    add_core_tool_sources(&context, &mut registry);
    registry.runtimes().cloned().collect()
}

#[cfg(test)]
#[instrument(level = "trace", skip_all)]
pub(crate) fn append_source_tool_runtimes(
    turn_context: &TurnContext,
    tool_runtimes: &mut Vec<Arc<dyn CoreToolRuntime>>,
    mcp_tool_runtimes: Vec<Arc<dyn CoreToolRuntime>>,
    extension_tool_executors: impl IntoIterator<Item = Arc<dyn ToolExecutor<ExtensionToolCall>>>,
    dynamic_tools: &[DynamicToolSpec],
) -> Vec<ToolSpec> {
    if crate::guardian::is_guardian_reviewer_source(&turn_context.session_source) {
        return Vec::new();
    }

    let mut registry = ToolRegistry::from_tools(std::mem::take(tool_runtimes));
    for runtime in mcp_tool_runtimes {
        registry.register_external(runtime);
    }
    let standalone_web_search_tool =
        append_extension_tool_executors(turn_context, extension_tool_executors, &mut registry);
    append_dynamic_tool_runtimes(dynamic_tools, &mut registry);
    tool_runtimes.extend(registry.runtimes().cloned());
    hosted_model_tool_specs(turn_context, standalone_web_search_tool.as_slice())
}

#[instrument(level = "trace", skip_all)]
pub(crate) fn extension_tool_executors<'a>(
    session: &'a Session,
    step_store: &'a ExtensionData,
) -> impl Iterator<Item = Arc<dyn ToolExecutor<ExtensionToolCall>>> + 'a {
    session
        .services
        .extensions
        .tool_contributors()
        .iter()
        .flat_map(move |contributor| {
            contributor.tools_for_step(
                &session.services.session_extension_data,
                &session.services.thread_extension_data,
                step_store,
            )
        })
}

#[instrument(level = "trace", skip_all)]
pub(crate) fn finalize_tool_router(
    turn_context: &TurnContext,
    mut registry: ToolRegistry,
    hosted_specs: Vec<ToolSpec>,
    tool_search_handler_cache: &ToolSearchHandlerCache,
) -> ToolRouter {
    apply_direct_model_only_namespace_overrides(turn_context, &mut registry);
    let code_mode_enabled = matches!(
        effective_tool_mode(turn_context),
        ToolMode::CodeMode | ToolMode::CodeModeOnly
    );
    if code_mode_enabled {
        registry.remove(&ToolName::plain(codex_code_mode::PUBLIC_TOOL_NAME));
        registry.remove(&ToolName::plain(codex_code_mode::WAIT_TOOL_NAME));
    }
    let tool_search_name = ToolName::plain(TOOL_SEARCH_TOOL_NAME);
    if search_tool_enabled(turn_context)
        && registry.runtimes().any(|runtime| {
            runtime.tool_name() != tool_search_name
                && runtime.exposure() == ToolExposure::Deferred
                && runtime.search_info().is_some()
        })
    {
        registry.remove(&tool_search_name);
        append_tool_search_executor(turn_context, &mut registry, tool_search_handler_cache);
    }

    let code_mode_tool_names = register_code_mode_executors(turn_context, &mut registry);

    let ModelVisibleSpecs {
        specs,
        flat_tool_names,
        omitted_tools,
    } = build_model_visible_specs(turn_context, &registry, &code_mode_tool_names, hosted_specs);
    ToolRouter::from_parts(registry, specs)
        .with_flat_tool_names(flat_tool_names.with_multi_agent_v2_namespace(
            turn_context.config.multi_agent_v2.tool_namespace.clone(),
        ))
        .with_omitted_tools(omitted_tools)
}

fn apply_direct_model_only_namespace_overrides(
    turn_context: &TurnContext,
    registry: &mut ToolRegistry,
) {
    for runtime in registry.runtimes_mut() {
        let configured = runtime
            .tool_name()
            .namespace
            .as_ref()
            .is_some_and(|namespace| {
                turn_context
                    .config
                    .code_mode
                    .direct_only_tool_namespaces
                    .contains(namespace)
            });
        if configured && runtime.exposure().is_available_in_code_mode() {
            *runtime = override_tool_exposure(Arc::clone(runtime), ToolExposure::DirectModelOnly);
        }
    }
}

/// What one request offers the model.
struct ModelVisibleSpecs {
    specs: Vec<ToolSpec>,
    flat_tool_names: FlatToolNames,
    /// Tools left out by the tool-count limit (#688: reported to the person
    /// once per session by the turn, not logged on every request).
    omitted_tools: Vec<ToolName>,
}

#[instrument(level = "trace", skip_all)]
fn build_model_visible_specs(
    turn_context: &TurnContext,
    registry: &ToolRegistry,
    code_mode_tool_names: &BTreeMap<String, ToolName>,
    hosted_specs: Vec<ToolSpec>,
) -> ModelVisibleSpecs {
    let namespace_tools = namespace_tools_enabled(turn_context);
    let mut specs = Vec::new();
    // Without namespace support, namespaced tools are offered under flat names
    // instead of being dropped with their namespace: MCP tools (#515, #227)
    // and every other grouped tool (#547). Built-in tools go first so MCP tools
    // cannot push them past the tool-count limit.
    let mut flattened_builtin_tools = Vec::new();
    let mut flattened_mcp_tools = Vec::new();
    for runtime in registry.runtimes() {
        let tool_name = runtime.tool_name();
        let exposure = runtime.exposure();
        if exposure.is_direct() && !is_hidden_by_code_mode_only(turn_context, &tool_name, exposure)
        {
            let spec = spec_for_model_request(
                turn_context,
                exposure,
                &tool_name,
                code_mode_tool_names,
                runtime.spec(),
            );
            match spec {
                ToolSpec::Namespace(namespace) if !namespace_tools => {
                    // V1 sub-agent tools have plain aliases here (`spawn_agent_v1`, ...).
                    if namespace.name == MULTI_AGENT_V1_NAMESPACE {
                        continue;
                    }
                    // Memory reads would send stored memories to providers that
                    // never saw them; that disclosure is a product decision, so
                    // they stay off these wires for now (#547).
                    if namespace.name == MEMORY_TOOLS_NAMESPACE {
                        continue;
                    }
                    let Some(tool) = flat_namespace_function(namespace, &tool_name) else {
                        warn!(
                            tool = %tool_name,
                            "namespaced tool has no single function; not offered without namespace tools"
                        );
                        continue;
                    };
                    // Key it by its registry name so the flat name routes back
                    // to exactly this runtime.
                    if runtime.tool_origin() == ToolOrigin::Mcp {
                        flattened_mcp_tools.push((tool_name, tool));
                    } else {
                        flattened_builtin_tools.push((tool_name, tool));
                    }
                }
                spec => specs.push(spec),
            }
        }
    }
    specs.extend(hosted_specs);

    let mut specs: Vec<ToolSpec> = merge_into_namespaces(specs)
        .into_iter()
        .filter(|spec| namespace_tools || !matches!(spec, ToolSpec::Namespace(_)))
        .collect();
    if flattened_builtin_tools.is_empty() && flattened_mcp_tools.is_empty() {
        return ModelVisibleSpecs {
            specs,
            flat_tool_names: FlatToolNames::default(),
            omitted_tools: Vec::new(),
        };
    }
    let mut budget = MAX_TOOLS_WITHOUT_NAMESPACES.saturating_sub(specs.len());
    let mut flattened = Vec::new();
    let mut omitted_tools = Vec::new();
    for mut tools in [flattened_builtin_tools, flattened_mcp_tools] {
        if tools.len() > budget {
            tools.sort_by(|(left, _), (right, _)| left.cmp(right));
            omitted_tools.extend(
                tools
                    .split_off(budget)
                    .into_iter()
                    .map(|(tool_name, _)| tool_name),
            );
        }
        budget -= tools.len();
        flattened.extend(tools);
    }
    omitted_tools.sort();
    // A flat name must not shadow any visible tool or any registered plain tool,
    // because the router resolves registered plain names first.
    let reserved = specs
        .iter()
        .map(|spec| spec.name().to_string())
        .chain(
            registry
                .runtimes()
                .map(|runtime| runtime.tool_name())
                .filter(|tool_name| tool_name.namespace.is_none())
                .map(|tool_name| tool_name.name),
        )
        .collect();
    let (tools, flat_tool_names) = flatten_namespaced_tools(flattened, &reserved);
    specs.extend(tools.into_iter().map(ToolSpec::Function));
    ModelVisibleSpecs {
        specs,
        flat_tool_names,
        omitted_tools,
    }
}

/// The function a namespaced runtime offers, for a wire without namespaces.
/// The namespace is not sent, so its description (an MCP server's, say) is
/// carried into the tool's own, bounded per tool because every tool of the
/// namespace repeats it.
fn flat_namespace_function(
    namespace: ResponsesApiNamespace,
    tool_name: &ToolName,
) -> Option<ResponsesApiTool> {
    let namespace_description = take_bytes_at_char_boundary(
        namespace.description.trim(),
        MAX_FLAT_NAMESPACE_DESCRIPTION_BYTES,
    )
    .to_string();
    let mut tools = namespace
        .tools
        .into_iter()
        .map(|tool| match tool {
            ResponsesApiNamespaceTool::Function(tool) => tool,
        })
        .collect::<Vec<_>>();
    // A runtime is one tool; with several, take the one it is named for.
    let index = if tools.len() == 1 {
        0
    } else {
        tools.iter().position(|tool| tool.name == tool_name.name)?
    };
    let mut tool = tools.swap_remove(index);
    if !namespace_description.is_empty() && !tool.description.contains(&namespace_description) {
        tool.description = if tool.description.trim().is_empty() {
            namespace_description
        } else {
            format!("{}\n\n{namespace_description}", tool.description)
        };
    }
    Some(tool)
}

fn spec_for_model_request(
    turn_context: &TurnContext,
    exposure: ToolExposure,
    tool_name: &ToolName,
    code_mode_tool_names: &BTreeMap<String, ToolName>,
    spec: ToolSpec,
) -> ToolSpec {
    let tool_mode = effective_tool_mode(turn_context);
    let spec = if matches!(tool_mode, ToolMode::CodeMode | ToolMode::CodeModeOnly)
        && exposure.is_available_in_code_mode()
        && !is_excluded_from_code_mode(turn_context, tool_name)
        && codex_code_mode::is_code_mode_nested_tool(spec.name())
        && code_mode_tool_names
            .get(&codex_code_mode::normalize_code_mode_identifier(
                &codex_tools::code_mode_name_for_tool_name(tool_name),
            ))
            .is_some_and(|winner| winner == tool_name)
    {
        codex_tools::augment_tool_spec_for_code_mode(spec)
    } else {
        spec
    };

    if multi_agent_v2_enabled(turn_context)
        && uses_openai_reserved_collaboration_schema(turn_context)
        && matches!(
            tool_name.name.as_str(),
            "spawn_agent"
                | "send_message"
                | "followup_task"
                | "wait_agent"
                | "interrupt_agent"
                | "list_agents"
        )
    {
        apply_openai_reserved_collaboration_schema(spec)
    } else {
        spec
    }
}

#[instrument(level = "trace", skip_all)]
fn hosted_model_tool_specs(
    turn_context: &TurnContext,
    registered_extension_tool_names: &[ToolName],
) -> Vec<ToolSpec> {
    // Responses Lite accepts schemas for client-executed tools, not hosted Responses tools.
    if turn_context.model_info.use_responses_lite
        || crate::guardian::is_guardian_reviewer_source(&turn_context.session_source)
    {
        return Vec::new();
    }

    let mut specs = Vec::new();
    let standalone_web_search_available = standalone_web_search_enabled(turn_context)
        && registered_extension_tool_names.contains(&ToolName::namespaced("web", "run"));
    // `Some(Cached/Live/Disabled)` are the options for mode when standalone search is unavailable
    // and the provider supports hosted search. `None` prevents emitting a hosted search tool.
    let web_search_mode = (!standalone_web_search_available
        && turn_context.provider.capabilities().web_search)
        .then_some(turn_context.config.web_search_mode.value());
    let web_search_config = web_search_mode
        .as_ref()
        .and(turn_context.config.web_search_config.as_ref());
    if let Some(hosted_web_search_tool) = create_web_search_tool(WebSearchToolOptions {
        web_search_mode,
        web_search_config,
        web_search_tool_type: turn_context.model_info.web_search_tool_type,
    }) {
        specs.push(hosted_web_search_tool);
    }
    specs
}

pub(crate) fn search_tool_enabled(turn_context: &TurnContext) -> bool {
    turn_context.model_info.supports_search_tool && namespace_tools_enabled(turn_context)
}

pub(crate) fn tool_suggest_enabled(turn_context: &TurnContext) -> bool {
    let features = turn_context.config.features.get();
    features.enabled(Feature::ToolSuggest)
        && features.enabled(Feature::Apps)
        && features.enabled(Feature::Plugins)
}

fn namespace_tools_enabled(turn_context: &TurnContext) -> bool {
    turn_context.provider.capabilities().namespace_tools
}

fn multi_agent_v2_enabled(turn_context: &TurnContext) -> bool {
    turn_context.multi_agent_version == MultiAgentVersion::V2
}

fn collab_tools_enabled(turn_context: &TurnContext) -> bool {
    match turn_context.multi_agent_version {
        MultiAgentVersion::Disabled => false,
        MultiAgentVersion::V1 => !exceeds_thread_spawn_depth_limit(
            next_thread_spawn_depth(&turn_context.session_source),
            turn_context.config.agent_max_depth,
        ),
        MultiAgentVersion::V2 => true,
    }
}

fn image_generation_available(turn_context: &TurnContext) -> bool {
    if !turn_context
        .config
        .features
        .get()
        .enabled(Feature::ImageGeneration)
    {
        return false;
    }

    if turn_context
        .auth_manager
        .as_deref()
        .and_then(AuthManager::auth_cached)
        .and_then(|auth| auth.account_plan_type())
        == Some(PlanType::Free)
    {
        return false;
    }

    let capabilities = turn_context.provider.capabilities();
    if !capabilities.image_generation || !capabilities.namespace_tools {
        return false;
    }

    if !turn_context
        .model_info
        .input_modalities
        .contains(&InputModality::Image)
    {
        return false;
    }

    let provider = turn_context.provider.info();
    let auth_manager = turn_context.auth_manager.as_deref();
    provider.uses_openai_actor_authorization()
        || (provider.requires_openai_auth
            && auth_manager.is_some_and(AuthManager::current_auth_uses_codex_backend))
        // An OpenAI API key on the built-in provider reaches OpenAI's public
        // Images API: `images/generations` under the same base URL and key as
        // inference, billed to that key.
        || (turn_context.config.model_provider_id == OPENAI_PROVIDER_ID
            && provider.is_openai()
            && auth_manager.and_then(AuthManager::get_api_auth_mode) == Some(AuthMode::ApiKey))
}

fn wait_agent_timeout_options(turn_context: &TurnContext) -> WaitAgentTimeoutOptions {
    if multi_agent_v2_enabled(turn_context) {
        return WaitAgentTimeoutOptions {
            default_timeout_ms: turn_context.config.multi_agent_v2.default_wait_timeout_ms,
            min_timeout_ms: turn_context.config.multi_agent_v2.min_wait_timeout_ms,
            max_timeout_ms: turn_context.config.multi_agent_v2.max_wait_timeout_ms,
        };
    }

    WaitAgentTimeoutOptions {
        default_timeout_ms: DEFAULT_WAIT_TIMEOUT_MS,
        min_timeout_ms: MIN_WAIT_TIMEOUT_MS,
        max_timeout_ms: MAX_WAIT_TIMEOUT_MS,
    }
}

fn agent_type_description(
    turn_context: &TurnContext,
    default_agent_type_description: &str,
) -> String {
    let agent_type_description =
        crate::agent::role::spawn_tool_spec::build(&turn_context.config.agent_roles);
    if agent_type_description.is_empty() {
        default_agent_type_description.to_string()
    } else {
        agent_type_description
    }
}

fn is_hidden_by_code_mode_only(
    turn_context: &TurnContext,
    tool_name: &ToolName,
    exposure: ToolExposure,
) -> bool {
    let tool_mode = effective_tool_mode(turn_context);
    tool_mode == ToolMode::CodeModeOnly
        && exposure.is_available_in_code_mode()
        && codex_code_mode::is_code_mode_nested_tool(&codex_tools::code_mode_name_for_tool_name(
            tool_name,
        ))
}

fn is_excluded_from_code_mode(turn_context: &TurnContext, tool_name: &ToolName) -> bool {
    tool_name.namespace.as_ref().is_some_and(|namespace| {
        turn_context
            .config
            .code_mode
            .excluded_tool_namespaces
            .contains(namespace)
    })
}

fn register_code_mode_executors(
    turn_context: &TurnContext,
    registry: &mut ToolRegistry,
) -> BTreeMap<String, ToolName> {
    let tool_mode = effective_tool_mode(turn_context);
    if !matches!(tool_mode, ToolMode::CodeMode | ToolMode::CodeModeOnly) {
        return BTreeMap::new();
    }

    let mut code_mode_tool_names = BTreeMap::new();
    let mut code_mode_nested_tool_specs = Vec::new();
    let mut exec_prompt_tool_specs = Vec::new();
    let mut deferred_exec_prompt_tool_specs = Vec::new();
    let deferred_tools_guidance_enabled = search_tool_enabled(turn_context);
    for executor in registry.runtimes() {
        let exposure = executor.exposure();
        if !exposure.is_available_in_code_mode() {
            continue;
        }

        let tool_name = executor.tool_name();
        if is_excluded_from_code_mode(turn_context, &tool_name) {
            continue;
        }

        let spec = executor.spec();
        let Some(code_mode_tool) = codex_tools::tool_spec_to_code_mode_tool_definition(&spec)
        else {
            continue;
        };

        let code_mode_name = code_mode_tool.name;
        match code_mode_tool_names.entry(codex_code_mode::normalize_code_mode_identifier(
            &code_mode_name,
        )) {
            Entry::Vacant(entry) => {
                entry.insert(tool_name);
            }
            Entry::Occupied(_) => {
                tracing::warn!(
                    tool_name = %tool_name,
                    code_mode_name = %code_mode_name,
                    "skipping tool with a duplicate normalized code-mode name"
                );
                continue;
            }
        }

        if exposure == ToolExposure::Deferred {
            if deferred_tools_guidance_enabled {
                deferred_exec_prompt_tool_specs.push(spec.clone());
            }
        } else {
            exec_prompt_tool_specs.push(spec.clone());
        }
        code_mode_nested_tool_specs.push(spec);
    }

    if turn_context.model_info.use_responses_lite {
        turn_context
            .turn_metadata_state
            .set_code_mode_tool_names(code_mode_tool_names.clone());
    }

    let namespace_descriptions = code_mode_namespace_descriptions(&exec_prompt_tool_specs);
    let mut enabled_tools =
        collect_code_mode_exec_prompt_tool_definitions(exec_prompt_tool_specs.iter());
    enabled_tools
        .sort_by(|left, right| compare_code_mode_tools(left, right, &namespace_descriptions));
    let deferred_tools =
        collect_code_mode_exec_prompt_tool_definitions(deferred_exec_prompt_tool_specs.iter());
    let default_exec_yield_time_ms =
        default_exec_yield_time_override_ms(&turn_context.config.features)
            .unwrap_or(codex_code_mode::DEFAULT_EXEC_YIELD_TIME_MS);

    let execute_handler = CodeModeExecuteHandler::new(
        create_code_mode_tool(
            &enabled_tools,
            &deferred_tools,
            &namespace_descriptions,
            default_exec_yield_time_ms,
            tool_mode == ToolMode::CodeModeOnly,
        ),
        code_mode_nested_tool_specs,
    );

    registry.prepend_trusted(Arc::new(CodeModeWaitHandler));
    registry.prepend_trusted(Arc::new(execute_handler));

    code_mode_tool_names
}

#[instrument(level = "trace", skip_all, fields(tool_spec_count = specs.len()))]
fn merge_into_namespaces(specs: Vec<ToolSpec>) -> Vec<ToolSpec> {
    let mut merged_specs = Vec::with_capacity(specs.len());
    let mut namespace_indices = BTreeMap::<String, usize>::new();
    for spec in specs {
        match spec {
            ToolSpec::Namespace(mut namespace) => {
                if let Some(index) = namespace_indices.get(&namespace.name).copied() {
                    let ToolSpec::Namespace(existing_namespace) = &mut merged_specs[index] else {
                        unreachable!("namespace index must point to a namespace spec");
                    };
                    if existing_namespace.description.trim().is_empty()
                        && !namespace.description.trim().is_empty()
                    {
                        existing_namespace.description = namespace.description;
                    }
                    existing_namespace.tools.append(&mut namespace.tools);
                    continue;
                }

                namespace_indices.insert(namespace.name.clone(), merged_specs.len());
                merged_specs.push(ToolSpec::Namespace(namespace));
            }
            spec => merged_specs.push(spec),
        }
    }

    for spec in &mut merged_specs {
        let ToolSpec::Namespace(namespace) = spec else {
            continue;
        };

        namespace.tools.sort_by(|left, right| match (left, right) {
            (
                ResponsesApiNamespaceTool::Function(left),
                ResponsesApiNamespaceTool::Function(right),
            ) => left.name.cmp(&right.name),
        });

        if namespace.description.trim().is_empty() {
            namespace.description = default_namespace_description(&namespace.name);
        }
    }

    merged_specs
}

fn code_mode_namespace_descriptions(
    specs: &[ToolSpec],
) -> BTreeMap<String, codex_code_mode::ToolNamespaceDescription> {
    let mut namespace_descriptions = BTreeMap::new();
    for spec in specs {
        let ToolSpec::Namespace(namespace) = spec else {
            continue;
        };

        let entry = namespace_descriptions
            .entry(namespace.name.clone())
            .or_insert_with(|| codex_code_mode::ToolNamespaceDescription {
                name: namespace.name.clone(),
                description: namespace.description.clone(),
            });
        if entry.description.trim().is_empty() && !namespace.description.trim().is_empty() {
            entry.description = namespace.description.clone();
        }
    }
    namespace_descriptions
}

#[instrument(level = "trace", skip_all)]
fn add_core_tool_sources(context: &CoreToolPlanContext<'_>, registry: &mut ToolRegistry) {
    // Guardian reviewers receive only `exec_command`, `write_stdin`, and `view_image`
    // when an environment is available; all general tool sources stay excluded.
    if crate::guardian::is_guardian_reviewer_source(&context.turn_context.session_source) {
        let turn_context = context.turn_context;
        let environment_mode = tool_environment_mode(context.environments);
        if environment_mode.has_environment() {
            let include_environment_id = matches!(environment_mode, ToolEnvironmentMode::Multiple);
            registry.add(ExecCommandHandler::new(ExecCommandHandlerOptions {
                allow_login_shell: turn_context.config.permissions.allow_login_shell,
                exec_permission_approvals_enabled: false,
                include_environment_id,
                include_shell_parameter: unified_exec_should_include_shell_parameter(
                    turn_context,
                    context.environments,
                ),
            }));
            registry.add(WriteStdinHandler);
            registry.add(ViewImageHandler::new(ViewImageToolOptions {
                can_request_original_image_detail: can_request_original_image_detail(
                    &turn_context.model_info,
                ),
                include_environment_id,
            }));
        }
        return;
    }

    add_shell_tools(context, registry);
    add_mcp_resource_tools(context, registry);
    add_core_utility_tools(context, registry);
    add_collaboration_tools(context, registry);
}

fn standalone_web_search_enabled(turn_context: &TurnContext) -> bool {
    namespace_tools_enabled(turn_context)
        && turn_context.provider.capabilities().web_search
        && (turn_context.model_info.use_responses_lite
            || turn_context
                .config
                .features
                .get()
                .enabled(Feature::StandaloneWebSearch))
}

fn tool_environment_mode(environments: &TurnEnvironmentSnapshot) -> ToolEnvironmentMode {
    ToolEnvironmentMode::from_count(environments.turn_environments().count())
}

#[instrument(level = "trace", skip_all)]
fn add_shell_tools(context: &CoreToolPlanContext<'_>, registry: &mut ToolRegistry) {
    let turn_context = context.turn_context;
    let features = turn_context.config.features.get();
    let environment_mode = tool_environment_mode(context.environments);
    if !environment_mode.has_environment() {
        return;
    }

    let allow_login_shell = turn_context.config.permissions.allow_login_shell;
    let exec_permission_approvals_enabled = features.enabled(Feature::ExecPermissionApprovals);
    let include_environment_id = matches!(environment_mode, ToolEnvironmentMode::Multiple);
    let shell_command_options = ShellCommandHandlerOptions {
        backend_config: shell_command_backend_for_features(features),
        allow_login_shell,
        exec_permission_approvals_enabled,
    };

    match shell_type_for_model_and_features(&turn_context.model_info, features) {
        ConfigShellToolType::UnifiedExec => {
            registry.add(ExecCommandHandler::new(ExecCommandHandlerOptions {
                allow_login_shell,
                exec_permission_approvals_enabled,
                include_environment_id,
                include_shell_parameter: unified_exec_should_include_shell_parameter(
                    turn_context,
                    context.environments,
                ),
            }));
            registry.add(WriteStdinHandler);

            // Keep the legacy shell tool registered while unified exec is
            // model-visible.
            registry.add_with_exposure(
                ShellCommandHandler::new(shell_command_options),
                ToolExposure::Hidden,
            );
        }
        ConfigShellToolType::Disabled => {}
        ConfigShellToolType::Default
        | ConfigShellToolType::Local
        | ConfigShellToolType::ShellCommand => {
            registry.add(ShellCommandHandler::new(shell_command_options));
        }
    }
}

fn unified_exec_should_include_shell_parameter(
    turn_context: &TurnContext,
    environments: &TurnEnvironmentSnapshot,
) -> bool {
    !matches!(
        &turn_context.unified_exec_shell_mode,
        UnifiedExecShellMode::ZshFork(_)
    ) || environments
        .turn_environments()
        .any(|environment| environment.environment.is_remote())
}

#[instrument(level = "trace", skip_all)]
fn add_mcp_resource_tools(context: &CoreToolPlanContext<'_>, registry: &mut ToolRegistry) {
    if context.mcp.has_servers() {
        registry.add(ListMcpResourcesHandler);
        registry.add(ListMcpResourceTemplatesHandler);
        registry.add(ReadMcpResourceHandler);
    }
}

#[instrument(level = "trace", skip_all)]
fn add_core_utility_tools(context: &CoreToolPlanContext<'_>, registry: &mut ToolRegistry) {
    let turn_context = context.turn_context;
    let features = turn_context.config.features.get();
    let environment_mode = tool_environment_mode(context.environments);

    if turn_context.config.update_plan_enabled {
        registry.add(PlanHandler);
    }

    if features.enabled(Feature::DeferredExecutor) {
        registry.add(
            context
                .wait_for_environment_tool_config
                .map(Arc::as_ref)
                .map_or_else(
                    WaitForEnvironmentHandler::default,
                    WaitForEnvironmentHandler::new,
                ),
        );
    }

    if turn_context.config.experimental_request_user_input_enabled {
        registry.add_with_exposure(
            RequestUserInputHandler {
                available_modes: request_user_input_available_modes(features),
            },
            ToolExposure::DirectModelOnly,
        );
    }

    if environment_mode.has_environment() && features.enabled(Feature::RequestPermissionsTool) {
        registry.add(RequestPermissionsHandler);
    }

    if features.enabled(Feature::TokenBudget) {
        registry.add_with_exposure(NewContextWindowHandler, ToolExposure::DirectModelOnly);
        registry.add(GetContextRemainingHandler);
    }

    if features.enabled(Feature::CurrentTimeReminder) {
        registry.add(CurrentTimeHandler);
    }
    if features.enabled(Feature::SleepTool)
        || turn_context
            .config
            .current_time_reminder
            .as_ref()
            .is_some_and(|config| config.sleep_tool)
    {
        registry.add(SleepHandler);
    }

    if tool_suggest_enabled(turn_context)
        && let Some(candidates) = context
            .tool_suggest_candidates
            .filter(|candidates| !candidates.tools.is_empty())
    {
        if candidates.presentation == crate::tools::router::ToolSuggestPresentation::ListTool {
            registry.add(ListAvailablePluginsToInstallHandler::new(
                collect_request_plugin_install_entries(&candidates.tools),
            ));
        }
        registry.add(RequestPluginInstallHandler::new(
            candidates.tools.clone(),
            candidates.presentation,
        ));
    }

    if environment_mode.has_environment() && edit_tools_allowed_by_permission_profile(turn_context)
    {
        let include_environment_id = matches!(environment_mode, ToolEnvironmentMode::Multiple);
        if native_structured_edit_protocol_enabled(turn_context) {
            registry.add(StructuredEditHandler::new(include_environment_id));
            registry.add(StructuredWriteHandler::new(include_environment_id));
        } else if turn_context.model_info.apply_patch_tool_type.is_some() {
            registry.add(ApplyPatchHandler::new(include_environment_id));
            registry.add(StructuredEditHandler::new(include_environment_id));
            registry.add(StructuredWriteHandler::new(include_environment_id));
        }
    }

    if turn_context
        .model_info
        .experimental_supported_tools
        .iter()
        .any(|tool| tool == "test_sync_tool")
    {
        registry.add(TestSyncHandler);
    }

    if environment_mode.has_environment() {
        let include_environment_id = matches!(environment_mode, ToolEnvironmentMode::Multiple);
        registry.add(ViewImageHandler::new(ViewImageToolOptions {
            can_request_original_image_detail: can_request_original_image_detail(
                &turn_context.model_info,
            ),
            include_environment_id,
        }));
    }
}

fn edit_tools_allowed_by_permission_profile(turn_context: &TurnContext) -> bool {
    let file_system_policy = turn_context.permission_profile.file_system_sandbox_policy();
    match file_system_policy.kind {
        FileSystemSandboxKind::Unrestricted | FileSystemSandboxKind::ExternalSandbox => true,
        FileSystemSandboxKind::Restricted => {
            #[allow(deprecated)]
            let cwd = turn_context.cwd.as_path();
            file_system_policy.can_write_path_with_cwd(cwd, cwd)
                || !file_system_policy
                    .get_writable_roots_with_cwd(cwd)
                    .is_empty()
        }
    }
}

#[instrument(level = "trace", skip_all)]
fn add_collaboration_tools(context: &CoreToolPlanContext<'_>, registry: &mut ToolRegistry) {
    let turn_context = context.turn_context;
    if collab_tools_enabled(turn_context) {
        if multi_agent_v2_enabled(turn_context) {
            let exposure = if turn_context.config.multi_agent_v2.non_code_mode_only {
                ToolExposure::DirectModelOnly
            } else {
                ToolExposure::Direct
            };
            let tool_namespace = namespace_tools_enabled(turn_context)
                .then_some(turn_context.config.multi_agent_v2.tool_namespace.as_deref())
                .flatten();
            let agent_type_description =
                agent_type_description(turn_context, context.default_agent_type_description);
            let hide_spawn_agent_metadata =
                turn_context.config.multi_agent_v2.hide_spawn_agent_metadata;
            let spawn_options = SpawnAgentToolOptions {
                available_models: spawn_agent_available_models(
                    turn_context,
                    /*supports_cross_provider_overrides*/ true,
                ),
                inherited_runtime: spawn_agent_inherited_runtime(turn_context),
                agent_type_description,
                // PF Terminal ships hierarchy roles (Nazgul, Troll, and Orc). They are
                // real runtime profiles, not optional user configuration, so the native
                // agent framework must always let a caller select them.
                expose_agent_type: true,
                hide_agent_type_model_reasoning: hide_spawn_agent_metadata,
                expose_spawn_agent_model_overrides: turn_context
                    .config
                    .multi_agent_v2
                    .expose_spawn_agent_model_overrides,
                usage_hint_text: turn_context.config.multi_agent_v2.usage_hint_text.clone(),
                expose_account: turn_context.config.features.enabled(Feature::NamedAccounts),
            };
            registry.register_trusted(override_tool_exposure(
                multi_agent_v2_handler(
                    SpawnAgentHandlerV2::new(spawn_options.clone()),
                    tool_namespace,
                ),
                exposure,
            ));
            registry.register_trusted(override_tool_exposure(
                multi_agent_v2_handler(SendMessageHandlerV2, tool_namespace),
                exposure,
            ));
            registry.register_trusted(override_tool_exposure(
                multi_agent_v2_handler(FollowupTaskHandlerV2, tool_namespace),
                exposure,
            ));
            if turn_context.config.multi_agent_v2.wait_agent_enabled {
                registry.register_trusted(override_tool_exposure(
                    multi_agent_v2_handler(
                        WaitAgentHandlerV2::new(context.wait_agent_timeouts),
                        tool_namespace,
                    ),
                    exposure,
                ));
            }
            registry.register_trusted(override_tool_exposure(
                multi_agent_v2_handler(InterruptAgentHandler, tool_namespace),
                exposure,
            ));
            registry.register_trusted(override_tool_exposure(
                multi_agent_v2_handler(ListAgentsHandlerV2, tool_namespace),
                exposure,
            ));
            if uses_openai_reserved_collaboration_schema(turn_context) {
                // OpenAI's reserved collaboration schema encrypts assignment fields. That is
                // correct for OpenAI recipients but opaque to external providers. These aliases
                // retry the same Core operation with an ordinary plaintext function argument;
                // the native handlers fail closed before admission when an adapter is required.
                registry.register_trusted(override_tool_exposure(
                    Arc::new(PlaintextSpawnAgentHandler::new(spawn_options)),
                    exposure,
                ));
                registry.register_trusted(override_tool_exposure(
                    Arc::new(PlaintextSendMessageHandler),
                    exposure,
                ));
                registry.register_trusted(override_tool_exposure(
                    Arc::new(PlaintextFollowupTaskHandler),
                    exposure,
                ));
            }
        } else {
            let agent_type_description =
                agent_type_description(turn_context, context.default_agent_type_description);
            let exposure = if search_tool_enabled(turn_context) {
                ToolExposure::Deferred
            } else {
                ToolExposure::Direct
            };
            let spawn_options = SpawnAgentToolOptions {
                available_models: spawn_agent_available_models(
                    turn_context,
                    /*supports_cross_provider_overrides*/ false,
                ),
                inherited_runtime: spawn_agent_inherited_runtime(turn_context),
                agent_type_description,
                // Built-in PF hierarchy roles remain selectable even when the operator has not
                // created any custom role files.
                expose_agent_type: true,
                hide_agent_type_model_reasoning: false,
                expose_spawn_agent_model_overrides: true,
                usage_hint_text: turn_context.config.multi_agent_v2.usage_hint_text.clone(),
                expose_account: turn_context.config.features.enabled(Feature::NamedAccounts),
            };
            registry.add_with_exposure(SpawnAgentHandler::new(spawn_options.clone()), exposure);
            registry.add_with_exposure(SendInputHandler, exposure);
            registry.add_with_exposure(ResumeAgentHandler, exposure);
            registry
                .add_with_exposure(WaitAgentHandler::new(context.wait_agent_timeouts), exposure);
            registry.add_with_exposure(CloseAgentHandler, exposure);
            if v1_plain_function_subagents_enabled(turn_context) {
                registry.register_trusted(multi_agent_v1_plain_function_handler(
                    SpawnAgentHandler::new(spawn_options),
                    "spawn_agent_v1",
                ));
                registry.register_trusted(multi_agent_v1_plain_function_handler(
                    SendInputHandler,
                    "send_input_v1",
                ));
                registry.register_trusted(multi_agent_v1_plain_function_handler(
                    WaitAgentHandler::new(context.wait_agent_timeouts),
                    "wait_agent_v1",
                ));
                registry.register_trusted(multi_agent_v1_plain_function_handler(
                    CloseAgentHandler,
                    "close_agent_v1",
                ));
                registry.register_trusted(multi_agent_v1_plain_function_handler(
                    ResumeAgentHandler,
                    "resume_agent_v1",
                ));
            }
        }
    }
}

#[instrument(level = "trace", skip_all, fields(dynamic_tool_count = dynamic_tools.len()))]
fn append_dynamic_tool_runtimes(dynamic_tools: &[DynamicToolSpec], registry: &mut ToolRegistry) {
    for spec in dynamic_tools {
        match spec {
            DynamicToolSpec::Function(tool) => {
                let Some(handler) = DynamicToolHandler::new(tool) else {
                    tracing::error!(
                        "Failed to convert dynamic tool {:?} to OpenAI tool",
                        tool.name
                    );
                    continue;
                };
                registry.register_external(Arc::new(handler));
            }
            DynamicToolSpec::Namespace(namespace) => {
                for tool in &namespace.tools {
                    let DynamicToolNamespaceTool::Function(tool) = tool;
                    let Some(handler) = DynamicToolHandler::new_in_namespace(namespace, tool)
                    else {
                        tracing::error!(
                            "Failed to convert dynamic tool {:?}.{:?} to OpenAI tool",
                            namespace.name,
                            tool.name
                        );
                        continue;
                    };
                    registry.register_external(Arc::new(handler));
                }
            }
        }
    }
}

#[instrument(level = "trace", skip_all)]
fn append_tool_search_executor(
    turn_context: &TurnContext,
    registry: &mut ToolRegistry,
    tool_search_handler_cache: &ToolSearchHandlerCache,
) {
    let search_infos = registry
        .runtimes()
        .filter(|executor| executor.exposure() == ToolExposure::Deferred)
        .filter_map(|executor| executor.search_info())
        .collect::<Vec<_>>();
    let source_listing = if turn_context
        .config
        .features
        .enabled(Feature::DeferredToolWorldState)
    {
        ToolSearchSourceListing::Omit
    } else {
        ToolSearchSourceListing::Include
    };
    registry.register_trusted(tool_search_handler_cache.get_or_build(search_infos, source_listing));
}

fn append_extension_tool_executors(
    turn_context: &TurnContext,
    executors: impl IntoIterator<Item = Arc<dyn ToolExecutor<ExtensionToolCall>>>,
    registry: &mut ToolRegistry,
) -> Option<ToolName> {
    let standalone_web_search_enabled = standalone_web_search_enabled(turn_context);
    let web_search_mode_on = turn_context.config.web_search_mode.value() != WebSearchMode::Disabled;
    let mut standalone_web_search_tool = None;

    for executor in executors {
        let tool_name = executor.tool_name();
        let is_standalone_web_search = tool_name == ToolName::namespaced("web", "run");
        if is_standalone_web_search && (!standalone_web_search_enabled || !web_search_mode_on) {
            continue;
        }
        if tool_name == ToolName::namespaced(IMAGE_GEN_NAMESPACE, IMAGEGEN_TOOL_NAME)
            && !image_generation_available(turn_context)
        {
            continue;
        }
        let runtime = Arc::new(ExtensionToolAdapter::new(executor));
        if registry.register_external(runtime) && is_standalone_web_search {
            standalone_web_search_tool = Some(tool_name);
        }
    }

    standalone_web_search_tool
}

fn multi_agent_v2_handler(
    handler: impl CoreToolRuntime + 'static,
    namespace: Option<&str>,
) -> Arc<dyn CoreToolRuntime> {
    match namespace {
        Some(namespace) => Arc::new(MultiAgentV2NamespaceOverride {
            handler: Arc::new(handler),
            namespace: namespace.to_string(),
        }),
        None => Arc::new(handler),
    }
}

/// V1 sub-agent tools are namespaced; every provider without namespace tools
/// (Chat Completions and Anthropic Messages wires) gets plain aliases (#547).
fn v1_plain_function_subagents_enabled(turn_context: &TurnContext) -> bool {
    !namespace_tools_enabled(turn_context)
}

fn spawn_agent_available_models(
    turn_context: &TurnContext,
    supports_cross_provider_overrides: bool,
) -> Vec<ModelPreset> {
    if !supports_cross_provider_overrides
        && third_party_provider_without_spawn_model_switching(turn_context)
    {
        return Vec::new();
    }
    let mut models = turn_context.available_models.clone();
    if let Some(api_key) = turn_context
        .auth_manager
        .as_ref()
        .and_then(|auth| auth.auth_mode().map(|mode| mode == AuthMode::ApiKey))
    {
        for model in &mut models {
            if let Some(metadata) = model.orchestration.as_mut() {
                metadata.resolve_billing_auth_mode(api_key);
            }
        }
    }
    // V1 has no provider argument. Advertising a model on another provider makes the generated
    // tool promise a route the handler cannot represent, so legacy sessions are restricted to
    // their current provider. V2 carries an explicit provider/model pair and may cross providers.
    if !supports_cross_provider_overrides {
        models.retain(|model| {
            model.provider_id.as_deref() == Some(turn_context.config.model_provider_id.as_str())
        });
    }
    if let Some(allowlist) = turn_context.config.agent_provider_allowlist.as_ref() {
        models.retain(|model| {
            model
                .provider_id
                .as_ref()
                .is_some_and(|provider| allowlist.contains(provider))
        });
    }
    // Explicit runtime discovery follows configured providers, not automatic-allocation
    // economics. Missing billing metadata does not make a requested model unavailable.
    models.retain(|model| {
        model
            .provider_id
            .as_ref()
            .is_some_and(|provider| turn_context.config.model_providers.contains_key(provider))
    });
    models
}

fn spawn_agent_inherited_runtime(turn_context: &TurnContext) -> Option<SpawnAgentRuntime> {
    let model_provider = turn_context.config.model_provider_id.clone();
    turn_context
        .config
        .agent_provider_allowlist
        .as_ref()
        .is_none_or(|allowlist| allowlist.contains(&model_provider))
        .then(|| SpawnAgentRuntime {
            model_provider,
            model: turn_context.model_info.slug.clone(),
            reasoning_effort: turn_context
                .reasoning_effort
                .clone()
                .or_else(|| turn_context.model_info.default_reasoning_level.clone()),
            service_tier: turn_context.config.service_tier.clone(),
        })
}

fn third_party_provider_without_spawn_model_switching(turn_context: &TurnContext) -> bool {
    let provider_info = turn_context.provider.info();
    provider_info.is_ambient()
        || provider_info.is_kimi_code()
        || provider_info.is_zai()
        || provider_info.is_openrouter()
        || provider_info.is_baseten()
        || provider_info.is_vercel()
}

fn multi_agent_v1_plain_function_handler(
    handler: impl CoreToolRuntime + 'static,
    plain_name: &'static str,
) -> Arc<dyn CoreToolRuntime> {
    Arc::new(MultiAgentV1PlainFunctionOverride {
        handler: Arc::new(handler),
        plain_name,
    })
}

struct MultiAgentV1PlainFunctionOverride {
    handler: Arc<dyn CoreToolRuntime>,
    plain_name: &'static str,
}

impl ToolExecutor<ToolInvocation> for MultiAgentV1PlainFunctionOverride {
    fn tool_name(&self) -> ToolName {
        ToolName::plain(self.plain_name)
    }

    fn spec(&self) -> ToolSpec {
        match self.handler.spec() {
            ToolSpec::Namespace(namespace) => {
                let mut tool = single_namespace_function(namespace, self.plain_name);
                tool.name = self.plain_name.to_string();
                tool.description = format!(
                    "{}\n\nCompatibility alias for V1 subagent support on providers that do not support namespace tools.",
                    tool.description
                );
                ToolSpec::Function(tool)
            }
            spec => spec,
        }
    }

    fn exposure(&self) -> ToolExposure {
        ToolExposure::Direct
    }

    fn supports_parallel_tool_calls(&self) -> bool {
        self.handler.supports_parallel_tool_calls()
    }

    fn search_info(&self) -> Option<ToolSearchInfo> {
        ToolSearchInfo::from_tool_spec(self.spec(), /*source_info*/ None)
    }

    fn handle(&self, invocation: ToolInvocation) -> codex_tools::ToolExecutorFuture<'_> {
        self.handler.handle(invocation)
    }
}

impl CoreToolRuntime for MultiAgentV1PlainFunctionOverride {
    fn repeated_identical_calls_are_polling(&self) -> bool {
        self.handler.repeated_identical_calls_are_polling()
    }

    fn matches_kind(&self, payload: &crate::tools::context::ToolPayload) -> bool {
        self.handler.matches_kind(payload)
    }

    fn create_diff_consumer(
        &self,
    ) -> Option<Box<dyn crate::tools::registry::ToolArgumentDiffConsumer>> {
        self.handler.create_diff_consumer()
    }
}

fn single_namespace_function(
    namespace: ResponsesApiNamespace,
    plain_name: &str,
) -> ResponsesApiTool {
    let mut tools = namespace.tools.into_iter();
    let Some(ResponsesApiNamespaceTool::Function(tool)) = tools.next() else {
        panic!(
            "expected V1 multi-agent namespace `{}` to contain one function",
            namespace.name
        );
    };
    if tools.next().is_some() {
        panic!(
            "expected V1 multi-agent namespace `{}` for `{plain_name}` to contain only one function",
            namespace.name
        );
    }
    tool
}

struct MultiAgentV2NamespaceOverride {
    handler: Arc<dyn CoreToolRuntime>,
    namespace: String,
}

impl ToolExecutor<ToolInvocation> for MultiAgentV2NamespaceOverride {
    fn tool_name(&self) -> ToolName {
        ToolName::namespaced(self.namespace.clone(), self.handler.tool_name().name)
    }

    fn spec(&self) -> ToolSpec {
        match self.handler.spec() {
            ToolSpec::Function(tool) => ToolSpec::Namespace(ResponsesApiNamespace {
                name: self.namespace.clone(),
                description: MULTI_AGENT_V2_NAMESPACE_DESCRIPTION.to_string(),
                tools: vec![ResponsesApiNamespaceTool::Function(tool)],
            }),
            spec => spec,
        }
    }

    fn exposure(&self) -> ToolExposure {
        self.handler.exposure()
    }

    fn supports_parallel_tool_calls(&self) -> bool {
        self.handler.supports_parallel_tool_calls()
    }

    fn search_info(&self) -> Option<ToolSearchInfo> {
        self.handler.search_info()
    }

    fn handle(&self, invocation: ToolInvocation) -> codex_tools::ToolExecutorFuture<'_> {
        self.handler.handle(invocation)
    }
}

impl CoreToolRuntime for MultiAgentV2NamespaceOverride {
    fn repeated_identical_calls_are_polling(&self) -> bool {
        self.handler.repeated_identical_calls_are_polling()
    }

    fn wait_until_ready<'a>(&'a self, session: &'a Arc<Session>) -> Option<BoxFuture<'a, ()>> {
        self.handler.wait_until_ready(session)
    }

    fn matches_kind(&self, payload: &crate::tools::context::ToolPayload) -> bool {
        self.handler.matches_kind(payload)
    }

    fn create_diff_consumer(
        &self,
    ) -> Option<Box<dyn crate::tools::registry::ToolArgumentDiffConsumer>> {
        self.handler.create_diff_consumer()
    }
}

fn compare_code_mode_tools(
    left: &codex_code_mode::ToolDefinition,
    right: &codex_code_mode::ToolDefinition,
    namespace_descriptions: &BTreeMap<String, codex_code_mode::ToolNamespaceDescription>,
) -> std::cmp::Ordering {
    let left_namespace = code_mode_namespace_name(left, namespace_descriptions);
    let right_namespace = code_mode_namespace_name(right, namespace_descriptions);

    left_namespace
        .cmp(&right_namespace)
        .then_with(|| left.tool_name.name.cmp(&right.tool_name.name))
        .then_with(|| left.name.cmp(&right.name))
}

fn code_mode_namespace_name<'a>(
    tool: &codex_code_mode::ToolDefinition,
    namespace_descriptions: &'a BTreeMap<String, codex_code_mode::ToolNamespaceDescription>,
) -> Option<&'a str> {
    tool.tool_name
        .namespace
        .as_ref()
        .and_then(|namespace| namespace_descriptions.get(namespace))
        .map(|namespace_description| namespace_description.name.as_str())
}

#[cfg(test)]
#[path = "spec_plan_tests.rs"]
mod tests;
