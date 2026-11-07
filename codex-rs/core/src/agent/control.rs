use crate::agent::AgentStatus;
use crate::agent::registry::AgentMetadata;
use crate::agent::registry::AgentRegistry;
use crate::agent::role::agent_nickname_candidates;
use crate::agent::status::is_final;
use crate::agent_communication::AgentCommunicationContext;
use crate::agent_communication::AgentCommunicationKind;
use crate::codex_thread::ThreadConfigSnapshot;
use crate::config::Config;
use crate::config::RolloutBudgetConfig;
use crate::environment_selection::TurnEnvironmentSnapshot;
use crate::rollout_budget::RolloutBudget;
use crate::security::EffectivePolicyInitialization;
use crate::security::EffectivePolicyView;
use crate::security::PersistedHumanSecurityState;
use crate::security::SecurityPolicyError;
use crate::security::TrustedSecurityController;
use crate::session::emit_subagent_session_started;
use crate::session_prefix::format_inter_agent_completion_message;
use crate::session_prefix::format_subagent_context_line;
use crate::session_prefix::format_subagent_notification_message;
use crate::thread_manager::ResumeThreadWithHistoryOptions;
use crate::thread_manager::ThreadManagerState;
use crate::thread_rollout_truncation::truncate_rollout_to_last_n_fork_turns;
use codex_protocol::AgentPath;
use codex_protocol::SessionId;
use codex_protocol::ThreadId;
use codex_protocol::crew::CREW_AUTO_DISPATCH_CHAIN_LIMIT;
use codex_protocol::error::CodexErr;
use codex_protocol::error::CodexErrorDetails;
use codex_protocol::error::Result as CodexResult;
use codex_protocol::models::ContentItem;
use codex_protocol::models::MessagePhase;
use codex_protocol::models::ResponseItem;
use codex_protocol::protocol::AgentMessageKind;
use codex_protocol::protocol::EventMsg;
use codex_protocol::protocol::InitialHistory;
use codex_protocol::protocol::InterAgentCommunication;
use codex_protocol::protocol::MultiAgentVersion;
use codex_protocol::protocol::Op;
use codex_protocol::protocol::ResumedHistory;
use codex_protocol::protocol::RolloutItem;
use codex_protocol::protocol::SessionSource;
use codex_protocol::protocol::SubAgentSource;
use codex_protocol::protocol::ThreadHistoryMode;
use codex_protocol::protocol::ThreadSource;
use codex_protocol::protocol::TurnEnvironmentSelection;
use codex_protocol::user_input::UserInput;
use codex_security_policy::PolicyPrincipal;
use codex_security_policy::PrincipalKind;
#[cfg(test)]
use codex_security_policy::RevocationState;
#[cfg(test)]
use codex_security_policy::SecurityLevel;
use codex_security_policy::SecuritySettings;
use codex_thread_store::LoadThreadHistoryParams;
use codex_thread_store::ReadThreadParams;
use serde::Deserialize;
use serde::Serialize;
use std::collections::HashMap;
use std::collections::VecDeque;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::Weak;
use tokio::sync::watch;
use tracing::warn;

pub(crate) use self::execution::AgentExecutionGuard;
use self::execution::AgentExecutionLimiter;
use self::residency::V2Residency;

const LAST_TASK_MESSAGE_MAX_CHARS: usize = 240;
const LAST_RESULT_MESSAGE_MAX_CHARS: usize = 500;
const ROOT_LAST_TASK_MESSAGE: &str = "Main thread";
mod execution;
mod held_mailbox_warning;
mod legacy;
mod mailbox;
mod residency;
mod spawn;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum SpawnAgentForkMode {
    FullHistory,
    LastNTurns(usize),
}

#[derive(Clone, Debug, Default)]
pub(crate) struct SpawnAgentOptions {
    pub(crate) fork_parent_spawn_call_id: Option<String>,
    pub(crate) fork_mode: Option<SpawnAgentForkMode>,
    pub(crate) parent_thread_id: Option<ThreadId>,
    pub(crate) parent_turn_id: Option<String>,
    pub(crate) environments: Option<Vec<TurnEnvironmentSelection>>,
}

