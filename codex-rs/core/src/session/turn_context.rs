use super::*;
use crate::environment_selection::TurnEnvironmentSnapshot;
use crate::shell_snapshot::ShellSnapshotFile;
use codex_core_plugins::PluginCommandAttribution;
use codex_core_plugins::TrustedPluginRoots;
use codex_core_skills::HostSkillsSnapshot;
use codex_file_system::FileSystemSandboxContext;
use codex_model_provider::SharedModelProvider;
use codex_model_provider::create_model_provider;
use codex_protocol::SessionId;
use codex_protocol::ThreadId;
use codex_protocol::models::AdditionalPermissionProfile;
use codex_protocol::openai_models::ModelInfo;
use codex_protocol::protocol::ErrorEvent;
use codex_protocol::protocol::MultiAgentVersion;
use codex_protocol::protocol::ThreadHistoryMode;
use codex_protocol::protocol::TurnEnvironmentSelection;
use codex_sandboxing::compatibility_sandbox_policy_for_permission_profile;
use codex_sandboxing::policy_transforms::effective_permission_profile;
use codex_utils_path_uri::PathUri;
use futures::FutureExt;
use futures::future::BoxFuture;
use futures::future::Shared;
use std::sync::atomic::AtomicBool;
use std::sync::atomic::AtomicU8;
use std::sync::atomic::AtomicU32;
use std::sync::atomic::AtomicU64;
use std::sync::atomic::Ordering;
use tracing::instrument;

#[derive(Clone, Debug)]
pub(crate) struct TurnSkillsContext {
    pub(crate) snapshot: HostSkillsSnapshot,
    pub(crate) implicit_invocation_seen_skills: Arc<Mutex<HashSet<String>>>,
}

impl TurnSkillsContext {
    pub(crate) fn new(snapshot: HostSkillsSnapshot) -> Self {
        Self {
            snapshot,
            implicit_invocation_seen_skills: Arc::new(Mutex::new(HashSet::new())),
        }
    }
}

pub(crate) type ShellSnapshotTask = Shared<BoxFuture<'static, Option<Arc<ShellSnapshotFile>>>>;

const STRICT_APPLY_PATCH_ONE_GRAMMAR_FAILURE: u8 = 1;
const STRICT_APPLY_PATCH_FALLBACK_ACTIVE: u8 = 2;

#[derive(Debug, Default)]
pub(crate) struct ModelEditProtocolState {
    strict_apply_patch_grammar_state: AtomicU8,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum PatchFallbackTransition {
    Unchanged { consecutive_failures: u8 },
    Activated { consecutive_failures: u8 },
}

impl PatchFallbackTransition {
    pub(crate) fn consecutive_failures(self) -> u8 {
        match self {
            Self::Unchanged {
                consecutive_failures,
            }
            | Self::Activated {
                consecutive_failures,
            } => consecutive_failures,
        }
    }
}

impl ModelEditProtocolState {
    fn record_grammar_failure(&self) -> PatchFallbackTransition {
        let mut current = self
            .strict_apply_patch_grammar_state
            .load(Ordering::Acquire);
        loop {
            if current >= STRICT_APPLY_PATCH_FALLBACK_ACTIVE {
                return PatchFallbackTransition::Unchanged {
                    consecutive_failures: STRICT_APPLY_PATCH_FALLBACK_ACTIVE,
                };
            }
            let next = current.saturating_add(1);
            match self.strict_apply_patch_grammar_state.compare_exchange(
                current,
                next,
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) if next == STRICT_APPLY_PATCH_FALLBACK_ACTIVE => {
                    return PatchFallbackTransition::Activated {
                        consecutive_failures: next,
                    };
                }
                Ok(_) => {
                    return PatchFallbackTransition::Unchanged {
                        consecutive_failures: next,
                    };
                }
                Err(observed) => current = observed,
            }
        }
    }

    fn record_parse_success(&self) {
        let mut current = self
            .strict_apply_patch_grammar_state
            .load(Ordering::Acquire);
        while current == STRICT_APPLY_PATCH_ONE_GRAMMAR_FAILURE {
            match self.strict_apply_patch_grammar_state.compare_exchange(
                current,
                0,
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => return,
                Err(observed) => current = observed,
            }
        }
    }

    fn fallback_enabled(&self) -> bool {
        self.strict_apply_patch_grammar_state
            .load(Ordering::Acquire)
            >= STRICT_APPLY_PATCH_FALLBACK_ACTIVE
    }
}

#[derive(Debug, Default)]
pub(crate) struct ExplicitToolBudgetState {
    shell_command_limit: AtomicU64,
    shell_commands_used: AtomicU64,
}

#[derive(Debug, Default)]
pub(crate) struct MalformedToolCallState {
    counts: Mutex<HashMap<String, u8>>,
}

impl MalformedToolCallState {
    async fn record(&self, signature: String) -> u8 {
        let mut counts = self.counts.lock().await;
        let count = counts.entry(signature).or_insert(0);
        *count = count.saturating_add(1);
        *count
    }
}

/// Tracks repeated direct tool calls across every model request in a turn.
/// Each sampling request builds a fresh tool runtime, so a model that re-issues
/// one call per request, or cycles through the same few calls, is only visible
/// at turn scope.
#[derive(Debug, Default)]
pub(crate) struct RepeatedToolCallState {
    streak: Mutex<Option<(String, u32)>>,
    /// Per call signature: every result identity it has returned this turn,
    /// and how many times it has returned an already-seen result since the
    /// turn last learned something new.
    results: Mutex<HashMap<String, (HashSet<String>, u32)>>,
    refusals: AtomicU32,
}

/// How a direct call repeats earlier ones in the same turn.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct ToolCallRepetition {
    /// Times in a row this exact call has now been made.
    pub(crate) consecutive_calls: u32,
    /// Times this exact call has already returned its latest result.
    pub(crate) identical_results: u32,
}

