//! Thread routing, buffering, and app-server operation submission for the TUI app.
//!
//! This module manages active thread channels, routes server requests and notifications into those
//! channels, submits thread-scoped operations through the app server, and replays buffered events
//! when the visible thread changes.

use super::session_lifecycle::ThreadAttachPresentation;
use super::*;
use crate::chatwidget::ThreadInputStateRestoreMode;
use crate::session_resume::read_session_model;
use codex_app_server_protocol::TurnInterruptParams;
use codex_app_server_protocol::TurnInterruptResponse;
use codex_app_server_protocol::WarningNotification;
use std::fmt::Write as _;

impl App {
    pub(super) async fn surface_turn_start_failure(
        &mut self,
        thread_id: ThreadId,
        details: String,
        will_retry: bool,
    ) {
        let label = self.thread_label(thread_id);
        let message = if will_retry {
            format!("turn/start failed for {label}; recovered after bounded retry")
        } else {
            format!("turn/start failed for {label}; retry limit reached, pane remains available")
        };
        if self.active_thread_id == Some(thread_id) {
            self.chat_widget
                .add_error_message(format!("{message}. Cause: {details}"));
            return;
        }
        let notification = if will_retry {
            // A retryable Error notification is intentionally transient in ChatWidget and is
            // cleared by the following turn/started event. Buffer a targeted warning instead so
            // an inactive affected pane retains visible recovery evidence when selected.
            ServerNotification::Warning(codex_app_server_protocol::WarningNotification {
                thread_id: Some(thread_id.to_string()),
                message: format!("ERROR — {message}. Cause: {details}"),
            })
        } else {
            ServerNotification::Error(ErrorNotification {
                error: AppServerTurnError {
                    message,
                    codex_error_info: None,
                    additional_details: Some(details.clone()),
                },
                will_retry,
                thread_id: thread_id.to_string(),
                turn_id: "turn-start-recovery".to_string(),
            })
        };
        if let Err(error) = self
            .enqueue_thread_notification(thread_id, notification)
            .await
        {
            tracing::error!(
                %thread_id,
                error = ?error,
                error_chain = %format!("{error:#}"),
                "failed to enqueue pane-local turn/start error banner"
            );
            self.chat_widget.add_error_message(format!(
                "turn/start failed for {label}: {details}. The pane remains available."
            ));
        }
    }

    pub(crate) fn native_thread_loaded_for_orchestrate(&self, thread_id: ThreadId) -> bool {
        match self.agent_navigation.get(&thread_id) {
            Some(entry) => !entry.is_closed,
            None => self.thread_event_channels.contains_key(&thread_id),
        }
    }

    pub(crate) fn native_thread_idle_for_orchestrate(&self, thread_id: ThreadId) -> bool {
        self.thread_event_channels
            .get(&thread_id)
            .and_then(|channel| channel.store.try_lock().ok())
            .is_some_and(|store| store.active_turn_id().is_none())
    }

    #[cfg_attr(not(test), allow(dead_code))]
    pub(super) fn note_thread_interrupt_failure(
        &mut self,
        thread_id: ThreadId,
        error: TypedRequestError,
    ) {
        let label = self.thread_label(thread_id);
        tracing::warn!(
            %thread_id,
            %error,
            "thread interrupt failed in TUI; keeping the app alive"
        );
        self.chat_widget.add_error_message(format!(
            "Failed to interrupt {label}: {error}. The pane remains open."
        ));
    }

    pub(super) fn note_thread_steer_failure(
        &mut self,
        thread_id: ThreadId,
        error: TypedRequestError,
    ) {
        let label = self.thread_label(thread_id);
        tracing::error!(
            %thread_id,
            error = ?error,
            "turn/steer failed in TUI; queueing the user input and keeping the pane alive"
        );
        if !self.chat_widget.enqueue_rejected_steer() {
            self.chat_widget.add_error_message(format!(
                "Failed to steer {label}: {error}. The pane remains open."
            ));
        }
    }

    pub(super) async fn shutdown_current_thread(&mut self, app_server: &mut AppServerSession) {
        let side_thread_ids: Vec<ThreadId> = self.side_threads.keys().copied().collect();
        for side_thread_id in side_thread_ids {
            self.discard_side_thread(app_server, side_thread_id).await;
        }
        if let Some(thread_id) = self.chat_widget.thread_id() {
            if let Err(err) = app_server.thread_unsubscribe(thread_id).await {
                tracing::warn!("failed to unsubscribe thread {thread_id}: {err}");
            }
            self.abort_thread_event_listener(thread_id);
        }
    }

    pub(super) fn abort_thread_event_listener(&mut self, thread_id: ThreadId) {
        if let Some(handle) = self.thread_event_listener_tasks.remove(&thread_id) {
            handle.abort();
        }
    }

    pub(super) fn abort_all_thread_event_listeners(&mut self) {
        for handle in self
            .thread_event_listener_tasks
            .drain()
            .map(|(_, handle)| handle)
        {
            handle.abort();
        }
    }

    pub(crate) fn ensure_thread_channel(&mut self, thread_id: ThreadId) -> &mut ThreadEventChannel {
        self.thread_event_channels
            .entry(thread_id)
            .or_insert_with(|| ThreadEventChannel::new(THREAD_EVENT_CHANNEL_CAPACITY))
    }

    pub(crate) fn thread_has_live_session(&self, thread_id: ThreadId) -> bool {
        self.primary_thread_id == Some(thread_id)
            || self
                .thread_event_channels
                .get(&thread_id)
                .is_some_and(|channel| channel.attachment() == ThreadEventAttachment::Live)
    }

    pub(crate) async fn loaded_thread_rollout_paths(&self) -> Vec<PathBuf> {
        let mut rollout_paths = Vec::new();
        if let Some(path) = self
            .primary_session_configured
            .as_ref()
            .and_then(|session| session.rollout_path.clone())
        {
            rollout_paths.push(path);
        }
        for channel in self.thread_event_channels.values() {
            let store = channel.store.lock().await;
            if let Some(path) = store
                .session
                .as_ref()
                .and_then(|session| session.rollout_path.clone())
            {
                rollout_paths.push(path);
            }
        }
        rollout_paths
    }

    pub(super) async fn set_thread_active(&mut self, thread_id: ThreadId, active: bool) {
        if let Some(channel) = self.thread_event_channels.get_mut(&thread_id) {
            let mut store = channel.store.lock().await;
            store.active = active;
        }
    }

    pub(super) async fn activate_thread_channel(&mut self, thread_id: ThreadId) {
        if self.active_thread_id.is_some() {
            return;
        }
        self.set_thread_active(thread_id, /*active*/ true).await;
        let receiver = if let Some(channel) = self.thread_event_channels.get_mut(&thread_id) {
            channel.receiver.take()
        } else {
            None
        };
        self.active_thread_id = Some(thread_id);
        self.active_thread_rx = receiver;
        self.refresh_pending_thread_approvals().await;
    }

    pub(crate) async fn store_active_thread_receiver(&mut self) {
        let Some(active_id) = self.active_thread_id else {
            return;
        };
        let input_state = self.chat_widget.capture_thread_input_state();
        if let Some(channel) = self.thread_event_channels.get_mut(&active_id) {
            let receiver = self.active_thread_rx.take();
            let mut store = channel.store.lock().await;
            store.active = false;
            store.input_state = input_state;
            if let Some(receiver) = receiver {
                channel.receiver = Some(receiver);
            }
        }
    }

    pub(crate) async fn detach_active_thread_for_external_pane(&mut self) {
        self.store_active_thread_receiver().await;
        self.active_thread_id = None;
        self.active_thread_rx = None;
        self.refresh_pending_thread_approvals().await;
    }

    pub(super) async fn activate_thread_for_replay(
        &mut self,
        thread_id: ThreadId,
    ) -> Option<(mpsc::Receiver<ThreadBufferedEvent>, ThreadEventSnapshot)> {
        let channel = self.thread_event_channels.get_mut(&thread_id)?;
        let receiver = channel.receiver.take()?;
        let mut store = channel.store.lock().await;
        store.active = true;
        let snapshot = store.snapshot();
        Some((receiver, snapshot))
    }

    pub(super) async fn clear_active_thread(&mut self) {
        if let Some(active_id) = self.active_thread_id.take() {
            self.set_thread_active(active_id, /*active*/ false).await;
        }
        self.active_thread_rx = None;
        self.refresh_pending_thread_approvals().await;
    }

    pub(super) async fn note_thread_outbound_op(&mut self, thread_id: ThreadId, op: &AppCommand) {
        let Some(channel) = self.thread_event_channels.get(&thread_id) else {
            return;
        };
        let mut store = channel.store.lock().await;
        store.note_outbound_op(op);
    }

    pub(super) async fn note_active_thread_outbound_op(&mut self, op: &AppCommand) {
        if !ThreadEventStore::op_can_change_pending_replay_state(op) {
            return;
        }
        let Some(thread_id) = self.active_thread_id else {
            return;
        };
        self.note_thread_outbound_op(thread_id, op).await;
    }

    pub(super) async fn active_turn_id_for_thread(&self, thread_id: ThreadId) -> Option<String> {
        let channel = self.thread_event_channels.get(&thread_id)?;
        let store = channel.store.lock().await;
        store.active_turn_id().map(ToOwned::to_owned)
    }

    pub(super) async fn active_turn_id_for_submission(
        &mut self,
        thread_id: ThreadId,
    ) -> Option<String> {
        let active_turn_id = self.active_turn_id_for_thread(thread_id).await?;
        if self.active_thread_id == Some(thread_id) && !self.chat_widget.visible_task_running() {
            tracing::warn!(
                thread_id = %thread_id,
                turn_id = active_turn_id,
                "clearing stale active turn id for visibly idle thread"
            );
            if let Some(channel) = self.thread_event_channels.get(&thread_id) {
                let mut store = channel.store.lock().await;
                store.clear_active_turn_id();
            }
            return None;
        }
        Some(active_turn_id)
    }