#[derive(Clone, Debug)]
pub(crate) struct LiveAgent {
    pub(crate) thread_id: ThreadId,
    pub(crate) metadata: AgentMetadata,
    pub(crate) status: AgentStatus,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub(crate) struct ListedAgent {
    pub(crate) agent_name: String,
    pub(crate) agent_nickname: Option<String>,
    pub(crate) agent_role: Option<String>,
    pub(crate) agent_status: AgentStatus,
    pub(crate) last_task_message: Option<String>,
    pub(crate) last_result_message: Option<String>,
}

#[derive(Clone, Debug, Default)]
pub(crate) struct NativeAutoLoopState {
    operator_input_seen: bool,
    chain: u32,
    auto_turn_running: bool,
    auto_turn_dispatched: bool,
}

/// Turn-scoped guard that finalizes native loop-breaker state on every exit path, including
/// interruption and provider/tool failure.
pub(crate) struct NativeAutoTurnGuard {
    control: AgentControl,
    agent_id: ThreadId,
}

impl Drop for NativeAutoTurnGuard {
    fn drop(&mut self) {
        self.control.complete_native_agent_turn(self.agent_id);
    }
}

/// Control-plane handle for multi-agent operations.
/// `AgentControl` is held by each session (via `SessionServices`). It provides capability to
/// spawn new agents and the inter-agent communication layer.
/// An `AgentControl` instance is intended to be created at most once per root thread/session
/// tree. That same `AgentControl` is then shared with every sub-agent spawned from that root,
/// which keeps the registry scoped to that root thread rather than the entire `ThreadManager`.
#[derive(Clone, Default)]
pub(crate) struct AgentControl {
    /// ID shared by the whole agent control session. This means every sub-agents from a common
    /// root share the same session ID.
    session_id: SessionId,
    /// Weak handle back to the global thread registry/state.
    /// This is `Weak` to avoid reference cycles and shadow persistence of the form
    /// `ThreadManagerState -> CodexThread -> Session -> SessionServices -> ThreadManagerState`.
    manager: Weak<ThreadManagerState>,
    state: Arc<AgentRegistry>,
    v2_residency: Arc<V2Residency>,
    agent_execution_limiter: Arc<AgentExecutionLimiter>,
    /// Session-scoped state shared by the root thread and every cloned sub-agent control handle.
    rollout_budget: Arc<RolloutBudget>,
    /// Native terminal-result auto-processing policy, shared across the whole agent tree. This is
    /// lifecycle policy over Core turns, not a second mailbox or task queue.
    native_auto_loop_state_by_agent: Arc<Mutex<HashMap<ThreadId, NativeAutoLoopState>>>,
    /// Read-only policy and child-inheritance capability shared by the agent tree.
    security_policy: EffectivePolicyView,
    /// Separate trusted mutation capability retained for the future human TUI
    /// path. No model/tool-facing method exposes it.
    trusted_security_controller: Option<TrustedSecurityController>,
}

impl AgentControl {
    /// Construct a new `AgentControl` that can spawn/message agents via the given manager state.
    pub(crate) fn new(
        manager: Weak<ThreadManagerState>,
        rollout_budget: Option<RolloutBudgetConfig>,
    ) -> Self {
        // Every control handle created by one ThreadManager must observe one lifecycle policy.
        // App-server `/spawn` can legitimately materialize a new control handle while loading a
        // persisted parent/child edge; sourcing the state from the manager prevents those handles
        // from becoming independent loop-breaker islands.
        let native_auto_loop_state_by_agent = manager
            .upgrade()
            .map(|state| Arc::clone(&state.native_auto_loop_state_by_agent))
            .unwrap_or_default();
        let control = Self {
            manager,
            native_auto_loop_state_by_agent,
            ..Default::default()
        };
        if let Some(rollout_budget) = rollout_budget {
            control.rollout_budget.configure(rollout_budget);
        }
        control
    }

    pub(crate) fn with_session_id(mut self, session_id: SessionId, max_threads: usize) -> Self {
        self.session_id = session_id;
        self.agent_execution_limiter.initialize(max_threads);
        self
    }

    pub(crate) fn session_id(&self) -> SessionId {
        self.session_id
    }

    #[cfg(test)]
    pub(crate) fn with_effective_security_policy(
        self,
        level: SecurityLevel,
        root_thread_id: ThreadId,
        inherits_from_spawn_parent: bool,
    ) -> Result<Self, SecurityPolicyError> {
        self.with_recovered_security_policy(
            crate::security::recovery::Recovery {
                level,
                revocations: RevocationState::new(),
                unreadable: None,
                home: None,
            },
            root_thread_id,
            inherits_from_spawn_parent,
        )
    }

    /// PF-23-S03: start from the recovered durable state (the stricter level
    /// and the stored revocations; Aggressive and the kill switch when it was
    /// unreadable).
    pub(crate) fn with_recovered_security_policy(
        mut self,
        recovery: crate::security::recovery::Recovery,
        root_thread_id: ThreadId,
        inherits_from_spawn_parent: bool,
    ) -> Result<Self, SecurityPolicyError> {
        let level = recovery.level;
        let home = recovery.home.clone();
        if self.security_policy.is_initialized()? {
            // Resuming an already-bound root on the same control plane must preserve its
            // binding. Treating it as a new auxiliary agent appends the same principal to its
            // own actor chain, which correctly fails cycle validation but makes root resume
            // impossible.
            if self
                .security_policy
                .snapshot_for_agent(root_thread_id)
                .is_ok()
            {
                return Ok(self);
            }
            if !inherits_from_spawn_parent {
                self.security_policy.inherit_auxiliary_agent(
                    root_thread_id,
                    format!("task:auxiliary:{root_thread_id}"),
                    level,
                )?;
            }
            return Ok(self);
        }
        let human_authority = PolicyPrincipal::new(
            PrincipalKind::Human,
            format!("human:session:{}", self.session_id),
        )?;
        let persisted = PersistedHumanSecurityState::new(
            SecuritySettings::new(level),
            human_authority,
            recovery.revocations,
        )?;
        let controller = TrustedSecurityController::initialize(
            &self.security_policy,
            persisted,
            root_thread_id,
            self.session_id,
            if inherits_from_spawn_parent {
                EffectivePolicyInitialization::DetachedSpawnedAgent
            } else if recovery.unreadable.is_some() {
                EffectivePolicyInitialization::UnreadableState
            } else {
                EffectivePolicyInitialization::Root
            },
        )?;
        if let Some(home) = home {
            // Register, then read again: a commit saved between the first
            // read and the registration is not missed.
            self.security_policy.register_home(&home);
            controller.catch_up(&crate::security::recovery::recover(&home, level));
        }
        self.trusted_security_controller = Some(controller);
        Ok(self)
    }

    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) fn effective_security_policy(&self) -> EffectivePolicyView {
        self.security_policy.clone()
    }

    #[cfg(test)]
    pub(crate) fn trusted_security_controller(&self) -> Option<TrustedSecurityController> {
        self.trusted_security_controller.clone()
    }

    pub(crate) fn rollout_budget(&self) -> &RolloutBudget {
        self.rollout_budget.as_ref()
    }

    fn native_auto_loop_states(
        &self,
    ) -> std::sync::MutexGuard<'_, HashMap<ThreadId, NativeAutoLoopState>> {
        self.native_auto_loop_state_by_agent
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Starts lifecycle accounting for one native agent turn.
    ///
    /// A turn containing only terminal-result mailbox input is automatic. Any user assignment,
    /// follow-up, or other non-terminal task is fresh work and resets a previously paused chain.
    pub(crate) fn begin_native_agent_turn(
        &self,
        agent_id: ThreadId,
        terminal_result_only: bool,
    ) -> NativeAutoTurnGuard {
        let mut states = self.native_auto_loop_states();
        let state = states.entry(agent_id).or_default();
        if terminal_result_only {
            state.auto_turn_running = state.operator_input_seen;
            state.auto_turn_dispatched = false;
        } else {
            state.operator_input_seen = true;
            state.chain = 0;
            state.auto_turn_running = false;
            state.auto_turn_dispatched = false;
        }
        drop(states);
        NativeAutoTurnGuard {
            control: self.clone(),
            agent_id,
        }
    }

