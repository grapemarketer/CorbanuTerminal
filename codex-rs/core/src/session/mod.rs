use std::borrow::Cow;
use std::collections::BTreeMap;
use std::collections::HashMap;
use std::collections::HashSet;
use std::fmt::Debug;
use std::path::Path;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::AtomicU64;
use std::time::Instant;
use std::time::SystemTime;
use std::time::UNIX_EPOCH;

use crate::agent::AgentControl;
use crate::agent::AgentStatus;
use crate::agent::agent_status_from_event;
use crate::agent::status::is_final;
use crate::agent_communication::AgentCommunicationContext;
use crate::agent_communication::AgentCommunicationKind;
use crate::attestation::AttestationProvider;
use crate::audio_preparation::prepare_response_items as prepare_audio_response_items;
use crate::build_available_skills;
use crate::compact;
use crate::compact::CompactedHistoryMetadata;
use crate::config::ManagedFeatures;
use crate::config::resolve_tool_suggest_config_from_layer_stack;
use crate::context::ApprovedCommandPrefixSaved;
use crate::context::AvailableSkillsInstructions;
use crate::context::ContextualUserFragment;
use crate::context::ModelSwitchInstructions;
use crate::context::NetworkRuleSaved;
use crate::context::RecommendedPluginsInstructions;
use crate::context::world_state::WorldState;
use crate::current_time::TimeProvider;
use crate::default_skill_metadata_budget;
use crate::environment_selection::TurnEnvironmentSnapshot;
use crate::exec_policy::BANNED_PREFIX_SUGGESTIONS;
use crate::exec_policy::ExecPolicyManager;
use crate::exec_policy::default_policy_path;
use crate::image_preparation::prepare_response_items as prepare_image_response_items;
use crate::parse_turn_item;
use crate::realtime_conversation::RealtimeConversationManager;
use crate::session::step_context::StepContext;
use crate::session::turn_context::TurnEnvironment;
use crate::session_prefix::format_inter_agent_completion_message;
use crate::skills::SkillRenderSideEffects;
use crate::skills_load_input_from_config;
use crate::turn_metadata::TurnMetadataState;
use crate::turn_timing::now_unix_timestamp_ms;
use async_channel::Receiver;
use async_channel::Sender;
use chrono::Local;
use chrono::Utc;
use codex_analytics::AnalyticsEventsClient;
use codex_analytics::SubAgentThreadStartedInput;
use codex_analytics::TurnCodexErrorFact;
use codex_async_utils::OrCancelExt;
use codex_connectors::connector_runtime_context_key;
use codex_core_skills::injection::HOST_SKILLS_CONTEXT_SOURCE_ID;
use codex_core_skills::injection::HostSkillsCatalogInWorldState;
use codex_exec_server::Environment;
use codex_exec_server::EnvironmentManager;
use codex_execpolicy::prefix_rule_migration;
use codex_extension_api::ExtensionDataInit;
use codex_extension_api::LoadedUserInstructions;
use codex_extension_api::PromptFragment;
use codex_extension_api::PromptSlot;
use codex_extension_api::TurnContextContributionInput;
use codex_features::FEATURES;
use codex_features::Feature;
use codex_features::unstable_features_warning_event;
use codex_hooks::Hooks;
use codex_hooks::HooksConfig;
use codex_login::AuthManager;
use codex_login::CodexAuth;
use codex_login::auth_env_telemetry::collect_auth_env_telemetry;
use codex_mcp::McpResourceClient;
use codex_mcp::McpRuntime;
use codex_mcp::McpRuntimeContext;
use codex_mcp::McpRuntimeInput;
use codex_models_manager::manager::RefreshStrategy;
use codex_models_manager::manager::SharedModelsManager;
use codex_network_proxy::NetworkProxy;
use codex_network_proxy::NetworkProxyAuditMetadata;
use codex_network_proxy::normalize_host;
use codex_otel::current_span_trace_id;
use codex_otel::current_span_w3c_trace_context;
use codex_otel::set_parent_from_w3c_trace_context;
use codex_protocol::SessionId;
use codex_protocol::ThreadId;
use codex_protocol::approvals::ElicitationRequest;
use codex_protocol::approvals::ElicitationRequestEvent;
use codex_protocol::approvals::ExecPolicyAmendment;
use codex_protocol::approvals::NetworkPolicyAmendment;
use codex_protocol::approvals::NetworkPolicyRuleAction;
use codex_protocol::config_types::ApprovalsReviewer;
use codex_protocol::config_types::AutoCompactTokenLimitScope;
use codex_protocol::config_types::ModeKind;
use codex_protocol::config_types::SERVICE_TIER_DEFAULT_REQUEST_VALUE;
use codex_protocol::config_types::Settings;
use codex_protocol::config_types::WebSearchMode;
use codex_protocol::dynamic_tools::DynamicToolResponse;
use codex_protocol::dynamic_tools::DynamicToolSpec;
use codex_protocol::items::EnteredReviewModeItem;
use codex_protocol::items::TurnItem;
use codex_protocol::items::UserMessageItem;
use codex_protocol::models::ActivePermissionProfile;
use codex_protocol::models::AdditionalPermissionProfile;
use codex_protocol::models::BaseInstructions;
use codex_protocol::models::PermissionProfile;
use codex_protocol::models::SandboxEnforcement;
use codex_protocol::models::format_allow_prefixes;
use codex_protocol::openai_models::ModelBilling;
use codex_protocol::openai_models::ModelCapabilityTier;
use codex_protocol::openai_models::ModelInfo;
use codex_protocol::openai_models::ModelOrchestrationMetadata;
use codex_protocol::openai_models::ModelPreset;
use codex_protocol::permissions::FileSystemSandboxPolicy;
use codex_protocol::permissions::NetworkSandboxPolicy;
use codex_protocol::protocol::AdditionalContextEntry;
use codex_protocol::protocol::FileChange;
use codex_protocol::protocol::HasLegacyEvent;
use codex_protocol::protocol::HistoryPosition;
use codex_protocol::protocol::InterAgentCommunication;
use codex_protocol::protocol::ItemCompletedEvent;
use codex_protocol::protocol::ItemStartedEvent;
use codex_protocol::protocol::MULTI_AGENT_MODE_OPEN_TAG;
use codex_protocol::protocol::MultiAgentVersion;
use codex_protocol::protocol::RawResponseItemEvent;
use codex_protocol::protocol::RolloutItem;
use codex_protocol::protocol::SessionSource;
use codex_protocol::protocol::SubAgentSource;
use codex_protocol::protocol::ThreadHistoryMode;
use codex_protocol::protocol::ThreadSource;
use codex_protocol::protocol::TurnAbortReason;
use codex_protocol::protocol::TurnContextItem;
use codex_protocol::protocol::TurnContextNetworkItem;
use codex_protocol::protocol::TurnEnvironmentSelection;
use codex_protocol::protocol::TurnEnvironmentSelections;
use codex_protocol::protocol::W3cTraceContext;
use codex_protocol::protocol::WorldStateItem;
use codex_protocol::request_permissions::PermissionGrantScope;
use codex_protocol::request_permissions::RequestPermissionProfile;
use codex_protocol::request_permissions::RequestPermissionsArgs;
use codex_protocol::request_permissions::RequestPermissionsEvent;
use codex_protocol::request_permissions::RequestPermissionsResponse;
use codex_protocol::request_user_input::RequestUserInputArgs;
use codex_protocol::request_user_input::RequestUserInputResponse;
use codex_rmcp_client::ElicitationResponse;
use codex_rollout::state_db;
use codex_rollout_trace::AgentResultTracePayload;
use codex_rollout_trace::ThreadStartedTraceMetadata;
use codex_rollout_trace::ThreadTraceContext;
use codex_sandboxing::policy_transforms::intersect_permission_profiles;
use codex_shell_command::parse_command::parse_command;
use codex_terminal_detection::user_agent;
use codex_thread_store::CreateThreadParams;
use codex_thread_store::LiveThread;
use codex_thread_store::LiveThreadInitGuard;
use codex_thread_store::LocalThreadStore;
use codex_thread_store::ReadThreadParams;
use codex_thread_store::ResumeThreadParams;
use codex_thread_store::ThreadPersistenceMetadata;
use codex_thread_store::ThreadStore;
use codex_utils_path_uri::PathUri;
use futures::future::BoxFuture;
use futures::future::Shared;
use futures::prelude::*;
use rmcp::model::RequestId;
use serde_json::Value;
use tokio::sync::Mutex;
use tokio::sync::RwLock;
use tokio::sync::oneshot;
use tokio::sync::watch;
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;
use toml::Value as TomlValue;
use tracing::Instrument;
use tracing::debug;
use tracing::error;
use tracing::info;
use tracing::info_span;
use tracing::instrument;
use tracing::warn;
use uuid::Uuid;

use crate::client::ModelClient;
use crate::codex_thread::ThreadConfigSnapshot;
#[cfg(test)]
use crate::compact::collect_user_messages;
use crate::config::Config;
use crate::config::Constrained;
use crate::config::ConstraintResult;
use crate::config::PermissionProfileSnapshot;
use crate::config::PermissionProfileState;
use crate::config::StartedNetworkProxy;
use crate::config::resolve_web_search_mode_for_turn;
use crate::context_manager::ContextManager;
use crate::context_manager::updates::ContextSection;
use crate::thread_rollout_truncation::initial_history_has_prior_user_turns;
use codex_config::CONFIG_TOML_FILE;
use codex_config::ConfigLayerSource;
use codex_config::ConfigLayerStackOrdering;
use codex_config::types::McpServerConfig;
use codex_model_provider_info::ModelProviderInfo;
use codex_protocol::error::CodexErr;
use codex_protocol::error::CodexErrorDetails;
use codex_protocol::error::Result as CodexResult;
#[cfg(test)]
use codex_protocol::exec_output::StreamOutput;

mod code_mode_warning;
mod config_lock;
mod confirmed_aggressive;
pub(crate) mod context_window;
mod extension_metrics;
mod handlers;
mod inject;
mod input_queue;
mod mcp;
mod mcp_prewarm;
mod mcp_refresh;
mod mcp_runtime;
pub(crate) mod memory_stage_one;
pub(crate) mod multi_agents;
mod output_text_stream;
mod review;
mod rollout_budget;
mod rollout_reconstruction;
#[allow(clippy::module_inception)]
pub(crate) mod session;
pub(crate) mod step_context;
pub(crate) mod time_reminder;
mod token_budget;
pub(crate) mod turn;
mod turn_completion;
pub(crate) mod turn_context;
mod world_state;
use self::code_mode_warning::unsupported_code_mode_warning;
use self::config_lock::export_config_lock_if_configured;
use self::config_lock::validate_config_lock_if_configured;
#[cfg(test)]
use self::handlers::submission_dispatch_span;
use self::handlers::submission_loop;
pub(crate) use self::input_queue::InputQueueActivity;
pub(crate) use self::input_queue::TurnInput;
pub(crate) use self::input_queue::TurnInputQueue;
use self::review::spawn_review_thread;
use self::session::AppServerClientMetadata;
use self::session::Session;
use self::session::SessionConfiguration;
pub(crate) use self::session::SessionSettingsUpdate;
#[cfg(test)]
use self::turn::AssistantMessageStreamParsers;
use self::turn::agent_message_text;
#[cfg(test)]
use self::turn::collect_explicit_app_ids_from_skill_items;
use self::turn::realtime_text_for_event;
use self::turn_context::TurnContext;
use self::turn_context::TurnSkillsContext;
#[cfg(test)]
mod rollout_reconstruction_tests;

#[derive(Debug, PartialEq)]
pub enum SteerInputError {
    NoActiveTurn(Vec<UserInput>),
    ExpectedTurnMismatch { expected: String, actual: String },
    ActiveTurnNotSteerable { turn_kind: NonSteerableTurnKind },
    AuthorizationChanged(Vec<UserInput>),
    EmptyInput,
}

impl SteerInputError {
    fn to_error_event(&self) -> ErrorEvent {
        match self {
            Self::NoActiveTurn(_) => ErrorEvent {
                message: "no active turn to steer".to_string(),
                codex_error_info: Some(CodexErrorInfo::BadRequest),
            },
            Self::ExpectedTurnMismatch { expected, actual } => ErrorEvent {
                message: format!("expected active turn id `{expected}` but found `{actual}`"),
                codex_error_info: Some(CodexErrorInfo::BadRequest),
            },
            Self::ActiveTurnNotSteerable { turn_kind } => {
                let turn_kind_label = match turn_kind {
                    NonSteerableTurnKind::Review => "review",
                    NonSteerableTurnKind::Compact => "compact",
                };
                ErrorEvent {
                    message: format!("cannot steer a {turn_kind_label} turn"),
                    codex_error_info: Some(CodexErrorInfo::ActiveTurnNotSteerable {
                        turn_kind: *turn_kind,
                    }),
                }
            }
            Self::AuthorizationChanged(_) => ErrorEvent {
                message: "Permissions changed since this turn started. Wait for it to finish or stop it, then submit your message again.".to_string(),
                codex_error_info: Some(CodexErrorInfo::BadRequest),
            },
            Self::EmptyInput => ErrorEvent {
                message: "input must not be empty".to_string(),
                codex_error_info: Some(CodexErrorInfo::BadRequest),
            },
        }
    }
}

/// PF-84: how long a switch to an account without a fresh cached model list
/// waits for that list (API-key sign-ins need no fetch and never wait).
const FIRST_MODEL_LIST_WAIT: std::time::Duration = std::time::Duration::from_secs(5);

/// Sends [`EventMsg::ExecApprovalRequestWithdrawn`] when a command approval
/// is dropped before it was answered (#722, D279).
struct WithdrawApprovalOnDrop {
    tx_event: Sender<Event>,
    event: Option<ExecApprovalRequestWithdrawnEvent>,
}

impl WithdrawApprovalOnDrop {
    fn disarm(mut self) {
        self.event = None;
    }
}

impl Drop for WithdrawApprovalOnDrop {
    fn drop(&mut self) {
        if let Some(event) = self.event.take() {
            // Unbounded channel: never blocks. A closed channel has no client.
            let _ = self.tx_event.try_send(Event {
                id: event.turn_id.clone(),
                msg: EventMsg::ExecApprovalRequestWithdrawn(event),
            });
        }
    }
}

/// Notes from the previous real user turn.
///
/// Conceptually this is the same role that `previous_model` used to fill, but
/// it can carry other prior-turn settings that matter when constructing
/// sensible state-change diffs or full-context reinjection, such as model
/// switches, compaction compatibility, or detecting a prior
/// `realtime_active -> false` transition.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct PreviousTurnSettings {
    pub(crate) model: String,
    pub(crate) model_provider: Option<String>,
    pub(crate) comp_hash: Option<String>,
    pub(crate) realtime_active: Option<bool>,
}

#[cfg(test)]
use crate::SkillMetadata;
use crate::SkillsService;
use crate::exec_policy::ExecPolicyUpdateError;
use crate::guardian::GuardianReviewOptions;
use crate::guardian::GuardianReviewSessionManager;
use crate::mcp::McpManager;
use crate::network_policy_decision::execpolicy_network_rule_amendment;
use crate::rollout::map_session_init_error;
use crate::session_startup_prewarm::SessionStartupPrewarmHandle;
use crate::shell;
#[cfg(test)]
use crate::skills::SkillLoadOutcome;
use crate::state::AutoCompactWindowIds;
use crate::state::AutoCompactWindowSnapshot;
use crate::state::PendingRequestPermissions;
use crate::state::SessionServices;
use crate::state::SessionState;
#[cfg(test)]
use crate::stream_events_utils::HandleOutputCtx;
#[cfg(test)]
use crate::stream_events_utils::handle_output_item_done;
use crate::tasks::ReviewTask;
use crate::tools::network_approval::NetworkApprovalService;
use crate::tools::network_approval::build_blocked_request_observer;
use crate::tools::network_approval::build_network_policy_decider;
#[cfg(test)]
use crate::tools::parallel::ToolCallRuntime;
use crate::tools::sandboxing::ApprovalStore;
use crate::turn_timing::TurnTimingState;
use crate::turn_timing::record_turn_ttfm_metric;
use crate::unified_exec::UnifiedExecProcessManager;
use crate::windows_sandbox::WindowsSandboxLevelExt;
use codex_core_plugins::PluginCommandAttribution;
use codex_core_plugins::PluginsManager;
use codex_core_plugins::RecommendedPluginCandidatesInput;
use codex_git_utils::get_git_repo_root;
use codex_mcp::McpConfig;
use codex_mcp::effective_mcp_servers;
use codex_otel::SessionTelemetry;
use codex_otel::THREAD_STARTED_METRIC;
use codex_otel::TelemetryAuthMode;
use codex_protocol::ResponseItemId;
use codex_protocol::config_types::CollaborationMode;
use codex_protocol::config_types::Personality;
use codex_protocol::config_types::ReasoningSummary as ReasoningSummaryConfig;
use codex_protocol::config_types::WindowsSandboxLevel;
use codex_protocol::models::LocalImagePreparation;
use codex_protocol::models::ResponseInputItem;
use codex_protocol::models::ResponseItem;
use codex_protocol::openai_models::ReasoningEffort as ReasoningEffortConfig;
use codex_protocol::protocol::AccountTokenUsage;
use codex_protocol::protocol::ApplyPatchApprovalRequestEvent;
use codex_protocol::protocol::AskForApproval;
use codex_protocol::protocol::CodexErrorInfo;
use codex_protocol::protocol::CompactedItem;
use codex_protocol::protocol::DeprecationNoticeEvent;
use codex_protocol::protocol::ErrorEvent;
use codex_protocol::protocol::Event;
use codex_protocol::protocol::EventMsg;
use codex_protocol::protocol::ExecApprovalRequestEvent;
use codex_protocol::protocol::ExecApprovalRequestWithdrawnEvent;
use codex_protocol::protocol::InitialHistory;
use codex_protocol::protocol::ModelRerouteEvent;
use codex_protocol::protocol::ModelRerouteReason;
use codex_protocol::protocol::ModelResponseCompletedEvent;
use codex_protocol::protocol::ModelVerification;
use codex_protocol::protocol::ModelVerificationEvent;
use codex_protocol::protocol::NetworkApprovalContext;
use codex_protocol::protocol::NonSteerableTurnKind;
use codex_protocol::protocol::Op;
use codex_protocol::protocol::RateLimitSnapshot;
use codex_protocol::protocol::RequestUserInputEvent;
use codex_protocol::protocol::ReviewDecision;
use codex_protocol::protocol::SandboxPolicy;
use codex_protocol::protocol::SessionConfiguredEvent;
use codex_protocol::protocol::SessionNetworkProxyRuntime;
use codex_protocol::protocol::StreamErrorEvent;
use codex_protocol::protocol::Submission;
use codex_protocol::protocol::ThreadMemoryMode;
use codex_protocol::protocol::TokenCountEvent;
use codex_protocol::protocol::TokenUsage;
use codex_protocol::protocol::TokenUsageInfo;
use codex_protocol::protocol::TurnModerationMetadataEvent;
use codex_protocol::protocol::WarningEvent;
use codex_protocol::user_input::UserInput;
use codex_tools::UnifiedExecShellMode;
use codex_utils_absolute_path::AbsolutePathBuf;
#[cfg(test)]
use codex_utils_stream_parser::ProposedPlanSegment;

/// Queue and lifecycle endpoints for a running [`Session`].
///
/// Runtime state lives on `Session`; keeping these endpoints separate lets all
/// submission senders be dropped to terminate the session loop. The shared
/// completion future observes that shutdown.
pub(crate) struct SessionIo {
    pub(crate) tx_sub: Sender<Submission>,
    pub(crate) rx_event: Receiver<Event>,
    // Last known status of the agent.
    pub(crate) agent_status: watch::Receiver<AgentStatus>,
    // Shared future for the background submission loop completion so multiple
    // callers can wait for shutdown.
    pub(crate) session_loop_termination: SessionLoopTermination,
}

pub(crate) type SessionLoopTermination = Shared<BoxFuture<'static, ()>>;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum GitEnrichmentPolicy {
    Fresh,
    Skip,
}

/// Controls which fork history belongs in the newly created thread's own rollout.
pub(crate) enum ForkPersistence {
    Copied,
    Referenced {
        history_base: Option<HistoryPosition>,
        inherited_item_count: usize,
    },
}

pub(crate) struct SessionSpawnArgs {
    pub(crate) config: Config,
    pub(crate) allow_provider_model_fallback: bool,
    pub(crate) user_instructions: LoadedUserInstructions,
    pub(crate) installation_id: String,
    pub(crate) auth_manager: Arc<AuthManager>,
    pub(crate) models_manager: SharedModelsManager,
    pub(crate) environment_manager: Arc<EnvironmentManager>,
    pub(crate) skills_service: Arc<SkillsService>,
    pub(crate) plugins_manager: Arc<PluginsManager>,
    pub(crate) mcp_manager: Arc<McpManager>,
    pub(crate) code_mode_session_provider: Arc<dyn codex_code_mode::CodeModeSessionProvider>,
    pub(crate) extensions: Arc<codex_extension_api::ExtensionRegistry<crate::config::Config>>,
    pub(crate) conversation_history: InitialHistory,
    pub(crate) requested_history_mode: Option<ThreadHistoryMode>,
    pub(crate) fork_persistence: ForkPersistence,
    pub(crate) session_source: SessionSource,
    pub(crate) forked_from_thread_id: Option<ThreadId>,
    pub(crate) parent_thread_id: Option<ThreadId>,
    pub(crate) thread_source: Option<ThreadSource>,
    pub(crate) originator: String,
    pub(crate) agent_control: AgentControl,
    pub(crate) dynamic_tools: Vec<DynamicToolSpec>,
    pub(crate) metrics_service_name: Option<String>,
    pub(crate) inherited_exec_policy: Option<Arc<ExecPolicyManager>>,
    pub(crate) inherited_environments: Option<TurnEnvironmentSnapshot>,
    /// Parent rollout trace used only to derive fresh spawned child traces.
    ///
    /// Root sessions and non-thread-spawn subagents pass a disabled context;
    /// `Session::new` creates the root trace itself when rollout tracing is enabled.
    pub(crate) parent_rollout_thread_trace: ThreadTraceContext,
    pub(crate) user_shell_override: Option<shell::Shell>,
    pub(crate) parent_trace: Option<W3cTraceContext>,
    pub(crate) environment_selections: Vec<TurnEnvironmentSelection>,
    pub(crate) thread_extension_init: ExtensionDataInit,
    pub(crate) supports_openai_form_elicitation: bool,
    pub(crate) analytics_events_client: Option<AnalyticsEventsClient>,
    pub(crate) thread_store: Arc<dyn ThreadStore>,
    pub(crate) attestation_provider: Option<Arc<dyn AttestationProvider>>,
    pub(crate) external_time_provider: Option<Arc<dyn TimeProvider>>,
    pub(crate) inherited_multi_agent_version: Option<MultiAgentVersion>,
    pub(crate) git_enrichment_policy: GitEnrichmentPolicy,
    pub(crate) windows_sandbox_proxy_settings_mode:
        codex_sandboxing::WindowsSandboxProxySettingsMode,
}

pub(crate) fn resolve_multi_agent_version(
    conversation_history: &InitialHistory,
    inherited_multi_agent_version: Option<MultiAgentVersion>,
) -> Option<MultiAgentVersion> {
    if inherited_multi_agent_version == Some(MultiAgentVersion::Disabled) {
        return Some(MultiAgentVersion::Disabled);
    }

    conversation_history
        .get_multi_agent_version()
        .or(inherited_multi_agent_version)
        .or(match conversation_history {
            InitialHistory::New | InitialHistory::Cleared => None,
            // Threads created before runtime metadata existed keep the legacy V1 tool surface.
            InitialHistory::Resumed(_) | InitialHistory::Forked(_) => Some(MultiAgentVersion::V1),
        })
}