    pub(crate) fn thread_label(&self, thread_id: ThreadId) -> String {
        let is_primary = self.primary_thread_id == Some(thread_id);
        let fallback_label = if is_primary {
            "Main [default]".to_string()
        } else {
            let thread_id = thread_id.to_string();
            let short_id: String = thread_id.chars().take(8).collect();
            format!("Agent ({short_id})")
        };
        if let Some(entry) = self.agent_navigation.get(&thread_id) {
            let label = format_agent_picker_item_name(
                entry.agent_nickname.as_deref(),
                entry.agent_role.as_deref(),
                is_primary,
            );
            if label == "Agent" {
                let thread_id = thread_id.to_string();
                let short_id: String = thread_id.chars().take(8).collect();
                format!("{label} ({short_id})")
            } else {
                label
            }
        } else {
            fallback_label
        }
    }

    /// Returns the thread whose transcript is currently on screen.
    ///
    /// `active_thread_id` is the source of truth during steady state, but the widget can briefly
    /// lag behind thread bookkeeping during transitions. The footer label and adjacent-thread
    /// navigation both follow what the user is actually looking at, not whichever thread most
    /// recently began switching.
    pub(super) fn current_displayed_thread_id(&self) -> Option<ThreadId> {
        self.active_thread_id.or(self.chat_widget.thread_id())
    }

    pub(super) fn ignore_same_thread_resume(
        &mut self,
        target_session: &crate::resume_picker::SessionTarget,
    ) -> bool {
        if self.active_thread_id != Some(target_session.thread_id) {
            return false;
        };

        self.chat_widget.add_info_message(
            format!("Already viewing {}.", target_session.display_label()),
            /*hint*/ None,
        );
        true
    }

    /// Mirrors the visible thread into the contextual footer row.
    ///
    /// The footer sometimes shows ambient context instead of an instructional hint. In multi-agent
    /// sessions, that contextual row includes the currently viewed agent label. The label is
    /// intentionally hidden until there is more than one known thread so single-thread sessions do
    /// not spend footer space restating that the user is already on the main conversation.
    pub(crate) fn sync_active_agent_label(&mut self) {
        let active_claude_title = self.claude_panes.active_claude_pane_title();
        let label = active_claude_title
            .map(|title| format!("{title} pane"))
            .or_else(|| {
                self.agent_navigation
                    .active_agent_label(self.current_displayed_thread_id(), self.primary_thread_id)
            });
        self.chat_widget
            .set_active_external_model_display(self.claude_panes.active_claude_pane_model_label());
        self.chat_widget.set_active_agent_label(label);
        self.sync_side_thread_ui();
    }

    pub(super) async fn thread_cwd(&self, thread_id: ThreadId) -> Option<AbsolutePathBuf> {
        let channel = self.thread_event_channels.get(&thread_id)?;
        let store = channel.store.lock().await;
        store.session.as_ref().map(|session| session.cwd.clone())
    }

    async fn thread_file_change_changes(
        &self,
        thread_id: ThreadId,
        turn_id: &str,
        item_id: &str,
    ) -> Option<Vec<codex_app_server_protocol::FileUpdateChange>> {
        let channel = self.thread_event_channels.get(&thread_id)?;
        let store = channel.store.lock().await;
        store.file_change_changes(turn_id, item_id)
    }

    pub(super) async fn interactive_request_for_thread_request(
        &self,
        thread_id: ThreadId,
        request: &ServerRequest,
    ) -> std::io::Result<Option<ThreadInteractiveRequest>> {
        let thread_label = Some(self.thread_label(thread_id));
        Ok(match request {
            ServerRequest::CommandExecutionRequestApproval { params, .. } => {
                let network_approval_context = params.network_approval_context.clone();
                let additional_permissions = params.additional_permissions.clone();
                let proposed_execpolicy_amendment = params.proposed_execpolicy_amendment.clone();
                let proposed_network_policy_amendments =
                    params.proposed_network_policy_amendments.clone();
                let approval = ExecApprovalRequest {
                    thread_id,
                    thread_label,
                    id: params
                        .approval_id
                        .clone()
                        .unwrap_or_else(|| params.item_id.clone()),
                    environment_id: params.environment_id.clone(),
                    command: params
                        .command
                        .as_deref()
                        .map(split_command_string)
                        .unwrap_or_default(),
                    reason: params.reason.clone(),
                    available_decisions: params.available_decisions.clone().unwrap_or_else(|| {
                        default_exec_approval_decisions(
                            network_approval_context.as_ref(),
                            proposed_execpolicy_amendment.as_ref(),
                            proposed_network_policy_amendments.as_deref(),
                            additional_permissions.as_ref(),
                        )
                    }),
                    network_approval_context,
                    additional_permissions,
                };
                Some(ThreadInteractiveRequest::Approval(ApprovalRequest::Exec(
                    approval,
                )))
            }
            ServerRequest::FileChangeRequestApproval { params, .. } => Some(
                ThreadInteractiveRequest::Approval(ApprovalRequest::ApplyPatch(
                    ApplyPatchApprovalRequest {
                    thread_id,
                    thread_label,
                    id: params.item_id.clone(),
                    reason: params.reason.clone(),
                    cwd: self
                        .thread_cwd(thread_id)
                        .await
                        .unwrap_or_else(|| self.config.cwd.clone()),
                    changes: self
                        .thread_file_change_changes(thread_id, &params.turn_id, &params.item_id)
                        .await
                        .map(crate::app_server_approval_conversions::file_update_changes_to_display)
                        .unwrap_or_default(),
                    response_destination:
                        crate::approval_events::ApprovalResponseDestination::Thread,
                    },
                )),
            ),
            ServerRequest::McpServerElicitationRequest { request_id, params } => {
                if let Some(params) = AppLinkViewParams::from_url_app_server_request(
                    thread_id,
                    &params.server_name,
                    request_id.clone(),
                    &params.request,
                ) {
                    Some(ThreadInteractiveRequest::AppLink(params))
                } else if let Some(request) =
                    McpServerElicitationFormRequest::from_app_server_request(
                        thread_id,
                        request_id.clone(),
                        params,
                    )
                {
                    Some(ThreadInteractiveRequest::McpServerElicitation(request))
                } else {
                    match &params.request {
                        codex_app_server_protocol::McpServerElicitationRequest::Form {
                            message,
                            ..
                        } => Some(ThreadInteractiveRequest::Approval(
                            ApprovalRequest::McpElicitation(McpElicitationApprovalRequest {
                                thread_id,
                                thread_label,
                                server_name: params.server_name.clone(),
                                request_id: request_id.clone(),
                                message: message.clone(),
                            }),
                        )),
                        codex_app_server_protocol::McpServerElicitationRequest::OpenAiForm {
                            ..
                        }
                        | codex_app_server_protocol::McpServerElicitationRequest::Url { .. } => {
                            self.app_event_tx.resolve_elicitation(
                                thread_id,
                                params.server_name.clone(),
                                request_id.clone(),
                                codex_app_server_protocol::McpServerElicitationAction::Decline,
                                /*content*/ None,
                                /*meta*/ None,
                            );
                            None
                        }
                    }
                }
            }
            ServerRequest::PermissionsRequestApproval { params, .. } => {
                // TODO(anp): Remove this native-path localization error path once core permission
                // paths remain PathUri after crossing the app-server boundary.
                let permissions = params.permissions.clone().try_into().map_err(|err| {
                    std::io::Error::new(
                        std::io::ErrorKind::InvalidInput,
                        format!("failed to localize requested filesystem paths: {err}"),
                    )
                })?;
                Some(ThreadInteractiveRequest::Approval(
                    ApprovalRequest::Permissions(PermissionsApprovalRequest {
                        thread_id,
                        thread_label,
                        call_id: params.item_id.clone(),
                        environment_id: params.environment_id.clone(),
                        reason: params.reason.clone(),
                        permissions,
                    }),
                ))
            }
            _ => None,
        })
    }

    pub(super) fn push_thread_interactive_request(&mut self, request: ThreadInteractiveRequest) {
        match request {
            ThreadInteractiveRequest::AppLink(params) => {
                self.chat_widget.open_app_link_view(params);
            }
            ThreadInteractiveRequest::Approval(request) => {
                self.render_inactive_patch_preview(&request);
                self.chat_widget.push_approval_request(request);
            }
            ThreadInteractiveRequest::McpServerElicitation(request) => {
                self.chat_widget
                    .push_mcp_server_elicitation_request(request);
            }
        }
    }

    fn render_inactive_patch_preview(&mut self, request: &ApprovalRequest) {
        let ApprovalRequest::ApplyPatch(request) = request else {
            return;
        };
        if request.thread_label.is_none() || request.changes.is_empty() {
            return;
        }
        self.chat_widget
            .add_to_history(history_cell::new_patch_event(
                request.changes.clone(),
                &request.cwd,
            ));
    }

    pub(super) async fn pending_inactive_thread_requests(&self) -> Vec<(ThreadId, ServerRequest)> {
        let channels: Vec<(ThreadId, Arc<Mutex<ThreadEventStore>>)> = self
            .thread_event_channels
            .iter()
            .map(|(thread_id, channel)| (*thread_id, Arc::clone(&channel.store)))
            .collect();

        let mut requests = Vec::new();
        for (thread_id, store) in channels {
            if Some(thread_id) == self.active_thread_id {
                continue;
            }

            let store = store.lock().await;
            if store.side_parent_pending_status().is_none() {
                continue;
            }
            requests.extend(
                store
                    .pending_replay_requests()
                    .into_iter()
                    .map(|request| (thread_id, request)),
            );
        }
        requests
    }

    pub(super) async fn surface_pending_inactive_thread_interactive_requests(
        &mut self,
    ) -> Result<()> {
        if self.active_side_parent_thread_id().is_some() {
            return Ok(());
        }

        let requests = self.pending_inactive_thread_requests().await;
        for (thread_id, request) in requests {
            if let Some(request) = self
                .interactive_request_for_thread_request(thread_id, &request)
                .await?
            {
                self.push_thread_interactive_request(request);
            }
        }
        Ok(())
    }

