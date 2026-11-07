use std::collections::HashMap;
use std::sync::Arc;
use std::sync::OnceLock;
use std::sync::atomic::AtomicBool;
use std::sync::atomic::Ordering;
use std::time::Instant;

use sha2::Digest;
use sha2::Sha256;
use tokio::sync::RwLock;
use tokio::task::JoinError;
use tokio_util::either::Either;
use tokio_util::sync::CancellationToken;
use tokio_util::task::AbortOnDropHandle;
use tracing::Instrument;
use tracing::info;
use tracing::instrument;
use tracing::trace_span;

use crate::function_tool::FunctionCallError;
use crate::session::session::Session;
use crate::session::step_context::StepContext;
use crate::tools::context::AbortedToolOutput;
use crate::tools::context::SharedTurnDiffTracker;
use crate::tools::context::ToolPayload;
use crate::tools::lifecycle::notify_tool_aborted;
use crate::tools::registry::AnyToolResult;
use crate::tools::registry::ToolArgumentDiffConsumer;
use crate::tools::router::ToolCall;
use crate::tools::router::ToolCallSource;
use crate::tools::router::ToolRouter;
use codex_protocol::error::CodexErr;
#[cfg(test)]
use codex_protocol::error::CodexErrorDetails;
use codex_protocol::models::ResponseInputItem;

const MAX_IDENTICAL_TOOL_CALLS_PER_TURN: u8 = 3;
/// Times in a row one exact direct call may run across the model requests of a
/// turn before further repeats are refused without running.
const MAX_CONSECUTIVE_IDENTICAL_TOOL_CALLS: u32 = 3;
/// Times one exact direct call may return the same result within a turn before
/// further repeats are refused. Catches loops that cycle through several calls,
/// which never repeat one call twice in a row.
const MAX_IDENTICAL_TOOL_RESULTS: u32 = 3;
/// Refused repeats after which the turn stops: the model has ignored the
/// refusals and is stuck.
const STOP_TURN_AFTER_REFUSED_REPEATS: u32 = 8;

/// Direct tool calls of a turn whose output may not be recorded yet. When an
/// interrupt hard-aborts a turn that missed the grace period, the abort path
/// records each call's abort output from here, so the model still sees how
/// long the call ran instead of the bare `aborted` placeholder that history
/// normalization would insert (#1467). A call whose tool already finished keeps
/// its real output, so a finished-then-killed call (for example a patch that was
/// applied) is not reported as aborted (D244).
#[derive(Default)]
pub(crate) struct PendingToolCalls {
    calls: std::sync::Mutex<Vec<PendingToolCall>>,
    /// The running turn's file-change tracker, so the abort path can send the
    /// turn's file-change summary as a regular turn end does (D282).
    turn_diff_tracker: std::sync::Mutex<Option<SharedTurnDiffTracker>>,
}

impl std::fmt::Debug for PendingToolCalls {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PendingToolCalls")
            .field("calls", &*self.lock())
            .finish_non_exhaustive()
    }
}

#[derive(Debug)]
struct PendingToolCall {
    call: ToolCall,
    started: Instant,
    finished_output: Option<ResponseInputItem>,
}

impl PendingToolCalls {
    fn register(&self, call: &ToolCall, started: Instant) {
        self.lock().push(PendingToolCall {
            call: call.clone(),
            started,
            finished_output: None,
        });
    }

    /// Keeps the output a call's tool returned until the turn records it.
    fn finish(&self, call_id: &str, output: &ResponseInputItem) {
        if let Some(pending) = self
            .lock()
            .iter_mut()
            .find(|pending| pending.call.call_id == call_id)
        {
            pending.finished_output = Some(output.clone());
        }
    }

    /// Forgets a call once its output is in history.
    pub(crate) fn complete(&self, call_id: &str) {
        self.lock()
            .retain(|pending| pending.call.call_id != call_id);
    }

    /// Takes the outputs of every remaining call: the tool's own output when it
    /// finished, else its abort output timed up to `cancelled_at`.
    pub(crate) fn take_unrecorded_outputs(&self, cancelled_at: Instant) -> Vec<ResponseInputItem> {
        std::mem::take(&mut *self.lock())
            .into_iter()
            .map(|pending| {
                pending.finished_output.unwrap_or_else(|| {
                    let secs = cancelled_at
                        .saturating_duration_since(pending.started)
                        .as_secs_f32()
                        .max(0.1);
                    ToolCallRuntime::aborted_response(&pending.call, secs).into_response()
                })
            })
            .collect()
    }

    /// Remembers the running turn's file-change tracker (D282).
    pub(crate) fn set_turn_diff_tracker(&self, tracker: &SharedTurnDiffTracker) {
        *self
            .turn_diff_tracker
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(Arc::clone(tracker));
    }