impl RepeatedToolCallState {
    async fn record_call(&self, signature: &str) -> ToolCallRepetition {
        let consecutive_calls = {
            let mut streak = self.streak.lock().await;
            let count = match streak.as_ref() {
                Some((last, count)) if last == signature => count.saturating_add(1),
                _ => 1,
            };
            *streak = Some((signature.to_string(), count));
            count
        };
        let identical_results = self
            .results
            .lock()
            .await
            .get(signature)
            .map_or(0, |(_, count)| *count);
        ToolCallRepetition {
            consecutive_calls,
            identical_results,
        }
    }

    /// Records a call's result. A result this call has never returned before
    /// in the turn is progress (for example the output of a new edit, or a
    /// test that now reports something new), so every repeat count restarts.
    /// Results seen before, including ones a model oscillates between by
    /// toggling an edit back and forth, are not progress.
    async fn record_result(&self, signature: String, identity: String) {
        let mut results = self.results.lock().await;
        if let Some((seen, count)) = results.get_mut(&signature)
            && seen.contains(&identity)
        {
            *count = count.saturating_add(1);
            return;
        }
        for (_, count) in results.values_mut() {
            *count = 0;
        }
        let (seen, count) = results.entry(signature).or_default();
        seen.insert(identity);
        *count = 1;
    }

    fn record_refusal(&self) -> u32 {
        self.refusals
            .fetch_add(1, Ordering::AcqRel)
            .saturating_add(1)
    }
}

#[derive(Clone)]
pub(crate) struct TurnEnvironment {
    pub(crate) environment_id: String,
    pub(crate) environment: Arc<Environment>,
    cwd: PathUri,
    workspace_roots: Vec<PathUri>,
    pub(crate) shell: Option<shell::Shell>,
    pub(crate) shell_snapshot: ShellSnapshotTask,
}

impl TurnEnvironment {
    pub(crate) fn new(
        environment_id: String,
        environment: Arc<Environment>,
        cwd: PathUri,
        workspace_roots: Vec<PathUri>,
        shell: Option<shell::Shell>,
    ) -> Self {
        Self {
            environment_id,
            environment,
            cwd,
            workspace_roots,
            shell,
            shell_snapshot: futures::future::ready(None).boxed().shared(),
        }
    }

    pub(crate) fn shell_snapshot(&self, cwd: &AbsolutePathBuf) -> Option<AbsolutePathBuf> {
        if self.cwd != PathUri::from_abs_path(cwd) {
            return None;
        }
        self.shell_snapshot
            .peek()?
            .as_deref()
            .map(ShellSnapshotFile::path)
    }

    pub(crate) fn cwd(&self) -> &PathUri {
        &self.cwd
    }

    pub(crate) fn workspace_roots(&self) -> &[PathUri] {
        &self.workspace_roots
    }

    pub(crate) fn selection(&self) -> TurnEnvironmentSelection {
        TurnEnvironmentSelection {
            environment_id: self.environment_id.clone(),
            cwd: self.cwd.clone(),
            workspace_roots: self.workspace_roots.clone(),
        }
    }
}

impl std::fmt::Debug for TurnEnvironment {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TurnEnvironment")
            .field("environment_id", &self.environment_id)
            .field("environment", &self.environment)
            .field("cwd", &self.cwd)
            .field("workspace_roots", &self.workspace_roots)
            .field("shell", &self.shell)
            .finish_non_exhaustive()
    }
}

/// The context needed for a single turn of the thread.
#[derive(Debug)]
pub struct TurnContext {
    pub(crate) sub_id: String,
    pub(crate) trace_id: Option<String>,
    pub(crate) realtime_active: bool,
    pub(crate) code_mode_available: bool,
    pub config: Arc<Config>,
    pub(crate) auth_manager: Option<Arc<AuthManager>>,
    pub(crate) model_info: ModelInfo,
    pub(crate) session_telemetry: SessionTelemetry,
    pub(crate) provider: SharedModelProvider,
    pub(crate) reasoning_effort: Option<ReasoningEffortConfig>,
    pub(crate) reasoning_summary: ReasoningSummaryConfig,
    pub(crate) session_source: SessionSource,
    pub(crate) history_mode: ThreadHistoryMode,
    pub(crate) parent_thread_id: Option<ThreadId>,
    pub(crate) originator: String,
    pub(crate) environments: TurnEnvironmentSnapshot,
    /// The session's absolute working directory. All relative paths provided
    /// by the model as well as sandbox policies are resolved against this path
    /// instead of `std::env::current_dir()`.
    #[deprecated(note = "use the selected turn environment cwd instead")]
    pub(crate) cwd: AbsolutePathBuf,
    pub(crate) current_date: Option<String>,
    pub(crate) timezone: Option<String>,
    pub(crate) app_server_client_name: Option<String>,
    pub(crate) developer_instructions: Option<String>,
    pub(crate) mode: ModeKind,
    pub(crate) collaboration_mode_developer_instructions: Option<String>,
    pub(crate) multi_agent_version: MultiAgentVersion,
    pub(crate) personality: Option<Personality>,
    pub(crate) approval_policy: Constrained<AskForApproval>,
    pub(crate) permission_profile: PermissionProfile,
    pub(crate) network: Option<NetworkProxy>,
    pub(crate) windows_sandbox_level: WindowsSandboxLevel,
    pub(crate) available_models: Vec<ModelPreset>,
    pub(crate) unified_exec_shell_mode: UnifiedExecShellMode,
    pub(crate) final_output_json_schema: Option<Value>,
    pub(crate) dynamic_tools: Vec<DynamicToolSpec>,
    pub(crate) turn_metadata_state: Arc<TurnMetadataState>,
    pub(crate) extension_data: Arc<codex_extension_api::ExtensionData>,
    pub(crate) turn_skills: TurnSkillsContext,
    pub(crate) turn_timing_state: Arc<TurnTimingState>,
    /// Whether this turn was admitted from collaboration work that requested a model turn.
    /// Direct operator turns in a managed child pane must not report their result to the
    /// child's structural parent.
    pub(crate) parent_completion_expected: Arc<AtomicBool>,
    pub(crate) terminal_error: Arc<Mutex<Option<ErrorEvent>>>,
    pub(crate) model_edit_protocol_state: Arc<ModelEditProtocolState>,
    pub(crate) explicit_tool_budget_state: Arc<ExplicitToolBudgetState>,
    pub(crate) malformed_tool_call_state: Arc<MalformedToolCallState>,
    pub(crate) repeated_tool_call_state: Arc<RepeatedToolCallState>,
    pub(crate) pending_tool_calls: Arc<crate::tools::parallel::PendingToolCalls>,
    pub(crate) server_model_warning_emitted: AtomicBool,
    pub(crate) provider_cache_pressure_warning_emitted: AtomicBool,
    pub(crate) model_verification_emitted: AtomicBool,
    /// The turn already warned that developer accounting stopped recording it.
    pub(crate) accounting_gap_warning_emitted: AtomicBool,
}