    pub(super) async fn submit_active_thread_op(
        &mut self,
        app_server: &mut AppServerSession,
        op: AppCommand,
    ) -> Result<()> {
        // The selected user pane owns input routing. Claude panes are external
        // runners and do not require a native app-server thread to be active.
        // Route them before resolving the native thread so a failed or absent
        // native pane cannot swallow external-pane input.
        if self.try_submit_active_claude_pane_op(&op) {
            return Ok(());
        }
        let Some(thread_id) = self.active_thread_id else {
            self.chat_widget
                .add_error_message("No active thread is available.".to_string());
            return Ok(());
        };

        self.submit_thread_op(app_server, thread_id, op).await
    }

    pub(super) async fn submit_thread_op(
        &mut self,
        app_server: &mut AppServerSession,
        thread_id: ThreadId,
        op: AppCommand,
    ) -> Result<()> {
        crate::session_log::log_outbound_op(&op);

        if self
            .try_resolve_app_server_request(app_server, thread_id, &op)
            .await?
        {
            return Ok(());
        }

        if self
            .try_submit_active_thread_op_via_app_server(app_server, thread_id, &op)
            .await?
        {
            if ThreadEventStore::op_can_change_pending_replay_state(&op) {
                self.note_thread_outbound_op(thread_id, &op).await;
                self.refresh_pending_thread_approvals().await;
                self.refresh_side_parent_status_from_store(thread_id).await;
            }
            return Ok(());
        }

        self.chat_widget
            .add_error_message(format!("Not available in TUI yet for thread {thread_id}."));
        Ok(())
    }

    /// Persist prompt text in the local cross-session message history.
    pub(super) fn append_message_history_entry(&self, thread_id: ThreadId, text: String) {
        let history_config = codex_message_history::HistoryConfig::new(
            self.chat_widget.config_ref().codex_home.clone(),
            &self.chat_widget.config_ref().history,
        );
        tokio::spawn(async move {
            if let Err(err) =
                codex_message_history::append_entry(&text, thread_id, &history_config).await
            {
                tracing::warn!(
                    thread_id = %thread_id,
                    error = %err,
                    "failed to append to message history"
                );
            }
        });
    }

    /// Fetch one local cross-session message history entry for the requesting thread.
    pub(super) async fn lookup_message_history_entry(
        &mut self,
        thread_id: ThreadId,
        offset: usize,
        log_id: u64,
    ) -> Result<()> {
        let history_config = codex_message_history::HistoryConfig::new(
            self.chat_widget.config_ref().codex_home.clone(),
            &self.chat_widget.config_ref().history,
        );
        let app_event_tx = self.app_event_tx.clone();
        tokio::spawn(async move {
            let entry_opt = tokio::task::spawn_blocking(move || {
                codex_message_history::lookup(log_id, offset, &history_config)
            })
            .await
            .unwrap_or_else(|err| {
                tracing::warn!(error = %err, "history lookup task failed");
                None
            });

            app_event_tx.send(AppEvent::ThreadHistoryEntryResponse {
                thread_id,
                event: HistoryLookupResponse::Entry {
                    offset,
                    log_id,
                    entry: entry_opt.map(|entry| entry.text),
                },
            });
        });
        Ok(())
    }

    /// Fetch one bounded local cross-session history batch for the requesting thread.
    pub(super) async fn lookup_message_history_batch(
        &mut self,
        thread_id: ThreadId,
        cursor: codex_message_history::HistoryBatchCursor,
        log_id: u64,
    ) -> Result<()> {
        let history_config = codex_message_history::HistoryConfig::new(
            self.chat_widget.config_ref().codex_home.clone(),
            &self.chat_widget.config_ref().history,
        );
        let app_event_tx = self.app_event_tx.clone();
        tokio::spawn(async move {
            let event = match tokio::task::spawn_blocking(move || {
                codex_message_history::lookup_batch(log_id, cursor, &history_config)
            })
            .await
            {
                Ok(Ok(batch)) => {
                    let entries = batch
                        .entries
                        .into_iter()
                        .map(|entry| crate::app_event::HistoryBatchEntryResponse {
                            offset: entry.offset,
                            entry: entry.entry.map(|entry| entry.text),
                        })
                        .collect();
                    HistoryLookupResponse::Batch {
                        cursor,
                        log_id,
                        entries,
                        next_older_cursor: batch.next_older_cursor,
                    }
                }
                Ok(Err(err)) => {
                    tracing::warn!(error = %err, "history batch lookup failed");
                    HistoryLookupResponse::BatchError { cursor, log_id }
                }
                Err(err) => {
                    tracing::warn!(error = %err, "history batch lookup task failed");
                    HistoryLookupResponse::BatchError { cursor, log_id }
                }
            };

            app_event_tx.send(AppEvent::ThreadHistoryEntryResponse { thread_id, event });
        });
        Ok(())
    }

    pub(super) async fn try_submit_active_thread_op_via_app_server(
        &mut self,
        app_server: &mut AppServerSession,
        thread_id: ThreadId,
        op: &AppCommand,
    ) -> Result<bool> {
        match op {
            AppCommand::Interrupt => {
                let mut turn_id = self
                    .active_turn_id_for_thread(thread_id)
                    .await
                    .unwrap_or_default();
                let (thread_event_tx, thread_event_store) = {
                    let channel = self.ensure_thread_channel(thread_id);
                    (channel.sender.clone(), Arc::clone(&channel.store))
                };
                self.reset_backtrack_state();
                if !turn_id.is_empty() {
                    let mut store = thread_event_store.lock().await;
                    if store.pending_interrupt_turn_id.as_deref() == Some(turn_id.as_str()) {
                        return Ok(true);
                    }
                    store.pending_interrupt_turn_id = Some(turn_id.clone());
                }
                let request_handle = app_server.request_handle();
                let request_ids = [app_server.next_request_id(), app_server.next_request_id()];
                tokio::spawn(async move {
                    for (attempt, request_id) in request_ids.into_iter().enumerate() {
                        let result = request_handle
                            .request_typed::<TurnInterruptResponse>(ClientRequest::TurnInterrupt {
                                request_id,
                                params: TurnInterruptParams {
                                    thread_id: thread_id.to_string(),
                                    turn_id: turn_id.clone(),
                                },
                            })
                            .await;

                        match result {
                            Ok(_) => break,
                            Err(error) => {
                                if attempt == 0
                                    && let Some(actual_turn_id) = active_turn_interrupt_race(&error)
                                    && actual_turn_id != turn_id
                                {
                                    thread_event_store.lock().await.pending_interrupt_turn_id =
                                        Some(actual_turn_id.clone());
                                    turn_id = actual_turn_id;
                                    continue;
                                }
                                tracing::warn!(error = %error, "turn/interrupt failed in TUI");
                                let notification =
                                    ServerNotification::Warning(WarningNotification {
                                        thread_id: Some(thread_id.to_string()),
                                        message: format!("Failed to interrupt turn: {error}"),
                                    });
                                let should_send = {
                                    let mut store = thread_event_store.lock().await;
                                    store.push_notification_ref(&notification);
                                    store.active
                                };
                                if should_send
                                    && let Err(error) = thread_event_tx
                                        .send(ThreadBufferedEvent::Notification(Box::new(
                                            notification,
                                        )))
                                        .await
                                {
                                    tracing::warn!(error = %error, "thread event channel closed");
                                }
                                break;
                            }
                        }
                    }
                });
                Ok(true)
            }
            AppCommand::UserTurn {
                items,
                cwd,
                approval_policy,
                approvals_reviewer,
                active_permission_profile,
                model,
                effort,
                summary,
                service_tier,
                final_output_json_schema,
                collaboration_mode,
                personality,
            } => {
                let mut should_start_turn = true;
                if let Some(turn_id) = self.active_turn_id_for_submission(thread_id).await {
                    let mut steer_turn_id = turn_id;
                    let mut retried_after_turn_mismatch = false;
                    loop {
                        match app_server
                            .turn_steer(
                                thread_id,
                                steer_turn_id.clone(),
                                items.to_vec(),
                                /*client_user_message_id*/ None,
                            )
                            .await
                        {
                            Ok(_) => return Ok(true),
                            Err(error) => {
                                if let Some(turn_error) =
                                    active_turn_not_steerable_turn_error(&error)
                                {
                                    if !self.chat_widget.enqueue_rejected_steer() {
                                        self.chat_widget.add_error_message(turn_error.message);
                                    }
                                    return Ok(true);
                                }
                                match active_turn_steer_race(&error) {
                                    Some(ActiveTurnSteerRace::Missing) => {
                                        if let Some(channel) =
                                            self.thread_event_channels.get(&thread_id)
                                        {
                                            let mut store = channel.store.lock().await;
                                            store.clear_active_turn_id();
                                        }
                                        should_start_turn = true;
                                        break;
                                    }
                                    Some(ActiveTurnSteerRace::ExpectedTurnMismatch {
                                        actual_turn_id,
                                    }) if !retried_after_turn_mismatch
                                        && actual_turn_id != steer_turn_id =>
                                    {
                                        // Review flows can swap the active turn before the TUI
                                        // processes the corresponding notification. Retry once with
                                        // the server-reported turn id so non-steerable review turns
                                        // still fall through to the existing queueing behavior.
                                        if let Some(channel) =
                                            self.thread_event_channels.get(&thread_id)
                                        {
                                            let mut store = channel.store.lock().await;
                                            store.active_turn_id = Some(actual_turn_id.clone());
                                        }
                                        steer_turn_id = actual_turn_id;
                                        retried_after_turn_mismatch = true;
                                    }
                                    Some(ActiveTurnSteerRace::ExpectedTurnMismatch {
                                        actual_turn_id,
                                    }) => {
                                        if let Some(channel) =
                                            self.thread_event_channels.get(&thread_id)
                                        {
                                            let mut store = channel.store.lock().await;
                                            store.active_turn_id = Some(actual_turn_id);
                                        }
                                        self.note_thread_steer_failure(thread_id, error);
                                        return Ok(true);
                                    }
                                    None => {
                                        self.note_thread_steer_failure(thread_id, error);
                                        return Ok(true);
                                    }
                                }
                            }
                        }
                    }
                }
                if should_start_turn {
                    if self
                        .pending_permission_confirmation
                        .as_ref()
                        .is_some_and(|pending| pending.thread_id == thread_id)
                    {
                        self.chat_widget.defer_turn_for_permission_confirmation();
                        return Ok(true);
                    }
                    let config = self.chat_widget.config_ref();
                    let approvals_reviewer =
                        approvals_reviewer.unwrap_or(config.approvals_reviewer);
                    let permissions_override = Self::turn_permissions_override_from_config(
                        config,
                        active_permission_profile.as_ref(),
                        self.runtime_permission_profile_override
                            .as_ref()
                            .map(|profile| &profile.permission_profile),
                    );
                    let response = app_server
                        .turn_start(
                            thread_id,
                            items.to_vec(),
                            cwd.clone(),
                            *approval_policy,
                            approvals_reviewer,
                            permissions_override,
                            config.permissions.user_visible_workspace_roots(),
                            model.to_string(),
                            effort.clone(),
                            *summary,
                            service_tier.clone(),
                            collaboration_mode.clone(),
                            *personality,
                            final_output_json_schema.clone(),
                            self.spawn_additional_context_for_thread(thread_id),
                            /*client_user_message_id*/ None,
                        )
                        .await?;
                    match response {
                        crate::app_server_session::TurnStartOutcome::Started {
                            turn_id,
                            recovered_failures,
                        } => {
                            if !recovered_failures.is_empty() {
                                self.surface_turn_start_failure(
                                    thread_id,
                                    recovered_failures.join("\n"),
                                    /*will_retry*/ true,
                                )
                                .await;
                            }
                            if self.active_thread_id == Some(thread_id)
                                && self.chat_widget.thread_id() == Some(thread_id)
                            {
                                self.chat_widget.record_safety_buffering_turn(turn_id, op);
                            }
                        }
                        crate::app_server_session::TurnStartOutcome::CapacityUnavailable {
                            max_threads,
                        } => {
                            self.chat_widget.add_error_message(format!(
                                "Cannot start turn: all {max_threads} execution slots are in use."
                            ));
                        }
                    }
                }
                Ok(true)
            }
            AppCommand::ListSkills { cwds, force_reload } => {
                self.refresh_skills(app_server, cwds.clone(), *force_reload);
                Ok(true)
            }
            AppCommand::Compact => {
                app_server.thread_compact_start(thread_id).await?;
                Ok(true)
            }
            AppCommand::SetThreadName { name } => {
                app_server
                    .thread_set_name(thread_id, name.to_string())
                    .await?;
                Ok(true)
            }
            AppCommand::Review { target } => {
                let response = app_server.review_start(thread_id, target.clone()).await?;
                let review_thread_id = ThreadId::from_string(&response.review_thread_id)
                    .wrap_err("review/start returned invalid review thread id")?;
                let store = Arc::clone(&self.ensure_thread_channel(review_thread_id).store);
                let mut store = store.lock().await;
                store.active_turn_id = Some(response.turn.id);
                Ok(true)
            }
            AppCommand::CleanBackgroundTerminals => {
                app_server
                    .thread_background_terminals_clean(thread_id)
                    .await?;
                Ok(true)
            }
            AppCommand::RunUserShellCommand { command } => {
                app_server
                    .thread_shell_command(thread_id, command.to_string())
                    .await?;
                Ok(true)
            }
            AppCommand::ReloadUserConfig => {
                app_server.reload_user_config().await?;
                Ok(true)
            }
            AppCommand::OverrideTurnContext { .. } => {
                self.sync_override_turn_context_settings(app_server, thread_id, op)
                    .await;
                Ok(true)
            }
            AppCommand::ApproveGuardianDeniedAction { event } => {
                app_server
                    .thread_approve_guardian_denied_action(thread_id, event)
                    .await?;
                Ok(true)
            }
            _ => Ok(false),
        }
    }