    /// The tracker [`Self::set_turn_diff_tracker`] stored, if any.
    pub(crate) fn turn_diff_tracker(&self) -> Option<SharedTurnDiffTracker> {
        self.turn_diff_tracker
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Vec<PendingToolCall>> {
        self.calls
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

struct ToolCallTimingGuard {
    started_at: Instant,
    execution_started_at: Arc<OnceLock<Instant>>,
    conversation_id: String,
    turn_id: String,
    call_id: String,
    tool_name: codex_tools::ToolName,
}

#[derive(Clone)]
pub(crate) struct ToolCallRuntime {
    router: Arc<ToolRouter>,
    session: Arc<Session>,
    // Tool calls may run later, so retain the step whose tool list advertised them.
    step_context: Arc<StepContext>,
    tracker: SharedTurnDiffTracker,
    parallel_execution: Arc<RwLock<()>>,
    tool_call_counts: Arc<RwLock<HashMap<String, u8>>>,
}

pub(crate) trait IntoToolStepContext {
    fn into_tool_step_context(self) -> Arc<StepContext>;
}

impl IntoToolStepContext for Arc<StepContext> {
    fn into_tool_step_context(self) -> Arc<StepContext> {
        self
    }
}

#[cfg(test)]
impl IntoToolStepContext for Arc<crate::session::turn_context::TurnContext> {
    fn into_tool_step_context(self) -> Arc<StepContext> {
        StepContext::for_test(self)
    }
}

impl ToolCallRuntime {
    pub(crate) fn new(
        router: Arc<ToolRouter>,
        session: Arc<Session>,
        step_context: impl IntoToolStepContext,
        tracker: SharedTurnDiffTracker,
    ) -> Self {
        let step_context = step_context.into_tool_step_context();
        Self {
            router,
            session,
            step_context,
            tracker,
            parallel_execution: Arc::new(RwLock::new(())),
            tool_call_counts: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    pub(crate) fn create_diff_consumer(
        &self,
        tool_name: &codex_tools::ToolName,
    ) -> Option<Box<dyn ToolArgumentDiffConsumer>> {
        self.router.create_diff_consumer(tool_name)
    }

    #[instrument(level = "trace", skip_all)]
    pub(crate) fn handle_tool_call(
        self,
        call: ToolCall,
        cancellation_token: CancellationToken,
    ) -> impl std::future::Future<Output = Result<ResponseInputItem, CodexErr>> {
        let call = self.router.resolve_flat_tool_call(call);
        let error_call = call.clone();
        let source = call.direct_source();
        let pending_tool_calls = Arc::clone(&self.step_context.turn.pending_tool_calls);
        pending_tool_calls.register(&call, Instant::now());
        let future = self.handle_tool_call_with_source(call, source, cancellation_token);
        async move {
            let call_id = error_call.call_id.clone();
            let output = match future.await {
                Ok(response) => response.into_response(),
                Err(FunctionCallError::Fatal(message)) => return Err(CodexErr::Fatal(message)),
                Err(other) => Self::failure_response(error_call, other),
            };
            pending_tool_calls.finish(&call_id, &output);
            Ok(output)
        }
        .in_current_span()
    }

    #[instrument(level = "trace", skip_all)]
    pub(crate) fn handle_tool_call_with_source(
        self,
        call: ToolCall,
        source: ToolCallSource,
        cancellation_token: CancellationToken,
    ) -> impl std::future::Future<Output = Result<AnyToolResult, FunctionCallError>> {
        let call = self.router.resolve_flat_tool_call(call);
        if self
            .step_context
            .turn
            .config
            .features
            .enabled(codex_features::Feature::ExecutedToolCallMetadata)
            && let Some(executed_tool_calls) = self.session.services.executed_tool_calls.as_ref()
        {
            executed_tool_calls.record_tool_call(
                &call,
                &source,
                super::effective_tool_mode(&self.step_context.turn),
            );
        }
        let supports_parallel = self.router.tool_supports_parallel(&call);
        let tool_runtime = self.router.tool_runtime(&call);
        let router = Arc::clone(&self.router);
        let session = Arc::clone(&self.session);
        let step_context = Arc::clone(&self.step_context);
        let turn = Arc::clone(&step_context.turn);
        let tracker = Arc::clone(&self.tracker);
        let lock = Arc::clone(&self.parallel_execution);
        let invocation_cancellation_token = cancellation_token.clone();
        let wait_for_runtime_cancellation = self.router.tool_waits_for_runtime_cancellation(&call);
        let started = Instant::now();
        let tool_call_counts = Arc::clone(&self.tool_call_counts);
        let tool_signature = tool_call_signature(&call);
        let guards_repeats = matches!(source, ToolCallSource::Direct)
            && !tool_runtime
                .as_ref()
                .is_some_and(|runtime| runtime.repeated_identical_calls_are_polling());
        let tool_call_timing_guard =
            ToolCallTimingGuard::capture(started, &session.thread_id, &turn.sub_id, &call, &source);
        let execution_started_at = tool_call_timing_guard
            .as_ref()
            .map(|timing| Arc::clone(&timing.execution_started_at));
        let abort_session = Arc::clone(&session);
        let abort_source = source.clone();
        let abort_turn = Arc::clone(&turn);
        let terminal_outcome_reached = Arc::new(AtomicBool::new(false));
        let dispatch_terminal_outcome_reached = Arc::clone(&terminal_outcome_reached);
        let dispatch_call = call.clone();

        let dispatch_span = trace_span!(
            "dispatch_tool_call_with_code_mode_result",
            otel.name = %call.tool_name,
            tool_name = %call.tool_name,
            call_id = call.call_id.as_str(),
            aborted = false,
        );
        let abort_dispatch_span = dispatch_span.clone();

        let mut dispatch_handle: AbortOnDropHandle<Result<AnyToolResult, FunctionCallError>> =
            AbortOnDropHandle::new(tokio::spawn(async move {
                let identical_count = {
                    let mut counts = tool_call_counts.write().await;
                    let count = counts.entry(tool_signature.clone()).or_insert(0);
                    *count = count.saturating_add(1);
                    *count
                };
                if identical_count > MAX_IDENTICAL_TOOL_CALLS_PER_TURN {
                    return Err(FunctionCallError::RespondToModel(format!(
                        "repeated identical tool call stopped after {MAX_IDENTICAL_TOOL_CALLS_PER_TURN} attempts: {}. Change approach; do not retry the same tool payload again.",
                        dispatch_call.tool_name
                    )));
                }
                if guards_repeats {
                    let repetition = turn.record_direct_tool_call(&tool_signature).await;
                    if let Some(refusal) = repeated_call_refusal(&dispatch_call, repetition) {
                        let refusals = turn.record_repeated_tool_call_refusal();
                        if refusals >= STOP_TURN_AFTER_REFUSED_REPEATS {
                            return Err(FunctionCallError::Fatal(format!(
                                "stopped the turn after refusing {refusals} repeated tool calls; \
                                 the model kept repeating calls whose results could not change \
                                 (last: `{}`)",
                                dispatch_call.tool_name
                            )));
                        }
                        return Err(FunctionCallError::RespondToModel(refusal));
                    }
                }
                if let Some(tool_runtime) = tool_runtime
                    && let Some(readiness) = tool_runtime.wait_until_ready(&session)
                {
                    readiness.await;
                }

                let _guard = if supports_parallel {
                    Either::Left(lock.read().await)
                } else {
                    Either::Right(lock.write().await)
                };
                // Admission through the parallel-execution gate marks the end
                // of dispatch waiting and the start of handler execution.
                if let Some(execution_started_at) = execution_started_at {
                    let _ = execution_started_at.set(Instant::now());
                }

                let result = router
                    .dispatch_tool_call_with_terminal_outcome(
                        session,
                        step_context,
                        invocation_cancellation_token,
                        tracker,
                        dispatch_call,
                        source,
                        dispatch_terminal_outcome_reached,
                    )
                    .instrument(dispatch_span.clone())
                    .await;
                if guards_repeats && let Some(identity) = result_identity(&result) {
                    turn.record_direct_tool_result(tool_signature, identity)
                        .await;
                }
                match result {
                    Err(FunctionCallError::MalformedToolCall {
                        diagnostic,
                        message,
                    }) => {
                        let signature = malformed_tool_call_signature(&diagnostic);
                        let count = turn.record_malformed_tool_call(signature).await;
                        if count > 1 {
                            Err(FunctionCallError::Fatal(format!(
                                "stopped the turn after {count} equivalent malformed tool calls \
                                 for `{}` (category={}); the model did not correct the tool payload \
                                 after one retry",
                                diagnostic.tool, diagnostic.category
                            )))
                        } else {
                            Err(FunctionCallError::MalformedToolCall {
                                diagnostic,
                                message,
                            })
                        }
                    }
                    other => other,
                }
            }));

        async move {
            let _tool_call_timing_guard = tool_call_timing_guard;
            tokio::select! {
                res = &mut dispatch_handle => res.map_err(Self::tool_task_join_error)?,
                _ = cancellation_token.cancelled() => {
                    if terminal_outcome_reached.load(Ordering::Acquire) || dispatch_handle.is_finished() {
                        dispatch_handle.await.map_err(Self::tool_task_join_error)?
                    } else {
                        let secs = started.elapsed().as_secs_f32().max(0.1);
                        abort_dispatch_span.record("aborted", true);
                        if wait_for_runtime_cancellation {
                            if terminal_outcome_reached.swap(true, Ordering::AcqRel) {
                                return dispatch_handle.await.map_err(Self::tool_task_join_error)?;
                            }
                            // The abort owns the terminal outcome; await only so
                            // the runtime can finish process teardown.
                            match dispatch_handle.await {
                                Ok(_) => {}
                                Err(err) if err.is_cancelled() => {}
                                Err(err) => return Err(Self::tool_task_join_error(err)),
                            }
                        } else {
                            dispatch_handle.abort();
                            match dispatch_handle.await {
                                Ok(result) => return result,
                                Err(err) if err.is_cancelled() => {}
                                Err(err) => return Err(Self::tool_task_join_error(err)),
                            }
                        }
                        let response = Self::aborted_response(&call, secs);
                        notify_tool_aborted(
                            abort_session.as_ref(),
                            abort_turn.as_ref(),
                            call.call_id.as_str(),
                            &call.tool_name,
                            abort_source,
                        )
                        .await;
                        Ok(response)
                    }
                },
            }
        }
        .in_current_span()
    }
}

/// Returns the refusal for a direct call that cannot tell the model anything
/// new: it already ran `MAX_CONSECUTIVE_IDENTICAL_TOOL_CALLS` times in a row, or
/// it already returned the same result `MAX_IDENTICAL_TOOL_RESULTS` times this
/// turn. Re-running a command after an edit that changes its result is
/// unaffected.
fn repeated_call_refusal(
    call: &ToolCall,
    repetition: crate::session::turn_context::ToolCallRepetition,
) -> Option<String> {
    if repetition.consecutive_calls > MAX_CONSECUTIVE_IDENTICAL_TOOL_CALLS {
        return Some(format!(
            "Not run: this exact `{}` call has already run {MAX_CONSECUTIVE_IDENTICAL_TOOL_CALLS} \
             times in a row and its result is in the conversation above. Repeating it will not \
             produce new information. Use that result, or take a different action.",
            call.tool_name
        ));
    }
    if repetition.identical_results >= MAX_IDENTICAL_TOOL_RESULTS {
        return Some(format!(
            "Not run: this exact `{}` call has already returned the same result \
             {MAX_IDENTICAL_TOOL_RESULTS} times in this turn without any new result in \
             between, so running it again will not produce new information. Use the result in \
             the conversation above, or take a different action.",
            call.tool_name
        ));
    }
    None
}

/// Identity of a dispatched call's result for repeat detection, or `None` when
/// the outcome carries no model-visible result.
fn result_identity(result: &Result<AnyToolResult, FunctionCallError>) -> Option<String> {
    let identity = match result {
        Ok(result) => result.result.result_identity(&result.payload).to_string(),
        Err(FunctionCallError::RespondToModel(message)) => format!("error:{message}"),
        Err(_) => return None,
    };
    let digest = Sha256::digest(identity.as_bytes());
    Some(digest.iter().map(|byte| format!("{byte:02x}")).collect())
}

fn tool_call_signature(call: &ToolCall) -> String {
    let mut hasher = Sha256::new();
    hasher.update(call.tool_name.to_string().as_bytes());
    hasher.update(b"\0");
    hasher.update(call.payload.log_payload().as_bytes());
    let digest = hasher.finalize();
    let payload_hash = digest
        .iter()
        .take(8)
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    format!("{}:{payload_hash}", call.tool_name)
}

fn malformed_tool_call_signature(diagnostic: &codex_tools::MalformedToolCallDiagnostic) -> String {
    let normalized_excerpt = diagnostic
        .excerpt
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    let mut hasher = Sha256::new();
    hasher.update(diagnostic.tool.as_bytes());
    hasher.update(b"\0");
    hasher.update(diagnostic.category.as_bytes());
    hasher.update(b"\0");
    hasher.update(normalized_excerpt.as_bytes());
    let digest = hasher.finalize();
    digest
        .iter()
        .take(16)
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

impl ToolCallRuntime {
    fn tool_task_join_error(err: JoinError) -> FunctionCallError {
        FunctionCallError::Fatal(format!("tool task failed to receive: {err:?}"))
    }

    fn failure_response(call: ToolCall, err: FunctionCallError) -> ResponseInputItem {
        let message = err.to_string();
        match call.payload {
            ToolPayload::ToolSearch { .. } => ResponseInputItem::ToolSearchOutput {
                call_id: call.call_id,
                status: "completed".to_string(),
                execution: "client".to_string(),
                tools: Vec::new(),
            },
            ToolPayload::Custom { .. } => ResponseInputItem::CustomToolCallOutput {
                call_id: call.call_id,
                name: None,
                output: codex_protocol::models::FunctionCallOutputPayload {
                    body: codex_protocol::models::FunctionCallOutputBody::Text(message),
                    success: Some(false),
                },
            },
            _ => ResponseInputItem::FunctionCallOutput {
                call_id: call.call_id,
                output: codex_protocol::models::FunctionCallOutputPayload {
                    body: codex_protocol::models::FunctionCallOutputBody::Text(message),
                    success: Some(false),
                },
            },
        }
    }

    fn aborted_response(call: &ToolCall, secs: f32) -> AnyToolResult {
        AnyToolResult {
            call_id: call.call_id.clone(),
            payload: call.payload.clone(),
            result: Box::new(AbortedToolOutput {
                message: Self::abort_message(call, secs),
            }),
            post_tool_use_payload: None,
        }
    }

    fn abort_message(call: &ToolCall, secs: f32) -> String {
        if call.tool_name.namespace.is_none()
            && matches!(
                call.tool_name.name.as_str(),
                "shell_command" | "unified_exec"
            )
        {
            format!("Wall time: {secs:.1} seconds\naborted by user")
        } else {
            format!("aborted by user after {secs:.1}s")
        }
    }
}

impl ToolCallTimingGuard {
    fn capture(
        started_at: Instant,
        conversation_id: &impl std::fmt::Display,
        turn_id: &str,
        call: &ToolCall,
        source: &ToolCallSource,
    ) -> Option<Self> {
        // Code-mode calls are nested within a direct code-mode tool call whose
        // timing already includes them. Suppress nested guards so consumers do
        // not mistake overlapping events for independent tool-call latency.
        if !matches!(
            source,
            ToolCallSource::Direct | ToolCallSource::DirectPlaintextMessage
        ) || !tracing::enabled!(tracing::Level::INFO)
        {
            return None;
        }

        Some(Self {
            started_at,
            execution_started_at: Arc::new(OnceLock::new()),
            conversation_id: conversation_id.to_string(),
            turn_id: turn_id.to_string(),
            call_id: call.call_id.clone(),
            tool_name: call.tool_name.clone(),
        })
    }
}

impl Drop for ToolCallTimingGuard {
    fn drop(&mut self) {
        let completed_at = Instant::now();
        // Snapshot once so a concurrently-starting dispatch cannot make one
        // event internally inconsistent.
        let execution_started_at = self
            .execution_started_at
            .get()
            .copied()
            .filter(|execution_started_at| *execution_started_at <= completed_at);
        let duration_ms = |duration: std::time::Duration| u64::try_from(duration.as_millis()).ok();
        let total_duration_ms = duration_ms(completed_at.duration_since(self.started_at));
        let dispatch_duration_ms = execution_started_at.map_or_else(
            || total_duration_ms,
            |execution_started_at| {
                duration_ms(execution_started_at.duration_since(self.started_at))
            },
        );
        let handler_duration_ms = execution_started_at.map_or(Some(0), |execution_started_at| {
            duration_ms(completed_at.duration_since(execution_started_at))
        });

        macro_rules! log_tool_call {
            ($dispatch_duration_ms:expr, $handler_duration_ms:expr, $total_duration_ms:expr) => {
                info!(
                    event.name = "codex.tool_call",
                    trace_id = %codex_otel::current_span_trace_id().unwrap_or_default(),
                    conversation.id = %self.conversation_id,
                    turn_id = %self.turn_id,
                    tool_name = %self.tool_name,
                    call_id = %self.call_id,
                    tool_source = "direct",
                    execution_started = execution_started_at.is_some(),
                    dispatch_duration_ms = $dispatch_duration_ms,
                    handler_duration_ms = $handler_duration_ms,
                    total_duration_ms = $total_duration_ms,
                    "tool call completed"
                );
            };
        }

        match (dispatch_duration_ms, handler_duration_ms, total_duration_ms) {
            (Some(dispatch_duration_ms), Some(handler_duration_ms), Some(total_duration_ms)) => {
                log_tool_call!(dispatch_duration_ms, handler_duration_ms, total_duration_ms);
            }
            _ => {
                log_tool_call!(
                    tracing::field::Empty,
                    tracing::field::Empty,
                    tracing::field::Empty
                );
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    use crate::session::step_context::StepContext;
    use crate::tools::context::FunctionToolOutput;
    use crate::tools::context::ToolInvocation;
    use crate::tools::registry::CoreToolRuntime;
    use crate::tools::registry::ToolExecutor;
    use crate::tools::registry::ToolRegistry;
    use crate::turn_diff_tracker::TurnDiffTracker;
    use codex_extension_api::ToolCallOutcome;
    use codex_protocol::models::FunctionCallOutputBody;
    use codex_protocol::models::FunctionCallOutputPayload;
    use pretty_assertions::assert_eq;
    use tokio::sync::Notify;
    use tokio::sync::oneshot;
    use tracing_test::internal::MockWriter;

    #[test]
    fn tool_call_timing_guard_ignores_code_mode_source() {
        let subscriber = tracing_subscriber::fmt()
            .with_max_level(tracing::Level::INFO)
            .finish();
        tracing::subscriber::with_default(subscriber, || {
            let call = ToolCall {
                tool_name: codex_tools::ToolName::plain("test_tool"),
                call_id: "call-1".to_string(),
                payload: ToolPayload::Function {
                    arguments: "{}".to_string(),
                },
                encrypted_function_args: None,
            };
            let direct_guard = ToolCallTimingGuard::capture(
                Instant::now(),
                &"conversation-id",
                "turn-id",
                &call,
                &ToolCallSource::Direct,
            );
            assert!(
                direct_guard.is_some(),
                "direct tool calls should create a timing guard"
            );
            drop(direct_guard);

            let code_mode_guard = ToolCallTimingGuard::capture(
                Instant::now(),
                &"conversation-id",
                "turn-id",
                &call,
                &ToolCallSource::CodeMode {
                    cell_id: "cell-1".to_string(),
                    runtime_tool_call_id: "runtime-call-1".to_string(),
                },
            );
            assert!(
                code_mode_guard.is_none(),
                "nested code-mode calls should not create overlapping timing events"
            );
        });
    }

    #[tokio::test]
    async fn cancellation_before_dispatch_admission_logs_dispatch_only_timing() -> anyhow::Result<()>
    {
        let (session, turn_context) = crate::session::tests::make_session_and_context().await;
        let session = Arc::new(session);
        let turn_context = Arc::new(turn_context);
        let tool_name = codex_tools::ToolName::plain("test_tool");
        let handler = Arc::new(ImmediateHandler {
            tool_name: tool_name.clone(),
        }) as Arc<dyn CoreToolRuntime>;
        let step_context = StepContext::for_test(Arc::clone(&turn_context));
        let router = Arc::new(ToolRouter::from_parts(
            ToolRegistry::from_tools([handler]),
            Vec::new(),
        ));
        let tracker = Arc::new(tokio::sync::Mutex::new(TurnDiffTracker::new()));
        let runtime = ToolCallRuntime::new(router, session, step_context, tracker);
        let execution_gate = Arc::clone(&runtime.parallel_execution);
        let execution_gate_guard = execution_gate
            .try_write_owned()
            .expect("execution gate should be available before dispatch starts");
        let (release_execution_gate_tx, release_execution_gate_rx) = std::sync::mpsc::channel();
        let execution_gate_task = tokio::task::spawn_blocking(move || {
            let _execution_gate_guard = execution_gate_guard;
            release_execution_gate_rx
                .recv()
                .expect("test should release the execution gate");
        });

        let buffer: &'static std::sync::Mutex<Vec<u8>> =
            Box::leak(Box::new(std::sync::Mutex::new(Vec::new())));
        let subscriber = tracing_subscriber::fmt()
            .with_ansi(false)
            .with_max_level(tracing::Level::INFO)
            .with_writer(MockWriter::new(buffer))
            .finish();
        let _subscriber_guard = tracing::subscriber::set_default(subscriber);

        let cancellation_token = CancellationToken::new();
        let call = ToolCall {
            tool_name,
            call_id: "call-1".to_string(),
            payload: ToolPayload::Function {
                arguments: "{}".to_string(),
            },
            encrypted_function_args: None,
        };
        let response_task =
            tokio::spawn(runtime.handle_tool_call(call, cancellation_token.clone()));
        cancellation_token.cancel();
        tokio::time::timeout(Duration::from_secs(1), response_task)
            .await
            .expect("timed out waiting for cancelled tool response")
            .expect("cancelled tool response task should join")
            .expect("cancelled tool call should produce a response");

        let logs = String::from_utf8(
            buffer
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .clone(),
        )?;
        let timing_events = logs
            .lines()
            .filter(|line| line.contains("event.name=\"codex.tool_call\""))
            .collect::<Vec<_>>();
        assert_eq!(
            timing_events.len(),
            1,
            "cancelled tool call should emit exactly one timing event; logs:\n{logs}"
        );
        let timing_event = timing_events[0];
        assert!(
            timing_event.contains("execution_started=false"),
            "tool cancelled before admission should not report execution started: {timing_event}"
        );
        assert!(
            timing_event.contains("handler_duration_ms=0"),
            "tool cancelled before admission should report zero handler duration: {timing_event}"
        );
        let duration_field = |name: &str| {
            timing_event.split_whitespace().find_map(|field| {
                field
                    .strip_prefix(&format!("{name}="))
                    .and_then(|value| value.parse::<u64>().ok())
            })
        };
        let dispatch_duration_ms = duration_field("dispatch_duration_ms")
            .expect("timing event should include dispatch_duration_ms");
        let total_duration_ms = duration_field("total_duration_ms")
            .expect("timing event should include total_duration_ms");
        assert_eq!(
            dispatch_duration_ms, total_duration_ms,
            "tool cancelled before admission should attribute all elapsed time to dispatch: {timing_event}"
        );
        release_execution_gate_tx
            .send(())
            .expect("execution gate task should remain available");
        execution_gate_task
            .await
            .expect("execution gate task should join");

        Ok(())
    }

    struct ImmediateHandler {
        tool_name: codex_tools::ToolName,
    }

    impl ToolExecutor<ToolInvocation> for ImmediateHandler {
        fn tool_name(&self) -> codex_tools::ToolName {
            self.tool_name.clone()
        }

        fn spec(&self) -> codex_tools::ToolSpec {
            codex_tools::ToolSpec::Function(codex_tools::ResponsesApiTool {
                name: self.tool_name.name.clone(),
                description: "Immediate test tool.".to_string(),
                strict: false,
                defer_loading: None,
                parameters: codex_tools::JsonSchema::default(),
                output_schema: None,
            })
        }

        fn handle(&self, _invocation: ToolInvocation) -> codex_tools::ToolExecutorFuture<'_> {
            Box::pin(async {
                Ok(
                    Box::new(FunctionToolOutput::from_text("ok".to_string(), Some(true)))
                        as Box<dyn crate::tools::context::ToolOutput>,
                )
            })
        }
    }

    impl CoreToolRuntime for ImmediateHandler {}

    struct MalformedHandler {
        tool_name: codex_tools::ToolName,
    }

    impl ToolExecutor<ToolInvocation> for MalformedHandler {
        fn tool_name(&self) -> codex_tools::ToolName {
            self.tool_name.clone()
        }

        fn spec(&self) -> codex_tools::ToolSpec {
            codex_tools::ToolSpec::Function(codex_tools::ResponsesApiTool {
                name: self.tool_name.name.clone(),
                description: "Malformed-call test tool.".to_string(),
                strict: false,
                defer_loading: None,
                parameters: codex_tools::JsonSchema::default(),
                output_schema: None,
            })
        }

        fn handle(&self, _invocation: ToolInvocation) -> codex_tools::ToolExecutorFuture<'_> {
            Box::pin(async {
                let diagnostic = codex_tools::MalformedToolCallDiagnostic {
                    tool: "structured_write".to_string(),
                    byte_len: 71,
                    category: "Eof".to_string(),
                    excerpt: "{\"path\":\"runtime/engine.js\",\"mode\":\"overwrite\"".to_string(),
                    finish_reason: None,
                };
                Err(FunctionCallError::MalformedToolCall {
                    message: format!("{diagnostic}"),
                    diagnostic,
                })
            })
        }
    }

    impl CoreToolRuntime for MalformedHandler {}

    struct CancellationCleanupHandler {
        tool_name: codex_tools::ToolName,
        started: std::sync::Mutex<Option<oneshot::Sender<()>>>,
        cleanup_started: std::sync::Mutex<Option<oneshot::Sender<()>>>,
        allow_cleanup: Arc<Notify>,
    }

    impl ToolExecutor<ToolInvocation> for CancellationCleanupHandler {
        fn tool_name(&self) -> codex_tools::ToolName {
            self.tool_name.clone()
        }

        fn spec(&self) -> codex_tools::ToolSpec {
            codex_tools::ToolSpec::Function(codex_tools::ResponsesApiTool {
                name: self.tool_name.name.clone(),
                description: "Cancellation cleanup test tool.".to_string(),
                strict: false,
                defer_loading: None,
                parameters: codex_tools::JsonSchema::default(),
                output_schema: None,
            })
        }

        fn handle(&self, invocation: ToolInvocation) -> codex_tools::ToolExecutorFuture<'_> {
            Box::pin(self.handle_call(invocation))
        }
    }

    impl CancellationCleanupHandler {
        async fn handle_call(
            &self,
            invocation: ToolInvocation,
        ) -> Result<Box<dyn crate::tools::context::ToolOutput>, FunctionCallError> {
            let started = self
                .started
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .take();
            if let Some(started) = started {
                let _ = started.send(());
            }
            invocation.cancellation_token.cancelled().await;
            let cleanup_started = self
                .cleanup_started
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .take();
            if let Some(cleanup_started) = cleanup_started {
                let _ = cleanup_started.send(());
            }
            self.allow_cleanup.notified().await;
            Ok(Box::new(FunctionToolOutput::from_text(
                "cleanup complete".to_string(),
                Some(false),
            )) as Box<dyn crate::tools::context::ToolOutput>)
        }
    }

    impl CoreToolRuntime for CancellationCleanupHandler {
        fn waits_for_runtime_cancellation(&self) -> bool {
            true
        }
    }

    struct FinishRecorder {
        records: Arc<std::sync::Mutex<Vec<ToolCallOutcome>>>,
    }

    impl codex_extension_api::ToolLifecycleContributor for FinishRecorder {
        fn on_tool_finish<'a>(
            &'a self,
            input: codex_extension_api::ToolFinishInput<'a>,
        ) -> codex_extension_api::ToolLifecycleFuture<'a> {
            let records = Arc::clone(&self.records);
            let outcome = input.outcome;
            Box::pin(async move {
                records
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .push(outcome);
            })
        }
    }

    struct BlockingFinishContributor {
        records: Arc<std::sync::Mutex<Vec<ToolCallOutcome>>>,
        finish_started: std::sync::Mutex<Option<oneshot::Sender<()>>>,
        allow_finish: Arc<Notify>,
    }

    impl codex_extension_api::ToolLifecycleContributor for BlockingFinishContributor {
        fn on_tool_finish<'a>(
            &'a self,
            input: codex_extension_api::ToolFinishInput<'a>,
        ) -> codex_extension_api::ToolLifecycleFuture<'a> {
            let records = Arc::clone(&self.records);
            let allow_finish = Arc::clone(&self.allow_finish);
            let finish_started = self
                .finish_started
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .take();
            let outcome = input.outcome;
            Box::pin(async move {
                if let Some(finish_started) = finish_started {
                    let _ = finish_started.send(());
                }
                allow_finish.notified().await;
                records
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .push(outcome);
            })
        }
    }

    #[tokio::test]
    async fn cancellation_after_handler_finishes_preserves_completed_lifecycle()
    -> anyhow::Result<()> {
        let (mut session, turn_context) = crate::session::tests::make_session_and_context().await;
        let records = Arc::new(std::sync::Mutex::new(Vec::new()));
        let (finish_started_tx, finish_started_rx) = oneshot::channel();
        let allow_finish = Arc::new(Notify::new());
        let mut builder =
            codex_extension_api::ExtensionRegistryBuilder::<crate::config::Config>::new();
        builder.tool_lifecycle_contributor(Arc::new(BlockingFinishContributor {
            records: Arc::clone(&records),
            finish_started: std::sync::Mutex::new(Some(finish_started_tx)),
            allow_finish: Arc::clone(&allow_finish),
        }));
        session.services.extensions = Arc::new(builder.build());

        let session = Arc::new(session);
        let turn_context = Arc::new(turn_context);
        let tool_name = codex_tools::ToolName::plain("test_tool");
        let handler = Arc::new(ImmediateHandler {
            tool_name: tool_name.clone(),
        }) as Arc<dyn CoreToolRuntime>;
        let step_context = StepContext::for_test(Arc::clone(&turn_context));
        let router = Arc::new(ToolRouter::from_parts(
            ToolRegistry::from_tools([handler]),
            Vec::new(),
        ));
        let tracker = Arc::new(tokio::sync::Mutex::new(TurnDiffTracker::new()));
        let runtime = ToolCallRuntime::new(router, session, step_context, tracker);
        let cancellation_token = CancellationToken::new();
        let call = ToolCall {
            tool_name,
            call_id: "call-1".to_string(),
            payload: ToolPayload::Function {
                arguments: "{}".to_string(),
            },
            encrypted_function_args: None,
        };

        let response_task =
            tokio::spawn(runtime.handle_tool_call(call, cancellation_token.clone()));
        tokio::time::timeout(Duration::from_secs(1), finish_started_rx)
            .await
            .expect("timed out waiting for lifecycle notification to start")
            .expect("lifecycle notification should start");
        cancellation_token.cancel();
        tokio::time::sleep(Duration::from_millis(10)).await;
        allow_finish.notify_waiters();

        let response = tokio::time::timeout(Duration::from_secs(1), response_task)
            .await
            .expect("timed out waiting for tool response")
            .expect("tool response task should join")?;
        let expected_response = ResponseInputItem::FunctionCallOutput {
            call_id: "call-1".to_string(),
            output: FunctionCallOutputPayload {
                body: FunctionCallOutputBody::Text("ok".to_string()),
                success: Some(true),
            },
        };
        assert_eq!(expected_response, response);

        let actual = records
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .drain(..)
            .collect::<Vec<_>>();
        assert_eq!(vec![ToolCallOutcome::Completed { success: true }], actual);

        Ok(())
    }

    #[tokio::test]
    async fn repeated_identical_tool_calls_are_stopped_after_budget() -> anyhow::Result<()> {
        let (session, turn_context) = crate::session::tests::make_session_and_context().await;
        let session = Arc::new(session);
        let turn_context = Arc::new(turn_context);
        let tool_name = codex_tools::ToolName::plain("test_tool");
        let handler = Arc::new(ImmediateHandler {
            tool_name: tool_name.clone(),
        }) as Arc<dyn CoreToolRuntime>;
        let router = Arc::new(ToolRouter::from_parts(
            ToolRegistry::from_tools([handler]),
            Vec::new(),
        ));
        let tracker = Arc::new(tokio::sync::Mutex::new(TurnDiffTracker::new()));
        let runtime = ToolCallRuntime::new(router, session, turn_context, tracker);
        let cancellation_token = CancellationToken::new();

        for idx in 0..MAX_IDENTICAL_TOOL_CALLS_PER_TURN {
            let response = runtime
                .clone()
                .handle_tool_call(
                    ToolCall {
                        tool_name: tool_name.clone(),
                        call_id: format!("call-{idx}"),
                        payload: ToolPayload::Function {
                            arguments: "{\"cmd\":\"pwd\"}".to_string(),
                        },
                        encrypted_function_args: None,
                    },
                    cancellation_token.clone(),
                )
                .await?;
            let ResponseInputItem::FunctionCallOutput { output, .. } = response else {
                panic!("expected function call output");
            };
            assert_eq!(output.success, Some(true));
        }

        let response = runtime
            .handle_tool_call(
                ToolCall {
                    tool_name,
                    call_id: "call-blocked".to_string(),
                    payload: ToolPayload::Function {
                        arguments: "{\"cmd\":\"pwd\"}".to_string(),
                    },
                    encrypted_function_args: None,
                },
                cancellation_token,
            )
            .await?;
        let ResponseInputItem::FunctionCallOutput { output, .. } = response else {
            panic!("expected function call output");
        };
        assert_eq!(output.success, Some(false));
        let FunctionCallOutputBody::Text(message) = output.body else {
            panic!("expected text output");
        };
        assert!(
            message.contains("repeated identical tool call stopped"),
            "{message}"
        );

        Ok(())
    }

    struct PollingHandler {
        tool_name: codex_tools::ToolName,
    }

    impl ToolExecutor<ToolInvocation> for PollingHandler {
        fn tool_name(&self) -> codex_tools::ToolName {
            self.tool_name.clone()
        }

        fn spec(&self) -> codex_tools::ToolSpec {
            ImmediateHandler {
                tool_name: self.tool_name.clone(),
            }
            .spec()
        }

        fn handle(&self, _invocation: ToolInvocation) -> codex_tools::ToolExecutorFuture<'_> {
            Box::pin(async {
                Ok(Box::new(FunctionToolOutput::from_text(
                    "still running".to_string(),
                    Some(true),
                ))
                    as Box<dyn crate::tools::context::ToolOutput>)
            })
        }
    }

    impl CoreToolRuntime for PollingHandler {
        fn repeated_identical_calls_are_polling(&self) -> bool {
            true
        }
    }

    /// One runtime per call, as `run_sampling_request` builds for each model
    /// request of a turn.
    async fn call_in_new_sampling_runtime(
        router: &Arc<ToolRouter>,
        session: &Arc<Session>,
        turn_context: &Arc<crate::session::turn_context::TurnContext>,
        tool_name: &codex_tools::ToolName,
        call_id: String,
        arguments: &str,
    ) -> Result<ResponseInputItem, CodexErr> {
        let tracker = Arc::new(tokio::sync::Mutex::new(TurnDiffTracker::new()));
        ToolCallRuntime::new(
            Arc::clone(router),
            Arc::clone(session),
            Arc::clone(turn_context),
            tracker,
        )
        .handle_tool_call(
            ToolCall {
                tool_name: tool_name.clone(),
                call_id,
                payload: ToolPayload::Function {
                    arguments: arguments.to_string(),
                },
                encrypted_function_args: None,
            },
            CancellationToken::new(),
        )
        .await
    }

    fn output_text(response: ResponseInputItem) -> (Option<bool>, String) {
        let ResponseInputItem::FunctionCallOutput { output, .. } = response else {
            panic!("expected function call output");
        };
        let FunctionCallOutputBody::Text(text) = output.body else {
            panic!("expected text output");
        };
        (output.success, text)
    }

    #[tokio::test]
    async fn consecutive_identical_calls_are_refused_across_sampling_runtimes() -> anyhow::Result<()>
    {
        let (session, turn_context) = crate::session::tests::make_session_and_context().await;
        let (session, turn_context) = (Arc::new(session), Arc::new(turn_context));
        let tool_name = codex_tools::ToolName::plain("exec_command");
        let router = Arc::new(ToolRouter::from_parts(
            ToolRegistry::from_tools([Arc::new(ImmediateHandler {
                tool_name: tool_name.clone(),
            }) as Arc<dyn CoreToolRuntime>]),
            Vec::new(),
        ));
        let grep = r#"{"cmd":"grep -n kept src/queryforge/__init__.py"}"#;
        let call = |idx: usize, arguments: &'static str| {
            call_in_new_sampling_runtime(
                &router,
                &session,
                &turn_context,
                &tool_name,
                format!("call-{idx}"),
                arguments,
            )
        };

        for idx in 0..3 {
            assert_eq!(
                output_text(call(idx, grep).await?),
                (Some(true), "ok".to_string())
            );
        }
        let (success, message) = output_text(call(3, grep).await?);
        assert_eq!(success, Some(false));
        assert!(
            message.starts_with("Not run: this exact `exec_command` call"),
            "{message}"
        );

        // A different call ends the run, so the same command may run again,
        // as when tests are re-run after an edit.
        let edit = r#"{"cmd":"sed -i s/a/b/ src/queryforge/__init__.py"}"#;
        assert_eq!(output_text(call(4, edit).await?).0, Some(true));
        assert_eq!(output_text(call(5, grep).await?).0, Some(true));
        Ok(())
    }