enum TurnMultiAgentRuntime {
    ResolveAndStore,
    Preview,
}

impl TurnContext {
    pub(crate) fn collaboration_mode(&self) -> CollaborationMode {
        CollaborationMode {
            mode: self.mode,
            settings: Settings {
                model: self.model_info.slug.clone(),
                reasoning_effort: self.reasoning_effort.clone(),
                developer_instructions: self.collaboration_mode_developer_instructions.clone(),
            },
        }
    }

    pub(crate) fn plugin_attribution_for_command(
        &self,
        command: &[String],
        cwd: &AbsolutePathBuf,
    ) -> Option<PluginCommandAttribution> {
        self.extension_data
            .get::<TrustedPluginRoots>()?
            .resolve_attribution(command, cwd)
    }

    pub(crate) fn permission_profile(&self) -> PermissionProfile {
        self.permission_profile.clone()
    }

    pub(crate) fn file_system_sandbox_policy(&self) -> FileSystemSandboxPolicy {
        self.permission_profile.file_system_sandbox_policy()
    }

    pub(crate) fn network_sandbox_policy(&self) -> NetworkSandboxPolicy {
        self.permission_profile.network_sandbox_policy()
    }

    pub(crate) fn sandbox_policy(&self) -> SandboxPolicy {
        compatibility_sandbox_policy_for_permission_profile(
            &self.permission_profile,
            #[allow(deprecated)]
            &self.cwd,
        )
    }

    pub(crate) fn effective_reasoning_effort(&self) -> Option<ReasoningEffortConfig> {
        self.reasoning_effort
            .clone()
            .or_else(|| self.model_info.default_reasoning_level.clone())
    }

    pub(crate) fn effective_reasoning_effort_for_tracing(&self) -> String {
        self.effective_reasoning_effort()
            .map(|effort| effort.to_string())
            .unwrap_or_else(|| "default".to_string())
    }

    pub(crate) fn model_context_window(&self) -> Option<i64> {
        let effective_context_window_percent = self.model_info.effective_context_window_percent;
        self.model_info
            .resolved_context_window()
            .map(|context_window| {
                context_window.saturating_mul(effective_context_window_percent) / 100
            })
    }

    pub(crate) fn apps_enabled(&self) -> bool {
        let uses_codex_backend = self
            .auth_manager
            .as_deref()
            .is_some_and(AuthManager::current_auth_uses_codex_backend);
        self.config
            .features
            .apps_enabled_for_auth(uses_codex_backend)
            && self.config.orchestrator_mcp_enabled
    }

    pub(crate) fn record_strict_apply_patch_grammar_failure(&self) -> PatchFallbackTransition {
        self.model_edit_protocol_state.record_grammar_failure()
    }

    pub(crate) fn record_strict_apply_patch_parse_success(&self) {
        self.model_edit_protocol_state.record_parse_success();
    }

    pub(crate) fn structured_edit_fallback_enabled(&self) -> bool {
        self.model_edit_protocol_state.fallback_enabled()
    }

    pub(crate) async fn record_malformed_tool_call(&self, signature: String) -> u8 {
        self.malformed_tool_call_state.record(signature).await
    }

    pub(crate) async fn record_direct_tool_call(&self, signature: &str) -> ToolCallRepetition {
        self.repeated_tool_call_state.record_call(signature).await
    }

    pub(crate) async fn record_direct_tool_result(&self, signature: String, identity: String) {
        self.repeated_tool_call_state
            .record_result(signature, identity)
            .await
    }

    /// Returns how many repeated calls this turn has refused, including this one.
    pub(crate) fn record_repeated_tool_call_refusal(&self) -> u32 {
        self.repeated_tool_call_state.record_refusal()
    }

    pub(crate) fn set_explicit_shell_command_budget(&self, limit: u64) {
        if limit == 0 {
            return;
        }
        let mut current = self
            .explicit_tool_budget_state
            .shell_command_limit
            .load(Ordering::Acquire);
        loop {
            if current != 0 && current <= limit {
                return;
            }
            match self
                .explicit_tool_budget_state
                .shell_command_limit
                .compare_exchange(current, limit, Ordering::AcqRel, Ordering::Acquire)
            {
                Ok(_) => return,
                Err(next) => current = next,
            }
        }
    }

    pub(crate) fn record_shell_command_against_explicit_budget(
        &self,
    ) -> Result<Option<(u64, u64)>, u64> {
        let limit = self
            .explicit_tool_budget_state
            .shell_command_limit
            .load(Ordering::Acquire);
        if limit == 0 {
            return Ok(None);
        }

        let mut used = self
            .explicit_tool_budget_state
            .shell_commands_used
            .load(Ordering::Acquire);
        loop {
            if used >= limit {
                return Err(limit);
            }
            match self
                .explicit_tool_budget_state
                .shell_commands_used
                .compare_exchange(
                    used,
                    used.saturating_add(1),
                    Ordering::AcqRel,
                    Ordering::Acquire,
                ) {
                Ok(_) => return Ok(Some((used.saturating_add(1), limit))),
                Err(next) => used = next,
            }
        }
    }