    /// Explicit human steering resets a native auto-processing chain even when it joins an
    /// already-running terminal-result turn.
    pub(crate) fn note_native_operator_input(&self, agent_id: ThreadId) {
        let mut states = self.native_auto_loop_states();
        let state = states.entry(agent_id).or_default();
        state.operator_input_seen = true;
        state.chain = 0;
        state.auto_turn_running = false;
        state.auto_turn_dispatched = false;
    }

    /// Records the first successful outbound collaboration action from an automatic manager turn.
    /// Multiple sends in the same turn count as one link in the chain.
    pub(crate) fn note_native_agent_dispatch(&self, agent_id: ThreadId) {
        let mut states = self.native_auto_loop_states();
        let state = states.entry(agent_id).or_default();
        if state.auto_turn_running && !state.auto_turn_dispatched {
            state.auto_turn_dispatched = true;
            state.chain = state.chain.saturating_add(1);
        }
    }

    fn complete_native_agent_turn(&self, agent_id: ThreadId) {
        let mut states = self.native_auto_loop_states();
        let state = states.entry(agent_id).or_default();
        if state.auto_turn_running && !state.auto_turn_dispatched {
            // Acknowledging a child result without creating more work terminates the chain.
            state.chain = 0;
        }
        state.auto_turn_running = false;
        state.auto_turn_dispatched = false;
    }

    fn native_terminal_result_auto_trigger_allowed(&self, agent_id: ThreadId) -> bool {
        let states = self.native_auto_loop_states();
        states.get(&agent_id).is_some_and(|state| {
            state.operator_input_seen && state.chain < CREW_AUTO_DISPATCH_CHAIN_LIMIT
        })
    }

    #[cfg(test)]
    pub(crate) fn native_auto_loop_state_for_test(
        &self,
        agent_id: ThreadId,
    ) -> Option<(bool, u32, bool, bool)> {
        self.native_auto_loop_states().get(&agent_id).map(|state| {
            (
                state.operator_input_seen,
                state.chain,
                state.auto_turn_running,
                state.auto_turn_dispatched,
            )
        })
    }

    /// Send rich user input items from the agent `sender` to an existing agent thread.
    pub(crate) async fn send_input(
        &self,
        agent_id: ThreadId,
        sender: Option<ThreadId>,
        input: Vec<UserInput>,
        parent_turn_id: Option<String>,
    ) -> CodexResult<String> {
        let state = self.upgrade()?;
        self.ensure_execution_capacity_for_turn_start(agent_id, /*starts_turn*/ true)
            .await?;
        self.send_input_after_capacity_check(agent_id, sender, &state, input, parent_turn_id)
            .await
    }

    async fn send_input_after_capacity_check(
        &self,
        agent_id: ThreadId,
        sender: Option<ThreadId>,
        state: &Arc<ThreadManagerState>,
        input: Vec<UserInput>,
        parent_turn_id: Option<String>,
    ) -> CodexResult<String> {
        Box::pin(register_agent_input_origin(state, agent_id, sender, &input)).await;
        self.handle_thread_request_result(
            agent_id,
            state,
            state.send_op(agent_id, input.into(), parent_turn_id).await,
        )
        .await
    }

    pub(crate) async fn send_inter_agent_communication(
        &self,
        agent_id: ThreadId,
        communication: InterAgentCommunication,
        agent_communication_context: AgentCommunicationContext,
        parent_turn_id: Option<String>,
    ) -> CodexResult<String> {
        let state = self.upgrade()?;
        self.ensure_execution_capacity_for_turn_start(agent_id, communication.trigger_turn)
            .await?;
        self.send_inter_agent_communication_after_capacity_check(
            agent_id,
            &state,
            communication,
            agent_communication_context,
            parent_turn_id,
        )
        .await
    }

    async fn send_inter_agent_communication_after_capacity_check(
        &self,
        agent_id: ThreadId,
        _state: &Arc<ThreadManagerState>,
        communication: InterAgentCommunication,
        context: AgentCommunicationContext,
        parent_turn_id: Option<String>,
    ) -> CodexResult<String> {
        let communication_for_log =
            crate::agent_communication::logging_enabled().then(|| communication.clone());
        let result = self
            .send_persisted_inter_agent_communication(agent_id, communication, parent_turn_id)
            .await;
        if let (Some(communication), Ok(communication_id)) =
            (communication_for_log, result.as_ref())
        {
            crate::agent_communication::emit_agent_communication_send(
                communication_id,
                &context,
                &communication,
                agent_id,
            );
        }
        result
    }

    /// Interrupt the current task for an existing agent thread.
    pub(crate) async fn interrupt_agent(&self, agent_id: ThreadId) -> CodexResult<String> {
        let state = self.upgrade()?;
        self.handle_thread_request_result(
            agent_id,
            &state,
            state
                .send_op(agent_id, Op::Interrupt, /*parent_turn_id*/ None)
                .await,
        )
        .await
    }

    async fn handle_thread_request_result(
        &self,
        agent_id: ThreadId,
        state: &Arc<ThreadManagerState>,
        result: CodexResult<String>,
    ) -> CodexResult<String> {
        if result
            .as_ref()
            .is_err_and(|err| matches!(err.details(), CodexErrorDetails::InternalAgentDied))
        {
            let _ = state.remove_thread(&agent_id).await;
            self.forget_v2_residency(agent_id);
            self.state.release_spawned_thread(agent_id);
        }
        result
    }