pub(crate) const INITIAL_SUBMIT_ID: &str = "";
pub(crate) const SUBMISSION_CHANNEL_CAPACITY: usize = 512;
const CYBER_VERIFY_URL: &str = "https://chatgpt.com/cyber";
const CYBER_SAFETY_URL: &str = "https://developers.openai.com/codex/concepts/cyber-safety";

impl Session {
    /// Spawn and initialize a new session.
    pub(crate) async fn spawn(args: SessionSpawnArgs) -> CodexResult<(Arc<Self>, SessionIo)> {
        let parent_trace = match args.parent_trace {
            Some(trace) => {
                if codex_otel::context_from_w3c_trace_context(&trace).is_some() {
                    Some(trace)
                } else {
                    warn!("ignoring invalid thread spawn trace carrier");
                    None
                }
            }
            None => None,
        };
        let thread_spawn_span = info_span!("thread_spawn", otel.name = "thread_spawn");
        if let Some(trace) = parent_trace.as_ref() {
            let _ = set_parent_from_w3c_trace_context(&thread_spawn_span, trace);
        }
        Self::spawn_internal(SessionSpawnArgs {
            parent_trace,
            ..args
        })
        .instrument(thread_spawn_span)
        .await
    }

    async fn spawn_internal(args: SessionSpawnArgs) -> CodexResult<(Arc<Self>, SessionIo)> {
        let SessionSpawnArgs {
            mut config,
            allow_provider_model_fallback,
            user_instructions,
            installation_id,
            auth_manager,
            models_manager,
            environment_manager,
            skills_service,
            plugins_manager,
            mcp_manager,
            code_mode_session_provider,
            extensions,
            conversation_history,
            requested_history_mode,
            fork_persistence,
            session_source,
            forked_from_thread_id,
            parent_thread_id,
            thread_source,
            originator,
            agent_control,
            dynamic_tools,
            metrics_service_name,
            user_shell_override,
            inherited_exec_policy,
            inherited_environments,
            parent_rollout_thread_trace,
            parent_trace: _,
            environment_selections,
            thread_extension_init,
            supports_openai_form_elicitation,
            analytics_events_client,
            thread_store,
            attestation_provider,
            external_time_provider,
            inherited_multi_agent_version,
            git_enrichment_policy,
            windows_sandbox_proxy_settings_mode,
        } = args;
        let (tx_sub, rx_sub) = async_channel::bounded(SUBMISSION_CHANNEL_CAPACITY);
        let (tx_event, rx_event) = async_channel::unbounded();

        let LoadedUserInstructions {
            instructions: user_instructions,
            warnings: user_instruction_provider_warnings,
        } = user_instructions;
        // TODO(anp) pull startup_warnings out of Config
        config
            .startup_warnings
            .extend(user_instruction_provider_warnings);
        let exec_policy = if crate::guardian::is_guardian_reviewer_source(&session_source) {
            // Guardian review should rely on the built-in shell safety checks,
            // not on caller-provided exec-policy rules that could shape the
            // reviewer or silently auto-approve commands.
            Arc::new(ExecPolicyManager::default())
        } else if let Some(exec_policy) = &inherited_exec_policy {
            Arc::clone(exec_policy)
        } else {
            if !config
                .config_layer_stack
                .ignore_user_and_project_exec_policy_rules()
            {
                let codex_home = config.codex_home.clone();
                let policy_path = default_policy_path(codex_home.as_path());
                if let Err(err) = prefix_rule_migration(
                    codex_home.as_path(),
                    policy_path.as_path(),
                    BANNED_PREFIX_SUGGESTIONS,
                )
                .await
                {
                    tracing::warn!(error = %err, "failed to run prefix rule migration");
                }
            }
            Arc::new(
                ExecPolicyManager::load(&config.config_layer_stack, config.strict_rules)
                    .await
                    .map_err(|err| match err {
                        err @ crate::exec_policy::ExecPolicyError::ParsePolicy { .. }
                            if config.strict_rules =>
                        {
                            CodexErr::Fatal(format!(
                                "failed to load rules: {}. A rules file that does not parse stops new threads while strict_rules is on (security level Aggressive); fix or remove it, then start a new thread.",
                                crate::exec_policy::format_exec_policy_error_with_source(&err)
                            ))
                        }
                        err => CodexErr::Fatal(format!("failed to load rules: {err}")),
                    })?,
            )
        };

        let mut config = Arc::new(config);
        // PF-84: a user's session lists models with its own account's sign-in.
        if !session_source.is_non_root_agent() {
            let account = codex_model_provider::named_sign_in_account(&config.model_provider);
            models_manager.follow_sign_in_account(account.as_ref());
        }
        let refresh_strategy = if session_source.is_non_root_agent() {
            codex_models_manager::manager::RefreshStrategy::Offline
        } else {
            codex_models_manager::manager::RefreshStrategy::OnlineIfUncached
        };
        if config.model.is_none()
            || !matches!(
                refresh_strategy,
                codex_models_manager::manager::RefreshStrategy::Offline
            )
        {
            let _ = models_manager
                .list_models(refresh_strategy, config.http_client_factory())
                .await;
        }
        let model = models_manager
            .get_default_model(
                &config.model,
                allow_provider_model_fallback,
                refresh_strategy,
                config.http_client_factory(),
            )
            .await;
        if allow_provider_model_fallback
            && let Some(requested_model) = config.model.as_ref()
            && model != *requested_model
        {
            info!(
                model_provider = %config.model_provider_id,
                requested_model,
                fallback_model = %model,
                "replaced unavailable requested model with provider default"
            );
        }

        // Resolve base instructions for the session. Priority order:
        // 1. config.base_instructions override
        // 2. conversation history => session_meta.base_instructions
        // 3. base_instructions for current model
        let model_info = models_manager
            .get_model_info(
                model.as_str(),
                &config.to_models_manager_config_for_model(Some(model.as_str())),
            )
            .await;
        if config.config_lock_export_dir.is_some()
            && config.config_lock_save_fields_resolved_from_model_catalog
        {
            self::token_budget::apply_model_defaults(Arc::make_mut(&mut config), &model_info);
        }
        let multi_agent_version = config.multi_agent_version_override().or_else(|| {
            resolve_multi_agent_version(&conversation_history, inherited_multi_agent_version)
        });
        let history_mode = conversation_history.get_history_mode(
            requested_history_mode.unwrap_or_else(|| thread_store.default_history_mode()),
        );
        let base_instructions = config
            .base_instructions
            .clone()
            .or_else(|| conversation_history.get_base_instructions().map(|s| s.text))
            .unwrap_or_else(|| model_info.get_model_instructions(config.personality));

        // Dynamic tools are defined at thread start and persisted in rollout session metadata.
        let dynamic_tools = if dynamic_tools.is_empty() {
            conversation_history.get_dynamic_tools().unwrap_or_default()
        } else {
            dynamic_tools
        };
        // TODO (aibrahim): Consolidate config.model and config.model_reasoning_effort into config.collaboration_mode
        // to avoid extracting these fields separately and constructing CollaborationMode here.
        let collaboration_mode = CollaborationMode {
            mode: ModeKind::Default,
            settings: Settings {
                model: model.clone(),
                reasoning_effort: config.model_reasoning_effort.clone(),
                developer_instructions: None,
            },
        };
        let fast_mode_enabled = config.features.enabled(Feature::FastMode);
        let initial_service_tier_warning = unsupported_service_tier_warning(
            config.service_tier.as_deref(),
            fast_mode_enabled,
            &model_info,
        );
        let service_tier =
            get_service_tier(config.service_tier.clone(), fast_mode_enabled, &model_info);
        let session_configuration = SessionConfiguration {
            provider: config.model_provider.clone(),
            collaboration_mode,
            model_reasoning_summary: config.model_reasoning_summary,
            service_tier,
            runtime_model_context_window: None,
            developer_instructions: config.developer_instructions.clone(),
            personality: config.personality,
            base_instructions,
            compact_prompt: config.compact_prompt.clone(),
            approval_policy: config.permissions.approval_policy.clone(),
            approvals_reviewer: config.approvals_reviewer,
            aggressive_approvals: false,
            permission_profile_state: session_permission_profile_state_from_config(&config)?,
            windows_sandbox_level: WindowsSandboxLevel::from_config(&config),
            environments: TurnEnvironmentSelections::new(
                config.cwd.clone(),
                environment_selections,
            ),
            codex_home: config.codex_home.clone(),
            thread_name: None,
            original_config_do_not_use: Arc::clone(&config),
            metrics_service_name,
            app_server_client_name: None,
            app_server_client_version: None,
            session_source,
            history_mode,
            forked_from_thread_id,
            parent_thread_id,
            thread_source,
            originator,
            dynamic_tools,
            user_shell_override,
        };

        // Generate a unique ID for the lifetime of this session.
        let session_source_clone = session_configuration.session_source.clone();
        let (agent_status_tx, agent_status_rx) = watch::channel(AgentStatus::PendingInit);

        let session = Box::pin(Session::new(
            session_configuration,
            config.clone(),
            user_instructions,
            installation_id,
            auth_manager.clone(),
            models_manager.clone(),
            exec_policy,
            tx_sub.clone(),
            tx_event.clone(),
            agent_status_tx.clone(),
            conversation_history,
            fork_persistence,
            session_source_clone,
            skills_service,
            plugins_manager,
            mcp_manager.clone(),
            code_mode_session_provider,
            extensions,
            thread_extension_init,
            supports_openai_form_elicitation,
            agent_control,
            environment_manager,
            inherited_environments,
            analytics_events_client,
            thread_store,
            parent_rollout_thread_trace,
            attestation_provider,
            external_time_provider,
            multi_agent_version,
            git_enrichment_policy,
            windows_sandbox_proxy_settings_mode,
        ))
        .await
        .map_err(|e| {
            error!("Failed to create session: {e:#}");
            map_session_init_error(&e, &config.codex_home)
        })?;
        if let Some(message) = initial_service_tier_warning {
            session
                .send_event_raw(Event {
                    id: INITIAL_SUBMIT_ID.to_owned(),
                    msg: EventMsg::Warning(WarningEvent { message }),
                })
                .await;
        }
        let thread_id = session.thread_id;

        // This task will run until Op::Shutdown is received.
        let session_for_loop = Arc::clone(&session);
        let session_loop_handle = tokio::spawn(async move {
            submission_loop(session_for_loop, config, rx_sub)
                .instrument(info_span!("session_loop", thread_id = %thread_id))
                .await;
        });
        let io = SessionIo {
            tx_sub,
            rx_event,
            agent_status: agent_status_rx,
            session_loop_termination: session_loop_termination_from_handle(session_loop_handle),
        };

        Ok((session, io))
    }
}

impl SessionIo {
    /// Submit the `op` wrapped in a `Submission` with a unique ID.
    pub(crate) async fn submit(&self, op: Op) -> CodexResult<String> {
        self.submit_with_trace(op, /*trace*/ None, /*parent_turn_id*/ None)
            .await
    }

    pub(crate) async fn submit_with_trace(
        &self,
        op: Op,
        trace: Option<W3cTraceContext>,
        parent_turn_id: Option<String>,
    ) -> CodexResult<String> {
        let id = new_submission_id();
        let sub = Submission {
            id: id.clone(),
            op,
            client_user_message_id: None,
            trace,
            parent_turn_id,
        };
        self.submit_with_id(sub).await?;
        Ok(id)
    }

    pub(crate) async fn submit_user_input_with_client_user_message_id(
        &self,
        op: Op,
        trace: Option<W3cTraceContext>,
        client_user_message_id: Option<String>,
    ) -> CodexResult<String> {
        debug_assert!(matches!(op, Op::UserInput { .. }));
        let id = new_submission_id();
        let sub = Submission {
            id: id.clone(),
            op,
            client_user_message_id,
            trace,
            parent_turn_id: None,
        };
        self.submit_with_id(sub).await?;
        Ok(id)
    }

    /// Use sparingly: prefer `submit()` so submission IDs are generated consistently.
    pub(crate) async fn submit_with_id(&self, mut sub: Submission) -> CodexResult<()> {
        if sub.trace.is_none() {
            sub.trace = current_span_w3c_trace_context();
        }
        self.tx_sub
            .send(sub)
            .await
            .map_err(|_| CodexErr::InternalAgentDied)?;
        Ok(())
    }

    pub(crate) async fn shutdown_and_wait(&self) -> CodexResult<()> {
        let session_loop_termination = self.session_loop_termination.clone();
        match self.submit(Op::Shutdown).await {
            Ok(_) => {}
            Err(err) if matches!(err.details(), CodexErrorDetails::InternalAgentDied) => {}
            Err(err) => return Err(err),
        }
        session_loop_termination.await;
        Ok(())
    }

    pub(crate) async fn next_event(&self) -> CodexResult<Event> {
        loop {
            let event = self
                .rx_event
                .recv()
                .await
                .map_err(|_| CodexErr::InternalAgentDied)?;
            // PF-28-S01: one choke point for every producer, including those
            // holding a cloned sender (MCP startup, elicitation).
            if let Some(msg) = crate::security::disclosure_gate::gate_delivered(event.msg) {
                return Ok(Event { id: event.id, msg });
            }
        }
    }

    pub(crate) async fn agent_status(&self) -> AgentStatus {
        self.agent_status.borrow().clone()
    }
}

/// Generate a core submission ID. App-server exposes submission IDs that
/// create turns as a public-facing turn ID.
///
/// Some use cases take advantage of the fact that these are UUID7 which
/// encodes a timestamp, so think carefully before changing this.
fn new_submission_id() -> String {
    Uuid::now_v7().to_string()
}

fn get_service_tier(
    configured_service_tier: Option<String>,
    fast_mode_enabled: bool,
    model_info: &ModelInfo,
) -> Option<String> {
    if !fast_mode_enabled {
        return None;
    }
    configured_service_tier.filter(|service_tier| {
        service_tier == SERVICE_TIER_DEFAULT_REQUEST_VALUE
            || model_info.supports_service_tier(service_tier)
    })
}

fn unsupported_service_tier_warning(
    configured_service_tier: Option<&str>,
    fast_mode_enabled: bool,
    model_info: &ModelInfo,
) -> Option<String> {
    let service_tier = configured_service_tier.filter(|service_tier| {
        fast_mode_enabled
            && *service_tier != SERVICE_TIER_DEFAULT_REQUEST_VALUE
            && !model_info.supports_service_tier(service_tier)
    })?;
    Some(format!(
        "Configured service tier `{service_tier}` is not advertised as supported for model `{}` and will be omitted from requests.",
        model_info.slug
    ))
}

fn session_permission_profile_state_from_config(
    config: &Config,
) -> CodexResult<PermissionProfileState> {
    Ok(config.permissions.permission_profile_state().clone())
}

#[cfg(test)]
pub(crate) fn completed_session_loop_termination() -> SessionLoopTermination {
    futures::future::ready(()).boxed().shared()
}

/// `source_envelopes`: an MCP-supplied token-budget hint moves out of the host
/// developer message into its own data message. Returns (inline, separate).
pub(crate) fn token_budget_hint_placement(
    separate: bool,
    hint: Option<String>,
) -> (Option<String>, Option<String>) {
    if separate { (None, hint) } else { (hint, None) }
}

pub(crate) fn session_loop_termination_from_handle(
    handle: JoinHandle<()>,
) -> SessionLoopTermination {
    async move {
        let _ = handle.await;
    }
    .boxed()
    .shared()
}

async fn thread_title_from_thread_store(
    live_thread: Option<&LiveThread>,
    thread_store: &Arc<dyn ThreadStore>,
    conversation_id: ThreadId,
) -> Option<String> {
    let thread = match live_thread {
        Some(live_thread) => {
            live_thread
                .read_thread(
                    /*include_archived*/ true, /*include_history*/ false,
                )
                .await
        }
        None => {
            thread_store
                .read_thread(ReadThreadParams {
                    thread_id: conversation_id,
                    include_archived: true,
                    include_history: false,
                })
                .await
        }
    }
    .ok()?;

    let title = thread.name.as_deref()?.trim();
    (!title.is_empty() && thread.preview.trim() != title).then(|| title.to_string())
}

fn push_prompt_fragment(
    fragment: PromptFragment,
    developer_sections: &mut Vec<ContextSection>,
    contextual_user_sections: &mut Vec<String>,
    separate_developer_sections: &mut Vec<ContextSection>,
) {
    let developer_section =
        || ContextSection::new(fragment.text().to_string(), fragment.source_id());
    match fragment.slot() {
        PromptSlot::DeveloperPolicy | PromptSlot::DeveloperCapabilities => {
            developer_sections.push(developer_section());
        }
        PromptSlot::ContextualUser => {
            contextual_user_sections.push(fragment.text().to_string());
        }
        PromptSlot::SeparateDeveloper => {
            separate_developer_sections.push(developer_section());
        }
    }
}

impl Session {
    fn trace_session_timing(label: &str, start: Instant) {
        if std::env::var_os("PFTERMINAL_TRACE_STREAM_TIMING").is_some() {
            debug!(
                target: "corbanu_session",
                label,
                elapsed_ms = start.elapsed().as_millis(),
                "pfterminal session timing"
            );
        }
    }

    pub(crate) async fn app_server_client_metadata(&self) -> AppServerClientMetadata {
        let state = self.state.lock().await;
        AppServerClientMetadata {
            client_name: state.session_configuration.app_server_client_name.clone(),
            client_version: state
                .session_configuration
                .app_server_client_version
                .clone(),
        }
    }

    fn managed_network_proxy_active_for_permission_profile(
        permission_profile: &PermissionProfile,
    ) -> bool {
        !matches!(permission_profile, PermissionProfile::Disabled)
    }

    /// Builds the `x-codex-beta-features` header value for this session.
    ///
    /// `ModelClient` is session-scoped and intentionally does not depend on the full `Config`, so
    /// we precompute the comma-separated list of enabled experimental feature keys at session
    /// creation time and thread it into the client.
    fn build_model_client_beta_features_header(config: &Config) -> Option<String> {
        let beta_features_header = FEATURES
            .iter()
            .filter_map(|spec| {
                let advertise_in_model_client_header =
                    spec.stage.experimental_menu_description().is_some()
                        || spec.id == Feature::RemoteCompactionV2;
                if advertise_in_model_client_header && config.features.enabled(spec.id) {
                    Some(spec.key)
                } else {
                    None
                }
            })
            .collect::<Vec<_>>()
            .join(",");

        if beta_features_header.is_empty() {
            None
        } else {
            Some(beta_features_header)
        }
    }

    async fn start_managed_network_proxy(
        spec: &crate::config::NetworkProxySpec,
        exec_policy: &codex_execpolicy::Policy,
        permission_profile: &PermissionProfile,
        network_policy_decider: Option<Arc<dyn codex_network_proxy::NetworkPolicyDecider>>,
        blocked_request_observer: Option<Arc<dyn codex_network_proxy::BlockedRequestObserver>>,
        managed_network_requirements_enabled: bool,
        audit_metadata: NetworkProxyAuditMetadata,
    ) -> anyhow::Result<(StartedNetworkProxy, SessionNetworkProxyRuntime)> {
        let spec = spec
            .with_exec_policy_network_rules(exec_policy)
            .map_err(|err| {
                tracing::warn!(
                    "failed to apply execpolicy network rules to managed proxy; continuing with configured network policy: {err}"
                );
                err
            })
            .unwrap_or_else(|_| spec.clone());
        let network_proxy = spec
            .start_proxy(
                permission_profile,
                network_policy_decider,
                blocked_request_observer,
                managed_network_requirements_enabled,
                audit_metadata,
            )
            .await
            .map_err(|err| anyhow::anyhow!("failed to start managed network proxy: {err}"))?;
        let session_network_proxy = {
            let proxy = network_proxy.proxy();
            SessionNetworkProxyRuntime {
                http_addr: proxy.http_addr().to_string(),
                socks_addr: proxy.socks_addr().to_string(),
            }
        };
        Ok((network_proxy, session_network_proxy))
    }

    async fn refresh_managed_network_proxy_for_current_permission_profile(&self) {
        let Ok(_refresh_guard) = self.managed_network_proxy_refresh_lock.acquire().await else {
            error!("managed network proxy refresh semaphore closed");
            return;
        };
        let session_configuration = {
            let state = self.state.lock().await;
            state.session_configuration.clone()
        };
        let Some(spec) = session_configuration
            .original_config_do_not_use
            .permissions
            .network
            .as_ref()
            .cloned()
        else {
            self.services.network_proxy.store(None);
            return;
        };

        let spec = match spec
            .recompute_for_permission_profile(&session_configuration.permission_profile())
        {
            Ok(spec) => spec,
            Err(err) => {
                warn!("failed to rebuild managed network proxy policy for sandbox change: {err}");
                return;
            }
        };
        // D52 (#1001): under Aggressive, network allow rules saved outside it
        // are suspended.
        let current_exec_policy = self
            .services
            .exec_policy
            .current_for(self.services.model_client().live_level_is_aggressive());
        let spec = match spec.with_exec_policy_network_rules(current_exec_policy.as_ref()) {
            Ok(spec) => spec,
            Err(err) => {
                warn!(
                    "failed to apply execpolicy network rules while refreshing managed network proxy: {err}"
                );
                spec
            }
        };
        if let Some(started_proxy) = self.services.network_proxy.load_full() {
            if let Err(err) = spec.apply_to_started_proxy(started_proxy.as_ref()).await {
                warn!("failed to refresh managed network proxy for sandbox change: {err}");
            }
            return;
        }

        match Self::start_managed_network_proxy(
            &spec,
            current_exec_policy.as_ref(),
            &session_configuration.permission_profile(),
            /*network_policy_decider*/ None,
            Some(build_blocked_request_observer(Arc::clone(
                &self.services.network_approval,
            ))),
            self.services.managed_network_requirements_configured,
            self.services.network_proxy_audit_metadata.clone(),
        )
        .await
        {
            Ok((started_proxy, _session_network_proxy)) => {
                self.services
                    .network_proxy
                    .store(Some(Arc::new(started_proxy)));
            }
            Err(err) => {
                warn!("failed to start managed network proxy for sandbox change: {err}");
            }
        }
    }

    #[cfg(test)]
    pub(crate) async fn codex_home(&self) -> AbsolutePathBuf {
        let state = self.state.lock().await;
        state.session_configuration.codex_home().clone()
    }

    pub(crate) fn subscribe_elicitation_pause_state(&self) -> watch::Receiver<bool> {
        self.services.elicitations.subscribe()
    }

    pub(crate) fn get_tx_event(&self) -> Sender<Event> {
        self.tx_event.clone()
    }

    pub(crate) fn state_db(&self) -> Option<state_db::StateDbHandle> {
        self.services.state_db.clone()
    }

    pub(crate) fn live_thread_for_persistence(
        &self,
        operation: &str,
    ) -> anyhow::Result<&LiveThread> {
        self.live_thread()
            .ok_or_else(|| anyhow::anyhow!("Session persistence is disabled; cannot {operation}."))
    }

    pub(crate) fn live_thread(&self) -> Option<&LiveThread> {
        self.services.live_thread.as_ref()
    }

    pub(crate) async fn set_thread_memory_mode(
        self: &Arc<Self>,
        mode: ThreadMemoryMode,
    ) -> anyhow::Result<()> {
        handlers::persist_thread_memory_mode_update(self, mode).await
    }

    /// Flush rollout writes and return the final durability-barrier result.
    #[instrument(name = "session.flush_rollout", level = "trace", skip_all)]
    pub(crate) async fn flush_rollout(&self) -> std::io::Result<()> {
        if let Some(live_thread) = self.live_thread() {
            live_thread.flush().await.map_err(std::io::Error::other)
        } else {
            Ok(())
        }
    }

    pub(crate) async fn try_ensure_rollout_materialized(&self) -> std::io::Result<()> {
        if let Some(live_thread) = self.live_thread() {
            live_thread.persist().await.map_err(std::io::Error::other)?;
        }
        Ok(())
    }

    pub(crate) async fn ensure_rollout_materialized(&self) {
        if let Err(e) = self.try_ensure_rollout_materialized().await {
            warn!("failed to materialize thread persistence: {e}");
        }
    }