    pub(crate) async fn with_model(
        &self,
        model: String,
        models_manager: &SharedModelsManager,
    ) -> Self {
        let mut config = (*self.config).clone();
        config.model = Some(model.clone());
        let model_info =
            crate::model_info_for_provider::model_info_for_config(models_manager, &model, &config)
                .await;
        let supported_reasoning_levels = model_info
            .supported_reasoning_levels
            .iter()
            .map(|preset| preset.effort.clone())
            .collect::<Vec<_>>();
        let reasoning_effort = if let Some(current_reasoning_effort) = self.reasoning_effort.clone()
        {
            if supported_reasoning_levels.contains(&current_reasoning_effort) {
                Some(current_reasoning_effort)
            } else {
                supported_reasoning_levels
                    .get(supported_reasoning_levels.len().saturating_sub(1) / 2)
                    .cloned()
                    .or_else(|| model_info.default_reasoning_level.clone())
            }
        } else {
            supported_reasoning_levels
                .get(supported_reasoning_levels.len().saturating_sub(1) / 2)
                .cloned()
                .or_else(|| model_info.default_reasoning_level.clone())
        };
        config.model_reasoning_effort = reasoning_effort.clone();

        let available_models = models_manager
            .list_models(
                RefreshStrategy::OnlineIfUncached,
                config.http_client_factory(),
            )
            .await;

        Self {
            sub_id: self.sub_id.clone(),
            trace_id: self.trace_id.clone(),
            realtime_active: self.realtime_active,
            code_mode_available: self.code_mode_available,
            config: Arc::new(config),
            auth_manager: self.auth_manager.clone(),
            model_info: model_info.clone(),
            session_telemetry: self
                .session_telemetry
                .clone()
                .with_model(model.as_str(), model_info.slug.as_str()),
            provider: self.provider.clone(),
            reasoning_effort,
            reasoning_summary: self.reasoning_summary,
            session_source: self.session_source.clone(),
            history_mode: self.history_mode,
            parent_thread_id: self.parent_thread_id,
            originator: self.originator.clone(),
            environments: self.environments.clone(),
            #[allow(deprecated)]
            cwd: self.cwd.clone(),
            current_date: self.current_date.clone(),
            timezone: self.timezone.clone(),
            app_server_client_name: self.app_server_client_name.clone(),
            developer_instructions: self.developer_instructions.clone(),
            mode: self.mode,
            collaboration_mode_developer_instructions: self
                .collaboration_mode_developer_instructions
                .clone(),
            multi_agent_version: self.multi_agent_version,
            personality: self.personality,
            approval_policy: self.approval_policy.clone(),
            permission_profile: self.permission_profile.clone(),
            network: self.network.clone(),
            windows_sandbox_level: self.windows_sandbox_level,
            available_models,
            unified_exec_shell_mode: self.unified_exec_shell_mode.clone(),
            final_output_json_schema: self.final_output_json_schema.clone(),
            dynamic_tools: self.dynamic_tools.clone(),
            turn_metadata_state: self.turn_metadata_state.clone(),
            extension_data: Arc::clone(&self.extension_data),
            turn_skills: self.turn_skills.clone(),
            turn_timing_state: Arc::clone(&self.turn_timing_state),
            parent_completion_expected: Arc::clone(&self.parent_completion_expected),
            terminal_error: Arc::clone(&self.terminal_error),
            model_edit_protocol_state: Arc::clone(&self.model_edit_protocol_state),
            explicit_tool_budget_state: Arc::clone(&self.explicit_tool_budget_state),
            malformed_tool_call_state: Arc::clone(&self.malformed_tool_call_state),
            repeated_tool_call_state: Arc::clone(&self.repeated_tool_call_state),
            pending_tool_calls: Arc::clone(&self.pending_tool_calls),
            server_model_warning_emitted: AtomicBool::new(
                self.server_model_warning_emitted.load(Ordering::Relaxed),
            ),
            provider_cache_pressure_warning_emitted: AtomicBool::new(
                self.provider_cache_pressure_warning_emitted
                    .load(Ordering::Relaxed),
            ),
            model_verification_emitted: AtomicBool::new(
                self.model_verification_emitted.load(Ordering::Relaxed),
            ),
            accounting_gap_warning_emitted: AtomicBool::new(
                self.accounting_gap_warning_emitted.load(Ordering::Relaxed),
            ),
        }
    }

    pub(crate) fn file_system_sandbox_context(
        &self,
        additional_permissions: Option<AdditionalPermissionProfile>,
        environment: &TurnEnvironment,
    ) -> FileSystemSandboxContext {
        let mut permissions = effective_permission_profile(
            self.config.permissions.permission_profile(),
            additional_permissions.as_ref(),
        );
        // PF-27-S02: in-process file tools get the protected profile, or no
        // file access when the turn's permissions cannot be protected.
        if let Some(contract) = crate::security::launch_contract::active() {
            let cwd = environment.cwd().to_abs_path().ok();
            let sandboxed = cwd.is_some() && cfg!(any(target_os = "macos", target_os = "linux"));
            let cwd = cwd.map(|cwd| cwd.to_path_buf()).unwrap_or_default();
            permissions = contract.file_tool_permissions(&permissions, sandboxed, &cwd);
        }
        FileSystemSandboxContext {
            permissions: permissions.into(),
            cwd: Some(environment.cwd().clone()),
            workspace_roots: environment.workspace_roots().to_vec(),
            windows_sandbox_level: self.windows_sandbox_level,
            windows_sandbox_private_desktop: self
                .config
                .permissions
                .windows_sandbox_private_desktop,
            windows_sandbox_proxy_settings_mode: None,
            use_legacy_landlock: self.config.features.use_legacy_landlock(),
        }
    }