    /// Fetch the last known status for `agent_id`, returning `NotFound` when unavailable.
    pub(crate) async fn get_status(&self, agent_id: ThreadId) -> AgentStatus {
        let Ok(state) = self.upgrade() else {
            // No agent available if upgrade fails.
            return AgentStatus::NotFound;
        };
        let Ok(thread) = state.get_thread(agent_id).await else {
            return if self.state.agent_metadata_for_thread(agent_id).is_some() {
                AgentStatus::Unloaded
            } else {
                AgentStatus::NotFound
            };
        };
        thread.agent_status().await
    }

    pub(crate) fn register_session_root(
        &self,
        current_thread_id: ThreadId,
        current_parent_thread_id: Option<ThreadId>,
    ) {
        if current_parent_thread_id.is_none() {
            self.state.register_root_thread(current_thread_id);
        }
    }

    pub(crate) fn get_agent_metadata(&self, agent_id: ThreadId) -> Option<AgentMetadata> {
        self.state.agent_metadata_for_thread(agent_id)
    }

    pub(crate) fn get_agent_metadata_for_path(
        &self,
        agent_path: &AgentPath,
    ) -> Option<AgentMetadata> {
        self.state.agent_metadata_for_path(agent_path)
    }

    pub(crate) fn is_root_thread(&self, agent_id: ThreadId) -> bool {
        self.state.agent_id_for_path(&AgentPath::root()) == Some(agent_id)
    }

    /// Whether `thread_id` is loaded and is the root of its own tree: a thread a client
    /// started (such as a pane the person opened), not a spawned or internal agent.
    pub(crate) async fn is_loaded_root_thread(&self, thread_id: ThreadId) -> bool {
        let Ok(state) = self.upgrade() else {
            return false;
        };
        state
            .get_thread(thread_id)
            .await
            .is_ok_and(|thread| !thread.session_source.is_non_root_agent())
    }

    pub(crate) fn ensure_agent_known(&self, agent_id: ThreadId) -> CodexResult<AgentMetadata> {
        self.state
            .agent_metadata_for_thread(agent_id)
            .ok_or_else(|| CodexErr::ThreadNotFound(agent_id))
    }

    /// Whether native terminal mailbox items should wake this thread automatically.
    ///
    /// Persistent crew managers process child results without a TUI-side report queue.
    /// Human-facing roots and ephemeral task agents retain ordinary Codex behavior.
    pub(crate) async fn auto_processes_terminal_results(&self, agent_id: ThreadId) -> bool {
        let Ok(state) = self.upgrade() else {
            return false;
        };
        let Ok(thread) = state.get_thread(agent_id).await else {
            return false;
        };
        let is_persistent_crew_member = matches!(
            thread.session_source.get_agent_class(),
            Some(codex_protocol::crew::AgentClass::CrewMember { .. })
        );
        is_persistent_crew_member && self.native_terminal_result_auto_trigger_allowed(agent_id)
    }

    pub(crate) fn record_agent_result_status(
        &self,
        agent_id: ThreadId,
        status: &AgentStatus,
    ) -> Option<String> {
        let result_message = result_message_from_status(status);
        match result_message.as_ref() {
            Some(message) => self
                .state
                .update_last_result_message(agent_id, message.clone()),
            None => self.state.clear_last_result_message(agent_id),
        }
        result_message
    }

    pub(crate) fn register_thread_spawn_metadata(
        &self,
        thread_id: ThreadId,
        session_source: &SessionSource,
    ) {
        let SessionSource::SubAgent(SubAgentSource::ThreadSpawn {
            agent_path,
            agent_nickname,
            agent_role,
            ..
        }) = session_source
        else {
            return;
        };
        self.state.register_spawned_thread(AgentMetadata {
            agent_id: Some(thread_id),
            agent_path: agent_path.clone(),
            agent_nickname: agent_nickname.clone(),
            agent_role: agent_role.clone(),
            last_task_message: None,
            last_result_message: None,
        });
    }

    pub(crate) fn restore_thread_spawn_metadata(
        &self,
        thread_id: ThreadId,
        session_source: &SessionSource,
    ) {
        let SessionSource::SubAgent(SubAgentSource::ThreadSpawn {
            agent_path,
            agent_nickname,
            agent_role,
            ..
        }) = session_source
        else {
            return;
        };
        self.state.restore_spawned_thread(AgentMetadata {
            agent_id: Some(thread_id),
            agent_path: agent_path.clone(),
            agent_nickname: agent_nickname.clone(),
            agent_role: agent_role.clone(),
            last_task_message: None,
            last_result_message: None,
        });
        self.ensure_thread_security_inheritance(thread_id, session_source);
    }

    fn ensure_thread_security_inheritance(
        &self,
        thread_id: ThreadId,
        session_source: &SessionSource,
    ) {
        if self.security_policy.snapshot_for_agent(thread_id).is_ok() {
            return;
        }
        let SessionSource::SubAgent(SubAgentSource::ThreadSpawn {
            parent_thread_id, ..
        }) = session_source
        else {
            return;
        };
        let Ok(parent) = self.security_policy.snapshot_for_agent(*parent_thread_id) else {
            warn!(
                %thread_id,
                %parent_thread_id,
                "cannot restore child security binding without its parent binding"
            );
            return;
        };
        if let Err(error) = self.security_policy.inherit_child(
            *parent_thread_id,
            thread_id,
            format!("task:restore:{thread_id}"),
            parent.level,
        ) {
            warn!(%thread_id, %error, "failed to restore child security binding");
        }
    }

    pub(crate) async fn restore_persisted_agent_subtree(
        &self,
        root_thread_id: ThreadId,
    ) -> CodexResult<()> {
        let state = self.upgrade()?;
        let Some(agent_graph_store) = state.agent_graph_store() else {
            return Ok(());
        };
        let descendant_ids = match agent_graph_store
            .list_thread_spawn_descendants(
                root_thread_id,
                Some(codex_agent_graph_store::ThreadSpawnEdgeStatus::Open),
            )
            .await
        {
            Ok(descendant_ids) => descendant_ids,
            Err(err) => {
                warn!(
                    %root_thread_id,
                    %err,
                    "failed to enumerate persisted agent subtree during resume"
                );
                return Ok(());
            }
        };
        for thread_id in descendant_ids {
            let stored_thread = match state
                .read_stored_thread(ReadThreadParams {
                    thread_id,
                    include_archived: true,
                    include_history: false,
                })
                .await
            {
                Ok(stored_thread) => stored_thread,
                Err(err) => {
                    warn!(
                        %root_thread_id,
                        %thread_id,
                        %err,
                        "failed to restore one persisted agent during resume"
                    );
                    continue;
                }
            };
            self.restore_thread_spawn_metadata(thread_id, &stored_thread.source);
        }
        Ok(())
    }

