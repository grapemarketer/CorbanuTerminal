mod compact;
mod lifecycle;
mod regular;
mod review;
mod user_shell;

use std::sync::Arc;
use std::time::Duration;
use std::time::Instant;

use codex_extension_api::ExtensionData;
use futures::FutureExt;
use futures::future::BoxFuture;
use tokio::select;
use tokio::sync::Notify;
use tokio_util::sync::CancellationToken;
use tokio_util::task::AbortOnDropHandle;
use tracing::Instrument;
use tracing::Span;
use tracing::debug;
use tracing::field;
use tracing::info_span;
use tracing::trace;
use tracing::trace_span;
use tracing::warn;

use crate::agent::control::AgentExecutionGuard;
use crate::codex_thread::BackgroundTerminalInfo;
use crate::config::Config;
use crate::context::ContextualUserFragment;
use crate::hook_runtime::inspect_pending_input;
use crate::hook_runtime::record_additional_contexts;
use crate::hook_runtime::record_pending_input;
use crate::session::TurnInput;
use crate::session::session::Session;
use crate::session::turn::turn_inputs_are_terminal_result_only;
use crate::session::turn::turn_inputs_expect_parent_completion;
use crate::session::turn_context::TurnContext;
use crate::state::ActiveTurn;
use crate::state::RunningTask;
use crate::state::TaskKind;
use codex_analytics::TurnProfileFact;
use codex_analytics::TurnTokenUsageFact;
use codex_login::AuthManager;
use codex_models_manager::manager::SharedModelsManager;
use codex_otel::SessionTelemetry;
use codex_otel::TURN_E2E_DURATION_METRIC;
use codex_otel::TURN_MEMORY_METRIC;
use codex_otel::TURN_NETWORK_PROXY_METRIC;
use codex_otel::TURN_TOKEN_USAGE_METRIC;
use codex_otel::TURN_TOOL_CALL_METRIC;
use codex_protocol::models::ResponseItem;
use codex_protocol::protocol::EventMsg;
use codex_protocol::protocol::MultiAgentVersion;
use codex_protocol::protocol::Op;
use codex_protocol::protocol::TokenUsage;
use codex_protocol::protocol::TurnAbortReason;
use codex_protocol::protocol::TurnAbortedEvent;
use codex_protocol::protocol::TurnCompleteEvent;
use codex_protocol::protocol::TurnDiffEvent;
use codex_protocol::protocol::WarningEvent;

use codex_features::Feature;
use codex_protocol::error::CodexErr;
use codex_protocol::error::CodexErrorDetails;
use codex_protocol::error::Result as CodexResult;
use codex_protocol::models::ContentItem;
pub(crate) use compact::CompactTask;
pub(crate) use regular::RegularTask;
pub(crate) use review::ReviewTask;
pub(crate) use user_shell::UserShellCommandMode;
pub(crate) use user_shell::UserShellCommandTask;
pub(crate) use user_shell::execute_user_shell_command;

const GRACEFULL_INTERRUPTION_TIMEOUT_MS: u64 = 100;
const TASK_COMPACT_METRIC: &str = "codex.task.compact";

pub(crate) type SessionTaskResult = CodexResult<Option<String>>;

/// The turn error reported when a task panicked (#616). The panic itself is
/// logged by the process panic hook; its payload is not repeated here.
pub(crate) const TURN_PANICKED_MESSAGE: &str =
    "this turn stopped because of an internal error (a panic); the details are in the log";

pub(crate) enum MailboxParentProvenance {
    Ignore,
    Attribute,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum InterruptedTurnHistoryMarker {
    Disabled,
    ContextualUser,
    Developer,
}

impl InterruptedTurnHistoryMarker {
    pub(crate) fn from_config_and_version(
        config: &Config,
        multi_agent_version: MultiAgentVersion,
    ) -> Self {
        if !config.agent_interrupt_message_enabled {
            return Self::Disabled;
        }
        if multi_agent_version == MultiAgentVersion::V2 {
            Self::Developer
        } else {
            Self::ContextualUser
        }
    }
}

/// Shared model-visible marker used by both the real interrupt path and
/// interrupted fork snapshots.
pub(crate) fn interrupted_turn_history_marker(
    marker: InterruptedTurnHistoryMarker,
) -> Option<ResponseItem> {
    match marker {
        InterruptedTurnHistoryMarker::Disabled => None,
        InterruptedTurnHistoryMarker::ContextualUser => Some(ContextualUserFragment::into(
            crate::context::TurnAborted::new(crate::context::TurnAborted::INTERRUPTED_GUIDANCE),
        )),
        InterruptedTurnHistoryMarker::Developer => {
            let marker = crate::context::TurnAborted::new(
                crate::context::TurnAborted::INTERRUPTED_DEVELOPER_GUIDANCE,
            );
            Some(ResponseItem::Message {
                id: None,
                role: "developer".to_string(),
                content: vec![ContentItem::InputText {
                    text: marker.render(),
                }],
                phase: None,
                internal_chat_message_metadata_passthrough: None,
            })
        }
    }
}

fn emit_turn_network_proxy_metric(
    session_telemetry: &SessionTelemetry,
    network_proxy_active: bool,
    tmp_mem: (&str, &str),
) {
    let active = if network_proxy_active {
        "true"
    } else {
        "false"
    };
    session_telemetry.counter(
        TURN_NETWORK_PROXY_METRIC,
        /*inc*/ 1,
        &[("active", active), tmp_mem],
    );
}

fn emit_turn_memory_metric(
    session_telemetry: &SessionTelemetry,
    feature_enabled: bool,
    config_enabled: bool,
    has_citations: bool,
) {
    let read_allowed = feature_enabled && config_enabled;
    session_telemetry.counter(
        TURN_MEMORY_METRIC,
        /*inc*/ 1,
        &[
            ("read_allowed", bool_tag(read_allowed)),
            ("feature_enabled", bool_tag(feature_enabled)),
            ("config_use_memories", bool_tag(config_enabled)),
            ("has_citations", bool_tag(has_citations)),
        ],
    );
}

pub(crate) fn emit_compact_metric(
    session_telemetry: &SessionTelemetry,
    compact_type: &'static str,
    manual: bool,
) {
    session_telemetry.counter(
        TASK_COMPACT_METRIC,
        /*inc*/ 1,
        &[("type", compact_type), ("manual", bool_tag(manual))],
    );
}

fn bool_tag(value: bool) -> &'static str {
    if value { "true" } else { "false" }
}