    #[tokio::test]
    async fn turn_stops_when_refused_identical_calls_continue() -> anyhow::Result<()> {
        let (session, turn_context) = crate::session::tests::make_session_and_context().await;
        let (session, turn_context) = (Arc::new(session), Arc::new(turn_context));
        let tool_name = codex_tools::ToolName::plain("exec_command");
        let router = Arc::new(ToolRouter::from_parts(
            ToolRegistry::from_tools([Arc::new(ImmediateHandler {
                tool_name: tool_name.clone(),
            }) as Arc<dyn CoreToolRuntime>]),
            Vec::new(),
        ));
        let arguments = r#"{"cmd":"python3 check.py"}"#;
        let refusals_before_stop = STOP_TURN_AFTER_REFUSED_REPEATS - 1;
        for idx in 1..=(MAX_CONSECUTIVE_IDENTICAL_TOOL_CALLS + refusals_before_stop) {
            let response = call_in_new_sampling_runtime(
                &router,
                &session,
                &turn_context,
                &tool_name,
                format!("call-{idx}"),
                arguments,
            )
            .await?;
            let expected_success = idx <= MAX_CONSECUTIVE_IDENTICAL_TOOL_CALLS;
            assert_eq!(
                output_text(response).0,
                Some(expected_success),
                "call {idx}"
            );
        }
        let Err(err) = call_in_new_sampling_runtime(
            &router,
            &session,
            &turn_context,
            &tool_name,
            "call-final".to_string(),
            arguments,
        )
        .await
        else {
            panic!("the turn must stop after repeated refusals");
        };
        let CodexErrorDetails::Fatal(message) = err.details() else {
            panic!("expected a fatal repeated-call stop, got {err:?}");
        };
        assert!(
            message.contains("refusing 8 repeated tool calls"),
            "{message}"
        );
        Ok(())
    }