    pub(crate) async fn list_live_agent_subtree_thread_ids(
        &self,
        agent_id: ThreadId,
    ) -> CodexResult<Vec<ThreadId>> {
        let mut thread_ids = vec![agent_id];
        thread_ids.extend(self.live_thread_spawn_descendants(agent_id).await?);
        Ok(thread_ids)
    }

    pub(crate) async fn get_agent_config_snapshot(
        &self,
        agent_id: ThreadId,
    ) -> Option<ThreadConfigSnapshot> {
        let Ok(state) = self.upgrade() else {
            return None;
        };
        let Ok(thread) = state.get_thread(agent_id).await else {
            return None;
        };
        Some(thread.config_snapshot().await)
    }

    /// The live session of `thread_id`, if this thread manager holds it. Used
    /// to attribute paid work done for a conversation that is not persisted
    /// itself (PF-60-S06).
    pub(crate) async fn live_session(
        &self,
        thread_id: ThreadId,
    ) -> Option<Arc<crate::session::session::Session>> {
        let state = self.upgrade().ok()?;
        let thread = state.get_thread(thread_id).await.ok()?;
        Some(Arc::clone(&thread.session))
    }

    pub(crate) async fn resolve_agent_reference(
        &self,
        _current_thread_id: ThreadId,
        current_session_source: &SessionSource,
        agent_reference: &str,
    ) -> CodexResult<ThreadId> {
        let agent_reference = agent_reference.trim();
        if let Ok(thread_id) = ThreadId::from_string(agent_reference)
            && self.state.agent_metadata_for_thread(thread_id).is_some()
        {
            return Ok(thread_id);
        }
        if let Some(thread_id) = self.state.agent_id_for_nickname(agent_reference) {
            return Ok(thread_id);
        }
        if let Some(agent_nickname) = nickname_from_picker_label(agent_reference)
            && let Some(thread_id) = self.state.agent_id_for_nickname(agent_nickname)
        {
            return Ok(thread_id);
        }
        let current_agent_path = current_session_source
            .get_agent_path()
            .unwrap_or_else(AgentPath::root);
        let agent_path = current_agent_path
            .resolve(agent_reference)
            .map_err(CodexErr::UnsupportedOperation)?;
        if let Some(thread_id) = self.state.agent_id_for_path(&agent_path) {
            return Ok(thread_id);
        }
        Err(CodexErr::UnsupportedOperation(format!(
            "live agent path `{}` not found",
            agent_path.as_str()
        )))
    }

    /// Subscribe to status updates for `agent_id`, yielding the latest value and changes.
    pub(crate) async fn subscribe_status(
        &self,
        agent_id: ThreadId,
    ) -> CodexResult<watch::Receiver<AgentStatus>> {
        let state = self.upgrade()?;
        let thread = state.get_thread(agent_id).await?;
        Ok(thread.subscribe_status())
    }

    pub(crate) async fn format_environment_context_subagents(
        &self,
        parent_thread_id: ThreadId,
    ) -> String {
        let Ok(agents) = self.open_thread_spawn_children(parent_thread_id).await else {
            return String::new();
        };

        let state = self.upgrade().ok();
        let mut lines = Vec::new();
        for (thread_id, metadata) in agents {
            let status = match state.as_ref() {
                Some(state) => match state.get_thread(thread_id).await {
                    Ok(thread) => Some(thread.agent_status().await),
                    Err(_) => None,
                },
                None => None,
            };
            let last_result_message = metadata
                .last_result_message
                .clone()
                .or_else(|| status.as_ref().and_then(result_message_from_status));
            let reference = metadata
                .agent_path
                .as_ref()
                .map(|agent_path| agent_path.name().to_string())
                .unwrap_or_else(|| thread_id.to_string());
            lines.push(format_subagent_context_line(
                reference.as_str(),
                metadata.agent_nickname.as_deref(),
                metadata.agent_role.as_deref(),
                status.as_ref(),
                metadata.last_task_message.as_deref(),
                last_result_message.as_deref(),
            ));
        }

        lines
            .into_iter()
            .filter(|line| !line.is_empty())
            .collect::<Vec<_>>()
            .join("\n")
    }

    pub(crate) async fn list_agents(
        &self,
        current_session_source: &SessionSource,
        path_prefix: Option<&str>,
    ) -> CodexResult<Vec<ListedAgent>> {
        let state = self.upgrade()?;
        let resolved_prefix = path_prefix
            .map(|prefix| {
                current_session_source
                    .get_agent_path()
                    .unwrap_or_else(AgentPath::root)
                    .resolve(prefix)
                    .map_err(CodexErr::UnsupportedOperation)
            })
            .transpose()?;

        let mut live_agents = self.state.live_agents();
        live_agents.sort_by(|left, right| {
            left.agent_path
                .as_deref()
                .unwrap_or_default()
                .cmp(right.agent_path.as_deref().unwrap_or_default())
                .then_with(|| {
                    left.agent_id
                        .map(|id| id.to_string())
                        .unwrap_or_default()
                        .cmp(&right.agent_id.map(|id| id.to_string()).unwrap_or_default())
                })
        });

        let root_path = AgentPath::root();
        let mut agents = Vec::with_capacity(live_agents.len().saturating_add(1));
        if resolved_prefix
            .as_ref()
            .is_none_or(|prefix| agent_matches_prefix(Some(&root_path), prefix))
            && let Some(root_thread_id) = self.state.agent_id_for_path(&root_path)
            && let Ok(root_thread) = state.get_thread(root_thread_id).await
        {
            agents.push(ListedAgent {
                agent_name: root_path.to_string(),
                agent_nickname: None,
                agent_role: None,
                agent_status: root_thread.agent_status().await,
                last_task_message: Some(ROOT_LAST_TASK_MESSAGE.to_string()),
                last_result_message: None,
            });
        }

        for metadata in live_agents {
            let Some(thread_id) = metadata.agent_id else {
                continue;
            };
            if resolved_prefix
                .as_ref()
                .is_some_and(|prefix| !agent_matches_prefix(metadata.agent_path.as_ref(), prefix))
            {
                continue;
            }

            let agent_name = metadata
                .agent_path
                .as_ref()
                .map(ToString::to_string)
                .unwrap_or_else(|| thread_id.to_string());
            let agent_status = match state.get_thread(thread_id).await {
                Ok(thread) => thread.agent_status().await,
                Err(_) => AgentStatus::Unloaded,
            };
            let last_result_message = metadata
                .last_result_message
                .clone()
                .or_else(|| result_message_from_status(&agent_status));
            agents.push(ListedAgent {
                agent_name,
                agent_nickname: metadata.agent_nickname.clone(),
                agent_role: metadata.agent_role.clone(),
                agent_status,
                last_task_message: metadata.last_task_message.clone(),
                last_result_message,
            });
        }

        Ok(agents)
    }