/// Thin wrapper that exposes the parts of [`Session`] task runners need.
#[derive(Clone)]
pub(crate) struct SessionTaskContext {
    session: Arc<Session>,
    turn_extension_data: Arc<ExtensionData>,
}

impl SessionTaskContext {
    pub(crate) fn new(session: Arc<Session>, turn_extension_data: Arc<ExtensionData>) -> Self {
        Self {
            session,
            turn_extension_data,
        }
    }

    pub(crate) fn clone_session(&self) -> Arc<Session> {
        Arc::clone(&self.session)
    }

    pub(crate) fn turn_extension_data(&self) -> Arc<ExtensionData> {
        Arc::clone(&self.turn_extension_data)
    }

    pub(crate) fn auth_manager(&self) -> Arc<AuthManager> {
        self.session.services.auth_manager()
    }

    pub(crate) fn models_manager(&self) -> SharedModelsManager {
        Arc::clone(&self.session.services.models_manager)
    }
}

/// Async task that drives a [`Session`] turn.
///
/// Implementations encapsulate a specific Codex workflow (regular chat,
/// reviews, ghost snapshots, etc.). Each task instance is owned by a
/// [`Session`] and executed on a background Tokio task. The trait is
/// intentionally small: implementers identify themselves via
/// [`SessionTask::kind`], perform their work in [`SessionTask::run`], and may
/// release resources in [`SessionTask::abort`].
pub(crate) trait SessionTask: Send + Sync + 'static {
    /// Describes the type of work the task performs so the session can
    /// surface it in telemetry and UI.
    fn kind(&self) -> TaskKind;

    /// Returns the tracing name for a spawned task span.
    fn span_name(&self) -> &'static str;

    /// Executes the task until completion or cancellation.
    ///
    /// Implementations typically stream protocol events using `session` and
    /// `ctx`, returning an optional final agent message when finished. The
    /// provided `cancellation_token` is cancelled when the session requests an
    /// abort; implementers should watch for it and terminate quickly once it
    /// fires. Returning [`Some`] yields a final message that
    /// [`Session::on_task_finished`] will emit to the client. Returning
    /// [`CodexErr::TurnAborted`] completes the task through the aborted-turn
    /// lifecycle instead.
    fn run(
        self: Arc<Self>,
        session: Arc<SessionTaskContext>,
        ctx: Arc<TurnContext>,
        input: Vec<TurnInput>,
        cancellation_token: CancellationToken,
    ) -> impl std::future::Future<Output = SessionTaskResult> + Send;

    /// Gives the task a chance to perform cleanup after an abort.
    ///
    /// The default implementation is a no-op; override this if additional
    /// teardown or notifications are required once
    /// [`Session::abort_all_tasks`] cancels the task.
    fn abort(
        &self,
        session: Arc<SessionTaskContext>,
        ctx: Arc<TurnContext>,
    ) -> impl std::future::Future<Output = ()> + Send {
        async move {
            let _ = (session, ctx);
        }
    }
}

pub(crate) trait AnySessionTask: Send + Sync + 'static {
    fn kind(&self) -> TaskKind;

    fn span_name(&self) -> &'static str;

    fn run(
        self: Arc<Self>,
        session: Arc<SessionTaskContext>,
        ctx: Arc<TurnContext>,
        input: Vec<TurnInput>,
        cancellation_token: CancellationToken,
    ) -> BoxFuture<'static, SessionTaskResult>;

    fn abort<'a>(
        &'a self,
        session: Arc<SessionTaskContext>,
        ctx: Arc<TurnContext>,
    ) -> BoxFuture<'a, ()>;
}

impl<T> AnySessionTask for T
where
    T: SessionTask + Send + Sync + 'static,
{
    fn kind(&self) -> TaskKind {
        SessionTask::kind(self)
    }

    fn span_name(&self) -> &'static str {
        SessionTask::span_name(self)
    }

    fn run(
        self: Arc<Self>,
        session: Arc<SessionTaskContext>,
        ctx: Arc<TurnContext>,
        input: Vec<TurnInput>,
        cancellation_token: CancellationToken,
    ) -> BoxFuture<'static, SessionTaskResult> {
        Box::pin(SessionTask::run(
            self,
            session,
            ctx,
            input,
            cancellation_token,
        ))
    }

    fn abort<'a>(
        &'a self,
        session: Arc<SessionTaskContext>,
        ctx: Arc<TurnContext>,
    ) -> BoxFuture<'a, ()> {
        Box::pin(SessionTask::abort(self, session, ctx))
    }
}