    fn turn_permissions_override_from_config(
        config: &Config,
        active_permission_profile: Option<&ActivePermissionProfile>,
        runtime_permission_profile_override: Option<&PermissionProfile>,
    ) -> TurnPermissionsOverride {
        if let Some(active_permission_profile) = active_permission_profile {
            return TurnPermissionsOverride::ActiveProfile(active_permission_profile.clone());
        }

        let effective_permission_profile = config.permissions.effective_permission_profile();
        let runtime_permission_profile_override =
            runtime_permission_profile_override.map(|profile| {
                profile
                    .clone()
                    .materialize_project_roots_with_workspace_roots(
                        &config.effective_workspace_roots(),
                    )
            });
        if runtime_permission_profile_override
            .as_ref()
            .is_some_and(|profile| profile == &effective_permission_profile)
        {
            return TurnPermissionsOverride::LegacySandbox(effective_permission_profile);
        }

        TurnPermissionsOverride::Preserve
    }

    pub(super) fn handle_skills_list_result(
        &mut self,
        result: Result<SkillsListResponse>,
        failure_message: &str,
    ) {
        match result {
            Ok(response) => self.handle_skills_list_response(response),
            Err(err) => {
                tracing::warn!("{failure_message}: {err:#}");
                self.chat_widget
                    .add_error_message(format!("{failure_message}: {err:#}"));
            }
        }
    }

    pub(super) async fn try_resolve_app_server_request(
        &mut self,
        app_server: &AppServerSession,
        thread_id: ThreadId,
        op: &AppCommand,
    ) -> Result<bool> {
        let Some(resolution) = self
            .pending_app_server_requests
            .take_resolution(op)
            .map_err(|err| color_eyre::eyre::eyre!(err))?
        else {
            return Ok(false);
        };

        match app_server
            .resolve_server_request(resolution.request_id, resolution.result)
            .await
        {
            Ok(()) => {
                if ThreadEventStore::op_can_change_pending_replay_state(op) {
                    self.note_thread_outbound_op(thread_id, op).await;
                    self.refresh_pending_thread_approvals().await;
                    self.refresh_side_parent_status_from_store(thread_id).await;
                }
                Ok(true)
            }
            Err(err) => {
                self.chat_widget.add_error_message(format!(
                    "Failed to resolve app-server request for thread {thread_id}: {err}"
                ));
                Ok(false)
            }
        }
    }

    pub(super) async fn refresh_pending_thread_approvals(&mut self) {
        let side_parent_thread_id = self.active_side_parent_thread_id();
        let channels: Vec<(ThreadId, Arc<Mutex<ThreadEventStore>>)> = self
            .thread_event_channels
            .iter()
            .map(|(thread_id, channel)| (*thread_id, Arc::clone(&channel.store)))
            .collect();

        let mut pending_thread_ids = Vec::new();
        for (thread_id, store) in channels {
            if Some(thread_id) == self.active_thread_id || Some(thread_id) == side_parent_thread_id
            {
                continue;
            }

            let store = store.lock().await;
            if store.has_pending_thread_approvals() {
                pending_thread_ids.push(thread_id);
            }
        }

        pending_thread_ids.sort_by_key(ThreadId::to_string);

        let threads = pending_thread_ids
            .into_iter()
            .map(|thread_id| self.thread_label(thread_id))
            .collect();

        self.chat_widget.set_pending_thread_approvals(threads);
    }

    pub(super) async fn refresh_side_parent_status_from_store(&mut self, thread_id: ThreadId) {
        let Some(channel) = self.thread_event_channels.get(&thread_id) else {
            return;
        };
        let status = {
            let store = channel.store.lock().await;
            store.side_parent_pending_status()
        };
        if let Some(status) = status {
            self.set_side_parent_status(thread_id, Some(status));
        } else {
            self.clear_side_parent_action_status(thread_id);
        }
    }

    pub(super) async fn enqueue_thread_notification(
        &mut self,
        thread_id: ThreadId,
        notification: ServerNotification,
    ) -> Result<()> {
        if self.abandoned_side_threads.contains(&thread_id) {
            return Ok(());
        }
        if matches!(notification, ServerNotification::ThreadSettingsUpdated(_))
            && self.primary_thread_id.is_some()
            && self.primary_thread_id != Some(thread_id)
            && !self.thread_event_channels.contains_key(&thread_id)
        {
            return Ok(());
        }
        if let ServerNotification::ThreadSettingsUpdated(notification) = &notification {
            self.apply_thread_settings_to_cached_session(thread_id, &notification.thread_settings)
                .await;
        }
        self.cache_collab_receiver_threads_for_notification(&notification);
        self.update_spawn_status_for_thread_notification(&notification);
        let inferred_session = self
            .infer_session_for_thread_notification(thread_id, &notification)
            .await;
        let is_turn_started = matches!(notification, ServerNotification::TurnStarted(_));
        let kill_switch_stopped = matches!(
            &notification,
            ServerNotification::Warning(warning)
                if warning.message == crate::legacy_core::security_revocation::KILL_SWITCH_STOPPED
        );
        let is_thread_closed = matches!(notification, ServerNotification::ThreadClosed(_));
        let notification_status_change = SideParentStatusChange::for_notification(&notification);
        let (sender, store) = {
            let channel = self.ensure_thread_channel(thread_id);
            (channel.sender.clone(), Arc::clone(&channel.store))
        };
        let (notification, pending_status, turn_stopped) = {
            let mut guard = store.lock().await;
            if guard.session.is_none()
                && let Some(session) = inferred_session
            {
                guard.session = Some(session);
            }
            let turn_stopped = match &notification {
                ServerNotification::TurnCompleted(notification) => {
                    guard.active_turn_id() == Some(notification.turn.id.as_str())
                }
                ServerNotification::ThreadClosed(_) => true,
                _ => false,
            };
            let notification = if guard.active {
                guard.push_notification_ref(&notification);
                Some(notification)
            } else {
                guard.push_notification(notification);
                None
            };
            (
                notification,
                guard.side_parent_pending_status(),
                turn_stopped,
            )
        };
        if is_turn_started || is_thread_closed {
            self.clear_kill_switch_held_native_task(thread_id);
        }
        if is_turn_started {
            self.agent_navigation.mark_running(thread_id);
        } else if turn_stopped {
            self.agent_navigation.mark_stopped(thread_id);
        }
        if kill_switch_stopped && self.primary_thread_id != Some(thread_id) {
            self.note_kill_switch_stopped_agent(thread_id);
        }

        if let Some(notification) = notification {
            match sender.try_send(ThreadBufferedEvent::Notification(Box::new(notification))) {
                Ok(()) => {}
                Err(TrySendError::Full(event)) => {
                    tokio::spawn(async move {
                        if let Err(err) = sender.send(event).await {
                            tracing::warn!("thread {thread_id} event channel closed: {err}");
                        }
                    });
                }
                Err(TrySendError::Closed(_)) => {
                    tracing::warn!("thread {thread_id} event channel closed");
                }
            }
        }
        if let Some(status) = pending_status {
            self.set_side_parent_status(thread_id, Some(status));
        } else if let Some(change) = notification_status_change {
            self.apply_side_parent_status_change(thread_id, change);
        }
        self.refresh_pending_thread_approvals().await;
        Ok(())
    }

