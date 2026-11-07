use std::sync::Arc;
use std::time::Duration;
use std::time::Instant;

use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;
use tokio_util::task::AbortOnDropHandle;
use tracing::Instrument;
use tracing::info;
use tracing::instrument;
use tracing::trace_span;
use tracing::warn;

use crate::client::ModelClientSession;
use crate::guardian::routes_approval_to_guardian;
use crate::responses_metadata::CodexResponsesRequestKind;
use crate::session::INITIAL_SUBMIT_ID;
use crate::session::session::Session;
use crate::session::turn::build_prompt;
use codex_otel::STARTUP_PREWARM_AGE_AT_FIRST_TURN_METRIC;
use codex_otel::STARTUP_PREWARM_DURATION_METRIC;
use codex_otel::SessionTelemetry;
use codex_protocol::error::Result as CodexResult;
use codex_protocol::models::BaseInstructions;
use codex_protocol::protocol::Event;
use codex_protocol::protocol::EventMsg;
use codex_protocol::protocol::WarningEvent;

pub(crate) struct SessionStartupPrewarmHandle {
    task: AbortOnDropHandle<CodexResult<ModelClientSession>>,
    started_at: Instant,
    timeout: Duration,
}

pub(crate) enum SessionStartupPrewarmResolution {
    Cancelled,
    Ready(Box<ModelClientSession>),
    Unavailable {
        status: &'static str,
        prewarm_duration: Option<Duration>,
    },
}

impl SessionStartupPrewarmHandle {
    pub(crate) fn new(
        task: JoinHandle<CodexResult<ModelClientSession>>,
        started_at: Instant,
        timeout: Duration,
    ) -> Self {
        Self {
            task: AbortOnDropHandle::new(task),
            started_at,
            timeout,
        }
    }

    pub(crate) async fn abort(self) {
        self.task.abort();
        let _ = self.task.await;
    }

    #[instrument(name = "startup_prewarm.resolve", level = "trace", skip_all)]
    async fn resolve(
        self,
        session_telemetry: &SessionTelemetry,
        cancellation_token: &CancellationToken,
    ) -> SessionStartupPrewarmResolution {
        let resolve_started_at = Instant::now();
        let Self {
            mut task,
            started_at,
            timeout,
        } = self;
        let age_at_first_turn = started_at.elapsed();
        let remaining = timeout.saturating_sub(age_at_first_turn);

        let resolution = if task.is_finished() {
            Self::resolution_from_join_result(task.await, started_at)
        } else {
            match tokio::select! {
                _ = cancellation_token.cancelled() => None,
                result = tokio::time::timeout(remaining, &mut task) => Some(result),
            } {
                Some(Ok(result)) => Self::resolution_from_join_result(result, started_at),
                Some(Err(_elapsed)) => {
                    task.abort();
                    info!("startup websocket prewarm timed out before the first turn could use it");
                    SessionStartupPrewarmResolution::Unavailable {
                        status: "timed_out",
                        prewarm_duration: Some(started_at.elapsed()),
                    }
                }
                None => {
                    task.abort();
                    session_telemetry.record_startup_phase(
                        "startup_prewarm_resolve",
                        resolve_started_at.elapsed(),
                        Some("cancelled"),
                    );
                    session_telemetry.record_duration(
                        STARTUP_PREWARM_AGE_AT_FIRST_TURN_METRIC,
                        age_at_first_turn,
                        &[("status", "cancelled")],
                    );
                    session_telemetry.record_duration(
                        STARTUP_PREWARM_DURATION_METRIC,
                        started_at.elapsed(),
                        &[("status", "cancelled")],
                    );
                    return SessionStartupPrewarmResolution::Cancelled;
                }
            }
        };
        let status = match &resolution {
            SessionStartupPrewarmResolution::Cancelled => "cancelled",
            SessionStartupPrewarmResolution::Ready(_) => "ready",
            SessionStartupPrewarmResolution::Unavailable { status, .. } => status,
        };
        session_telemetry.record_startup_phase(
            "startup_prewarm_resolve",
            resolve_started_at.elapsed(),
            Some(status),
        );

        match resolution {
            SessionStartupPrewarmResolution::Cancelled => {
                SessionStartupPrewarmResolution::Cancelled
            }
            SessionStartupPrewarmResolution::Ready(prewarmed_session) => {
                session_telemetry.record_duration(
                    STARTUP_PREWARM_AGE_AT_FIRST_TURN_METRIC,
                    age_at_first_turn,
                    &[("status", "consumed")],
                );
                SessionStartupPrewarmResolution::Ready(prewarmed_session)
            }
            SessionStartupPrewarmResolution::Unavailable {
                status,
                prewarm_duration,
            } => {
                session_telemetry.record_duration(
                    STARTUP_PREWARM_AGE_AT_FIRST_TURN_METRIC,
                    age_at_first_turn,
                    &[("status", status)],
                );
                if let Some(prewarm_duration) = prewarm_duration {
                    session_telemetry.record_duration(
                        STARTUP_PREWARM_DURATION_METRIC,
                        prewarm_duration,
                        &[("status", status)],
                    );
                }
                SessionStartupPrewarmResolution::Unavailable {
                    status,
                    prewarm_duration,
                }
            }
        }
    }

