use super::residency::is_v2_resident_session_source;
use super::*;
use crate::agent::role::apply_role_to_config_for_multi_agent_v2;
use crate::config::PermissionProfileSnapshot;
use codex_extension_api::ExtensionDataInit;

struct SpawnAgentThreadInheritance {
    environments: Option<TurnEnvironmentSnapshot>,
    exec_policy: Option<Arc<crate::exec_policy::ExecPolicyManager>>,
}

/// Initial input delivered after a spawned agent acquires execution capacity.
///
/// V2 communication spawns keep the communication and its context paired so centralized
/// submission and lifecycle logging cannot receive one without the other. Other spawn sources
/// provide user input directly, making an uncontextualized inter-agent communication
/// unrepresentable.
#[allow(
    clippy::large_enum_variant,
    reason = "the value is short-lived and boxing would complicate the spawn handoff boundary"
)]
enum SpawnInitialInput {
    UserInput(Vec<UserInput>),
    InterAgentCommunication(InterAgentCommunication, AgentCommunicationContext),
}

fn keep_forked_rollout_item(item: &RolloutItem, preserve_reference_context_item: bool) -> bool {
    match item {
        RolloutItem::ResponseItem(ResponseItem::Message { role, phase, .. }) => match role.as_str()
        {
            "system" | "developer" | "user" => true,
            "assistant" => *phase == Some(MessagePhase::FinalAnswer),
            _ => false,
        },
        RolloutItem::ResponseItem(
            ResponseItem::AdditionalTools { .. }
            | ResponseItem::AgentMessage { .. }
            | ResponseItem::Reasoning { .. }
            | ResponseItem::LocalShellCall { .. }
            | ResponseItem::FunctionCall { .. }
            | ResponseItem::ToolSearchCall { .. }
            | ResponseItem::FunctionCallOutput { .. }
            | ResponseItem::CustomToolCall { .. }
            | ResponseItem::CustomToolCallOutput { .. }
            | ResponseItem::ToolSearchOutput { .. }
            | ResponseItem::WebSearchCall { .. }
            | ResponseItem::ImageGenerationCall { .. }
            | ResponseItem::Compaction { .. }
            | ResponseItem::CompactionTrigger { .. }
            | ResponseItem::ContextCompaction { .. }
            | ResponseItem::Other,
        ) => false,
        RolloutItem::InterAgentCommunication(_)
        | RolloutItem::InterAgentCommunicationMetadata { .. } => false,
        // Full-history forks preserve the cached prompt prefix and can keep diffing
        // from the parent's durable baseline. Truncated forks drop part of that prompt,
        // so they must rebuild context on their first child turn.
        RolloutItem::TurnContext(_) | RolloutItem::WorldState(_) => preserve_reference_context_item,
        // Origin records are content-keyed: a dropped item's record matches nothing.
        RolloutItem::Compacted(_)
        | RolloutItem::EventMsg(_)
        | RolloutItem::SessionMeta(_)
        | RolloutItem::SourceOrigin(_) => true,
    }
}

fn is_multi_agent_v2_usage_hint_message(item: &ResponseItem, usage_hint_texts: &[String]) -> bool {
    let ResponseItem::Message { role, content, .. } = item else {
        return false;
    };
    if role != "developer" {
        return false;
    }
    let [ContentItem::InputText { text }] = content.as_slice() else {
        return false;
    };

    usage_hint_texts
        .iter()
        .any(|usage_hint_text| usage_hint_text == text)
}

async fn load_agent_model_context(
    state: &ThreadManagerState,
    thread_id: ThreadId,
    history_mode: ThreadHistoryMode,
) -> CodexResult<Option<Vec<RolloutItem>>> {
    match history_mode {
        ThreadHistoryMode::Legacy => Ok(state
            .read_stored_thread(ReadThreadParams {
                thread_id,
                include_archived: true,
                include_history: true,
            })
            .await?
            .history
            .map(|history| history.items)),
        ThreadHistoryMode::Paginated => Ok(Some(
            state
                .load_latest_model_context(LoadThreadHistoryParams {
                    thread_id,
                    include_archived: true,
                })
                .await?
                .items,
        )),
    }
}

fn latest_runtime_from_model_context(
    history: &[RolloutItem],
) -> (
    Option<String>,
    Option<String>,
    Option<codex_protocol::openai_models::ReasoningEffort>,
) {
    let mut model = None;
    let mut model_provider: Option<String> = None;
    let mut reasoning_effort = None;
    for item in history {
        match item {
            RolloutItem::TurnContext(turn_context) => {
                model = Some(turn_context.model.clone());
                model_provider = turn_context.model_provider.clone();
                reasoning_effort = turn_context.effort.clone();
            }
            RolloutItem::EventMsg(EventMsg::ThreadSettingsApplied(event)) => {
                model = Some(event.thread_settings.model.clone());
                model_provider = Some(event.thread_settings.model_provider_id.clone());
                reasoning_effort = event.thread_settings.reasoning_effort.clone();
            }
            _ => {}
        }
    }
    (model, model_provider, reasoning_effort)
}

/// #1129: the account (`default` included) the newest turn context recorded,
/// while named accounts were on, when it ran on `provider_id`.
fn recorded_account_from_model_context<'a>(
    history: &'a [RolloutItem],
    provider_id: &str,
) -> Option<&'a str> {
    history.iter().rev().find_map(|item| match item {
        RolloutItem::TurnContext(turn_context) => Some(
            turn_context
                .provider_account
                .as_deref()
                .filter(|_| turn_context.model_provider.as_deref() == Some(provider_id)),
        ),
        _ => None,
    })?
}