    pub(crate) fn next_internal_sub_id(&self) -> String {
        let id = self
            .next_internal_sub_id
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        format!("auto-compact-{id}")
    }

    pub(crate) async fn route_realtime_text_input(self: &Arc<Self>, text: String) {
        handlers::user_input_or_turn_inner(
            self,
            Uuid::now_v7().to_string(),
            Op::UserInput {
                items: vec![UserInput::Text {
                    text,
                    text_elements: Vec::new(),
                }],
                final_output_json_schema: None,
                responsesapi_client_metadata: None,
                additional_context: Default::default(),
                thread_settings: Default::default(),
            },
            /*client_user_message_id*/ None,
            /*parent_turn_id*/ None,
        )
        .await;
    }

    pub(crate) async fn get_total_token_usage(&self) -> i64 {
        let state = self.state.lock().await;
        state.get_total_token_usage(state.server_reasoning_included())
    }

    pub(crate) async fn auto_compact_window_snapshot(&self) -> AutoCompactWindowSnapshot {
        let state = self.state.lock().await;
        state.auto_compact_window_snapshot()
    }

    pub(crate) async fn estimated_tokens_after_last_model_generated_item(&self) -> i64 {
        let state = self.state.lock().await;
        state
            .history
            .estimated_tokens_after_last_model_generated_item()
    }

    pub(crate) async fn total_token_usage(&self) -> Option<TokenUsage> {
        let state = self.state.lock().await;
        state.token_info().map(|info| info.total_token_usage)
    }

    /// Returns the complete token usage snapshot currently cached for this session.
    ///
    /// Resume and fork reconstruction seed this state from the last persisted rollout
    /// `TokenCount` event. Callers that need to replay restored usage to a client
    /// should use this accessor instead of `total_token_usage`, because the app-server
    /// notification includes both total and last-turn usage.
    pub(crate) async fn token_usage_info(&self) -> Option<TokenUsageInfo> {
        let state = self.state.lock().await;
        state.token_info()
    }

    /// PF-84: usage of requests made for this conversation outside its own
    /// turn loop (a guardian or `/review` delegate, stage-one memory
    /// extraction), which the accounting ledger also records under it. Sent
    /// at once on the active turn, if any; otherwise with the next count.
    pub(crate) async fn record_delegated_account_usage(&self, usage: &[AccountTokenUsage]) {
        if usage.is_empty() || !self.features().enabled(Feature::NamedAccounts) {
            return;
        }
        {
            let mut state = self.state.lock().await;
            for entry in usage {
                state.record_account_token_usage(&entry.provider_id, &entry.account, &entry.usage);
            }
        }
        if let Some((turn_context, _)) = self.active_turn_context_and_cancellation_token().await {
            self.send_token_count_event(&turn_context).await;
        }
    }

    /// PF-84: this thread's provider-reported usage per named account, for
    /// replay to a client that attaches after resume.
    pub(crate) async fn account_token_usage(&self) -> Option<Vec<AccountTokenUsage>> {
        let state = self.state.lock().await;
        state.account_token_usage()
    }

    pub(crate) async fn get_estimated_token_count(
        &self,
        turn_context: &TurnContext,
    ) -> Option<i64> {
        let history = self.clone_history().await;
        history.estimate_token_count(turn_context)
    }

    pub(crate) async fn get_base_instructions(&self) -> BaseInstructions {
        let state = self.state.lock().await;
        BaseInstructions {
            text: state.session_configuration.base_instructions.clone(),
        }
    }