    /// Starts a detached watcher for sub-agents spawned from another thread.
    ///
    /// This is only enabled for `SubAgentSource::ThreadSpawn`, where a parent thread exists and
    /// can receive completion notifications.
    fn maybe_start_completion_watcher(
        &self,
        child_thread_id: ThreadId,
        session_source: Option<SessionSource>,
        child_reference: String,
        child_agent_path: Option<AgentPath>,
    ) {
        let Some(SessionSource::SubAgent(SubAgentSource::ThreadSpawn {
            parent_thread_id, ..
        })) = session_source
        else {
            return;
        };
        let control = self.clone();
        tokio::spawn(async move {
            let status = match control.subscribe_status(child_thread_id).await {
                Ok(mut status_rx) => {
                    let mut status = status_rx.borrow().clone();
                    while !is_final(&status) {
                        if status_rx.changed().await.is_err() {
                            status = control.get_status(child_thread_id).await;
                            break;
                        }
                        status = status_rx.borrow().clone();
                    }
                    status
                }
                Err(_) => control.get_status(child_thread_id).await,
            };
            if !is_final(&status) {
                return;
            }
            control.record_agent_result_status(child_thread_id, &status);

            let Ok(state) = control.upgrade() else {
                return;
            };
            let child_thread = state.get_thread(child_thread_id).await.ok();
            let child_uses_multi_agent_v2 = match child_thread.as_ref() {
                Some(child_thread) => {
                    child_thread.multi_agent_version() == Some(MultiAgentVersion::V2)
                }
                None => true,
            };
            if child_agent_path.is_some() && child_uses_multi_agent_v2 {
                let Some(child_agent_path) = child_agent_path.clone() else {
                    return;
                };
                let Some(parent_agent_path) = child_agent_path
                    .as_str()
                    .rsplit_once('/')
                    .and_then(|(parent, _)| AgentPath::try_from(parent).ok())
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
                let communication = InterAgentCommunication::new(
                    child_agent_path,
                    parent_agent_path,
                    Vec::new(),
                    message,
                    /*trigger_turn*/ false,
                )
                .with_kind(AgentMessageKind::TerminalResult);
                let context =
                    AgentCommunicationContext::new(AgentCommunicationKind::Result, child_thread_id);
                let _ = control
                    .send_inter_agent_communication(
                        parent_thread_id,
                        communication,
                        context,
                        /*parent_turn_id*/ None,
                    )
                    .await;
                return;
            }
            let message = format_subagent_notification_message(child_reference.as_str(), &status);
            let Ok(parent_thread) = state.get_thread(parent_thread_id).await else {
                return;
            };
            parent_thread
                .inject_agent_result_without_turn(message)
                .await;
        });
    }

    fn prepare_agent_metadata(
        &self,
        reservation: &mut crate::agent::registry::SpawnReservation,
        config: &Config,
        agent_path: Option<AgentPath>,
        agent_role: Option<String>,
        preferred_agent_nickname: Option<String>,
    ) -> CodexResult<AgentMetadata> {
        if let Some(agent_path) = agent_path.as_ref() {
            reservation.reserve_agent_path(agent_path)?;
        }
        let candidate_names = agent_nickname_candidates(config, agent_role.as_deref());
        let candidate_name_refs: Vec<&str> = candidate_names.iter().map(String::as_str).collect();
        let agent_nickname = Some(reservation.reserve_agent_nickname_with_preference(
            &candidate_name_refs,
            preferred_agent_nickname.as_deref(),
        )?);
        Ok(AgentMetadata {
            agent_id: None,
            agent_path,
            agent_nickname,
            agent_role,
            ..Default::default()
        })
    }

    #[allow(clippy::too_many_arguments)]
    fn prepare_thread_spawn(
        &self,
        reservation: &mut crate::agent::registry::SpawnReservation,
        config: &Config,
        parent_thread_id: ThreadId,
        depth: i32,
        agent_path: Option<AgentPath>,
        agent_role: Option<String>,
        preferred_agent_nickname: Option<String>,
        agent_class: Option<codex_protocol::crew::AgentClass>,
    ) -> CodexResult<(SessionSource, AgentMetadata)> {
        if depth == 1 {
            self.state.register_root_thread(parent_thread_id);
        }
        let agent_metadata = self.prepare_agent_metadata(
            reservation,
            config,
            agent_path,
            agent_role,
            preferred_agent_nickname,
        )?;
        let session_source = SessionSource::SubAgent(SubAgentSource::ThreadSpawn {
            parent_thread_id,
            depth,
            agent_path: agent_metadata.agent_path.clone(),
            agent_nickname: agent_metadata.agent_nickname.clone(),
            agent_role: agent_metadata.agent_role.clone(),
            agent_class,
        });
        Ok((session_source, agent_metadata))
    }