    #[tokio::test]
    async fn cycles_of_calls_with_unchanged_results_are_refused() -> anyhow::Result<()> {
        let (session, turn_context) = crate::session::tests::make_session_and_context().await;
        let (session, turn_context) = (Arc::new(session), Arc::new(turn_context));
        let tool_name = codex_tools::ToolName::plain("exec_command");
        let router = Arc::new(ToolRouter::from_parts(
            ToolRegistry::from_tools([Arc::new(ImmediateHandler {
                tool_name: tool_name.clone(),
            }) as Arc<dyn CoreToolRuntime>]),
            Vec::new(),
        ));
        let check_a = r#"{"cmd":"python3 check_a.py"}"#;
        let check_b = r#"{"cmd":"python3 check_b.py"}"#;
        let mut idx = 0;
        let mut call = |arguments: &'static str| {
            idx += 1;
            call_in_new_sampling_runtime(
                &router,
                &session,
                &turn_context,
                &tool_name,
                format!("call-{idx}"),
                arguments,
            )
        };

        // A, B, A, B, ...: never twice in a row, and results never change. B's
        // first result was new information, so A's count restarts once; B is
        // refused on lap four and A on lap five.
        for _ in 0..MAX_IDENTICAL_TOOL_RESULTS {
            assert_eq!(output_text(call(check_a).await?).0, Some(true));
            assert_eq!(output_text(call(check_b).await?).0, Some(true));
        }
        assert_eq!(output_text(call(check_a).await?).0, Some(true));
        for arguments in [check_b, check_a] {
            let (success, message) = output_text(call(arguments).await?);
            assert_eq!(success, Some(false));
            assert!(
                message.contains("already returned the same result 3 times"),
                "{message}"
            );
        }

