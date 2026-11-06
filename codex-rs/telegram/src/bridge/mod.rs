use std::collections::HashMap;
use std::collections::VecDeque;
use std::sync::Arc;
use std::time::Duration;

use anyhow::Context;
use codex_app_server_client::InProcessAppServerClient;
use codex_app_server_client::InProcessServerEvent;
use codex_app_server_client::TypedRequestError;
use codex_app_server_protocol::ClientRequest;
use codex_app_server_protocol::JSONRPCErrorError;
use codex_app_server_protocol::RequestId;
use codex_app_server_protocol::SandboxMode as AppServerSandboxMode;
use codex_app_server_protocol::ThreadResumeParams;
use codex_app_server_protocol::ThreadResumeResponse;
use codex_app_server_protocol::ThreadSource;
use codex_app_server_protocol::ThreadStartParams;
use codex_app_server_protocol::ThreadStartResponse;
use codex_app_server_protocol::ThreadUnsubscribeParams;
use codex_app_server_protocol::ThreadUnsubscribeResponse;
use codex_app_server_protocol::TurnInterruptParams;
use codex_app_server_protocol::TurnInterruptResponse;
use codex_core::config::Config;
use codex_login::AuthManager;
use codex_protocol::protocol::SandboxPolicy;
use serde::de::DeserializeOwned;
use teloxide::ApiError;
use teloxide::Bot;
use teloxide::RequestError;
use teloxide::payloads::EditMessageTextSetters;
use teloxide::payloads::SendMessageSetters;
use teloxide::prelude::Requester;
use teloxide::types::Message;
use teloxide::types::MessageId;
use teloxide::types::ParseMode;
use teloxide::types::UserId;
use tokio::sync::Mutex;
use tokio::sync::mpsc;
use tokio::sync::oneshot;
use tokio::task::JoinHandle;
use tokio::time::timeout;
use tracing::instrument;
use tracing::warn;

use crate::approvals::ApprovalCallback;
use crate::conversation::ConversationKey;
use crate::model_selection::ModelPickerCallback;
use crate::render::render_html_chunks;
use crate::session::SessionStore;

mod commands;
mod notifications;
mod server_requests;
mod status;
mod user_input;

use self::user_input::QueuedUserInput;

const BRIDGE_CHANNEL_CAPACITY: usize = 128;

#[derive(Debug)]
struct RequestIdSequencer {
    next: i64,
}

impl RequestIdSequencer {
    fn new() -> Self {
        Self { next: 1 }
    }

    fn next(&mut self) -> RequestId {
        let id = self.next;
        self.next += 1;
        RequestId::Integer(id)
    }
}

