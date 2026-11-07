//! Top-level TUI application state and runtime wiring.
//!
//! This module owns the `App` struct, shared imports, and the high-level run loop that coordinates
//! the focused app submodules.

use crate::AppServerTarget;
use crate::app_backtrack::BacktrackState;
use crate::app_command::AppCommand;
use crate::app_event::AppEvent;
use crate::app_event::ExitMode;
use crate::app_event::FeedbackCategory;
use crate::app_event::HistoryLookupResponse;
use crate::app_event::PermissionProfileSelection;
use crate::app_event::PluginLocation;
use crate::app_event::PluginRemoteSectionError;
use crate::app_event::RateLimitRefreshOrigin;
#[cfg(target_os = "windows")]
use crate::app_event::WindowsSandboxEnableMode;
use crate::app_event_sender::AppEventSender;
use crate::app_server_session::AppServerBootstrap;
use crate::app_server_session::AppServerSession;
use crate::app_server_session::AppServerStartedThread;
use crate::app_server_session::ResumeModelOverride;
use crate::app_server_session::TurnPermissionsOverride;
use crate::app_server_session::app_server_rate_limit_snapshots;
use crate::bottom_pane::AppLinkViewParams;
use crate::bottom_pane::ApplyPatchApprovalRequest;
use crate::bottom_pane::ApprovalRequest;
use crate::bottom_pane::ExecApprovalRequest;
use crate::bottom_pane::FeedbackAudience;
use crate::bottom_pane::McpElicitationApprovalRequest;
use crate::bottom_pane::McpServerElicitationFormRequest;
use crate::bottom_pane::PermissionsApprovalRequest;
use crate::bottom_pane::SelectionItem;
use crate::bottom_pane::SelectionViewParams;
use crate::bottom_pane::popup_consts::standard_popup_hint_line;
use crate::chatwidget::ChatWidget;
use crate::chatwidget::ExternalEditorState;
use crate::chatwidget::ReplayKind;
use crate::chatwidget::ThreadInputState;
use crate::cwd_prompt::CwdPromptAction;
use crate::diff_render::DiffSummary;
use crate::exec_command::split_command_string;
use crate::exec_command::strip_bash_lc_and_escape;
use crate::external_editor;
use crate::file_search::FileSearchManager;
use crate::history_cell;
use crate::history_cell::HistoryCell;
#[cfg(not(debug_assertions))]
use crate::history_cell::UpdateAvailableHistoryCell;
use crate::hooks_rpc::HookTrustUpdate;
use crate::key_hint::KeyBindingListExt;
use crate::keymap::RuntimeKeymap;
use crate::legacy_core::config::Config;
use crate::legacy_core::config::ConfigBuilder;
use crate::legacy_core::config::ConfigOverrides;
use crate::legacy_core::config::PermissionProfileSnapshot;
use crate::legacy_core::config::edit::ConfigEditsBuilder;
use crate::managed_new_thread_defaults::apply_managed_new_thread_defaults;
use crate::model_catalog::ModelCatalog;
use crate::model_migration::ModelMigrationOutcome;
use crate::model_migration::migration_copy_for_models;
use crate::model_migration::run_model_migration_prompt;
use crate::multi_agents::agent_picker_status_dot_spans;
use crate::multi_agents::format_agent_picker_item_name;
use crate::multi_agents::next_agent_shortcut_matches;
use crate::multi_agents::previous_agent_shortcut_matches;
use crate::multi_agents::sub_agent_activity_display;
use crate::pager_overlay::Overlay;
use crate::render::highlight::highlight_bash_to_lines;
use crate::render::renderable::Renderable;
use crate::resume_picker::SessionSelection;
use crate::resume_picker::SessionTarget;
use crate::session_state::ThreadSessionState;
#[cfg(test)]
use crate::test_support::PathBufExt;
#[cfg(test)]
use crate::test_support::test_path_buf;
#[cfg(test)]
use crate::test_support::test_path_display;
use crate::token_usage::TokenUsage;
use crate::transcript_reflow::TranscriptReflowState;
use crate::tui;
use crate::tui::TuiEvent;
use crate::update_action::UpdateAction;
use crate::version::CODEX_CLI_VERSION;
use crate::workspace_command::AppServerWorkspaceCommandRunner;
use crate::workspace_command::WorkspaceCommandRunner;
use codex_ansi_escape::ansi_escape_line;
use codex_app_server_client::AppServerRequestHandle;
use codex_app_server_client::TypedRequestError;
use codex_app_server_protocol::AddCreditsNudgeCreditType;
use codex_app_server_protocol::AskForApproval;
use codex_app_server_protocol::ClientRequest;
use codex_app_server_protocol::CodexErrorInfo as AppServerCodexErrorInfo;
use codex_app_server_protocol::ConfigBatchWriteParams;
use codex_app_server_protocol::ConfigReadResponse;
use codex_app_server_protocol::ConfigValueWriteParams;
use codex_app_server_protocol::ConfigWriteResponse;
use codex_app_server_protocol::ErrorNotification;
use codex_app_server_protocol::FeedbackUploadParams;
use codex_app_server_protocol::FeedbackUploadResponse;
use codex_app_server_protocol::GetAccountRateLimitsResponse;
use codex_app_server_protocol::HooksListEntry;
use codex_app_server_protocol::ListMcpServerStatusParams;
use codex_app_server_protocol::ListMcpServerStatusResponse;
#[cfg(test)]
use codex_app_server_protocol::McpAuthStatus;
use codex_app_server_protocol::McpServerStatus;
use codex_app_server_protocol::McpServerStatusDetail;
use codex_app_server_protocol::MergeStrategy;
use codex_app_server_protocol::PluginInstallParams;
use codex_app_server_protocol::PluginInstallResponse;
use codex_app_server_protocol::PluginListMarketplaceKind;
use codex_app_server_protocol::PluginListParams;
use codex_app_server_protocol::PluginListResponse;
use codex_app_server_protocol::PluginMarketplaceEntry;
use codex_app_server_protocol::PluginReadParams;
use codex_app_server_protocol::PluginReadResponse;
use codex_app_server_protocol::PluginUninstallParams;
use codex_app_server_protocol::PluginUninstallResponse;
use codex_app_server_protocol::SandboxMode as AppServerSandboxMode;
use codex_app_server_protocol::SendAddCreditsNudgeEmailParams;
use codex_app_server_protocol::ServerNotification;
use codex_app_server_protocol::ServerRequest;
use codex_app_server_protocol::SkillErrorInfo;
use codex_app_server_protocol::SkillsListParams;
use codex_app_server_protocol::SkillsListResponse;
use codex_app_server_protocol::ThreadItem;
use codex_app_server_protocol::ThreadLoadedListParams;
use codex_app_server_protocol::ThreadMemoryMode;
use codex_app_server_protocol::ThreadStartSource;
use codex_app_server_protocol::Turn;
use codex_app_server_protocol::TurnError as AppServerTurnError;
use codex_app_server_protocol::TurnStatus;
use codex_app_server_protocol::WriteStatus;
use codex_config::CloudConfigBundleLoader;
use codex_config::ConfigLayerStackOrdering;
use codex_config::LoaderOverrides;
use codex_config::types::ApprovalsReviewer;
use codex_config::types::MemoriesToml;
#[cfg(test)]
use codex_config::types::ModelAvailabilityNuxConfig;
#[cfg(target_os = "windows")]
use codex_config::types::WindowsToml;
use codex_exec_server::EnvironmentManager;
use codex_features::Feature;
use codex_features::FeaturesToml;
use codex_model_provider::create_model_provider;
use codex_model_provider_info::ModelProviderInfo;
use codex_models_manager::model_presets::HIDE_GPT_5_1_CODEX_MAX_MIGRATION_PROMPT_CONFIG;
use codex_models_manager::model_presets::HIDE_GPT5_1_MIGRATION_PROMPT_CONFIG;
use codex_otel::SessionTelemetry;
use codex_protocol::ThreadId;
use codex_protocol::config_types::Personality;
#[cfg(target_os = "windows")]
use codex_protocol::config_types::WindowsSandboxLevel;
use codex_protocol::models::ActivePermissionProfile;
use codex_protocol::models::BUILT_IN_PERMISSION_PROFILE_WORKSPACE;
use codex_protocol::models::PermissionProfile;
use codex_protocol::openai_models::InputModality;
#[cfg(test)]
use codex_protocol::openai_models::ModelAvailabilityNux;
use codex_protocol::openai_models::ModelBilling;
use codex_protocol::openai_models::ModelCapabilityTier;
use codex_protocol::openai_models::ModelOrchestrationMetadata;
use codex_protocol::openai_models::ModelPreset;
use codex_protocol::openai_models::ModelUpgrade;
use codex_protocol::openai_models::ReasoningEffort as ReasoningEffortConfig;
#[cfg(target_os = "windows")]
use codex_protocol::permissions::FileSystemSandboxKind;
use codex_rollout::StateDbHandle;
use codex_terminal_detection::user_agent;
use codex_utils_absolute_path::AbsolutePathBuf;
use codex_utils_approval_presets::builtin_permission_profile_for_active_permission_profile;
use color_eyre::eyre::Result;
use color_eyre::eyre::WrapErr;
use crossterm::event::KeyCode;
use crossterm::event::KeyEvent;
use crossterm::event::KeyEventKind;
use crossterm::event::KeyModifiers;
use ratatui::backend::Backend;
use ratatui::layout::Rect;
use ratatui::style::Stylize;
use ratatui::text::Line;
use ratatui::widgets::Paragraph;
use ratatui::widgets::Wrap;
use std::collections::BTreeMap;
use std::collections::HashMap;
use std::collections::HashSet;
use std::collections::VecDeque;
use std::io::Write;
use std::path::Path;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::sync::atomic::AtomicU64;
use std::sync::atomic::AtomicUsize;
use std::sync::atomic::Ordering;
use std::thread;
use std::time::Duration;
use std::time::Instant;
use tokio::select;
use tokio::sync::Mutex;
use tokio::sync::mpsc;
use tokio::sync::mpsc::error::TryRecvError;
use tokio::sync::mpsc::error::TrySendError;
use tokio::sync::mpsc::unbounded_channel;
use tokio::task::JoinHandle;
use tokio_stream::Stream;
use tokio_stream::StreamExt;
use toml::Value as TomlValue;
use uuid::Uuid;
mod agent_message_consolidation;
mod agent_navigation;
mod agent_picker;
mod agent_status_feed;
mod app_server_event_targets;
mod app_server_events;
pub(crate) mod app_server_requests;
mod background_child;
mod background_requests;
mod config_persistence;
pub(crate) use config_persistence::resume_model_settings_for_overrides;
mod event_dispatch;
mod history_ui;
mod input;
mod loaded_threads;
mod pending_interactive_replay;
mod permission_confirmation;
mod pets;
mod platform_actions;
mod plugin_mentions;
mod provider_accounts;
mod provider_management;
mod provider_management_auth;
mod provider_management_status;
mod replay_filter;
mod resize_reflow;
mod resume_defaults;
mod safety_buffering;
mod session_lifecycle;
mod side;
mod startup_prompts;
mod thread_events;
mod thread_goal_actions;
mod thread_routing;
mod thread_session_state;
pub(crate) mod thread_settings;

use self::agent_navigation::AgentNavigationDirection;
use self::agent_navigation::AgentNavigationState;
use self::app_server_requests::PendingAppServerRequests;
use self::loaded_threads::find_loaded_subagent_threads_for_primary;
use self::pending_interactive_replay::PendingInteractiveReplayState;
use self::platform_actions::*;
use self::side::SideParentStatus;
use self::side::SideParentStatusChange;
use self::side::SideThreadState;
use self::startup_prompts::*;
use self::thread_events::*;