        // A call with a new result is progress, so the checks may run again.
        assert_eq!(
            output_text(call(r#"{"cmd":"apply fix"}"#).await?).0,
            Some(true)
        );
        assert_eq!(output_text(call(check_a).await?).0, Some(true));
        Ok(())
    }

    /// Output that alternates between two states, as when a model toggles an
    /// edit back and forth and re-runs the same test.
    struct TogglingHandler {
        tool_name: codex_tools::ToolName,
        calls: std::sync::atomic::AtomicUsize,
    }

    impl ToolExecutor<ToolInvocation> for TogglingHandler {
        fn tool_name(&self) -> codex_tools::ToolName {
            self.tool_name.clone()
        }

        fn spec(&self) -> codex_tools::ToolSpec {
            ImmediateHandler {
                tool_name: self.tool_name.clone(),
            }
            .spec()
        }

        fn handle(&self, invocation: ToolInvocation) -> codex_tools::ToolExecutorFuture<'_> {
            // Each edit flips the state; the test reports the current state.
            let is_edit = invocation.payload.log_payload().contains("toggle");
            let state = if is_edit {
                self.calls.fetch_add(1, Ordering::SeqCst) + 1
            } else {
                self.calls.load(Ordering::SeqCst)
            } % 2;
            let text = if is_edit {
                "edited".to_string()
            } else {
                format!("state {state}")
            };
            Box::pin(async move {
                Ok(Box::new(FunctionToolOutput::from_text(text, Some(true)))
                    as Box<dyn crate::tools::context::ToolOutput>)
            })
        }
    }