    fn resolution_from_join_result(
        result: std::result::Result<CodexResult<ModelClientSession>, tokio::task::JoinError>,
        started_at: Instant,
    ) -> SessionStartupPrewarmResolution {
        match result {
            Ok(Ok(prewarmed_session)) => {
                SessionStartupPrewarmResolution::Ready(Box::new(prewarmed_session))
            }
            Ok(Err(err)) => {
                warn!("startup websocket prewarm setup failed: {err:#}");
                SessionStartupPrewarmResolution::Unavailable {
                    status: "failed",
                    prewarm_duration: None,
                }
            }
            Err(err) => {
                warn!("startup websocket prewarm setup join failed: {err}");
                SessionStartupPrewarmResolution::Unavailable {
                    status: "join_failed",
                    prewarm_duration: Some(started_at.elapsed()),
                }
            }
        }
    }
}

impl Session {
    /// `reopened`: the session resumes or forks a saved conversation (#403).
    /// Its warm-up is a paid request the user did not ask for, so it is
    /// labelled as a reopen warm-up, announced once sent, and skipped
    /// entirely when `skip_reopen_warmup` is set. `skip_startup_warmup` skips
    /// every warm-up, for clients that start a thread only for realtime (#1045).
    pub(crate) async fn schedule_startup_prewarm(
        self: &Arc<Self>,
        base_instructions: String,
        reopened: bool,
    ) {
        let config = self.get_config().await;
        let skip_warmup = config.skip_startup_warmup || (reopened && config.skip_reopen_warmup);
        if skip_warmup || !self.services.model_client().responses_websocket_enabled() {
            // Without websocket prewarm, resolve auth once so Agent Identity bootstrap can
            // register or engage this session's bearer fallback before the first user request.
            let model_client = self.services.model_client();
            tokio::spawn(async move {
                if let Err(err) = model_client.prewarm_auth().await {
                    warn!("startup auth prewarm failed: {err:#}");
                }
            });
            return;
        }

        let session_telemetry = self.services.session_telemetry.clone();
        let websocket_connect_timeout = self.provider().await.websocket_connect_timeout();
        let started_at = Instant::now();
        let startup_prewarm_session = Arc::clone(self);
        let startup_prewarm = tokio::spawn(
            async move {
                let result = schedule_startup_prewarm_inner(
                    startup_prewarm_session,
                    base_instructions,
                    reopened,
                )
                .await;
                let status = if result.is_ok() { "ready" } else { "failed" };
                session_telemetry.record_startup_phase(
                    "startup_prewarm_total",
                    started_at.elapsed(),
                    Some(status),
                );
                session_telemetry.record_duration(
                    STARTUP_PREWARM_DURATION_METRIC,
                    started_at.elapsed(),
                    &[("status", status)],
                );
                result
            }
            .instrument(trace_span!(
                "startup_prewarm",
                otel.name = "startup_prewarm",
                thread.id = %self.thread_id(),
            )),
        );
        self.set_session_startup_prewarm(SessionStartupPrewarmHandle::new(
            startup_prewarm,
            started_at,
            websocket_connect_timeout,
        ))
        .await;
    }

    pub(crate) async fn consume_startup_prewarm_for_regular_turn(
        &self,
        cancellation_token: &CancellationToken,
    ) -> SessionStartupPrewarmResolution {
        let Some(startup_prewarm) = self.take_session_startup_prewarm().await else {
            return SessionStartupPrewarmResolution::Unavailable {
                status: "not_scheduled",
                prewarm_duration: None,
            };
        };
        startup_prewarm
            .resolve(&self.services.session_telemetry, cancellation_token)
            .await
    }
}