    // Merges connector IDs into the session-level explicit connector selection.
    #[tracing::instrument(
        level = "trace",
        skip_all,
        fields(connector_count = connector_ids.len())
    )]
    pub(crate) async fn merge_connector_selection(
        &self,
        connector_ids: HashSet<String>,
    ) -> HashSet<String> {
        let mut state = self.state.lock().await;
        state.merge_connector_selection(connector_ids)
    }

    // Returns the connector IDs currently selected for this session.
    pub(crate) async fn get_connector_selection(&self) -> HashSet<String> {
        let state = self.state.lock().await;
        state.get_connector_selection()
    }

    // Clears connector IDs that were accumulated for explicit selection.
    pub(crate) async fn clear_connector_selection(&self) {
        let mut state = self.state.lock().await;
        state.clear_connector_selection();
    }

    async fn record_initial_history(&self, conversation_history: InitialHistory) {
        let initial_history_started_at = Instant::now();
        Self::trace_session_timing("record_initial_history_start", initial_history_started_at);
        let (is_subagent, is_paginated_subagent) = {
            let state = self.state.lock().await;
            let session_configuration = &state.session_configuration;
            (
                session_configuration.session_source.is_non_root_agent(),
                matches!(
                    session_configuration.history_mode,
                    ThreadHistoryMode::Paginated
                ) && matches!(
                    session_configuration.thread_source.as_ref(),
                    Some(ThreadSource::Subagent)
                ),
            )
        };
        Self::trace_session_timing(
            "record_initial_history_after_subagent_check",
            initial_history_started_at,
        );
        let has_prior_user_turns = initial_history_has_prior_user_turns(&conversation_history);
        {
            let mut state = self.state.lock().await;
            state.set_next_turn_is_first(!has_prior_user_turns);
        }
        Self::trace_session_timing(
            "record_initial_history_after_first_turn_state",
            initial_history_started_at,
        );
        match conversation_history {
            InitialHistory::New | InitialHistory::Cleared => {
                // Defer initial context insertion until the first real turn starts so
                // turn/start overrides can be merged before we write model-visible context.
                self.set_previous_turn_settings(/*previous_turn_settings*/ None)
                    .await;
                Self::trace_session_timing(
                    "record_initial_history_new_done",
                    initial_history_started_at,
                );
            }
            InitialHistory::Resumed(resumed_history) => {
                let turn_context = self.new_default_turn().await;
                let rollout_items = resumed_history.history;
                Self::trace_session_timing(
                    "record_initial_history_resumed_after_default_turn",
                    initial_history_started_at,
                );
                let previous_turn_settings = self
                    .apply_rollout_reconstruction(&turn_context, &rollout_items)
                    .await;
                Self::trace_session_timing(
                    "record_initial_history_resumed_after_reconstruction",
                    initial_history_started_at,
                );

                // If resuming, warn when the last recorded model differs from the current one.
                let curr: &str = turn_context.model_info.slug.as_str();
                if let Some(prev) = previous_turn_settings
                    .as_ref()
                    .map(|settings| settings.model.as_str())
                    .filter(|model| *model != curr)
                {
                    warn!("resuming session with different model: previous={prev}, current={curr}");
                    self.send_event(
                        &turn_context,
                        EventMsg::Warning(WarningEvent {
                            message: format!(
                                "This session was recorded with model `{prev}` but is resuming with `{curr}`. \
                         Consider switching back to `{prev}` as it may affect Codex performance."
                            ),
                        }),
                    )
                    .await;
                }

                if let Some(event) =
                    Self::last_model_response_completed_from_rollout(&rollout_items)
                    && event.model == turn_context.model_info.slug.as_str()
                    && event.model_provider_id == turn_context.config.model_provider_id.as_str()
                {
                    self.services
                        .model_client()
                        .seed_server_response_id(event.response_id);
                }

                // Seed usage info from the recorded rollout so UIs can show token counts
                // immediately on resume/fork.
                if let Some(info) = Self::last_token_info_from_rollout(&rollout_items) {
                    let mut state = self.state.lock().await;
                    state.set_token_info(Some(info));
                }
                // PF-84: a resumed thread keeps its usage per account, only while
                // named accounts are on. A fork does not: its parent's requests
                // stay the parent's.
                if turn_context.config.features.enabled(Feature::NamedAccounts)
                    && let Some(usage) = Self::last_account_token_usage_from_rollout(&rollout_items)
                {
                    let mut state = self.state.lock().await;
                    state.set_account_token_usage(usage);
                }
                Self::trace_session_timing(
                    "record_initial_history_resumed_after_token_seed",
                    initial_history_started_at,
                );

                // Defer seeding the session's initial context until the first turn starts so
                // turn/start overrides can be merged before we write to the rollout.
                if !is_subagent {
                    let _ = self.flush_rollout().await;
                }
                Self::trace_session_timing(
                    "record_initial_history_resumed_done",
                    initial_history_started_at,
                );
            }
            InitialHistory::Forked(mut rollout_items) => {
                let turn_context = self.new_default_turn().await;
                Self::assign_missing_rollout_response_item_ids(&mut rollout_items);
                self.apply_rollout_reconstruction(&turn_context, &rollout_items)
                    .await;
                Self::trace_session_timing(
                    "record_initial_history_forked_after_reconstruction",
                    initial_history_started_at,
                );

                // Seed usage info from the recorded rollout so UIs can show token counts
                // immediately on resume/fork.
                if let Some(info) = Self::last_token_info_from_rollout(&rollout_items) {
                    let mut state = self.state.lock().await;
                    state.set_token_info(Some(info));
                }

                let thread_settings_applied =
                    RolloutItem::EventMsg(handlers::thread_settings_applied_event(self).await);
                match &self.fork_persistence {
                    ForkPersistence::Referenced {
                        inherited_item_count,
                        ..
                    } => {
                        // Ancestor records remain behind history_base; only effective child
                        // settings and boundaries synthesized by snapshot processing are local.
                        rollout_items.drain(..*inherited_item_count);
                        rollout_items.insert(0, thread_settings_applied);
                    }
                    ForkPersistence::Copied if is_paginated_subagent => {
                        // Paginated subagents already persist inherited context when their live
                        // thread is created.
                        rollout_items.clear();
                        rollout_items.push(thread_settings_applied);
                    }
                    ForkPersistence::Copied => {
                        // Keep the copied prefix and effective child settings in one append so a
                        // cold resume cannot observe inherited settings as the latest value.
                        rollout_items.push(thread_settings_applied);
                    }
                }
                // PF-84: the copied prefix holds the parent's usage per account; mark
                // where this fork's own (none yet) begins, so a resume before its first
                // reply does not count the parent's again.
                if turn_context.config.features.enabled(Feature::NamedAccounts) {
                    rollout_items.push(RolloutItem::EventMsg(EventMsg::TokenCount(
                        TokenCountEvent {
                            info: None,
                            rate_limits: None,
                            account_usage: Some(Vec::new()),
                        },
                    )));
                }
                self.persist_rollout_items(&rollout_items).await;

                // Forked threads should remain file-backed immediately after startup.
                self.ensure_rollout_materialized().await;

                // Flush after seeding history and any persisted rollout copy.
                if !is_subagent {
                    let _ = self.flush_rollout().await;
                }
                Self::trace_session_timing(
                    "record_initial_history_forked_done",
                    initial_history_started_at,
                );
            }
        }
    }

    #[instrument(
        level = "trace",
        skip_all,
        fields(
            thread_id = %self.thread_id(),
            rollout_item_count = rollout_items.len()
        )
    )]
    async fn apply_rollout_reconstruction(
        &self,
        turn_context: &TurnContext,
        rollout_items: &[RolloutItem],
    ) -> Option<PreviousTurnSettings> {
        let rollout_reconstruction::RolloutReconstruction {
            mut history,
            previous_turn_settings,
            reference_context_item,
            world_state_baseline,
            window_number,
            first_window_id,
            previous_window_id,
            window_id,
        } = self
            .reconstruct_history_from_rollout(turn_context, rollout_items)
            .await;
        // Keep the recorded rollout unchanged. Prepare its reconstructed history before
        // installing it, so legacy media is processed once for this resume or fork and
        // will be processed again if the rollout is reconstructed in a future session.
        // This meets media preparation requirements without modifying persisted rollouts.
        prepare_image_response_items(&mut history);
        prepare_audio_response_items(&mut history);
        // Restored content keeps the standing its origin records give it;
        // anything unrecorded stays labelled (`source_envelopes`).
        self.services.model_client().note_restored_history(
            &history,
            rollout_items.iter().filter_map(|item| match item {
                RolloutItem::SourceOrigin(record) => Some(record),
                _ => None,
            }),
        );
        {
            let mut state = self.state.lock().await;
            state.replace_history(history, reference_context_item);
            if let Some(world_state) = world_state_baseline {
                state.history.set_world_state_baseline(world_state);
            }
            let fallback_ids = state.auto_compact_window_ids();
            let window_id = window_id.unwrap_or(fallback_ids.window_id);
            state.restore_auto_compact_window(
                window_number,
                AutoCompactWindowIds {
                    first_window_id: first_window_id.unwrap_or(window_id),
                    previous_window_id,
                    window_id,
                },
            );
            state.set_previous_turn_settings(previous_turn_settings.clone());
        }
        let prefix_tokens = if matches!(
            turn_context.config.model_auto_compact_token_limit_scope,
            AutoCompactTokenLimitScope::BodyAfterPrefix
        ) {
            let history = self.clone_history().await;
            let base_instructions = self.get_base_instructions().await;
            history.estimate_token_count_with_base_instructions(&base_instructions)
        } else {
            None
        };
        if let Some(prefix_tokens) = prefix_tokens {
            self.set_auto_compact_window_estimated_prefill_for_scope(turn_context, prefix_tokens)
                .await;
        }
        previous_turn_settings
    }

    async fn set_auto_compact_window_estimated_prefill_for_scope(
        &self,
        turn_context: &TurnContext,
        tokens: i64,
    ) {
        if !matches!(
            turn_context.config.model_auto_compact_token_limit_scope,
            AutoCompactTokenLimitScope::BodyAfterPrefix
        ) {
            return;
        }

        let mut state = self.state.lock().await;
        state.set_auto_compact_window_estimated_prefill(tokens);
    }

    fn last_token_info_from_rollout(rollout_items: &[RolloutItem]) -> Option<TokenUsageInfo> {
        rollout_items.iter().rev().find_map(|item| match item {
            RolloutItem::EventMsg(EventMsg::TokenCount(ev)) => ev.info.clone(),
            _ => None,
        })
    }

    fn last_account_token_usage_from_rollout(
        rollout_items: &[RolloutItem],
    ) -> Option<Vec<AccountTokenUsage>> {
        rollout_items.iter().rev().find_map(|item| match item {
            RolloutItem::EventMsg(EventMsg::TokenCount(ev)) => ev.account_usage.clone(),
            _ => None,
        })
    }

    fn last_model_response_completed_from_rollout(
        rollout_items: &[RolloutItem],
    ) -> Option<ModelResponseCompletedEvent> {
        rollout_items.iter().rev().find_map(|item| match item {
            RolloutItem::EventMsg(EventMsg::ModelResponseCompleted(ev))
                if !ev.response_id.is_empty() =>
            {
                Some(ev.clone())
            }
            _ => None,
        })
    }

    async fn previous_turn_settings(&self) -> Option<PreviousTurnSettings> {
        let state = self.state.lock().await;
        state.previous_turn_settings()
    }

    #[tracing::instrument(level = "trace", skip_all)]
    pub(crate) async fn set_previous_turn_settings(
        &self,
        previous_turn_settings: Option<PreviousTurnSettings>,
    ) {
        let mut state = self.state.lock().await;
        state.set_previous_turn_settings(previous_turn_settings);
    }

    pub(crate) async fn update_settings(
        &self,
        mut updates: SessionSettingsUpdate,
    ) -> ConstraintResult<()> {
        self.ensure_runtime_model_provider(&mut updates).await;
        let notify_config_contributors = !self.services.extensions.config_contributors().is_empty();
        let (
            previous_config,
            new_config,
            permission_profile_changed,
            mcp_inputs_changed,
            stale_startup_prewarm,
            follows_model_list,
        ) = {
            let mut state = self.state.lock().await;
            let updated = match state.session_configuration.apply(&updates) {
                Ok(updated) => updated,
                Err(err) => {
                    warn!("rejected session settings update: {err}");
                    return Err(err);
                }
            };

            let previous_config = notify_config_contributors
                .then(|| Self::build_effective_session_config(&state.session_configuration));
            let new_config =
                notify_config_contributors.then(|| Self::build_effective_session_config(&updated));
            let previous_permission_profile = state.session_configuration.permission_profile();
            let updated_permission_profile = updated.permission_profile();
            let permission_profile_changed =
                previous_permission_profile != updated_permission_profile;
            let mcp_inputs_changed = state.session_configuration.mcp_inputs_differ(&updated);
            let model_provider_changed = state
                .session_configuration
                .original_config_do_not_use
                .model_provider_id
                != updated.original_config_do_not_use.model_provider_id
                || state.session_configuration.provider != updated.provider;
            let account_switched = model_provider_changed && self.follow_provider_account(&updated);
            if model_provider_changed {
                // Publish while updates are serialized, before the new configuration
                // becomes observable. Frame guards read this client without state locks.
                self.services
                    .replace_model_client(self.build_model_client_for_configuration(&updated));
            }
            let stale_startup_prewarm = if model_provider_changed {
                state.take_session_startup_prewarm()
            } else {
                None
            };
            if updates.environments.is_some() {
                self.services
                    .turn_environments
                    .update_selections(updated.environment_selections());
            }
            let follows_model_list =
                model_provider_changed && self.model_list_follows_this_session(&updated);
            state.session_configuration = updated;
            if mcp_inputs_changed {
                self.mark_mcp_runtime_dirty();
            }
            (
                previous_config,
                new_config,
                permission_profile_changed,
                mcp_inputs_changed || account_switched,
                stale_startup_prewarm,
                follows_model_list,
            )
        };
        if follows_model_list {
            self.follow_model_list_account().await;
        }
        self.emit_config_changed_contributors(previous_config.as_ref(), new_config.as_ref());
        if let Some(startup_prewarm) = stale_startup_prewarm {
            startup_prewarm.abort().await;
        }
        if permission_profile_changed {
            self.refresh_managed_network_proxy_for_current_permission_profile()
                .await;
        }
        if mcp_inputs_changed {
            self.schedule_mcp_prewarm();
        }
        Ok(())
    }

    /// PF-84: whether this session's account drives the process's model
    /// list: a user's session, not a sub-agent's or an internal one's.
    pub(super) fn model_list_follows_this_session(
        &self,
        configuration: &SessionConfiguration,
    ) -> bool {
        !configuration.session_source.is_non_root_agent()
    }

    /// PF-84: on a provider change, makes the new provider's sign-in (a named
    /// account's or the default login) the one connectors, plugin installs and
    /// accounting use, before the model client is built. Called under the
    /// state lock by every path that changes the provider. Returns whether the
    /// account changed; connectors are then marked for reconnection.
    pub(super) fn follow_provider_account(&self, next: &SessionConfiguration) -> bool {
        if !self.services.active_auth.follow_provider(&next.provider) {
            return false;
        }
        let account = codex_model_provider::named_sign_in_account(&next.provider)
            .map_or_else(|| "default".to_string(), |account| account.to_string());
        info!(
            %account,
            "connectors, plugin installs, accounting and the model list follow the switch"
        );
        self.mark_mcp_runtime_dirty();
        true
    }

    /// PF-84: lists models with the sign-in of the account this session's
    /// provider uses (the default login for any other provider). When that
    /// changed, the account's list is fetched again: in the background when
    /// its own cache (see `AccountModelsManager`) is fresh, otherwise waited
    /// for, at most [`FIRST_MODEL_LIST_WAIT`], so the client's next
    /// `model/list` has it.
    pub(super) async fn follow_model_list_account(&self) {
        let (provider, http_client_factory) = {
            let state = self.state.lock().await;
            (
                state.session_configuration.provider.clone(),
                state
                    .session_configuration
                    .original_config_do_not_use
                    .http_client_factory(),
            )
        };
        let account = codex_model_provider::named_sign_in_account(&provider);
        if !self
            .services
            .models_manager
            .follow_sign_in_account(account.as_ref())
        {
            return;
        }
        let models_manager = Arc::clone(&self.services.models_manager);
        let cached = models_manager.load_cached_list().await;
        let fetch = self.services.runtime_handle.spawn(async move {
            models_manager
                .list_models(RefreshStrategy::Online, http_client_factory)
                .await;
        });
        if !cached {
            // On timeout the fetch goes on in the background.
            let _ = tokio::time::timeout(FIRST_MODEL_LIST_WAIT, fetch).await;
        }
    }

    async fn ensure_runtime_model_provider(&self, updates: &mut SessionSettingsUpdate) {
        let model_provider_id = match updates.model_provider.clone() {
            Some(model_provider_id) => model_provider_id,
            None => {
                let current_model_provider_id = {
                    let state = self.state.lock().await;
                    state
                        .session_configuration
                        .original_config_do_not_use
                        .model_provider_id
                        .clone()
                };
                if !current_model_provider_id.starts_with("gpu-") {
                    return;
                }
                updates.model_provider = Some(current_model_provider_id.clone());
                current_model_provider_id
            }
        };
        if !model_provider_id.starts_with("gpu-") {
            updates.runtime_model_context_window = Some(None);
            return;
        }
        {
            let runtime_overlay_source = {
                let state = self.state.lock().await;
                (
                    state
                        .session_configuration
                        .original_config_do_not_use
                        .sqlite
                        .home()
                        .to_path_buf(),
                    state
                        .session_configuration
                        .original_config_do_not_use
                        .codex_home
                        .clone(),
                )
            };
            let (sqlite_home, codex_home) = runtime_overlay_source;
            let records =
                crate::config::load_gpu_runtime_model_provider_records(&sqlite_home).await;
            updates.runtime_model_context_window = Some(
                records
                    .iter()
                    .find(|record| record.provider_id == model_provider_id)
                    .and_then(|record| record.maximum_context_tokens),
            );
            let runtime_providers = records
                .into_iter()
                .filter_map(|record| crate::config::gpu_runtime_model_provider(record, &codex_home))
                .collect::<HashMap<_, _>>();
            if runtime_providers.contains_key(&model_provider_id) {
                let mut state = self.state.lock().await;
                let mut config = (*state.session_configuration.original_config_do_not_use).clone();
                // Runtime endpoints are controller-owned process-local transports. Replacing an
                // existing entry is required after controller or SSH-forward recovery; presence
                // alone does not mean the cached base URL is still current.
                config.model_providers.extend(runtime_providers);
                state.session_configuration.original_config_do_not_use = Arc::new(config);
            }
        }
    }

    fn build_model_client_for_configuration(
        &self,
        configuration: &SessionConfiguration,
    ) -> ModelClient {
        let config = configuration.original_config_do_not_use.as_ref();
        ModelClient::new(
            Some(self.services.auth_manager()),
            if config.features.enabled(Feature::UseAgentIdentity) {
                codex_login::auth::AgentIdentityAuthPolicy::ChatGptAuth
            } else {
                codex_login::auth::AgentIdentityAuthPolicy::JwtOnly
            },
            self.thread_id,
            configuration.provider.clone(),
            configuration.session_source.clone(),
            configuration.originator.clone(),
            config.model_verbosity,
            config.features.enabled(Feature::EnableRequestCompression),
            config.features.enabled(Feature::RuntimeMetrics),
            Self::build_model_client_beta_features_header(config),
            config
                .features
                .enabled(Feature::ConcurrentReasoningSummaries),
            self.services.attestation_provider.clone(),
            config.http_client_factory(),
        )
        .with_ingress_policy(
            config.security_level,
            self.services.agent_control.effective_security_policy(),
        )
        .with_native_ingress_from(&self.services.model_client())
        .with_source_envelopes(config.features.enabled(Feature::SourceEnvelopes))
        .with_source_origin_key(config.codex_home.as_path())
        .with_prompt_cache_key_override(
            crate::guardian::prompt_cache_key_override_for_review_session(
                &configuration.session_source,
                configuration.parent_thread_id,
            ),
        )
    }

    pub(crate) async fn preview_settings(
        &self,
        updates: &SessionSettingsUpdate,
    ) -> ConstraintResult<ThreadConfigSnapshot> {
        let mut updates = updates.clone();
        self.ensure_runtime_model_provider(&mut updates).await;
        let state = self.state.lock().await;
        state
            .session_configuration
            .apply(&updates)
            .map(|configuration| configuration.thread_config_snapshot())
    }

    pub(crate) async fn thread_config_snapshot(&self) -> ThreadConfigSnapshot {
        let state = self.state.lock().await;
        state.session_configuration.thread_config_snapshot()
    }

    pub(crate) async fn set_app_server_client_info(
        &self,
        app_server_client_name: Option<String>,
        app_server_client_version: Option<String>,
        mcp_elicitations_auto_deny: bool,
    ) -> ConstraintResult<()> {
        self.update_settings(SessionSettingsUpdate {
            app_server_client_name,
            app_server_client_version,
            ..Default::default()
        })
        .await?;
        self.services
            .mcp_runtime
            .set_elicitations_auto_deny(mcp_elicitations_auto_deny);
        Ok(())
    }

    pub(crate) async fn instruction_sources(&self) -> Vec<PathUri> {
        self.services
            .agents_md_manager
            .get_loaded()
            .await
            .as_ref()
            .map_or_else(Vec::new, |instructions| instructions.sources().collect())
    }

    pub(crate) async fn thread_environment_selections(&self) -> Vec<TurnEnvironmentSelection> {
        let state = self.state.lock().await;
        state
            .session_configuration
            .environment_selections()
            .to_vec()
    }

    pub(crate) async fn set_session_startup_prewarm(
        &self,
        startup_prewarm: SessionStartupPrewarmHandle,
    ) {
        let mut state = self.state.lock().await;
        state.set_session_startup_prewarm(startup_prewarm);
    }

    pub(crate) async fn take_session_startup_prewarm(&self) -> Option<SessionStartupPrewarmHandle> {
        let mut state = self.state.lock().await;
        state.take_session_startup_prewarm()
    }

    pub(crate) async fn get_config(&self) -> std::sync::Arc<Config> {
        let state = self.state.lock().await;
        state
            .session_configuration
            .original_config_do_not_use
            .clone()
    }

    pub(crate) async fn user_instructions(&self) -> Option<codex_extension_api::UserInstructions> {
        self.services.agents_md_manager.user_instructions()
    }

    pub(crate) async fn provider(&self) -> ModelProviderInfo {
        let state = self.state.lock().await;
        state.session_configuration.provider.clone()
    }

    /// The session's configured definition of provider `id`, user overrides
    /// such as `billing` included.
    pub(crate) async fn configured_model_provider(
        &self,
        id: &str,
    ) -> Option<codex_model_provider_info::ModelProviderInfo> {
        let state = self.state.lock().await;
        state
            .session_configuration
            .original_config_do_not_use
            .model_providers
            .get(id)
            .cloned()
    }

    /// Where this session's paid requests are recorded (PF-60-S05, PF-60-S04,
    /// PF-60-S06).
    ///
    /// A persisted session records under its own thread. A session with no
    /// thread of its own records under the conversation it works for, in that
    /// conversation's ledger, with a turn label saying what it is:
    /// - a guardian reviewer (the reusable trunk and its ephemeral forks): the
    ///   conversation it reviews for, as `review:`;
    /// - a memory consolidation agent: the conversation that started it, as
    ///   `consolidation:` (bound when the session is built, so its first
    ///   request, a startup prewarm included, already has its owner);
    /// - a sub-agent (a spawned agent, a `/review` task): the conversation it
    ///   works for, as `agent:` or `code-review:`. A `/review` task records
    ///   there even though it is persisted as a thread of its own (#898): that
    ///   thread names no parent the ledger can resolve, so its requests would
    ///   otherwise be counted under no conversation;
    /// - an ephemeral fork: the conversation it was forked from, as `side:`
    ///   for a TUI `/side` conversation and `fork:` for any other.
    ///
    /// When the conversation worked for is not persisted either (a guardian
    /// review or a sub-agent inside `/side`), its own owner is used and the
    /// labels nest (`side:review:`, `side:agent:`), so the work is counted
    /// once under the persisted conversation at the root.
    ///
    /// `Err` says why there is none: a session that is not collected by design
    /// (`exec --ephemeral`, or work for a conversation that is itself not
    /// collected), or a ledger that could not be reached.
    pub(crate) async fn accounting_owner(
        &self,
    ) -> Result<crate::accounting::AccountingOwner, crate::accounting::Uncollected> {
        self.accounting_owner_at(/*depth*/ 0).await
    }

    async fn accounting_owner_at(
        &self,
        depth: usize,
    ) -> Result<crate::accounting::AccountingOwner, crate::accounting::Uncollected> {
        use crate::accounting::Uncollected;
        use codex_protocol::protocol::SubAgentSource;
        let (guardian_parent, code_review_parent, worked_for, forked_from) = {
            let state = self.state.lock().await;
            let configuration = &state.session_configuration;
            let guardian_parent = match &configuration.session_source {
                SessionSource::SubAgent(SubAgentSource::Other(name))
                    if name == crate::guardian::GUARDIAN_REVIEWER_NAME =>
                {
                    Some(configuration.parent_thread_id)
                }
                _ => None,
            };
            let code_review_parent = match &configuration.session_source {
                SessionSource::SubAgent(SubAgentSource::Review) => configuration.parent_thread_id,
                _ => None,
            };
            let worked_for = match &configuration.session_source {
                SessionSource::SubAgent(SubAgentSource::ThreadSpawn {
                    parent_thread_id, ..
                }) => Some((*parent_thread_id, "agent:")),
                SessionSource::SubAgent(
                    SubAgentSource::Compact
                    | SubAgentSource::MemoryConsolidation
                    | SubAgentSource::Other(_),
                ) => configuration
                    .parent_thread_id
                    .map(|parent| (parent, "agent:")),
                _ => None,
            };
            let forked_from = configuration.forked_from_thread_id.map(|parent| {
                let side = matches!(
                    &configuration.thread_source,
                    Some(ThreadSource::Feature(feature))
                        if feature == ThreadSource::SIDE_CONVERSATION
                );
                (parent, if side { "side:" } else { "fork:" })
            });
            (guardian_parent, code_review_parent, worked_for, forked_from)
        };
        if let Some(parent) = guardian_parent {
            let parent = parent.ok_or(Uncollected::Excluded("guardian_without_parent"))?;
            return self
                .accounting_owner_elsewhere(parent, "review:", depth)
                .await;
        }
        if let Some(parent) = code_review_parent {
            return self
                .accounting_owner_elsewhere(parent, "code-review:", depth)
                .await;
        }
        if self.live_thread().is_some() {
            let db = self
                .state_db()
                .ok_or(Uncollected::Failed("no state database"))?;
            return Ok(crate::accounting::AccountingOwner {
                thread: self.thread_id,
                db,
                label_prefix: String::new(),
            });
        }
        if let Some(started_by) = self
            .services
            .thread_extension_data
            .get::<crate::accounting::StartedBy>()
        {
            return self
                .accounting_owner_elsewhere(started_by.0, "consolidation:", depth)
                .await;
        }
        if let Some((parent, label)) = worked_for.or(forked_from) {
            return self.accounting_owner_elsewhere(parent, label, depth).await;
        }
        Err(Uncollected::Excluded("ephemeral_session"))
    }

    /// `parent`'s ledger, for a session with no thread of its own that works
    /// for `parent`. Such a session has no database of its own; it shares the
    /// parent's thread store, and so the parent's ledger. A parent that is not
    /// persisted either is followed, while it is live, to its own owner.
    async fn accounting_owner_elsewhere(
        &self,
        parent: ThreadId,
        label: &'static str,
        depth: usize,
    ) -> Result<crate::accounting::AccountingOwner, crate::accounting::Uncollected> {
        use crate::accounting::Uncollected;
        /// Nested temporary work deeper than this is not followed further.
        const MAX_DEPTH: usize = 4;
        let db = match self.state_db() {
            Some(db) => Some(db),
            None => match self
                .services
                .thread_store
                .as_any()
                .downcast_ref::<codex_thread_store::LocalThreadStore>()
            {
                Some(store) => store.state_db().await,
                None => None,
            },
        }
        .ok_or(Uncollected::Failed("no state database"))?;
        match db.get_thread(parent).await {
            Ok(Some(_)) => Ok(crate::accounting::AccountingOwner {
                thread: parent,
                db,
                label_prefix: label.to_string(),
            }),
            // Work for a conversation that is itself not persisted (a side
            // conversation, a sub-agent of one) records where that
            // conversation records; work for one that is not collected at all
            // (exec --ephemeral) has no thread to record under.
            Ok(None) => {
                let parent_session = if depth < MAX_DEPTH && parent != self.thread_id {
                    self.services.agent_control.live_session(parent).await
                } else {
                    None
                };
                let Some(parent_session) = parent_session else {
                    return Err(Uncollected::Excluded("parent_not_persisted"));
                };
                match Box::pin(parent_session.accounting_owner_at(depth + 1)).await {
                    Ok(owner) => {
                        // A saved conversation whose thread row is not
                        // written yet (work started before its first saved
                        // item, such as `/review` racing the item that opens
                        // it) records under its own thread; save it now, as
                        // its own first request would, so the ledger has the
                        // thread to record under (#898).
                        if owner.thread == parent {
                            parent_session
                                .try_ensure_rollout_materialized()
                                .await
                                .map_err(|_| Uncollected::Failed("parent thread not saved"))?;
                        }
                        Ok(crate::accounting::AccountingOwner {
                            label_prefix: format!("{}{label}", owner.label_prefix),
                            ..owner
                        })
                    }
                    Err(Uncollected::Excluded(_)) => {
                        Err(Uncollected::Excluded("parent_not_persisted"))
                    }
                    Err(error) => Err(error),
                }
            }
            Err(_) => Err(Uncollected::Failed("parent thread unreadable")),
        }
    }

    /// The accounting mode and provider identity in force right now.
    ///
    /// Read live rather than snapshotted: a session's provider can change, and
    /// a client that records against the identity it had at startup would
    /// attribute a request to a provider it no longer uses.
    pub(crate) async fn accounting_binding(&self) -> (crate::config::AccountingMode, String) {
        let state = self.state.lock().await;
        let config = &state.session_configuration.original_config_do_not_use;
        (config.accounting.clone(), config.model_provider_id.clone())
    }

    pub(crate) async fn refresh_runtime_config(&self, next_config: Config) {
        // Refresh only the user layer from the incoming snapshot. Preserve thread-local
        // layers such as request/session overrides that were present when this session
        // was created.
        let notify_config_contributors = !self.services.extensions.config_contributors().is_empty();
        let (previous_config, new_config, config) = {
            let mut state = self.state.lock().await;
            let previous_config = notify_config_contributors
                .then(|| Self::build_effective_session_config(&state.session_configuration));
            let mut config = (*state.session_configuration.original_config_do_not_use).clone();
            config.config_layer_stack = config
                .config_layer_stack
                .with_user_layer_from(&next_config.config_layer_stack);
            config.tool_suggest =
                resolve_tool_suggest_config_from_layer_stack(&config.config_layer_stack);
            config.mcp_servers = next_config.mcp_servers.clone();
            config.mcp_oauth_credentials_store_mode = next_config.mcp_oauth_credentials_store_mode;
            if let Err(err) = config.features.set_enabled(
                Feature::SecretAuthStorage,
                next_config.features.enabled(Feature::SecretAuthStorage),
            ) {
                warn!("failed to refresh MCP auth storage config: {err}");
            }
            let config = Arc::new(config);
            state.session_configuration.original_config_do_not_use = Arc::clone(&config);
            self.mark_mcp_runtime_dirty();
            let new_config = notify_config_contributors
                .then(|| Self::build_effective_session_config(&state.session_configuration));
            (previous_config, new_config, config)
        };
        self.emit_config_changed_contributors(previous_config.as_ref(), new_config.as_ref());
        self.schedule_mcp_prewarm();
        let environments = self.services.turn_environments.snapshot().await;
        let hooks = build_hooks_for_config(
            config.as_ref(),
            self.services.plugins_manager.as_ref(),
            environments.single_local_environment(),
        )
        .await;

        let state = self.state.lock().await;
        // A newer refresh may have updated the config while this hook build was in flight.
        // Only publish hooks derived from the current config snapshot.
        if Arc::ptr_eq(
            &state.session_configuration.original_config_do_not_use,
            &config,
        ) {
            self.services.hooks.store(Arc::new(hooks));
        }
    }

    pub(crate) async fn refresh_mcp_config(&self, next_config: Config) {
        let mut state = self.state.lock().await;
        let mut config = (*state.session_configuration.original_config_do_not_use).clone();
        config.config_layer_stack = next_config
            .config_layer_stack
            .with_user_layer_from(&config.config_layer_stack);
        config.mcp_servers = next_config.mcp_servers;
        config.mcp_oauth_credentials_store_mode = next_config.mcp_oauth_credentials_store_mode;
        if let Err(err) = config.features.set_enabled(
            Feature::SecretAuthStorage,
            next_config.features.enabled(Feature::SecretAuthStorage),
        ) {
            warn!("failed to refresh MCP auth storage config: {err}");
        }
        state.session_configuration.original_config_do_not_use = Arc::new(config);
        self.mark_mcp_runtime_dirty();
        drop(state);
        self.schedule_mcp_prewarm();
    }

    fn emit_config_changed_contributors(
        &self,
        previous_config: Option<&Config>,
        new_config: Option<&Config>,
    ) {
        let (Some(previous_config), Some(new_config)) = (previous_config, new_config) else {
            return;
        };
        if previous_config == new_config {
            return;
        }
        for contributor in self.services.extensions.config_contributors() {
            contributor.on_config_changed(
                &self.services.session_extension_data,
                &self.services.thread_extension_data,
                previous_config,
                new_config,
            );
        }
    }

    pub(crate) async fn reload_user_config_layer(&self) {
        // Refresh layer-backed runtime state for an existing session, including enabled plugin,
        // skill, and hook state. Derived config fields such as feature gates and legacy notify
        // settings remain session-static.
        //
        // Prefer `refresh_runtime_config()` when the host can already provide a materialized
        // config snapshot. This file-based path exists for legacy local reload flows.
        let config_toml_paths = {
            let state = self.state.lock().await;
            let config = &state.session_configuration.original_config_do_not_use;
            let user_config_paths = config
                .config_layer_stack
                .get_user_layers(
                    ConfigLayerStackOrdering::LowestPrecedenceFirst,
                    /*include_disabled*/ true,
                )
                .into_iter()
                .filter_map(|layer| match &layer.name {
                    ConfigLayerSource::User { file, .. } => Some(file.clone()),
                    _ => None,
                })
                .collect::<Vec<_>>();
            if user_config_paths.is_empty() {
                vec![
                    state
                        .session_configuration
                        .codex_home
                        .join(CONFIG_TOML_FILE),
                ]
            } else {
                user_config_paths
            }
        };

        let mut reloaded_user_configs = Vec::with_capacity(config_toml_paths.len());
        for config_toml_path in config_toml_paths {
            let user_config = match std::fs::read_to_string(&config_toml_path) {
                Ok(contents) => match toml::from_str::<toml::Value>(&contents) {
                    Ok(config) => config,
                    Err(err) => {
                        warn!("failed to parse user config while reloading layer: {err}");
                        return;
                    }
                },
                Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
                    toml::Value::Table(Default::default())
                }
                Err(err) => {
                    warn!("failed to read user config while reloading layer: {err}");
                    return;
                }
            };
            reloaded_user_configs.push((config_toml_path, user_config));
        }

        let next_config = {
            let state = self.state.lock().await;
            let mut config = (*state.session_configuration.original_config_do_not_use).clone();
            for (config_toml_path, user_config) in reloaded_user_configs {
                let config_layer_stack = match config
                    .config_layer_stack
                    .with_user_config(&config_toml_path, user_config)
                {
                    Ok(config_layer_stack) => config_layer_stack,
                    Err(err) => {
                        warn!("failed to validate user config while reloading layer: {err}");
                        return;
                    }
                };
                config.config_layer_stack = config_layer_stack;
            }
            config.tool_suggest =
                resolve_tool_suggest_config_from_layer_stack(&config.config_layer_stack);
            config
        };
        self.services.skills_service.clear_cache();
        self.services.plugins_manager.clear_cache();
        self.refresh_runtime_config(next_config).await;
    }

    /// Record a terminal CodexErr before the app-server completion notification is reduced.
    pub(crate) fn track_turn_codex_error(&self, turn_context: &TurnContext, error: &CodexErr) {
        self.services
            .analytics_events_client
            .track_turn_codex_error(TurnCodexErrorFact::from_codex_err(
                self.thread_id.to_string(),
                turn_context.sub_id.clone(),
                error,
            ));
    }

    /// Persist the event to rollout and send it to clients.
    pub(crate) async fn send_event(&self, turn_context: &TurnContext, msg: EventMsg) {
        // PF-28-S01: gate before traces, rollout, realtime mirroring and clients.
        for msg in crate::security::disclosure_gate::gate_event(&turn_context.sub_id, msg) {
            self.send_gated_event(turn_context, msg).await;
        }
    }

    async fn send_gated_event(&self, turn_context: &TurnContext, msg: EventMsg) {
        let legacy_source = msg.clone();
        if let EventMsg::Error(error) = &legacy_source
            && error
                .codex_error_info
                .as_ref()
                .is_some_and(CodexErrorInfo::affects_turn_status)
        {
            turn_context
                .terminal_error
                .lock()
                .await
                .replace(error.clone());
        }
        self.services
            .rollout_thread_trace
            .record_codex_turn_event(&turn_context.sub_id, &legacy_source);
        self.services
            .rollout_thread_trace
            .record_tool_call_event(turn_context.sub_id.clone(), &legacy_source);
        let event = Event {
            id: turn_context.sub_id.clone(),
            msg,
        };
        self.send_event_raw(event).await;
        self.maybe_notify_parent_of_terminal_turn(turn_context, &legacy_source)
            .await;
        self.maybe_mirror_event_text_to_realtime(&legacy_source)
            .await;
        self.maybe_clear_realtime_handoff_for_event(&legacy_source)
            .await;

        let show_raw_agent_reasoning = self.show_raw_agent_reasoning();
        for legacy in legacy_source.as_legacy_events(show_raw_agent_reasoning) {
            self.services
                .rollout_thread_trace
                .record_tool_call_event(turn_context.sub_id.clone(), &legacy);
            let legacy_event = Event {
                id: turn_context.sub_id.clone(),
                msg: legacy,
            };
            self.send_event_raw(legacy_event).await;
        }
    }

    /// Forwards terminal turn events from spawned MultiAgentV2 children to their direct parent.
    async fn maybe_notify_parent_of_terminal_turn(
        &self,
        turn_context: &TurnContext,
        msg: &EventMsg,
    ) {
        if turn_context.multi_agent_version != MultiAgentVersion::V2 {
            return;
        }

        if !matches!(msg, EventMsg::TurnComplete(_) | EventMsg::TurnAborted(_)) {
            return;
        }

        if !turn_context
            .parent_completion_expected
            .load(std::sync::atomic::Ordering::Acquire)
        {
            return;
        }

        let SessionSource::SubAgent(SubAgentSource::ThreadSpawn {
            parent_thread_id,
            agent_path: Some(child_agent_path),
            ..
        }) = &turn_context.session_source
        else {
            return;
        };

        let status = match turn_context.terminal_error.lock().await.take() {
            Some(error) => {
                let status = AgentStatus::Errored(error.message);
                self.agent_status.send_replace(status.clone());
                status
            }
            None => {
                let Some(status) = agent_status_from_event(msg) else {
                    return;
                };
                status
            }
        };
        if !is_final(&status) {
            return;
        }

        self.forward_child_completion_to_parent(
            turn_context,
            *parent_thread_id,
            child_agent_path,
            status,
        )
        .await;
    }

    /// Sends the standard completion envelope from a spawned MultiAgentV2 child to its parent.
    async fn forward_child_completion_to_parent(
        &self,
        turn_context: &TurnContext,
        parent_thread_id: ThreadId,
        child_agent_path: &codex_protocol::AgentPath,
        status: AgentStatus,
    ) {
        let Some(parent_agent_path) = child_agent_path
            .as_str()
            .rsplit_once('/')
            .and_then(|(parent, _)| codex_protocol::AgentPath::try_from(parent).ok())
        else {
            return;
        };

        let Some(message) = format_inter_agent_completion_message(
            parent_agent_path.clone(),
            child_agent_path.clone(),
            &status,
        ) else {
            return;
        };
        // `communication` owns the message. Keep a second copy only when the
        // recorder will actually need it after parent delivery succeeds.
        let trace_message = self
            .services
            .rollout_thread_trace
            .is_enabled()
            .then(|| message.clone());
        let trigger_turn = self
            .services
            .agent_control
            .auto_processes_terminal_results(parent_thread_id)
            .await;
        let mut communication = InterAgentCommunication::new(
            child_agent_path.clone(),
            parent_agent_path,
            Vec::new(),
            message,
            trigger_turn,
        );
        communication.kind = Some(codex_protocol::protocol::AgentMessageKind::TerminalResult);
        communication.message_id = Some(format!(
            "completion:{}:{}",
            self.thread_id, turn_context.sub_id
        ));
        let context =
            AgentCommunicationContext::new(AgentCommunicationKind::Result, self.thread_id);
        if let Err(err) = self
            .services
            .agent_control
            .send_inter_agent_communication(
                parent_thread_id,
                communication,
                context,
                /*parent_turn_id*/ None,
            )
            .await
        {
            debug!("failed to notify parent thread {parent_thread_id}: {err}");
            return;
        }
        if let Some(message) = trace_message {
            self.services
                .rollout_thread_trace
                .record_agent_result_interaction(
                    turn_context.sub_id.as_str(),
                    parent_thread_id,
                    &AgentResultTracePayload {
                        child_agent_path: child_agent_path.as_str(),
                        message: &message,
                        status: &status,
                    },
                );
        }
    }

    async fn maybe_mirror_event_text_to_realtime(&self, msg: &EventMsg) {
        if self.conversation.running_state().await.is_none() {
            return;
        }
        match msg {
            EventMsg::ItemStarted(event) => {
                if let TurnItem::AgentMessage(item) = &event.item {
                    self.conversation
                        .register_handoff_stream_item(
                            item.id.clone(),
                            item.phase.clone(),
                            agent_message_text(item),
                        )
                        .await;
                }
                return;
            }
            EventMsg::AgentMessageContentDelta(event) => {
                if let Err(err) = self
                    .conversation
                    .stream_handoff_delta(&event.item_id, event.delta.clone())
                    .await
                {
                    debug!("failed to stream event text to realtime conversation: {err}");
                }
                return;
            }
            EventMsg::ItemCompleted(event) => {
                if let TurnItem::AgentMessage(item) = &event.item
                    && self.conversation.finish_handoff_stream_item(&item.id).await
                {
                    return;
                }
            }
            _ => {}
        }
        let Some((text, phase)) = realtime_text_for_event(msg) else {
            return;
        };
        if let Err(err) = self.conversation.handoff_out(text, phase).await {
            debug!("failed to mirror event text to realtime conversation: {err}");
        }
    }

    async fn maybe_clear_realtime_handoff_for_event(&self, msg: &EventMsg) {
        if !matches!(msg, EventMsg::TurnComplete(_)) {
            return;
        }
        if let Err(err) = self.conversation.handoff_complete().await {
            debug!("failed to finalize realtime handoff output: {err}");
        }
        self.conversation.clear_active_handoff().await;
    }

    pub(crate) async fn send_event_raw(&self, event: Event) {
        self.send_event_raw_with_persistence(event, /*persist*/ true)
            .await;
    }

    /// Delivers an event to the client without writing it to the rollout: a
    /// notice about this session's own start that a later reopen must not
    /// replay as if it had happened again.
    pub(crate) async fn send_event_raw_unpersisted(&self, event: Event) {
        self.send_event_raw_with_persistence(event, /*persist*/ false)
            .await;
    }

    /// Delivers an event without creating a local rollout for a thread that has not materialized.
    pub(crate) async fn send_event_raw_without_materializing_rollout(&self, event: Event) {
        let persist = match self.current_rollout_path().await {
            Ok(Some(path)) => codex_rollout::existing_rollout_path(&path).await.is_some(),
            Ok(None) => true,
            Err(err) => {
                warn!("failed to check whether thread persistence is materialized: {err}");
                true
            }
        };
        self.send_event_raw_with_persistence(event, persist).await;
    }

    async fn send_event_raw_with_persistence(&self, event: Event, persist: bool) {
        let mut event = event;
        match crate::security::disclosure_gate::gate_presented(event.msg) {
            Some(msg) => event.msg = msg,
            None => return,
        }
        // Persist the event into rollout storage; the store applies its persistence policy.
        if persist {
            let rollout_items = vec![RolloutItem::EventMsg(event.msg.clone())];
            self.persist_rollout_items(&rollout_items).await;
        }
        self.services
            .rollout_thread_trace
            .record_protocol_event(&event.msg);
        self.deliver_event_raw(event).await;
    }

    /// Sets the agent status a later event would set, for tests that need a
    /// status (`PendingInit`, say) a real turn would leave at any moment.
    #[cfg(test)]
    pub(crate) fn set_agent_status_for_test(&self, status: AgentStatus) {
        self.agent_status.send_replace(status);
    }

    async fn deliver_event_raw(&self, event: Event) {
        // Record the last known agent status.
        if let Some(status) = agent_status_from_event(&event.msg) {
            self.agent_status.send_replace(status);
        }
        if let Err(e) = self.tx_event.send(event).await {
            debug!("dropping event because channel is closed: {e}");
        }
    }

    pub(crate) async fn emit_turn_item_started(&self, turn_context: &TurnContext, item: &TurnItem) {
        let started_at_ms = turn_context
            .turn_timing_state
            .record_item_started(item.id(), now_unix_timestamp_ms())
            .await;
        self.send_event(
            turn_context,
            EventMsg::ItemStarted(ItemStartedEvent {
                thread_id: self.thread_id,
                turn_id: turn_context.sub_id.clone(),
                item: item.clone(),
                started_at_ms,
            }),
        )
        .await;
    }

    pub(crate) async fn emit_turn_item_completed(
        &self,
        turn_context: &TurnContext,
        item: TurnItem,
    ) {
        record_turn_ttfm_metric(turn_context, &item).await;
        let completed_at_ms = now_unix_timestamp_ms();
        let item_id = item.id();
        let started_at_ms = turn_context
            .turn_timing_state
            .take_item_started(&item_id)
            .await
            .unwrap_or_else(|| {
                warn!(
                    thread_id = %self.thread_id,
                    turn_id = %turn_context.sub_id,
                    item_id = %item_id,
                    "item completed without a recorded start timestamp"
                );
                completed_at_ms
            });
        self.send_event(
            turn_context,
            EventMsg::ItemCompleted(ItemCompletedEvent {
                thread_id: self.thread_id,
                turn_id: turn_context.sub_id.clone(),
                item,
                started_at_ms: Some(started_at_ms),
                completed_at_ms,
            }),
        )
        .await;
    }

    /// Adds an execpolicy amendment to both the in-memory and on-disk policies so future
    /// commands can use the newly approved prefix. #626: one approved while
    /// Aggressive is active is recorded as such, so it applies under
    /// Aggressive too (the others are suspended there).
    /// Saves a "don't ask again" answer. #945: `shown_under` is the policy
    /// marker its prompt was shown under; when a security transition was
    /// committed since, the answer was for a stale prompt and nothing is
    /// saved (`Ok(None)`).
    pub(crate) async fn persist_execpolicy_amendment(
        &self,
        amendment: &ExecPolicyAmendment,
        shown_under: Option<(u64, u64)>,
    ) -> Result<Option<crate::exec_policy::AmendmentScope>, ExecPolicyUpdateError> {
        let codex_home = self
            .state
            .lock()
            .await
            .session_configuration
            .codex_home()
            .clone();

        // Aggressive in force now (an unreadable live level counts as
        // Aggressive, as it is enforced): the person approved it under
        // Aggressive. Read before the marker: the marker only moves forward,
        // so an unchanged marker means this was the level at the prompt.
        let approved_under = if self.services.model_client().live_level_is_aggressive() {
            crate::exec_policy::ApprovedUnder::Aggressive
        } else {
            crate::exec_policy::ApprovedUnder::LessStrict
        };
        if self.services.transition_since(shown_under) {
            return Ok(None);
        }
        self.services
            .exec_policy
            .append_amendment_and_update(&codex_home, amendment, approved_under)
            .await
            .map(Some)
    }

    /// #945: the policy marker the open approval prompt `approval_id` was
    /// shown under; `None` when no such prompt is open.
    #[expect(
        clippy::await_holding_invalid_type,
        reason = "active turn reads must stay consistent with the matching turn state"
    )]
    pub(crate) async fn pending_approval_shown_under(
        &self,
        approval_id: &str,
    ) -> Option<Option<(u64, u64)>> {
        let active = self.active_turn.lock().await;
        let active = active.as_ref()?;
        let ts = active.turn_state.lock().await;
        ts.pending_approval_shown_under(approval_id)
    }

    pub(crate) async fn turn_context_for_sub_id(&self, sub_id: &str) -> Option<Arc<TurnContext>> {
        let active = self.active_turn.lock().await;
        active
            .as_ref()
            .and_then(|turn| turn.task.as_ref())
            .filter(|task| task.turn_context.sub_id == sub_id)
            .map(|task| Arc::clone(&task.turn_context))
    }

    async fn active_turn_context_and_cancellation_token(
        &self,
    ) -> Option<(Arc<TurnContext>, CancellationToken)> {
        let active = self.active_turn.lock().await;
        let task = active.as_ref()?.task.as_ref()?;
        Some((
            Arc::clone(&task.turn_context),
            task.cancellation_token.child_token(),
        ))
    }

    pub(crate) async fn record_execpolicy_amendment_message(
        &self,
        sub_id: &str,
        amendment: &ExecPolicyAmendment,
    ) {
        let Some(prefixes) = format_allow_prefixes(vec![amendment.command.clone()]) else {
            warn!("execpolicy amendment for {sub_id} had no command prefix");
            return;
        };
        let message: ResponseItem =
            ContextualUserFragment::into(ApprovedCommandPrefixSaved::new(prefixes));
        let turn_context = self.turn_context_for_sub_id(sub_id).await;
        self.inject_no_new_turn(vec![message], turn_context.as_deref())
            .await;
    }

    /// `shown_under` (#945): the policy marker an allowing answer's prompt
    /// was shown under; when a security transition was committed since,
    /// nothing is changed.
    pub(crate) async fn persist_network_policy_amendment(
        &self,
        amendment: &NetworkPolicyAmendment,
        network_approval_context: &NetworkApprovalContext,
        shown_under: Option<Option<(u64, u64)>>,
        on_policy_applied: impl FnOnce() + Send,
    ) -> anyhow::Result<()> {
        let _refresh_guard = self
            .managed_network_proxy_refresh_lock
            .acquire()
            .await
            .map_err(|_| anyhow::anyhow!("managed network proxy refresh semaphore closed"))?;
        // D52 (#1001): one allowed while Aggressive is in force applies under
        // Aggressive too (the others are suspended there). An unreadable live
        // level counts as Aggressive, as it is enforced. Read before the
        // marker: the marker only moves forward, so an unchanged marker means
        // this was the level at the prompt (as for command rules, #945).
        let approved_under = if self.services.model_client().live_level_is_aggressive() {
            crate::exec_policy::ApprovedUnder::Aggressive
        } else {
            crate::exec_policy::ApprovedUnder::LessStrict
        };
        if let Some(shown_under) = shown_under
            && self.services.transition_since(shown_under)
        {
            anyhow::bail!("the security level changed while network access was being approved");
        }
        let host =
            Self::validated_network_policy_amendment_host(amendment, network_approval_context)?;
        let codex_home = self
            .state
            .lock()
            .await
            .session_configuration
            .codex_home()
            .clone();
        let execpolicy_amendment =
            execpolicy_network_rule_amendment(amendment, network_approval_context, &host);
        let mut on_policy_applied = Some(on_policy_applied);

        if let Some(started_network_proxy) = self.services.network_proxy.load_full() {
            let proxy = started_network_proxy.proxy();
            match amendment.action {
                NetworkPolicyRuleAction::Allow => proxy
                    .add_allowed_domain(&host)
                    .await
                    .map_err(|err| anyhow::anyhow!("failed to update runtime allowlist: {err}"))?,
                NetworkPolicyRuleAction::Deny => proxy
                    .add_denied_domain(&host)
                    .await
                    .map_err(|err| anyhow::anyhow!("failed to update runtime denylist: {err}"))?,
            }
            // Active enforcement changed successfully. Notify the owner before
            // the next fallible await so cancellation cannot contradict it.
            if let Some(on_policy_applied) = on_policy_applied.take() {
                on_policy_applied();
            }
        }

        self.services
            .exec_policy
            .append_network_rule_and_update(
                &codex_home,
                &host,
                execpolicy_amendment.protocol,
                execpolicy_amendment.decision,
                Some(execpolicy_amendment.justification),
                approved_under,
            )
            .await
            .map_err(|err| {
                anyhow::anyhow!("failed to persist network policy amendment to execpolicy: {err}")
            })?;

        // Without a running proxy, persistence is the first effective policy change.
        if let Some(on_policy_applied) = on_policy_applied {
            on_policy_applied();
        }

        Ok(())
    }

    fn validated_network_policy_amendment_host(
        amendment: &NetworkPolicyAmendment,
        network_approval_context: &NetworkApprovalContext,
    ) -> anyhow::Result<String> {
        let approved_host = normalize_host(&network_approval_context.host);
        let amendment_host = normalize_host(&amendment.host);
        if amendment_host != approved_host {
            return Err(anyhow::anyhow!(
                "network policy amendment host '{}' does not match approved host '{}'",
                amendment.host,
                network_approval_context.host
            ));
        }
        Ok(approved_host)
    }

    pub(crate) async fn record_network_policy_amendment_message(
        &self,
        sub_id: &str,
        amendment: &NetworkPolicyAmendment,
    ) {
        let message: ResponseItem = ContextualUserFragment::into(NetworkRuleSaved::new(amendment));
        let turn_context = self.turn_context_for_sub_id(sub_id).await;
        self.inject_no_new_turn(vec![message], turn_context.as_deref())
            .await;
    }

    /// Emit an exec approval request event and await the user's decision.
    ///
    /// The request is keyed by `call_id` + `approval_id` so matching responses
    /// are delivered to the correct in-flight turn. If the pending approval is
    /// cleared before a response arrives, treat it as an abort so interrupted
    /// turns do not continue on a synthetic denial.
    ///
    /// Note that if `available_decisions` is `None`, then the other fields will
    /// be used to derive the available decisions via
    /// [ExecApprovalRequestEvent::default_available_decisions].
    #[allow(clippy::too_many_arguments)]
    #[expect(
        clippy::await_holding_invalid_type,
        reason = "active turn checks and turn state updates must remain atomic"
    )]
    pub async fn request_command_approval(
        &self,
        turn_context: &TurnContext,
        call_id: String,
        approval_id: Option<String>,
        environment_id: Option<String>,
        command: Vec<String>,
        cwd: AbsolutePathBuf,
        reason: Option<String>,
        network_approval_context: Option<NetworkApprovalContext>,
        proposed_execpolicy_amendment: Option<ExecPolicyAmendment>,
        additional_permissions: Option<AdditionalPermissionProfile>,
        available_decisions: Option<Vec<ReviewDecision>>,
        plugin_attribution_override: Option<PluginCommandAttribution>,
    ) -> ReviewDecision {
        let _elicitation = self.services.elicitations.register();
        //  command-level approvals use `call_id`.
        // `approval_id` is only present for subcommand callbacks (execve intercept)
        let effective_approval_id = approval_id.clone().unwrap_or_else(|| call_id.clone());
        // Add the tx_approve callback to the map before sending the request.
        let (tx_approve, rx_approve) = oneshot::channel();
        let prev_entry = {
            let mut active = self.active_turn.lock().await;
            match active.as_mut() {
                Some(at) => {
                    let mut ts = at.turn_state.lock().await;
                    ts.insert_pending_approval(
                        effective_approval_id.clone(),
                        tx_approve,
                        self.services.authority_marker(),
                    )
                }
                None => None,
            }
        };
        if prev_entry.is_some() {
            warn!("Overwriting existing pending approval for call_id: {effective_approval_id}");
        }

        let parsed_cmd = parse_command(&command);
        let proposed_network_policy_amendments = network_approval_context.as_ref().map(|context| {
            vec![
                NetworkPolicyAmendment {
                    host: context.host.clone(),
                    action: NetworkPolicyRuleAction::Allow,
                },
                NetworkPolicyAmendment {
                    host: context.host.clone(),
                    action: NetworkPolicyRuleAction::Deny,
                },
            ]
        });
        let available_decisions = available_decisions.unwrap_or_else(|| {
            ExecApprovalRequestEvent::default_available_decisions(
                network_approval_context.as_ref(),
                proposed_execpolicy_amendment.as_ref(),
                proposed_network_policy_amendments.as_deref(),
                additional_permissions.as_ref(),
            )
        });
        let plugin_attribution = plugin_attribution_override
            .or_else(|| turn_context.plugin_attribution_for_command(&command, &cwd));
        let (plugin_id, script_path) = plugin_attribution
            .as_ref()
            .map(PluginCommandAttribution::serialized_fields)
            .unzip();
        let withdrawn = ExecApprovalRequestWithdrawnEvent {
            call_id: call_id.clone(),
            approval_id: approval_id.clone(),
            turn_id: turn_context.sub_id.clone(),
        };
        let event = EventMsg::ExecApprovalRequest(ExecApprovalRequestEvent {
            call_id,
            plugin_id,
            script_path,
            approval_id,
            turn_id: turn_context.sub_id.clone(),
            environment_id,
            started_at_ms: now_unix_timestamp_ms(),
            command,
            cwd,
            reason,
            network_approval_context,
            proposed_execpolicy_amendment,
            proposed_network_policy_amendments,
            additional_permissions,
            available_decisions: Some(available_decisions),
            parsed_cmd,
        });
        // #722 (D279): a caller that stops waiting (a remote network request
        // timed out or its process ended) drops this future, and the action is
        // already refused. Tell clients to close the prompt.
        let withdraw_on_drop = WithdrawApprovalOnDrop {
            tx_event: self.tx_event.clone(),
            event: Some(withdrawn),
        };
        self.send_event(turn_context, event).await;
        let decision = rx_approve.await.unwrap_or(ReviewDecision::Abort);
        withdraw_on_drop.disarm();
        decision
    }

    #[expect(
        clippy::await_holding_invalid_type,
        reason = "active turn checks and turn state updates must remain atomic"
    )]
    pub async fn request_patch_approval(
        &self,
        turn_context: &TurnContext,
        call_id: String,
        changes: HashMap<PathBuf, FileChange>,
        reason: Option<String>,
        grant_root: Option<PathBuf>,
    ) -> ReviewDecision {
        let _elicitation = self.services.elicitations.register();
        // Add the tx_approve callback to the map before sending the request.
        let (tx_approve, rx_approve) = oneshot::channel();
        let approval_id = call_id.clone();
        let prev_entry = {
            let mut active = self.active_turn.lock().await;
            match active.as_mut() {
                Some(at) => {
                    let mut ts = at.turn_state.lock().await;
                    ts.insert_pending_approval(
                        approval_id.clone(),
                        tx_approve,
                        self.services.authority_marker(),
                    )
                }
                None => None,
            }
        };
        if prev_entry.is_some() {
            warn!("Overwriting existing pending approval for call_id: {approval_id}");
        }

        let event = EventMsg::ApplyPatchApprovalRequest(ApplyPatchApprovalRequestEvent {
            call_id,
            turn_id: turn_context.sub_id.clone(),
            started_at_ms: now_unix_timestamp_ms(),
            changes,
            reason,
            grant_root,
        });
        self.send_event(turn_context, event).await;
        rx_approve.await.unwrap_or(ReviewDecision::Abort)
    }

    pub(crate) async fn request_permissions_for_environment(
        self: &Arc<Self>,
        turn_context: &Arc<TurnContext>,
        call_id: String,
        args: RequestPermissionsArgs,
        environment: TurnEnvironmentSelection,
        cancellation_token: CancellationToken,
    ) -> Option<RequestPermissionsResponse> {
        self.request_permissions_explained(
            turn_context,
            call_id,
            args,
            environment,
            cancellation_token,
        )
        .await
        .unwrap_or_else(|_refusal| {
            Some(RequestPermissionsResponse {
                permissions: RequestPermissionProfile::default(),
                scope: PermissionGrantScope::Turn,
                strict_auto_review: false,
            })
        })
    }

    /// [`Self::request_permissions_for_environment`], or `Err` with why when
    /// the request was refused before anyone was asked (#584).
    #[expect(
        clippy::await_holding_invalid_type,
        reason = "active turn checks and turn state updates must remain atomic"
    )]
    pub(crate) async fn request_permissions_explained(
        self: &Arc<Self>,
        turn_context: &Arc<TurnContext>,
        call_id: String,
        args: RequestPermissionsArgs,
        environment: TurnEnvironmentSelection,
        cancellation_token: CancellationToken,
    ) -> Result<Option<RequestPermissionsResponse>, String> {
        match turn_context.as_ref().approval_policy.value() {
            AskForApproval::Never => {
                return Ok(Some(RequestPermissionsResponse {
                    permissions: RequestPermissionProfile::default(),
                    scope: PermissionGrantScope::Turn,
                    strict_auto_review: false,
                }));
            }
            AskForApproval::Granular(granular_config)
                if !granular_config.allows_request_permissions() =>
            {
                return Ok(Some(RequestPermissionsResponse {
                    permissions: RequestPermissionProfile::default(),
                    scope: PermissionGrantScope::Turn,
                    strict_auto_review: false,
                }));
            }
            AskForApproval::OnRequest
            | AskForApproval::UnlessTrusted
            | AskForApproval::Granular(_) => {}
        }

        let requested_permissions = args.permissions;
        // TODO(anp): Migrate request_permissions to support paths from foreign environments.
        let Ok(native_environment_cwd) = environment.cwd.to_abs_path() else {
            warn!(
                cwd = %environment.cwd,
                "request_permissions requires a cwd native to the Codex host"
            );
            return Ok(Some(RequestPermissionsResponse {
                permissions: RequestPermissionProfile::default(),
                scope: PermissionGrantScope::Turn,
                strict_auto_review: false,
            }));
        };
        let requested_permissions = tokio::select! {
            biased;
            _ = cancellation_token.cancelled() => return Ok(None),
            kept = self.grantable_request(
                turn_context.as_ref(),
                requested_permissions,
                &native_environment_cwd,
            ) => kept?,
        };

        if crate::guardian::routes_approval_to_guardian(turn_context.as_ref()) {
            let originating_turn_state = {
                let active = self.active_turn.lock().await;
                active.as_ref().map(|active| Arc::clone(&active.turn_state))
            };
            let review_id = crate::guardian::new_guardian_review_id();
            // #945: the policy the request is reviewed under.
            let shown_under = self.services.authority_marker();
            let session = Arc::clone(self);
            let turn = Arc::clone(turn_context);
            let request = crate::guardian::GuardianApprovalRequest::RequestPermissions {
                id: call_id,
                turn_id: turn_context.sub_id.clone(),
                reason: args.reason,
                permissions: requested_permissions.clone(),
            };
            let review_rx = crate::guardian::spawn_approval_request_review(
                session,
                turn,
                review_id,
                request,
                /*retry_reason*/ None,
                GuardianReviewOptions {
                    plugin_attribution_override: None,
                    approval_request_source:
                        codex_analytics::GuardianApprovalRequestSource::MainTurn,
                    external_cancel: Some(cancellation_token.clone()),
                },
            );
            let decision = tokio::select! {
                biased;
                _ = cancellation_token.cancelled() => return Ok(None),
                decision = review_rx => decision.unwrap_or_else(|_| {
                    ReviewDecision::denied("automatic approval review could not complete")
                }),
            };
            let response = match decision {
                ReviewDecision::Approved | ReviewDecision::ApprovedExecpolicyAmendment { .. } => {
                    RequestPermissionsResponse {
                        permissions: requested_permissions.clone(),
                        scope: PermissionGrantScope::Turn,
                        strict_auto_review: false,
                    }
                }
                ReviewDecision::ApprovedForSession => RequestPermissionsResponse {
                    permissions: requested_permissions.clone(),
                    scope: PermissionGrantScope::Session,
                    strict_auto_review: false,
                },
                ReviewDecision::NetworkPolicyAmendment {
                    network_policy_amendment,
                } => match network_policy_amendment.action {
                    NetworkPolicyRuleAction::Allow => RequestPermissionsResponse {
                        permissions: requested_permissions.clone(),
                        scope: PermissionGrantScope::Turn,
                        strict_auto_review: false,
                    },
                    NetworkPolicyRuleAction::Deny => RequestPermissionsResponse {
                        permissions: RequestPermissionProfile::default(),
                        scope: PermissionGrantScope::Turn,
                        strict_auto_review: false,
                    },
                },
                ReviewDecision::Abort
                | ReviewDecision::Denied { .. }
                | ReviewDecision::TimedOut => RequestPermissionsResponse {
                    permissions: RequestPermissionProfile::default(),
                    scope: PermissionGrantScope::Turn,
                    strict_auto_review: false,
                },
            };
            let response = self.unless_stale_grant(response, shown_under);
            let response = Self::normalize_request_permissions_response(
                requested_permissions,
                response,
                native_environment_cwd.as_path(),
            );
            let response = self
                .without_protected_grant(
                    response,
                    &native_environment_cwd,
                    Some(turn_context.as_ref()),
                )
                .await;
            self.record_granted_request_permissions_for_turn(
                &response,
                &environment.environment_id,
                originating_turn_state.as_ref(),
                shown_under,
            )
            .await;
            return Ok(Some(response));
        }

        let _elicitation = self.services.elicitations.register();
        let (tx_response, rx_response) = oneshot::channel();
        let prev_entry = {
            let mut active = self.active_turn.lock().await;
            match active.as_mut() {
                Some(at) => {
                    let mut ts = at.turn_state.lock().await;
                    ts.insert_pending_request_permissions(
                        call_id.clone(),
                        PendingRequestPermissions {
                            tx_response,
                            requested_permissions: requested_permissions.clone(),
                            environment: environment.clone(),
                            shown_under: self.services.authority_marker(),
                        },
                    )
                }
                None => None,
            }
        };
        if prev_entry.is_some() {
            warn!("Overwriting existing pending request_permissions for call_id: {call_id}");
        }

        let event = EventMsg::RequestPermissions(RequestPermissionsEvent {
            call_id: call_id.clone(),
            turn_id: turn_context.sub_id.clone(),
            environment_id: Some(environment.environment_id.clone()),
            started_at_ms: now_unix_timestamp_ms(),
            reason: args.reason,
            permissions: requested_permissions,
            cwd: Some(native_environment_cwd),
        });
        self.send_event(turn_context.as_ref(), event).await;
        Ok(tokio::select! {
            biased;
            _ = cancellation_token.cancelled() => {
                let mut active = self.active_turn.lock().await;
                if let Some(at) = active.as_mut() {
                    let mut ts = at.turn_state.lock().await;
                    let _ = ts.remove_pending_request_permissions(&call_id);
                }
                None
            }
            response = rx_response => response.ok(),
        })
    }

    pub(crate) async fn request_permissions_for_cwd(
        self: &Arc<Self>,
        turn_context: &Arc<TurnContext>,
        call_id: String,
        args: RequestPermissionsArgs,
        cwd: AbsolutePathBuf,
        cancellation_token: CancellationToken,
    ) -> Option<RequestPermissionsResponse> {
        let turn_environment = match args.environment_id.as_deref() {
            Some(environment_id) => turn_context
                .environments
                .turn_environments()
                .find(|environment| environment.environment_id == environment_id),
            None => turn_context.environments.primary(),
        };
        let Some(turn_environment) = turn_environment else {
            return Some(RequestPermissionsResponse {
                permissions: RequestPermissionProfile::default(),
                scope: PermissionGrantScope::Turn,
                strict_auto_review: false,
            });
        };
        let mut environment = turn_environment.selection();
        environment.cwd = PathUri::from_abs_path(&cwd);
        self.request_permissions_for_environment(
            turn_context,
            call_id,
            args,
            environment,
            cancellation_token,
        )
        .await
    }

    #[expect(
        clippy::await_holding_invalid_type,
        reason = "active turn checks and turn state updates must remain atomic"
    )]
    pub async fn request_user_input(
        &self,
        turn_context: &TurnContext,
        call_id: String,
        args: RequestUserInputArgs,
    ) -> Option<RequestUserInputResponse> {
        let _elicitation = self.services.elicitations.register();
        let sub_id = turn_context.sub_id.clone();
        let (tx_response, rx_response) = oneshot::channel();
        let event_id = sub_id.clone();
        let prev_entry = {
            let mut active = self.active_turn.lock().await;
            match active.as_mut() {
                Some(at) => {
                    let mut ts = at.turn_state.lock().await;
                    ts.insert_pending_user_input(sub_id, tx_response)
                }
                None => None,
            }
        };
        if prev_entry.is_some() {
            warn!("Overwriting existing pending user input for sub_id: {event_id}");
        }

        let event = EventMsg::RequestUserInput(RequestUserInputEvent {
            call_id,
            turn_id: turn_context.sub_id.clone(),
            questions: args.questions,
            auto_resolution_ms: args.auto_resolution_ms,
        });
        turn_context
            .turn_metadata_state
            .mark_user_input_requested_during_turn();
        self.send_event(turn_context, event).await;
        rx_response.await.ok()
    }

    #[expect(
        clippy::await_holding_invalid_type,
        reason = "active turn checks and turn state updates must remain atomic"
    )]
    pub async fn notify_user_input_response(
        &self,
        sub_id: &str,
        response: RequestUserInputResponse,
    ) {
        let entry = {
            let mut active = self.active_turn.lock().await;
            match active.as_mut() {
                Some(at) => {
                    let mut ts = at.turn_state.lock().await;
                    ts.remove_pending_user_input(sub_id)
                }
                None => None,
            }
        };
        match entry {
            Some(tx_response) => {
                tx_response.send(response).ok();
            }
            None => {
                warn!("No pending user input found for sub_id: {sub_id}");
            }
        }
    }

    #[expect(
        clippy::await_holding_invalid_type,
        reason = "active turn checks and turn state updates must remain atomic"
    )]
    pub async fn notify_request_permissions_response(
        &self,
        call_id: &str,
        response: RequestPermissionsResponse,
    ) {
        let (entry, originating_turn_state) = {
            let mut active = self.active_turn.lock().await;
            match active.as_mut() {
                Some(at) => {
                    let mut ts = at.turn_state.lock().await;
                    let entry = ts.remove_pending_request_permissions(call_id);
                    let originating_turn_state = entry.as_ref().map(|_| Arc::clone(&at.turn_state));
                    (entry, originating_turn_state)
                }
                None => (None, None),
            }
        };
        match entry {
            Some(entry) => {
                // TODO(anp): Migrate request_permissions to support paths from foreign environments.
                let response = match entry.environment.cwd.to_abs_path() {
                    Ok(native_environment_cwd) => {
                        let response = Self::normalize_request_permissions_response(
                            entry.requested_permissions,
                            response,
                            native_environment_cwd.as_path(),
                        );
                        self.without_protected_grant(
                            response,
                            &native_environment_cwd,
                            /*turn_context*/ None,
                        )
                        .await
                    }
                    Err(err) => {
                        warn!(
                            cwd = %entry.environment.cwd,
                            %err,
                            "request_permissions requires a cwd native to the Codex host"
                        );
                        RequestPermissionsResponse {
                            permissions: RequestPermissionProfile::default(),
                            scope: PermissionGrantScope::Turn,
                            strict_auto_review: false,
                        }
                    }
                };
                let response = self.unless_stale_grant(response, entry.shown_under);
                self.record_granted_request_permissions_for_turn(
                    &response,
                    &entry.environment.environment_id,
                    originating_turn_state.as_ref(),
                    entry.shown_under,
                )
                .await;
                entry.tx_response.send(response).ok();
            }
            None => {
                warn!("No pending request_permissions found for call_id: {call_id}");
            }
        }
    }

    /// #945: a grant answered after a security transition was for a stale
    /// request: nothing is granted (the model may ask again under the new
    /// level).
    fn unless_stale_grant(
        &self,
        response: RequestPermissionsResponse,
        shown_under: Option<(u64, u64)>,
    ) -> RequestPermissionsResponse {
        if response.permissions.is_empty() || !self.services.transition_since(shown_under) {
            return response;
        }
        warn!("request_permissions answered after a security transition; nothing granted");
        RequestPermissionsResponse {
            permissions: RequestPermissionProfile::default(),
            scope: PermissionGrantScope::Turn,
            strict_auto_review: false,
        }
    }

    fn normalize_request_permissions_response(
        requested_permissions: RequestPermissionProfile,
        response: RequestPermissionsResponse,
        cwd: &Path,
    ) -> RequestPermissionsResponse {
        if response.strict_auto_review && matches!(response.scope, PermissionGrantScope::Session) {
            return RequestPermissionsResponse {
                permissions: RequestPermissionProfile::default(),
                scope: PermissionGrantScope::Turn,
                strict_auto_review: false,
            };
        }

        if response.permissions.is_empty() {
            return response;
        }

        RequestPermissionsResponse {
            permissions: intersect_permission_profiles(
                requested_permissions.into(),
                response.permissions.into(),
                cwd,
            )
            .into(),
            scope: response.scope,
            strict_auto_review: response.strict_auto_review,
        }
    }

    /// #584: under Aggressive no answer grants a protected path or a
    /// read-only one (`ReadOnlyScope::for_grant`; #520: the project's
    /// `.codex` and `.agents` included), so those entries are left out before
    /// anyone is asked, and the person is told why. `Err` with why when
    /// nothing would be left to grant: then nobody is asked.
    async fn grantable_request(
        &self,
        turn_context: &TurnContext,
        requested: RequestPermissionProfile,
        cwd: &AbsolutePathBuf,
    ) -> Result<RequestPermissionProfile, String> {
        if requested.is_empty() {
            return Ok(requested);
        }
        let Some(level @ codex_security_policy::SecurityLevel::Aggressive) =
            crate::security::protected_additional::protected_level(self)
        else {
            return Ok(requested);
        };
        let (codex_home, sqlite_home, configs) = self.grant_check_configs(Some(cwd)).await;
        let (kept, refusal) = crate::security::protected_additional::strip_grant_off_thread(
            level,
            codex_home,
            sqlite_home,
            configs,
            requested.clone().into(),
        )
        .await;
        let Some(refusal) = refusal else {
            return Ok(requested);
        };
        let unchecked = refusal == crate::security::protected_additional::unchecked_refusal(level);
        crate::security::inspection::record_refusal(
            self.thread_id(),
            "a permission request naming a protected path",
            match kept {
                Some(_) => "those paths were left out before anyone was asked",
                None if unchecked => "refused before anyone was asked; it could not be checked",
                None => "refused before anyone was asked; never granted under Aggressive",
            },
        );
        crate::security::protected_reads::notify(self, turn_context, refusal.clone()).await;
        kept.map(Into::into).ok_or(refusal)
    }

    /// Issue #245: once the protected-path rules apply, a permission grant
    /// never names a protected path. Such entries are dropped from the answer
    /// the model gets and from what is recorded, and the person is told why.
    async fn without_protected_grant(
        &self,
        response: RequestPermissionsResponse,
        cwd: &AbsolutePathBuf,
        turn_context: Option<&TurnContext>,
    ) -> RequestPermissionsResponse {
        if response.permissions.is_empty() {
            return response;
        }
        let Some(level) = crate::security::protected_additional::protected_level(self) else {
            return response;
        };
        let (codex_home, sqlite_home, configs) = self.grant_check_configs(Some(cwd)).await;
        let (kept, refusal) = crate::security::protected_additional::strip_grant_off_thread(
            level,
            codex_home,
            sqlite_home,
            configs,
            response.permissions.clone().into(),
        )
        .await;
        let Some(refusal) = refusal else {
            return response;
        };
        // #622: listed under the inspector's recent denials too.
        crate::security::inspection::record_refusal(
            self.thread_id(),
            "a permission grant naming a protected path",
            "those paths were left out of the grant",
        );
        match turn_context {
            Some(turn_context) => {
                crate::security::protected_reads::notify(self, turn_context, refusal).await;
            }
            None => {
                if let Some((turn_context, _)) =
                    self.active_turn_context_and_cancellation_token().await
                {
                    crate::security::protected_reads::notify(self, &turn_context, refusal).await;
                }
            }
        }
        RequestPermissionsResponse {
            permissions: kept.map(Into::into).unwrap_or_default(),
            ..response
        }
    }

    /// Records a grant answered to a request shown under `granted_under`.
    /// D51 (#1001): it holds only while no security transition is committed
    /// (see [`crate::state::GrantedPermissions`]).
    pub(crate) async fn record_granted_request_permissions_for_turn(
        &self,
        response: &RequestPermissionsResponse,
        environment_id: &str,
        originating_turn_state: Option<&Arc<Mutex<crate::state::TurnState>>>,
        granted_under: Option<(u64, u64)>,
    ) {
        if response.permissions.is_empty() {
            return;
        }
        let permissions: AdditionalPermissionProfile = response.permissions.clone().into();
        match response.scope {
            PermissionGrantScope::Turn => {
                if let Some(turn_state) = originating_turn_state {
                    let mut ts = turn_state.lock().await;
                    // Read under the lock: a later grant is never recorded
                    // under an earlier marker.
                    let current = self.services.authority_marker();
                    let recorded = ts.record_granted_permissions(
                        environment_id,
                        permissions,
                        granted_under,
                        current,
                    );
                    // D51: strict auto-review comes with the grant and ends
                    // with it.
                    if recorded && response.strict_auto_review {
                        ts.enable_strict_auto_review(current);
                    }
                }
            }
            PermissionGrantScope::Session => {
                let mut state = self.state.lock().await;
                let current = self.services.authority_marker();
                state.record_granted_permissions(
                    environment_id,
                    permissions,
                    granted_under,
                    current,
                );
            }
        }
    }

    #[expect(
        clippy::await_holding_invalid_type,
        reason = "active turn reads must stay consistent with the matching turn state"
    )]
    pub(crate) async fn granted_turn_permissions(
        &self,
        environment_id: &str,
    ) -> Option<AdditionalPermissionProfile> {
        let granted = {
            let active = self.active_turn.lock().await;
            let active = active.as_ref()?;
            let mut ts = active.turn_state.lock().await;
            ts.granted_permissions(environment_id, self.services.authority_marker())
        };
        self.usable_grant(granted).await
    }

    /// Issue #245: a stored grant as later commands and file tools may use
    /// it: without protected paths while the protected-path rules apply. A
    /// grant recorded before they applied (the level raised mid-session)
    /// cannot reopen one either.
    async fn usable_grant(
        &self,
        granted: Option<AdditionalPermissionProfile>,
    ) -> Option<AdditionalPermissionProfile> {
        let granted = granted?;
        let Some(level) = crate::security::protected_additional::protected_level(self) else {
            return Some(granted);
        };
        let (codex_home, sqlite_home, configs) = self.grant_check_configs(/*cwd*/ None).await;
        crate::security::protected_additional::strip_grant_off_thread(
            level,
            codex_home,
            sqlite_home,
            configs,
            granted,
        )
        .await
        .0
    }

    /// Issue #524: the Corbanu home and its database folder (#611), and what
    /// a grant is checked against:
    /// `cwd` (or the session's original folder) with the original profile,
    /// and the session's current folder and profile, which may have changed
    /// since, each with the current workspace roots.
    async fn grant_check_configs(
        &self,
        cwd: Option<&AbsolutePathBuf>,
    ) -> (
        std::path::PathBuf,
        std::path::PathBuf,
        Vec<crate::security::protected_additional::GrantCheckConfig>,
    ) {
        use crate::security::protected_additional::GrantCheckConfig;
        let state = self.state.lock().await;
        let current = &state.session_configuration;
        let config = &current.original_config_do_not_use;
        let workspace_roots = current.primary_workspace_roots();
        let mut configs = vec![GrantCheckConfig {
            cwd: cwd.unwrap_or(&config.cwd).clone(),
            workspace_roots: workspace_roots.clone(),
            profile: config.permissions.permission_profile().clone(),
        }];
        let now = GrantCheckConfig {
            cwd: current.cwd().clone(),
            workspace_roots,
            profile: current.permission_profile(),
        };
        if !configs.contains(&now) {
            configs.push(now);
        }
        (
            config.codex_home.to_path_buf(),
            config.sqlite.home().to_path_buf(),
            configs,
        )
    }

    #[expect(
        clippy::await_holding_invalid_type,
        reason = "active turn reads must stay consistent with the matching turn state"
    )]
    pub(crate) async fn strict_auto_review_enabled_for_turn(&self) -> bool {
        let active = self.active_turn.lock().await;
        let Some(active) = active.as_ref() else {
            return false;
        };
        let ts = active.turn_state.lock().await;
        ts.strict_auto_review_enabled(self.services.authority_marker())
    }

    pub(crate) async fn granted_session_permissions(
        &self,
        environment_id: &str,
    ) -> Option<AdditionalPermissionProfile> {
        let granted = {
            let mut state = self.state.lock().await;
            state.granted_permissions(environment_id, self.services.authority_marker())
        };
        self.usable_grant(granted).await
    }

    #[expect(
        clippy::await_holding_invalid_type,
        reason = "active turn checks and turn state updates must remain atomic"
    )]
    pub async fn notify_dynamic_tool_response(&self, call_id: &str, response: DynamicToolResponse) {
        let entry = {
            let mut active = self.active_turn.lock().await;
            match active.as_mut() {
                Some(at) => {
                    let mut ts = at.turn_state.lock().await;
                    ts.remove_pending_dynamic_tool(call_id)
                }
                None => None,
            }
        };
        match entry {
            Some(tx_response) => {
                tx_response.send(response).ok();
            }
            None => {
                warn!("No pending dynamic tool call found for call_id: {call_id}");
            }
        }
    }

    #[expect(
        clippy::await_holding_invalid_type,
        reason = "active turn checks and turn state updates must remain atomic"
    )]
    pub async fn notify_approval(&self, approval_id: &str, decision: ReviewDecision) {
        let entry = {
            let mut active = self.active_turn.lock().await;
            match active.as_mut() {
                Some(at) => {
                    let mut ts = at.turn_state.lock().await;
                    ts.remove_pending_approval(approval_id)
                }
                None => None,
            }
        };
        match entry {
            Some(tx_approve) => {
                tx_approve.send(decision).ok();
            }
            None => {
                warn!("No pending approval found for call_id: {approval_id}");
            }
        }
    }

    /// Records conversation items: append to history, persist to rollout, and
    /// notify clients observing raw response items.
    pub(crate) fn prepare_conversation_items_for_history<'a>(
        &self,
        turn_context: &TurnContext,
        items: &'a [ResponseItem],
    ) -> Cow<'a, [ResponseItem]> {
        let mut items = Cow::Borrowed(items);
        prepare_image_response_items(items.to_mut());
        prepare_audio_response_items(items.to_mut());
        // Most response items get their passthrough turn ID at the durable history boundary.
        for item in items.to_mut() {
            item.set_turn_id_if_missing(&turn_context.sub_id);
        }
        Self::assign_missing_response_item_ids(items)
    }

    fn assign_missing_response_item_ids(items: Cow<'_, [ResponseItem]>) -> Cow<'_, [ResponseItem]> {
        if items
            .iter()
            .all(|item| item.id().is_some_and(|id| !id.is_empty()))
        {
            return items;
        }
        let mut items = items;
        for item in items.to_mut() {
            Self::assign_missing_response_item_id(item);
        }
        items
    }

    fn assign_missing_response_item_id(item: &mut ResponseItem) {
        if item.id().is_some_and(|id| !id.is_empty()) {
            return;
        }
        let Some(prefix) = item.id_prefix() else {
            return;
        };
        item.set_id(Some(ResponseItemId::new(prefix)));
    }

    fn assign_missing_rollout_response_item_ids(items: &mut [RolloutItem]) {
        for item in items {
            if let RolloutItem::ResponseItem(response_item) = item {
                Self::assign_missing_response_item_id(response_item);
            }
        }
    }

    pub(crate) fn response_item_from_user_input(&self, input: Vec<UserInput>) -> ResponseItem {
        ResponseItem::from(ResponseInputItem::from_user_input(
            input,
            LocalImagePreparation::Defer,
        ))
    }

    #[tracing::instrument(level = "trace", skip_all, fields(item_count = items.len()))]
    pub(crate) async fn record_conversation_items(
        &self,
        turn_context: &TurnContext,
        items: &[ResponseItem],
    ) {
        self.record_conversation_items_from(turn_context, items, /*origin*/ None)
            .await;
    }

    /// Record items whose messages have a host-known origin. Unattributed
    /// messages (`None`) reach a protected provider only as labelled data.
    pub(crate) async fn record_conversation_items_from(
        &self,
        turn_context: &TurnContext,
        items: &[ResponseItem],
        origin: Option<crate::security::ingress::MessageOrigin>,
    ) {
        let items = self.prepare_conversation_items_for_history(turn_context, items);
        // PF-28-S01: tool results and model output are gated before history,
        // rollout and clients see them.
        let gated = crate::security::disclosure_gate::gate_values(
            codex_secret_broker::output_gate::OutputSink::ToolResult,
            items.as_ref(),
        );
        let items = gated.as_deref().unwrap_or(items.as_ref());
        if let Some(origin) = origin {
            self.services
                .model_client()
                .register_message_origin(items, origin);
        }
        self.services.model_client().note_recorded_for_taint(items);
        self.services.model_client().observe_native_ingress(items);
        {
            let mut state = self.state.lock().await;
            state.current_time_reminder.note_recorded_items(items);
            state.record_items(
                items.iter(),
                turn_context.model_info.truncation_policy.into(),
            );
        }
        let mut rollout_items: Vec<RolloutItem> = items
            .iter()
            .cloned()
            .map(RolloutItem::ResponseItem)
            .collect();
        // Origins follow the items they describe so a resume restores them.
        if let Some(record) = self.services.model_client().take_source_origin_record() {
            rollout_items.push(RolloutItem::SourceOrigin(record));
        }
        self.persist_rollout_items(&rollout_items).await;
        self.send_raw_response_items(turn_context, items).await;
    }

    /// Persist origin registrations made outside `record_conversation_items_from`
    /// (compaction installs its replacement history directly).
    async fn persist_source_origins(&self) {
        if let Some(record) = self.services.model_client().take_source_origin_record() {
            self.persist_rollout_items(&[RolloutItem::SourceOrigin(record)])
                .await;
        }
    }

    pub(crate) async fn record_step_world_state_if_changed(
        &self,
        previous_world_state: &Arc<WorldState>,
        step_context: &step_context::StepContext,
    ) -> CodexResult<Arc<WorldState>> {
        let turn_context = step_context.turn.as_ref();
        // Render model-visible state from the same step used to build and run tools.
        let world_state = Arc::new(self.build_world_state_for_step(step_context).await?);
        // Derive the model update and persisted patch from the same two snapshots.
        let previous_snapshot = previous_world_state.snapshot();
        let world_state_snapshot = world_state.snapshot();
        let world_state_item = world_state_snapshot
            .merge_patch_from(&previous_snapshot)
            .map(WorldStateItem::patch);
        let items = crate::context_manager::updates::merge_contextual_fragments(
            world_state.render_diff(&previous_snapshot),
        );
        if !items.is_empty() {
            self.record_conversation_items_from(
                turn_context,
                &items,
                Some(crate::security::ingress::MessageOrigin::Host),
            )
            .await;
        }

        // ContextManager remembers this for later turns; run_turn owns the live value.
        self.state
            .lock()
            .await
            .history
            .set_world_state_baseline(world_state_snapshot);
        // Record the patch after the context it describes is present in model history.
        if let Some(world_state_item) = world_state_item {
            self.persist_rollout_items(&[RolloutItem::WorldState(world_state_item)])
                .await;
        }
        Ok(world_state)
    }

    /// Captures one request-scoped view of dynamic state.
    ///
    /// This may refresh filesystem-derived state. Normal turns should call it only from
    /// `run_turn` and pass the result down; standalone request or history boundaries may capture
    /// their own step.
    #[tracing::instrument(name = "step_context.capture", level = "info", skip_all)]
    pub(crate) async fn capture_step_context(
        self: &Arc<Self>,
        turn_context: Arc<TurnContext>,
        cancellation_token: &CancellationToken,
    ) -> CodexResult<Arc<StepContext>> {
        self.capture_step_context_with_required_mcp_servers(
            turn_context,
            cancellation_token,
            /*required_servers*/ &[],
        )
        .await
    }

    pub(crate) async fn capture_step_context_with_required_mcp_servers(
        self: &Arc<Self>,
        turn_context: Arc<TurnContext>,
        cancellation_token: &CancellationToken,
        required_servers: &[String],
    ) -> CodexResult<Arc<StepContext>> {
        // Keep selections fixed for the turn while allowing their startup work to finish.
        let environments = turn_context.environments.refresh_readiness();
        self.services
            .agents_md_manager
            .refresh(&turn_context.config, &environments)
            .await;
        let loaded_agents_md = self.services.agents_md_manager.get_loaded().await;
        let selected_capability_roots = self
            .resolve_selected_capability_roots_for_step(&environments)
            .await;
        let ready_selected_capability_roots =
            Self::ready_selected_capability_roots(&selected_capability_roots);
        let executor_capability_discovery = self
            .executor_capability_discovery_for_step(
                &turn_context.config,
                &ready_selected_capability_roots,
                &environments,
                turn_context.windows_sandbox_level,
            )
            .await;
        let extension_data = codex_extension_api::ExtensionData::new(turn_context.sub_id.clone());
        extension_data.insert(selected_capability_roots.clone());
        if let Some(discovery) = &executor_capability_discovery {
            extension_data.insert(discovery.as_ref().clone());
            if !discovery.sandbox_contexts().is_empty() {
                extension_data.insert(discovery.sandbox_contexts().clone());
            }
        } else if !turn_context
            .config
            .permissions
            .file_system_sandbox_policy()
            .has_full_disk_read_access()
        {
            let sandbox_contexts = environments
                .turn_environments()
                .map(|environment| {
                    (
                        environment.environment_id.clone(),
                        // PF-23-S01: post-taint read denials, as for file tools.
                        crate::security::protected_surface::protect_file_tool_context(
                            self,
                            &turn_context,
                            turn_context.file_system_sandbox_context(
                                /*additional_permissions*/ None,
                                environment,
                            ),
                        ),
                    )
                })
                .collect::<HashMap<_, _>>();
            extension_data.insert(sandbox_contexts);
        }
        let (mcp, prepared_recommendations) = async {
            tokio::join!(
                self.mcp_runtime_for_step(
                    turn_context.as_ref(),
                    &selected_capability_roots,
                    required_servers,
                ),
                turn::prepare_tool_recommendations(self.as_ref(), turn_context.as_ref()),
            )
        }
        .or_cancel(cancellation_token)
        .await?;
        let (mcp_tools, tool_router) = turn::built_tools(
            self.as_ref(),
            turn_context.as_ref(),
            &environments,
            mcp.as_ref(),
            &extension_data,
            prepared_recommendations,
        )
        .or_cancel(cancellation_token)
        .await?;
        Ok(Arc::new(StepContext {
            turn: turn_context,
            environments,
            selected_capability_roots,
            executor_capability_discovery,
            mcp,
            mcp_tools,
            tool_router,
            loaded_agents_md,
        }))
    }

    pub(crate) async fn record_inter_agent_communication(
        &self,
        turn_context: &TurnContext,
        mut communication: InterAgentCommunication,
    ) {
        communication.set_turn_id_if_missing(&turn_context.sub_id);
        if communication.id.as_ref().is_none_or(|id| id.is_empty()) {
            communication.id = Some(ResponseItemId::new("amsg"));
        }
        let message_id = communication.message_id.clone();
        if let Some(message_id) = message_id.as_ref()
            && !self
                .applied_agent_message_ids
                .lock()
                .await
                .insert(message_id.clone())
        {
            return;
        }
        let response_item = communication.to_model_input_item();
        let items = self.prepare_conversation_items_for_history(
            turn_context,
            std::slice::from_ref(&response_item),
        );
        let items = items.as_ref();
        let response_item = items[0].clone();
        // Agent mail is data without standing (PF-30-S03 taint).
        self.services.model_client().note_recorded_for_taint(items);
        {
            let mut state = self.state.lock().await;
            state.current_time_reminder.note_recorded_items(items);
            state.record_items(
                items.iter(),
                turn_context.model_info.truncation_policy.into(),
            );
        }
        self.persist_rollout_items(&[RolloutItem::InterAgentCommunication(communication)])
            .await;
        if let Some(message_id) = message_id
            && let Some(state_db) = self.state_db()
        {
            match self.flush_rollout().await {
                Ok(()) => {
                    if self
                        .track_agent_message_for_turn(&turn_context.sub_id, message_id.clone())
                        .await
                    {
                        match state_db
                            .transition_agent_message(
                                &message_id,
                                codex_state::AgentMailboxPhase::Submitted,
                                codex_state::AgentMailboxPhase::ProviderRunning,
                                crate::turn_timing::now_unix_timestamp_ms(),
                            )
                            .await
                        {
                            Ok(true) => {}
                            Ok(false) => warn!(
                                %message_id,
                                "agent mailbox message could not enter provider_running from its current state"
                            ),
                            Err(err) => warn!(
                                %err,
                                %message_id,
                                "failed to mark agent mailbox message provider_running"
                            ),
                        }
                    } else if let Err(err) = state_db
                        .mark_agent_message_completed(
                            &message_id,
                            crate::turn_timing::now_unix_timestamp_ms(),
                        )
                        .await
                    {
                        warn!(%err, %message_id, "failed to complete locally applied mailbox message");
                    }
                }
                Err(err) => warn!(
                    %err,
                    %message_id,
                    "leaving agent mailbox message recoverable because its rollout did not flush"
                ),
            }
        }
        self.send_raw_response_items(turn_context, items).await;
        // Plaintext inter-agent controls are operator-visible transcript content. Raw response
        // notifications preserve provider compatibility, but app-server clients render the
        // item lifecycle; emit it here just as we do for ordinary recorded response items.
        if let Some(item) = parse_turn_item(&response_item) {
            self.emit_turn_item_started(turn_context, &item).await;
            self.emit_turn_item_completed(turn_context, item).await;
        }
    }

    pub(crate) async fn has_applied_agent_message_id(&self, message_id: &str) -> bool {
        self.applied_agent_message_ids
            .lock()
            .await
            .contains(message_id)
    }

    async fn track_agent_message_for_turn(&self, sub_id: &str, message_id: String) -> bool {
        let Some(turn_state) = self
            .input_queue
            .turn_state_for_sub_id(&self.active_turn, sub_id)
            .await
        else {
            return false;
        };
        turn_state.lock().await.track_mailbox_message(message_id);
        true
    }

    async fn maybe_warn_on_server_model_mismatch(
        self: &Arc<Self>,
        turn_context: &Arc<TurnContext>,
        server_model: String,
    ) -> bool {
        let requested_model = turn_context.model_info.slug.clone();
        let server_model_normalized = server_model.to_ascii_lowercase();
        let requested_model_normalized = requested_model.to_ascii_lowercase();
        if server_model_normalized == requested_model_normalized {
            info!("server reported model {server_model} (matches requested model)");
            return false;
        }

        warn!("server reported model {server_model} while requested model was {requested_model}");

        if !turn_context.config.model_provider.is_openai() {
            info!(
                provider = %turn_context.config.model_provider_id,
                "skipping model reroute warning for non-OpenAI provider"
            );
            return false;
        }

        let warning_message = format!(
            "Your account was flagged for potentially high-risk cyber activity and this request was routed to gpt-5.2 as a fallback. To regain access to gpt-5.3-codex, apply for trusted access: {CYBER_VERIFY_URL} or learn more: {CYBER_SAFETY_URL}"
        );

        self.send_event(
            turn_context,
            EventMsg::ModelReroute(ModelRerouteEvent {
                from_model: requested_model.clone(),
                to_model: server_model.clone(),
                reason: ModelRerouteReason::HighRiskCyberActivity,
            }),
        )
        .await;

        self.send_event(
            turn_context,
            EventMsg::Warning(WarningEvent {
                message: warning_message.clone(),
            }),
        )
        .await;
        true
    }

    fn record_server_model_identity(
        self: &Arc<Self>,
        turn_context: &Arc<TurnContext>,
        server_model: &str,
    ) {
        let requested_model = turn_context.model_info.slug.as_str();
        if server_model.eq_ignore_ascii_case(requested_model) {
            info!("server reported model {server_model} (matches requested model)");
            return;
        }
        info!(
            model_provider = %turn_context.config.model_provider_id,
            requested_model,
            reported_model = server_model,
            "provider reported a different model identity; recording it without inferring a reroute reason"
        );
    }

    pub(crate) async fn emit_model_verification(
        self: &Arc<Self>,
        turn_context: &Arc<TurnContext>,
        verifications: Vec<ModelVerification>,
    ) {
        self.send_event(
            turn_context,
            EventMsg::ModelVerification(ModelVerificationEvent { verifications }),
        )
        .await;
    }

    pub(crate) async fn emit_turn_moderation_metadata(
        self: &Arc<Self>,
        turn_context: &Arc<TurnContext>,
        metadata: TurnModerationMetadataEvent,
    ) {
        self.send_event(turn_context, EventMsg::TurnModerationMetadata(metadata))
            .await;
    }

    #[cfg(test)]
    pub(crate) async fn replace_history(
        &self,
        items: Vec<ResponseItem>,
        reference_context_item: Option<TurnContextItem>,
    ) {
        let mut state = self.state.lock().await;
        state.replace_history(items, reference_context_item);
    }

    pub(crate) async fn replace_compacted_history(
        &self,
        items: Vec<ResponseItem>,
        reference_context_item: Option<TurnContextItem>,
        world_state_baseline: Option<Arc<WorldState>>,
        metadata: CompactedHistoryMetadata,
    ) {
        let items = Self::assign_missing_response_item_ids(Cow::Owned(items)).into_owned();
        // Resumes that start at this checkpoint (paginated threads, referenced
        // forks) never read earlier records, so restate the current origins.
        self.services.model_client().journal_current_origins(&items);
        let compacted_item = CompactedItem {
            message: metadata.message,
            replacement_history: Some(items.clone()),
            window_number: Some(metadata.window_number),
            first_window_id: Some(metadata.window_ids.first_window_id.to_string()),
            previous_window_id: metadata
                .window_ids
                .previous_window_id
                .map(|id| id.to_string()),
            window_id: Some(metadata.window_ids.window_id.to_string()),
        };
        // Compaction starts a new history window, so its WorldState baseline must be full.
        let mut world_state_item = None;
        {
            let mut state = self.state.lock().await;
            state.replace_history(items, reference_context_item.clone());
            if let Some(world_state) = world_state_baseline {
                let snapshot = world_state.snapshot();
                world_state_item = Some(WorldStateItem::full(snapshot.clone().into_value()));
                state.history.set_world_state_baseline(snapshot);
            }
        }

        self.persist_rollout_items(&[RolloutItem::Compacted(compacted_item)])
            .await;
        // Origins registered for the replacement history follow it.
        self.persist_source_origins().await;
        // Persist the baseline after the replacement history that established it.
        if let Some(world_state_item) = world_state_item {
            self.persist_rollout_items(&[RolloutItem::WorldState(world_state_item)])
                .await;
        }
        if let Some(turn_context_item) = reference_context_item {
            self.persist_rollout_items(&[RolloutItem::TurnContext(turn_context_item)])
                .await;
        }
        {
            let mut state = self.state.lock().await;
            state.queue_pending_session_start_source(codex_hooks::SessionStartSource::Compact);
        }
    }

    pub fn enabled(&self, feature: Feature) -> bool {
        self.features.enabled(feature)
    }

    pub(crate) fn features(&self) -> ManagedFeatures {
        self.features.clone()
    }

    pub(crate) async fn collaboration_mode(&self) -> CollaborationMode {
        let state = self.state.lock().await;
        state.session_configuration.collaboration_mode.clone()
    }

    pub(crate) fn multi_agent_version(&self) -> Option<MultiAgentVersion> {
        self.multi_agent_version.get().copied()
    }

    pub(crate) fn set_multi_agent_version_if_unset(
        &self,
        multi_agent_version: MultiAgentVersion,
    ) -> MultiAgentVersion {
        *self.multi_agent_version.get_or_init(|| multi_agent_version)
    }

    pub(crate) fn resolve_multi_agent_version_for_model(
        &self,
        model_info: &ModelInfo,
        config: &Config,
    ) -> MultiAgentVersion {
        if let Some(multi_agent_version) = self.multi_agent_version() {
            return config.multi_agent_version_for_model(Some(multi_agent_version));
        }

        let selected = config.multi_agent_version_for_model(model_info.multi_agent_version);

        self.set_multi_agent_version_if_unset(selected)
    }

    #[tracing::instrument(level = "trace", skip_all, fields(item_count = items.len()))]
    async fn send_raw_response_items(&self, turn_context: &TurnContext, items: &[ResponseItem]) {
        for item in items {
            self.send_event(
                turn_context,
                EventMsg::RawResponseItem(RawResponseItemEvent { item: item.clone() }),
            )
            .await;
        }
    }

    /// `source_envelopes`: one developer message per stored-data fragment
    /// (memories, MCP-supplied hints), registered as external data before any
    /// caller records the surrounding context as host. The first registration
    /// wins, so it is never upgraded and reaches a protected request labelled
    /// with its real source (`source=memory`, `source=mcp`).
    fn push_stored_data_messages(
        &self,
        items: &mut Vec<ResponseItem>,
        sections: Vec<String>,
        kind: codex_protocol::provenance::SourceKind,
    ) {
        for section in sections {
            if let Some(message) =
                crate::context_manager::updates::build_developer_update_item(vec![section])
            {
                self.services.model_client().register_message_origin(
                    std::slice::from_ref(&message),
                    crate::security::ingress::MessageOrigin::External(kind),
                );
                items.push(message);
            }
        }
    }

    async fn build_turn_context_contribution_items(
        &self,
        step_context: &StepContext,
    ) -> Vec<ResponseItem> {
        let turn_context = step_context.turn.as_ref();
        let mut developer_sections = Vec::new();
        let mut contextual_user_sections = Vec::new();
        let mut separate_developer_sections = Vec::new();
        let context_contributors = self.services.extensions.context_contributors().to_vec();
        let separate_stored_data = self.services.model_client().source_envelopes_enabled();
        let mut stored_data_sections = Vec::new();

        for contributor in &context_contributors {
            for fragment in contributor
                .contribute_turn_context(TurnContextContributionInput {
                    thread_id: self.thread_id(),
                    turn_id: turn_context.sub_id.as_str(),
                    session_store: &self.services.session_extension_data,
                    thread_store: &self.services.thread_extension_data,
                    turn_store: turn_context.extension_data.as_ref(),
                    model_context_window: turn_context.model_context_window(),
                })
                .await
            {
                if separate_stored_data && fragment.is_stored_data() {
                    stored_data_sections.push(fragment.text().to_string());
                    continue;
                }
                push_prompt_fragment(
                    fragment,
                    &mut developer_sections,
                    &mut contextual_user_sections,
                    &mut separate_developer_sections,
                );
            }
        }

        let mut items = Vec::with_capacity(3);
        if let Some(developer_message) =
            crate::context_manager::updates::build_developer_update_item(developer_sections)
        {
            items.push(developer_message);
        }
        for section in separate_developer_sections {
            if let Some(developer_message) =
                crate::context_manager::updates::build_developer_update_item(vec![section])
            {
                items.push(developer_message);
            }
        }
        self.push_stored_data_messages(
            &mut items,
            stored_data_sections,
            codex_protocol::provenance::SourceKind::Memory,
        );
        if let Some(contextual_user_message) =
            crate::context_manager::updates::build_contextual_user_message(contextual_user_sections)
        {
            items.push(contextual_user_message);
        }
        items
    }

    pub(crate) async fn build_initial_context_with_world_state(
        &self,
        turn_context: &TurnContext,
        world_state: &WorldState,
    ) -> Vec<ResponseItem> {
        let mut developer_sections = Vec::<ContextSection>::with_capacity(8);
        let mut contextual_user_sections = Vec::<String>::with_capacity(2);
        let mut separate_developer_sections = Vec::<ContextSection>::new();
        let (session_source, auto_compact_window_ids) = {
            let state = self.state.lock().await;
            (
                state.session_configuration.session_source.clone(),
                state.auto_compact_window_ids(),
            )
        };
        let separate_guardian_developer_message =
            crate::guardian::is_guardian_reviewer_source(&session_source);
        // Keep the guardian policy prompt out of the aggregated developer bundle so it
        // stays isolated as its own top-level developer message for guardian subagents.
        if !separate_guardian_developer_message
            && let Some(developer_instructions) = turn_context.developer_instructions.as_deref()
            && !developer_instructions.is_empty()
        {
            developer_sections.push(developer_instructions.to_string().into());
        }
        if turn_context.config.include_skill_instructions
            && turn_context
                .extension_data
                .get::<HostSkillsCatalogInWorldState>()
                .is_none()
        {
            let available_skills = build_available_skills(
                turn_context.turn_skills.snapshot.outcome(),
                default_skill_metadata_budget(turn_context.model_info.context_window),
                SkillRenderSideEffects::ThreadStart {
                    session_telemetry: &self.services.session_telemetry,
                },
            );
            if let Some(available_skills) = available_skills {
                let warning_message = available_skills.warning_message.clone();
                let skills_instructions = AvailableSkillsInstructions::from_available_skills(
                    &available_skills,
                    turn_context.model_info.include_skills_usage_instructions,
                );
                if let Some(warning_message) = warning_message {
                    self.send_event_raw(Event {
                        id: String::new(),
                        msg: EventMsg::Warning(WarningEvent {
                            message: warning_message,
                        }),
                    })
                    .await;
                }
                developer_sections.push(ContextSection::attributed(
                    skills_instructions.render(),
                    HOST_SKILLS_CONTEXT_SOURCE_ID,
                ));
            }
        }
        let loaded_plugins = self
            .services
            .plugins_manager
            .plugins_for_config(&turn_context.config.plugins_config_input())
            .await;
        let features = turn_context.config.features.get();
        let recommended_plugin_candidates = if features.enabled(Feature::Apps)
            && features.enabled(Feature::Plugins)
            && (features.enabled(Feature::ToolSuggest)
                || features.enabled(Feature::RecommendedPlugins))
        {
            let auth = self.services.auth_manager().auth().await;
            let plugins_config = turn_context.config.plugins_config_input();
            self.services
                .plugins_manager
                .recommended_plugin_candidates_for_config(RecommendedPluginCandidatesInput {
                    plugins_config: &plugins_config,
                    loaded_plugins: &loaded_plugins,
                    auth: auth.as_ref(),
                    disabled_tools: &turn_context.config.tool_suggest.disabled_tools,
                    app_server_client_name: turn_context.app_server_client_name.as_deref(),
                })
                .await
        } else {
            None
        };
        if let Some(recommended_plugins) = recommended_plugin_candidates
            .as_deref()
            .and_then(RecommendedPluginsInstructions::from_plugins)
        {
            contextual_user_sections.push(recommended_plugins.render());
        }
        let context_contributors = self.services.extensions.context_contributors().to_vec();
        // `source_envelopes`: stored-data fragments (memories) get their own
        // message so they can be labelled without labelling host policy.
        let separate_stored_data = self.services.model_client().source_envelopes_enabled();
        let mut stored_data_sections = Vec::<String>::new();
        let mut mcp_hint_sections = Vec::<String>::new();
        for contributor in &context_contributors {
            for fragment in contributor
                .contribute_thread_context(
                    &self.services.session_extension_data,
                    &self.services.thread_extension_data,
                )
                .await
            {
                if separate_stored_data && fragment.is_stored_data() {
                    stored_data_sections.push(fragment.text().to_string());
                    continue;
                }
                push_prompt_fragment(
                    fragment,
                    &mut developer_sections,
                    &mut contextual_user_sections,
                    &mut separate_developer_sections,
                );
            }
        }
        for contributor in &context_contributors {
            for fragment in contributor
                .contribute_turn_context(TurnContextContributionInput {
                    thread_id: self.thread_id(),
                    turn_id: turn_context.sub_id.as_str(),
                    session_store: &self.services.session_extension_data,
                    thread_store: &self.services.thread_extension_data,
                    turn_store: turn_context.extension_data.as_ref(),
                    model_context_window: turn_context.model_context_window(),
                })
                .await
            {
                if separate_stored_data && fragment.is_stored_data() {
                    stored_data_sections.push(fragment.text().to_string());
                    continue;
                }
                push_prompt_fragment(
                    fragment,
                    &mut developer_sections,
                    &mut contextual_user_sections,
                    &mut separate_developer_sections,
                );
            }
        }
        // This is full-context metadata. Steady-state context diffs should not re-emit it.
        if turn_context.config.features.enabled(Feature::TokenBudget)
            && turn_context.model_context_window().is_some()
        {
            let mcp_result = self
                .services
                .mcp_runtime
                .latest_call_tool(
                    "notes",
                    "thread_hint",
                    /*arguments*/ None,
                    Some(serde_json::json!({
                        "threadId": self.thread_id().to_string(),
                    })),
                )
                .await
                .ok()
                .and_then(|result| {
                    let text = result
                        .content
                        .iter()
                        .filter_map(|content| {
                            content.get("text").and_then(serde_json::Value::as_str)
                        })
                        .filter(|text| !text.is_empty())
                        .collect::<Vec<_>>()
                        .join("\n");
                    (!text.is_empty()).then_some(text)
                });
            // `source_envelopes`: the MCP server's hint is external text, not
            // host policy; send it as its own `source=mcp` data message.
            let (mcp_result, data_hint) =
                token_budget_hint_placement(separate_stored_data, mcp_result);
            mcp_hint_sections.extend(data_hint);
            developer_sections.push(
                crate::context::TokenBudgetContext::new(
                    self.thread_id(),
                    auto_compact_window_ids.first_window_id,
                    auto_compact_window_ids.previous_window_id,
                    auto_compact_window_ids.window_id,
                    mcp_result,
                )
                .render()
                .into(),
            );
        }
        // Render the active mode after the usage hint so it can override that hint.
        let mut initial_multi_agent_mode = None;
        for fragment in world_state.render_full() {
            match fragment.role() {
                "developer"
                    if fragment.markers().0 == ModelSwitchInstructions::type_markers().0 =>
                {
                    // New-model instructions must precede the rest of the developer context.
                    developer_sections.insert(0, ContextSection::from_fragment(fragment.as_ref()));
                }
                "developer" if fragment.markers().0 == MULTI_AGENT_MODE_OPEN_TAG => {
                    initial_multi_agent_mode = Some(fragment);
                }
                "developer" => {
                    developer_sections.push(ContextSection::from_fragment(fragment.as_ref()))
                }
                "user" => contextual_user_sections.push(fragment.render()),
                _ => {}
            }
        }

        let multi_agent_v2_usage_hint_text =
            multi_agents::usage_hint_text(turn_context, &session_source);

        let mut items = Vec::with_capacity(4);
        if let Some(developer_message) =
            crate::context_manager::updates::build_developer_update_item(developer_sections)
        {
            items.push(developer_message);
        }
        for section in separate_developer_sections {
            if let Some(developer_message) =
                crate::context_manager::updates::build_developer_update_item(vec![section])
            {
                items.push(developer_message);
            }
        }
        self.push_stored_data_messages(
            &mut items,
            stored_data_sections,
            codex_protocol::provenance::SourceKind::Memory,
        );
        self.push_stored_data_messages(
            &mut items,
            mcp_hint_sections,
            codex_protocol::provenance::SourceKind::Mcp,
        );
        if let Some(usage_hint_text) = multi_agent_v2_usage_hint_text
            && let Some(usage_hint_message) =
                crate::context_manager::updates::build_developer_update_item(vec![
                    usage_hint_text.to_string(),
                ])
        {
            items.push(usage_hint_message);
        }
        if let Some(initial_multi_agent_mode) = initial_multi_agent_mode {
            items.push(initial_multi_agent_mode.into_boxed_response_item());
        }
        if let Some(contextual_user_message) =
            crate::context_manager::updates::build_contextual_user_message(contextual_user_sections)
        {
            items.push(contextual_user_message);
        }
        // Emit the guardian policy prompt as a separate developer item so the guardian
        // subagent sees a distinct, easy-to-audit instruction block.
        if separate_guardian_developer_message
            && let Some(developer_instructions) = turn_context.developer_instructions.as_deref()
            && !developer_instructions.is_empty()
            && let Some(guardian_developer_message) =
                crate::context_manager::updates::build_developer_update_item(vec![
                    developer_instructions.to_string(),
                ])
        {
            items.push(guardian_developer_message);
        }
        // New context windows and compaction install these items directly into replacement history.
        for item in &mut items {
            item.set_turn_id_if_missing(&turn_context.sub_id);
        }
        items
    }

    #[tracing::instrument(level = "trace", skip_all, fields(item_count = items.len()))]
    pub(crate) async fn persist_rollout_items(&self, items: &[RolloutItem]) {
        let gated = crate::security::disclosure_gate::gate_values(
            codex_secret_broker::output_gate::OutputSink::Transcript,
            items,
        );
        let items = gated.as_deref().unwrap_or(items);
        let Some(live_thread) = self.live_thread() else {
            return;
        };
        let Err(e) = live_thread.append_items(items).await else {
            // #616: only an append that wrote something shows the home works.
            if items.iter().any(|item| {
                codex_rollout::is_persisted_rollout_item(item, live_thread.history_mode())
            }) {
                crate::home_access::note_rollout_written(self.thread_id());
            }
            return;
        };
        error!("failed to record rollout items: {e:#}");
        // #616: the writer keeps unwritten items for its next flush; exec
        // waits for their folder before it saves the session at exit. Record
        // why the rollout could not be written.
        let message = format!("{e:#}");
        if crate::home_access::is_operation_not_permitted(&message)
            && let Ok(Some(rollout)) = live_thread.local_rollout_path().await
        {
            if let Some(dir) = rollout.parent() {
                crate::home_access::note_rollout_unwritten(self.thread_id(), dir.to_path_buf());
            }
            tokio::task::spawn_blocking(move || {
                crate::home_access::record_home_access_failure("rollout write", &rollout, &message)
            });
        }
    }

    /// See `ModelClient::agent_handoff_origin`: the standing of text this
    /// session's model hands to another agent, from its current history.
    pub(crate) async fn agent_handoff_origin(&self) -> crate::security::ingress::MessageOrigin {
        let state = self.state.lock().await;
        self.services
            .model_client()
            .agent_handoff_origin(state.history.raw_items())
    }

    pub(crate) async fn clone_history(&self) -> ContextManager {
        let state = self.state.lock().await;
        state.clone_history()
    }

    /// Drops each tool output whose call already has an output in history.
    pub(crate) async fn retain_unrecorded_tool_outputs(&self, outputs: &mut Vec<ResponseItem>) {
        fn output_call_id(item: &ResponseItem) -> Option<&str> {
            match item {
                ResponseItem::FunctionCallOutput { call_id, .. }
                | ResponseItem::CustomToolCallOutput { call_id, .. } => Some(call_id),
                ResponseItem::ToolSearchOutput { call_id, .. } => call_id.as_deref(),
                _ => None,
            }
        }
        let state = self.state.lock().await;
        let recorded: HashSet<&str> = state
            .history
            .raw_items()
            .iter()
            .filter_map(output_call_id)
            .collect();
        outputs.retain(|item| output_call_id(item).is_none_or(|id| !recorded.contains(id)));
    }

    pub(crate) async fn current_window_id(&self) -> String {
        let state = self.state.lock().await;
        let thread_id = self.thread_id;
        let window_number = state.auto_compact_window_number();
        format!("{thread_id}:{window_number}")
    }

    pub(crate) async fn advance_auto_compact_window(&self) -> (u64, AutoCompactWindowIds) {
        let mut state = self.state.lock().await;
        state.advance_auto_compact_window()
    }

    pub(crate) async fn request_new_context_window(&self) {
        let mut state = self.state.lock().await;
        state.request_new_context_window();
    }

    pub(crate) async fn take_new_context_window_request(&self) -> bool {
        let mut state = self.state.lock().await;
        state.take_new_context_window_request()
    }

    pub(crate) async fn start_new_context_window(
        &self,
        step_context: &StepContext,
        world_state: Arc<WorldState>,
    ) -> u64 {
        let turn_context = step_context.turn.as_ref();
        let window = {
            let mut state = self.state.lock().await;
            state.start_new_context_window()
        };
        let (window_number, window_ids) = window;
        let context_items = self
            .build_initial_context_with_world_state(turn_context, world_state.as_ref())
            .await;
        // Host-built context; stored-data messages were registered first.
        self.services.model_client().register_message_origin(
            &context_items,
            crate::security::ingress::MessageOrigin::Host,
        );
        let turn_context_item = turn_context.to_turn_context_item();
        self.replace_compacted_history(
            context_items,
            Some(turn_context_item),
            Some(world_state),
            CompactedHistoryMetadata {
                message: String::new(),
                window_number,
                window_ids,
            },
        )
        .await;
        self.recompute_token_usage(turn_context).await;
        window_number
    }

    pub(crate) async fn reference_context_item(&self) -> Option<TurnContextItem> {
        let state = self.state.lock().await;
        state.reference_context_item()
    }

    /// Persist the latest turn context snapshot for the first real user turn and for
    /// steady-state turns that emit model-visible context updates.
    ///
    /// When the reference snapshot is missing, this injects full initial context. Otherwise, it
    /// emits only context diffs.
    ///
    /// If full context is injected and a model switch occurred, this prepends the
    /// `<model_switch>` developer message so model-specific instructions are not lost.
    ///
    /// This is the normal runtime path that establishes a new `reference_context_item`.
    /// Mid-turn compaction is the other path that can re-establish that reference when it
    /// reinjects full initial context into replacement history. Live world-state changes may
    /// independently advance their in-memory baseline within a turn.
    #[instrument(level = "trace", skip_all)]
    pub(crate) async fn record_context_updates_and_set_reference_context_item(
        &self,
        step_context: &StepContext,
    ) -> CodexResult<Arc<WorldState>> {
        let turn_context = step_context.turn.as_ref();
        let reference_context_item = {
            let state = self.state.lock().await;
            state.reference_context_item()
        };
        let mut turn_context_item = turn_context.to_turn_context_item();
        // PF-23-S03: record the level in force, including a stricter one
        // committed during this session; memory work treats it as a floor.
        if let Ok(policy) = self
            .services
            .agent_control
            .effective_security_policy()
            .snapshot_for_agent(self.thread_id)
        {
            turn_context_item.security_level =
                turn_context_item.security_level.max(Some(policy.level));
        }
        // The recorded level is not model-visible context: a change in it
        // (or its absence in older rollouts) alone does not re-inject.
        let turn_context_changed = reference_context_item.as_ref().is_none_or(|reference| {
            let mut reference = reference.clone();
            reference.security_level = turn_context_item.security_level;
            reference != turn_context_item
        });
        let security_level_changed = reference_context_item
            .as_ref()
            .is_some_and(|reference| reference.security_level != turn_context_item.security_level);
        let world_state = Arc::new(self.build_world_state_for_step(step_context).await?);
        let reinject_host_context = self.services.model_client().take_host_context_reinjection();
        let should_inject_full_context = reference_context_item.is_none() || reinject_host_context;
        // Full initial context resets the baseline; later turns persist only its changes.
        let (mut context_items, world_state_item) = if should_inject_full_context {
            let context_items = self
                .build_initial_context_with_world_state(turn_context, world_state.as_ref())
                .await;
            let snapshot = world_state.snapshot();
            self.state
                .lock()
                .await
                .history
                .set_world_state_baseline(snapshot.clone());
            (
                context_items,
                Some(WorldStateItem::full(snapshot.into_value())),
            )
        } else {
            let (world_state_items, world_state_item) = {
                let mut state = self.state.lock().await;
                let (fragments, rollout_item) =
                    state.history.update_world_state(world_state.as_ref());
                (
                    crate::context_manager::updates::merge_contextual_fragments(fragments),
                    rollout_item,
                )
            };
            (world_state_items, world_state_item)
        };
        if !should_inject_full_context && turn_context_changed {
            context_items.extend(
                self.build_turn_context_contribution_items(step_context)
                    .await,
            );
        }
        // A snapshot can change without producing model-visible or TurnContext updates.
        let only_world_state_changed =
            !turn_context_changed && !security_level_changed && context_items.is_empty();
        if only_world_state_changed && world_state_item.is_none() {
            return Ok(world_state);
        }
        if !context_items.is_empty() {
            self.record_conversation_items_from(
                turn_context,
                &context_items,
                Some(crate::security::ingress::MessageOrigin::Host),
            )
            .await;
        }
        // Persist state only after any model-visible context generated from it.
        if let Some(world_state_item) = world_state_item {
            self.persist_rollout_items(&[RolloutItem::WorldState(world_state_item)])
                .await;
        }
        // A snapshot-only change does not require a duplicate TurnContext record.
        if only_world_state_changed {
            return Ok(world_state);
        }
        // Persist one `TurnContextItem` per real user turn so resume/lazy replay can recover the
        // latest durable baseline even when this turn emitted no model-visible context diffs.
        self.persist_rollout_items(&[RolloutItem::TurnContext(turn_context_item.clone())])
            .await;

        // Advance the persisted-settings baseline even when this turn emitted no model-visible
        // context items.
        let mut state = self.state.lock().await;
        state.set_reference_context_item(Some(turn_context_item));
        Ok(world_state)
    }

    pub(crate) async fn update_token_usage_info(
        &self,
        turn_context: &TurnContext,
        token_usage: Option<&TokenUsage>,
    ) -> CodexResult<()> {
        let result = self
            .record_token_usage_info(turn_context, token_usage)
            .await;
        self.send_token_count_event(turn_context).await;
        result
    }

    pub(crate) async fn record_token_usage_info(
        &self,
        turn_context: &TurnContext,
        token_usage: Option<&TokenUsage>,
    ) -> CodexResult<()> {
        if let Some(token_usage) = token_usage {
            let token_info = {
                let mut state = self.state.lock().await;
                state
                    .update_token_info_from_usage(token_usage, turn_context.model_context_window());
                // PF-84: attribute the usage to the account that served it, as the
                // accounting ledger does (provider id and account name only).
                if turn_context.config.features.enabled(Feature::NamedAccounts) {
                    let account = turn_context
                        .provider
                        .info()
                        .account
                        .as_ref()
                        .map_or(codex_vault::DEFAULT_PROVIDER_ACCOUNT, |account| {
                            account.name.as_str()
                        });
                    state.record_account_token_usage(
                        &turn_context.config.model_provider_id,
                        account,
                        token_usage,
                    );
                }
                if matches!(
                    turn_context.config.model_auto_compact_token_limit_scope,
                    AutoCompactTokenLimitScope::BodyAfterPrefix
                ) {
                    state.ensure_auto_compact_window_server_prefill_from_usage(token_usage);
                }
                state.token_info()
            };
            let budget_result = self.record_rollout_budget_usage(token_usage);
            if let Some(token_info) = token_info.as_ref() {
                for contributor in self.services.extensions.token_usage_contributors() {
                    contributor
                        .on_token_usage(
                            &self.services.session_extension_data,
                            &self.services.thread_extension_data,
                            turn_context.extension_data.as_ref(),
                            token_info,
                        )
                        .await;
                }
            }
            budget_result?;
        }
        Ok(())
    }

    pub(crate) async fn recompute_token_usage(&self, turn_context: &TurnContext) {
        let history = self.clone_history().await;
        let base_instructions = self.get_base_instructions().await;
        let Some(estimated_total_tokens) =
            history.estimate_token_count_with_base_instructions(&base_instructions)
        else {
            return;
        };
        {
            let mut state = self.state.lock().await;
            let mut info = state.token_info().unwrap_or(TokenUsageInfo {
                total_token_usage: TokenUsage::default(),
                last_token_usage: TokenUsage::default(),
                model_context_window: None,
            });

            info.last_token_usage = TokenUsage {
                input_tokens: 0,
                cached_input_tokens: 0,
                cache_write_input_tokens: 0,
                output_tokens: 0,
                reasoning_output_tokens: 0,
                total_tokens: estimated_total_tokens.max(0),
            };

            if let Some(model_context_window) = turn_context.model_context_window() {
                info.model_context_window = Some(model_context_window);
            }

            state.set_token_info(Some(info));
        }
        self.set_auto_compact_window_estimated_prefill_for_scope(
            turn_context,
            estimated_total_tokens,
        )
        .await;
        self.send_token_count_event(turn_context).await;
    }

    pub(crate) async fn update_rate_limits(
        &self,
        turn_context: &TurnContext,
        new_rate_limits: RateLimitSnapshot,
    ) {
        self.record_rate_limits_info(new_rate_limits).await;
        self.send_token_count_event(turn_context).await;
    }

    pub(crate) async fn record_rate_limits_info(&self, new_rate_limits: RateLimitSnapshot) {
        {
            let mut state = self.state.lock().await;
            state.set_rate_limits(new_rate_limits);
        }
    }

    pub(crate) async fn mcp_dependency_prompted(&self) -> HashSet<String> {
        let state = self.state.lock().await;
        state.mcp_dependency_prompted()
    }

    pub(crate) async fn record_mcp_dependency_prompted<I>(&self, names: I)
    where
        I: IntoIterator<Item = String>,
    {
        let mut state = self.state.lock().await;
        state.record_mcp_dependency_prompted(names);
    }

    pub(crate) async fn set_server_reasoning_included(&self, included: bool) {
        let mut state = self.state.lock().await;
        state.set_server_reasoning_included(included);
    }

    pub(crate) async fn send_token_count_event(&self, turn_context: &TurnContext) {
        let (info, rate_limits, account_usage) = {
            let state = self.state.lock().await;
            let (info, rate_limits) = state.token_info_and_rate_limits();
            (info, rate_limits, state.account_token_usage())
        };
        let event = EventMsg::TokenCount(TokenCountEvent {
            info,
            rate_limits,
            account_usage,
        });
        self.send_event(turn_context, event).await;
    }

    pub(crate) async fn set_total_tokens_full(&self, turn_context: &TurnContext) {
        if let Some(context_window) = turn_context.model_context_window() {
            let mut state = self.state.lock().await;
            state.set_token_usage_full(context_window);
        }
        self.send_token_count_event(turn_context).await;
    }

    pub(crate) async fn record_response_item_and_emit_turn_item(
        &self,
        turn_context: &TurnContext,
        response_item: ResponseItem,
    ) {
        // Add to conversation history and persist response item to rollout.
        self.record_conversation_items(turn_context, std::slice::from_ref(&response_item))
            .await;

        // Derive a turn item and emit lifecycle events if applicable.
        if let Some(item) = parse_turn_item(&response_item) {
            self.emit_turn_item_started(turn_context, &item).await;
            self.emit_turn_item_completed(turn_context, item).await;
        }
    }

    pub(crate) async fn record_user_prompt_and_emit_turn_item(
        &self,
        turn_context: &TurnContext,
        input: &[UserInput],
        client_id: Option<String>,
    ) {
        // Persist the user message to history, but emit the turn item from `UserInput` so
        // UI-only `text_elements` are preserved. `ResponseItem::Message` does not carry
        // those spans, and `record_response_item_and_emit_turn_item` would drop them.
        let response_item = self.response_item_from_user_input(input.to_vec());
        // Input another agent submitted keeps that agent's standing (PF-30-S02).
        let origin = self
            .services
            .model_client()
            .take_agent_input_origin(input)
            .unwrap_or(crate::security::ingress::MessageOrigin::Human);
        self.record_conversation_items_from(
            turn_context,
            std::slice::from_ref(&response_item),
            Some(origin),
        )
        .await;
        let mut user_message_item = UserMessageItem::new(input);
        user_message_item.client_id = client_id;
        let turn_item = TurnItem::UserMessage(user_message_item);
        self.emit_turn_item_started(turn_context, &turn_item).await;
        self.emit_turn_item_completed(turn_context, turn_item).await;
        self.ensure_rollout_materialized().await;
    }

    pub(crate) async fn notify_stream_error(
        &self,
        turn_context: &TurnContext,
        message: impl Into<String>,
        codex_error: CodexErr,
    ) {
        let additional_details = codex_error.to_string();
        let codex_error_info = CodexErrorInfo::ResponseStreamDisconnected {
            http_status_code: codex_error.http_status_code_value(),
        };
        let event = EventMsg::StreamError(StreamErrorEvent {
            message: message.into(),
            codex_error_info: Some(codex_error_info),
            additional_details: Some(additional_details),
        });
        self.send_event(turn_context, event).await;
    }

    // Compare execution authority directly. User-layer reloads, model settings and
    // config provenance cannot affect this check.
    pub(super) fn authorization_matches(
        current: &SessionConfiguration,
        captured: &TurnContext,
    ) -> bool {
        current.approval_policy.value() == captured.approval_policy.value()
            && current
                .permission_profile()
                .materialize_project_roots_with_workspace_roots(&current.primary_workspace_roots())
                == captured.permission_profile
            && current.approvals_reviewer == captured.config.approvals_reviewer
            && current.windows_sandbox_level == captured.windows_sandbox_level
    }

    /// Inject additional user input into the currently active turn.
    ///
    /// Returns the active turn id when accepted.
    #[expect(
        clippy::await_holding_invalid_type,
        reason = "active turn checks and turn state updates must remain atomic"
    )]
    pub async fn steer_input(
        &self,
        input: Vec<UserInput>,
        additional_context: BTreeMap<String, AdditionalContextEntry>,
        expected_turn_id: Option<&str>,
        client_user_message_id: Option<String>,
        responsesapi_client_metadata: Option<HashMap<String, String>>,
    ) -> Result<String, SteerInputError> {
        let mut active = self.active_turn.lock().await;
        let Some(active_turn) = active.as_mut() else {
            return Err(SteerInputError::NoActiveTurn(input));
        };

        let Some(active_task) = active_turn.task.as_ref() else {
            return Err(SteerInputError::NoActiveTurn(input));
        };
        let active_turn_id = &active_task.turn_context.sub_id;

        if let Some(expected_turn_id) = expected_turn_id
            && expected_turn_id != active_turn_id
        {
            return Err(SteerInputError::ExpectedTurnMismatch {
                expected: expected_turn_id.to_string(),
                actual: active_turn_id.clone(),
            });
        }

        match active_task.kind {
            crate::state::TaskKind::Regular => {}
            crate::state::TaskKind::Review => {
                return Err(SteerInputError::ActiveTurnNotSteerable {
                    turn_kind: NonSteerableTurnKind::Review,
                });
            }
            crate::state::TaskKind::Compact => {
                return Err(SteerInputError::ActiveTurnNotSteerable {
                    turn_kind: NonSteerableTurnKind::Compact,
                });
            }
        }

        if input.is_empty() {
            return Err(SteerInputError::EmptyInput);
        }

        // Serialize authorization comparison and admission with settings updates.
        // A new context cannot re-sandbox existing exec sessions: steered work
        // could still send them instructions through write_stdin. Refuse admission
        // instead of changing running work or revoking an already granted approval.
        let mut state = self.state.lock().await;
        if !Self::authorization_matches(&state.session_configuration, &active_task.turn_context) {
            return Err(SteerInputError::AuthorizationChanged(input));
        }
        let additional_context_input = state.additional_context.merge(additional_context);

        if let Some(responsesapi_client_metadata) = responsesapi_client_metadata {
            active_task
                .turn_context
                .turn_metadata_state
                .set_responsesapi_client_metadata(responsesapi_client_metadata);
        }

        let mut pending_input = additional_context_input
            .into_iter()
            .map(ResponseItem::from)
            .map(TurnInput::ResponseItem)
            .collect::<Vec<_>>();
        pending_input.push(TurnInput::UserInput {
            content: input,
            client_id: client_user_message_id,
        });
        self.input_queue
            .extend_pending_input_and_accept_mailbox_delivery_for_turn_state(
                active_turn.turn_state.as_ref(),
                pending_input,
            )
            .await;
        drop(state);
        Ok(active_turn_id.clone())
    }

    #[expect(
        clippy::await_holding_invalid_type,
        reason = "active turn checks and turn state updates must remain atomic"
    )]
    pub(crate) async fn defer_user_input_until_active_turn_finished(
        &self,
        input: Vec<UserInput>,
        additional_context: BTreeMap<String, AdditionalContextEntry>,
        client_user_message_id: Option<String>,
        final_output_json_schema: Option<Value>,
    ) -> Result<String, SteerInputError> {
        let mut active = self.active_turn.lock().await;
        let Some(active_turn) = active.as_mut() else {
            return Err(SteerInputError::NoActiveTurn(input));
        };

        let Some(active_task) = active_turn.task.as_ref() else {
            return Err(SteerInputError::NoActiveTurn(input));
        };

        if input.is_empty() {
            return Err(SteerInputError::EmptyInput);
        }

        let additional_context_input = {
            let mut state = self.state.lock().await;
            state.additional_context.merge(additional_context)
        };

        let mut pending_input = additional_context_input
            .into_iter()
            .map(ResponseItem::from)
            .map(TurnInput::ResponseItem)
            .collect::<Vec<_>>();
        pending_input.push(TurnInput::UserInput {
            content: input,
            client_id: client_user_message_id,
        });
        self.input_queue
            .defer_input_for_turn_state(
                active_turn.turn_state.as_ref(),
                pending_input,
                final_output_json_schema,
            )
            .await;
        Ok(active_task.turn_context.sub_id.clone())
    }

    pub(crate) async fn submit_internal_follow_up(
        &self,
        op: Op,
        client_user_message_id: Option<String>,
    ) -> CodexResult<()> {
        let sub = Submission {
            id: Uuid::now_v7().to_string(),
            op,
            parent_turn_id: None,
            client_user_message_id,
            trace: current_span_w3c_trace_context(),
        };
        self.tx_sub
            .send(sub)
            .await
            .map_err(|_| CodexErr::InternalAgentDied)
    }

    pub(crate) async fn record_memory_citation_for_turn(&self, sub_id: &str) {
        let turn_state = self
            .input_queue
            .turn_state_for_sub_id(&self.active_turn, sub_id)
            .await;
        let Some(turn_state) = turn_state else {
            return;
        };
        turn_state.lock().await.has_memory_citation = true;
    }

    pub async fn interrupt_task(self: &Arc<Self>) {
        info!("interrupt received: abort current task, if any");
        let had_active_turn = self.active_turn.lock().await.is_some();
        self.abort_all_tasks(TurnAbortReason::Interrupted).await;
        if !had_active_turn {
            self.cancel_mcp_startup();
        }
    }

    pub(crate) fn hooks(&self) -> Arc<Hooks> {
        self.services.hooks.load_full()
    }

    pub(crate) fn user_shell(&self) -> Arc<shell::Shell> {
        Arc::clone(&self.services.user_shell)
    }

    pub(crate) async fn current_rollout_path(&self) -> anyhow::Result<Option<PathBuf>> {
        let Some(live_thread) = self.live_thread() else {
            return Ok(None);
        };
        live_thread.local_rollout_path().await.map_err(Into::into)
    }

    pub(crate) async fn hook_transcript_path(&self) -> Option<PathBuf> {
        let rollout_path = match self.current_rollout_path().await {
            Ok(Some(path)) => path,
            Ok(None) => return None,
            Err(err) => {
                warn!("{err}");
                return None;
            }
        };
        self.ensure_rollout_materialized().await;
        Some(rollout_path)
    }

    pub(crate) async fn take_pending_session_start_source(
        &self,
    ) -> Option<codex_hooks::SessionStartSource> {
        let mut state = self.state.lock().await;
        state.take_pending_session_start_source()
    }

    fn show_raw_agent_reasoning(&self) -> bool {
        self.services.show_raw_agent_reasoning
    }
}