    fn upgrade(&self) -> CodexResult<Arc<ThreadManagerState>> {
        self.manager
            .upgrade()
            .ok_or_else(|| CodexErr::UnsupportedOperation("thread manager dropped".to_string()))
    }

    async fn inherited_environments_for_source(
        &self,
        state: &Arc<ThreadManagerState>,
        session_source: Option<&SessionSource>,
    ) -> Option<TurnEnvironmentSnapshot> {
        let Some(SessionSource::SubAgent(SubAgentSource::ThreadSpawn {
            parent_thread_id, ..
        })) = session_source
        else {
            return None;
        };

        let parent_thread = state.get_thread(*parent_thread_id).await.ok()?;
        Some(
            parent_thread
                .session
                .services
                .turn_environments
                .snapshot()
                .await,
        )
    }

    async fn inherited_exec_policy_for_source(
        &self,
        state: &Arc<ThreadManagerState>,
        session_source: Option<&SessionSource>,
        child_config: &Config,
    ) -> Option<Arc<crate::exec_policy::ExecPolicyManager>> {
        let Some(SessionSource::SubAgent(SubAgentSource::ThreadSpawn {
            parent_thread_id, ..
        })) = session_source
        else {
            return None;
        };

        let parent_thread = state.get_thread(*parent_thread_id).await.ok()?;
        let parent_config = parent_thread.session.get_config().await;
        if !crate::exec_policy::child_uses_parent_exec_policy(&parent_config, child_config) {
            return None;
        }

        Some(Arc::clone(&parent_thread.session.services.exec_policy))
    }

    async fn open_thread_spawn_children(
        &self,
        parent_thread_id: ThreadId,
    ) -> CodexResult<Vec<(ThreadId, AgentMetadata)>> {
        let mut children_by_parent = self.live_thread_spawn_children().await?;
        Ok(children_by_parent
            .remove(&parent_thread_id)
            .unwrap_or_default())
    }

    async fn live_thread_spawn_children(
        &self,
    ) -> CodexResult<HashMap<ThreadId, Vec<(ThreadId, AgentMetadata)>>> {
        let state = self.upgrade()?;
        let mut children_by_parent = HashMap::<ThreadId, Vec<(ThreadId, AgentMetadata)>>::new();

        for (parent_thread_id, child_thread_id) in state.list_live_thread_spawn_edges().await {
            let metadata = match self.state.agent_metadata_for_thread(child_thread_id) {
                Some(metadata) => metadata,
                None => match state.get_thread(child_thread_id).await {
                    Ok(child_thread) => AgentMetadata {
                        agent_id: Some(child_thread_id),
                        agent_path: child_thread.session_source.get_agent_path(),
                        agent_nickname: child_thread.session_source.get_nickname(),
                        agent_role: child_thread.session_source.get_agent_role(),
                        last_task_message: None,
                        last_result_message: None,
                    },
                    Err(_) => AgentMetadata {
                        agent_id: Some(child_thread_id),
                        ..Default::default()
                    },
                },
            };
            children_by_parent
                .entry(parent_thread_id)
                .or_default()
                .push((child_thread_id, metadata));
        }

        for children in children_by_parent.values_mut() {
            children.sort_by(|left, right| {
                left.1
                    .agent_path
                    .as_deref()
                    .unwrap_or_default()
                    .cmp(right.1.agent_path.as_deref().unwrap_or_default())
                    .then_with(|| left.0.to_string().cmp(&right.0.to_string()))
            });
        }

        Ok(children_by_parent)
    }

    /// Establishes the durable parent side of a native spawn before a child can be created.
    ///
    /// A lazily started human root has no resumable rollout until its first material write. A
    /// durable child must never be committed below that transient root: the graph edge would
    /// survive while the parent needed to reconstruct and address it would not.
    pub(crate) async fn prepare_durable_thread_spawn_parent(
        &self,
        session_source: &SessionSource,
        child_is_ephemeral: bool,
    ) -> CodexResult<()> {
        if child_is_ephemeral {
            return Ok(());
        }
        let Some(parent_thread_id) = session_source.parent_thread_id() else {
            return Ok(());
        };
        let state = self.upgrade()?;
        let parent_thread = state.get_thread(parent_thread_id).await?;
        parent_thread
            .session
            .try_ensure_rollout_materialized()
            .await
            .map_err(|err| {
                CodexErr::Fatal(format!(
                    "failed to materialize native agent parent {parent_thread_id}: {err}"
                ))
            })?;
        parent_thread.flush_rollout().await.map_err(|err| {
            CodexErr::Fatal(format!(
                "failed to flush native agent parent {parent_thread_id}: {err}"
            ))
        })
    }

    /// Persists the child before publishing its durable Core graph edge.
    pub(crate) async fn persist_durable_thread_spawn(
        &self,
        child_thread: &crate::CodexThread,
        child_thread_id: ThreadId,
        session_source: Option<&SessionSource>,
    ) -> CodexResult<()> {
        if child_thread.config_snapshot().await.ephemeral {
            return Ok(());
        }
        if session_source
            .and_then(SessionSource::parent_thread_id)
            .is_none()
        {
            return Ok(());
        }
        child_thread
            .session
            .try_ensure_rollout_materialized()
            .await
            .map_err(|err| {
                CodexErr::Fatal(format!(
                    "failed to materialize native agent child {child_thread_id}: {err}"
                ))
            })?;
        child_thread.flush_rollout().await.map_err(|err| {
            CodexErr::Fatal(format!(
                "failed to flush native agent child {child_thread_id}: {err}"
            ))
        })?;
        self.persist_thread_spawn_edge_for_source(child_thread, child_thread_id, session_source)
            .await
    }