    /// #867: the security kill switch stopped a sub-agent's turn (its own session says so in its
    /// thread). The agent list shows it as stopped, and the thread on screen, when it is another
    /// one (the parent waiting for it), names it.
    fn note_kill_switch_stopped_agent(&mut self, thread_id: ThreadId) {
        self.agent_navigation.mark_kill_switch_stopped(thread_id);
        if self.active_thread_id == Some(thread_id) {
            return;
        }
        let label = self.thread_label(thread_id);
        self.chat_widget.add_info_message(
            format!(
                "The security kill switch also stopped sub-agent {label} (thread {thread_id}) \
                 and the commands it was running."
            ),
            Some(
                "/agent lists it as stopped by the kill switch; open it there to see its thread."
                    .to_string(),
            ),
        );
    }

    /// Locally remembers receiver threads referenced by a collab notification.
    ///
    /// This intentionally avoids app-server reads on the active-thread rendering path. During large
    /// fan-outs the app-server can be saturated with spawn work, and blocking here would freeze the
    /// TUI event loop. Metadata from `ThreadStarted` or explicit picker refreshes still fills in
    /// names and roles later; until then, rendering falls back to the thread id.
    pub(super) fn cache_collab_receiver_threads_for_notification(
        &mut self,
        notification: &ServerNotification,
    ) {
        if let Some(activity) =
            sub_agent_activity_item(notification).and_then(sub_agent_activity_display)
        {
            self.agent_navigation.record_sub_agent_activity(activity);
            self.sync_active_agent_label();
            return;
        }

        let sender_thread_id = collab_sender_thread_id(notification);
        let Some(receiver_thread_ids) = collab_receiver_thread_ids(notification) else {
            return;
        };

        let mut parent_map_changed = false;
        for receiver_thread_id in receiver_thread_ids {
            if collab_receiver_is_not_found(notification, receiver_thread_id) {
                continue;
            }

            let Ok(thread_id) = ThreadId::from_string(receiver_thread_id) else {
                tracing::warn!(
                    thread_id = receiver_thread_id,
                    "ignoring collab receiver with invalid thread id during local caching"
                );
                continue;
            };

            if let Some(sender_thread_id) = sender_thread_id {
                self.spawn_parent_by_thread
                    .insert(thread_id, sender_thread_id);
                self.spawn_parent_by_node.insert(
                    crate::spawn_orchestration::thread_node_id(thread_id),
                    crate::spawn_orchestration::thread_node_id(sender_thread_id),
                );
                parent_map_changed = true;
            }

            if let Some(status) = collab_receiver_status(notification, receiver_thread_id)
                && should_apply_collab_receiver_status(
                    self.spawn_status_by_thread.get(&thread_id),
                    status,
                )
            {
                let is_running = matches!(
                    status.status,
                    codex_app_server_protocol::CollabAgentStatus::PendingInit
                        | codex_app_server_protocol::CollabAgentStatus::Running
                );
                self.spawn_status_by_thread
                    .insert(thread_id, status.clone());
                self.agent_navigation.set_running(thread_id, is_running);
                self.agent_navigation
                    .set_last_result_message(thread_id, status.message.clone());
            }

            if self.agent_navigation.get(&thread_id).is_some() {
                continue;
            }

            self.upsert_agent_picker_thread(
                thread_id, /*agent_nickname*/ None, /*agent_role*/ None,
                /*is_closed*/ false,
            );
        }
        if parent_map_changed {
            self.persist_pane_state();
        }
    }

    pub(super) async fn infer_session_for_thread_notification(
        &mut self,
        thread_id: ThreadId,
        notification: &ServerNotification,
    ) -> Option<ThreadSessionState> {
        let ServerNotification::ThreadStarted(notification) = notification else {
            return None;
        };
        let mut session = self.primary_session_configured.clone()?;
        session.thread_id = thread_id;
        session.thread_name = notification.thread.name.clone();
        session.model_provider_id = notification.thread.model_provider.clone();
        session
            .set_cwd_retargeting_implicit_runtime_workspace_root(notification.thread.cwd.clone());
        let rollout_path = notification.thread.path.clone();
        if let Some(model) =
            read_session_model(self.state_db.as_deref(), thread_id, rollout_path.as_deref()).await
        {
            session.model = model;
        } else if rollout_path.is_some() {
            session.model.clear();
        }
        session.message_history = None;
        session.rollout_path = rollout_path;
        self.upsert_agent_picker_thread(
            thread_id,
            notification.thread.agent_nickname.clone(),
            notification.thread.agent_role.clone(),
            /*is_closed*/ false,
        );
        self.agent_navigation
            .set_model(thread_id, Some(session.model.clone()));
        Some(session)
    }

    pub(super) async fn enqueue_thread_request(
        &mut self,
        thread_id: ThreadId,
        request: ServerRequest,
    ) -> Result<()> {
        let inactive_interactive_request = if self.active_thread_id != Some(thread_id) {
            self.interactive_request_for_thread_request(thread_id, &request)
                .await?
        } else {
            None
        };
        let (sender, store) = {
            let channel = self.ensure_thread_channel(thread_id);
            (channel.sender.clone(), Arc::clone(&channel.store))
        };

        let (should_send, pending_status) = {
            let mut guard = store.lock().await;
            guard.push_request(request.clone());
            (guard.active, guard.side_parent_pending_status())
        };
        let request_status = SideParentStatus::for_request(&request);

        if should_send {
            match sender.try_send(ThreadBufferedEvent::Request(Box::new(request))) {
                Ok(()) => {}
                Err(TrySendError::Full(event)) => {
                    tokio::spawn(async move {
                        if let Err(err) = sender.send(event).await {
                            tracing::warn!("thread {thread_id} event channel closed: {err}");
                        }
                    });
                }
                Err(TrySendError::Closed(_)) => {
                    tracing::warn!("thread {thread_id} event channel closed");
                }
            }
        } else if self.active_side_parent_thread_id().is_none()
            && let Some(request) = inactive_interactive_request
        {
            self.push_thread_interactive_request(request);
        }
        if let Some(status) = pending_status.or(request_status) {
            self.set_side_parent_status(thread_id, Some(status));
        }
        self.refresh_pending_thread_approvals().await;
        Ok(())
    }

    pub(super) async fn enqueue_thread_history_entry_response(
        &mut self,
        thread_id: ThreadId,
        event: HistoryLookupResponse,
    ) -> Result<()> {
        let (sender, store) = {
            let channel = self.ensure_thread_channel(thread_id);
            (channel.sender.clone(), Arc::clone(&channel.store))
        };

        let should_send = {
            let mut guard = store.lock().await;
            let should_send = guard.active;
            // Active batch responses remain queued in the receiver across a concurrent detach, so
            // retaining another deep copy for thread replay only accumulates already-delivered
            // history data. Inactive responses still need the buffer because they are not sent.
            if !should_send || !matches!(&event, HistoryLookupResponse::Batch { .. }) {
                guard
                    .buffer
                    .push_back(ThreadBufferedEvent::HistoryEntryResponse(event.clone()));
                if guard.buffer.len() > guard.capacity
                    && let Some(removed) = guard.buffer.pop_front()
                    && let ThreadBufferedEvent::Request(request) = &removed
                {
                    guard
                        .pending_interactive_replay
                        .note_evicted_server_request(request.as_ref());
                }
            }
            should_send
        };

        if should_send {
            match sender.try_send(ThreadBufferedEvent::HistoryEntryResponse(event)) {
                Ok(()) => {}
                Err(TrySendError::Full(event)) => {
                    tokio::spawn(async move {
                        if let Err(err) = sender.send(event).await {
                            tracing::warn!("thread {thread_id} event channel closed: {err}");
                        }
                    });
                }
                Err(TrySendError::Closed(_)) => {
                    tracing::warn!("thread {thread_id} event channel closed");
                }
            }
        }
        Ok(())
    }

    pub(super) async fn enqueue_primary_thread_session(
        &mut self,
        session: ThreadSessionState,
        turns: Vec<Turn>,
    ) -> Result<()> {
        self.enqueue_primary_thread_session_with_presentation(
            session,
            turns,
            ThreadAttachPresentation::SessionLineage,
        )
        .await
    }