    impl CoreToolRuntime for TogglingHandler {}

    #[tokio::test]
    async fn oscillating_results_are_not_progress() -> anyhow::Result<()> {
        let (session, turn_context) = crate::session::tests::make_session_and_context().await;
        let (session, turn_context) = (Arc::new(session), Arc::new(turn_context));
        let tool_name = codex_tools::ToolName::plain("exec_command");
        let router = Arc::new(ToolRouter::from_parts(
            ToolRegistry::from_tools([Arc::new(TogglingHandler {
                tool_name: tool_name.clone(),
                calls: std::sync::atomic::AtomicUsize::new(0),
            }) as Arc<dyn CoreToolRuntime>]),
            Vec::new(),
        ));
        let test_run = r#"{"cmd":"python3 -m unittest"}"#;
        let edit = r#"{"cmd":"toggle the join rename"}"#;
        let mut outcomes = Vec::new();
        for idx in 0..16 {
            let arguments = if idx % 2 == 0 { edit } else { test_run };
            let response = call_in_new_sampling_runtime(
                &router,
                &session,
                &turn_context,
                &tool_name,
                format!("call-{idx}"),
                arguments,
            )
            .await?;
            outcomes.push(output_text(response).0 == Some(true));
        }
        // The first edit/test pairs report states 1 and 0 for the first time;
        // after that the test only returns results it has already returned.
        assert!(outcomes[..4].iter().all(|ran| *ran), "{outcomes:?}");
        assert!(
            outcomes.iter().any(|ran| !ran),
            "toggling between two known results must eventually be refused: {outcomes:?}"
        );
        Ok(())
    }