enum BridgeCommand {
    UserText {
        conversation: ConversationKey,
        text: String,
        /// Local paths of already-downloaded inbound images; each becomes a
        /// `UserInput::LocalImage` on the turn.
        images: Vec<std::path::PathBuf>,
        client_user_message_id: String,
        actor_user_id: Option<UserId>,
        admission_tx: oneshot::Sender<Result<UserInputAdmission, String>>,
        completion_tx: oneshot::Sender<Result<(), String>>,
    },
    NewThread {
        conversation: ConversationKey,
        response_tx: oneshot::Sender<Result<(), String>>,
    },
    Cancel {
        conversation: ConversationKey,
        response_tx: oneshot::Sender<Result<(), String>>,
    },
    Model {
        conversation: ConversationKey,
        args: String,
        response_tx: oneshot::Sender<Result<(), String>>,
    },
    ModelPicker {
        conversation: ConversationKey,
        callback: ModelPickerCallback,
        response_tx: oneshot::Sender<Result<String, String>>,
    },
    Approvals {
        conversation: ConversationKey,
        args: String,
        response_tx: oneshot::Sender<Result<(), String>>,
    },
    Compact {
        conversation: ConversationKey,
        response_tx: oneshot::Sender<Result<(), String>>,
    },
    Diff {
        conversation: ConversationKey,
        response_tx: oneshot::Sender<Result<(), String>>,
    },
    Skills {
        conversation: ConversationKey,
        response_tx: oneshot::Sender<Result<(), String>>,
    },
    Status {
        conversation: ConversationKey,
        response_tx: oneshot::Sender<String>,
    },
    Approval {
        conversation: ConversationKey,
        callback: ApprovalCallback,
        actor_user_id: UserId,
        response_tx: oneshot::Sender<String>,
    },
    Shutdown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum UserInputAdmission {
    Applied,
    Queued,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ThreadResolution {
    thread_id: String,
    reconcile_inbox: bool,
}

impl ThreadResolution {
    fn existing(thread_id: String) -> Self {
        Self {
            thread_id,
            reconcile_inbox: true,
        }
    }

    fn fresh(thread_id: String) -> Self {
        Self {
            thread_id,
            reconcile_inbox: false,
        }
    }
}

pub(crate) enum UserInputReceipt {
    Applied,
    Queued(oneshot::Receiver<Result<(), String>>),
}

#[derive(Clone)]
pub struct BridgeHandle {
    command_tx: mpsc::Sender<BridgeCommand>,
    task: Arc<Mutex<Option<JoinHandle<()>>>>,
}

impl BridgeHandle {
    pub fn spawn(
        bot: Bot,
        client: InProcessAppServerClient,
        config: Arc<Config>,
        sessions: Arc<SessionStore>,
        auth_manager: Arc<AuthManager>,
    ) -> Self {
        let (command_tx, command_rx) = mpsc::channel(BRIDGE_CHANNEL_CAPACITY);
        let runtime = BridgeRuntime {
            bot,
            client,
            config,
            auth_manager,
            sessions,
            request_ids: RequestIdSequencer::new(),
            pending_inputs: HashMap::new(),
            last_successful_contact_at: HashMap::new(),
            last_errors: HashMap::new(),
            last_token_usage: HashMap::new(),
        };
        let task = tokio::spawn(async move {
            runtime.run(command_rx).await;
        });
        Self {
            command_tx,
            task: Arc::new(Mutex::new(Some(task))),
        }
    }

    #[instrument(skip(self, text))]
    pub async fn send_user_text(
        &self,
        conversation: ConversationKey,
        text: String,
        client_user_message_id: String,
        actor_user_id: Option<UserId>,
    ) -> anyhow::Result<UserInputReceipt> {
        self.send_user_input(
            conversation,
            text,
            Vec::new(),
            client_user_message_id,
            actor_user_id,
        )
        .await
    }

    #[instrument(skip(self, text, images))]
    pub async fn send_user_input(
        &self,
        conversation: ConversationKey,
        text: String,
        images: Vec<std::path::PathBuf>,
        client_user_message_id: String,
        actor_user_id: Option<UserId>,
    ) -> anyhow::Result<UserInputReceipt> {
        let (admission_tx, admission_rx) = oneshot::channel();
        let (completion_tx, completion_rx) = oneshot::channel();
        self.command_tx
            .send(BridgeCommand::UserText {
                conversation,
                text,
                images,
                client_user_message_id,
                actor_user_id,
                admission_tx,
                completion_tx,
            })
            .await
            .context("telegram bridge task stopped")?;
        match admission_rx
            .await
            .context("telegram bridge user-input acknowledgement dropped")?
            .map_err(anyhow::Error::msg)?
        {
            UserInputAdmission::Applied => Ok(UserInputReceipt::Applied),
            UserInputAdmission::Queued => Ok(UserInputReceipt::Queued(completion_rx)),
        }
    }

    #[instrument(skip(self))]
    pub async fn new_thread(&self, conversation: ConversationKey) -> anyhow::Result<()> {
        let (response_tx, response_rx) = oneshot::channel();
        self.command_tx
            .send(BridgeCommand::NewThread {
                conversation,
                response_tx,
            })
            .await
            .context("telegram bridge task stopped")?;
        await_command_ack(response_rx).await
    }

    #[instrument(skip(self))]
    pub async fn cancel(&self, conversation: ConversationKey) -> anyhow::Result<()> {
        let (response_tx, response_rx) = oneshot::channel();
        self.command_tx
            .send(BridgeCommand::Cancel {
                conversation,
                response_tx,
            })
            .await
            .context("telegram bridge task stopped")?;
        await_command_ack(response_rx).await
    }

    #[instrument(skip(self, args))]
    pub async fn model(&self, conversation: ConversationKey, args: String) -> anyhow::Result<()> {
        let (response_tx, response_rx) = oneshot::channel();
        self.command_tx
            .send(BridgeCommand::Model {
                conversation,
                args,
                response_tx,
            })
            .await
            .context("telegram bridge task stopped")?;
        await_command_ack(response_rx).await
    }

    pub async fn handle_model_picker_callback(
        &self,
        conversation: ConversationKey,
        callback: ModelPickerCallback,
    ) -> anyhow::Result<String> {
        let (response_tx, response_rx) = oneshot::channel();
        self.command_tx
            .send(BridgeCommand::ModelPicker {
                conversation,
                callback,
                response_tx,
            })
            .await
            .context("telegram bridge task stopped")?;
        response_rx
            .await
            .context("telegram bridge model-picker acknowledgement dropped")?
            .map_err(anyhow::Error::msg)
    }

    #[instrument(skip(self, args))]
    pub async fn approvals(
        &self,
        conversation: ConversationKey,
        args: String,
    ) -> anyhow::Result<()> {
        let (response_tx, response_rx) = oneshot::channel();
        self.command_tx
            .send(BridgeCommand::Approvals {
                conversation,
                args,
                response_tx,
            })
            .await
            .context("telegram bridge task stopped")?;
        await_command_ack(response_rx).await
    }

    #[instrument(skip(self))]
    pub async fn compact(&self, conversation: ConversationKey) -> anyhow::Result<()> {
        let (response_tx, response_rx) = oneshot::channel();
        self.command_tx
            .send(BridgeCommand::Compact {
                conversation,
                response_tx,
            })
            .await
            .context("telegram bridge task stopped")?;
        await_command_ack(response_rx).await
    }

    #[instrument(skip(self))]
    pub async fn diff(&self, conversation: ConversationKey) -> anyhow::Result<()> {
        let (response_tx, response_rx) = oneshot::channel();
        self.command_tx
            .send(BridgeCommand::Diff {
                conversation,
                response_tx,
            })
            .await
            .context("telegram bridge task stopped")?;
        await_command_ack(response_rx).await
    }

    #[instrument(skip(self))]
    pub async fn skills(&self, conversation: ConversationKey) -> anyhow::Result<()> {
        let (response_tx, response_rx) = oneshot::channel();
        self.command_tx
            .send(BridgeCommand::Skills {
                conversation,
                response_tx,
            })
            .await
            .context("telegram bridge task stopped")?;
        await_command_ack(response_rx).await
    }

    pub async fn status_text(&self, conversation: ConversationKey) -> anyhow::Result<String> {
        let (response_tx, response_rx) = oneshot::channel();
        self.command_tx
            .send(BridgeCommand::Status {
                conversation,
                response_tx,
            })
            .await
            .context("telegram bridge task stopped")?;
        response_rx.await.context("telegram bridge status dropped")
    }

    pub async fn handle_approval_callback(
        &self,
        conversation: ConversationKey,
        callback: ApprovalCallback,
        actor_user_id: UserId,
    ) -> anyhow::Result<String> {
        let (response_tx, response_rx) = oneshot::channel();
        self.command_tx
            .send(BridgeCommand::Approval {
                conversation,
                callback,
                actor_user_id,
                response_tx,
            })
            .await
            .context("telegram bridge task stopped")?;
        response_rx
            .await
            .context("telegram bridge approval dropped")
    }

    pub async fn shutdown(&self) -> anyhow::Result<()> {
        if let Err(err) = self.command_tx.send(BridgeCommand::Shutdown).await {
            warn!("telegram bridge task already stopped before shutdown request: {err}");
        }
        let task = {
            let mut task = self.task.lock().await;
            task.take()
        };
        let Some(task) = task else {
            return Ok(());
        };
        match timeout(Duration::from_secs(10), task).await {
            Ok(Ok(())) => {}
            Ok(Err(err)) => warn!("telegram bridge task failed during shutdown: {err}"),
            Err(_) => warn!("timed out waiting for Telegram bridge shutdown"),
        }
        Ok(())
    }
}

struct BridgeRuntime {
    bot: Bot,
    client: InProcessAppServerClient,
    config: Arc<Config>,
    auth_manager: Arc<AuthManager>,
    sessions: Arc<SessionStore>,
    request_ids: RequestIdSequencer,
    pending_inputs: HashMap<ConversationKey, VecDeque<QueuedUserInput>>,
    last_successful_contact_at: HashMap<ConversationKey, u64>,
    last_errors: HashMap<ConversationKey, String>,
    // A Telegram chat has no natural session boundary: every message resumes the
    // same thread until someone runs `/new`. The server already reports usage per
    // turn, so keep the latest report to give `/status` the running cost of a
    // thread that would otherwise grow unobserved.
    last_token_usage: HashMap<ConversationKey, status::StatusTokenUsage>,
}

impl BridgeRuntime {
    async fn run(mut self, mut command_rx: mpsc::Receiver<BridgeCommand>) {
        loop {
            tokio::select! {
                command = command_rx.recv() => {
                    match command {
                        Some(BridgeCommand::Shutdown) | None => break,
                        Some(command) => {
                            let error_conversation = command.error_conversation();
                            let contact_conversation = command.conversation();
                            let clears_prior_error = command.clears_prior_error_on_success();
                            if let Err(err) = self.handle_command(command).await {
                                if let Some(conversation) = error_conversation {
                                    self.last_errors.insert(conversation, format!("{err:#}"));
                                }
                                warn!("telegram bridge command failed: {err}");
                                if let Some(conversation) = error_conversation
                                    && let Err(send_err) = self
                                        .send_text(conversation, &format!("Error: {err:#}"))
                                        .await
                                {
                                    warn!("failed to report Telegram bridge error to chat: {send_err}");
                                }
                            } else if let Some(conversation) = contact_conversation {
                                let now = std::time::SystemTime::now()
                                    .duration_since(std::time::UNIX_EPOCH)
                                    .ok()
                                    .map(|duration| duration.as_secs());
                                if let Some(now) = now {
                                    self.last_successful_contact_at.insert(conversation, now);
                                }
                                if clears_prior_error {
                                    self.last_errors.remove(&conversation);
                                }
                            }
                        }
                    }
                }
                event = self.client.next_event() => {
                    let Some(event) = event else {
                        break;
                    };
                    if let Err(err) = self.handle_event(event).await {
                        warn!("telegram bridge event handling failed: {err}");
                    }
                    self.pump_pending_inputs().await;
                }
            }
        }

        let thread_ids = self.sessions.thread_ids().await;
        for thread_id in thread_ids {
            if let Err(err) = self.unsubscribe_thread(&thread_id).await {
                warn!("thread/unsubscribe failed during Telegram shutdown: {err}");
            }
        }
        if let Err(err) = self.client.shutdown().await {
            warn!("in-process app-server shutdown failed: {err}");
        }
    }

    async fn handle_command(&mut self, command: BridgeCommand) -> anyhow::Result<()> {
        match command {
            BridgeCommand::UserText {
                conversation,
                text,
                images,
                client_user_message_id,
                actor_user_id,
                admission_tx,
                completion_tx,
            } => {
                self.handle_user_input(
                    conversation,
                    text,
                    images,
                    client_user_message_id,
                    actor_user_id,
                    admission_tx,
                    completion_tx,
                )
                .await
            }
            BridgeCommand::NewThread {
                conversation,
                response_tx,
            } => {
                let result = async {
                    let thread_id = self.start_new_thread(conversation).await?;
                    self.notify_after_effect(
                        conversation,
                        &format!("Started new thread {thread_id}."),
                        "new-thread confirmation",
                    )
                    .await;
                    Ok(())
                }
                .await;
                acknowledge_command(response_tx, result)
            }
            BridgeCommand::Cancel {
                conversation,
                response_tx,
            } => acknowledge_command(response_tx, self.cancel_turn(conversation).await),
            BridgeCommand::Model {
                conversation,
                args,
                response_tx,
            } => acknowledge_command(response_tx, self.handle_model(conversation, args).await),
            BridgeCommand::ModelPicker {
                conversation,
                callback,
                response_tx,
            } => match self
                .handle_model_picker_callback(conversation, callback)
                .await
            {
                Ok(response) => {
                    let _ = response_tx.send(Ok(response));
                    Ok(())
                }
                Err(err) => {
                    let _ = response_tx.send(Err(format!("{err:#}")));
                    Err(err)
                }
            },
            BridgeCommand::Approvals {
                conversation,
                args,
                response_tx,
            } => acknowledge_command(response_tx, self.handle_approvals(conversation, args).await),
            BridgeCommand::Compact {
                conversation,
                response_tx,
            } => acknowledge_command(response_tx, self.compact_thread(conversation).await),
            BridgeCommand::Diff {
                conversation,
                response_tx,
            } => acknowledge_command(response_tx, self.send_diff(conversation).await),
            BridgeCommand::Skills {
                conversation,
                response_tx,
            } => acknowledge_command(response_tx, self.list_skills(conversation).await),
            BridgeCommand::Status {
                conversation,
                response_tx,
            } => {
                let _ = response_tx.send(self.runtime_status_text(conversation).await);
                Ok(())
            }
            BridgeCommand::Approval {
                conversation,
                callback,
                actor_user_id,
                response_tx,
            } => {
                let result = self
                    .resolve_approval(conversation, actor_user_id, callback)
                    .await;
                let text = match result {
                    Ok(text) => text,
                    Err(err) => format!("Approval failed: {err}"),
                };
                let _ = response_tx.send(text);
                Ok(())
            }
            BridgeCommand::Shutdown => Ok(()),
        }
    }

    async fn handle_event(&mut self, event: InProcessServerEvent) -> anyhow::Result<()> {
        match event {
            InProcessServerEvent::ServerRequest(request) => {
                self.handle_server_request(*request).await
            }
            InProcessServerEvent::ServerNotification(notification) => {
                self.handle_notification(*notification).await
            }
            InProcessServerEvent::Lagged { skipped } => self.handle_lagged(skipped as u64).await,
        }
    }

    async fn cancel_turn(&mut self, conversation: ConversationKey) -> anyhow::Result<()> {
        let cancelled_queued = self.cancel_pending_inputs(conversation);
        let Some(thread_id) = self.sessions.thread_id(conversation).await else {
            let message = if cancelled_queued > 0 {
                format!("Cancelled {cancelled_queued} queued message(s).")
            } else {
                "No active thread to cancel.".to_string()
            };
            self.send_text(conversation, &message).await?;
            return Ok(());
        };
        let Some(turn_id) = self.sessions.turn_id(conversation).await else {
            let message = if cancelled_queued > 0 {
                format!("Cancelled {cancelled_queued} queued message(s); no turn was running.")
            } else {
                "No active turn to cancel.".to_string()
            };
            self.send_text(conversation, &message).await?;
            return Ok(());
        };
        let request_id = self.request_ids.next();
        let _: TurnInterruptResponse = self
            .request_typed(
                ClientRequest::TurnInterrupt {
                    request_id,
                    params: TurnInterruptParams { thread_id, turn_id },
                },
                "turn/interrupt",
            )
            .await?;
        let message = if cancelled_queued > 0 {
            format!("Cancel requested; also cancelled {cancelled_queued} queued message(s).")
        } else {
            "Cancel requested.".to_string()
        };
        self.notify_after_effect(conversation, &message, "cancel confirmation")
            .await;
        Ok(())
    }

    async fn ensure_thread(
        &mut self,
        conversation: ConversationKey,
    ) -> anyhow::Result<ThreadResolution> {
        if let Some(thread_id) = self.sessions.thread_id(conversation).await {
            if !self.sessions.thread_loaded(conversation).await {
                match self
                    .try_resume_thread(conversation, thread_id.clone())
                    .await
                {
                    Ok(()) => {}
                    Err(TypedRequestError::Server { method, source })
                        if thread_resume_has_no_rollout(&method, &source) =>
                    {
                        warn!(
                            %conversation,
                            stale_thread_id = %thread_id,
                            "stored Telegram thread has no rollout; replacing it"
                        );
                        let replacement = self
                            .start_new_thread(conversation)
                            .await
                            .context("failed to replace unresumable Telegram thread")?;
                        self.notify_after_effect(
                            conversation,
                            "The previous Corbanu Terminal thread could not be resumed. I started a new thread and kept your pending Telegram message; prior thread context is not available in this chat.",
                            "thread replacement notice",
                        )
                        .await;
                        return Ok(ThreadResolution::fresh(replacement));
                    }
                    Err(err) => {
                        return Err(err).context(
                            "failed app-server request `thread/resume for Telegram session`",
                        );
                    }
                }
            }
            return Ok(ThreadResolution::existing(thread_id));
        }
        self.start_new_thread(conversation)
            .await
            .map(ThreadResolution::fresh)
    }

    async fn start_new_thread(&mut self, conversation: ConversationKey) -> anyhow::Result<String> {
        let request_id = self.request_ids.next();
        let params = self.thread_start_params(conversation).await;
        let response: ThreadStartResponse = self
            .request_typed(
                ClientRequest::ThreadStart { request_id, params },
                "thread/start",
            )
            .await?;
        let thread_id = response.thread.id;
        self.sessions
            .set_thread(conversation, thread_id.clone())
            .await?;
        // The previous thread's usage is not this thread's: `/status` shows
        // nothing for it until the new thread reports or is estimated.
        self.last_token_usage.remove(&conversation);
        Ok(thread_id)
    }

    async fn resume_thread(
        &mut self,
        conversation: ConversationKey,
        thread_id: String,
    ) -> anyhow::Result<()> {
        self.try_resume_thread(conversation, thread_id)
            .await
            .context("failed app-server request `thread/resume`")
    }

    async fn try_resume_thread(
        &mut self,
        conversation: ConversationKey,
        thread_id: String,
    ) -> Result<(), TypedRequestError> {
        let request_id = self.request_ids.next();
        let (model, model_provider) = self.active_model_settings(conversation).await;
        let approval_policy = self.active_approval_policy(conversation).await;
        let _: ThreadResumeResponse = self
            .client
            .request_typed(ClientRequest::ThreadResume {
                request_id,
                params: ThreadResumeParams {
                    thread_id,
                    model,
                    model_provider: Some(model_provider),
                    cwd: Some(self.config.cwd.to_string_lossy().to_string()),
                    runtime_workspace_roots: Some(self.config.workspace_roots.clone()),
                    approval_policy: Some(approval_policy),
                    sandbox: Some(app_server_sandbox_mode(
                        &self.config.legacy_sandbox_policy(),
                    )),
                    ..ThreadResumeParams::default()
                },
            })
            .await?;
        self.sessions.mark_thread_loaded(conversation).await;
        Ok(())
    }

    async fn thread_start_params(&self, conversation: ConversationKey) -> ThreadStartParams {
        let (model, model_provider) = self.active_model_settings(conversation).await;
        let approval_policy = self.active_approval_policy(conversation).await;
        ThreadStartParams {
            model,
            model_provider: Some(model_provider),
            cwd: Some(self.config.cwd.to_string_lossy().to_string()),
            runtime_workspace_roots: Some(self.config.workspace_roots.clone()),
            approval_policy: Some(approval_policy),
            approvals_reviewer: None,
            sandbox: Some(app_server_sandbox_mode(
                &self.config.legacy_sandbox_policy(),
            )),
            permissions: None,
            config: None,
            ephemeral: Some(self.config.ephemeral),
            session_start_source: None,
            thread_source: Some(ThreadSource::User),
            ..ThreadStartParams::default()
        }
    }

    pub(super) async fn send_text(
        &self,
        conversation: ConversationKey,
        text: &str,
    ) -> anyhow::Result<()> {
        for chunk in render_html_chunks(text) {
            self.send_html(conversation, &chunk.html).await?;
        }
        Ok(())
    }

    /// Report an already-accepted state change without turning a Telegram send
    /// failure into a replay of that mutation. The update remains successful;
    /// `/status` and the next command expose the resulting state.
    pub(super) async fn notify_after_effect(
        &self,
        conversation: ConversationKey,
        text: &str,
        operation: &str,
    ) {
        if let Err(err) = self.send_text(conversation, text).await {
            warn!(
                %conversation,
                operation,
                "Telegram confirmation failed after the operation was accepted: {err}"
            );
        }
    }

    pub(super) async fn send_html(
        &self,
        conversation: ConversationKey,
        html: &str,
    ) -> anyhow::Result<Message> {
        // Mutating: a timeout bounds the call, but it is never auto-retried —
        // a retried send could post the message twice. Duplicate protection
        // for sends lives at the update level (`crate::dedup`), not here.
        let bot = self.bot.clone();
        let html = html.to_string();
        crate::outbound::call_with_policy(
            crate::outbound::CallSafety::Mutating,
            crate::outbound::DEFAULT_API_TIMEOUT,
            "telegram sendMessage",
            move || {
                let bot = bot.clone();
                let html = html.clone();
                async move {
                    let mut request = bot
                        .send_message(conversation.chat_id, html)
                        .parse_mode(ParseMode::Html);
                    if let Some(thread_id) = conversation.thread_id {
                        request = request.message_thread_id(thread_id);
                    }
                    request.await
                }
            },
        )
        .await
        .context("send Telegram message")
    }

    pub(super) async fn edit_message(
        &self,
        conversation: ConversationKey,
        message_id: MessageId,
        text: &str,
    ) -> anyhow::Result<bool> {
        let chunks = render_html_chunks(text);
        if let Some(first) = chunks.first()
            && let Some(delay) = finish_edit_result(
                crate::outbound::call_with_policy(
                    crate::outbound::CallSafety::Mutating,
                    crate::outbound::DEFAULT_API_TIMEOUT,
                    "telegram editMessageText",
                    || {
                        let bot = self.bot.clone();
                        let html = first.html.clone();
                        async move {
                            bot.edit_message_text(conversation.chat_id, message_id, html)
                                .parse_mode(ParseMode::Html)
                                .await
                        }
                    },
                )
                .await,
                "edit Telegram message",
            )?
        {
            self.sessions
                .suppress_stream_edits_until(conversation, std::time::Instant::now() + delay)
                .await;
            return Ok(false);
        }
        Ok(true)
    }

    async fn unsubscribe_thread(&mut self, thread_id: &str) -> anyhow::Result<()> {
        let request_id = self.request_ids.next();
        let _: ThreadUnsubscribeResponse = self
            .request_typed(
                ClientRequest::ThreadUnsubscribe {
                    request_id,
                    params: ThreadUnsubscribeParams {
                        thread_id: thread_id.to_string(),
                    },
                },
                "thread/unsubscribe",
            )
            .await?;
        Ok(())
    }

    pub(super) async fn request_typed<T>(
        &self,
        request: ClientRequest,
        method: &str,
    ) -> anyhow::Result<T>
    where
        T: DeserializeOwned,
    {
        self.client
            .request_typed(request)
            .await
            .with_context(|| format!("failed app-server request `{method}`"))
    }
}

fn thread_resume_has_no_rollout(method: &str, error: &JSONRPCErrorError) -> bool {
    method == "thread/resume"
        && error.code == -32600
        && error.message.contains("no rollout found for thread id")
}

fn app_server_sandbox_mode(policy: &SandboxPolicy) -> AppServerSandboxMode {
    match policy {
        SandboxPolicy::ReadOnly { .. } => AppServerSandboxMode::ReadOnly,
        SandboxPolicy::WorkspaceWrite { .. } => AppServerSandboxMode::WorkspaceWrite,
        SandboxPolicy::DangerFullAccess | SandboxPolicy::ExternalSandbox { .. } => {
            AppServerSandboxMode::DangerFullAccess
        }
    }
}

#[cfg(test)]
#[path = "bridge_tests.rs"]
mod tests;

impl BridgeCommand {
    fn error_conversation(&self) -> Option<ConversationKey> {
        match self {
            Self::UserText { conversation, .. }
            | Self::NewThread { conversation, .. }
            | Self::Cancel { conversation, .. }
            | Self::Model { conversation, .. }
            | Self::ModelPicker { conversation, .. }
            | Self::Approvals { conversation, .. }
            | Self::Compact { conversation, .. }
            | Self::Diff { conversation, .. }
            | Self::Skills { conversation, .. } => Some(*conversation),
            Self::Status { .. } | Self::Approval { .. } | Self::Shutdown => None,
        }
    }

    fn conversation(&self) -> Option<ConversationKey> {
        match self {
            Self::UserText { conversation, .. }
            | Self::NewThread { conversation, .. }
            | Self::Cancel { conversation, .. }
            | Self::Model { conversation, .. }
            | Self::ModelPicker { conversation, .. }
            | Self::Approvals { conversation, .. }
            | Self::Compact { conversation, .. }
            | Self::Diff { conversation, .. }
            | Self::Skills { conversation, .. }
            | Self::Status { conversation, .. }
            | Self::Approval { conversation, .. } => Some(*conversation),
            Self::Shutdown => None,
        }
    }

    fn clears_prior_error_on_success(&self) -> bool {
        !matches!(self, Self::Status { .. } | Self::Shutdown)
    }
}

async fn await_command_ack(
    response_rx: oneshot::Receiver<Result<(), String>>,
) -> anyhow::Result<()> {
    response_rx
        .await
        .context("telegram bridge command acknowledgement dropped")?
        .map_err(anyhow::Error::msg)
}

fn acknowledge_command(
    response_tx: oneshot::Sender<Result<(), String>>,
    result: anyhow::Result<()>,
) -> anyhow::Result<()> {
    let response = result
        .as_ref()
        .map(|_| ())
        .map_err(|err| format!("{err:#}"));
    let _ = response_tx.send(response);
    result
}

pub(super) fn finish_edit_result(
    result: Result<Message, RequestError>,
    context: &str,
) -> anyhow::Result<Option<Duration>> {
    match result {
        Ok(_) => Ok(None),
        Err(RequestError::Api(ApiError::MessageNotModified)) => Ok(None),
        Err(RequestError::RetryAfter(delay)) => Ok(Some(delay.duration())),
        Err(err) => Err(err).context(context.to_string()),
    }
}