async fn schedule_startup_prewarm_inner(
    session: Arc<Session>,
    base_instructions: String,
    reopened: bool,
) -> CodexResult<ModelClientSession> {
    let prewarm_started_at = Instant::now();
    let startup_turn_context = session
        .new_startup_prewarm_turn_with_sub_id(INITIAL_SUBMIT_ID.to_owned())
        .await;
    startup_turn_context.session_telemetry.record_startup_phase(
        "startup_prewarm_create_turn_context",
        prewarm_started_at.elapsed(),
        /*status*/ None,
    );
    if routes_approval_to_guardian(&startup_turn_context) {
        let guardian_session = Arc::clone(&session);
        let guardian_parent_turn = Arc::clone(&startup_turn_context);
        drop(tokio::spawn(async move {
            if let Err(err) = guardian_session
                .guardian_review_session
                .initialize(Arc::clone(&guardian_session), guardian_parent_turn)
                .await
            {
                warn!("failed to initialize guardian review session: {err:#}");
            }
        }));
    }
    let startup_cancellation_token = CancellationToken::new();
    let built_tools_started_at = Instant::now();
    // Startup prewarm runs before run_turn and needs its own tool-building snapshot.
    let step_context = session
        .capture_step_context(
            Arc::clone(&startup_turn_context),
            &startup_cancellation_token,
        )
        .await?;
    let startup_router = Arc::clone(&step_context.tool_router);
    startup_turn_context.session_telemetry.record_startup_phase(
        "startup_prewarm_build_tools",
        built_tools_started_at.elapsed(),
        /*status*/ None,
    );
    let build_prompt_started_at = Instant::now();
    let startup_prompt = build_prompt(
        Vec::new(),
        startup_router.as_ref(),
        startup_turn_context.as_ref(),
        BaseInstructions {
            text: base_instructions,
        },
    );
    startup_turn_context.session_telemetry.record_startup_phase(
        "startup_prewarm_build_prompt",
        build_prompt_started_at.elapsed(),
        /*status*/ None,
    );
    let window_id = session.current_window_id().await;
    let responses_metadata = startup_turn_context
        .turn_metadata_state
        .to_responses_metadata(
            session.installation_id.clone(),
            window_id,
            CodexResponsesRequestKind::Prewarm,
        );
    let mut client_session = session.services.new_model_client_session();
    // Prewarm sends the session's whole prompt to prime the model's cache. The
    // provider charges for those cache writes, so it is inference the operator
    // paid for and it is collected like any other turn, under its own identity.
    // Best effort on purpose, exactly as compaction is: a prewarm that cannot be
    // recorded must still run, because accounting is an observer here and not a
    // gate on starting a session.
    let _accounting = match crate::accounting::attach_turn(
        &session,
        startup_turn_context.as_ref(),
        &client_session,
        if reopened {
            crate::accounting::reopen_prewarm_turn_label()
        } else {
            crate::accounting::prewarm_turn_label()
        },
    )
    .await
    {
        Ok(scopes) => Some(scopes),
        Err(error) => {
            tracing::warn!(%error, "accounting: startup prewarm proceeding unrecorded");
            None
        }
    };
    let websocket_warmup_started_at = Instant::now();
    client_session
        .prewarm_websocket(
            &startup_prompt,
            &startup_turn_context.model_info,
            &startup_turn_context.session_telemetry,
            startup_turn_context.reasoning_effort.clone(),
            startup_turn_context.reasoning_summary,
            startup_turn_context.config.service_tier.clone(),
            &responses_metadata,
        )
        .await?;
    startup_turn_context.session_telemetry.record_startup_phase(
        "startup_prewarm_websocket_warmup",
        websocket_warmup_started_at.elapsed(),
        /*status*/ None,
    );
    // Said only once a warm-up really went out and completed (a fallback to
    // HTTP sends none), and not written to the conversation: a later reopen
    // sends, and says, its own.
    if reopened && client_session.sent_websocket_request() {
        session
            .send_event_raw_unpersisted(Event {
                id: String::new(),
                msg: EventMsg::Warning(WarningEvent {
                    message: REOPEN_WARMUP_NOTICE.to_string(),
                }),
            })
            .await;
    }
    Ok(client_session)
}

/// Whether a session with this history and source is a conversation the user
/// reopens or forks (#403): resumed or forked history, started by the user's
/// client. A sub-agent, reviewer or internal worker given copied history is
/// not: the user opened nothing.
///
/// `reopened_by_prompt_edit` (#1038): editing the first message continues in
/// a conversation with no history, which is a reopen all the same.
pub(crate) fn is_reopen(
    history: &codex_protocol::protocol::InitialHistory,
    source: &codex_protocol::protocol::SessionSource,
    reopened_by_prompt_edit: bool,
) -> bool {
    use codex_protocol::protocol::InitialHistory;
    let reopened = match history {
        InitialHistory::Resumed(_) | InitialHistory::Forked(_) => true,
        InitialHistory::New => reopened_by_prompt_edit,
        InitialHistory::Cleared => false,
    };
    reopened && !source.is_non_root_agent()
}

/// What a reopened conversation says after its warm-up request is sent (#403).
pub(crate) const REOPEN_WARMUP_NOTICE: &str = if cfg!(feature = "developer-accounting") {
    "Reopening or forking this conversation sent a warm-up request to prime the model's cache. It is billed like any request; /cost lists it as a reopen warm-up. To skip it, set skip_reopen_warmup = true in config.toml."
} else {
    "Reopening or forking this conversation sent a warm-up request to prime the model's cache. It is billed like any request. To skip it, set skip_reopen_warmup = true in config.toml."
};

#[cfg(test)]
#[path = "session_startup_prewarm_tests.rs"]
mod tests;