    pub(super) async fn enqueue_primary_thread_session_with_presentation(
        &mut self,
        session: ThreadSessionState,
        turns: Vec<Turn>,
        presentation: ThreadAttachPresentation,
    ) -> Result<()> {
        let thread_id = session.thread_id;
        self.primary_thread_id = Some(thread_id);
        self.primary_session_configured = Some(session.clone());
        self.upsert_agent_picker_thread(
            thread_id, /*agent_nickname*/ None, /*agent_role*/ None,
            /*is_closed*/ false,
        );
        let channel = self.ensure_thread_channel(thread_id);
        {
            let mut store = channel.store.lock().await;
            store.set_session(session.clone(), turns.clone());
        }
        self.activate_thread_channel(thread_id).await;
        self.chat_widget
            .set_initial_user_message_submit_suppressed(/*suppressed*/ true);
        match presentation {
            ThreadAttachPresentation::SessionLineage => {
                self.chat_widget.handle_thread_session(session);
            }
            ThreadAttachPresentation::PromptEdit => {
                self.chat_widget.handle_prompt_edit_thread_session(session);
            }
        }
        let should_buffer_initial_replay = !turns.is_empty();
        if should_buffer_initial_replay {
            self.app_event_tx
                .send(AppEvent::BeginInitialHistoryReplayBuffer);
        }
        self.chat_widget
            .replay_thread_turns(turns, ReplayKind::ResumeInitialMessages);
        if should_buffer_initial_replay {
            self.app_event_tx
                .send(AppEvent::EndInitialHistoryReplayBuffer);
        }
        if matches!(presentation, ThreadAttachPresentation::PromptEdit) {
            self.chat_widget.emit_prompt_edit_thread_event();
        }
        let pending = std::mem::take(&mut self.pending_primary_events);
        for pending_event in pending {
            match pending_event {
                ThreadBufferedEvent::Notification(notification) => {
                    self.enqueue_thread_notification(thread_id, *notification)
                        .await?;
                }
                ThreadBufferedEvent::Request(request) => {
                    self.enqueue_thread_request(thread_id, *request).await?;
                }
                ThreadBufferedEvent::HistoryEntryResponse(event) => {
                    self.enqueue_thread_history_entry_response(thread_id, event)
                        .await?;
                }
                ThreadBufferedEvent::FeedbackSubmission(event) => {
                    self.enqueue_thread_feedback_event(thread_id, event).await;
                }
            }
        }
        self.chat_widget
            .set_initial_user_message_submit_suppressed(/*suppressed*/ false);
        self.chat_widget.submit_initial_user_message_if_pending();
        Ok(())
    }

    pub(super) async fn enqueue_primary_thread_notification(
        &mut self,
        notification: ServerNotification,
    ) -> Result<()> {
        if let Some(thread_id) = self.primary_thread_id {
            return self
                .enqueue_thread_notification(thread_id, notification)
                .await;
        }
        self.pending_primary_events
            .push_back(ThreadBufferedEvent::Notification(Box::new(notification)));
        Ok(())
    }

    pub(super) async fn enqueue_primary_thread_request(
        &mut self,
        request: ServerRequest,
    ) -> Result<()> {
        if let Some(thread_id) = self.primary_thread_id {
            return self.enqueue_thread_request(thread_id, request).await;
        }
        self.pending_primary_events
            .push_back(ThreadBufferedEvent::Request(Box::new(request)));
        Ok(())
    }

    pub(super) async fn refresh_snapshot_session_if_needed(
        &mut self,
        app_server: &mut AppServerSession,
        thread_id: ThreadId,
        is_replay_only: bool,
        snapshot: &mut ThreadEventSnapshot,
    ) {
        if !self.should_refresh_snapshot_session(thread_id, is_replay_only, snapshot) {
            return;
        }

        match app_server
            .resume_thread(
                self.config.clone(),
                thread_id,
                self.resume_model_settings(),
                self.resume_permission_settings(),
            )
            .await
        {
            Ok(started) => {
                self.apply_refreshed_snapshot_thread(thread_id, started, snapshot)
                    .await
            }
            Err(err) => {
                tracing::warn!(
                    thread_id = %thread_id,
                    error = %err,
                    "failed to refresh inferred thread session before replay"
                );
            }
        }
    }

    pub(super) fn should_refresh_snapshot_session(
        &self,
        thread_id: ThreadId,
        is_replay_only: bool,
        snapshot: &ThreadEventSnapshot,
    ) -> bool {
        !is_replay_only
            && !self.side_threads.contains_key(&thread_id)
            && snapshot.session.as_ref().is_none_or(|session| {
                session.model.trim().is_empty() || session.rollout_path.is_none()
            })
    }

    pub(super) async fn apply_refreshed_snapshot_thread(
        &mut self,
        thread_id: ThreadId,
        started: AppServerStartedThread,
        snapshot: &mut ThreadEventSnapshot,
    ) {
        if started.blocks_direct_input {
            self.agent_navigation.mark_parent_owned(thread_id);
        }
        let AppServerStartedThread { session, turns, .. } = started;
        if let Some(channel) = self.thread_event_channels.get(&thread_id) {
            let mut store = channel.store.lock().await;
            store.set_session(session.clone(), turns.clone());
            store.rebase_buffer_after_session_refresh();
        }
        snapshot.session = Some(session);
        snapshot.turns = turns;
        snapshot
            .events
            .retain(ThreadEventStore::event_survives_session_refresh);
    }

    /// Opens the `/agent` picker after refreshing cached labels for known threads.
    ///
    /// The picker state is derived from long-lived thread channels plus best-effort metadata
    /// refreshes from the backend. Refresh failures are treated as "thread is only inspectable by
    /// historical id now" and converted into closed picker entries instead of deleting them, so
    /// the stable traversal order remains intact for review and keyboard navigation.
    pub(super) async fn drain_active_thread_events(&mut self, tui: &mut tui::Tui) -> Result<()> {
        let Some(mut rx) = self.active_thread_rx.take() else {
            return Ok(());
        };

        let mut disconnected = false;
        loop {
            match rx.try_recv() {
                Ok(event) => self.handle_thread_event_now(event),
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => {
                    disconnected = true;
                    break;
                }
            }
        }

        if !disconnected {
            self.active_thread_rx = Some(rx);
        } else {
            self.clear_active_thread().await;
        }

        if self.backtrack_render_pending {
            tui.frame_requester().schedule_frame();
        }
        Ok(())
    }

    /// Returns `(closed_thread_id, primary_thread_id)` when a non-primary active
    /// thread has died and we should fail over to the primary thread.
    ///
    /// A user-requested shutdown (`ExitMode::ShutdownFirst`) sets
    /// `pending_shutdown_exit_thread_id`; matching shutdown completions are ignored
    /// here so Ctrl+C-like exits don't accidentally resurrect the main thread.
    ///
    /// Failover is only eligible when all of these are true:
    /// 1. the event is `thread/closed`;
    /// 2. the active thread differs from the primary thread;
    /// 3. the active thread is not the pending shutdown-exit thread.
    pub(super) fn active_non_primary_shutdown_target(
        &self,
        notification: &ServerNotification,
    ) -> Option<(ThreadId, ThreadId)> {
        if !matches!(notification, ServerNotification::ThreadClosed(_)) {
            return None;
        }
        let active_thread_id = self.active_thread_id?;
        let primary_thread_id = self.primary_thread_id?;
        if self.pending_shutdown_exit_thread_id == Some(active_thread_id) {
            return None;
        }
        (active_thread_id != primary_thread_id).then_some((active_thread_id, primary_thread_id))
    }

    pub(super) fn replay_thread_snapshot(
        &mut self,
        snapshot: ThreadEventSnapshot,
        resume_restored_queue: bool,
    ) {
        self.refresh_mcp_startup_expected_servers_from_config();
        let should_buffer_replay = !snapshot.turns.is_empty() || !snapshot.events.is_empty();
        if should_buffer_replay {
            self.app_event_tx
                .send(AppEvent::BeginThreadSwitchHistoryReplayBuffer);
        }
        let suppress_replay_notices =
            replay_filter::snapshot_has_pending_interactive_request(&snapshot);
        if let Some(session) = snapshot.session {
            if session.reasoning_effort != Some(ReasoningEffortConfig::Ultra) {
                self.chat_widget
                    .set_plan_mode_reasoning_effort(self.config.plan_mode_reasoning_effort.clone());
            }
            if self.side_threads.contains_key(&session.thread_id) {
                self.chat_widget.handle_side_thread_session(session);
            } else if suppress_replay_notices {
                self.chat_widget.handle_thread_session_quiet(session);
            } else {
                self.chat_widget.handle_thread_session(session);
            }
        }
        self.chat_widget
            .set_queue_autosend_suppressed(/*suppressed*/ true);
        self.chat_widget.restore_thread_input_state(
            snapshot.input_state,
            ThreadInputStateRestoreMode {
                preserve_in_flight_turn: true,
            },
        );
        if !snapshot.turns.is_empty() {
            self.chat_widget
                .replay_thread_turns(snapshot.turns, ReplayKind::ThreadSnapshot);
        }
        for event in snapshot.events {
            if suppress_replay_notices && replay_filter::event_is_notice(&event) {
                continue;
            }
            self.handle_thread_event_replay(event);
        }
        if should_buffer_replay {
            self.app_event_tx
                .send(AppEvent::EndInitialHistoryReplayBuffer);
        }
        self.chat_widget
            .set_queue_autosend_suppressed(/*suppressed*/ false);
        self.chat_widget
            .set_initial_user_message_submit_suppressed(/*suppressed*/ false);
        self.chat_widget.submit_initial_user_message_if_pending();
        if resume_restored_queue {
            self.chat_widget.maybe_send_next_queued_input();
        }
        self.refresh_status_line();
    }

    pub(super) fn should_wait_for_initial_session(session_selection: &SessionSelection) -> bool {
        matches!(
            session_selection,
            SessionSelection::StartFresh
                | SessionSelection::ResumePanesOnly { .. }
                | SessionSelection::Exit
        )
    }

    pub(super) fn should_prompt_for_paused_goal_after_startup_resume(
        session_selection: &SessionSelection,
        initial_prompt: &Option<String>,
        initial_images: &[PathBuf],
    ) -> bool {
        matches!(session_selection, SessionSelection::Resume(_))
            && initial_prompt.is_none()
            && initial_images.is_empty()
    }

    pub(super) fn should_handle_active_thread_events(
        waiting_for_initial_session_configured: bool,
        has_active_thread_receiver: bool,
    ) -> bool {
        has_active_thread_receiver && !waiting_for_initial_session_configured
    }

    pub(super) fn should_stop_waiting_for_initial_session(
        waiting_for_initial_session_configured: bool,
        primary_thread_id: Option<ThreadId>,
    ) -> bool {
        waiting_for_initial_session_configured && primary_thread_id.is_some()
    }