    fn non_legacy_file_system_sandbox_policy(&self) -> Option<FileSystemSandboxPolicy> {
        // Omit the derived split filesystem policy when it is equivalent to
        // the legacy sandbox policy. This keeps turn-context payloads stable
        // while both fields exist; once callers consume only the split policy,
        // this comparison and the legacy projection should go away.
        let legacy_file_system_sandbox_policy =
            FileSystemSandboxPolicy::from_legacy_sandbox_policy_for_cwd(
                &self.sandbox_policy(),
                #[allow(deprecated)]
                &self.cwd,
            );
        let file_system_sandbox_policy = self.file_system_sandbox_policy();
        (file_system_sandbox_policy != legacy_file_system_sandbox_policy)
            .then_some(file_system_sandbox_policy)
    }

    /// #578: the provider (and named account) recorded as the producer of
    /// this turn's model output items: `<provider id>`, or
    /// `<provider id>:<account>` for a named account.
    pub(crate) fn output_producer(&self) -> String {
        crate::context_manager::provider_items::producer_key(
            &self.config.model_provider_id,
            self.provider.info().account.as_ref(),
        )
    }

    pub(crate) fn to_turn_context_item(&self) -> TurnContextItem {
        let workspace_roots = self.config.effective_workspace_roots();
        #[allow(deprecated)]
        let cwd = self.cwd.clone();
        TurnContextItem {
            turn_id: Some(self.sub_id.clone()),
            cwd,
            workspace_roots: (!workspace_roots.is_empty()).then_some(workspace_roots),
            current_date: self.current_date.clone(),
            timezone: self.timezone.clone(),
            approval_policy: self.approval_policy.value(),
            approvals_reviewer: Some(self.config.approvals_reviewer),
            sandbox_policy: self.sandbox_policy(),
            permission_profile: Some(self.permission_profile()),
            network: self.turn_context_network_item(),
            file_system_sandbox_policy: self.non_legacy_file_system_sandbox_policy(),
            model: self.model_info.slug.clone(),
            model_provider: Some(self.config.model_provider_id.clone()),
            provider_account: self
                .config
                .features
                .enabled(Feature::NamedAccounts)
                .then(|| {
                    // #1129: an account borrowed from another provider (same
                    // API key, see `with_account_from`) keeps its owner.
                    self.provider.info().account.as_ref().map_or_else(
                        || "default".to_string(),
                        |account| {
                            if account.provider_id == self.config.model_provider_id {
                                account.name.clone()
                            } else {
                                account.to_string()
                            }
                        },
                    )
                }),
            comp_hash: self.model_info.comp_hash.clone(),
            personality: self.personality,
            collaboration_mode: Some(self.collaboration_mode()),
            multi_agent_version: Some(self.multi_agent_version),
            multi_agent_mode: None,
            realtime_active: Some(self.realtime_active),
            effort: self.reasoning_effort.clone(),
            security_level: Some(self.config.security_level),
            summary: ReasoningSummaryConfig::Auto,
        }
    }

    fn turn_context_network_item(&self) -> Option<TurnContextNetworkItem> {
        let network = self
            .config
            .config_layer_stack
            .requirements()
            .network
            .as_ref()?;
        Some(TurnContextNetworkItem {
            allowed_domains: network
                .domains
                .as_ref()
                .and_then(codex_config::NetworkDomainPermissionsToml::allowed_domains)
                .unwrap_or_default(),
            denied_domains: network
                .domains
                .as_ref()
                .and_then(codex_config::NetworkDomainPermissionsToml::denied_domains)
                .unwrap_or_default(),
        })
    }
}

fn local_time_context() -> (String, String) {
    match iana_time_zone::get_timezone() {
        Ok(timezone) => (Local::now().format("%Y-%m-%d").to_string(), timezone),
        Err(_) => (
            Utc::now().format("%Y-%m-%d").to_string(),
            "Etc/UTC".to_string(),
        ),
    }
}

impl Session {
    /// Don't expand the number of mutated arguments on config. We are in the process of getting rid of it.
    pub(crate) fn build_per_turn_config(
        session_configuration: &SessionConfiguration,
        cwd: AbsolutePathBuf,
    ) -> Config {
        // todo(aibrahim): store this state somewhere else so we don't need to mut config
        let config = session_configuration.original_config_do_not_use.clone();
        let mut per_turn_config = (*config).clone();
        per_turn_config.model = Some(session_configuration.collaboration_mode.model().to_string());
        per_turn_config.cwd = cwd;
        let workspace_roots = session_configuration.primary_workspace_roots();
        per_turn_config.workspace_roots = workspace_roots.clone();
        per_turn_config
            .permissions
            .set_workspace_roots(workspace_roots);
        per_turn_config.model_reasoning_effort =
            session_configuration.collaboration_mode.reasoning_effort();
        per_turn_config.model_reasoning_summary = session_configuration.model_reasoning_summary;
        if let Some(runtime_limit) = session_configuration.runtime_model_context_window {
            per_turn_config.model_context_window = Some(
                per_turn_config
                    .model_context_window
                    .map_or(runtime_limit, |configured| configured.min(runtime_limit)),
            );
        }
        per_turn_config.service_tier = session_configuration.service_tier.clone();
        per_turn_config.personality = session_configuration.personality;
        per_turn_config.approvals_reviewer = session_configuration.approvals_reviewer;
        session_configuration
            .apply_permission_profile_to_permissions(&mut per_turn_config.permissions);
        let permission_profile = session_configuration.permission_profile();
        let resolved_web_search_mode =
            resolve_web_search_mode_for_turn(&per_turn_config.web_search_mode, &permission_profile);
        if let Err(err) = per_turn_config
            .web_search_mode
            .set(resolved_web_search_mode)
        {
            let fallback_value = per_turn_config.web_search_mode.value();
            tracing::warn!(
                error = %err,
                ?resolved_web_search_mode,
                ?fallback_value,
                "resolved web_search_mode is disallowed by requirements; keeping constrained value"
            );
        }
        per_turn_config.features = config.features.clone();
        per_turn_config
    }