    async fn persist_thread_spawn_edge_for_source(
        &self,
        child_thread: &crate::CodexThread,
        child_thread_id: ThreadId,
        session_source: Option<&SessionSource>,
    ) -> CodexResult<()> {
        let Some(parent_thread_id) = session_source.and_then(SessionSource::parent_thread_id)
        else {
            return Ok(());
        };
        if child_thread.config_snapshot().await.ephemeral {
            return Ok(());
        }
        let state = self.upgrade()?;
        let Some(agent_graph_store) = state.agent_graph_store() else {
            return Ok(());
        };
        agent_graph_store
            .upsert_thread_spawn_edge(
                parent_thread_id,
                child_thread_id,
                codex_agent_graph_store::ThreadSpawnEdgeStatus::Open,
            )
            .await
            .map_err(|err| {
                CodexErr::Fatal(format!(
                    "failed to persist native agent edge {parent_thread_id} -> {child_thread_id}: {err}"
                ))
            })
    }

    async fn live_thread_spawn_descendants(
        &self,
        root_thread_id: ThreadId,
    ) -> CodexResult<Vec<ThreadId>> {
        let mut children_by_parent = self.live_thread_spawn_children().await?;
        let mut descendants = Vec::new();
        let mut stack = children_by_parent
            .remove(&root_thread_id)
            .unwrap_or_default()
            .into_iter()
            .map(|(child_thread_id, _)| child_thread_id)
            .rev()
            .collect::<Vec<_>>();

        while let Some(thread_id) = stack.pop() {
            descendants.push(thread_id);
            if let Some(children) = children_by_parent.remove(&thread_id) {
                for (child_thread_id, _) in children.into_iter().rev() {
                    stack.push(child_thread_id);
                }
            }
        }

        Ok(descendants)
    }
}

fn agent_matches_prefix(agent_path: Option<&AgentPath>, prefix: &AgentPath) -> bool {
    if prefix.is_root() {
        return true;
    }

    agent_path.is_some_and(|agent_path| {
        agent_path == prefix
            || agent_path
                .as_str()
                .strip_prefix(prefix.as_str())
                .is_some_and(|suffix| suffix.starts_with('/'))
    })
}

pub(crate) fn render_input_preview(input: &[UserInput]) -> String {
    input
        .iter()
        .map(|item| match item {
            UserInput::Text { text, .. } => text.clone(),
            UserInput::Image { .. } => "[image]".to_string(),
            UserInput::LocalImage { path, .. } => {
                format!("[local_image:{}]", path.display())
            }
            UserInput::Audio { .. } => "[audio]".to_string(),
            UserInput::LocalAudio { path } => {
                format!("[local_audio:{}]", path.display())
            }
            UserInput::Skill { name, path, .. } => {
                format!("[skill:${name}]({})", path.display())
            }
            UserInput::Mention { name, path, .. } => format!("[mention:${name}]({path})"),
            _ => "[input]".to_string(),
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn thread_spawn_depth(session_source: &SessionSource) -> Option<i32> {
    match session_source {
        SessionSource::SubAgent(SubAgentSource::ThreadSpawn { depth, .. }) => Some(*depth),
        _ => None,
    }
}

fn last_task_message_from_communication(communication: &InterAgentCommunication) -> Option<String> {
    if communication.encrypted_content.is_some() {
        return None;
    }
    non_empty_bounded_message(communication.content.clone(), LAST_TASK_MESSAGE_MAX_CHARS)
}

fn result_message_from_status(status: &AgentStatus) -> Option<String> {
    match status {
        AgentStatus::Completed(Some(message)) => {
            non_empty_bounded_message(message.clone(), LAST_RESULT_MESSAGE_MAX_CHARS)
        }
        AgentStatus::Completed(None) => None,
        AgentStatus::Errored(error) => non_empty_bounded_message(
            format!("Agent errored: {error}"),
            LAST_RESULT_MESSAGE_MAX_CHARS,
        ),
        AgentStatus::Shutdown => Some("Agent shut down.".to_string()),
        AgentStatus::NotFound => Some("Agent was not found.".to_string()),
        AgentStatus::PendingInit
        | AgentStatus::Unloaded
        | AgentStatus::Running
        | AgentStatus::Interrupted => None,
    }
}

fn nickname_from_picker_label(agent_reference: &str) -> Option<&str> {
    let agent_reference = agent_reference.trim();
    let (nickname, role_suffix) = agent_reference.rsplit_once(" [")?;
    if nickname.trim().is_empty() || !role_suffix.ends_with(']') {
        return None;
    }
    Some(nickname.trim())
}

fn non_empty_bounded_message(message: String, max_chars: usize) -> Option<String> {
    let message = message.trim();
    if message.is_empty() {
        return None;
    }
    let mut chars = message.chars();
    let preview = chars.by_ref().take(max_chars).collect::<String>();
    if chars.next().is_some() {
        Some(format!("{preview}..."))
    } else {
        Some(preview)
    }
}
/// PF-30-S02 lineage. Text one agent hands another arrives as `Op::UserInput`,
/// which the receiver would otherwise record with human standing. Mark the
/// exact input on the receiver before submitting it; its prompt seam then
/// records it with the sender's standing: host only while every item in the
/// sender's history had standing, otherwise agent data. An unknown sender
/// counts as agent data. Only the receiver's `source_envelopes` mode records
/// anything; otherwise a no-op.
async fn register_agent_input_origin(
    state: &Arc<ThreadManagerState>,
    agent_id: ThreadId,
    sender: Option<ThreadId>,
    input: &[UserInput],
) {
    use crate::security::ingress::MessageOrigin;
    let Ok(receiver) = state.get_thread(agent_id).await else {
        return;
    };
    let client = receiver.session.services.model_client();
    if !client.source_envelopes_enabled() {
        return;
    }
    let origin = match sender {
        Some(sender) => match state.get_thread(sender).await {
            Ok(sender) => sender.session.agent_handoff_origin().await,
            Err(_) => MessageOrigin::External(codex_protocol::provenance::SourceKind::ChildAgent),
        },
        None => MessageOrigin::External(codex_protocol::provenance::SourceKind::ChildAgent),
    };
    client.mark_agent_input(input, origin);
}

#[cfg(test)]
#[path = "control_tests.rs"]
mod tests;