    #[allow(clippy::too_many_arguments)]
    pub(super) fn handle_skills_list_response(&mut self, response: SkillsListResponse) {
        let cwd = self.chat_widget.config_ref().cwd.clone();
        let errors = errors_for_cwd(&cwd, &response);
        let errors = self.skill_load_warnings.newly_active_errors(&errors);
        emit_skill_load_warnings(&self.app_event_tx, &errors);
        self.chat_widget.handle_skills_list_response(response);
    }

    pub(super) fn handle_thread_event_now(&mut self, event: ThreadBufferedEvent) {
        let needs_refresh = matches!(
            &event,
            ThreadBufferedEvent::Notification(notification)
                if matches!(
                    notification.as_ref(),
                    ServerNotification::TurnStarted(_)
                        | ServerNotification::ThreadTokenUsageUpdated(_)
                )
        );
        match event {
            ThreadBufferedEvent::Notification(notification) => {
                let settings_updated = matches!(
                    notification.as_ref(),
                    ServerNotification::ThreadSettingsUpdated(_)
                );
                self.update_spawn_status_for_thread_notification(notification.as_ref());
                self.cache_collab_receiver_threads_for_notification(notification.as_ref());
                self.chat_widget
                    .handle_server_notification(*notification, /*replay_kind*/ None);
                if settings_updated {
                    self.observe_permission_confirmation();
                }
            }
            ThreadBufferedEvent::Request(request) => {
                if self
                    .pending_app_server_requests
                    .contains_server_request(request.as_ref())
                {
                    self.chat_widget
                        .handle_server_request(*request, /*replay_kind*/ None);
                }
            }
            ThreadBufferedEvent::HistoryEntryResponse(event) => {
                self.chat_widget.handle_history_entry_response(event);
            }
            ThreadBufferedEvent::FeedbackSubmission(event) => {
                self.handle_feedback_thread_event(event);
            }
        }
        if needs_refresh {
            self.refresh_status_line();
        }
    }

    pub(super) fn update_spawn_status_for_thread_notification(
        &mut self,
        notification: &ServerNotification,
    ) {
        match notification {
            ServerNotification::ItemStarted(notification) => {
                if let codex_app_server_protocol::ThreadItem::CollabAgentToolCall {
                    id,
                    tool: codex_app_server_protocol::CollabAgentTool::Wait,
                    status: codex_app_server_protocol::CollabAgentToolCallStatus::InProgress,
                    ..
                } = &notification.item
                    && let Ok(thread_id) = ThreadId::from_string(&notification.thread_id)
                    && self.is_native_spawn_thread(thread_id)
                {
                    self.spawn_waiting_for_agents_by_thread
                        .insert(thread_id, (notification.turn_id.clone(), id.clone()));
                }
            }
            ServerNotification::ItemCompleted(notification) => {
                if let codex_app_server_protocol::ThreadItem::CollabAgentToolCall {
                    id,
                    tool: codex_app_server_protocol::CollabAgentTool::Wait,
                    ..
                } = &notification.item
                    && let Ok(thread_id) = ThreadId::from_string(&notification.thread_id)
                    && self
                        .spawn_waiting_for_agents_by_thread
                        .get(&thread_id)
                        .is_some_and(|(turn_id, call_id)| {
                            turn_id == &notification.turn_id && call_id == id
                        })
                {
                    self.spawn_waiting_for_agents_by_thread.remove(&thread_id);
                }
            }
            ServerNotification::TurnStarted(notification) => {
                if let Ok(thread_id) = ThreadId::from_string(&notification.thread_id) {
                    self.spawn_waiting_for_agents_by_thread.remove(&thread_id);
                }
            }
            ServerNotification::TurnCompleted(notification) => {
                if let Ok(thread_id) = ThreadId::from_string(&notification.thread_id) {
                    self.spawn_waiting_for_agents_by_thread.remove(&thread_id);
                }
            }
            ServerNotification::ThreadClosed(notification) => {
                if let Ok(thread_id) = ThreadId::from_string(&notification.thread_id) {
                    self.spawn_waiting_for_agents_by_thread.remove(&thread_id);
                }
            }
            _ => {}
        }
        if let ServerNotification::ThreadClosed(notification) = notification {
            let Ok(thread_id) = ThreadId::from_string(&notification.thread_id) else {
                return;
            };
            self.note_assignment_node_gone(&crate::spawn_orchestration::thread_node_id(thread_id));
            if !self.is_spawn_orchestration_thread(thread_id) {
                return;
            }
            self.spawn_status_by_thread.insert(
                thread_id,
                codex_app_server_protocol::CollabAgentState {
                    status: codex_app_server_protocol::CollabAgentStatus::Shutdown,
                    message: Some("thread closed".to_string()),
                },
            );
            self.agent_navigation.mark_closed(thread_id);
            self.persist_pane_state();
            return;
        }

        let (thread_id, status, message, should_process_terminal_side_effects) = match notification
        {
            ServerNotification::TurnStarted(notification) => {
                let Ok(thread_id) = ThreadId::from_string(&notification.thread_id) else {
                    return;
                };
                let node_key = self.spawn_orchestration_node_for_thread(thread_id);
                self.note_whip_target_started(&node_key);
                (
                    thread_id,
                    codex_app_server_protocol::CollabAgentStatus::Running,
                    None,
                    true,
                )
            }
            ServerNotification::TurnCompleted(notification) => {
                let Ok(thread_id) = ThreadId::from_string(&notification.thread_id) else {
                    return;
                };
                self.process_native_completed_turn_for_orchestration(thread_id, &notification.turn);
                let status = match notification.turn.status {
                    TurnStatus::Completed => {
                        codex_app_server_protocol::CollabAgentStatus::Completed
                    }
                    TurnStatus::Interrupted => {
                        codex_app_server_protocol::CollabAgentStatus::Interrupted
                    }
                    TurnStatus::Failed => codex_app_server_protocol::CollabAgentStatus::Errored,
                    TurnStatus::InProgress => codex_app_server_protocol::CollabAgentStatus::Running,
                };
                let terminal_key = (thread_id, notification.turn.id.clone());
                let should_process_terminal_side_effects = self
                    .spawn_processed_terminal_turns
                    .insert(terminal_key.clone());
                const PROCESSED_TERMINAL_TURN_LIMIT: usize = 4_096;
                if self.spawn_processed_terminal_turns.len() > PROCESSED_TERMINAL_TURN_LIMIT {
                    self.spawn_processed_terminal_turns.clear();
                    self.spawn_processed_terminal_turns.insert(terminal_key);
                }
                (
                    thread_id,
                    status,
                    spawn_turn_result_message(&notification.turn),
                    should_process_terminal_side_effects,
                )
            }
            ServerNotification::ItemCompleted(notification) => {
                match &notification.item {
                    codex_app_server_protocol::ThreadItem::CollabAgentToolCall {
                        tool: codex_app_server_protocol::CollabAgentTool::SendInput,
                        status: codex_app_server_protocol::CollabAgentToolCallStatus::Completed,
                        sender_thread_id,
                        receiver_thread_ids,
                        ..
                    } => self.note_native_collab_assignment_dispatch(
                        sender_thread_id,
                        receiver_thread_ids,
                    ),
                    // Multi-Agent v2 represents successful `send_message` and
                    // `followup_task` admission as a completed Interacted activity rather than
                    // the v1 SendInput item. Both are valid native assignment dispatches when
                    // the exact sender/receiver pair owns an armed assignment.
                    codex_app_server_protocol::ThreadItem::SubAgentActivity {
                        kind: codex_app_server_protocol::SubAgentActivityKind::Interacted,
                        agent_thread_id,
                        ..
                    } => self.note_native_collab_assignment_dispatch(
                        &notification.thread_id,
                        std::slice::from_ref(agent_thread_id),
                    ),
                    _ => {}
                }
                let Ok(thread_id) = ThreadId::from_string(&notification.thread_id) else {
                    return;
                };
                // Streaming deltas still never dispatch. A completed assistant message can
                // dispatch immediately so dispatch-then-wait works; interrupted/truncated turns
                // never emit ItemCompleted for that message, and TurnCompleted remains a deduped
                // catch-all for completed turns.
                if let codex_app_server_protocol::ThreadItem::AgentMessage {
                    id, text, phase, ..
                } = &notification.item
                    && !matches!(
                        phase,
                        Some(codex_protocol::models::MessagePhase::Commentary)
                    )
                {
                    let source_node_id = self.spawn_orchestration_node_for_thread(thread_id);
                    self.dispatch_orchestrate_blocks_from_text(&source_node_id, text);
                    let _ = id;
                }
                return;
            }
            // Deliberately NOT AgentMessageDelta: spawn task blocks must never dispatch from a
            ServerNotification::ThreadTokenUsageUpdated(notification) => {
                let Ok(thread_id) = ThreadId::from_string(&notification.thread_id) else {
                    return;
                };
                self.update_spawn_context_pressure_for_thread(thread_id, &notification.token_usage);
                return;
            }
            // Deliberately NOT AgentMessageDelta: spawn task blocks must never dispatch from a
            // streaming turn. Dispatch happens only from completed assistant-message items or from
            // TurnCompleted with a clean Completed status, so an interrupted or failed turn can
            // never fire a truncated pfterminal_send_task block.
            _ => return,
        };

        if !self.is_spawn_orchestration_thread(thread_id) {
            return;
        }

        let is_running = matches!(
            status,
            codex_app_server_protocol::CollabAgentStatus::PendingInit
                | codex_app_server_protocol::CollabAgentStatus::Running
        );
        // Preserve what this turn actually emitted for orchestration decisions. The synthetic
        // status text below is useful in the picker and child reports, but it must not make an
        // empty Manager completion look like visible provider output.
        let current_turn_message = message.clone();
        let message = if !is_running && message.as_deref().is_none_or(|text| text.trim().is_empty())
        {
            match &status {
                codex_app_server_protocol::CollabAgentStatus::Completed => {
                    Some("Turn completed without visible output.".to_string())
                }
                codex_app_server_protocol::CollabAgentStatus::Interrupted => {
                    Some("Turn interrupted without visible output.".to_string())
                }
                codex_app_server_protocol::CollabAgentStatus::Errored => {
                    Some("Turn failed without visible output.".to_string())
                }
                _ => message,
            }
        } else {
            message
        };
        self.spawn_status_by_thread.insert(
            thread_id,
            codex_app_server_protocol::CollabAgentState {
                status: status.clone(),
                message: message.clone(),
            },
        );
        self.agent_navigation.set_running(thread_id, is_running);
        if message.is_some() {
            self.agent_navigation
                .set_last_result_message(thread_id, message);
        }
        if !is_running {
            if matches!(
                status,
                codex_app_server_protocol::CollabAgentStatus::Shutdown
                    | codex_app_server_protocol::CollabAgentStatus::NotFound
            ) {
                self.note_assignment_node_gone(&crate::spawn_orchestration::thread_node_id(
                    thread_id,
                ));
            }
            let turn_succeeded = matches!(
                status,
                codex_app_server_protocol::CollabAgentStatus::Completed
            );
            let node_key = self.spawn_orchestration_node_for_thread(thread_id);
            if should_process_terminal_side_effects {
                self.note_whip_target_idle_with_fire_control(
                    &node_key,
                    current_turn_message.as_deref(),
                    /*allow_fire*/ true,
                    turn_succeeded,
                );
            }
        }
    }