    pub(crate) fn build_effective_session_config(
        session_configuration: &SessionConfiguration,
    ) -> Config {
        let mut config =
            Self::build_per_turn_config(session_configuration, session_configuration.cwd().clone());
        config.model = Some(session_configuration.collaboration_mode.model().to_string());
        config.permissions.approval_policy = session_configuration.approval_policy.clone();
        config
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn make_turn_context(
        thread_id: ThreadId,
        session_id: SessionId,
        auth_manager: Option<Arc<AuthManager>>,
        session_telemetry: &SessionTelemetry,
        provider: ModelProviderInfo,
        session_configuration: &SessionConfiguration,
        multi_agent_version: MultiAgentVersion,
        user_shell: &shell::Shell,
        shell_zsh_path: Option<&PathBuf>,
        main_execve_wrapper_exe: Option<&PathBuf>,
        per_turn_config: Config,
        model_info: ModelInfo,
        models_manager: &SharedModelsManager,
        network: Option<NetworkProxy>,
        environments: TurnEnvironmentSnapshot,
        cwd: AbsolutePathBuf,
        sub_id: String,
        skills_snapshot: HostSkillsSnapshot,
    ) -> TurnContext {
        let collaboration_mode = &session_configuration.collaboration_mode;
        let reasoning_effort = collaboration_mode.reasoning_effort();
        let reasoning_summary = session_configuration
            .model_reasoning_summary
            .unwrap_or(model_info.default_reasoning_summary);
        let session_telemetry = session_telemetry.clone().with_model(
            session_configuration.collaboration_mode.model(),
            model_info.slug.as_str(),
        );
        let session_source = session_configuration.session_source.clone();
        let auth_manager_for_context = auth_manager.clone();
        let provider_for_context = create_model_provider(provider, auth_manager);
        let session_telemetry_for_context = session_telemetry;
        let mut available_models = models_manager.try_list_models().unwrap_or_default();
        let runtime_provider_id = per_turn_config.model_provider_id.clone();
        if runtime_provider_id.starts_with("gpu-")
            && !available_models
                .iter()
                .any(|preset| preset.model == model_info.slug)
        {
            let mut preset = ModelPreset::from(model_info.clone());
            preset.provider_id = Some(runtime_provider_id.clone());
            preset.orchestration = Some(ModelOrchestrationMetadata::Eligible {
                provider_id: runtime_provider_id,
                capability: ModelCapabilityTier::Balanced,
                billing: ModelBilling::Local,
            });
            available_models.push(preset);
        }
        let unified_exec_shell_mode = UnifiedExecShellMode::for_session(
            codex_tools::unified_exec_feature_mode_for_features(per_turn_config.features.get()),
            crate::tools::tool_user_shell_type(user_shell),
            shell_zsh_path,
            main_execve_wrapper_exe,
        );

        let mut per_turn_config = per_turn_config;
        super::token_budget::apply_model_defaults(&mut per_turn_config, &model_info);
        per_turn_config.service_tier = get_service_tier(
            per_turn_config.service_tier,
            per_turn_config.features.enabled(Feature::FastMode),
            &model_info,
        );
        let permission_profile = per_turn_config.permissions.effective_permission_profile();
        let per_turn_config = Arc::new(per_turn_config);
        let turn_metadata_state = Arc::new(TurnMetadataState::new(
            session_id.to_string(),
            thread_id.to_string(),
            session_configuration.forked_from_thread_id,
            session_configuration.parent_thread_id,
            &session_configuration.session_source,
            session_configuration.thread_source.clone(),
            sub_id.clone(),
            cwd.clone(),
            &permission_profile,
            session_configuration.windows_sandbox_level,
            network.is_some(),
        ));
        let (current_date, timezone) = local_time_context();
        let extension_data = Arc::new(codex_extension_api::ExtensionData::new(sub_id.clone()));
        extension_data.insert(skills_snapshot.clone());
        TurnContext {
            sub_id,
            trace_id: current_span_trace_id(),
            realtime_active: false,
            code_mode_available: true,
            config: per_turn_config,
            auth_manager: auth_manager_for_context,
            model_info,
            session_telemetry: session_telemetry_for_context,
            provider: provider_for_context,
            reasoning_effort,
            reasoning_summary,
            session_source,
            history_mode: session_configuration.history_mode,
            parent_thread_id: session_configuration.parent_thread_id,
            originator: session_configuration.originator.clone(),
            environments,
            #[allow(deprecated)]
            cwd,
            current_date: Some(current_date),
            timezone: Some(timezone),
            app_server_client_name: session_configuration.app_server_client_name.clone(),
            developer_instructions: session_configuration.developer_instructions.clone(),
            mode: collaboration_mode.mode,
            collaboration_mode_developer_instructions: collaboration_mode
                .settings
                .developer_instructions
                .clone(),
            multi_agent_version,
            personality: session_configuration.personality,
            approval_policy: session_configuration.approval_policy.clone(),
            permission_profile,
            network,
            windows_sandbox_level: session_configuration.windows_sandbox_level,
            available_models,
            unified_exec_shell_mode,
            final_output_json_schema: None,
            dynamic_tools: session_configuration.dynamic_tools.clone(),
            turn_metadata_state,
            extension_data,
            turn_skills: TurnSkillsContext::new(skills_snapshot),
            turn_timing_state: Arc::new(TurnTimingState::default()),
            parent_completion_expected: Arc::new(AtomicBool::new(false)),
            terminal_error: Arc::new(Mutex::new(None)),
            model_edit_protocol_state: Arc::new(ModelEditProtocolState::default()),
            explicit_tool_budget_state: Arc::new(ExplicitToolBudgetState::default()),
            malformed_tool_call_state: Arc::new(MalformedToolCallState::default()),
            repeated_tool_call_state: Arc::new(RepeatedToolCallState::default()),
            pending_tool_calls: Arc::default(),
            server_model_warning_emitted: AtomicBool::new(false),
            provider_cache_pressure_warning_emitted: AtomicBool::new(false),
            model_verification_emitted: AtomicBool::new(false),
            accounting_gap_warning_emitted: AtomicBool::new(false),
        }
    }

    pub(crate) async fn new_turn_with_sub_id(
        &self,
        sub_id: String,
        mut updates: SessionSettingsUpdate,
    ) -> CodexResult<Arc<TurnContext>> {
        // GPU endpoints are process-local controller transports and can change while a
        // session remains open. Refresh the selected runtime provider before every turn so
        // recovery does not leave the session's ModelClient pinned to a dead local port.
        self.ensure_runtime_model_provider(&mut updates).await;
        let notify_config_contributors = !self.services.extensions.config_contributors().is_empty();
        let update_result: CodexResult<_> = {
            let mut state = self.state.lock().await;
            match state.session_configuration.clone().apply(&updates) {
                Ok(next) => {
                    let mcp_inputs_changed = state.session_configuration.mcp_inputs_differ(&next);
                    let previous_permission_profile =
                        state.session_configuration.permission_profile();
                    let next_permission_profile = next.permission_profile();
                    let permission_profile_changed =
                        previous_permission_profile != next_permission_profile;
                    let previous_config = notify_config_contributors.then(|| {
                        Self::build_effective_session_config(&state.session_configuration)
                    });
                    let new_config = notify_config_contributors
                        .then(|| Self::build_effective_session_config(&next));
                    let model_provider_changed = state
                        .session_configuration
                        .original_config_do_not_use
                        .model_provider_id
                        != next.original_config_do_not_use.model_provider_id
                        || state.session_configuration.provider != next.provider;
                    // PF-84: as in update_settings, the account follows first.
                    if model_provider_changed {
                        self.follow_provider_account(&next);
                    }
                    let follows_model_list =
                        model_provider_changed && self.model_list_follows_this_session(&next);
                    if model_provider_changed {
                        // Match update_settings: frame guards must see this provider
                        // before its configuration is published, under the same writer lock.
                        self.services
                            .replace_model_client(self.build_model_client_for_configuration(&next));
                    }
                    let stale_startup_prewarm = if model_provider_changed {
                        state.take_session_startup_prewarm()
                    } else {
                        None
                    };
                    if updates.environments.is_some() {
                        self.services
                            .turn_environments
                            .update_selections(next.environment_selections());
                    }
                    if mcp_inputs_changed {
                        self.mark_mcp_runtime_dirty();
                    }
                    state.session_configuration = next.clone();
                    Ok((
                        next,
                        mcp_inputs_changed,
                        permission_profile_changed,
                        previous_config,
                        new_config,
                        stale_startup_prewarm,
                        follows_model_list,
                    ))
                }
                Err(err) => Err(CodexErr::InvalidRequest(err.to_string())),
            }
        };

        let (
            session_configuration,
            mcp_inputs_changed,
            permission_profile_changed,
            previous_config,
            new_config,
            stale_startup_prewarm,
            follows_model_list,
        ) = match update_result {
            Ok(update) => update,
            Err(err) => {
                let message = err.to_string();
                self.send_event_raw(Event {
                    id: sub_id.clone(),
                    msg: EventMsg::Error(ErrorEvent {
                        message: message.clone(),
                        codex_error_info: Some(CodexErrorInfo::BadRequest),
                    }),
                })
                .await;
                return Err(CodexErr::InvalidRequest(message));
            }
        };
        self.emit_config_changed_contributors(previous_config.as_ref(), new_config.as_ref());
        if mcp_inputs_changed {
            self.schedule_mcp_prewarm();
        }
        if follows_model_list {
            self.follow_model_list_account().await;
        }

        if let Some(startup_prewarm) = stale_startup_prewarm {
            startup_prewarm.abort().await;
        }

        if permission_profile_changed {
            self.refresh_managed_network_proxy_for_current_permission_profile()
                .await;
        }
        Ok(self
            .new_turn_from_configuration(
                sub_id,
                session_configuration,
                updates.final_output_json_schema,
            )
            .await)
    }

    async fn new_turn_from_configuration(
        &self,
        sub_id: String,
        session_configuration: SessionConfiguration,
        final_output_json_schema: Option<Option<Value>>,
    ) -> Arc<TurnContext> {
        self.new_turn_context_from_configuration(
            sub_id,
            session_configuration,
            final_output_json_schema,
            TurnMultiAgentRuntime::ResolveAndStore,
            self.git_enrichment_policy,
        )
        .await
    }

    async fn new_startup_prewarm_turn_from_configuration(
        &self,
        sub_id: String,
        session_configuration: SessionConfiguration,
    ) -> Arc<TurnContext> {
        self.new_turn_context_from_configuration(
            sub_id,
            session_configuration,
            /*final_output_json_schema*/ None,
            TurnMultiAgentRuntime::Preview,
            GitEnrichmentPolicy::Skip,
        )
        .await
    }

    #[instrument(name = "turn_context.build", level = "trace", skip_all)]
    async fn new_turn_context_from_configuration(
        &self,
        sub_id: String,
        session_configuration: SessionConfiguration,
        final_output_json_schema: Option<Option<Value>>,
        multi_agent_runtime: TurnMultiAgentRuntime,
        git_enrichment_policy: GitEnrichmentPolicy,
    ) -> Arc<TurnContext> {
        let turn_environments = self.services.turn_environments.snapshot().await;
        let primary_turn_environment = turn_environments.primary();
        // TODO(anp): Migrate per-turn config and legacy TurnContext cwd consumers to PathUri so
        // a foreign primary environment does not fall back to the session's host cwd.
        let cwd = primary_turn_environment
            .as_ref()
            .and_then(|turn_environment| turn_environment.cwd().to_abs_path().ok())
            .unwrap_or_else(|| session_configuration.cwd().clone());
        let per_turn_config = Self::build_per_turn_config(&session_configuration, cwd.clone());
        // PF-84 (#572): Bedrock's own metadata after a switch to Bedrock.
        let model_info = crate::model_info_for_provider::model_info_for_config(
            &self.services.models_manager,
            session_configuration.collaboration_mode.model(),
            &per_turn_config,
        )
        .await;
        self.services
            .thread_extension_data
            .insert(model_info.clone());

        let multi_agent_version = match multi_agent_runtime {
            TurnMultiAgentRuntime::ResolveAndStore => {
                self.resolve_multi_agent_version_for_model(&model_info, &per_turn_config)
            }
            TurnMultiAgentRuntime::Preview => per_turn_config.multi_agent_version_for_model(
                self.multi_agent_version()
                    .or(model_info.multi_agent_version),
            ),
        };
        let plugins_input = per_turn_config.plugins_config_input();
        let plugin_outcome = self
            .services
            .plugins_manager
            .plugins_for_config(&plugins_input)
            .await;
        let trusted_plugin_roots = TrustedPluginRoots::from_plugin_load_outcome(
            &plugin_outcome,
            per_turn_config.codex_home.as_path(),
        );
        let effective_skill_roots = plugin_outcome.effective_plugin_skill_roots();
        let plugin_skill_snapshots = self
            .services
            .plugins_manager
            .plugin_skill_snapshots_for_config(&plugins_input);
        let skills_input = skills_load_input_from_config(&per_turn_config, effective_skill_roots)
            .with_plugin_skill_snapshots(plugin_skill_snapshots);
        let fs = primary_turn_environment
            .map(|turn_environment| turn_environment.environment.get_filesystem());
        let skills_snapshot = self
            .services
            .skills_service
            .snapshot_for_config(&skills_input, fs)
            .await;
        let mut turn_context: TurnContext = Self::make_turn_context(
            self.thread_id(),
            self.session_id(),
            Some(self.services.auth_manager()),
            &self.services.session_telemetry,
            session_configuration.provider.clone(),
            &session_configuration,
            multi_agent_version,
            self.services.user_shell.as_ref(),
            self.services.shell_zsh_path.as_ref(),
            self.services.main_execve_wrapper_exe.as_ref(),
            per_turn_config,
            model_info,
            &self.services.models_manager,
            self.services
                .network_proxy
                .load_full()
                .as_ref()
                .and_then(|started_proxy| {
                    Self::managed_network_proxy_active_for_permission_profile(
                        &session_configuration.permission_profile(),
                    )
                    .then(|| started_proxy.proxy())
                }),
            turn_environments,
            cwd,
            sub_id,
            skills_snapshot,
        );
        turn_context.code_mode_available = self.services.code_mode_service.is_available();
        turn_context.extension_data.insert(trusted_plugin_roots);
        turn_context.realtime_active = self.conversation.running_state().await.is_some();

        if let Some(final_schema) = final_output_json_schema {
            turn_context.final_output_json_schema = final_schema;
        }
        let turn_context = Arc::new(turn_context);
        if git_enrichment_policy == GitEnrichmentPolicy::Fresh
            && turn_context
                .environments
                .single_local_environment_cwd()
                .is_some()
        {
            turn_context.turn_metadata_state.spawn_git_enrichment_task();
        }
        turn_context
    }

    pub(crate) async fn maybe_emit_model_warnings_for_turn(&self, tc: &TurnContext) {
        if tc.model_info.used_fallback_model_metadata {
            self.send_event(
                tc,
                EventMsg::Warning(WarningEvent {
                    message: format!(
                        "Model metadata for `{}` not found. Defaulting to fallback metadata; this can degrade performance and cause issues.",
                        tc.model_info.slug
                    ),
                }),
            )
            .await;
        }

        if !tc.code_mode_available
            && matches!(
                crate::tools::requested_tool_mode(tc),
                codex_protocol::openai_models::ToolMode::CodeMode
                    | codex_protocol::openai_models::ToolMode::CodeModeOnly
            )
            && let Some(message) = self
                .services
                .code_mode_service
                .take_unavailable_warning(crate::tools::effective_tool_mode(tc))
        {
            self.send_event(tc, EventMsg::Warning(WarningEvent { message }))
                .await;
        }

        if let Some(message) =
            unsupported_code_mode_warning(&tc.model_info, tc.config.features.get())
        {
            self.send_event(tc, EventMsg::Warning(WarningEvent { message }))
                .await;
        }
    }

    pub(crate) async fn new_default_turn(&self) -> Arc<TurnContext> {
        self.new_default_turn_with_sub_id(self.next_internal_sub_id())
            .await
    }

    pub(crate) async fn new_default_turn_with_sub_id(&self, sub_id: String) -> Arc<TurnContext> {
        let session_configuration = self.default_turn_configuration().await;
        self.new_turn_from_configuration(
            sub_id,
            session_configuration,
            /*final_output_json_schema*/ None,
        )
        .await
    }

    pub(crate) async fn new_startup_prewarm_turn_with_sub_id(
        &self,
        sub_id: String,
    ) -> Arc<TurnContext> {
        let session_configuration = self.default_turn_configuration().await;
        self.new_startup_prewarm_turn_from_configuration(sub_id, session_configuration)
            .await
    }

    async fn default_turn_configuration(&self) -> SessionConfiguration {
        let state = self.state.lock().await;
        state.session_configuration.clone()
    }
}

#[cfg(test)]
#[path = "turn_context_tests.rs"]
mod tests;