const EXTERNAL_EDITOR_HINT: &str = "Save and close external editor to continue.";
const THREAD_EVENT_CHANNEL_CAPACITY: usize = 32768;
const TUI_INPUT_WATCHDOG_INTERVAL: Duration = Duration::from_secs(5);
const TUI_INPUT_WATCHDOG_STALLED_AFTER: Duration = Duration::from_secs(10);
const WHIP_SWEEP_INTERVAL: Duration = Duration::from_secs(45);

type BoxedTuiEventStream = Pin<Box<dyn Stream<Item = TuiEvent> + Send + 'static>>;

struct DrainedTuiEvents {
    rx: mpsc::UnboundedReceiver<TuiEvent>,
    watchdog: TuiInputDrainWatchdog,
}

#[derive(Clone)]
struct TuiInputDrainWatchdog {
    pending_events: Arc<AtomicUsize>,
    last_drained_ms: Arc<AtomicU64>,
    last_handled_ms: Arc<AtomicU64>,
    started_at: Instant,
}

impl TuiInputDrainWatchdog {
    fn new() -> Self {
        let started_at = Instant::now();
        Self {
            pending_events: Arc::new(AtomicUsize::new(0)),
            last_drained_ms: Arc::new(AtomicU64::new(0)),
            last_handled_ms: Arc::new(AtomicU64::new(0)),
            started_at,
        }
    }

    fn elapsed_ms(&self) -> u64 {
        self.started_at
            .elapsed()
            .as_millis()
            .min(u128::from(u64::MAX)) as u64
    }

    fn note_drained(&self) {
        self.last_drained_ms
            .store(self.elapsed_ms(), Ordering::Relaxed);
        self.pending_events.fetch_add(1, Ordering::Relaxed);
    }

    fn note_drainer_send_failed(&self) {
        self.pending_events
            .try_update(Ordering::Relaxed, Ordering::Relaxed, |pending| {
                pending.checked_sub(1)
            })
            .ok();
    }

    fn note_handled(&self) {
        self.last_handled_ms
            .store(self.elapsed_ms(), Ordering::Relaxed);
        self.pending_events
            .try_update(Ordering::Relaxed, Ordering::Relaxed, |pending| {
                pending.checked_sub(1)
            })
            .ok();
    }
}

fn spawn_tui_event_drainer(mut tui_events: BoxedTuiEventStream) -> DrainedTuiEvents {
    let (tx, rx) = mpsc::unbounded_channel();
    let watchdog = TuiInputDrainWatchdog::new();
    let drainer_watchdog = watchdog.clone();
    tokio::spawn(async move {
        while let Some(event) = tui_events.next().await {
            drainer_watchdog.note_drained();
            if tx.send(event).is_err() {
                drainer_watchdog.note_drainer_send_failed();
                break;
            }
        }
    });

    DrainedTuiEvents { rx, watchdog }
}

/// Lend the single input queue to a modal without losing watchdog accounting.
fn modal_tui_events<'a>(
    rx: &'a mut mpsc::UnboundedReceiver<TuiEvent>,
    watchdog: &'a TuiInputDrainWatchdog,
) -> impl Stream<Item = TuiEvent> + Send + Unpin + 'a {
    ModalTuiEvents { rx, watchdog }
}

struct ModalTuiEvents<'a> {
    rx: &'a mut mpsc::UnboundedReceiver<TuiEvent>,
    watchdog: &'a TuiInputDrainWatchdog,
}

impl Stream for ModalTuiEvents<'_> {
    type Item = TuiEvent;

    fn poll_next(
        self: Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Option<Self::Item>> {
        let this = self.get_mut();
        this.rx.poll_recv(cx).map(|event| {
            if event.is_some() {
                this.watchdog.note_handled();
            }
            event
        })
    }
}

fn spawn_tui_input_watchdog(
    watchdog: TuiInputDrainWatchdog,
    frame_requester: tui::FrameRequester,
) -> JoinHandle<()> {
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(TUI_INPUT_WATCHDOG_INTERVAL);
        loop {
            interval.tick().await;
            let pending_events = watchdog.pending_events.load(Ordering::Relaxed);
            if pending_events == 0 {
                continue;
            }

            let now_ms = watchdog.elapsed_ms();
            let last_handled_ms = watchdog.last_handled_ms.load(Ordering::Relaxed);
            if now_ms.saturating_sub(last_handled_ms)
                >= TUI_INPUT_WATCHDOG_STALLED_AFTER.as_millis() as u64
            {
                tracing::warn!(
                    pending_events,
                    last_drained_ms = watchdog.last_drained_ms.load(Ordering::Relaxed),
                    last_handled_ms,
                    "tui input watchdog observed queued terminal events without handler progress"
                );
                frame_requester.schedule_frame();
            }
        }
    })
}

fn spawn_whip_sweep_tick(app_event_tx: AppEventSender) -> JoinHandle<()> {
    tokio::spawn(async move {
        let interval_duration = if std::env::var("PFTERMINAL_ORCHESTRATE_QA").as_deref() == Ok("1")
        {
            Duration::from_millis(250)
        } else {
            WHIP_SWEEP_INTERVAL
        };
        let mut interval = tokio::time::interval(interval_duration);
        loop {
            interval.tick().await;
            app_event_tx.send(AppEvent::WhipSweepTick);
        }
    })
}

enum ThreadInteractiveRequest {
    AppLink(AppLinkViewParams),
    Approval(ApprovalRequest),
    McpServerElicitation(McpServerElicitationFormRequest),
}

/// Extracts `receiver_thread_ids` from collab agent tool-call notifications.
///
/// Only `ItemStarted` and `ItemCompleted` notifications with a `CollabAgentToolCall` item carry
/// receiver thread ids. All other notification variants return `None`.
fn collab_receiver_thread_ids(notification: &ServerNotification) -> Option<&[String]> {
    match notification {
        ServerNotification::ItemStarted(notification) => match &notification.item {
            ThreadItem::CollabAgentToolCall {
                receiver_thread_ids,
                ..
            } => Some(receiver_thread_ids),
            _ => None,
        },
        ServerNotification::ItemCompleted(notification) => match &notification.item {
            ThreadItem::CollabAgentToolCall {
                receiver_thread_ids,
                ..
            } => Some(receiver_thread_ids),
            _ => None,
        },
        _ => None,
    }
}

fn collab_sender_thread_id(notification: &ServerNotification) -> Option<ThreadId> {
    let sender_thread_id = match notification {
        ServerNotification::ItemStarted(notification) => match &notification.item {
            ThreadItem::CollabAgentToolCall {
                sender_thread_id, ..
            } => sender_thread_id,
            _ => return None,
        },
        ServerNotification::ItemCompleted(notification) => match &notification.item {
            ThreadItem::CollabAgentToolCall {
                sender_thread_id, ..
            } => sender_thread_id,
            _ => return None,
        },
        _ => return None,
    };
    ThreadId::from_string(sender_thread_id).ok()
}

fn collab_receiver_status<'a>(
    notification: &'a ServerNotification,
    receiver_thread_id: &str,
) -> Option<&'a codex_app_server_protocol::CollabAgentState> {
    match notification {
        ServerNotification::ItemStarted(notification) => match &notification.item {
            ThreadItem::CollabAgentToolCall { agents_states, .. } => {
                agents_states.get(receiver_thread_id)
            }
            _ => None,
        },
        ServerNotification::ItemCompleted(notification) => match &notification.item {
            ThreadItem::CollabAgentToolCall { agents_states, .. } => {
                agents_states.get(receiver_thread_id)
            }
            _ => None,
        },
        _ => None,
    }
}

fn sub_agent_activity_item(notification: &ServerNotification) -> Option<&ThreadItem> {
    match notification {
        ServerNotification::ItemStarted(notification) => match &notification.item {
            ThreadItem::SubAgentActivity { .. } => Some(&notification.item),
            _ => None,
        },
        ServerNotification::ItemCompleted(notification) => match &notification.item {
            ThreadItem::SubAgentActivity { .. } => Some(&notification.item),
            _ => None,
        },
        _ => None,
    }
}

fn collab_receiver_is_not_found(
    notification: &ServerNotification,
    receiver_thread_id: &str,
) -> bool {
    match notification {
        ServerNotification::ItemCompleted(notification) => match &notification.item {
            ThreadItem::CollabAgentToolCall { agents_states, .. } => {
                agents_states.get(receiver_thread_id).is_some_and(|state| {
                    matches!(
                        &state.status,
                        codex_app_server_protocol::CollabAgentStatus::NotFound
                    )
                })
            }
            _ => false,
        },
        _ => false,
    }
}