    fn update_spawn_context_pressure_for_thread(
        &mut self,
        thread_id: ThreadId,
        token_usage: &codex_app_server_protocol::ThreadTokenUsage,
    ) {
        if !self.is_spawn_orchestration_thread(thread_id) {
            return;
        }
        let Some(context_left) = context_left_percent_from_token_usage(token_usage) else {
            return;
        };
        self.spawn_context_left_by_thread
            .insert(thread_id, context_left);
    }

    pub(super) fn handle_thread_event_replay(&mut self, event: ThreadBufferedEvent) {
        match event {
            ThreadBufferedEvent::Notification(notification) => self
                .chat_widget
                .handle_server_notification(*notification, Some(ReplayKind::ThreadSnapshot)),
            ThreadBufferedEvent::Request(request) => self
                .chat_widget
                .handle_server_request(*request, Some(ReplayKind::ThreadSnapshot)),
            ThreadBufferedEvent::HistoryEntryResponse(event) => {
                self.chat_widget.handle_history_entry_response(event)
            }
            ThreadBufferedEvent::FeedbackSubmission(event) => {
                self.handle_feedback_thread_event(event);
            }
        }
    }

    /// Handles an event emitted by the currently active thread.
    ///
    /// This function enforces shutdown intent routing: unexpected non-primary
    /// thread shutdowns fail over to the primary thread, while user-requested
    /// app exits consume only the tracked shutdown completion and then proceed.
    pub(super) async fn handle_active_thread_event(
        &mut self,
        tui: &mut tui::Tui,
        app_server: &mut AppServerSession,
        event: ThreadBufferedEvent,
    ) -> Result<()> {
        // Capture this before any potential thread switch: we only want to clear
        // the exit marker when the currently active thread acknowledges shutdown.
        let pending_shutdown_exit_completed = matches!(
            &event,
            ThreadBufferedEvent::Notification(notification)
                if matches!(notification.as_ref(), ServerNotification::ThreadClosed(_))
        ) && self.pending_shutdown_exit_thread_id
            == self.active_thread_id;

        // Processing order matters:
        //
        // 1. handle unexpected non-primary shutdown failover first;
        // 2. clear pending exit marker for matching shutdown;
        // 3. forward the event through normal handling.
        //
        // This preserves the mental model that user-requested exits do not trigger
        // failover, while true sub-agent deaths still do.
        if let ThreadBufferedEvent::Notification(notification) = &event
            && let Some((closed_thread_id, primary_thread_id)) =
                self.active_non_primary_shutdown_target(notification.as_ref())
        {
            self.mark_agent_picker_thread_closed(closed_thread_id);
            if self.side_threads.contains_key(&closed_thread_id) {
                self.discard_closed_side_thread(closed_thread_id).await;
                self.select_agent_thread(tui, app_server, primary_thread_id)
                    .await?;
            } else {
                self.select_agent_thread_and_discard_side(tui, app_server, primary_thread_id)
                    .await?;
            }
            if self.active_thread_id == Some(primary_thread_id) {
                self.chat_widget.add_info_message(
                    format!(
                        "Agent thread {closed_thread_id} closed. Switched back to main thread."
                    ),
                    /*hint*/ None,
                );
            } else {
                self.clear_active_thread().await;
                self.chat_widget.add_error_message(format!(
                    "Agent thread {closed_thread_id} closed. Failed to switch back to main thread {primary_thread_id}.",
                ));
            }
            return Ok(());
        }

        if pending_shutdown_exit_completed {
            // Clear only after seeing the shutdown completion for the tracked
            // thread, so unrelated shutdowns cannot consume this marker.
            self.pending_shutdown_exit_thread_id = None;
        }
        self.handle_thread_event_now(event);
        if self.backtrack_render_pending {
            tui.frame_requester().schedule_frame();
        }
        Ok(())
    }
}

fn spawn_turn_result_message(turn: &codex_app_server_protocol::Turn) -> Option<String> {
    let agent_text = turn.items.iter().rev().find_map(|item| match item {
        codex_app_server_protocol::ThreadItem::AgentMessage { text, phase, .. }
            if !matches!(
                phase,
                Some(codex_protocol::models::MessagePhase::Commentary)
            ) =>
        {
            let trimmed = text.trim();
            (!trimmed.is_empty()).then_some(trimmed)
        }
        _ => None,
    });

    let error_text = matches!(turn.status, TurnStatus::Failed | TurnStatus::Interrupted)
        .then(|| turn.error.as_ref())
        .flatten()
        .map(format_turn_error_for_spawn_report);

    let text = match (error_text, agent_text) {
        (Some(error), Some(agent_text)) => format!("{error}; result={agent_text}"),
        (Some(error), None) => error,
        (None, Some(agent_text)) => agent_text.to_string(),
        (None, None) => return None,
    };

    Some(crate::spawn_orchestration::bounded_spawn_report_value(
        &text,
    ))
}

fn format_turn_error_for_spawn_report(error: &AppServerTurnError) -> String {
    let mut text = format!("turn_error={}", error.message.trim());
    if let Some(info) = &error.codex_error_info {
        let _ = write!(text, "; info={info:?}");
    }
    if let Some(details) = error
        .additional_details
        .as_deref()
        .map(str::trim)
        .filter(|details| !details.is_empty())
    {
        let _ = write!(text, "; details={details}");
    }
    text
}

fn context_left_percent_from_token_usage(
    token_usage: &codex_app_server_protocol::ThreadTokenUsage,
) -> Option<i64> {
    let context_window = token_usage.model_context_window?;
    Some(
        TokenUsage {
            total_tokens: token_usage.last.total_tokens,
            input_tokens: token_usage.last.input_tokens,
            cached_input_tokens: token_usage.last.cached_input_tokens,
            output_tokens: token_usage.last.output_tokens,
            reasoning_output_tokens: token_usage.last.reasoning_output_tokens,
        }
        .percent_of_context_window_remaining(context_window),
    )
}

fn should_apply_collab_receiver_status(
    current: Option<&codex_app_server_protocol::CollabAgentState>,
    incoming: &codex_app_server_protocol::CollabAgentState,
) -> bool {
    let Some(current) = current else {
        return true;
    };
    !collab_agent_status_is_running(&incoming.status)
        || collab_agent_status_is_running(&current.status)
}

fn collab_agent_status_is_running(status: &codex_app_server_protocol::CollabAgentStatus) -> bool {
    matches!(
        status,
        codex_app_server_protocol::CollabAgentStatus::PendingInit
            | codex_app_server_protocol::CollabAgentStatus::Running
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use codex_protocol::models::ActivePermissionProfile;
    use codex_protocol::models::BUILT_IN_PERMISSION_PROFILE_WORKSPACE;

    async fn config_with_workspace_profile() -> Config {
        let temp_dir = tempfile::tempdir().expect("tempdir");
        ConfigBuilder::default()
            .codex_home(temp_dir.path().to_path_buf())
            .harness_overrides(ConfigOverrides {
                default_permissions: Some(BUILT_IN_PERMISSION_PROFILE_WORKSPACE.to_string()),
                ..ConfigOverrides::default()
            })
            .build()
            .await
            .expect("config should build")
    }

    #[tokio::test]
    async fn turn_permissions_use_active_profile_when_available() {
        let config = config_with_workspace_profile().await;
        let active_permission_profile = config.permissions.active_permission_profile();

        assert_eq!(
            App::turn_permissions_override_from_config(
                &config,
                active_permission_profile.as_ref(),
                /*runtime_permission_profile_override*/ None,
            ),
            TurnPermissionsOverride::ActiveProfile(ActivePermissionProfile::new(
                BUILT_IN_PERMISSION_PROFILE_WORKSPACE
            ))
        );
    }

    #[tokio::test]
    async fn turn_permissions_preserve_server_snapshot_without_local_override() {
        let mut config = config_with_workspace_profile().await;
        config
            .permissions
            .set_permission_profile(PermissionProfile::read_only())
            .expect("read-only profile should be allowed");

        assert_eq!(
            App::turn_permissions_override_from_config(
                &config, /*active_permission_profile*/ None,
                /*runtime_permission_profile_override*/ None,
            ),
            TurnPermissionsOverride::Preserve
        );
    }

    #[tokio::test]
    async fn turn_permissions_send_legacy_sandbox_for_local_override() {
        let mut config = config_with_workspace_profile().await;
        let permission_profile = PermissionProfile::workspace_write();
        config
            .permissions
            .set_permission_profile(permission_profile.clone())
            .expect("workspace profile should be allowed");
        let effective_permission_profile = config.permissions.effective_permission_profile();

        assert_eq!(
            App::turn_permissions_override_from_config(
                &config,
                /*active_permission_profile*/ None,
                Some(&permission_profile),
            ),
            TurnPermissionsOverride::LegacySandbox(effective_permission_profile)
        );
    }
}