    #[tokio::test]
    async fn polling_tools_may_repeat_identical_calls() -> anyhow::Result<()> {
        let (session, turn_context) = crate::session::tests::make_session_and_context().await;
        let (session, turn_context) = (Arc::new(session), Arc::new(turn_context));
        let tool_name = codex_tools::ToolName::plain("write_stdin");
        let router = Arc::new(ToolRouter::from_parts(
            ToolRegistry::from_tools([Arc::new(PollingHandler {
                tool_name: tool_name.clone(),
            }) as Arc<dyn CoreToolRuntime>]),
            Vec::new(),
        ));
        for idx in 0..(MAX_CONSECUTIVE_IDENTICAL_TOOL_CALLS + STOP_TURN_AFTER_REFUSED_REPEATS + 2) {
            let response = call_in_new_sampling_runtime(
                &router,
                &session,
                &turn_context,
                &tool_name,
                format!("poll-{idx}"),
                r#"{"session_id":7,"chars":""}"#,
            )
            .await?;
            assert_eq!(output_text(response).0, Some(true), "poll {idx}");
        }
        Ok(())
    }

    #[tokio::test]
    async fn equivalent_malformed_calls_stop_across_sampling_runtimes() -> anyhow::Result<()> {
        let (session, turn_context) = crate::session::tests::make_session_and_context().await;
        let session = Arc::new(session);
        let turn_context = Arc::new(turn_context);
        let tool_name = codex_tools::ToolName::plain("structured_write");
        let handler = Arc::new(MalformedHandler {
            tool_name: tool_name.clone(),
        }) as Arc<dyn CoreToolRuntime>;
        let router = Arc::new(ToolRouter::from_parts(
            ToolRegistry::from_tools([handler]),
            Vec::new(),
        ));
        let tracker = Arc::new(tokio::sync::Mutex::new(TurnDiffTracker::new()));
        let call = |call_id: &str| ToolCall {
            tool_name: tool_name.clone(),
            call_id: call_id.to_string(),
            payload: ToolPayload::Function {
                arguments: "{\"path\":\"runtime/engine.js\",\"mode\":\"overwrite\"".to_string(),
            },
            encrypted_function_args: None,
        };

        let first_runtime = ToolCallRuntime::new(
            Arc::clone(&router),
            Arc::clone(&session),
            Arc::clone(&turn_context),
            Arc::clone(&tracker),
        );
        let first = first_runtime
            .handle_tool_call(call("call-1"), CancellationToken::new())
            .await?;
        let ResponseInputItem::FunctionCallOutput { output, .. } = first else {
            panic!("first malformed call should return a corrective tool result");
        };
        assert_eq!(output.success, Some(false));

        // A new runtime represents the next model sampling request in the same user turn.
        let second_runtime = ToolCallRuntime::new(router, session, turn_context, tracker);
        let second = second_runtime
            .handle_tool_call(call("call-2"), CancellationToken::new())
            .await;
        let Err(err) = second else {
            panic!("second equivalent malformed call must stop the turn: {second:?}");
        };
        let CodexErrorDetails::Fatal(message) = err.details() else {
            panic!("expected fatal malformed-call guard, got {err:?}");
        };
        assert!(message.contains("after 2 equivalent malformed tool calls"));

        Ok(())
    }