pub(crate) fn emit_subagent_session_started(
    analytics_events_client: &AnalyticsEventsClient,
    client_metadata: AppServerClientMetadata,
    session_id: SessionId,
    thread_id: ThreadId,
    parent_thread_id: Option<ThreadId>,
    thread_config: ThreadConfigSnapshot,
    subagent_source: SubAgentSource,
) {
    let AppServerClientMetadata {
        client_name,
        client_version,
    } = client_metadata;
    let (Some(client_name), Some(client_version)) = (client_name, client_version) else {
        tracing::warn!("skipping subagent thread analytics: missing inherited client metadata");
        return;
    };
    let created_at = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    analytics_events_client.track_subagent_thread_started(SubAgentThreadStartedInput {
        session_id: session_id.to_string(),
        thread_id: thread_id.to_string(),
        parent_thread_id: parent_thread_id.map(|thread_id| thread_id.to_string()),
        forked_from_thread_id: thread_config
            .forked_from_thread_id
            .map(|thread_id| thread_id.to_string()),
        product_client_id: thread_config.originator.clone(),
        client_name,
        client_version,
        model: thread_config.model,
        ephemeral: thread_config.ephemeral,
        subagent_source,
        created_at,
    });
}

/// Builds the hook engine for one config snapshot, including any enabled plugin hooks.
async fn build_hooks_for_config(
    config: &Config,
    plugins_manager: &PluginsManager,
    environment: Option<&TurnEnvironment>,
) -> Hooks {
    let (hook_shell_program, hook_shell_argv) = environment
        .and_then(|environment| environment.shell.as_ref())
        .map(|shell| {
            let mut argv = shell.derive_exec_args("", /*use_login_shell*/ false);
            let program = argv.remove(0);
            let _ = argv.pop();
            (Some(program), argv)
        })
        .unwrap_or_default();
    let plugins_input = config.plugins_config_input();
    let plugin_outcome = plugins_manager.plugins_for_config(&plugins_input).await;
    let plugin_hook_sources = plugin_outcome.effective_plugin_hook_sources();
    let plugin_hook_load_warnings = plugin_outcome.effective_plugin_hook_warnings();
    Hooks::new(HooksConfig {
        legacy_notify_argv: config.notify.clone(),
        feature_enabled: config.features.enabled(Feature::CodexHooks),
        bypass_hook_trust: config.bypass_hook_trust,
        config_layer_stack: Some(config.config_layer_stack.clone()),
        plugin_hook_sources,
        plugin_hook_load_warnings,
        shell_program: hook_shell_program,
        shell_args: hook_shell_argv,
    })
}

#[cfg(test)]
#[path = "elicitation_holders_tests.rs"]
mod elicitation_holders_tests;

#[cfg(test)]
pub(crate) mod tests;

#[cfg(test)]
#[path = "pending_mailbox_wake_tests.rs"]
mod pending_mailbox_wake_tests;

#[cfg(test)]
#[path = "account_switch_tests.rs"]
mod account_switch_tests;

#[cfg(test)]
#[path = "home_access_session_tests.rs"]
mod home_access_session_tests;

#[cfg(test)]
#[path = "stale_approval_tests.rs"]
mod stale_approval_tests;

#[cfg(test)]
#[path = "withdrawn_approval_tests.rs"]
mod withdrawn_approval_tests;