fn default_exec_approval_decisions(
    network_approval_context: Option<&codex_app_server_protocol::NetworkApprovalContext>,
    proposed_execpolicy_amendment: Option<&codex_app_server_protocol::ExecPolicyAmendment>,
    proposed_network_policy_amendments: Option<
        &[codex_app_server_protocol::NetworkPolicyAmendment],
    >,
    additional_permissions: Option<&codex_app_server_protocol::AdditionalPermissionProfile>,
) -> Vec<codex_app_server_protocol::CommandExecutionApprovalDecision> {
    use codex_app_server_protocol::CommandExecutionApprovalDecision;
    use codex_app_server_protocol::NetworkPolicyRuleAction;

    if network_approval_context.is_some() {
        let mut decisions = vec![
            CommandExecutionApprovalDecision::Accept,
            CommandExecutionApprovalDecision::AcceptForSession,
        ];
        if let Some(amendment) = proposed_network_policy_amendments.and_then(|amendments| {
            amendments
                .iter()
                .find(|amendment| amendment.action == NetworkPolicyRuleAction::Allow)
        }) {
            decisions.push(
                CommandExecutionApprovalDecision::ApplyNetworkPolicyAmendment {
                    network_policy_amendment: amendment.clone(),
                },
            );
        }
        decisions.push(CommandExecutionApprovalDecision::Cancel);
        return decisions;
    }

    if additional_permissions.is_some() {
        return vec![
            CommandExecutionApprovalDecision::Accept,
            CommandExecutionApprovalDecision::Cancel,
        ];
    }

    let mut decisions = vec![CommandExecutionApprovalDecision::Accept];
    if let Some(execpolicy_amendment) = proposed_execpolicy_amendment {
        decisions.push(
            CommandExecutionApprovalDecision::AcceptWithExecpolicyAmendment {
                execpolicy_amendment: execpolicy_amendment.clone(),
            },
        );
    }
    decisions.push(CommandExecutionApprovalDecision::Cancel);
    decisions
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct AutoReviewMode {
    approval_policy: AskForApproval,
    approvals_reviewer: ApprovalsReviewer,
    active_permission_profile: ActivePermissionProfile,
}

/// Enabling the Auto-review experiment in the TUI should also switch the
/// current `/permissions` settings to the matching Auto-review mode. Users
/// can still change `/permissions` afterward; this just assumes that opting into
/// the experiment means they want Auto-review enabled immediately.
fn auto_review_mode() -> AutoReviewMode {
    AutoReviewMode {
        approval_policy: AskForApproval::OnRequest,
        approvals_reviewer: ApprovalsReviewer::AutoReview,
        active_permission_profile: ActivePermissionProfile::new(
            BUILT_IN_PERMISSION_PROFILE_WORKSPACE,
        ),
    }
}

#[cfg(test)]
impl AutoReviewMode {
    fn permission_profile(&self) -> PermissionProfile {
        builtin_permission_profile_for_active_permission_profile(&self.active_permission_profile)
            .expect("auto-review mode should use a built-in permission profile")
    }
}

#[cfg(target_os = "windows")]
fn managed_filesystem_sandbox_is_restricted(permission_profile: &PermissionProfile) -> bool {
    matches!(
        permission_profile.file_system_sandbox_policy().kind,
        FileSystemSandboxKind::Restricted
    )
}

/// Baseline cadence for periodic stream commit animation ticks.
///
/// Smooth-mode streaming drains one line per tick, so this interval controls
/// perceived typing speed for non-backlogged output.
const COMMIT_ANIMATION_TICK: Duration = tui::TARGET_FRAME_INTERVAL;

#[derive(Debug, Clone)]
pub struct AppExitInfo {
    pub token_usage: TokenUsage,
    pub thread_id: Option<ThreadId>,
    pub resume_hint: Option<String>,
    pub update_action: Option<UpdateAction>,
    pub exit_reason: ExitReason,
}

impl AppExitInfo {
    pub fn fatal(message: impl Into<String>) -> Self {
        Self {
            token_usage: TokenUsage::default(),
            thread_id: None,
            resume_hint: None,
            update_action: None,
            exit_reason: ExitReason::Fatal(message.into()),
        }
    }
}

#[derive(Debug)]
pub(crate) enum AppRunControl {
    Continue,
    Exit(ExitReason),
}

#[derive(Debug, Clone)]
pub enum ExitReason {
    UserRequested,
    Fatal(String),
    /// `/security` "restart now" (PF-24-S02): start again with the same
    /// options so a saved level takes effect.
    Restart,
}

fn session_summary(
    token_usage: TokenUsage,
    thread_id: Option<ThreadId>,
    thread_name: Option<String>,
    rollout_path: Option<&Path>,
) -> Option<SessionSummary> {
    let usage_line = (!token_usage.is_zero()).then(|| token_usage.to_string());
    let resume_hint = resume_hint_for_resumable_thread(thread_id, thread_name, rollout_path);

    if usage_line.is_none() && resume_hint.is_none() {
        return None;
    }

    Some(SessionSummary {
        usage_line,
        resume_hint,
    })
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ResumableThread {
    thread_id: ThreadId,
    thread_name: Option<String>,
}

fn resumable_thread(
    thread_id: Option<ThreadId>,
    thread_name: Option<String>,
    rollout_path: Option<&Path>,
) -> Option<ResumableThread> {
    let thread_id = thread_id?;
    let rollout_path = rollout_path?;
    rollout_path_is_resumable(rollout_path).then_some(ResumableThread {
        thread_id,
        thread_name,
    })
}

fn resume_hint_for_resumable_thread(
    thread_id: Option<ThreadId>,
    thread_name: Option<String>,
    rollout_path: Option<&Path>,
) -> Option<String> {
    let thread = resumable_thread(thread_id, thread_name, rollout_path)?;
    resume_hint_for_thread(&thread)
}

fn resume_hint_for_thread(thread: &ResumableThread) -> Option<String> {
    codex_utils_cli::resume_hint(thread.thread_name.as_deref(), Some(thread.thread_id))
}

fn resume_hint_for_pane_layout_thread(
    codex_home: &Path,
    thread_id: Option<ThreadId>,
) -> Option<String> {
    let thread_id = thread_id?;
    let thread_id_text = thread_id.to_string();
    crate::claude_panes::load_pane_layout(codex_home, Some(&thread_id_text))?;
    codex_utils_cli::resume_hint(/*thread_name*/ None, Some(thread_id))
}

fn rollout_path_is_resumable(rollout_path: &Path) -> bool {
    std::fs::metadata(rollout_path).is_ok_and(|metadata| metadata.is_file() && metadata.len() > 0)
}

fn errors_for_cwd(cwd: &Path, response: &SkillsListResponse) -> Vec<SkillErrorInfo> {
    response
        .data
        .iter()
        .find(|entry| entry.cwd.as_path() == cwd)
        .map(|entry| entry.errors.clone())
        .unwrap_or_default()
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct SessionSummary {
    usage_line: Option<String>,
    resume_hint: Option<String>,
}

#[derive(Debug, Default)]
struct InitialHistoryReplayBuffer {
    retained_lines: VecDeque<crate::terminal_hyperlinks::HyperlinkLine>,
    render_from_transcript_tail: bool,
}

pub(crate) struct App {
    model_catalog: Arc<ModelCatalog>,
    pub(crate) session_telemetry: SessionTelemetry,
    pub(crate) app_event_tx: AppEventSender,
    pub(crate) chat_widget: ChatWidget,
    workspace_command_runner: Option<WorkspaceCommandRunner>,
    /// Config is stored here so we can recreate ChatWidgets as needed.
    pub(crate) config: Config,
    pub(crate) active_deferred_provider_setup:
        Option<crate::onboarding::provider_setup::DeferredProviderSetup>,
    pub(crate) shared_provider_setup_session:
        Option<crate::onboarding::provider_setup::ProviderSetupSession>,
    pub(crate) shared_provider_status_host: Option<crate::provider_status_host::ProviderStatusHost>,
    pub(crate) shared_provider_account_auth_host:
        Option<crate::provider_account_auth_host::ProviderAccountAuthHost>,
    pub(crate) provider_management_host:
        Option<crate::provider_management_host::ProviderManagementHost>,
    pub(crate) provider_management_generation: u64,
    pub(crate) pending_wallet_create_deferred:
        Option<crate::onboarding::provider_setup::DeferredProviderSetup>,
    launch_cwd: PathBuf,
    pub(crate) state_db: Option<StateDbHandle>,
    cli_kv_overrides: Vec<(String, TomlValue)>,
    harness_overrides: ConfigOverrides,
    loader_overrides: LoaderOverrides,
    cloud_config_bundle: CloudConfigBundleLoader,
    runtime_approval_policy_override: Option<AskForApproval>,
    runtime_permission_profile_override: Option<RuntimePermissionProfileOverride>,
    pending_permission_confirmation: Option<permission_confirmation::PendingPermissionConfirmation>,

    pub(crate) file_search: FileSearchManager,

    pub(crate) transcript_cells: Vec<Arc<dyn HistoryCell>>,
    pub(crate) claude_pane_transcript_cells: HashMap<String, Vec<Arc<dyn HistoryCell>>>,

    // Pager overlay state (Transcript or Static like Diff)
    pub(crate) overlay: Option<Overlay>,
    pub(crate) deferred_history_lines: Vec<crate::terminal_hyperlinks::HyperlinkLine>,
    has_emitted_history_lines: bool,
    transcript_reflow: TranscriptReflowState,
    initial_history_replay_buffer: Option<InitialHistoryReplayBuffer>,

    pub(crate) enhanced_keys_supported: bool,
    pub(crate) keymap: RuntimeKeymap,

    /// Controls the animation thread that sends CommitTick events.
    pub(crate) commit_anim_running: Arc<AtomicBool>,
    // Shared across ChatWidget instances so invalid status-line config warnings only emit once.
    status_line_invalid_items_warned: Arc<AtomicBool>,
    // Shared across ChatWidget instances so invalid terminal-title config warnings only emit once.
    terminal_title_invalid_items_warned: Arc<AtomicBool>,
    // Tracks active skill-load warnings so refreshes do not duplicate history cells.
    skill_load_warnings: SkillLoadWarningState,
    skills_refresh_generation: Arc<AtomicU64>,

    // Esc-backtracking state grouped
    pub(crate) backtrack: crate::app_backtrack::BacktrackState,
    /// When set, the next draw rebuilds terminal scrollback from the retained transcript cells.
    ///
    /// This is used after a confirmed thread rollback to ensure scrollback reflects the trimmed
    /// transcript cells.
    pub(crate) backtrack_render_pending: bool,
    pub(crate) feedback: codex_feedback::CodexFeedback,
    feedback_audience: FeedbackAudience,
    environment_manager: Arc<EnvironmentManager>,
    app_server_target: AppServerTarget,
    /// Set when the user confirms an update; propagated on exit.
    pub(crate) pending_update_action: Option<UpdateAction>,

    /// Tracks the thread we intentionally shut down while exiting the app.
    ///
    /// When this matches the active thread, its `ShutdownComplete` should lead to
    /// process exit instead of being treated as an unexpected sub-agent death that
    /// triggers failover to the primary thread.
    ///
    /// This is thread-scoped state (`Option<ThreadId>`) instead of a global bool
    /// so shutdown events from other threads still take the normal failover path.
    pending_shutdown_exit_thread_id: Option<ThreadId>,

    windows_sandbox: WindowsSandboxState,

    thread_event_channels: HashMap<ThreadId, ThreadEventChannel>,
    thread_event_listener_tasks: HashMap<ThreadId, JoinHandle<()>>,
    pub(crate) agent_navigation: AgentNavigationState,
    pub(crate) spawn_parent_by_thread: HashMap<ThreadId, ThreadId>,
    pub(crate) spawn_parent_by_node: HashMap<String, String>,
    pub(crate) spawn_native_runtime_by_node:
        HashMap<String, crate::dispatch_queue::SavedNativeSpawnRuntime>,
    pub(crate) spawn_native_endpoint_by_node: HashMap<String, ThreadId>,
    pub(crate) spawn_crew: Option<crate::crew_state::CrewInstanceState>,
    /// A restored pre-CrewSpec hierarchy remains inspectable but cannot be mutated by the native
    /// control plane because its identity and parentage cannot be proven.
    pub(crate) spawn_legacy_read_only: bool,
    pub(crate) spawn_status_by_thread:
        HashMap<ThreadId, codex_app_server_protocol::CollabAgentState>,
    /// Active `wait_agent` call by native spawn thread, keyed to the exact turn and item so a
    /// delayed completion from an older wait cannot clear a newer wait state.
    pub(crate) spawn_waiting_for_agents_by_thread: HashMap<ThreadId, (String, String)>,
    pub(crate) spawn_parent_reports_by_node: HashMap<String, VecDeque<String>>,
    pub(crate) spawn_dispatch_acks_by_target_task:
        HashMap<(String, String), VecDeque<crate::spawn_orchestration::SpawnDispatchAck>>,
    pub(crate) spawn_next_dispatch_seq: u64,
    pub(crate) spawn_processed_dispatch_seq_ids: HashSet<u64>,
    pub(crate) spawn_processed_dispatch_origins: HashSet<String>,
    /// Terminal turn notifications are observed both on receipt and during buffered replay. Keep
    /// orchestration side effects idempotent across those two delivery paths.
    pub(crate) spawn_processed_terminal_turns: HashSet<(ThreadId, String)>,
    /// Edge-adapter loop breaker for external Claude-pane child-report turns. Native crew
    /// lifecycle policy lives in Core's agent controller.
    pub(crate) spawn_auto_loop_state_by_node:
        HashMap<String, crate::spawn_orchestration::SpawnAutoLoopState>,
    /// False after resume until a live operator/non-auto turn proves current intent.
    pub(crate) spawn_operator_input_seen: bool,
    /// #507: the task another agent last sent to each Claude pane while the
    /// kill switch was on (one per pane); it goes with that pane's next turn
    /// after the switch is off (turning it off sends nothing).
    pub(crate) kill_switch_held_claude_tasks: Vec<(String, String)>,
    /// #660 follow-up: the managed Codex panes whose mailbox holds a task
    /// another agent sent while the kill switch was on, until that pane's
    /// next turn starts (it goes with that turn).
    pub(crate) kill_switch_held_native_tasks: Vec<codex_protocol::ThreadId>,
    /// #507: whether this app last saw the kill switch on, so a change
    /// reported twice (here and by the home's watch) is handled once.
    pub(crate) kill_switch_known_on: bool,
    pub(crate) spawn_quarantine_notified_by_node: HashSet<String>,
    pub(crate) spawn_context_left_by_thread: HashMap<ThreadId, i64>,
    pub(crate) spawn_last_report_seq_by_node: HashMap<String, u64>,
    pub(crate) spawn_last_dispatch_seq_by_node: HashMap<String, u64>,
    pub(crate) spawn_last_event_at_by_node: HashMap<String, String>,
    pub(crate) spawn_nazgul_pane_id: Option<String>,
    pub(crate) spawn_nazgul_rebind_required: bool,
    #[allow(clippy::box_collection)]
    pub(crate) orchestrate_whips: Box<HashMap<String, crate::orchestrate::Whip>>,
    pub(crate) orchestrate_next_whip_seq: u64,
    #[cfg(test)]
    pub(crate) orchestrate_now_override: Option<chrono::DateTime<chrono::Utc>>,
    #[allow(clippy::box_collection)]
    pub(crate) orchestrate_idle_generation_by_target: Box<HashMap<String, u64>>,
    side_threads: HashMap<ThreadId, SideThreadState>,
    pub(crate) claude_panes: crate::claude_panes::ClaudePaneRegistry,
    abandoned_side_threads: HashSet<ThreadId>,
    pub(crate) active_thread_id: Option<ThreadId>,
    active_thread_rx: Option<mpsc::Receiver<ThreadBufferedEvent>>,
    pub(crate) primary_thread_id: Option<ThreadId>,
    last_subagent_backfill_attempt: Option<ThreadId>,
    primary_session_configured: Option<ThreadSessionState>,
    pending_primary_events: VecDeque<ThreadBufferedEvent>,
    pending_app_server_requests: PendingAppServerRequests,
    pending_startup_thread_start: bool,
    /// Invalidates in-flight full rate-limit reads when a newer rolling hard stop arrives.
    rate_limit_hard_stop_generation: u64,
    // Serialize plugin enablement writes per plugin so stale completions cannot
    // overwrite a newer toggle, even if the plugin is toggled from different
    // cwd contexts.
    pending_plugin_enabled_writes: HashMap<String, Option<bool>>,
    // Serialize hook enablement writes per hook so stale completions cannot
    // persist an older toggle after a newer one.
    pending_hook_enabled_writes: HashMap<String, Option<bool>>,
}

#[derive(Debug, Clone, PartialEq)]
struct RuntimePermissionProfileOverride {
    permission_profile: PermissionProfile,
    active_permission_profile: Option<ActivePermissionProfile>,
    network: Option<crate::legacy_core::config::NetworkProxySpec>,
}

impl RuntimePermissionProfileOverride {
    fn from_config(config: &Config) -> Self {
        Self {
            permission_profile: config.permissions.permission_profile().clone(),
            active_permission_profile: config.permissions.active_permission_profile(),
            network: config.permissions.network.clone(),
        }
    }
}

fn active_turn_not_steerable_turn_error(error: &TypedRequestError) -> Option<AppServerTurnError> {
    let TypedRequestError::Server { source, .. } = error else {
        return None;
    };
    let turn_error: AppServerTurnError = serde_json::from_value(source.data.clone()?).ok()?;
    matches!(
        turn_error.codex_error_info,
        Some(AppServerCodexErrorInfo::ActiveTurnNotSteerable { .. })
    )
    .then_some(turn_error)
}

async fn resolve_runtime_model_provider_base_url(provider: &ModelProviderInfo) -> Option<String> {
    let provider = create_model_provider(provider.clone(), /*auth_manager*/ None);
    match provider.runtime_base_url().await {
        Ok(base_url) => base_url,
        Err(err) => {
            tracing::warn!(%err, "failed to resolve runtime model provider base URL for status");
            None
        }
    }
}

fn gpu_runtime_model_preset(provider: &codex_state::GpuRuntimeProvider) -> Option<ModelPreset> {
    if provider.health != "ready" || !provider.provider_id.starts_with("gpu-") {
        return None;
    }
    Some(ModelPreset {
        id: format!("{}:{}", provider.provider_id, provider.model_id),
        model: provider.model_id.clone(),
        provider_id: Some(provider.provider_id.clone()),
        orchestration: Some(ModelOrchestrationMetadata::Eligible {
            provider_id: provider.provider_id.clone(),
            capability: ModelCapabilityTier::Balanced,
            billing: ModelBilling::Local,
        }),
        multi_agent_version: None,
        display_name: provider.model_id.clone(),
        description: format!(
            "{} · active GPU rental {} · ${:.4}/hour",
            gpu_infrastructure_provider_label(provider.infrastructure_provider.as_str()),
            provider.rental_id,
            provider.display_hourly_microusd as f64 / 1_000_000.0
        ),
        default_reasoning_effort: ReasoningEffortConfig::None,
        supported_reasoning_efforts: Vec::new(),
        supports_personality: false,
        additional_speed_tiers: Vec::new(),
        service_tiers: Vec::new(),
        default_service_tier: None,
        is_default: false,
        upgrade: None,
        show_in_picker: true,
        availability_nux: None,
        supported_in_api: true,
        input_modalities: vec![InputModality::Text],
    })
}

fn gpu_infrastructure_provider_label(provider: &str) -> &str {
    match provider {
        "runpod" => "RunPod",
        "vast" => "Vast.ai",
        _ => "GPU marketplace",
    }
}

fn spawn_startup_thread_start(
    app_server: &AppServerSession,
    config: Config,
    app_event_tx: AppEventSender,
) {
    let request_handle = app_server.request_handle();
    let thread_params_mode = app_server.thread_params_mode();
    let remote_cwd_override = app_server.remote_cwd_override().map(Path::to_path_buf);
    tokio::spawn(async move {
        let result = crate::app_server_session::start_thread_with_request_handle(
            request_handle,
            config,
            thread_params_mode,
            remote_cwd_override,
        )
        .await
        .map_err(|err| format!("{err:#}"));
        app_event_tx.send(AppEvent::StartupThreadStarted { result });
    });
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum ActiveTurnSteerRace {
    Missing,
    ExpectedTurnMismatch { actual_turn_id: String },
}

fn active_turn_steer_race(error: &TypedRequestError) -> Option<ActiveTurnSteerRace> {
    let TypedRequestError::Server { method, source } = error else {
        return None;
    };
    if method != "turn/steer" {
        return None;
    }
    if source.message == "no active turn to steer" {
        return Some(ActiveTurnSteerRace::Missing);
    }

    // App-server steer mismatches mean our cached active turn id is stale, but the response
    // includes the server's current active turn so we can resynchronize and retry once.
    let mismatch_prefix = "expected active turn id `";
    let mismatch_separator = "` but found `";
    let actual_turn_id = source
        .message
        .strip_prefix(mismatch_prefix)?
        .split_once(mismatch_separator)?
        .1
        .strip_suffix('`')?
        .to_string();
    Some(ActiveTurnSteerRace::ExpectedTurnMismatch { actual_turn_id })
}

/// [`session_start_failure`]; for a refusal the screen is cleared and the app
/// server shut down, as on the other early exits.
async fn end_session_start(
    tui: &mut tui::Tui,
    app_server: AppServerSession,
    action: &str,
    target_session: &SessionTarget,
    err: color_eyre::eyre::Report,
) -> Result<AppExitInfo> {
    let result = session_start_failure(action, target_session, err);
    if result.is_ok() {
        // As for a missing session: the message prints on a clean screen.
        let _ = tui.terminal.clear();
        app_server
            .shutdown()
            .await
            .inspect_err(|err| tracing::warn!("app-server shutdown failed: {err}"))
            .ok();
    }
    result
}

/// A resume/fork the app server refused (archived thread, account rules, ...)
/// ends the launch with a plain `ERROR:` line like other launch refusals (#656).
/// Anything else (transport failure, bad response) stays an error report.
fn session_start_failure(
    action: &str,
    target_session: &SessionTarget,
    err: color_eyre::eyre::Report,
) -> Result<AppExitInfo> {
    if let Some(message) = archived_session_guidance(&err) {
        return Ok(AppExitInfo::fatal(message));
    }
    if let Some(message) = session_start_refusal(action, &err) {
        let thread_id = target_session.thread_id;
        return Ok(AppExitInfo::fatal(format!(
            "Could not {action} session {thread_id}: {message}"
        )));
    }

    let target_label = target_session.display_label();
    Err(color_eyre::eyre::eyre!(
        "Failed to {action} session from {target_label}: {err}"
    ))
}

/// The server's own message when `thread/{action}` was answered with a
/// JSON-RPC error, without the bootstrap context and error code.
fn session_start_refusal(action: &str, err: &color_eyre::eyre::Report) -> Option<String> {
    let err = err.to_string();
    let server_prefix = format!("thread/{action} failed: ");
    let message = &err[err.find(&server_prefix)? + server_prefix.len()..];
    // `{message} (code {code})`, then `, data: {data}` when the error has data;
    // either part may itself contain ` (code `.
    let (end, _) = message.match_indices(" (code ").find(|(start, marker)| {
        message[start + marker.len()..]
            .split_once(')')
            .is_some_and(|(code, rest)| {
                code.parse::<i64>().is_ok() && (rest.is_empty() || rest.starts_with(", data: "))
            })
    })?;
    let message = message[..end].trim();
    (!message.is_empty()).then(|| message.to_string())
}

fn archived_session_guidance(err: &color_eyre::eyre::Report) -> Option<String> {
    let err = err.to_string();
    let message = &err[err.find("session ")?..];
    if !message.contains(" is archived. Run `") || !message.contains(" unarchive ") {
        return None;
    }
    let message = message
        .split_once(" (code ")
        .map_or(message, |(message, _)| message);
    Some(
        message
            .replace("Run `codex unarchive ", "Run `corbanu unarchive ")
            .replace("Run `pfterminal unarchive ", "Run `corbanu unarchive "),
    )
}

fn active_turn_interrupt_race(error: &TypedRequestError) -> Option<String> {
    let TypedRequestError::Server { method, source } = error else {
        return None;
    };
    if method != "turn/interrupt" {
        return None;
    }
    let mismatch_prefix = "expected active turn id ";
    let mismatch_separator = " but found ";
    Some(
        source
            .message
            .strip_prefix(mismatch_prefix)?
            .split_once(mismatch_separator)?
            .1
            .to_string(),
    )
}

impl App {
    fn exit_resumable_thread(&self) -> Option<ResumableThread> {
        let active_rollout_path = self.chat_widget.rollout_path();
        if let Some(thread) = resumable_thread(
            self.chat_widget.thread_id(),
            self.chat_widget.thread_name(),
            active_rollout_path.as_deref(),
        ) {
            return Some(thread);
        }

        let primary_session = self.primary_session_configured.as_ref()?;
        if self.primary_thread_id != Some(primary_session.thread_id) {
            return None;
        }
        resumable_thread(
            Some(primary_session.thread_id),
            primary_session.thread_name.clone(),
            primary_session.rollout_path.as_deref(),
        )
    }

    pub fn chatwidget_init_for_forked_or_resumed_thread(
        &self,
        tui: &mut tui::Tui,
        cfg: crate::legacy_core::config::Config,
        initial_user_message: Option<crate::chatwidget::UserMessage>,
    ) -> crate::chatwidget::ChatWidgetInit {
        crate::chatwidget::ChatWidgetInit {
            config: cfg,
            frame_requester: tui.frame_requester(),
            app_event_tx: self.app_event_tx.clone(),
            workspace_command_runner: self.workspace_command_runner.clone(),
            initial_user_message,
            enhanced_keys_supported: self.enhanced_keys_supported,
            has_chatgpt_account: self.chat_widget.has_chatgpt_account(),
            has_codex_backend_auth: self.chat_widget.has_codex_backend_auth(),
            model_catalog: self.model_catalog.clone(),
            feedback: self.feedback.clone(),
            is_first_run: false,
            status_account_display: self.chat_widget.status_account_display().cloned(),
            runtime_model_provider_base_url: self
                .chat_widget
                .runtime_model_provider_base_url()
                .map(str::to_string),
            initial_plan_type: self.chat_widget.current_plan_type(),
            model: Some(self.chat_widget.current_model().to_string()),
            startup_tooltip_override: None,
            status_line_invalid_items_warned: self.status_line_invalid_items_warned.clone(),
            terminal_title_invalid_items_warned: self.terminal_title_invalid_items_warned.clone(),
            session_telemetry: self.session_telemetry.clone(),
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub async fn run(
        tui: &mut tui::Tui,
        mut app_server: AppServerSession,
        mut config: Config,
        launch_cwd: PathBuf,
        cli_kv_overrides: Vec<(String, TomlValue)>,
        harness_overrides: ConfigOverrides,
        loader_overrides: LoaderOverrides,
        cloud_config_bundle: CloudConfigBundleLoader,
        initial_prompt: Option<String>,
        initial_images: Vec<PathBuf>,
        session_selection: SessionSelection,
        startup_resume_model_override: Option<ResumeModelOverride>,
        feedback: codex_feedback::CodexFeedback,
        is_first_run: bool,
        should_prompt_windows_sandbox_nux_at_startup: bool,
        app_server_target: AppServerTarget,
        state_db: Option<StateDbHandle>,
        environment_manager: Arc<EnvironmentManager>,
        startup_elapsed_before_app: Duration,
        startup_bootstrap: Option<AppServerBootstrap>,
        startup_hooks_browser: Option<HooksListEntry>,
        deferred_provider_setup: Option<crate::onboarding::provider_setup::DeferredProviderSetup>,
        startup_provider_resolution: Option<crate::startup_provider::StartupProviderResolution>,
    ) -> Result<AppExitInfo> {
        let startup_started_at = Instant::now();
        let (app_event_tx, mut app_event_rx) = unbounded_channel();
        let app_event_tx = AppEventSender::new(app_event_tx);
        crate::chatwidget::agent_control::start(&app_event_tx);
        crate::chatwidget::campaign_tracker::start(&app_event_tx);
        emit_project_config_warnings(&app_event_tx, &config);
        emit_system_bwrap_warning(&app_event_tx, &config);
        emit_legacy_windows_sandbox_warning(&app_event_tx, &config);
        tui.set_notification_settings(
            config.tui_notifications.method,
            config.tui_notifications.condition,
        );

        let _startup_resume_model_override = match (
            startup_resume_model_override,
            &session_selection,
            state_db.as_ref(),
        ) {
            (Some(explicit), _, _) => Some(explicit),
            (None, SessionSelection::Resume(target), Some(state_db)) => {
                match state_db.get_thread(target.thread_id).await {
                    Ok(Some(metadata)) => apply_persisted_resume_runtime(
                        &mut config,
                        metadata.model.as_deref(),
                        metadata.model_provider.as_str(),
                        metadata.reasoning_effort.clone(),
                    ),
                    Ok(None) => None,
                    Err(error) => {
                        tracing::warn!(
                            thread_id = %target.thread_id,
                            %error,
                            "failed to load persisted runtime for startup resume"
                        );
                        None
                    }
                }
            }
            (None, _, _) => None,
        };

        let harness_overrides =
            normalize_harness_overrides_for_cwd(harness_overrides, &config.cwd)?;
        let bootstrap = match startup_bootstrap {
            Some(bootstrap) => bootstrap,
            None => app_server.bootstrap(&config).await?,
        };
        let bootstrap_ms = bootstrap.duration.as_millis();
        if matches!(
            &session_selection,
            SessionSelection::StartFresh | SessionSelection::Exit
        ) {
            apply_managed_new_thread_defaults(
                &mut config,
                app_server.managed_new_thread_defaults(),
                &cli_kv_overrides,
                &harness_overrides,
            );
        }
        let mut model = config.model.clone().unwrap_or(bootstrap.default_model);
        let mut available_models = bootstrap.available_models;
        let listed_models = available_models.clone();
        if let Some(state_db) = state_db.as_ref() {
            match state_db.list_gpu_runtime_providers().await {
                Ok(providers) => {
                    available_models.extend(providers.iter().filter_map(gpu_runtime_model_preset))
                }
                Err(error) => tracing::warn!(%error, "failed to load GPU rental model catalog"),
            }
        }
        let remote_connection = crate::status::remote_connection::remote_connection_status_value(
            &app_server_target,
            app_server.server_version(),
        );
        let exit_info = handle_model_migration_prompt_if_needed(
            tui,
            &mut config,
            model.as_str(),
            &app_event_tx,
            &available_models,
        )
        .await;
        if let Some(exit_info) = exit_info {
            app_server
                .shutdown()
                .await
                .inspect_err(|err| {
                    tracing::warn!("app-server shutdown failed: {err}");
                })
                .ok();
            return Ok(exit_info);
        }
        if let Some(updated_model) = config.model.clone() {
            model = updated_model;
        }
        let provider_runtime =
            crate::startup_provider::resolve_for_app(&config, startup_provider_resolution).await;
        crate::model_catalog::include_runtime_model(
            &mut available_models,
            &model,
            &config.model_provider_id,
        );
        let session_recovery_only = matches!(
            provider_runtime.current,
            codex_provider_auth::CurrentSelectionDecision::RequireExplicitRecovery { .. }
        ) && matches!(&session_selection, SessionSelection::Resume(_));
        let shared_provider_status_host = Some(provider_runtime.policy.host());
        let model_catalog = Arc::new(ModelCatalog::new(available_models.clone()));
        model_catalog.mark_listed(&listed_models);
        model_catalog.set_provider_policy(provider_runtime.policy);
        // #572: Bedrock models for a session that may switch to a named Bedrock account.
        provider_management_status::check_named_bedrock_account(
            &model_catalog,
            &config,
            &app_event_tx,
        );
        // D86/#1193: which providers have named accounts, for `/status`.
        crate::provider_named_accounts::check_named_account_providers(&model_catalog, &config);
        model_catalog.set_session_recovery_only(session_recovery_only);
        let feedback_audience = bootstrap.feedback_audience;
        let auth_mode = bootstrap.auth_mode;
        let has_chatgpt_account = bootstrap.has_chatgpt_account;
        let has_codex_backend_auth = bootstrap.has_codex_backend_auth;
        let requires_openai_auth = bootstrap.requires_openai_auth;
        let status_account_display = bootstrap.status_account_display.clone();
        let initial_plan_type = bootstrap.plan_type;
        let session_telemetry = SessionTelemetry::new(
            ThreadId::new(),
            model.as_str(),
            model.as_str(),
            /*account_id*/ None,
            bootstrap.account_email.clone(),
            auth_mode,
            codex_login::default_client::originator().value,
            config.otel.log_user_prompt,
            user_agent(),
            serde_json::from_value(serde_json::json!("cli"))
                .unwrap_or_else(|err| panic!("cli session source should deserialize: {err}")),
        );
        if config
            .tui_status_line
            .as_ref()
            .is_some_and(|cmd| !cmd.is_empty())
        {
            session_telemetry.counter("codex.status_line", /*inc*/ 1, &[]);
        }

        let status_line_invalid_items_warned = Arc::new(AtomicBool::new(false));
        let terminal_title_invalid_items_warned = Arc::new(AtomicBool::new(false));
        let workspace_command_runner: WorkspaceCommandRunner = Arc::new(
            AppServerWorkspaceCommandRunner::new(app_server.request_handle()),
        );
        let runtime_model_provider_started_at = Instant::now();
        let runtime_model_provider_base_url =
            resolve_runtime_model_provider_base_url(&config.model_provider).await;
        let runtime_model_provider_ms = runtime_model_provider_started_at.elapsed().as_millis();

        let enhanced_keys_supported = tui.enhanced_keys_supported();
        let wait_for_initial_session_configured =
            Self::should_wait_for_initial_session(&session_selection);
        let should_prompt_for_paused_goal_after_startup_resume =
            Self::should_prompt_for_paused_goal_after_startup_resume(
                &session_selection,
                &initial_prompt,
                &initial_images,
            );
        let thread_and_widget_started_at = Instant::now();
        let pending_startup_thread_start = matches!(
            &session_selection,
            SessionSelection::StartFresh
                | SessionSelection::ResumePanesOnly { .. }
                | SessionSelection::Exit
        );
        let restore_pane_layout_for_thread_id = match &session_selection {
            SessionSelection::Resume(target_session) => Some(target_session.thread_id.to_string()),
            SessionSelection::ResumePanesOnly { codex_thread_id } => Some(codex_thread_id.clone()),
            SessionSelection::Fork(_) | SessionSelection::StartFresh | SessionSelection::Exit => {
                None
            }
        };
        let (mut chat_widget, initial_started_thread) = match session_selection {
            SessionSelection::StartFresh
            | SessionSelection::ResumePanesOnly { .. }
            | SessionSelection::Exit => {
                spawn_startup_thread_start(&app_server, config.clone(), app_event_tx.clone());
                // Count a startup tooltip once the initial chat widget can render it.
                let startup_tooltip_override =
                    prepare_startup_tooltip_override(&mut config, &available_models, is_first_run)
                        .await;
                let init = crate::chatwidget::ChatWidgetInit {
                    config: config.clone(),
                    frame_requester: tui.frame_requester(),
                    app_event_tx: app_event_tx.clone(),
                    workspace_command_runner: Some(workspace_command_runner.clone()),
                    initial_user_message: crate::chatwidget::create_initial_user_message(
                        initial_prompt.clone(),
                        initial_images.clone(),
                        // CLI prompt args are plain strings, so they don't provide element ranges.
                        Vec::new(),
                    ),
                    enhanced_keys_supported,
                    has_chatgpt_account,
                    has_codex_backend_auth,
                    model_catalog: model_catalog.clone(),
                    feedback: feedback.clone(),
                    is_first_run,
                    status_account_display: status_account_display.clone(),
                    runtime_model_provider_base_url: runtime_model_provider_base_url.clone(),
                    initial_plan_type,
                    model: Some(model.clone()),
                    startup_tooltip_override,
                    status_line_invalid_items_warned: status_line_invalid_items_warned.clone(),
                    terminal_title_invalid_items_warned: terminal_title_invalid_items_warned
                        .clone(),
                    session_telemetry: session_telemetry.clone(),
                };
                let mut chat_widget = ChatWidget::new_with_app_event(init);
                chat_widget.set_queue_submissions_until_session_configured(/*queue*/ true);
                (chat_widget, None)
            }
            SessionSelection::Resume(target_session) => {
                let model_settings = config_persistence::resume_model_settings_for_overrides(
                    &config,
                    &harness_overrides,
                );
                let permission_settings =
                    config_persistence::resume_permission_settings_for_overrides(
                        &config,
                        &harness_overrides,
                    );
                let resumed = match app_server
                    .resume_thread(
                        config.clone(),
                        target_session.thread_id,
                        model_settings,
                        permission_settings,
                    )
                    .await
                {
                    Ok(resumed) => resumed,
                    Err(err) => {
                        return end_session_start(tui, app_server, "resume", &target_session, err)
                            .await;
                    }
                };
                apply_persisted_resume_runtime(
                    &mut config,
                    Some(&resumed.session.model),
                    &resumed.session.model_provider_id,
                    resumed.session.reasoning_effort.clone(),
                );
                config.service_tier = resumed.session.service_tier.clone();
                if !resumed.blocks_direct_input
                    && let Err(error) = resume_defaults::remember_resumed_model(
                        &app_server,
                        &config,
                        model_settings,
                        &resumed.session,
                    )
                    .await
                {
                    app_event_tx.send(AppEvent::InsertHistoryCell(Box::new(
                        history_cell::new_error_event(format!(
                            "Session restored, but failed to save its startup model: {error}"
                        )),
                    )));
                }
                let init = crate::chatwidget::ChatWidgetInit {
                    config: config.clone(),
                    frame_requester: tui.frame_requester(),
                    app_event_tx: app_event_tx.clone(),
                    workspace_command_runner: Some(workspace_command_runner.clone()),
                    initial_user_message: crate::chatwidget::create_initial_user_message(
                        initial_prompt.clone(),
                        initial_images.clone(),
                        // CLI prompt args are plain strings, so they don't provide element ranges.
                        Vec::new(),
                    ),
                    enhanced_keys_supported,
                    has_chatgpt_account,
                    has_codex_backend_auth,
                    model_catalog: model_catalog.clone(),
                    feedback: feedback.clone(),
                    is_first_run,
                    status_account_display: status_account_display.clone(),
                    runtime_model_provider_base_url: runtime_model_provider_base_url.clone(),
                    initial_plan_type,
                    model: config.model.clone(),
                    startup_tooltip_override: None,
                    status_line_invalid_items_warned: status_line_invalid_items_warned.clone(),
                    terminal_title_invalid_items_warned: terminal_title_invalid_items_warned
                        .clone(),
                    session_telemetry: session_telemetry.clone(),
                };
                (ChatWidget::new_with_app_event(init), Some(resumed))
            }
            SessionSelection::Fork(target_session) => {
                session_telemetry.counter(
                    "codex.thread.fork",
                    /*inc*/ 1,
                    &[("source", "cli_subcommand")],
                );
                let forked = match app_server
                    .fork_thread(config.clone(), target_session.thread_id)
                    .await
                {
                    Ok(forked) => forked,
                    Err(err) => {
                        return end_session_start(tui, app_server, "fork", &target_session, err)
                            .await;
                    }
                };
                let init = crate::chatwidget::ChatWidgetInit {
                    config: config.clone(),
                    frame_requester: tui.frame_requester(),
                    app_event_tx: app_event_tx.clone(),
                    workspace_command_runner: Some(workspace_command_runner.clone()),
                    initial_user_message: crate::chatwidget::create_initial_user_message(
                        initial_prompt.clone(),
                        initial_images.clone(),
                        // CLI prompt args are plain strings, so they don't provide element ranges.
                        Vec::new(),
                    ),
                    enhanced_keys_supported,
                    has_chatgpt_account,
                    has_codex_backend_auth,
                    model_catalog: model_catalog.clone(),
                    feedback: feedback.clone(),
                    is_first_run,
                    status_account_display: status_account_display.clone(),
                    runtime_model_provider_base_url: runtime_model_provider_base_url.clone(),
                    initial_plan_type,
                    model: config.model.clone(),
                    startup_tooltip_override: None,
                    status_line_invalid_items_warned: status_line_invalid_items_warned.clone(),
                    terminal_title_invalid_items_warned: terminal_title_invalid_items_warned
                        .clone(),
                    session_telemetry: session_telemetry.clone(),
                };
                (ChatWidget::new_with_app_event(init), Some(forked))
            }
        };
        chat_widget.remote_connection = remote_connection;
        let thread_and_widget_ms = thread_and_widget_started_at.elapsed().as_millis();
        chat_widget
            .maybe_prompt_windows_sandbox_enable(should_prompt_windows_sandbox_nux_at_startup);
        chat_widget.maybe_prompt_planted_project_metadata();

        let file_search = FileSearchManager::new(config.cwd.to_path_buf(), app_event_tx.clone());
        let runtime_keymap = RuntimeKeymap::from_config(&config.tui_keymap).map_err(|err| {
            color_eyre::eyre::eyre!(
                "Invalid `tui.keymap` configuration: {err}\n\
Fix the config and retry.\n\
See the Corbanu Terminal keymap documentation for supported actions and examples."
            )
        })?;
        #[cfg(not(debug_assertions))]
        let upgrade_version = crate::updates::get_upgrade_version(&config);

        let restored_pane_layout =
            restore_pane_layout_for_thread_id
                .as_deref()
                .and_then(|thread_id| {
                    crate::claude_panes::load_pane_layout(
                        config.codex_home.as_ref(),
                        Some(thread_id),
                    )
                });
        let restored_spawn_parent_by_node = restored_pane_layout
            .as_ref()
            .map(|layout| {
                layout
                    .spawn_parent_by_node
                    .iter()
                    .map(|(key, value)| (key.clone(), value.clone()))
                    .collect()
            })
            .unwrap_or_default();
        let restored_claude_panes = crate::claude_panes::ClaudePaneRegistry::restore_from_disk(
            config.codex_home.as_ref(),
            restored_pane_layout.as_ref(),
        );
        let restored_codex_user_pane_ids = restored_pane_layout
            .as_ref()
            .map(|layout| {
                layout
                    .codex_user_pane_ids
                    .iter()
                    .filter_map(|thread_id| ThreadId::from_string(thread_id).ok())
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        let restored_spawn_nazgul_pane_id = restored_pane_layout
            .as_ref()
            .and_then(|layout| layout.spawn_nazgul_pane_id.clone())
            .filter(|pane_id| {
                // `codex-main`, any restored Claude pane, or a native Codex agent thread node id
                // (`thread:<uuid>`) are all valid Nazgul root bindings.
                if pane_id == crate::claude_panes::CODEX_MAIN_PANE_ID {
                    return true;
                }
                if crate::spawn_orchestration::node_id_thread(pane_id).is_some() {
                    return true;
                }
                restored_claude_panes
                    .panes()
                    .iter()
                    .any(|pane| pane.id == *pane_id)
            });
        let restored_spawn_nazgul_rebind_required = restored_pane_layout
            .as_ref()
            .is_some_and(|layout| layout.spawn_nazgul_rebind_required);
        let restored_spawn_legacy_read_only = restored_pane_layout
            .as_ref()
            .is_some_and(crate::spawn_orchestration::pane_layout_has_legacy_spawn_hierarchy);
        let mut restored_orchestrate_whips: HashMap<_, _> = restored_pane_layout
            .as_ref()
            .map(|layout| {
                layout
                    .orchestrate_whips
                    .iter()
                    .filter(|(_, whip)| whip.state != crate::orchestrate::WhipState::Detached)
                    .map(|(key, value)| (key.clone(), value.clone()))
                    .collect()
            })
            .unwrap_or_default();
        for whip in restored_orchestrate_whips.values_mut() {
            if matches!(
                whip.kind,
                crate::orchestrate::WhipKind::Assignment {
                    phase: crate::orchestrate::AssignmentPhase::Executing,
                    ..
                }
            ) {
                whip.last_fire_utc = Some(chrono::Utc::now());
                // Generation zero represents the state restored from disk. Treat any saved
                // Worker output as already observed so restart honors one full cadence; a fresh
                // completion increments the generation and can still trigger immediately.
                whip.last_idle_generation_fired = Some(0);
            } else {
                whip.last_idle_generation_fired = Some(0);
            }
        }
        let restored_orchestrate_next_whip_seq = restored_pane_layout
            .as_ref()
            .map(|layout| layout.orchestrate_next_whip_seq)
            .unwrap_or_else(|| {
                restored_orchestrate_whips
                    .keys()
                    .filter_map(|id| {
                        id.strip_prefix("whip-")
                            .or_else(|| id.strip_prefix("assignment-"))?
                            .parse::<u64>()
                            .ok()
                    })
                    .max()
                    .unwrap_or(0)
            });
        let restored_orchestrate_idle_generation_by_target = restored_orchestrate_whips
            .values()
            .map(|whip| (whip.target.clone(), 0))
            .collect();
        let restored_spawn_next_dispatch_seq = restored_pane_layout
            .as_ref()
            .map(|layout| layout.spawn_next_dispatch_seq.max(1))
            .unwrap_or(1);
        let restored_spawn_processed_dispatch_seq_ids = restored_pane_layout
            .as_ref()
            .map(|layout| {
                crate::spawn_orchestration::bounded_spawn_processed_dispatch_seq_ids(
                    layout.spawn_processed_dispatch_seq_ids.iter().copied(),
                    restored_spawn_next_dispatch_seq,
                )
            })
            .unwrap_or_default();
        let restored_spawn_processed_dispatch_origins = restored_pane_layout
            .as_ref()
            .map(|layout| {
                layout
                    .spawn_processed_dispatch_origin_ids
                    .iter()
                    .cloned()
                    .collect()
            })
            .unwrap_or_default();

        let mut app = Self {
            model_catalog,
            session_telemetry: session_telemetry.clone(),
            app_event_tx,
            chat_widget,
            workspace_command_runner: Some(workspace_command_runner),
            config,
            active_deferred_provider_setup: deferred_provider_setup.clone(),
            shared_provider_setup_session: None,
            shared_provider_status_host,
            shared_provider_account_auth_host: None,
            provider_management_host: None,
            provider_management_generation: 0,
            pending_wallet_create_deferred: None,
            launch_cwd,
            state_db,
            cli_kv_overrides,
            harness_overrides,
            loader_overrides,
            cloud_config_bundle,
            runtime_approval_policy_override: None,
            runtime_permission_profile_override: None,
            pending_permission_confirmation: None,
            file_search,
            enhanced_keys_supported,
            keymap: runtime_keymap,
            transcript_cells: Vec::new(),
            claude_pane_transcript_cells: HashMap::new(),
            overlay: None,
            deferred_history_lines: Vec::new(),
            has_emitted_history_lines: false,
            transcript_reflow: TranscriptReflowState::default(),
            initial_history_replay_buffer: None,
            commit_anim_running: Arc::new(AtomicBool::new(false)),
            status_line_invalid_items_warned: status_line_invalid_items_warned.clone(),
            terminal_title_invalid_items_warned: terminal_title_invalid_items_warned.clone(),
            skill_load_warnings: SkillLoadWarningState::default(),
            skills_refresh_generation: Arc::new(AtomicU64::new(0)),
            backtrack: BacktrackState::default(),
            backtrack_render_pending: false,
            feedback: feedback.clone(),
            feedback_audience,
            environment_manager,
            app_server_target,
            pending_update_action: None,
            pending_shutdown_exit_thread_id: None,
            windows_sandbox: WindowsSandboxState::default(),
            thread_event_channels: HashMap::new(),
            thread_event_listener_tasks: HashMap::new(),
            agent_navigation: AgentNavigationState::default(),
            spawn_parent_by_thread: HashMap::new(),
            spawn_parent_by_node: restored_spawn_parent_by_node,
            spawn_native_runtime_by_node: restored_pane_layout
                .as_ref()
                .map(|layout| {
                    layout
                        .spawn_native_runtime_by_node
                        .clone()
                        .into_iter()
                        .collect()
                })
                .unwrap_or_default(),
            spawn_native_endpoint_by_node: restored_pane_layout
                .as_ref()
                .map(|layout| {
                    layout
                        .spawn_native_endpoint_by_node
                        .iter()
                        .filter_map(|(node, endpoint)| {
                            ThreadId::from_string(endpoint)
                                .ok()
                                .map(|endpoint| (node.clone(), endpoint))
                        })
                        .collect()
                })
                .unwrap_or_default(),
            spawn_crew: restored_pane_layout
                .as_ref()
                .and_then(|layout| layout.spawn_crew.clone()),
            spawn_legacy_read_only: restored_spawn_legacy_read_only,
            spawn_status_by_thread: HashMap::new(),
            spawn_waiting_for_agents_by_thread: HashMap::new(),
            spawn_parent_reports_by_node: HashMap::new(),
            spawn_dispatch_acks_by_target_task: HashMap::new(),
            spawn_next_dispatch_seq: restored_spawn_next_dispatch_seq,
            spawn_processed_dispatch_seq_ids: restored_spawn_processed_dispatch_seq_ids,
            spawn_processed_dispatch_origins: restored_spawn_processed_dispatch_origins,
            spawn_processed_terminal_turns: HashSet::new(),
            spawn_auto_loop_state_by_node: HashMap::new(),
            spawn_operator_input_seen: false,
            kill_switch_held_claude_tasks: Vec::new(),
            kill_switch_held_native_tasks: Vec::new(),
            // Set from the home's watch at start (`watch_kill_switch_elsewhere`).
            kill_switch_known_on: false,
            spawn_quarantine_notified_by_node: HashSet::new(),
            spawn_context_left_by_thread: HashMap::new(),
            spawn_last_report_seq_by_node: HashMap::new(),
            spawn_last_dispatch_seq_by_node: HashMap::new(),
            spawn_last_event_at_by_node: HashMap::new(),
            spawn_nazgul_pane_id: restored_spawn_nazgul_pane_id,
            spawn_nazgul_rebind_required: restored_spawn_nazgul_rebind_required,
            orchestrate_whips: Box::new(restored_orchestrate_whips),
            orchestrate_next_whip_seq: restored_orchestrate_next_whip_seq,
            #[cfg(test)]
            orchestrate_now_override: None,
            orchestrate_idle_generation_by_target: Box::new(
                restored_orchestrate_idle_generation_by_target,
            ),
            side_threads: HashMap::new(),
            claude_panes: restored_claude_panes,
            abandoned_side_threads: HashSet::new(),
            active_thread_id: None,
            active_thread_rx: None,
            primary_thread_id: None,
            last_subagent_backfill_attempt: None,
            primary_session_configured: None,
            pending_primary_events: VecDeque::new(),
            pending_app_server_requests: PendingAppServerRequests::default(),
            pending_startup_thread_start,
            rate_limit_hard_stop_generation: 0,
            pending_plugin_enabled_writes: HashMap::new(),
            pending_hook_enabled_writes: HashMap::new(),
        };
        if let Some(deferred) = deferred_provider_setup {
            app.app_event_tx
                .send(AppEvent::BeginDeferredCorbanuPlan { deferred });
        }
        for thread_id in restored_codex_user_pane_ids {
            app.upsert_agent_picker_thread(
                thread_id, /*agent_nickname*/ None, /*agent_role*/ None,
                /*is_closed*/ false,
            );
        }
        app.refresh_gpu_spend_indicator().await;
        // A rental outlives the TUI that created it. Reattach the independent reconciler on
        // every startup; its process lock deduplicates concurrent Corbanu Terminal processes and it
        // exits immediately when there is no potentially billable work.
        app.start_gpu_controller();
        app.watch_kill_switch_elsewhere();
        app.watch_level_raised_elsewhere();
        let telegram_home = app.config.codex_home.clone().to_path_buf();
        tokio::spawn(async move {
            let result = tokio::task::spawn_blocking(move || {
                crate::chatwidget::telegram_setup::ensure_connector(&telegram_home)
            })
            .await;
            match result {
                Ok(Ok(())) => {}
                Ok(Err(error)) => {
                    tracing::warn!(%error, "configured Telegram connector did not start")
                }
                Err(error) => {
                    tracing::warn!(%error, "Telegram connector startup task failed")
                }
            }
        });
        if let Some(entry) = startup_hooks_browser {
            app.chat_widget.open_hooks_browser(entry);
        }
        app.seed_restored_claude_pane_transcripts();
        app.show_restored_active_claude_pane();
        let initial_session_started_at = Instant::now();
        if let Some(started) = initial_started_thread {
            let thread_id = started.session.thread_id;
            if started.blocks_direct_input {
                app.mark_primary_thread_parent_owned(thread_id);
            }
            app.enqueue_primary_thread_session(started.session, started.turns)
                .await?;
            app.restore_codex_user_panes_from_saved_state(&mut app_server)
                .await;
            app.restore_native_spawn_panes_from_saved_state(&mut app_server)
                .await;
            app.audit_restored_assignments();
            if should_prompt_for_paused_goal_after_startup_resume {
                app.maybe_prompt_resume_paused_goal_after_resume(&mut app_server, thread_id)
                    .await;
            }
        }
        let initial_session_ms = initial_session_started_at.elapsed().as_millis();

        // On startup, if a managed filesystem sandbox is active, warn about
        // world-writable dirs on Windows.
        #[cfg(target_os = "windows")]
        {
            let startup_permission_profile = app.config.permissions.effective_permission_profile();
            let should_check = crate::windows_sandbox::level_from_config(&app.config)
                != WindowsSandboxLevel::Disabled
                && managed_filesystem_sandbox_is_restricted(&startup_permission_profile)
                && !app
                    .config
                    .notices
                    .hide_world_writable_warning
                    .unwrap_or(false);
            if should_check {
                let cwd = app.config.cwd.clone();
                let workspace_roots = app.config.effective_workspace_roots();
                let env_map: std::collections::HashMap<String, String> = std::env::vars().collect();
                let tx = app.app_event_tx.clone();
                let logs_base_dir = app.config.codex_home.clone();
                Self::spawn_world_writable_scan(
                    cwd,
                    workspace_roots,
                    env_map,
                    logs_base_dir,
                    startup_permission_profile,
                    tx,
                );
            }
        }

        let event_stream_started_at = Instant::now();
        let drained_tui_events = spawn_tui_event_drainer(tui.event_stream());
        let tui_input_watchdog =
            spawn_tui_input_watchdog(drained_tui_events.watchdog.clone(), tui.frame_requester());
        let _whip_sweep_tick = spawn_whip_sweep_tick(app.app_event_tx.clone());
        let mut tui_event_rx = drained_tui_events.rx;
        let tui_input_watchdog_state = drained_tui_events.watchdog;

        tui.frame_requester().schedule_frame();
        tracing::info!(
            duration_ms = %(startup_elapsed_before_app + startup_started_at.elapsed()).as_millis(),
            bootstrap_ms = %bootstrap_ms,
            runtime_model_provider_ms = %runtime_model_provider_ms,
            thread_and_widget_ms = %thread_and_widget_ms,
            initial_session_ms = %initial_session_ms,
            event_stream_ms = %event_stream_started_at.elapsed().as_millis(),
            "tui startup initial frame scheduled"
        );
        app.refresh_startup_skills(&app_server);
        // Kick off a non-blocking rate-limit prefetch so the first `/status`
        // already has data and available reset credits can be surfaced, without
        // delaying the initial frame render.
        if requires_openai_auth && has_chatgpt_account {
            let reset_hint_request_id = app.chat_widget.start_rate_limit_reset_startup_check();
            app.refresh_rate_limits(
                &app_server,
                RateLimitRefreshOrigin::StartupPrefetch {
                    reset_hint_request_id,
                },
            );
        }

        let mut listen_for_app_server_events = true;
        let mut app_server_reconnect_tick = tokio::time::interval(Duration::from_secs(5));
        app_server_reconnect_tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        let mut gpu_runtime_overlay_tick = tokio::time::interval(Duration::from_secs(5));
        gpu_runtime_overlay_tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        let (app_server_reconnect_tx, mut app_server_reconnect_rx) =
            mpsc::unbounded_channel::<Result<AppServerSession, String>>();
        let mut app_server_reconnect_inflight = false;
        let mut app_server_reconnect_failure_notified = false;
        let mut waiting_for_initial_session_configured = wait_for_initial_session_configured;

        #[cfg(not(debug_assertions))]
        let pre_loop_exit_reason = if let Some(latest_version) = upgrade_version {
            let control = Box::pin(app.handle_event(
                tui,
                &mut app_server,
                AppEvent::InsertHistoryCell(Box::new(UpdateAvailableHistoryCell::new(
                    latest_version,
                    crate::update_action::get_update_action(),
                ))),
            ))
            .await?;
            match control {
                AppRunControl::Continue => None,
                AppRunControl::Exit(exit_reason) => Some(exit_reason),
            }
        } else {
            None
        };
        #[cfg(debug_assertions)]
        let pre_loop_exit_reason: Option<ExitReason> = None;

        let exit_reason_result = if let Some(exit_reason) = pre_loop_exit_reason {
            Ok(exit_reason)
        } else {
            loop {
                let control = select! {
                    Some(event) = app_event_rx.recv() => {
                        if matches!(event, AppEvent::OpenExternalAgentConfigMigration) {
                            app.handle_external_agent_config_migration_event(
                                tui,
                                &mut app_server,
                                &mut tui_event_rx,
                            )
                            .await;
                            AppRunControl::Continue
                        } else {
                            let result = if matches!(event, AppEvent::OpenResumePicker) {
                                // Keep a single reader of the terminal. Lend its queue to
                                // the modal, including watchdog accounting, until it closes.
                                let mut events = modal_tui_events(&mut tui_event_rx, &tui_input_watchdog_state);
                                app.handle_resume_picker_event(tui, &mut app_server, &mut events).await
                            } else {
                                Box::pin(app.handle_event(tui, &mut app_server, event)).await
                            };
                            match result {
                                Ok(control) => control,
                                Err(err) => {
                                    tracing::error!(error = ?err, "contained app event handler failure");
                                    app.chat_widget.add_error_message(format!(
                                        "A command failed but Corbanu Terminal is still running: {err:#}"
                                    ));
                                    AppRunControl::Continue
                                },
                            }
                        }
                    }
                    active = async {
                        if let Some(rx) = app.active_thread_rx.as_mut() {
                            rx.recv().await
                        } else {
                            None
                        }
                    }, if App::should_handle_active_thread_events(
                        waiting_for_initial_session_configured,
                        app.active_thread_rx.is_some()
                    ) => {
                        if let Some(event) = active {
                            if let Err(err) = app.handle_active_thread_event(tui, &mut app_server, event).await {
                                tracing::error!(error = ?err, "contained active-thread event failure");
                                app.chat_widget.add_error_message(format!(
                                    "A pane event failed but Corbanu Terminal is still running: {err:#}"
                                ));
                            }
                        } else {
                            app.clear_active_thread().await;
                        }
                        AppRunControl::Continue
                    }
                    event = tui_event_rx.recv() => {
                        if let Some(event) = event {
                            tui_input_watchdog_state.note_handled();
                            match app.handle_tui_event(tui, &mut app_server, event).await {
                                Ok(control) => control,
                                Err(err) => {
                                    tracing::error!(error = ?err, "contained terminal input handler failure");
                                    app.chat_widget.add_error_message(format!(
                                        "An input command failed but Corbanu Terminal is still running: {err:#}"
                                    ));
                                    AppRunControl::Continue
                                },
                            }
                        } else {
                            tracing::warn!("terminal input stream closed; shutting down active thread");
                            app.handle_exit_mode(&mut app_server, ExitMode::ShutdownFirst).await
                        }
                    }
                    signal = tokio::signal::ctrl_c() => {
                        if let Err(err) = signal {
                            tracing::warn!(error = %err, "failed to listen for SIGINT");
                            AppRunControl::Continue
                        } else if app.active_thread_id.is_some() {
                            match Box::pin(app.handle_event(
                                tui,
                                &mut app_server,
                                AppEvent::CodexOp(
                                    AppCommand::interrupt_and_restore_prompt_if_no_output(),
                                ),
                            ))
                            .await
                            {
                                Ok(control) => control,
                                Err(err) => {
                                    tracing::error!(error = ?err, "contained interrupt handler failure");
                                    app.chat_widget.add_error_message(format!(
                                        "Interrupt failed but Corbanu Terminal is still running: {err:#}"
                                    ));
                                    AppRunControl::Continue
                                },
                            }
                        } else {
                            app.handle_exit_mode(&mut app_server, ExitMode::ShutdownFirst).await
                        }
                    }
                    app_server_event = app_server.next_event(), if listen_for_app_server_events => {
                        match app_server_event {
                            Some(event) => app.handle_app_server_event(&app_server, event).await,
                            None => {
                                listen_for_app_server_events = false;
                                tracing::warn!("app-server event stream closed");
                                app.chat_widget.add_error_message(
                                    "App-server event stream closed; Corbanu Terminal is degraded and will reconnect automatically."
                                        .to_string(),
                                );
                            }
                        }
                        AppRunControl::Continue
                    }
                    _ = app_server_reconnect_tick.tick(), if !listen_for_app_server_events && !app_server_reconnect_inflight => {
                        app_server_reconnect_inflight = true;
                        let config = app.config.clone();
                        let target = app.app_server_target.clone();
                        let state_db = app.state_db.clone();
                        let environment_manager = app.environment_manager.clone();
                        let reconnect_tx = app_server_reconnect_tx.clone();
                        tokio::spawn(async move {
                            let result = crate::start_app_server_for_picker(
                                &config,
                                &target,
                                state_db,
                                environment_manager,
                            )
                            .await
                            .map_err(|error| format!("{error:#}"));
                            let _ = reconnect_tx.send(result);
                        });
                        AppRunControl::Continue
                    }
                    Some(result) = app_server_reconnect_rx.recv(), if app_server_reconnect_inflight => {
                        app_server_reconnect_inflight = false;
                        match result {
                            Ok(mut replacement) => {
                                std::mem::swap(&mut app_server, &mut replacement);
                                tokio::spawn(async move {
                                    if let Err(error) = replacement.shutdown().await {
                                        tracing::warn!(%error, "old app-server shutdown after reconnect failed");
                                    }
                                });
                                app.restore_codex_user_panes_from_saved_state(&mut app_server).await;
                                app.restore_native_spawn_panes_from_saved_state(&mut app_server).await;
                                listen_for_app_server_events = true;
                                app_server_reconnect_failure_notified = false;
                                app.chat_widget.add_info_message(
                                    "App-server connection restored.".to_string(),
                                    Some("Pending identified deliveries are being reconciled.".to_string()),
                                );
                            }
                            Err(error) => {
                                tracing::warn!(%error, "bounded app-server reconnect attempt failed");
                                if !app_server_reconnect_failure_notified {
                                    app_server_reconnect_failure_notified = true;
                                    app.chat_widget.add_error_message(format!(
                                        "App-server reconnect failed; retrying every 5 seconds: {error:#}"
                                    ));
                                }
                            }
                        }
                        AppRunControl::Continue
                    }
                    _ = gpu_runtime_overlay_tick.tick() => {
                        app.refresh_gpu_runtime_overlay().await;
                        AppRunControl::Continue
                    }
                };
                if App::should_stop_waiting_for_initial_session(
                    waiting_for_initial_session_configured,
                    app.primary_thread_id,
                ) {
                    waiting_for_initial_session_configured = false;
                }
                match control {
                    AppRunControl::Continue => {}
                    AppRunControl::Exit(reason) => break Ok(reason),
                }
            }
        };
        let shutdown_detail = match &exit_reason_result {
            Ok(_) => "user or terminal exit",
            Err(_) => "TUI event loop failure",
        };
        if let Err(error) = app
            .show_backend_shutdown_control_events(tui, shutdown_detail)
            .await
        {
            tracing::error!(
                error = ?error,
                error_chain = %format!("{error:#}"),
                "failed to render backend shutdown control events"
            );
        }
        app.abort_all_thread_event_listeners();
        if let Err(err) = app_server.shutdown().await {
            tracing::error!(error = ?err, "failed to shut down embedded app server");
        }
        tui_input_watchdog.abort();
        let clear_pet_result = tui.clear_ambient_pet_image();
        let clear_result = tui.terminal.clear();
        let exit_reason = match exit_reason_result {
            Ok(exit_reason) => {
                clear_pet_result?;
                clear_result?;
                exit_reason
            }
            Err(err) => {
                if let Err(clear_pet_err) = clear_pet_result {
                    tracing::warn!(error = %clear_pet_err, "failed to clear ambient pet image");
                }
                if let Err(clear_err) = clear_result {
                    tracing::warn!(error = %clear_err, "failed to clear terminal UI");
                }
                return Err(err);
            }
        };
        let exit_resumable_thread = app.exit_resumable_thread();
        // D243 (#1412): the conversation on screen at quit is the one
        // `resume --last` reopens.
        if matches!(exit_reason, ExitReason::UserRequested)
            && let Some(thread) = exit_resumable_thread.as_ref()
        {
            crate::quit_thread_record::record_quit_thread(
                app.config.codex_home.as_ref(),
                thread.thread_id,
                chrono::Utc::now().timestamp(),
            );
        }
        let thread_id = exit_resumable_thread
            .as_ref()
            .map(|thread| thread.thread_id)
            .or_else(|| app.chat_widget.thread_id().or(app.primary_thread_id));
        let resume_hint = exit_resumable_thread
            .as_ref()
            .and_then(resume_hint_for_thread)
            .or_else(|| {
                resume_hint_for_pane_layout_thread(app.config.codex_home.as_ref(), thread_id)
            });
        Ok(AppExitInfo {
            token_usage: app.token_usage(),
            thread_id,
            resume_hint,
            update_action: app.pending_update_action,
            exit_reason,
        })
    }

    pub(crate) async fn handle_tui_event(
        &mut self,
        tui: &mut tui::Tui,
        app_server: &mut AppServerSession,
        event: TuiEvent,
    ) -> Result<AppRunControl> {
        if matches!(event, TuiEvent::Draw | TuiEvent::Resize) {
            self.handle_draw_pre_render(tui)?;
        }

        if self.overlay.is_some() {
            let _ = self.handle_backtrack_overlay_event(tui, event).await?;
        } else {
            match event {
                TuiEvent::Key(key_event) => {
                    self.handle_key_event(tui, app_server, key_event).await;
                }
                TuiEvent::Paste(pasted) => {
                    // Many terminals convert newlines to \r when pasting (e.g., iTerm2),
                    // but tui-textarea expects \n. Normalize CR to LF.
                    // [tui-textarea]: https://github.com/rhysd/tui-textarea/blob/4d18622eeac13b309e0ff6a55a46ac6706da68cf/src/textarea.rs#L782-L783
                    // [iTerm2]: https://github.com/gnachman/iTerm2/blob/5d0c0d9f68523cbd0494dad5422998964a2ecd8d/sources/iTermPasteHelper.m#L206-L216
                    let pasted = pasted.replace("\r", "\n");
                    self.chat_widget.handle_paste(pasted);
                }
                TuiEvent::Draw | TuiEvent::Resize => {
                    if self.backtrack_render_pending {
                        self.rebuild_transcript_after_backtrack(tui)?;
                        self.backtrack_render_pending = false;
                    }
                    self.chat_widget.maybe_post_pending_notification(tui);
                    if self
                        .chat_widget
                        .handle_paste_burst_tick(tui.frame_requester())
                    {
                        return Ok(AppRunControl::Continue);
                    }
                    // Allow widgets to process any pending timers before rendering.
                    self.chat_widget.pre_draw_tick();
                    let rendered_area = self.render_chat_widget_frame(tui)?;
                    if self.chat_widget.ambient_pet_image_enabled() {
                        let terminal_size = tui.terminal.size()?;
                        let ambient_pet_area = Rect::new(
                            /*x*/ 0,
                            /*y*/ 0,
                            terminal_size.width,
                            terminal_size.height,
                        );
                        if let Err(err) = tui.draw_ambient_pet_image(
                            self.chat_widget
                                .ambient_pet_draw(ambient_pet_area, rendered_area.bottom()),
                        ) {
                            self.handle_ambient_pet_image_render_error(tui, err)?;
                        }
                    }
                    if let Some(request) = self.chat_widget.pet_picker_preview_draw() {
                        if let Err(err) = tui.draw_pet_picker_preview_image(Some(request)) {
                            self.handle_pet_picker_preview_image_render_error(tui, err)?;
                        }
                    } else if self.chat_widget.should_clear_pet_picker_preview_image()
                        && let Err(err) = tui.draw_pet_picker_preview_image(/*request*/ None)
                    {
                        self.handle_pet_picker_preview_image_render_error(tui, err)?;
                    }
                    if self.chat_widget.external_editor_state() == ExternalEditorState::Requested {
                        self.chat_widget
                            .set_external_editor_state(ExternalEditorState::Active);
                        self.app_event_tx.send(AppEvent::LaunchExternalEditor);
                    }
                }
            }
        }
        Ok(AppRunControl::Continue)
    }

    pub(super) fn show_shutdown_feedback(&mut self, tui: &mut tui::Tui) -> Result<()> {
        self.disable_ambient_pet_before_shutdown(tui)?;
        self.chat_widget.show_shutdown_in_progress();
        self.handle_draw_pre_render(tui)?;
        self.chat_widget.pre_draw_tick();
        self.render_chat_widget_frame(tui)?;
        Ok(())
    }

    async fn show_backend_shutdown_control_events(
        &mut self,
        tui: &mut tui::Tui,
        detail: &str,
    ) -> Result<()> {
        let message = format!("CONTROL EVENT — TUI shutdown: {detail}");
        self.chat_widget.add_info_message(
            message.clone(),
            Some(
                "Stopping every pane and flushing rollouts before returning to the shell."
                    .to_string(),
            ),
        );
        let mut thread_ids = self
            .thread_event_channels
            .keys()
            .copied()
            .collect::<Vec<_>>();
        if let Some(thread_id) = self.primary_thread_id {
            thread_ids.push(thread_id);
        }
        thread_ids.sort_by_key(ThreadId::to_string);
        thread_ids.dedup();
        for thread_id in thread_ids {
            let notification =
                ServerNotification::Warning(codex_app_server_protocol::WarningNotification {
                    thread_id: Some(thread_id.to_string()),
                    message: message.clone(),
                });
            if let Err(error) = self
                .enqueue_thread_notification(thread_id, notification)
                .await
            {
                tracing::error!(
                    %thread_id,
                    error = ?error,
                    error_chain = %format!("{error:#}"),
                    "failed to record backend shutdown control event"
                );
            }
        }
        self.show_shutdown_feedback(tui)
    }

    fn render_chat_widget_frame(&mut self, tui: &mut tui::Tui) -> Result<Rect> {
        let width = tui.terminal.size()?.width;
        self.with_chat_widget_frame(width, |desired_height, chat_widget| {
            let mut rendered_area = Rect::default();
            tui.draw_with_resize_reflow(desired_height, |frame| {
                let area = frame.area();
                rendered_area = area;
                chat_widget.render(area, frame.buffer);
                self.chat_widget.note_rendered_width(area.width);
                if let Some((x, y)) = chat_widget.cursor_pos(area) {
                    frame.set_cursor_style(chat_widget.cursor_style(area));
                    frame.set_cursor_position((x, y));
                }
            })?;
            Ok(rendered_area)
        })
    }

    fn with_chat_widget_frame<T>(
        &self,
        width: u16,
        render: impl FnOnce(u16, &dyn Renderable) -> T,
    ) -> T {
        let chat_widget = self.chat_widget.as_renderable();
        render(chat_widget.desired_height(width), &chat_widget)
    }
}

fn apply_persisted_resume_runtime(
    config: &mut Config,
    model: Option<&str>,
    model_provider: &str,
    reasoning_effort: Option<codex_protocol::openai_models::ReasoningEffort>,
) -> Option<ResumeModelOverride> {
    let model = model?.to_string();
    config.model = Some(model.clone());
    config.model_reasoning_effort = reasoning_effort;
    if let Some(provider) = config.model_providers.get(model_provider).cloned() {
        config.model_provider_id = model_provider.to_string();
        config.model_provider = provider;
    } else {
        tracing::warn!(
            model,
            model_provider,
            "persisted resume provider is not configured in this client"
        );
    }
    Some(ResumeModelOverride {
        model: Some(model),
        model_provider: Some(model_provider.to_string()),
    })
}

impl Drop for App {
    fn drop(&mut self) {
        if let Err(err) = self.chat_widget.clear_managed_terminal_title() {
            tracing::debug!(error = %err, "failed to clear terminal title on app drop");
        }
    }
}

#[cfg(test)]
pub(super) mod test_support;
#[cfg(test)]
mod tests;