impl Session {
    pub async fn spawn_task<T: SessionTask + Send + Sync + 'static>(
        self: &Arc<Self>,
        turn_context: Arc<TurnContext>,
        input: Vec<TurnInput>,
        task: T,
    ) {
        self.abort_all_tasks(TurnAbortReason::Replaced).await;
        self.clear_connector_selection().await;
        self.start_task(turn_context, input, task, MailboxParentProvenance::Ignore)
            .await;
    }

    pub(crate) async fn start_task<T: SessionTask + Send + Sync + 'static>(
        self: &Arc<Self>,
        turn_context: Arc<TurnContext>,
        input: Vec<TurnInput>,
        task: T,
        mailbox_parent_provenance: MailboxParentProvenance,
    ) {
        let agent_execution_guard = self.services.agent_control.execution_guard(
            turn_context.multi_agent_version,
            &turn_context.session_source,
        );
        self.start_task_with_execution_guard(
            turn_context,
            input,
            task,
            mailbox_parent_provenance,
            agent_execution_guard,
        )
        .await;
    }

    async fn start_task_with_execution_guard<T: SessionTask + Send + Sync + 'static>(
        self: &Arc<Self>,
        turn_context: Arc<TurnContext>,
        input: Vec<TurnInput>,
        task: T,
        mailbox_parent_provenance: MailboxParentProvenance,
        agent_execution_guard: Option<AgentExecutionGuard>,
    ) {
        let task: Arc<dyn AnySessionTask> = Arc::new(task);
        let task_kind = task.kind();
        let span_name = task.span_name();
        let started_at = Instant::now();
        let turn_started_at_unix_ms = turn_context
            .turn_timing_state
            .mark_turn_started(started_at)
            .await;
        turn_context
            .turn_metadata_state
            .set_turn_started_at_unix_ms(turn_started_at_unix_ms);
        let token_usage_at_turn_start = self.total_token_usage().await.unwrap_or_default();

        let cancellation_token = CancellationToken::new();
        let done = Arc::new(Notify::new());

        self.services
            .guardian_rejection_circuit_breaker
            .lock()
            .await
            .clear_turn(&turn_context.sub_id);

        let (pending_items, parent_turn_id) = self
            .input_queue
            .get_pending_input_for_task_start(&self.active_turn)
            .await;
        // Pending-work wakes deliberately start RegularTask with empty explicit input and move
        // mailbox items through InputQueue. Classify the complete admitted task here, after that
        // move, so a terminal-result turn cannot masquerade as fresh operator work.
        let terminal_result_only =
            turn_inputs_are_terminal_result_only(input.iter().chain(pending_items.iter()));
        let parent_completion_expected =
            turn_inputs_expect_parent_completion(input.iter().chain(pending_items.iter()));
        turn_context.parent_completion_expected.store(
            parent_completion_expected,
            std::sync::atomic::Ordering::Release,
        );
        let native_auto_turn_guard = self
            .services
            .agent_control
            .begin_native_agent_turn(self.thread_id, terminal_result_only);
        if let (MailboxParentProvenance::Attribute, Some(id)) =
            (mailbox_parent_provenance, parent_turn_id)
        {
            turn_context.turn_metadata_state.set_parent_turn_id(id);
        }
        let turn_state = {
            let mut active = self.active_turn.lock().await;
            let turn = active.get_or_insert_with(ActiveTurn::default);
            debug_assert!(turn.task.is_none());
            Arc::clone(&turn.turn_state)
        };
        turn_state.lock().await.token_usage_at_turn_start = token_usage_at_turn_start.clone();
        self.input_queue
            .extend_pending_input_for_turn_state(turn_state.as_ref(), pending_items)
            .await;
        self.emit_turn_start_lifecycle(turn_context.as_ref(), &token_usage_at_turn_start)
            .await;

        let turn_extension_data = Arc::clone(&turn_context.extension_data);
        let mut active = self.active_turn.lock().await;
        let turn = active.get_or_insert_with(ActiveTurn::default);
        debug_assert!(turn.task.is_none());
        let done_clone = Arc::clone(&done);
        let session_ctx = Arc::new(SessionTaskContext::new(
            Arc::clone(self),
            Arc::clone(&turn_extension_data),
        ));
        let ctx = Arc::clone(&turn_context);
        let task_for_run = Arc::clone(&task);
        let task_input = input;
        let task_cancellation_token = cancellation_token.child_token();
        // Task-owned turn spans keep a core-owned span open for the
        // full task lifecycle after the submission dispatch span ends.
        let reasoning_effort = turn_context.effective_reasoning_effort_for_tracing();
        let task_span = info_span!(
            "turn",
            otel.name = span_name,
            thread.id = %self.thread_id,
            turn.id = %turn_context.sub_id,
            model = %turn_context.model_info.slug,
            codex.turn.reasoning_effort = %reasoning_effort,
            codex.turn.token_usage.input_tokens = field::Empty,
            codex.turn.token_usage.cached_input_tokens = field::Empty,
            codex.turn.token_usage.cache_write_input_tokens = field::Empty,
            codex.turn.token_usage.non_cached_input_tokens = field::Empty,
            codex.turn.token_usage.output_tokens = field::Empty,
            codex.turn.token_usage.reasoning_output_tokens = field::Empty,
            codex.turn.token_usage.total_tokens = field::Empty,
        );
        let handle = tokio::spawn(
            async move {
                let ctx_for_finish = Arc::clone(&ctx);
                // #434: vault values stored since the vault was last read
                // (before this process started, or by another process) are
                // protected before this task produces output.
                let session_for_refresh = session_ctx.clone_session();
                let run_session_ctx = Arc::clone(&session_ctx);
                let run_cancellation_token = task_cancellation_token.clone();
                let run = async move {
                    tokio::select! {
                        () = crate::security::disclosure_gate::refresh_vault_values_for_task(
                            &session_for_refresh,
                            ctx.as_ref(),
                        ) => {}
                        () = run_cancellation_token.cancelled() => {}
                    }
                    task_for_run
                        .run(
                            run_session_ctx,
                            ctx,
                            task_input,
                            run_cancellation_token.child_token(),
                        )
                        .instrument(trace_span!("session_task.run"))
                        .await
                };
                // #616: a panic inside the task must still finish the turn
                // (with an error), or every client waiting for its end (exec,
                // the TUI) waits forever.
                let task_result = std::panic::AssertUnwindSafe(run)
                    .catch_unwind()
                    .await
                    .unwrap_or_else(|_| Err(CodexErr::Fatal(TURN_PANICKED_MESSAGE.to_string())));
                let sess = session_ctx.clone_session();
                if let Err(err) = sess.flush_rollout().await {
                    warn!("failed to flush rollout before completing turn: {err}");
                    sess.send_event(
                        ctx_for_finish.as_ref(),
                        EventMsg::Warning(WarningEvent {
                            message: format!(
                                "Failed to save the conversation transcript; Codex will continue retrying. Error: {err}"
                            ),
                        }),
                    )
                    .await;
                }
                if !task_cancellation_token.is_cancelled() {
                    // Finish uniformly from the spawn site so all tasks share the same lifecycle.
                    sess.on_task_finished(Arc::clone(&ctx_for_finish), task_result)
                        .await;
                }
                done_clone.notify_waiters();
            }
            .instrument(task_span),
        );
        let timer = turn_context
            .session_telemetry
            .start_timer(TURN_E2E_DURATION_METRIC, &[])
            .ok();
        let running_task = RunningTask {
            done,
            handle: AbortOnDropHandle::new(handle),
            kind: task_kind,
            task,
            cancellation_token,
            turn_context: Arc::clone(&turn_context),
            turn_extension_data,
            _agent_execution_guard: agent_execution_guard,
            _native_auto_turn_guard: native_auto_turn_guard,
            _timer: timer,
        };
        turn.task = Some(running_task);
    }

    /// Returns whether an extension has marked this thread as durably asleep.
    pub(crate) fn has_outstanding_durable_sleep(&self) -> bool {
        self.services
            .thread_extension_data
            .get::<codex_extension_items::sleep::SleepItem>()
            .is_some()
    }

    /// Starts a regular turn when the session is idle and pending work is waiting.
    ///
    /// Pending work includes mailbox mail marked with `trigger_turn`, or any mailbox mail while
    /// an outstanding durable sleep is attached to the thread.
    ///
    /// This helper generates a fresh sub-id for the synthetic turn before delegating to the
    /// explicit-sub-id variant.
    pub(crate) fn maybe_start_turn_for_pending_work(self: &Arc<Self>) -> BoxFuture<'static, ()> {
        let session = Arc::clone(self);
        Box::pin(async move {
            session
                .maybe_start_turn_for_pending_work_with_sub_id(uuid::Uuid::new_v4().to_string())
                .await;
        })
    }

    /// Starts a regular turn with the provided sub-id when pending work should wake an idle
    /// session.
    ///
    /// The turn is created only when the session is idle and mailbox mail either requests a turn
    /// or can wake an outstanding durable sleep.
    pub(crate) async fn maybe_start_turn_for_pending_work_with_sub_id(
        self: &Arc<Self>,
        sub_id: String,
    ) {
        if !self.input_queue.has_pending_mailbox_items().await
            || (!self.input_queue.has_trigger_turn_mailbox_items().await
                && !self.has_outstanding_durable_sleep())
        {
            return;
        }
        // Issue #445: no turn is started for agent messages while the kill
        // switch is on; they wait until a person turns it off and a turn runs.
        if self.services.model_client().kill_switch_on() {
            return;
        }

        // A busy session cannot start another turn. Avoid preparing a turn and reading its
        // named account here; the locked reservation below still checks for a concurrent start.
        if self.active_turn.lock().await.is_some() {
            return;
        }

        let turn_context = self.new_default_turn_with_sub_id(sub_id.clone()).await;
        // #1331 (D171): nor while its named account is removed or signed out.
        if let Some(err) = crate::session::turn::turn_refusal(self, &turn_context).await {
            let message = format!(
                "Agent messages remain queued: {err}. After recovery, send another message or start a turn to retry."
            );
            self.send_event(
                &turn_context,
                EventMsg::Warning(WarningEvent {
                    message: message.clone(),
                }),
            )
            .await;
            if let Err(err) = self
                .services
                .agent_control
                .warn_parent_of_held_mailbox(self.thread_id, &turn_context.session_source, message)
                .await
            {
                debug!("failed to notify parent of held mailbox: {err}");
            }
            return;
        }
        let agent_execution_guard = match self.services.agent_control.try_execution_guard(
            turn_context.multi_agent_version,
            &turn_context.session_source,
        ) {
            Ok(guard) => guard,
            Err(err) if matches!(err.details(), CodexErrorDetails::AgentLimitReached { .. }) => {
                if self.input_queue.try_schedule_capacity_wait() {
                    let session = Arc::clone(self);
                    tokio::spawn(async move {
                        session
                            .services
                            .agent_control
                            .wait_for_execution_capacity()
                            .await;
                        session.input_queue.finish_capacity_wait();
                        session.submit_pending_work_wake().await;
                    });
                }
                return;
            }
            Err(err) => {
                warn!(%err, "failed to reserve execution capacity for pending mailbox work");
                return;
            }
        };

        {
            let mut active_turn = self.active_turn.lock().await;
            if active_turn.is_some() {
                return;
            }
            *active_turn = Some(ActiveTurn::default());
        }

        self.maybe_emit_model_warnings_for_turn(turn_context.as_ref())
            .await;
        self.start_task_with_execution_guard(
            turn_context,
            Vec::new(),
            RegularTask::new(),
            MailboxParentProvenance::Attribute,
            agent_execution_guard,
        )
        .await;
    }

    pub async fn abort_all_tasks(self: &Arc<Self>, reason: TurnAbortReason) {
        let mut aborted_turn = false;
        let mut active_turn_to_clear = None;
        let mut turn_context = None;
        if let Some(mut active_turn) = self.take_active_turn().await {
            let task = active_turn.task.take();
            aborted_turn = task.is_some();
            turn_context = task.as_ref().map(|task| Arc::clone(&task.turn_context));
            if let Some(task) = task {
                self.handle_task_abort(task, reason.clone()).await;
            }
            if aborted_turn {
                active_turn_to_clear = Some(active_turn);
            }
        }

        if let Some(turn_context) = turn_context.as_deref() {
            self.emit_turn_abort_lifecycle(reason.clone(), turn_context.extension_data.as_ref())
                .await;
        }
        if let Some(active_turn) = active_turn_to_clear {
            self.finish_turn_mailbox_messages(
                active_turn.turn_state.as_ref(),
                /*completed_naturally*/ false,
            )
            .await;
            // Let interrupted tasks observe cancellation before dropping pending approvals, or an
            // in-flight approval wait can surface as a model-visible rejection before TurnAborted.
            self.input_queue.clear_pending(&active_turn).await;
        }
        if reason == TurnAbortReason::Interrupted && aborted_turn {
            self.maybe_start_turn_for_pending_work().await;
        }
    }

    pub(crate) async fn abort_turn_if_active(
        self: &Arc<Self>,
        turn_id: &str,
        reason: TurnAbortReason,
    ) -> bool {
        let active_turn = {
            let mut active = self.active_turn.lock().await;
            if active
                .as_ref()
                .and_then(|active_turn| active_turn.task.as_ref())
                .is_some_and(|task| task.turn_context.sub_id == turn_id)
            {
                active.take()
            } else {
                None
            }
        };
        let Some(mut active_turn) = active_turn else {
            return false;
        };

        let task = active_turn.task.take();
        let turn_context = task.as_ref().map(|task| Arc::clone(&task.turn_context));
        if let Some(task) = task {
            self.handle_task_abort(task, reason.clone()).await;
        }
        if let Some(turn_context) = turn_context.as_deref() {
            self.emit_turn_abort_lifecycle(reason.clone(), turn_context.extension_data.as_ref())
                .await;
        }
        self.finish_turn_mailbox_messages(
            active_turn.turn_state.as_ref(),
            /*completed_naturally*/ false,
        )
        .await;
        // Let interrupted tasks observe cancellation before dropping pending approvals, or an
        // in-flight approval wait can surface as a model-visible rejection before TurnAborted.
        self.input_queue.clear_pending(&active_turn).await;

        if reason == TurnAbortReason::Interrupted {
            self.maybe_start_turn_for_pending_work().await;
        }

        true
    }

    pub async fn on_task_finished(
        self: &Arc<Self>,
        turn_context: Arc<TurnContext>,
        task_result: SessionTaskResult,
    ) {
        let (last_agent_message, abort_reason, completed_naturally) = match task_result {
            Ok(last_agent_message) => (last_agent_message, None, true),
            Err(err) if matches!(err.details(), CodexErrorDetails::TurnAborted) => {
                (None, Some(TurnAbortReason::Interrupted), false)
            }
            Err(err) => {
                warn!(%err, "session task returned an unexpected error");
                self.emit_turn_error_lifecycle(
                    turn_context.as_ref(),
                    err.to_codex_protocol_error(),
                )
                .await;
                self.track_turn_codex_error(turn_context.as_ref(), &err);
                self.send_event(
                    turn_context.as_ref(),
                    EventMsg::Error(err.to_error_event(/*message_prefix*/ None)),
                )
                .await;
                (None, None, false)
            }
        };
        turn_context
            .turn_metadata_state
            .cancel_git_enrichment_task();

        let turn_state = {
            let mut active = self.active_turn.lock().await;
            active.as_mut().and_then(|active_turn| {
                let task = active_turn.task.take()?;
                task.handle.detach();
                Some(Arc::clone(&active_turn.turn_state))
            })
        };
        let Some(turn_state) = turn_state else {
            return;
        };
        let pending_input = self
            .input_queue
            .take_pending_input_for_turn_state(turn_state.as_ref())
            .await;
        let (turn_had_memory_citation, turn_tool_calls, token_usage_at_turn_start) = {
            let ts = turn_state.lock().await;
            (
                ts.has_memory_citation,
                ts.tool_calls,
                ts.token_usage_at_turn_start.clone(),
            )
        };
        self.finish_turn_mailbox_messages(turn_state.as_ref(), completed_naturally)
            .await;
        // Queue-only mail deliberately does not wake an idle thread or join the immediately
        // following admitted request. Crossing this terminal boundary makes it eligible to join
        // the next human turn without creating a model-only continuation loop.
        self.input_queue.mark_mailbox_ready_for_next_turn().await;
        let mut follow_up_input = Vec::new();
        if !pending_input.is_empty() {
            for (pending_input_item, final_output_json_schema) in pending_input {
                if completed_naturally && pending_input_seeds_follow_up(&pending_input_item) {
                    follow_up_input.push((pending_input_item, final_output_json_schema));
                } else {
                    let hook_outcome =
                        inspect_pending_input(self, &turn_context, &pending_input_item).await;
                    if hook_outcome.should_stop {
                        record_additional_contexts(
                            self,
                            &turn_context,
                            hook_outcome.additional_contexts,
                        )
                        .await;
                    } else {
                        record_pending_input(
                            self,
                            &turn_context,
                            pending_input_item,
                            hook_outcome.additional_contexts,
                        )
                        .await;
                    }
                }
            }
        }
        // Emit token usage metrics.
        {
            // TODO(jif): drop this
            let tmp_mem = (
                "tmp_mem_enabled",
                if self.enabled(Feature::MemoryTool) {
                    "true"
                } else {
                    "false"
                },
            );
            let network_proxy = self.services.network_proxy.load_full();
            let network_proxy_active = match network_proxy.as_ref() {
                Some(started_network_proxy) => {
                    match started_network_proxy.proxy().current_cfg().await {
                        Ok(config) => config.enabled,
                        Err(err) => {
                            warn!(
                                "failed to read managed network proxy state for turn metrics: {err:#}"
                            );
                            false
                        }
                    }
                }
                None => false,
            };
            emit_turn_network_proxy_metric(
                &self.services.session_telemetry,
                network_proxy_active,
                tmp_mem,
            );
            self.services.session_telemetry.histogram(
                TURN_TOOL_CALL_METRIC,
                i64::try_from(turn_tool_calls).unwrap_or(i64::MAX),
                &[tmp_mem],
            );
            let total_token_usage = self.total_token_usage().await.unwrap_or_default();
            let turn_token_usage = TokenUsage {
                input_tokens: (total_token_usage.input_tokens
                    - token_usage_at_turn_start.input_tokens)
                    .max(0),
                cached_input_tokens: (total_token_usage.cached_input_tokens
                    - token_usage_at_turn_start.cached_input_tokens)
                    .max(0),
                cache_write_input_tokens: (total_token_usage.cache_write_input_tokens
                    - token_usage_at_turn_start.cache_write_input_tokens)
                    .max(0),
                output_tokens: (total_token_usage.output_tokens
                    - token_usage_at_turn_start.output_tokens)
                    .max(0),
                reasoning_output_tokens: (total_token_usage.reasoning_output_tokens
                    - token_usage_at_turn_start.reasoning_output_tokens)
                    .max(0),
                total_tokens: (total_token_usage.total_tokens
                    - token_usage_at_turn_start.total_tokens)
                    .max(0),
            };
            let current_span = Span::current();
            current_span.record(
                "codex.turn.token_usage.input_tokens",
                turn_token_usage.input_tokens,
            );
            current_span.record(
                "codex.turn.token_usage.cached_input_tokens",
                turn_token_usage.cached_input(),
            );
            current_span.record(
                "codex.turn.token_usage.cache_write_input_tokens",
                turn_token_usage.cache_write_input_tokens,
            );
            current_span.record(
                "codex.turn.token_usage.non_cached_input_tokens",
                turn_token_usage.non_cached_input(),
            );
            current_span.record(
                "codex.turn.token_usage.output_tokens",
                turn_token_usage.output_tokens,
            );
            current_span.record(
                "codex.turn.token_usage.reasoning_output_tokens",
                turn_token_usage.reasoning_output_tokens,
            );
            current_span.record(
                "codex.turn.token_usage.total_tokens",
                turn_token_usage.total_tokens,
            );
            self.services
                .analytics_events_client
                .track_turn_token_usage(TurnTokenUsageFact {
                    turn_id: turn_context.sub_id.clone(),
                    thread_id: self.thread_id.to_string(),
                    token_usage: turn_token_usage.clone(),
                });
            self.services.session_telemetry.histogram(
                TURN_TOKEN_USAGE_METRIC,
                turn_token_usage.total_tokens,
                &[("token_type", "total"), tmp_mem],
            );
            self.services.session_telemetry.histogram(
                TURN_TOKEN_USAGE_METRIC,
                turn_token_usage.input_tokens,
                &[("token_type", "input"), tmp_mem],
            );
            self.services.session_telemetry.histogram(
                TURN_TOKEN_USAGE_METRIC,
                turn_token_usage.cached_input(),
                &[("token_type", "cached_input"), tmp_mem],
            );
            self.services.session_telemetry.histogram(
                TURN_TOKEN_USAGE_METRIC,
                turn_token_usage.cache_write_input_tokens,
                &[("token_type", "cache_write_input"), tmp_mem],
            );
            self.services.session_telemetry.histogram(
                TURN_TOKEN_USAGE_METRIC,
                turn_token_usage.output_tokens,
                &[("token_type", "output"), tmp_mem],
            );
            self.services.session_telemetry.histogram(
                TURN_TOKEN_USAGE_METRIC,
                turn_token_usage.reasoning_output_tokens,
                &[("token_type", "reasoning_output"), tmp_mem],
            );
        }
        emit_turn_memory_metric(
            &self.services.session_telemetry,
            turn_context.config.features.enabled(Feature::MemoryTool),
            turn_context.config.memories.use_memories,
            turn_had_memory_citation,
        );
        let started_at = turn_context.turn_timing_state.started_at_unix_secs().await;
        let (completed_at, duration_ms, profile) = turn_context
            .turn_timing_state
            .complete_profile_and_duration_ms()
            .await;
        self.services
            .analytics_events_client
            .track_turn_profile(TurnProfileFact {
                turn_id: turn_context.sub_id.clone(),
                profile,
            });
        // #874 (D6): a process an agent command left running may have
        // changed a git folder this session watches since its last look;
        // the person is told before the turn ends.
        if let Some(level) = crate::security::protected_additional::protected_level(self)
            .filter(|level| *level != codex_security_policy::SecurityLevel::Permissive)
        {
            let thread = self.thread_id;
            let level = format!("{level:?}");
            // A failed look is told too.
            let message = tokio::task::spawn_blocking(move || {
                crate::security::git_postcheck::recheck(thread, &level, &[])
            })
            .await
            .map_or_else(
                |_| Some(crate::security::git_postcheck::CHECK_FAILED.to_string()),
                |(message, _)| message,
            );
            if let Some(message) = message {
                self.send_event(
                    turn_context.as_ref(),
                    EventMsg::Warning(codex_protocol::protocol::WarningEvent { message }),
                )
                .await;
            }
        }
        let event = if let Some(reason) = abort_reason {
            self.emit_turn_abort_lifecycle(reason.clone(), turn_context.extension_data.as_ref())
                .await;
            EventMsg::TurnAborted(TurnAbortedEvent {
                turn_id: Some(turn_context.sub_id.clone()),
                reason,
                started_at,
                completed_at,
                duration_ms,
            })
        } else {
            let time_to_first_token_ms = turn_context
                .turn_timing_state
                .time_to_first_token_ms()
                .await;
            let error = turn_context.terminal_error.lock().await.clone();
            self.emit_turn_stop_lifecycle(turn_context.extension_data.as_ref())
                .await;
            EventMsg::TurnComplete(TurnCompleteEvent {
                turn_id: turn_context.sub_id.clone(),
                last_agent_message,
                error,
                started_at,
                completed_at,
                duration_ms,
                time_to_first_token_ms,
            })
        };
        self.send_event(turn_context.as_ref(), event).await;
        self.services
            .guardian_rejection_circuit_breaker
            .lock()
            .await
            .clear_turn(&turn_context.sub_id);

        let cleared_active_turn = {
            let mut active = self.active_turn.lock().await;
            if let Some(active_turn) = active.as_ref()
                && active_turn.task.is_none()
                && Arc::ptr_eq(&active_turn.turn_state, &turn_state)
            {
                *active = None;
                true
            } else {
                false
            }
        };
        // Regular items were flushed before this terminal event was appended; buffering
        // thread writers may not flush it without another explicit barrier.
        if let Err(err) = self.flush_rollout().await {
            warn!("failed to flush rollout after emitting terminal turn event: {err}");
        }
        // A completed task owns the pending inputs it detached above. Dispatch them even if a
        // concurrent lifecycle update replaced the active-turn slot while terminal hooks ran;
        // coupling delivery to slot cleanup loses accepted user input during that race.
        if !follow_up_input.is_empty() {
            self.submit_follow_up_input(follow_up_input).await;
            return;
        }
        if cleared_active_turn {
            self.maybe_start_turn_for_pending_work().await;
            self.emit_thread_idle_lifecycle_if_idle().await;
        }
    }

    async fn finish_turn_mailbox_messages(
        &self,
        turn_state: &tokio::sync::Mutex<crate::state::TurnState>,
        completed_naturally: bool,
    ) {
        let message_ids = turn_state.lock().await.take_mailbox_message_ids();
        if message_ids.is_empty() {
            return;
        }
        let Some(state_db) = self.state_db() else {
            return;
        };
        for message_id in message_ids {
            let result = if completed_naturally {
                state_db
                    .mark_agent_message_completed(
                        &message_id,
                        crate::turn_timing::now_unix_timestamp_ms(),
                    )
                    .await
            } else {
                state_db
                    .transition_agent_message(
                        &message_id,
                        codex_state::AgentMailboxPhase::ProviderRunning,
                        codex_state::AgentMailboxPhase::UnknownOutcome,
                        crate::turn_timing::now_unix_timestamp_ms(),
                    )
                    .await
            };
            if let Err(err) = result {
                warn!(
                    %err,
                    %message_id,
                    completed_naturally,
                    "failed to finalize agent mailbox message with its turn"
                );
            }
        }
    }

    async fn take_active_turn(&self) -> Option<ActiveTurn> {
        let mut active = self.active_turn.lock().await;
        active.take()
    }

    pub(crate) async fn close_unified_exec_processes(&self) {
        self.services
            .unified_exec_manager
            .terminate_all_processes()
            .await;
    }

    pub(crate) async fn list_background_terminals(&self) -> Vec<BackgroundTerminalInfo> {
        self.services.unified_exec_manager.list_processes().await
    }

    pub(crate) async fn terminate_background_terminal(&self, process_id: i32) -> bool {
        self.services
            .unified_exec_manager
            .terminate_process(process_id)
            .await
    }

    async fn submit_follow_up_input(
        &self,
        follow_up_input: Vec<(TurnInput, Option<serde_json::Value>)>,
    ) {
        for (input, final_output_json_schema) in follow_up_input {
            let (op, client_user_message_id) = match input {
                TurnInput::UserInput { content, client_id } => (
                    Op::UserInput {
                        items: content,
                        final_output_json_schema,
                        responsesapi_client_metadata: None,
                        additional_context: Default::default(),
                        thread_settings: Default::default(),
                    },
                    client_id,
                ),
                TurnInput::InterAgentCommunication(mut communication) => {
                    communication.trigger_turn = true;
                    (
                        Op::InterAgentCommunication { communication },
                        /*client_user_message_id*/ None,
                    )
                }
                TurnInput::ResponseItem(_) => continue,
            };
            if let Err(err) = self
                .submit_internal_follow_up(op, client_user_message_id)
                .await
            {
                warn!(%err, "failed to submit follow-up input after task finished");
                continue;
            }
        }
    }

    async fn submit_pending_work_wake(&self) {
        if !self.input_queue.has_trigger_turn_mailbox_items().await {
            return;
        }
        if let Err(err) = self
            .submit_internal_follow_up(Op::WakePendingWork, /*client_user_message_id*/ None)
            .await
        {
            warn!(%err, "failed to submit pending-work wake after task finished");
        }
    }

    async fn handle_task_abort(self: &Arc<Self>, mut task: RunningTask, reason: TurnAbortReason) {
        let sub_id = task.turn_context.sub_id.clone();
        if task.cancellation_token.is_cancelled() {
            return;
        }

        trace!(task_kind = ?task.kind, sub_id, "aborting running task");
        let cancelled_at = Instant::now();
        task.cancellation_token.cancel();
        task.turn_context
            .turn_metadata_state
            .cancel_git_enrichment_task();
        let session_task = task.task;

        let hard_aborted = select! {
            _ = task.done.notified() => false,
            _ = tokio::time::sleep(Duration::from_millis(GRACEFULL_INTERRUPTION_TIMEOUT_MS)) => {
                warn!("task {sub_id} didn't complete gracefully after {}ms", GRACEFULL_INTERRUPTION_TIMEOUT_MS);
                true
            }
        };

        task.handle.abort();
        if hard_aborted {
            // The turn did not record its tool outputs in time. Once it has stopped,
            // record each pending call's output, as the turn would have: the tool's
            // own output if it finished (D244), else its abort output (#1467).
            // Joining waits only for the turn's current poll to return, so the turn
            // cannot record an output after the check below. The join is skipped only
            // when the turn task aborts itself; aborts from tasks the turn spawned must
            // run in a detached task (as guardian's does), or the join cancels them.
            if tokio::task::try_id() != Some(task.handle.abort_handle().id()) {
                let _ = (&mut task.handle).await;
            }
            let mut outputs: Vec<ResponseItem> = task
                .turn_context
                .pending_tool_calls
                .take_unrecorded_outputs(cancelled_at)
                .into_iter()
                .map(ResponseItem::from)
                .collect();
            self.retain_unrecorded_tool_outputs(&mut outputs).await;
            if !outputs.is_empty() {
                self.record_conversation_items(task.turn_context.as_ref(), &outputs)
                    .await;
                // A finished call's real output is recorded here instead of by the
                // turn (D244), so apply the turn's external-context check too.
                for output in &outputs {
                    crate::stream_events_utils::mark_thread_memory_mode_polluted_if_external_context(
                        self,
                        task.turn_context.as_ref(),
                        output,
                    )
                    .await;
                }
            }
            // D282: the turn did not reach its end, which sends the turn's
            // file-change summary; send it here.
            if let Some(tracker) = task.turn_context.pending_tool_calls.turn_diff_tracker() {
                let unified_diff = tracker.lock().await.get_unified_diff();
                if let Some(unified_diff) = unified_diff {
                    self.send_event(
                        task.turn_context.as_ref(),
                        EventMsg::TurnDiff(TurnDiffEvent { unified_diff }),
                    )
                    .await;
                }
            }
        }

        let session_ctx = Arc::new(SessionTaskContext::new(
            Arc::clone(self),
            Arc::clone(&task.turn_extension_data),
        ));
        session_task
            .abort(session_ctx, Arc::clone(&task.turn_context))
            .await;

        if reason == TurnAbortReason::Interrupted
            && let Some(marker) = interrupted_turn_history_marker(
                InterruptedTurnHistoryMarker::from_config_and_version(
                    task.turn_context.config.as_ref(),
                    task.turn_context.multi_agent_version,
                ),
            )
        {
            self.record_conversation_items_from(
                task.turn_context.as_ref(),
                std::slice::from_ref(&marker),
                Some(crate::security::ingress::MessageOrigin::Host),
            )
            .await;
            // Ensure the marker is durably visible before emitting TurnAborted: some clients
            // synchronously re-read the rollout on receipt of the abort event.
            if let Err(err) = self.flush_rollout().await {
                warn!("failed to flush interrupted-turn marker before emitting TurnAborted: {err}");
            }
        }

        let started_at = task
            .turn_context
            .turn_timing_state
            .started_at_unix_secs()
            .await;
        let (completed_at, duration_ms, profile) = task
            .turn_context
            .turn_timing_state
            .complete_profile_and_duration_ms()
            .await;
        self.services
            .analytics_events_client
            .track_turn_profile(TurnProfileFact {
                turn_id: task.turn_context.sub_id.clone(),
                profile,
            });
        let event = EventMsg::TurnAborted(TurnAbortedEvent {
            turn_id: Some(task.turn_context.sub_id.clone()),
            reason,
            started_at,
            completed_at,
            duration_ms,
        });
        self.send_event(task.turn_context.as_ref(), event).await;
        self.services
            .guardian_rejection_circuit_breaker
            .lock()
            .await
            .clear_turn(&task.turn_context.sub_id);
        // Regular items were flushed before this terminal event was appended; buffering
        // thread writers may not flush it without another explicit barrier.
        if let Err(err) = self.flush_rollout().await {
            warn!("failed to flush rollout after emitting terminal turn event: {err}");
        }
    }
}

fn pending_input_seeds_follow_up(input: &TurnInput) -> bool {
    match input {
        TurnInput::UserInput { content, .. } => !content.is_empty(),
        TurnInput::InterAgentCommunication(_) => true,
        TurnInput::ResponseItem(_) => false,
    }
}

#[cfg(test)]
#[path = "mod_tests.rs"]
mod tests;