    #[tokio::test]
    async fn cancellation_waiting_for_runtime_cleanup_emits_only_aborted_lifecycle()
    -> anyhow::Result<()> {
        let (mut session, turn_context) = crate::session::tests::make_session_and_context().await;
        let records = Arc::new(std::sync::Mutex::new(Vec::new()));
        let mut builder =
            codex_extension_api::ExtensionRegistryBuilder::<crate::config::Config>::new();
        builder.tool_lifecycle_contributor(Arc::new(FinishRecorder {
            records: Arc::clone(&records),
        }));
        session.services.extensions = Arc::new(builder.build());

        let session = Arc::new(session);
        let turn_context = Arc::new(turn_context);
        let tool_name = codex_tools::ToolName::plain("cleanup_tool");
        let (started_tx, started_rx) = oneshot::channel();
        let (cleanup_started_tx, cleanup_started_rx) = oneshot::channel();
        let allow_cleanup = Arc::new(Notify::new());
        let handler = Arc::new(CancellationCleanupHandler {
            tool_name: tool_name.clone(),
            started: std::sync::Mutex::new(Some(started_tx)),
            cleanup_started: std::sync::Mutex::new(Some(cleanup_started_tx)),
            allow_cleanup: Arc::clone(&allow_cleanup),
        }) as Arc<dyn CoreToolRuntime>;
        let step_context = StepContext::for_test(Arc::clone(&turn_context));
        let router = Arc::new(ToolRouter::from_parts(
            ToolRegistry::from_tools([handler]),
            Vec::new(),
        ));
        let tracker = Arc::new(tokio::sync::Mutex::new(TurnDiffTracker::new()));
        let runtime = ToolCallRuntime::new(router, session, step_context, tracker);
        let cancellation_token = CancellationToken::new();
        let call = ToolCall {
            tool_name,
            call_id: "call-1".to_string(),
            payload: ToolPayload::Function {
                arguments: "{}".to_string(),
            },
            encrypted_function_args: None,
        };

        let response_task =
            tokio::spawn(runtime.handle_tool_call(call, cancellation_token.clone()));
        started_rx.await.expect("handler should start");
        cancellation_token.cancel();
        cleanup_started_rx
            .await
            .expect("handler should start cleanup");
        tokio::time::sleep(Duration::from_millis(10)).await;
        allow_cleanup.notify_one();

        let response = tokio::time::timeout(Duration::from_secs(1), response_task)
            .await
            .expect("timed out waiting for tool response")
            .expect("tool response task should join")?;
        let ResponseInputItem::FunctionCallOutput { output, .. } = response else {
            anyhow::bail!("cancelled tool should return function output");
        };
        let FunctionCallOutputBody::Text(text) = output.body else {
            anyhow::bail!("cancelled tool output should be text");
        };
        assert!(text.contains("aborted by user"));

        let actual = records
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .drain(..)
            .collect::<Vec<_>>();
        assert_eq!(vec![ToolCallOutcome::Aborted], actual);

        Ok(())
    }

    #[test]
    fn abort_message_reports_wall_time_for_shell_tools() {
        let call = |tool_name| ToolCall {
            tool_name,
            call_id: "call-1".to_string(),
            payload: ToolPayload::Function {
                arguments: "{}".to_string(),
            },
            encrypted_function_args: None,
        };
        let secs = 1.5;

        for name in ["shell_command", "unified_exec"] {
            assert_eq!(
                "Wall time: 1.5 seconds\naborted by user",
                ToolCallRuntime::abort_message(&call(codex_tools::ToolName::plain(name)), secs)
            );
        }
        assert_eq!(
            "aborted by user after 1.5s",
            ToolCallRuntime::abort_message(
                &call(codex_tools::ToolName::namespaced("mcp", "shell_command")),
                secs
            )
        );
        assert_eq!(
            "aborted by user after 1.5s",
            ToolCallRuntime::abort_message(&call(codex_tools::ToolName::plain("other")), secs)
        );
    }
}