impl AgentControl {
    /// Restore persisted V2 agent identities without reopening their runtimes.
    pub(crate) async fn restore_v2_agent_metadata(
        &self,
        config: &Config,
        root_thread_id: ThreadId,
    ) {
        self.state.register_root_thread(root_thread_id);

        let Ok(state) = self.upgrade() else {
            return;
        };
        let Some(agent_graph_store) = state.agent_graph_store() else {
            return;
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
                warn!("failed to restore persisted V2 agent metadata for {root_thread_id}: {err}");
                return;
            }
        };

        for thread_id in descendant_ids {
            if self.state.agent_metadata_for_thread(thread_id).is_some() {
                continue;
            }
            let restore_result = async {
                let stored_thread = state
                    .read_stored_thread(ReadThreadParams {
                        thread_id,
                        include_archived: true,
                        include_history: false,
                    })
                    .await?;
                let stored_agent_path = stored_thread
                    .agent_path
                    .as_deref()
                    .map(AgentPath::try_from)
                    .transpose()
                    .map_err(|err| {
                        CodexErr::InvalidRequest(format!("invalid stored agent path: {err}"))
                    })?;
                let mut reservation = self.state.reserve_spawn_slot(/*max_threads*/ None)?;
                let mut metadata = self.prepare_agent_metadata(
                    &mut reservation,
                    config,
                    stored_agent_path.or_else(|| stored_thread.source.get_agent_path()),
                    stored_thread
                        .agent_role
                        .or_else(|| stored_thread.source.get_agent_role()),
                    stored_thread
                        .agent_nickname
                        .or_else(|| stored_thread.source.get_nickname()),
                )?;
                metadata.agent_id = Some(thread_id);
                reservation.commit(metadata);
                Ok::<(), CodexErr>(())
            }
            .await;
            if let Err(err) = restore_result {
                warn!("failed to restore V2 agent metadata for {thread_id}: {err}");
            }
        }
    }

    /// Spawn a new agent thread and submit the initial prompt.
    #[cfg(test)]
    pub(crate) async fn spawn_agent(
        &self,
        config: Config,
        initial_input: Vec<UserInput>,
        session_source: Option<SessionSource>,
    ) -> CodexResult<ThreadId> {
        let options = SpawnAgentOptions::default();
        let control = self
            .control_with_live_parent_security_policy(session_source.as_ref(), &options)
            .await?;
        let spawned_agent = Box::pin(control.spawn_agent_internal(
            config,
            SpawnInitialInput::UserInput(initial_input),
            session_source,
            options,
        ))
        .await?;
        Ok(spawned_agent.thread_id)
    }

    /// Spawn an agent thread with some metadata.
    pub(crate) async fn spawn_agent_with_metadata(
        &self,
        config: Config,
        initial_input: Vec<UserInput>,
        session_source: Option<SessionSource>,
        options: SpawnAgentOptions, // TODO(jif) drop with new fork.
    ) -> CodexResult<LiveAgent> {
        let control = self
            .control_with_live_parent_security_policy(session_source.as_ref(), &options)
            .await?;
        Box::pin(control.spawn_agent_internal(
            config,
            SpawnInitialInput::UserInput(initial_input),
            session_source,
            options,
        ))
        .await
    }

    pub(crate) async fn spawn_agent_with_communication(
        &self,
        config: Config,
        communication: InterAgentCommunication,
        context: AgentCommunicationContext,
        session_source: Option<SessionSource>,
        options: SpawnAgentOptions,
    ) -> CodexResult<LiveAgent> {
        let control = self
            .control_with_live_parent_security_policy(session_source.as_ref(), &options)
            .await?;
        Box::pin(control.spawn_agent_internal(
            config,
            SpawnInitialInput::InterAgentCommunication(communication, context),
            session_source,
            options,
        ))
        .await
    }

    async fn control_with_live_parent_security_policy(
        &self,
        session_source: Option<&SessionSource>,
        options: &SpawnAgentOptions,
    ) -> CodexResult<Self> {
        let parent_thread_id = options.parent_thread_id.or_else(|| {
            session_source.and_then(|source| match source {
                SessionSource::SubAgent(SubAgentSource::ThreadSpawn {
                    parent_thread_id, ..
                }) => Some(*parent_thread_id),
                _ => None,
            })
        });
        let Some(parent_thread_id) = parent_thread_id else {
            return Ok(self.clone());
        };
        if self
            .security_policy
            .snapshot_for_agent(parent_thread_id)
            .is_ok()
        {
            return Ok(self.clone());
        }

        let state = self.upgrade()?;
        let parent_thread = state.get_thread(parent_thread_id).await?;
        let parent_control = parent_thread.session.services.agent_control.clone();
        parent_control
            .security_policy
            .snapshot_for_agent(parent_thread_id)
            .map_err(|error| {
                CodexErr::Fatal(format!(
                    "live parent has no effective security policy: {error}"
                ))
            })?;

        let mut control = self.clone();
        control.session_id = parent_control.session_id;
        control.security_policy = parent_control.security_policy;
        control.trusted_security_controller = parent_control.trusted_security_controller;
        Ok(control)
    }

    pub(crate) async fn ensure_v2_agent_loaded(
        &self,
        mut config: Config,
        thread_id: ThreadId,
    ) -> CodexResult<()> {
        let state = self.upgrade()?;
        if state.get_thread(thread_id).await.is_ok() {
            self.touch_loaded_v2_residency(&state, thread_id).await;
            return Ok(());
        }
        // The caller's runtime provider, before the stored runtime replaces it.
        let caller = (
            config.model_provider_id.clone(),
            config.model_provider.clone(),
        );
        if self.state.agent_metadata_for_thread(thread_id).is_none() {
            warn!(%thread_id, "cannot cold-load V2 agent without restored metadata");
            return Err(CodexErr::ThreadNotFound(thread_id));
        }

        let stored_thread = state
            .read_stored_thread(ReadThreadParams {
                thread_id,
                include_archived: true,
                include_history: false,
            })
            .await
            .inspect_err(|err| warn!(%thread_id, %err, "failed to read stored V2 agent"))?;
        let stored_source = stored_thread.source.clone();
        let stored_parent_thread_id = stored_thread.parent_thread_id;
        let stored_role = stored_thread
            .agent_role
            .clone()
            .or_else(|| stored_source.get_agent_role());
        if let Some(role_name) = stored_role.as_deref() {
            let runtime_approval_policy = config.permissions.approval_policy.value();
            let runtime_approvals_reviewer = config.approvals_reviewer;
            let runtime_cwd = config.cwd.clone();
            let runtime_permission_profile = match config.permissions.active_permission_profile() {
                Some(active_permission_profile) => {
                    PermissionProfileSnapshot::active_with_profile_workspace_roots(
                        config.permissions.permission_profile().clone(),
                        active_permission_profile,
                        config.permissions.profile_workspace_roots().to_vec(),
                    )
                }
                None => PermissionProfileSnapshot::legacy(
                    config.permissions.permission_profile().clone(),
                ),
            };

            // Reapply the persisted role before resolving the stored runtime. Role files may own
            // custom provider definitions; resolving first makes a valid cold-resumed agent look
            // unconfigured. The persisted provider/model below still win over role-file changes.
            apply_role_to_config_for_multi_agent_v2(&mut config, Some(role_name))
                .await
                .map_err(CodexErr::InvalidRequest)?;
            config
                .permissions
                .approval_policy
                .set(runtime_approval_policy)
                .map_err(|err| {
                    CodexErr::InvalidRequest(format!("approval_policy is invalid: {err}"))
                })?;
            config.approvals_reviewer = runtime_approvals_reviewer;
            config.cwd = runtime_cwd;
            config
                .permissions
                .set_permission_profile_from_session_snapshot(runtime_permission_profile)
                .map_err(|err| {
                    CodexErr::InvalidRequest(format!("permission_profile is invalid: {err}"))
                })?;
        }
        let history = load_agent_model_context(&state, thread_id, stored_thread.history_mode)
            .await
            .inspect_err(|err| warn!(%thread_id, %err, "failed to load V2 agent history"))?
            .ok_or_else(|| {
                warn!(%thread_id, "stored V2 agent has no resumable history");
                CodexErr::ThreadNotFound(thread_id)
            })?;
        let (history_model, history_provider, history_reasoning_effort) =
            latest_runtime_from_model_context(&history);
        let stored_model = stored_thread
            .model
            .clone()
            .or(history_model)
            .ok_or_else(|| {
                CodexErr::InvalidRequest(format!(
                    "cannot resume agent {thread_id}: its persisted runtime has no model"
                ))
            })?;
        let stored_provider_id = if stored_thread.model_provider.is_empty() {
            history_provider.ok_or_else(|| {
                CodexErr::InvalidRequest(format!(
                    "cannot resume agent {thread_id}: its persisted runtime has no provider"
                ))
            })?
        } else {
            stored_thread.model_provider.clone()
        };
        let mut stored_provider = config
            .model_providers
            .get(&stored_provider_id)
            .cloned()
            .ok_or_else(|| {
                CodexErr::InvalidRequest(format!(
                    "cannot resume agent {thread_id}: persisted provider `{stored_provider_id}` is not configured"
                ))
            })?;
        // #1129: back on the account it ran on, or refused.
        if let Some(recorded) = recorded_account_from_model_context(&history, &stored_provider_id) {
            crate::config::restore_recorded_agent_account(
                &config,
                &stored_provider_id,
                &mut stored_provider,
                recorded,
            )
            .map_err(CodexErr::InvalidRequest)?;
        }
        // #1074: refused before it starts when its connection has no
        // credential of its own (#689).
        if let Some(message) = crate::config::resume_default_key_error(
            &config,
            &stored_provider_id,
            &stored_provider,
            &caller.0,
            &caller.1,
        ) {
            return Err(CodexErr::InvalidRequest(message));
        }
        config.model = Some(stored_model);
        config.model_provider_id = stored_provider_id;
        config.model_provider = stored_provider;
        config.model_reasoning_effort = stored_thread
            .reasoning_effort
            .clone()
            .or(history_reasoning_effort);
        let initial_history = InitialHistory::Resumed(ResumedHistory {
            conversation_id: thread_id,
            history: Arc::new(history),
            rollout_path: stored_thread.rollout_path,
        });
        // Older persisted histories can predate the explicit V2 marker even though the
        // current runtime and agent graph are V2. The configured feature is therefore an
        // equally authoritative recovery signal at this cold-load boundary.
        if config.multi_agent_version_from_features() != MultiAgentVersion::V2
            && initial_history.get_multi_agent_version() != Some(MultiAgentVersion::V2)
        {
            warn!(
                %thread_id,
                configured_version = ?config.multi_agent_version_from_features(),
                persisted_version = ?initial_history.get_multi_agent_version(),
                "refusing to cold-load an agent without a V2 recovery signal"
            );
            return Err(CodexErr::ThreadNotFound(thread_id));
        }
        let (session_source, _) = initial_history
            .get_resumed_session_sources()
            .unwrap_or((stored_source, None));
        let residency_slot = self
            .reserve_v2_residency_slot(&state, &config, Some(thread_id))
            .await?;

        let parent_thread_id = initial_history
            .get_resumed_parent_thread_id()
            .or(stored_parent_thread_id);
        let inherited_environments = self
            .inherited_environments_for_source(&state, Some(&session_source))
            .await;
        let inherited_exec_policy = self
            .inherited_exec_policy_for_source(&state, Some(&session_source), &config)
            .await;

        match state
            .resume_thread_with_history_with_source(ResumeThreadWithHistoryOptions {
                config,
                initial_history,
                agent_control: self.clone(),
                session_source,
                parent_thread_id,
                inherited_environments,
                inherited_exec_policy,
            })
            .await
        {
            Ok(reloaded_thread) => {
                residency_slot.commit(reloaded_thread.thread_id);
                state.notify_thread_created(reloaded_thread.thread_id);
                self.recover_inter_agent_communications(reloaded_thread.thread_id)
                    .await?;
                Ok(())
            }
            Err(err) => {
                if state.get_thread(thread_id).await.is_ok() {
                    drop(residency_slot);
                    self.touch_loaded_v2_residency(&state, thread_id).await;
                    return Ok(());
                }
                warn!(%thread_id, %err, "failed to resume persisted V2 agent runtime");
                Err(err)
            }
        }
    }

    async fn spawn_agent_internal(
        &self,
        config: Config,
        initial_input: SpawnInitialInput,
        session_source: Option<SessionSource>,
        options: SpawnAgentOptions,
    ) -> CodexResult<LiveAgent> {
        let state = self.upgrade()?;
        let multi_agent_version = state
            .effective_multi_agent_version_for_spawn(
                &InitialHistory::New,
                session_source.as_ref(),
                options.parent_thread_id,
                /*forked_from_thread_id*/ None,
                &config,
            )
            .await;
        if let Some(SessionSource::SubAgent(SubAgentSource::ThreadSpawn {
            parent_thread_id,
            depth,
            ..
        })) = session_source.as_ref()
        {
            let parent_thread = state.get_thread(*parent_thread_id).await?;
            let parent_snapshot = parent_thread.config_snapshot().await;
            let parent_depth = match &parent_snapshot.session_source {
                SessionSource::SubAgent(SubAgentSource::ThreadSpawn { depth, .. }) => *depth,
                _ => 0,
            };
            let expected_depth = parent_depth + 1;
            if *depth != expected_depth {
                return Err(CodexErr::InvalidRequest(format!(
                    "spawn depth {depth} does not follow parent depth {parent_depth}"
                )));
            }
        }
        if let Some(session_source) = session_source.as_ref() {
            self.prepare_durable_thread_spawn_parent(session_source, config.ephemeral)
                .await?;
        }
        let initial_input_uses_v2_mailbox = multi_agent_version == MultiAgentVersion::V2
            && matches!(
                &initial_input,
                SpawnInitialInput::InterAgentCommunication(..)
            );
        if !initial_input_uses_v2_mailbox && let Some(session_source) = session_source.as_ref() {
            self.ensure_execution_capacity(multi_agent_version, session_source)?;
        }
        let agent_max_threads = config.effective_agent_max_threads(multi_agent_version);
        let spawn_uses_v2_residency = multi_agent_version == MultiAgentVersion::V2
            && session_source
                .as_ref()
                .is_some_and(is_v2_resident_session_source);
        let residency_slot = if spawn_uses_v2_residency {
            Some(
                self.reserve_v2_residency_slot(&state, &config, /*protected_thread_id*/ None)
                    .await?,
            )
        } else {
            None
        };
        let reservation_max_threads = if spawn_uses_v2_residency {
            None
        } else {
            agent_max_threads
        };
        let mut reservation = self.state.reserve_spawn_slot(reservation_max_threads)?;
        let inheritance = SpawnAgentThreadInheritance {
            environments: self
                .inherited_environments_for_source(&state, session_source.as_ref())
                .await,
            exec_policy: self
                .inherited_exec_policy_for_source(&state, session_source.as_ref(), &config)
                .await,
        };
        let (session_source, mut agent_metadata) = match session_source {
            Some(SessionSource::SubAgent(SubAgentSource::ThreadSpawn {
                parent_thread_id,
                depth,
                agent_path,
                agent_role,
                agent_class,
                ..
            })) => {
                let (session_source, agent_metadata) = self.prepare_thread_spawn(
                    &mut reservation,
                    &config,
                    parent_thread_id,
                    depth,
                    agent_path,
                    agent_role,
                    /*preferred_agent_nickname*/ None,
                    agent_class,
                )?;
                (Some(session_source), agent_metadata)
            }
            other => (other, AgentMetadata::default()),
        };
        let notification_source = session_source.clone();
        let configured_child_security_level = config.security_level;
        let security_task_id = options.parent_turn_id.clone();

        // The same `AgentControl` is sent to spawn the thread.
        let new_thread = match (session_source, options.fork_mode.as_ref(), inheritance) {
            (Some(session_source), Some(_), inheritance) => {
                Box::pin(self.spawn_forked_thread(
                    &state,
                    config,
                    session_source,
                    &options,
                    inheritance,
                    multi_agent_version,
                ))
                .await?
            }
            (Some(session_source), None, inheritance) => {
                let history_mode = if let Some(parent_thread_id) = options.parent_thread_id
                    && let Ok(parent_thread) = state.get_thread(parent_thread_id).await
                {
                    matches!(
                        parent_thread.config_snapshot().await.history_mode,
                        ThreadHistoryMode::Paginated
                    )
                    .then_some(ThreadHistoryMode::Paginated)
                } else {
                    None
                };
                Box::pin(state.spawn_new_thread_with_source(
                    config.clone(),
                    self.clone(),
                    session_source,
                    history_mode,
                    options.parent_thread_id,
                    /*forked_from_thread_id*/ None,
                    /*thread_source*/ Some(ThreadSource::Subagent),
                    /*metrics_service_name*/ None,
                    inheritance.environments,
                    inheritance.exec_policy,
                    options.environments.clone(),
                ))
                .await?
            }
            (None, _, _) => Box::pin(state.spawn_new_thread(config.clone(), self.clone())).await?,
        };
        if let Some(SessionSource::SubAgent(SubAgentSource::ThreadSpawn {
            parent_thread_id, ..
        })) = notification_source.as_ref()
        {
            let task_id =
                security_task_id.unwrap_or_else(|| format!("task:spawn:{}", new_thread.thread_id));
            if let Err(error) = self.security_policy.inherit_child(
                *parent_thread_id,
                new_thread.thread_id,
                task_id,
                configured_child_security_level,
            ) {
                let _ = state.remove_thread(&new_thread.thread_id).await;
                return Err(CodexErr::Fatal(format!(
                    "failed to inherit effective security policy: {error}"
                )));
            }
        }
        agent_metadata.agent_id = Some(new_thread.thread_id);
        reservation.commit(agent_metadata.clone());
        if let Some(residency_slot) = residency_slot {
            residency_slot.commit(new_thread.thread_id);
        }

        if let Some(SessionSource::SubAgent(
            subagent_source @ SubAgentSource::ThreadSpawn {
                parent_thread_id, ..
            },
        )) = notification_source.as_ref()
        {
            let client_metadata = match state.get_thread(*parent_thread_id).await {
                Ok(parent_thread) => parent_thread.session.app_server_client_metadata().await,
                Err(error) => {
                    tracing::warn!(
                        error = %error,
                        parent_thread_id = %parent_thread_id,
                        "skipping subagent thread analytics: failed to load parent thread metadata"
                    );
                    crate::session::session::AppServerClientMetadata {
                        client_name: None,
                        client_version: None,
                    }
                }
            };
            let thread_config = new_thread.thread.config_snapshot().await;
            let parent_thread_id = thread_config.parent_thread_id;
            emit_subagent_session_started(
                &new_thread.thread.session.services.analytics_events_client,
                client_metadata,
                new_thread.thread.session.session_id(),
                new_thread.thread_id,
                parent_thread_id,
                thread_config,
                subagent_source.clone(),
            );
        }

        // Notify a new thread has been created. This notification will be processed by clients
        // to subscribe or drain this newly created thread.
        // TODO(jif) add helper for drain
        state.notify_thread_created(new_thread.thread_id);

        self.persist_durable_thread_spawn(
            new_thread.thread.as_ref(),
            new_thread.thread_id,
            notification_source.as_ref(),
        )
        .await?;

        // D435: until its first turn starts, an interrupt must still reach it.
        let starts_turn = match &initial_input {
            SpawnInitialInput::UserInput(_) => true,
            SpawnInitialInput::InterAgentCommunication(communication, _) => {
                communication.trigger_turn
            }
        };
        new_thread
            .thread
            .session
            .spawned_with_first_turn
            .store(starts_turn, std::sync::atomic::Ordering::Release);
        match initial_input {
            SpawnInitialInput::UserInput(input) => {
                // The spawning agent wrote this task; it carries that agent's standing.
                let sender = options
                    .parent_thread_id
                    .or(match notification_source.as_ref() {
                        Some(SessionSource::SubAgent(SubAgentSource::ThreadSpawn {
                            parent_thread_id,
                            ..
                        })) => Some(*parent_thread_id),
                        _ => None,
                    });
                self.send_input_after_capacity_check(
                    new_thread.thread_id,
                    sender,
                    &state,
                    input,
                    options.parent_turn_id,
                )
                .await?;
            }
            SpawnInitialInput::InterAgentCommunication(communication, context) => {
                self.send_inter_agent_communication_after_capacity_check(
                    new_thread.thread_id,
                    &state,
                    communication,
                    context,
                    options.parent_turn_id,
                )
                .await?;
            }
        }
        if multi_agent_version != MultiAgentVersion::V2 {
            let child_reference = agent_metadata
                .agent_path
                .as_ref()
                .map(ToString::to_string)
                .unwrap_or_else(|| new_thread.thread_id.to_string());
            self.maybe_start_completion_watcher(
                new_thread.thread_id,
                notification_source,
                child_reference,
                agent_metadata.agent_path.clone(),
            );
        }

        Ok(LiveAgent {
            thread_id: new_thread.thread_id,
            metadata: agent_metadata,
            status: self.get_status(new_thread.thread_id).await,
        })
    }

    async fn spawn_forked_thread(
        &self,
        state: &Arc<ThreadManagerState>,
        config: Config,
        session_source: SessionSource,
        options: &SpawnAgentOptions,
        inheritance: SpawnAgentThreadInheritance,
        multi_agent_version: MultiAgentVersion,
    ) -> CodexResult<crate::thread_manager::NewThread> {
        let SpawnAgentThreadInheritance {
            environments: inherited_environments,
            exec_policy: inherited_exec_policy,
        } = inheritance;
        if options.fork_parent_spawn_call_id.is_none() {
            return Err(CodexErr::Fatal(
                "spawn_agent fork requires a parent spawn call id".to_string(),
            ));
        }
        let Some(fork_mode) = options.fork_mode.as_ref() else {
            return Err(CodexErr::Fatal(
                "spawn_agent fork requires a fork mode".to_string(),
            ));
        };
        let SessionSource::SubAgent(SubAgentSource::ThreadSpawn {
            parent_thread_id, ..
        }) = &session_source
        else {
            return Err(CodexErr::Fatal(
                "spawn_agent fork requires a thread-spawn session source".to_string(),
            ));
        };

        let parent_thread_id = *parent_thread_id;
        let parent_thread = state.get_thread(parent_thread_id).await?;
        let (subagent_developer_instructions, parent_developer_instructions) = match (
            multi_agent_version,
            config
                .multi_agent_v2
                .subagent_developer_instructions
                .as_ref(),
        ) {
            (MultiAgentVersion::V2, Some(_)) => {
                let parent_developer_instructions = match parent_thread
                    .session
                    .new_default_turn()
                    .await
                    .developer_instructions
                    .clone()
                {
                    Some(instructions) if !instructions.is_empty() => Some(instructions),
                    Some(_) | None => None,
                };
                (
                    Some(config.developer_instructions.clone().unwrap_or_default()),
                    parent_developer_instructions,
                )
            }
            (MultiAgentVersion::Disabled | MultiAgentVersion::V1, _)
            | (MultiAgentVersion::V2, None) => (None, None),
        };
        let parent_history_mode = parent_thread.config_snapshot().await.history_mode;
        // `record_conversation_items` only queues persistence writes asynchronously.
        // Flush before snapshotting store history for a fork.
        parent_thread.ensure_rollout_materialized().await;
        parent_thread.flush_rollout().await?;

        let destination_history_mode = matches!(parent_history_mode, ThreadHistoryMode::Paginated)
            .then_some(ThreadHistoryMode::Paginated);
        let mut forked_rollout_items =
            load_agent_model_context(state, parent_thread_id, parent_history_mode)
                .await?
                .ok_or_else(|| {
                    CodexErr::Fatal(format!(
                        "parent thread history unavailable for fork: {parent_thread_id}"
                    ))
                })?;

        let selected_capability_roots = forked_rollout_items
            .iter()
            .find_map(|item| {
                let RolloutItem::SessionMeta(meta_line) = item else {
                    return None;
                };
                Some(meta_line.meta.selected_capability_roots.clone())
            })
            .unwrap_or_default();
        if let SpawnAgentForkMode::LastNTurns(last_n_turns) = fork_mode {
            forked_rollout_items =
                truncate_rollout_to_last_n_fork_turns(forked_rollout_items, *last_n_turns);
        }
        let multi_agent_v2_usage_hint_texts_to_filter: Vec<String> =
            if multi_agent_version == MultiAgentVersion::V2 {
                let parent_config = parent_thread.session.get_config().await;
                [
                    parent_config
                        .multi_agent_v2
                        .root_agent_usage_hint_text
                        .clone(),
                    parent_config
                        .multi_agent_v2
                        .subagent_usage_hint_text
                        .clone(),
                ]
                .into_iter()
                .flatten()
                .collect()
            } else {
                Vec::new()
            };
        let mut preserve_reference_context_item =
            matches!(fork_mode, SpawnAgentForkMode::FullHistory);
        if preserve_reference_context_item {
            for item in forked_rollout_items.iter().rev() {
                let RolloutItem::Compacted(compacted) = item else {
                    continue;
                };
                // Legacy checkpoints force the child to rebuild context regardless of the
                // live parent's reference baseline; an older superseded checkpoint does not.
                if compacted.replacement_history.is_none() {
                    preserve_reference_context_item = false;
                }
                break;
            }
        }
        let mut replaced_parent_developer_instructions = false;
        // Scrub inherited hints and replace only the parent's developer-instruction fragment.
        // Compaction stores response items separately, so sanitize both top-level messages and
        // compacted replacement histories with the same policy.
        let retain_forked_item = |response_item: &mut ResponseItem, replaced: &mut bool| {
            if matches!(response_item, ResponseItem::AgentMessage { .. }) {
                return false;
            }
            if is_multi_agent_v2_usage_hint_message(
                response_item,
                &multi_agent_v2_usage_hint_texts_to_filter,
            ) {
                return false;
            }

            if let Some(parent_developer_instructions) = parent_developer_instructions.as_ref()
                && let Some(subagent_developer_instructions) =
                    subagent_developer_instructions.as_ref()
                && matches!(response_item, ResponseItem::Message { role, .. } if role == "developer")
            {
                response_item.retain_message_content(|content_item| {
                    let ContentItem::InputText { text } = content_item else {
                        return true;
                    };
                    // TODO(anp) track better message fragment provenance in rollouts.
                    if !text.contains(parent_developer_instructions) {
                        return true;
                    }

                    *replaced = true;
                    let replacement = if preserve_reference_context_item {
                        subagent_developer_instructions.as_str()
                    } else {
                        ""
                    };
                    *text = text.replace(parent_developer_instructions, replacement);
                    !text.is_empty()
                });
                return !matches!(
                    response_item,
                    ResponseItem::Message { content, .. } if content.is_empty()
                );
            }

            true
        };
        forked_rollout_items.retain_mut(|item| {
            if !keep_forked_rollout_item(item, preserve_reference_context_item)
                || destination_history_mode == Some(ThreadHistoryMode::Paginated)
                    && matches!(
                        &*item,
                        RolloutItem::EventMsg(
                            EventMsg::ItemCompleted(_)
                                | EventMsg::TokenCount(_)
                                | EventMsg::ThreadGoalUpdated(_)
                                | EventMsg::ThreadSettingsApplied(_),
                        )
                    )
            {
                return false;
            }

            match item {
                RolloutItem::ResponseItem(response_item) => {
                    retain_forked_item(response_item, &mut replaced_parent_developer_instructions)
                }
                RolloutItem::Compacted(compacted) => {
                    if let Some(replacement_history) = compacted.replacement_history.as_mut() {
                        // Matches before this checkpoint cannot survive its replacement history.
                        replaced_parent_developer_instructions = false;
                        replacement_history.retain_mut(|response_item| {
                            retain_forked_item(
                                response_item,
                                &mut replaced_parent_developer_instructions,
                            )
                        });
                    }
                    true
                }
                RolloutItem::EventMsg(_)
                | RolloutItem::SessionMeta(_)
                | RolloutItem::TurnContext(_)
                | RolloutItem::InterAgentCommunication(_)
                | RolloutItem::InterAgentCommunicationMetadata { .. }
                | RolloutItem::WorldState(_)
                | RolloutItem::SourceOrigin(_) => true,
            }
        });
        // Full forks reuse the parent's reference context instead of rebuilding it. If that
        // context omitted the parent's developer fragment, append the child's override so its
        // instructions still reach the model exactly once.
        if let Some(subagent_developer_instructions) = subagent_developer_instructions.as_ref()
            && preserve_reference_context_item
            && !replaced_parent_developer_instructions
            && !subagent_developer_instructions.is_empty()
            && parent_thread
                .session
                .reference_context_item()
                .await
                .is_some()
            && let Some(developer_message) =
                crate::context_manager::updates::build_developer_update_item(vec![
                    subagent_developer_instructions.clone(),
                ])
        {
            forked_rollout_items.push(RolloutItem::ResponseItem(developer_message));
        }
        if preserve_reference_context_item
            && multi_agent_version == MultiAgentVersion::V2
            && let Some(subagent_usage_hint_text) =
                config.multi_agent_v2.subagent_usage_hint_text.clone()
            && let Some(subagent_usage_hint_message) =
                crate::context_manager::updates::build_developer_update_item(vec![
                    subagent_usage_hint_text,
                ])
        {
            forked_rollout_items.push(RolloutItem::ResponseItem(subagent_usage_hint_message));
        }
        let mut thread_extension_init = ExtensionDataInit::new();
        thread_extension_init.insert(selected_capability_roots);

        state
            .fork_thread_with_source(
                config.clone(),
                InitialHistory::Forked(forked_rollout_items),
                destination_history_mode,
                self.clone(),
                session_source,
                /*thread_source*/ Some(ThreadSource::Subagent),
                /*parent_thread_id*/ Some(parent_thread_id),
                /*forked_from_thread_id*/ Some(parent_thread_id),
                inherited_environments,
                inherited_exec_policy,
                options.environments.clone(),
                thread_extension_init,
            )
            .await
    }

    /// Resume an existing agent thread from a recorded rollout file.
    #[cfg(test)]
    pub(crate) async fn resume_agent_from_rollout(
        &self,
        config: Config,
        thread_id: ThreadId,
        session_source: SessionSource,
    ) -> CodexResult<ThreadId> {
        self.resume_agent_tree_from_rollout(config, thread_id, session_source)
            .await
            .map(|(thread_id, _)| thread_id)
    }

    /// Resume an existing agent thread from a recorded rollout file, with its
    /// open V1 descendants. A descendant that cannot be resumed (its recorded
    /// account is gone, say) does not fail the resume; it is returned with
    /// why, after the resumed thread's id, so the caller can tell the model
    /// and the user (#1144).
    pub(crate) async fn resume_agent_tree_from_rollout(
        &self,
        config: Config,
        thread_id: ThreadId,
        session_source: SessionSource,
    ) -> CodexResult<(ThreadId, Vec<(ThreadId, String)>)> {
        let root_depth = thread_spawn_depth(&session_source).unwrap_or(0);
        let (resumed_thread_id, resumed_multi_agent_version) = Box::pin(
            self.resume_single_agent_from_rollout(config.clone(), thread_id, session_source),
        )
        .await?;
        let mut refused_descendants = Vec::new();
        let state = self.upgrade()?;
        if config.multi_agent_version_from_features() == MultiAgentVersion::V2
            || resumed_multi_agent_version == MultiAgentVersion::V2
        {
            self.restore_persisted_agent_subtree(resumed_thread_id)
                .await?;
            return Ok((resumed_thread_id, refused_descendants));
        }
        let Some(agent_graph_store) = state.agent_graph_store() else {
            return Ok((resumed_thread_id, refused_descendants));
        };

        let mut resume_queue = VecDeque::from([(thread_id, root_depth)]);
        while let Some((parent_thread_id, parent_depth)) = resume_queue.pop_front() {
            let child_ids = match agent_graph_store
                .list_thread_spawn_children(
                    parent_thread_id,
                    Some(codex_agent_graph_store::ThreadSpawnEdgeStatus::Open),
                )
                .await
            {
                Ok(child_ids) => child_ids,
                Err(err) => {
                    warn!(
                        "failed to load persisted thread-spawn children for {parent_thread_id}: {err}"
                    );
                    continue;
                }
            };

            for child_thread_id in child_ids {
                let child_depth = parent_depth + 1;
                let child_resumed = if state.get_thread(child_thread_id).await.is_ok() {
                    true
                } else {
                    let child_session_source =
                        SessionSource::SubAgent(SubAgentSource::ThreadSpawn {
                            parent_thread_id,
                            depth: child_depth,
                            agent_path: None,
                            agent_nickname: None,
                            agent_role: None,
                            agent_class: None,
                        });
                    match Box::pin(self.resume_single_agent_from_rollout(
                        config.clone(),
                        child_thread_id,
                        child_session_source,
                    ))
                    .await
                    {
                        Ok((_, _)) => true,
                        Err(err) => {
                            warn!("failed to resume descendant thread {child_thread_id}: {err}");
                            refused_descendants.push((child_thread_id, err.to_string()));
                            false
                        }
                    }
                };
                if child_resumed {
                    resume_queue.push_back((child_thread_id, child_depth));
                }
            }
        }

        Ok((resumed_thread_id, refused_descendants))
    }

    async fn resume_single_agent_from_rollout(
        &self,
        mut config: Config,
        thread_id: ThreadId,
        session_source: SessionSource,
    ) -> CodexResult<(ThreadId, MultiAgentVersion)> {
        let state = self.upgrade()?;
        let stored_thread = state
            .read_stored_thread(ReadThreadParams {
                thread_id,
                include_archived: true,
                include_history: false,
            })
            .await?;
        let resumed_agent_path = stored_thread
            .agent_path
            .as_deref()
            .map(AgentPath::try_from)
            .transpose()
            .map_err(|err| CodexErr::InvalidRequest(format!("invalid stored agent path: {err}")))?;
        let resumed_agent_nickname = stored_thread.agent_nickname.clone();
        let resumed_agent_role = stored_thread.agent_role.clone();
        let history = load_agent_model_context(&state, thread_id, stored_thread.history_mode)
            .await?
            .ok_or(CodexErr::ThreadNotFound(thread_id))?;
        // #1129: an agent that ran on another account of its parent's
        // provider is resumed on that account, or refused.
        if let Some(recorded) =
            recorded_account_from_model_context(&history, &config.model_provider_id)
        {
            let parent = config.model_provider.clone();
            let mut provider = parent.clone();
            crate::config::restore_recorded_agent_account(
                &config,
                &config.model_provider_id,
                &mut provider,
                recorded,
            )
            .map_err(CodexErr::InvalidRequest)?;
            if provider.account != parent.account {
                // #1074: its own connection's credential, checked again now
                // that it may run on the default account.
                if let Some(message) = crate::config::resume_default_key_error(
                    &config,
                    &config.model_provider_id,
                    &provider,
                    &config.model_provider_id,
                    &parent,
                ) {
                    return Err(CodexErr::InvalidRequest(message));
                }
                config.model_provider = provider;
            }
        }
        let initial_history = InitialHistory::Resumed(ResumedHistory {
            conversation_id: thread_id,
            history: Arc::new(history),
            rollout_path: stored_thread.rollout_path,
        });
        let parent_thread_id = stored_thread.parent_thread_id;
        let multi_agent_version = state
            .effective_multi_agent_version_for_spawn(
                &initial_history,
                Some(&session_source),
                parent_thread_id,
                /*forked_from_thread_id*/ None,
                &config,
            )
            .await;
        let agent_max_threads = config.effective_agent_max_threads(multi_agent_version);
        let mut reservation = self.state.reserve_spawn_slot(agent_max_threads)?;
        let (session_source, agent_metadata) = match session_source {
            SessionSource::SubAgent(SubAgentSource::ThreadSpawn {
                parent_thread_id,
                depth,
                agent_path,
                agent_role: _,
                agent_nickname: _,
                agent_class,
            }) => self.prepare_thread_spawn(
                &mut reservation,
                &config,
                parent_thread_id,
                depth,
                agent_path.or(resumed_agent_path),
                resumed_agent_role,
                resumed_agent_nickname,
                agent_class,
            )?,
            other => (other, AgentMetadata::default()),
        };
        let notification_source = session_source.clone();
        let inherited_environments = self
            .inherited_environments_for_source(&state, Some(&session_source))
            .await;
        let inherited_exec_policy = self
            .inherited_exec_policy_for_source(&state, Some(&session_source), &config)
            .await;

        let resumed_thread = state
            .resume_thread_with_history_with_source(ResumeThreadWithHistoryOptions {
                config: config.clone(),
                initial_history,
                agent_control: self.clone(),
                session_source,
                parent_thread_id,
                inherited_environments,
                inherited_exec_policy,
            })
            .await?;
        let mut agent_metadata = agent_metadata;
        agent_metadata.agent_id = Some(resumed_thread.thread_id);
        reservation.commit(agent_metadata.clone());
        // Resumed threads are re-registered in-memory and need the same listener
        // attachment path as freshly spawned threads.
        state.notify_thread_created(resumed_thread.thread_id);
        if multi_agent_version != MultiAgentVersion::V2 {
            let child_reference = agent_metadata
                .agent_path
                .as_ref()
                .map(ToString::to_string)
                .unwrap_or_else(|| resumed_thread.thread_id.to_string());
            self.maybe_start_completion_watcher(
                resumed_thread.thread_id,
                Some(notification_source.clone()),
                child_reference,
                agent_metadata.agent_path.clone(),
            );
        }
        self.persist_durable_thread_spawn(
            resumed_thread.thread.as_ref(),
            resumed_thread.thread_id,
            Some(&notification_source),
        )
        .await?;

        Ok((resumed_thread.thread_id, multi_agent_version))
    }
}
