use crate::auth::SharedAuthProvider;
use crate::common::ChatCompletionsRequest;
use crate::common::CompletionFinishReason;
use crate::common::ResponseEvent;
use crate::common::ResponseStream;
use crate::endpoint::session::EndpointSession;
use crate::error::ApiError;
use crate::provider::Provider;
use crate::requests::headers::build_session_headers;
use crate::requests::headers::insert_header;
use crate::requests::headers::subagent_header;
use crate::telemetry::SseTelemetry;
use codex_client::ByteStream;
use codex_client::EncodedJsonBody;
use codex_client::HttpTransport;
use codex_client::RequestTelemetry;
use codex_client::StreamResponse;
use codex_client::TransportError;
use codex_protocol::ResponseItemId;
use codex_protocol::models::ContentItem;
use codex_protocol::models::ReasoningItemContent;
use codex_protocol::models::ReasoningItemReasoningSummary;
use codex_protocol::models::ResponseItem;
use codex_protocol::protocol::SessionSource;
use codex_protocol::protocol::TokenUsage;
use eventsource_stream::Event;
use eventsource_stream::EventStreamError;
use eventsource_stream::Eventsource;
use futures::StreamExt;
use futures::stream;
use http::HeaderMap;
use http::HeaderValue;
use http::Method;
use serde::Deserialize;
use serde::Deserializer;
use serde::Serialize;
use serde_json::Value;
use std::collections::BTreeMap;
use std::io::IsTerminal;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::AtomicU64;
use std::sync::atomic::Ordering;
use std::time::Duration;
use tokio::sync::mpsc;
use tokio::sync::watch;
use tokio::time::Instant;
use tokio::time::timeout;
use tracing::debug;
use tracing::instrument;
use tracing::trace;
use tracing::warn;

#[path = "chat_accounting.rs"]
pub(crate) mod accounting;
#[cfg(test)]
#[path = "chat_accounting_tests.rs"]
mod accounting_tests;

const REQUEST_ID_HEADER: &str = "x-request-id";
const GENERATION_ID_HEADERS: [&str; 3] = [
    "x-openrouter-generation-id",
    "x-generation-id",
    "openrouter-generation-id",
];
const SSE_IDLE_TIMEOUT_MESSAGE: &str = "idle timeout waiting for SSE";
const DEFAULT_ACTIONABLE_SILENCE_TIMEOUT: Duration = Duration::from_secs(180);
const SERIALIZED_TOOL_TEXT_PROBE_CHARS: usize = 96;
const CALL_METRICS_TAG: &str = "corbanu_call_metrics";
static CHAT_CALL_SEQUENCE: AtomicU64 = AtomicU64::new(0);

pub struct ChatCompletionsClient<T: HttpTransport> {
    session: EndpointSession<T>,
    sse_telemetry: Option<Arc<dyn SseTelemetry>>,
    usage_observer: Option<Arc<dyn accounting::ChatUsageObserver>>,
}

#[derive(Default)]
pub struct ChatCompletionsOptions {
    pub session_id: Option<String>,
    pub thread_id: Option<String>,
    pub session_source: Option<SessionSource>,
    pub extra_headers: HeaderMap,
    pub same_turn_attempt_index: Option<u64>,
    pub actionable_silence_timeout: Option<Duration>,
}

impl<T: HttpTransport> ChatCompletionsClient<T> {
    pub fn new(transport: T, provider: Provider, auth: SharedAuthProvider) -> Self {
        Self {
            session: EndpointSession::new(transport, provider, auth),
            sse_telemetry: None,
            usage_observer: None,
        }
    }

    pub fn with_usage_observer(
        mut self,
        observer: Option<Arc<dyn accounting::ChatUsageObserver>>,
    ) -> Self {
        self.usage_observer = observer;
        self
    }

    pub fn with_telemetry(
        self,
        request: Option<Arc<dyn RequestTelemetry>>,
        sse: Option<Arc<dyn SseTelemetry>>,
    ) -> Self {
        Self {
            session: self.session.with_request_telemetry(request),
            sse_telemetry: sse,
            usage_observer: self.usage_observer,
        }
    }

    #[instrument(
        name = "chat_completions.stream_request",
        level = "info",
        skip_all,
        fields(
            transport = "chat_completions_http",
            http.method = "POST",
            api.path = "chat/completions"
        )
    )]
    pub async fn stream_request(
        &self,
        request: ChatCompletionsRequest,
        options: ChatCompletionsOptions,
    ) -> Result<ResponseStream, ApiError> {
        let ChatCompletionsOptions {
            session_id,
            thread_id,
            session_source,
            extra_headers,
            same_turn_attempt_index,
            actionable_silence_timeout,
        } = options;

        let body = EncodedJsonBody::encode(&request).map_err(|e| {
            ApiError::Stream(format!("failed to encode chat completions request: {e}"))
        })?;
        let metrics = ChatCallMetrics::new(body.as_bytes().len(), same_turn_attempt_index);

        let mut headers = extra_headers;
        if let Some(ref thread_id) = thread_id {
            insert_header(&mut headers, "x-client-request-id", thread_id);
        }
        headers.extend(build_session_headers(session_id, thread_id));
        if let Some(subagent) = subagent_header(&session_source) {
            insert_header(&mut headers, "x-openai-subagent", &subagent);
        }

        let response_headers_started_at = Instant::now();
        let stream_response_result = self
            .session
            .stream_encoded_json_with(Method::POST, Self::path(), headers, Some(body), |req| {
                req.headers.insert(
                    http::header::ACCEPT,
                    HeaderValue::from_static("text/event-stream"),
                );
            })
            .await;
        let response_headers_elapsed = response_headers_started_at.elapsed();
        let stream_response = match stream_response_result {
            Ok(stream_response) => {
                metrics.record_response_headers(response_headers_elapsed, &stream_response.headers);
                stream_response
            }
            Err(err) => {
                metrics.record_response_header_error(response_headers_elapsed);
                metrics.finish(format!("error: {err}"));
                return Err(err);
            }
        };

        Ok(spawn_chat_completions_stream(
            stream_response,
            self.session.provider().stream_idle_timeout,
            actionable_silence_timeout.unwrap_or(DEFAULT_ACTIONABLE_SILENCE_TIMEOUT),
            self.sse_telemetry.clone(),
            metrics,
            self.usage_observer.clone(),
        ))
    }

    fn path() -> &'static str {
        "chat/completions"
    }
}

fn spawn_chat_completions_stream(
    stream_response: StreamResponse,
    idle_timeout: Duration,
    actionable_silence_timeout: Duration,
    telemetry: Option<Arc<dyn SseTelemetry>>,
    metrics: ChatCallMetrics,
    observer: Option<Arc<dyn accounting::ChatUsageObserver>>,
) -> ResponseStream {
    let upstream_request_id = stream_response
        .headers
        .get(REQUEST_ID_HEADER)
        .and_then(|value| value.to_str().ok())
        .map(str::to_string);
    let (tx_event, rx_event) = mpsc::channel::<Result<ResponseEvent, ApiError>>(1600);
    let response_id_hint = upstream_request_id.clone();
    tokio::spawn(async move {
        let _ = tx_event.send(Ok(ResponseEvent::Created)).await;
        process_chat_sse_observed(
            stream_response.bytes,
            tx_event,
            idle_timeout,
            actionable_silence_timeout,
            telemetry,
            response_id_hint,
            Some(metrics),
            observer,
        )
        .await;
    });

    ResponseStream {
        rx_event,
        upstream_request_id,
    }
}

#[derive(Debug, Deserialize)]
struct ChatCompletionChunk {
    id: Option<String>,
    model: Option<String>,
    provider: Option<String>,
    #[serde(default)]
    choices: Vec<ChatChoice>,
    #[serde(default)]
    usage: Option<ChatUsage>,
}

#[derive(Debug, Deserialize)]
struct ChatChoice {
    #[serde(default)]
    delta: ChatDelta,
    finish_reason: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
struct ChatDelta {
    content: Option<String>,
    /// Z.AI/DeepSeek-dialect raw reasoning delta field.
    reasoning_content: Option<String>,
    /// OpenRouter-normalized reasoning delta field (fast-pool hosts stream
    /// thinking here; treating it as non-actionable trips the
    /// actionable-silence timeout on any >180s think).
    reasoning: Option<String>,
    /// OpenRouter typed reasoning blocks; may arrive without a plain-text
    /// `reasoning` twin (e.g. encrypted/redacted thinking).
    #[serde(default, deserialize_with = "deserialize_null_default")]
    reasoning_details: Vec<serde_json::Value>,
    #[serde(default, deserialize_with = "deserialize_null_default")]
    tool_calls: Vec<ChatToolCallDelta>,
}

fn deserialize_null_default<'de, D, T>(deserializer: D) -> Result<T, D::Error>
where
    D: Deserializer<'de>,
    T: Deserialize<'de> + Default,
{
    Option::<T>::deserialize(deserializer).map(Option::unwrap_or_default)
}

#[derive(Debug, Deserialize)]
struct ChatToolCallDelta {
    index: usize,
    id: Option<String>,
    function: Option<ChatFunctionDelta>,
}

#[derive(Debug, Deserialize)]
struct ChatFunctionDelta {
    name: Option<String>,
    arguments: Option<String>,
}

#[derive(Debug, Deserialize)]
struct ChatUsage {
    #[serde(default)]
    prompt_tokens: i64,
    #[serde(default)]
    completion_tokens: i64,
    #[serde(default)]
    total_tokens: i64,
    #[serde(default)]
    prompt_tokens_details: Option<ChatPromptTokensDetails>,
    #[serde(default)]
    completion_tokens_details: Option<ChatCompletionTokensDetails>,
}

#[derive(Debug, Deserialize)]
struct ChatPromptTokensDetails {
    #[serde(default)]
    cached_tokens: i64,
}

#[derive(Debug, Deserialize)]
struct ChatCompletionTokensDetails {
    #[serde(default)]
    reasoning_tokens: i64,
}

impl From<ChatUsage> for TokenUsage {
    fn from(value: ChatUsage) -> Self {
        Self {
            input_tokens: value.prompt_tokens,
            cached_input_tokens: value
                .prompt_tokens_details
                .map(|details| details.cached_tokens)
                .unwrap_or(0),
            cache_write_input_tokens: 0,
            output_tokens: value.completion_tokens,
            reasoning_output_tokens: value
                .completion_tokens_details
                .map(|details| details.reasoning_tokens)
                .unwrap_or(0),
            total_tokens: value.total_tokens,
        }
    }
}

#[derive(Debug, Deserialize)]
struct ChatErrorEnvelope {
    error: ChatError,
}

#[derive(Debug, Deserialize)]
struct ChatError {
    message: Option<String>,
    code: Option<String>,
}

#[derive(Debug, Default)]
struct PendingToolCall {
    id: Option<String>,
    name: String,
    arguments: String,
}

#[derive(Debug, Deserialize)]
struct TextFunctionCall {
    #[serde(rename = "type")]
    kind: String,
    name: Option<String>,
    arguments: Option<Value>,
    call_id: Option<String>,
}

#[derive(Debug)]
struct ChatStreamState {
    response_id: Option<String>,
    last_server_model: Option<String>,
    message_added: bool,
    message_text: String,
    emitted_text_len: usize,
    reasoning_added: bool,
    reasoning_done: bool,
    reasoning_text: String,
    tool_calls: BTreeMap<usize, PendingToolCall>,
    token_usage: Option<TokenUsage>,
    response_id_hint: Option<String>,
    finish_reason: Option<String>,
}

#[derive(Debug, Default)]
struct CommentFrameCounter {
    pending_line: Vec<u8>,
}

impl CommentFrameCounter {
    fn push(&mut self, bytes: &[u8]) -> u64 {
        let mut comment_frames = 0;
        for byte in bytes {
            if *byte == b'\n' {
                if line_is_sse_comment(&self.pending_line) {
                    comment_frames += 1;
                }
                self.pending_line.clear();
            } else {
                self.pending_line.push(*byte);
            }
        }
        comment_frames
    }
}

fn line_is_sse_comment(line: &[u8]) -> bool {
    let line = line.strip_suffix(b"\r").unwrap_or(line);
    line.iter()
        .copied()
        .find(|byte| !matches!(byte, b' ' | b'\t'))
        == Some(b':')
}

#[derive(Clone, Debug)]
struct ChatStreamActivity {
    comment_frame_count: Arc<AtomicU64>,
    sequence: Arc<AtomicU64>,
    tx_activity: watch::Sender<u64>,
}

impl ChatStreamActivity {
    fn new() -> (Self, watch::Receiver<u64>) {
        let (tx_activity, rx_activity) = watch::channel(0);
        (
            Self {
                comment_frame_count: Arc::new(AtomicU64::new(0)),
                sequence: Arc::new(AtomicU64::new(0)),
                tx_activity,
            },
            rx_activity,
        )
    }

    fn record_bytes(&self, bytes: &[u8], comment_frames: u64) {
        if bytes.is_empty() {
            return;
        }
        if comment_frames == 0 {
            return;
        }
        self.comment_frame_count
            .fetch_add(comment_frames, Ordering::Relaxed);
        let sequence = self.sequence.fetch_add(1, Ordering::Relaxed) + 1;
        let _ = self.tx_activity.send(sequence);
    }

    fn comment_frame_count(&self) -> u64 {
        self.comment_frame_count.load(Ordering::Relaxed)
    }
}

#[derive(Debug, Clone)]
struct ChatCallMetrics {
    call_index: u64,
    attempt_number: u64,
    started_at: Instant,
    inner: Arc<Mutex<ChatCallMetricsInner>>,
}

#[derive(Debug, Default)]
struct ChatCallMetricsInner {
    request_byte_size: usize,
    ms_to_response_headers: Option<u64>,
    ms_to_first_sse_byte: Option<u64>,
    ms_to_first_parsed_data_event: Option<u64>,
    ms_to_first_actionable_event: Option<u64>,
    comment_frame_count: u64,
    parsed_event_count: u64,
    x_request_id: Option<String>,
    generation_id: Option<String>,
    provider: Option<String>,
    emitted: bool,
}

#[derive(Debug, Serialize, PartialEq, Eq)]
struct ChatCallRetryLinkage {
    same_turn_attempt_index: u64,
}

#[derive(Debug, Serialize, PartialEq, Eq)]
struct ChatCallMetricsRecord {
    tag: &'static str,
    call_index: u64,
    attempt_number: u64,
    request_byte_size: usize,
    ms_to_response_headers: Option<u64>,
    ms_to_first_sse_byte: Option<u64>,
    ms_to_first_parsed_data_event: Option<u64>,
    ms_to_first_actionable_event: Option<u64>,
    total_stream_ms: u64,
    comment_frame_count: u64,
    parsed_event_count: u64,
    x_request_id: Option<String>,
    generation_id: Option<String>,
    provider: Option<String>,
    finish_reason: String,
    retry_linkage: ChatCallRetryLinkage,
}

impl ChatCallMetrics {
    fn new(request_byte_size: usize, same_turn_attempt_index: Option<u64>) -> Self {
        let attempt_number = same_turn_attempt_index.unwrap_or(1);
        Self {
            call_index: CHAT_CALL_SEQUENCE.fetch_add(1, Ordering::Relaxed) + 1,
            attempt_number,
            started_at: Instant::now(),
            inner: Arc::new(Mutex::new(ChatCallMetricsInner {
                request_byte_size,
                ..ChatCallMetricsInner::default()
            })),
        }
    }

    fn record_response_headers(&self, elapsed: Duration, headers: &HeaderMap) {
        let mut inner = self
            .inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        inner.ms_to_response_headers = Some(duration_ms(elapsed));
        inner.x_request_id = header_value(headers, REQUEST_ID_HEADER);
        inner.generation_id = GENERATION_ID_HEADERS
            .iter()
            .find_map(|header| header_value(headers, header));
    }

    fn record_response_header_error(&self, elapsed: Duration) {
        let mut inner = self
            .inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        inner.ms_to_response_headers = Some(duration_ms(elapsed));
    }

    fn record_sse_bytes(&self, bytes: &[u8], comment_frames: u64) {
        let elapsed_ms = self.elapsed_ms();
        let mut inner = self
            .inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if !bytes.is_empty() && inner.ms_to_first_sse_byte.is_none() {
            inner.ms_to_first_sse_byte = Some(elapsed_ms);
        }
        inner.comment_frame_count += comment_frames;
    }

    fn record_parsed_data_event(&self) {
        let elapsed_ms = self.elapsed_ms();
        let mut inner = self
            .inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if inner.ms_to_first_parsed_data_event.is_none() {
            inner.ms_to_first_parsed_data_event = Some(elapsed_ms);
        }
        inner.parsed_event_count += 1;
    }

    fn record_generation_id(&self, generation_id: Option<&str>) {
        let Some(generation_id) = generation_id.filter(|value| !value.is_empty()) else {
            return;
        };
        let mut inner = self
            .inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if inner.generation_id.is_none() {
            inner.generation_id = Some(generation_id.to_string());
        }
    }

    fn record_provider(&self, provider: Option<&str>) {
        let Some(provider) = provider.filter(|value| !value.is_empty()) else {
            return;
        };
        let mut inner = self
            .inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if inner.provider.is_none() {
            inner.provider = Some(provider.to_string());
        }
    }

    fn record_actionable_event(&self) {
        let elapsed_ms = self.elapsed_ms();
        let mut inner = self
            .inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if inner.ms_to_first_actionable_event.is_none() {
            inner.ms_to_first_actionable_event = Some(elapsed_ms);
        }
    }

    fn finish(&self, finish_reason: impl Into<String>) {
        let finish_reason = finish_reason.into();
        let total_stream_ms = self.elapsed_ms();
        let record = {
            let mut inner = self
                .inner
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if inner.emitted {
                return;
            }
            inner.emitted = true;
            ChatCallMetricsRecord {
                tag: CALL_METRICS_TAG,
                call_index: self.call_index,
                attempt_number: self.attempt_number,
                request_byte_size: inner.request_byte_size,
                ms_to_response_headers: inner.ms_to_response_headers,
                ms_to_first_sse_byte: inner.ms_to_first_sse_byte,
                ms_to_first_parsed_data_event: inner.ms_to_first_parsed_data_event,
                ms_to_first_actionable_event: inner.ms_to_first_actionable_event,
                total_stream_ms,
                comment_frame_count: inner.comment_frame_count,
                parsed_event_count: inner.parsed_event_count,
                x_request_id: inner.x_request_id.clone(),
                generation_id: inner.generation_id.clone(),
                provider: inner.provider.clone(),
                finish_reason,
                retry_linkage: ChatCallRetryLinkage {
                    same_turn_attempt_index: self.attempt_number,
                },
            }
        };

        let serialized = serialize_chat_call_metrics(&record);
        // Bench/exec harnesses consume these records from redirected stderr. When stderr IS the
        // terminal (interactive TUI), printing them corrupts the rendered UI with one JSON line
        // per API call, so route them to the tracing log instead.
        if std::io::stderr().is_terminal() {
            tracing::info!(target: "corbanu_call_metrics", "{serialized}");
        } else {
            eprintln!("{serialized}");
        }
    }

    fn elapsed_ms(&self) -> u64 {
        duration_ms(self.started_at.elapsed())
    }
}

fn header_value(headers: &HeaderMap, name: &str) -> Option<String> {
    headers
        .get(name)
        .and_then(|value| value.to_str().ok())
        .filter(|value| !value.is_empty())
        .map(str::to_string)
}

fn duration_ms(duration: Duration) -> u64 {
    u64::try_from(duration.as_millis()).unwrap_or(u64::MAX)
}

fn serialize_chat_call_metrics(record: &ChatCallMetricsRecord) -> String {
    serde_json::to_string(record).unwrap_or_else(|err| {
        format!(
            r#"{{"tag":"{CALL_METRICS_TAG}","finish_reason":"metrics serialization error: {err}"}}"#
        )
    })
}

impl ChatStreamState {
    fn new(response_id_hint: Option<String>) -> Self {
        Self {
            response_id: None,
            last_server_model: None,
            message_added: false,
            message_text: String::new(),
            emitted_text_len: 0,
            reasoning_added: false,
            reasoning_done: false,
            reasoning_text: String::new(),
            tool_calls: BTreeMap::new(),
            token_usage: None,
            response_id_hint,
            finish_reason: None,
        }
    }

    async fn process_chunk(
        &mut self,
        chunk: ChatCompletionChunk,
        tx_event: &mpsc::Sender<Result<ResponseEvent, ApiError>>,
    ) -> bool {
        if self.response_id.is_none() {
            self.response_id = chunk.id.clone();
        }
        if let Some(model) = chunk.model
            && self.last_server_model.as_deref() != Some(model.as_str())
        {
            if tx_event
                .send(Ok(ResponseEvent::ServerModel(model.clone())))
                .await
                .is_err()
            {
                return false;
            }
            self.last_server_model = Some(model);
        }
        if let Some(usage) = chunk.usage {
            self.token_usage = Some(usage.into());
        }

        for choice in chunk.choices {
            if let Some(finish_reason) = choice.finish_reason.as_deref() {
                if let Some(previous) = self.finish_reason.as_deref()
                    && previous != finish_reason
                {
                    warn!(
                        previous,
                        finish_reason, "chat completion changed finish reason"
                    );
                }
                self.finish_reason = Some(finish_reason.to_string());
            }
            // Hosts speak one of two reasoning dialects: Z.AI-style
            // `reasoning_content` or OpenRouter-normalized `reasoning`.
            let reasoning_delta = match (choice.delta.reasoning_content, choice.delta.reasoning) {
                (Some(reasoning), _) if !reasoning.is_empty() => Some(reasoning),
                (_, Some(reasoning)) if !reasoning.is_empty() => Some(reasoning),
                _ => None,
            };
            if let Some(delta) = reasoning_delta {
                if self.reasoning_done || self.message_added {
                    trace!(
                        "dropping late chat completions reasoning delta after visible output started"
                    );
                } else {
                    if !self.ensure_reasoning_item_added(tx_event).await {
                        return false;
                    }
                    self.reasoning_text.push_str(&delta);
                    if tx_event
                        .send(Ok(ResponseEvent::ReasoningContentDelta {
                            delta,
                            content_index: 0,
                        }))
                        .await
                        .is_err()
                    {
                        return false;
                    }
                }
            }

            if let Some(delta) = choice.delta.content
                && !delta.is_empty()
            {
                if !self.finish_reasoning_item(tx_event).await {
                    return false;
                }
                self.message_text.push_str(&delta);
                if !self.should_delay_text_delta() && !self.emit_pending_text_delta(tx_event).await
                {
                    return false;
                }
            }

            for tool_delta in choice.delta.tool_calls {
                let tool_call = self.tool_calls.entry(tool_delta.index).or_default();
                if let Some(id) = tool_delta.id {
                    tool_call.id = Some(id);
                }
                if let Some(function) = tool_delta.function {
                    if let Some(name) = function.name {
                        tool_call.name.push_str(&name);
                    }
                    if let Some(arguments) = function.arguments {
                        tool_call.arguments.push_str(&arguments);
                    }
                }
            }
        }

        true
    }

    async fn complete(mut self, tx_event: &mpsc::Sender<Result<ResponseEvent, ApiError>>) {
        // Same boundary as the Anthropic adapter: a stream that terminates having
        // produced no assistant text, no reasoning, and no tool call carries no model
        // output, and reporting it as a completed turn hides a failed turn as a
        // successful one. The in-stream silence timeout only covers streams that stall;
        // this covers streams that close cleanly with nothing in them.
        // `message_added` only flips once a text delta is actually emitted, and a
        // serialized function call deliberately withholds that emission while buffering
        // the text, so emptiness must be judged on the buffered text rather than on
        // whether a message item was announced.
        if self.message_text.is_empty()
            && self.reasoning_text.is_empty()
            && !self.reasoning_added
            && self.tool_calls.is_empty()
        {
            let _ = tx_event
                .send(Err(ApiError::Stream(
                    "chat completions stream completed without any model output".to_string(),
                )))
                .await;
            return;
        }
        let finish_reason = self
            .finish_reason
            .take()
            .map(CompletionFinishReason::from_provider);
        if let Some(CompletionFinishReason::ProviderError(reason)) = finish_reason.as_ref() {
            let _ = tx_event
                .send(Err(ApiError::Stream(format!(
                    "chat completions provider ended the stream with retryable finish reason \
                     `{reason}`"
                ))))
                .await;
            return;
        }
        let response_id = self.response_id();
        let message_id = format!("msg_{response_id}");
        let token_usage = self.token_usage.take();

        if !self.finish_reasoning_item(tx_event).await {
            return;
        }

        if !self.message_text.is_empty() {
            let held_back = self.emitted_text_len == 0;
            match parse_serialized_function_call_text(&self.message_text, held_back) {
                Ok(Some(item)) => {
                    if tx_event
                        .send(Ok(ResponseEvent::OutputItemDone(item)))
                        .await
                        .is_err()
                    {
                        return;
                    }
                }
                Ok(None) => {
                    if self.emitted_text_len < self.message_text.len()
                        && !self.emit_pending_text_delta(tx_event).await
                    {
                        return;
                    }
                    if self.message_added {
                        let item = ResponseItem::Message {
                            id: Some(ResponseItemId::from_server(message_id)),
                            role: "assistant".to_string(),
                            content: vec![ContentItem::OutputText {
                                text: self.message_text,
                            }],
                            phase: None,
                            internal_chat_message_metadata_passthrough: None,
                        };
                        if tx_event
                            .send(Ok(ResponseEvent::OutputItemDone(item)))
                            .await
                            .is_err()
                        {
                            return;
                        }
                    }
                }
                Err(message) => {
                    let _ = tx_event.send(Err(ApiError::Stream(message))).await;
                    return;
                }
            }
        }

        for (index, tool_call) in self.tool_calls {
            if tool_call.name.is_empty() {
                let call_id = tool_call.id.as_deref().unwrap_or("<missing>");
                let _ = tx_event
                    .send(Err(ApiError::Stream(format!(
                        "chat completions stream emitted a tool call without a function name \
                         at index {index}; call_id={call_id}; arguments excerpt: {}",
                        diagnostic_excerpt(&tool_call.arguments)
                    ))))
                    .await;
                return;
            }
            let call_id = tool_call
                .id
                .unwrap_or_else(|| format!("chatcmpl_call_{index}"));
            let item = ResponseItem::FunctionCall {
                id: Some(ResponseItemId::from_server(format!("fc_{call_id}"))),
                name: tool_call.name,
                namespace: None,
                arguments: tool_call.arguments,
                encrypted_function_args: None,
                call_id,
                internal_chat_message_metadata_passthrough: None,
            };
            if tx_event
                .send(Ok(ResponseEvent::OutputItemDone(item)))
                .await
                .is_err()
            {
                return;
            }
        }

        // A length-limited response is incomplete but recoverable: the turn runner records
        // the partial assistant output, requests a continuation, and caps consecutive
        // provider-driven continuations. Filtered and unknown terminal states are left
        // indeterminate so the turn runner can surface them as explicit errors.
        let end_turn = match finish_reason.as_ref() {
            Some(CompletionFinishReason::Stop) => Some(true),
            Some(CompletionFinishReason::ToolCalls)
            | Some(CompletionFinishReason::FunctionCall)
            | Some(CompletionFinishReason::Length) => Some(false),
            Some(CompletionFinishReason::ContentFilter)
            | Some(CompletionFinishReason::ProviderError(_))
            | Some(CompletionFinishReason::Unknown(_))
            | None => None,
        };
        let _ = tx_event
            .send(Ok(ResponseEvent::Completed {
                response_id,
                token_usage,
                end_turn,
                finish_reason,
            }))
            .await;
    }

    fn message_id(&self) -> String {
        format!("msg_{}", self.response_id())
    }

    fn response_id(&self) -> String {
        self.response_id
            .clone()
            .or_else(|| self.response_id_hint.clone())
            .unwrap_or_else(|| "chatcmpl-unknown".to_string())
    }

    fn reasoning_id(&self) -> String {
        format!("rs_{}", self.response_id())
    }

    async fn ensure_reasoning_item_added(
        &mut self,
        tx_event: &mpsc::Sender<Result<ResponseEvent, ApiError>>,
    ) -> bool {
        if self.reasoning_added {
            return true;
        }

        let item = ResponseItem::Reasoning {
            id: Some(ResponseItemId::from_server(self.reasoning_id())),
            summary: Vec::new(),
            content: Some(Vec::new()),
            encrypted_content: None,
            anthropic_content_block: None,
            internal_chat_message_metadata_passthrough: None,
        };
        if tx_event
            .send(Ok(ResponseEvent::OutputItemAdded(item)))
            .await
            .is_err()
        {
            return false;
        }
        self.reasoning_added = true;
        true
    }

    async fn finish_reasoning_item(
        &mut self,
        tx_event: &mpsc::Sender<Result<ResponseEvent, ApiError>>,
    ) -> bool {
        if !self.reasoning_added || self.reasoning_done {
            return true;
        }

        let content = (!self.reasoning_text.is_empty()).then(|| {
            vec![ReasoningItemContent::ReasoningText {
                text: self.reasoning_text.clone(),
            }]
        });
        let item = ResponseItem::Reasoning {
            id: Some(ResponseItemId::from_server(self.reasoning_id())),
            summary: Vec::<ReasoningItemReasoningSummary>::new(),
            content,
            encrypted_content: None,
            anthropic_content_block: None,
            internal_chat_message_metadata_passthrough: None,
        };
        if tx_event
            .send(Ok(ResponseEvent::OutputItemDone(item)))
            .await
            .is_err()
        {
            return false;
        }
        self.reasoning_done = true;
        true
    }

    fn should_delay_text_delta(&self) -> bool {
        self.emitted_text_len == 0 && is_potential_serialized_tool_text(&self.message_text)
    }

    async fn emit_pending_text_delta(
        &mut self,
        tx_event: &mpsc::Sender<Result<ResponseEvent, ApiError>>,
    ) -> bool {
        if self.emitted_text_len >= self.message_text.len() {
            return true;
        }

        if !self.message_added {
            let item = ResponseItem::Message {
                id: Some(ResponseItemId::from_server(self.message_id())),
                role: "assistant".to_string(),
                content: Vec::new(),
                phase: None,
                internal_chat_message_metadata_passthrough: None,
            };
            if tx_event
                .send(Ok(ResponseEvent::OutputItemAdded(item)))
                .await
                .is_err()
            {
                return false;
            }
            self.message_added = true;
        }

        let delta = self.message_text[self.emitted_text_len..].to_string();
        self.emitted_text_len = self.message_text.len();
        tx_event
            .send(Ok(ResponseEvent::OutputTextDelta {
                item_id: Some(self.message_id()),
                delta,
            }))
            .await
            .is_ok()
    }
}

fn is_potential_serialized_tool_text(text: &str) -> bool {
    let trimmed = text.trim_start();
    if trimmed.is_empty() {
        return true;
    }
    if !trimmed.starts_with('{') {
        return false;
    }

    let probe: String = trimmed
        .chars()
        .take(SERIALIZED_TOOL_TEXT_PROBE_CHARS)
        .collect();
    if probe.contains("\"function_call\"") || probe.contains("\"custom_tool_call\"") {
        return true;
    }
    if probe.contains("\"call_id\"")
        || (probe.contains("\"arguments\"") && probe.contains("\"name\""))
    {
        return true;
    }

    probe.len() < SERIALIZED_TOOL_TEXT_PROBE_CHARS
}

/// `text` is the whole assistant text; `held_back` says none of it was streamed yet.
/// Only a JSON object whose top-level `type` is a tool-call type is a serialized call.
/// Other text that merely names "function_call" (a JSON answer about function calling)
/// stays an ordinary message, and so does text already streamed that is not JSON.
fn parse_serialized_function_call_text(
    text: &str,
    held_back: bool,
) -> Result<Option<ResponseItem>, String> {
    if !looks_like_serialized_tool_call(text) {
        return Ok(None);
    }

    let trimmed = text.trim();
    let value: Value = match serde_json::from_str(trimmed) {
        Ok(value) => value,
        Err(_) if !held_back => return Ok(None),
        Err(err) => {
            return Err(format!(
                "chat completions stream emitted malformed function-call JSON as assistant \
                 text: {err}; raw excerpt: {}",
                diagnostic_excerpt(trimmed)
            ));
        }
    };
    if !value
        .get("type")
        .and_then(Value::as_str)
        .is_some_and(|kind| matches!(kind, "function_call" | "custom_tool_call"))
    {
        return Ok(None);
    }
    let parsed: TextFunctionCall = serde_json::from_value(value).map_err(|err| {
        format!(
            "chat completions stream emitted malformed function-call JSON as assistant text: \
             {err}; raw excerpt: {}",
            diagnostic_excerpt(trimmed)
        )
    })?;

    if parsed.kind != "function_call" {
        return Err(format!(
            "chat completions stream emitted unsupported serialized tool call type `{}` as \
             assistant text; raw excerpt: {}",
            parsed.kind,
            diagnostic_excerpt(trimmed)
        ));
    }

    let name = parsed.name.filter(|name| !name.is_empty()).ok_or_else(|| {
        format!(
            "chat completions stream emitted serialized function_call text without a name; \
             raw excerpt: {}",
            diagnostic_excerpt(trimmed)
        )
    })?;
    let call_id = parsed
        .call_id
        .filter(|call_id| !call_id.is_empty())
        .ok_or_else(|| {
            format!(
                "chat completions stream emitted serialized function_call text without call_id; \
             raw excerpt: {}",
                diagnostic_excerpt(trimmed)
            )
        })?;
    let arguments = parsed.arguments.ok_or_else(|| {
        format!(
            "chat completions stream emitted serialized function_call text without arguments; \
             raw excerpt: {}",
            diagnostic_excerpt(trimmed)
        )
    })?;
    let arguments = match arguments {
        Value::String(arguments) => arguments,
        other => serde_json::to_string(&other).map_err(|err| {
            format!(
                "chat completions stream emitted function_call arguments that could not be \
                 serialized: {err}; raw excerpt: {}",
                diagnostic_excerpt(trimmed)
            )
        })?,
    };

    Ok(Some(ResponseItem::FunctionCall {
        id: Some(ResponseItemId::from_server(format!("fc_{call_id}"))),
        name,
        namespace: None,
        arguments,
        encrypted_function_args: None,
        call_id,
        internal_chat_message_metadata_passthrough: None,
    }))
}

fn looks_like_serialized_tool_call(text: &str) -> bool {
    let trimmed = text.trim_start();
    trimmed.starts_with('{')
        && (trimmed.contains("\"function_call\"") || trimmed.contains("\"custom_tool_call\""))
}

fn diagnostic_excerpt(text: &str) -> String {
    const MAX_EXCERPT_CHARS: usize = 800;
    let mut excerpt: String = text.chars().take(MAX_EXCERPT_CHARS).collect();
    if text.chars().count() > MAX_EXCERPT_CHARS {
        excerpt.push_str("...");
    }
    excerpt.replace('\n', "\\n")
}

#[cfg(test)]
async fn process_chat_sse(
    stream: ByteStream,
    tx_event: mpsc::Sender<Result<ResponseEvent, ApiError>>,
    idle_timeout: Duration,
    actionable_silence_timeout: Duration,
    telemetry: Option<Arc<dyn SseTelemetry>>,
    response_id_hint: Option<String>,
    metrics: Option<ChatCallMetrics>,
) {
    process_chat_sse_observed(
        stream,
        tx_event,
        idle_timeout,
        actionable_silence_timeout,
        telemetry,
        response_id_hint,
        metrics,
        /*observer*/ None,
    )
    .await;
}

#[allow(clippy::too_many_arguments)]
async fn process_chat_sse_observed(
    stream: ByteStream,
    tx_event: mpsc::Sender<Result<ResponseEvent, ApiError>>,
    idle_timeout: Duration,
    actionable_silence_timeout: Duration,
    telemetry: Option<Arc<dyn SseTelemetry>>,
    response_id_hint: Option<String>,
    metrics: Option<ChatCallMetrics>,
    mut observer: Option<Arc<dyn accounting::ChatUsageObserver>>,
) {
    let mut position = 0_i64;
    let (activity, mut activity_rx) = ChatStreamActivity::new();
    let mut stream =
        byte_idle_timeout_stream(stream, idle_timeout, metrics.clone(), activity.clone())
            .eventsource();
    let mut state = ChatStreamState::new(response_id_hint);
    let mut first_activity_at: Option<Instant> = None;
    let mut actionable_deadline_at: Option<Instant> = None;

    loop {
        let start = Instant::now();
        let polled = tokio::select! {
            biased;
            _ = tx_event.closed(), if observer.is_some() => return,
            value = poll_chat_sse_event(&mut stream, &mut activity_rx, actionable_deadline_at) => value,
        };
        let response = match polled {
            ChatSsePoll::Activity => {
                let now = Instant::now();
                if first_activity_at.is_none() {
                    first_activity_at = Some(now);
                }
                actionable_deadline_at = Some(now + actionable_silence_timeout);
                continue;
            }
            ChatSsePoll::ActionableTimeout => {
                let elapsed = first_activity_at
                    .map(|started_at| started_at.elapsed())
                    .unwrap_or(actionable_silence_timeout);
                let message = actionable_silence_timeout_message(
                    elapsed,
                    actionable_silence_timeout,
                    activity.comment_frame_count(),
                );
                if let Some(metrics) = metrics.as_ref() {
                    metrics.finish(message.clone());
                }
                let _ = tx_event.send(Err(ApiError::Stream(message))).await;
                return;
            }
            ChatSsePoll::Event(response) => response,
        };
        let telemetry_response = Ok::<_, tokio::time::error::Elapsed>(response);
        if let Some(t) = telemetry.as_ref() {
            t.on_sse_poll(&telemetry_response, start.elapsed());
        }
        let sse = match telemetry_response {
            Ok(Some(Ok(sse))) => sse,
            Ok(Some(Err(e))) => {
                debug!("Chat completions SSE error: {e:#}");
                let message = chat_sse_error_message(e);
                if let Some(metrics) = metrics.as_ref() {
                    metrics.finish(message.clone());
                }
                let _ = tx_event.send(Err(ApiError::Stream(message))).await;
                return;
            }
            Ok(None) => {
                let message = "stream closed before chat completions finished".to_string();
                if let Some(metrics) = metrics.as_ref() {
                    metrics.finish(message.clone());
                }
                let _ = tx_event.send(Err(ApiError::Stream(message))).await;
                return;
            }
            Err(_) => {
                if let Some(metrics) = metrics.as_ref() {
                    metrics.finish(SSE_IDLE_TIMEOUT_MESSAGE);
                }
                let _ = tx_event
                    .send(Err(ApiError::Stream("idle timeout waiting for SSE".into())))
                    .await;
                return;
            }
        };

        trace!("Chat completions SSE event: {}", &sse.data);
        if let Some(metrics) = metrics.as_ref() {
            metrics.record_parsed_data_event();
        }
        if first_activity_at.is_none() {
            let now = Instant::now();
            first_activity_at = Some(now);
            actionable_deadline_at = Some(now + actionable_silence_timeout);
        }

        if let Some(active) = &observer {
            let next = position.checked_add(1);
            let patch = match next {
                Some(next) => {
                    position = next;
                    if sse.data.trim() == "[DONE]" {
                        Ok(None)
                    } else {
                        accounting::decode(&sse.data)
                    }
                }
                None => Err(accounting::InvalidChatUsage),
            };
            if !matches!(patch, Ok(None)) {
                let usage = patch.and_then(|value| value.ok_or(accounting::InvalidChatUsage));
                let invalid = usage.is_err();
                let result = tokio::select! {
                    biased;
                    _ = tx_event.closed() => return,
                    result = active.observe(position, usage) => result,
                };
                // Accounting observes the stream; it never ends it. Rejected
                // evidence stops observation for the rest of this response.
                if invalid || result.is_err() {
                    observer = None;
                }
            }
        }

        if sse.data.trim() == "[DONE]" {
            state.complete(&tx_event).await;
            if let Some(metrics) = metrics.as_ref() {
                metrics.finish("ok");
            }
            return;
        }

        if let Ok(error) = serde_json::from_str::<ChatErrorEnvelope>(&sse.data) {
            let mut message = error
                .error
                .message
                .unwrap_or_else(|| "chat completions stream returned an error".to_string());
            if let Some(code) = error.error.code
                && !code.is_empty()
            {
                message = format!("{message} ({code})");
            }
            if let Some(metrics) = metrics.as_ref() {
                metrics.finish(message.clone());
            }
            let _ = tx_event.send(Err(ApiError::Stream(message))).await;
            return;
        }

        let chunk = match serde_json::from_str::<ChatCompletionChunk>(&sse.data) {
            Ok(chunk) => chunk,
            Err(err) => {
                debug!(
                    "failed to parse chat completions SSE event: {err}, data: {}",
                    &sse.data
                );
                continue;
            }
        };

        if let Some(metrics) = metrics.as_ref() {
            metrics.record_generation_id(chunk.id.as_deref());
            metrics.record_provider(chunk.provider.as_deref());
            if chunk_has_actionable_event(&chunk) {
                metrics.record_actionable_event();
            }
        }
        if chunk_has_actionable_event(&chunk) {
            actionable_deadline_at = Some(Instant::now() + actionable_silence_timeout);
        }
        if !state.process_chunk(chunk, &tx_event).await {
            if let Some(metrics) = metrics.as_ref() {
                metrics.finish("receiver dropped before chat completions finished");
            }
            return;
        }
    }
}

enum ChatSsePoll {
    Activity,
    ActionableTimeout,
    Event(Option<Result<Event, EventStreamError<TransportError>>>),
}

async fn poll_chat_sse_event(
    stream: &mut (impl futures::Stream<Item = Result<Event, EventStreamError<TransportError>>> + Unpin),
    activity_rx: &mut watch::Receiver<u64>,
    actionable_deadline_at: Option<Instant>,
) -> ChatSsePoll {
    tokio::select! {
        _ = async {
            if let Some(deadline) = actionable_deadline_at {
                tokio::time::sleep_until(deadline).await;
            } else {
                std::future::pending::<()>().await;
            }
        } => ChatSsePoll::ActionableTimeout,
        changed = activity_rx.changed() => {
            if changed.is_ok() {
                ChatSsePoll::Activity
            } else {
                ChatSsePoll::Event(None)
            }
        }
        response = stream.next() => ChatSsePoll::Event(response),
    }
}

fn actionable_silence_timeout_message(
    elapsed: Duration,
    timeout: Duration,
    comment_frame_count: u64,
) -> String {
    format!(
        "actionable silence timeout: no content, tool-call, or reasoning delta for {}ms after \
         stream activity; elapsed_ms={}; comment_frame_count={comment_frame_count}",
        duration_ms(timeout),
        duration_ms(elapsed),
    )
}

fn byte_idle_timeout_stream(
    stream: ByteStream,
    idle_timeout: Duration,
    metrics: Option<ChatCallMetrics>,
    activity: ChatStreamActivity,
) -> ByteStream {
    Box::pin(stream::unfold(
        (stream, CommentFrameCounter::default()),
        move |(mut stream, mut comment_counter)| {
            let metrics = metrics.clone();
            let activity = activity.clone();
            async move {
                match timeout(idle_timeout, stream.next()).await {
                    Ok(Some(item)) => {
                        let item = item.inspect(|bytes| {
                            let comment_frames = comment_counter.push(bytes);
                            if let Some(metrics) = metrics.as_ref() {
                                metrics.record_sse_bytes(bytes, comment_frames);
                            }
                            activity.record_bytes(bytes, comment_frames);
                        });
                        Some((item, (stream, comment_counter)))
                    }
                    Ok(None) => None,
                    Err(_) => Some((
                        Err(TransportError::Network(
                            SSE_IDLE_TIMEOUT_MESSAGE.to_string(),
                        )),
                        (stream, comment_counter),
                    )),
                }
            }
        },
    ))
}

fn chunk_has_actionable_event(chunk: &ChatCompletionChunk) -> bool {
    chunk.choices.iter().any(|choice| {
        choice
            .delta
            .content
            .as_deref()
            .is_some_and(|content| !content.is_empty())
            || choice
                .delta
                .reasoning_content
                .as_deref()
                .is_some_and(|reasoning| !reasoning.is_empty())
            || choice
                .delta
                .reasoning
                .as_deref()
                .is_some_and(|reasoning| !reasoning.is_empty())
            || !choice.delta.reasoning_details.is_empty()
            || choice.delta.tool_calls.iter().any(|tool_call| {
                tool_call.id.as_deref().is_some_and(|id| !id.is_empty())
                    || tool_call.function.as_ref().is_some_and(|function| {
                        function
                            .name
                            .as_deref()
                            .is_some_and(|name| !name.is_empty())
                            || function
                                .arguments
                                .as_deref()
                                .is_some_and(|arguments| !arguments.is_empty())
                    })
            })
    })
}

fn chat_sse_error_message(error: EventStreamError<TransportError>) -> String {
    match error {
        EventStreamError::Transport(TransportError::Network(message))
            if message == SSE_IDLE_TIMEOUT_MESSAGE =>
        {
            message
        }
        error => error.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use assert_matches::assert_matches;
    use bytes::Bytes;
    use codex_client::TransportError;
    use futures::TryStreamExt;
    use pretty_assertions::assert_eq;
    use tokio_test::io::Builder as IoBuilder;
    use tokio_util::io::ReaderStream;

    async fn collect_events(chunks: &[&[u8]]) -> Vec<Result<ResponseEvent, ApiError>> {
        let mut builder = IoBuilder::new();
        for chunk in chunks {
            builder.read(chunk);
        }

        let reader = builder.build();
        let body =
            ReaderStream::new(reader).map_err(|err| TransportError::Network(err.to_string()));
        let (tx_event, mut rx_event) = mpsc::channel(1600);
        process_chat_sse(
            Box::pin(body),
            tx_event,
            Duration::from_secs(5),
            DEFAULT_ACTIONABLE_SILENCE_TIMEOUT,
            /*telemetry*/ None,
            Some("req_123".to_string()),
            /*metrics*/ None,
        )
        .await;

        let mut events = Vec::new();
        while let Some(event) = rx_event.recv().await {
            events.push(event);
        }
        events
    }

    fn content_event(id: &str, content: &str) -> Vec<u8> {
        format!(
            "data: {}\n\n",
            serde_json::json!({
                "id": id,
                "choices": [
                    {
                        "delta": {
                            "content": content,
                        }
                    }
                ],
            })
        )
        .into_bytes()
    }

    fn delayed_body(chunks: Vec<(Duration, Vec<u8>)>) -> ByteStream {
        Box::pin(futures::stream::unfold(
            chunks.into_iter(),
            |mut chunks| async move {
                let (delay, chunk) = chunks.next()?;
                if !delay.is_zero() {
                    tokio::time::sleep(delay).await;
                }
                Some((Ok(Bytes::from(chunk)), chunks))
            },
        ))
    }

    #[tokio::test]
    async fn parses_text_deltas_and_usage() {
        let events = collect_events(&[
            br#"data: {"id":"chatcmpl-1","model":"ambient/large","choices":[{"delta":{"role":"assistant","content":"he"}}],"usage":null}"#,
            b"\n\n",
            br#"data: {"id":"chatcmpl-1","model":"ambient/large","choices":[{"delta":{"content":"llo"}}],"usage":null}"#,
            b"\n\n",
            br#"data: {"id":"chatcmpl-1","choices":[],"usage":{"prompt_tokens":3,"completion_tokens":2,"total_tokens":5,"prompt_tokens_details":{"cached_tokens":1},"completion_tokens_details":{"reasoning_tokens":0}}}"#,
            b"\n\n",
            b"data: [DONE]\n\n",
        ])
        .await;

        assert_matches!(&events[0], Ok(ResponseEvent::ServerModel(model)) if model == "ambient/large");
        assert_matches!(
            &events[1],
            Ok(ResponseEvent::OutputItemAdded(ResponseItem::Message { .. }))
        );
        assert_matches!(
            &events[2],
            Ok(ResponseEvent::OutputTextDelta { item_id: Some(item_id), delta })
                if item_id == "msg_chatcmpl-1" && delta == "he"
        );
        assert_matches!(
            &events[3],
            Ok(ResponseEvent::OutputTextDelta { item_id: Some(item_id), delta })
                if item_id == "msg_chatcmpl-1" && delta == "llo"
        );
        assert_matches!(
            &events[4],
            Ok(ResponseEvent::OutputItemDone(ResponseItem::Message { content, .. }))
                if content == &vec![ContentItem::OutputText { text: "hello".to_string() }]
        );
        assert_matches!(
            &events[5],
            Ok(ResponseEvent::Completed {
                response_id,
                token_usage: Some(TokenUsage {
                    input_tokens: 3,
                    cached_input_tokens: 1,
                    output_tokens: 2,
                    reasoning_output_tokens: 0,
                    total_tokens: 5,
                    ..
                }),
                ..
            }) if response_id == "chatcmpl-1"
        );
        assert_eq!(events.len(), 6);
    }

    #[tokio::test]
    async fn parses_sglang_text_deltas_with_null_optional_collections() {
        let events = collect_events(&[
            br#"data: {"id":"chatcmpl-sglang","model":"deepseek-ai/DeepSeek-V4-Flash","choices":[{"index":0,"delta":{"reasoning_content":null,"reasoning_details":null,"role":"assistant","content":""},"finish_reason":null}]}"#,
            b"\n\n",
            br#"data: {"id":"chatcmpl-sglang","choices":[{"index":0,"delta":{"role":null,"content":"RENTED_GPU_OK","reasoning_content":null,"reasoning_details":null,"tool_calls":null},"finish_reason":null}],"usage":null}"#,
            b"\n\n",
            br#"data: {"id":"chatcmpl-sglang","choices":[{"index":0,"delta":{"reasoning_content":null},"finish_reason":"stop"}]}"#,
            b"\n\n",
            b"data: [DONE]\n\n",
        ])
        .await;

        assert_matches!(
            &events[0],
            Ok(ResponseEvent::ServerModel(model))
                if model == "deepseek-ai/DeepSeek-V4-Flash"
        );
        assert_matches!(
            &events[1],
            Ok(ResponseEvent::OutputItemAdded(ResponseItem::Message { .. }))
        );
        assert_matches!(
            &events[2],
            Ok(ResponseEvent::OutputTextDelta { delta, .. }) if delta == "RENTED_GPU_OK"
        );
        assert_matches!(
            &events[3],
            Ok(ResponseEvent::OutputItemDone(ResponseItem::Message { content, .. }))
                if content == &vec![ContentItem::OutputText {
                    text: "RENTED_GPU_OK".to_string(),
                }]
        );
        assert_matches!(
            &events[4],
            Ok(ResponseEvent::Completed {
                response_id,
                end_turn: Some(true),
                finish_reason: Some(CompletionFinishReason::Stop),
                ..
            }) if response_id == "chatcmpl-sglang"
        );
        assert_eq!(events.len(), 5);
    }

    #[tokio::test]
    async fn preserves_and_classifies_chat_completion_finish_reasons() {
        let cases = [
            ("stop", CompletionFinishReason::Stop, Some(true)),
            ("length", CompletionFinishReason::Length, Some(false)),
            (
                "content_filter",
                CompletionFinishReason::ContentFilter,
                None,
            ),
            ("tool_calls", CompletionFinishReason::ToolCalls, Some(false)),
            (
                "provider_new_reason",
                CompletionFinishReason::Unknown("provider_new_reason".to_string()),
                None,
            ),
        ];

        for (raw, expected_reason, expected_end_turn) in cases {
            // Carry one content token. This test covers finish-reason classification;
            // a stream with no content at all is a separate failure case and is
            // rejected before a finish reason is ever classified.
            let content = content_event("chatcmpl-finish", "ok");
            let terminal = format!(
                "data: {{\"id\":\"chatcmpl-finish\",\"choices\":[{{\"delta\":{{}},\"finish_reason\":\"{raw}\"}}]}}\n\n"
            );
            let events =
                collect_events(&[content.as_slice(), terminal.as_bytes(), b"data: [DONE]\n\n"])
                    .await;
            assert_matches!(
                events.last(),
                Some(Ok(ResponseEvent::Completed {
                    finish_reason: Some(reason),
                    end_turn,
                    ..
                })) if reason == &expected_reason && end_turn == &expected_end_turn,
                "finish reason {raw} was not preserved"
            );
        }
    }

    #[tokio::test]
    async fn provider_error_finish_reasons_fail_before_committing_partial_output() {
        for raw in ["error", "failed", "server_error"] {
            let content = content_event("chatcmpl-provider-error", "partial output");
            let terminal = format!(
                "data: {{\"id\":\"chatcmpl-provider-error\",\"choices\":[{{\"delta\":{{}},\"finish_reason\":\"{raw}\"}}]}}\n\n"
            );
            let events =
                collect_events(&[content.as_slice(), terminal.as_bytes(), b"data: [DONE]\n\n"])
                    .await;

            assert!(
                events
                    .iter()
                    .all(|event| !matches!(event, Ok(ResponseEvent::OutputItemDone(_)))),
                "provider error `{raw}` must not commit partial output"
            );
            assert_matches!(
                events.last(),
                Some(Err(ApiError::Stream(message)))
                    if message.contains(raw) && message.contains("retryable"),
                "provider error `{raw}` must become a retryable stream error"
            );
        }
    }

    #[tokio::test]
    async fn parses_streamed_tool_call() {
        let events = collect_events(&[
            br#"data: {"id":"chatcmpl-2","choices":[{"delta":{"tool_calls":[{"index":0,"id":"call_1","type":"function","function":{"name":"exec_command","arguments":"{\"cmd\":"}}]}}]}"#,
            b"\n\n",
            br#"data: {"id":"chatcmpl-2","choices":[{"delta":{"tool_calls":[{"index":0,"function":{"arguments":"\"date\"}"}}]}}]}"#,
            b"\n\n",
            b"data: [DONE]\n\n",
        ])
        .await;

        assert_matches!(
            &events[0],
            Ok(ResponseEvent::OutputItemDone(ResponseItem::FunctionCall {
                name,
                arguments,
                call_id,
                ..
            })) if name == "exec_command" && arguments == "{\"cmd\":\"date\"}" && call_id == "call_1"
        );
        assert_matches!(
            &events[1],
            Ok(ResponseEvent::Completed { response_id, .. }) if response_id == "chatcmpl-2"
        );
        assert_eq!(events.len(), 2);
    }

    #[tokio::test]
    async fn parses_reasoning_content_without_leaking_as_text() {
        let events = collect_events(&[
            br#"data: {"id":"chatcmpl-reasoning","choices":[{"delta":{"reasoning_content":"private thought "}}]}"#,
            b"\n\n",
            br#"data: {"id":"chatcmpl-reasoning","choices":[{"delta":{"reasoning_content":"trace"}}]}"#,
            b"\n\n",
            br#"data: {"id":"chatcmpl-reasoning","choices":[{"delta":{"content":"visible answer"}}]}"#,
            b"\n\n",
            b"data: [DONE]\n\n",
        ])
        .await;

        assert_matches!(
            &events[0],
            Ok(ResponseEvent::OutputItemAdded(ResponseItem::Reasoning { id: Some(id), .. }))
                if id.as_str() == "rs_chatcmpl-reasoning"
        );
        assert_matches!(
            &events[1],
            Ok(ResponseEvent::ReasoningContentDelta { delta, content_index })
                if delta == "private thought " && *content_index == 0
        );
        assert_matches!(
            &events[2],
            Ok(ResponseEvent::ReasoningContentDelta { delta, content_index })
                if delta == "trace" && *content_index == 0
        );
        assert_matches!(
            &events[3],
            Ok(ResponseEvent::OutputItemDone(ResponseItem::Reasoning {
                content: Some(content),
                ..
            })) if content == &vec![ReasoningItemContent::ReasoningText {
                text: "private thought trace".to_string(),
            }]
        );
        assert_matches!(
            &events[4],
            Ok(ResponseEvent::OutputItemAdded(ResponseItem::Message { .. }))
        );
        assert_matches!(
            &events[5],
            Ok(ResponseEvent::OutputTextDelta { delta, .. }) if delta == "visible answer"
        );
        assert_matches!(
            &events[6],
            Ok(ResponseEvent::OutputItemDone(ResponseItem::Message { content, .. }))
                if content == &vec![ContentItem::OutputText {
                    text: "visible answer".to_string(),
                }]
        );
        assert_matches!(
            &events[7],
            Ok(ResponseEvent::Completed { response_id, .. }) if response_id == "chatcmpl-reasoning"
        );
        assert_eq!(events.len(), 8);
    }

    #[tokio::test]
    async fn parses_serialized_function_call_text_without_leaking_as_text() {
        let content = serde_json::json!({
            "type": "function_call",
            "name": "exec_command",
            "arguments": {
                "cmd": "date"
            },
            "call_id": "chatcmpl-tool-1",
        })
        .to_string();
        let event = content_event("chatcmpl-serialized", &content);

        let events = collect_events(&[event.as_slice(), b"data: [DONE]\n\n"]).await;

        assert_matches!(
            &events[0],
            Ok(ResponseEvent::OutputItemDone(ResponseItem::FunctionCall {
                name,
                arguments,
                call_id,
                ..
            })) if name == "exec_command" && arguments == "{\"cmd\":\"date\"}" && call_id == "chatcmpl-tool-1"
        );
        assert_matches!(
            &events[1],
            Ok(ResponseEvent::Completed { response_id, .. }) if response_id == "chatcmpl-serialized"
        );
        assert_eq!(events.len(), 2);
    }

    #[tokio::test]
    async fn flushes_ordinary_json_text_after_short_probe() {
        let content = format!(
            "{{\"status\":\"{}\",\"message\":\"not a serialized tool call\"}}",
            "a".repeat(SERIALIZED_TOOL_TEXT_PROBE_CHARS)
        );
        let event = content_event("chatcmpl-json-text", &content);

        let events = collect_events(&[event.as_slice(), b"data: [DONE]\n\n"]).await;

        assert_matches!(
            &events[0],
            Ok(ResponseEvent::OutputItemAdded(ResponseItem::Message { .. }))
        );
        assert_matches!(&events[1], Ok(ResponseEvent::OutputTextDelta { delta, .. }) if delta == &content);
        assert_matches!(
            &events[2],
            Ok(ResponseEvent::OutputItemDone(ResponseItem::Message { content: final_content, .. }))
                if final_content == &vec![ContentItem::OutputText { text: content }]
        );
        assert_matches!(
            &events[3],
            Ok(ResponseEvent::Completed { response_id, .. }) if response_id == "chatcmpl-json-text"
        );
        assert_eq!(events.len(), 4);
    }

    /// The events of one assistant message whose whole text is `content`.
    fn assert_single_text_message(
        events: &[Result<ResponseEvent, ApiError>],
        content: &str,
        response_id: &str,
    ) {
        assert_matches!(
            &events[0],
            Ok(ResponseEvent::OutputItemAdded(ResponseItem::Message { .. }))
        );
        assert_matches!(&events[1], Ok(ResponseEvent::OutputTextDelta { delta, .. }) if delta == content);
        assert_matches!(
            &events[2],
            Ok(ResponseEvent::OutputItemDone(ResponseItem::Message { content: final_content, .. }))
                if final_content == &vec![ContentItem::OutputText { text: content.to_string() }]
        );
        assert_matches!(
            &events[3],
            Ok(ResponseEvent::Completed { response_id: id, .. }) if id == response_id
        );
        assert_eq!(events.len(), 4);
    }

    #[tokio::test]
    async fn serialized_tool_text_streamed_json_that_mentions_function_call_stays_a_message() {
        // A JSON answer (the body of a legacy function-calling request) whose first 96
        // characters look ordinary, so it streams as text, and which names
        // "function_call" further on.
        let content = r#"{"model":"gpt-3.5-turbo-0613","messages":[{"role":"user","content":"What is the weather like in Boston today?"}],"functions":[{"name":"get_current_weather","parameters":{"type":"object"}}],"function_call":"auto"}"#;
        let event = content_event("chatcmpl-json-mentions-call", content);

        let events = collect_events(&[event.as_slice(), b"data: [DONE]\n\n"]).await;

        assert_single_text_message(&events, content, "chatcmpl-json-mentions-call");
    }

    #[tokio::test]
    async fn serialized_tool_text_streamed_json_and_prose_that_mention_function_call_stay_a_message()
     {
        let content = concat!(
            r#"{"model":"gpt-3.5-turbo-0613","messages":[{"role":"user","content":"What is the weather like in Boston today?"}],"function_call":"auto"}"#,
            "\n\nSetting \"function_call\" to \"auto\" lets the model decide.",
        );
        let event = content_event("chatcmpl-json-and-prose", content);

        let events = collect_events(&[event.as_slice(), b"data: [DONE]\n\n"]).await;

        assert_single_text_message(&events, content, "chatcmpl-json-and-prose");
    }

    #[tokio::test]
    async fn serialized_tool_text_held_back_json_without_a_tool_call_type_stays_a_message() {
        // "function_call" in the first 96 characters holds the text back; it is still an
        // ordinary JSON object with no tool-call `type`.
        let content = r#"{"function_call":"auto","functions":[{"name":"get_current_weather"}]}"#;
        let event = content_event("chatcmpl-held-json", content);

        let events = collect_events(&[event.as_slice(), b"data: [DONE]\n\n"]).await;

        assert_single_text_message(&events, content, "chatcmpl-held-json");
    }

    #[tokio::test]
    async fn errors_on_malformed_serialized_function_call_text() {
        let content = r#"{"type":"function_call","name":"exec_command","arguments":"{"cmd":"date"}","call_id":"chatcmpl-tool-bad"}"#;
        let event = content_event("chatcmpl-malformed", content);

        let events = collect_events(&[event.as_slice(), b"data: [DONE]\n\n"]).await;

        assert_matches!(
            &events[0],
            Err(ApiError::Stream(message))
                if message.contains("malformed function-call JSON as assistant text")
                    && message.contains("raw excerpt")
                    && message.contains("chatcmpl-tool-bad")
        );
        assert_eq!(events.len(), 1);
    }

    #[tokio::test]
    async fn errors_on_tool_call_delta_without_function_name() {
        let events = collect_events(&[
            br#"data: {"id":"chatcmpl-nameless","choices":[{"delta":{"tool_calls":[{"index":0,"id":"call_1","type":"function","function":{"arguments":"{\"cmd\":\"date\"}"}}]}}]}"#,
            b"\n\n",
            b"data: [DONE]\n\n",
        ])
        .await;

        assert_matches!(
            &events[0],
            Err(ApiError::Stream(message))
                if message.contains("tool call without a function name")
                    && message.contains("call_id=call_1")
                    && message.contains(r#"{"cmd":"date"}"#)
        );
        assert_eq!(events.len(), 1);
    }

    #[tokio::test]
    async fn comment_frames_keep_idle_timer_alive() {
        let body = delayed_body(vec![
            (Duration::ZERO, b": OPENROUTER PROCESSING\n\n".to_vec()),
            (
                Duration::from_millis(20),
                b": OPENROUTER PROCESSING\n\n".to_vec(),
            ),
            (
                Duration::from_millis(20),
                b": OPENROUTER PROCESSING\n\n".to_vec(),
            ),
            (
                Duration::from_millis(20),
                br#"data: {"id":"chatcmpl-comments","choices":[{"delta":{"content":"ok"}}]}"#
                    .to_vec(),
            ),
            (Duration::ZERO, b"\n\n".to_vec()),
            (Duration::ZERO, b"data: [DONE]\n\n".to_vec()),
        ]);
        let (tx_event, mut rx_event) = mpsc::channel(1600);

        process_chat_sse(
            body,
            tx_event,
            Duration::from_millis(30),
            DEFAULT_ACTIONABLE_SILENCE_TIMEOUT,
            /*telemetry*/ None,
            /*response_id_hint*/ None,
            /*metrics*/ None,
        )
        .await;

        let mut events = Vec::new();
        while let Some(event) = rx_event.recv().await {
            events.push(event);
        }

        assert!(
            events.iter().all(Result::is_ok),
            "comment keepalives should not produce errors: {events:?}"
        );
        assert_matches!(
            events.last(),
            Some(Ok(ResponseEvent::Completed { response_id, .. }))
                if response_id == "chatcmpl-comments"
        );
    }

    #[tokio::test]
    async fn comment_only_stream_survives_actionable_silence_timeout() {
        // Keepalives every 200ms span 1.2s, half again the 800ms
        // actionable-silence budget, so only the keepalives keep the stream
        // alive. Each gap leaves 600ms of slack: with 200ms against 300ms, a
        // loaded Windows Bazel run stretched one gap past the budget (#636).
        let keepalive = || {
            (
                Duration::from_millis(200),
                b": OPENROUTER PROCESSING\n\n".to_vec(),
            )
        };
        let mut chunks = vec![(Duration::ZERO, b": OPENROUTER PROCESSING\n\n".to_vec())];
        chunks.extend(std::iter::repeat_with(keepalive).take(5));
        chunks.extend([
            (
                Duration::from_millis(200),
                br#"data: {"id":"chatcmpl-late","choices":[{"delta":{"content":"late"}}]}"#
                    .to_vec(),
            ),
            (Duration::ZERO, b"\n\n".to_vec()),
            (Duration::ZERO, b"data: [DONE]\n\n".to_vec()),
        ]);
        let body = delayed_body(chunks);
        let (tx_event, mut rx_event) = mpsc::channel(1600);

        process_chat_sse(
            body,
            tx_event,
            Duration::from_millis(2_000),
            Duration::from_millis(800),
            /*telemetry*/ None,
            /*response_id_hint*/ None,
            /*metrics*/ None,
        )
        .await;

        let mut events = Vec::new();
        while let Some(event) = rx_event.recv().await {
            events.push(event);
        }

        assert!(
            events.iter().all(Result::is_ok),
            "comment keepalives should keep actionable silence alive: {events:?}"
        );
        assert_matches!(
            events.last(),
            Some(Ok(ResponseEvent::Completed { response_id, .. }))
                if response_id == "chatcmpl-late"
        );
    }

    #[test]
    fn reasoning_dialects_count_as_actionable() {
        let normalized: ChatCompletionChunk =
            serde_json::from_str(r#"{"choices":[{"delta":{"reasoning":"thinking"}}]}"#)
                .expect("chunk");
        assert!(chunk_has_actionable_event(&normalized));

        let details_only: ChatCompletionChunk = serde_json::from_str(
            r#"{"choices":[{"delta":{"reasoning_details":[{"type":"reasoning.encrypted"}]}}]}"#,
        )
        .expect("chunk");
        assert!(chunk_has_actionable_event(&details_only));

        let role_only: ChatCompletionChunk =
            serde_json::from_str(r#"{"choices":[{"delta":{"role":"assistant"}}]}"#).expect("chunk");
        assert!(!chunk_has_actionable_event(&role_only));
    }

    #[tokio::test]
    async fn normalized_reasoning_deltas_keep_actionable_silence_alive() {
        // Fast-pool OpenRouter hosts (StreamLake/AkashML/Together, probed
        // 2026-07-02) stream thinking as `delta.reasoning`, not
        // `reasoning_content`. Ten reasoning frames 50ms apart span 500ms
        // with a 250ms actionable timeout; the stream must survive because
        // each delta refreshes the deadline (pre-fix it was killed after the
        // first timeout). The 200ms of slack per gap covers loaded Windows
        // Bazel runs, which stretched 20ms gaps past a 60ms timeout (#636).
        let reasoning_frame = |i: u64| {
            format!(
                "data: {{\"id\":\"chatcmpl-r\",\"provider\":\"StreamLake\",\"choices\":[{{\"delta\":{{\"reasoning\":\"t{i}\"}}}}]}}\n\n"
            )
            .into_bytes()
        };
        let mut frames: Vec<(Duration, Vec<u8>)> = (0..10)
            .map(|i| (Duration::from_millis(50), reasoning_frame(i)))
            .collect();
        frames.push((
            Duration::from_millis(50),
            br#"data: {"id":"chatcmpl-r","choices":[{"delta":{"content":"done"}}]}"#.to_vec(),
        ));
        frames.push((Duration::ZERO, b"\n\n".to_vec()));
        frames.push((Duration::ZERO, b"data: [DONE]\n\n".to_vec()));
        let (tx_event, mut rx_event) = mpsc::channel(1600);

        process_chat_sse(
            delayed_body(frames),
            tx_event,
            Duration::from_millis(1_000),
            Duration::from_millis(250),
            /*telemetry*/ None,
            /*response_id_hint*/ None,
            /*metrics*/ None,
        )
        .await;

        let mut saw_reasoning_delta = false;
        while let Some(event) = rx_event.recv().await {
            match event {
                Ok(ResponseEvent::ReasoningContentDelta { .. }) => saw_reasoning_delta = true,
                Err(err) => panic!("normalized reasoning stream must survive, got {err:?}"),
                Ok(_) => {}
            }
        }
        assert!(
            saw_reasoning_delta,
            "normalized `reasoning` deltas must surface as reasoning events"
        );
    }

    #[tokio::test]
    async fn truly_silent_stream_hits_idle_timeout() {
        let body: ByteStream =
            Box::pin(futures::stream::pending::<Result<Bytes, TransportError>>());
        let (tx_event, mut rx_event) = mpsc::channel(1600);

        process_chat_sse(
            body,
            tx_event,
            Duration::from_millis(10),
            Duration::from_millis(50),
            /*telemetry*/ None,
            /*response_id_hint*/ None,
            /*metrics*/ None,
        )
        .await;

        let event = rx_event.recv().await.expect("event should be emitted");
        assert_matches!(
            event,
            Err(ApiError::Stream(message)) if message == SSE_IDLE_TIMEOUT_MESSAGE
        );
    }

    #[tokio::test]
    async fn returns_error_when_stream_closes_without_done() {
        let reader = IoBuilder::new()
            .read(br#"data: {"id":"chatcmpl-3","choices":[]}"#)
            .read(b"\n\n")
            .build();
        let body =
            ReaderStream::new(reader).map_err(|err| TransportError::Network(err.to_string()));
        let (tx_event, mut rx_event) = mpsc::channel(1600);

        process_chat_sse(
            Box::pin(body),
            tx_event,
            Duration::from_secs(5),
            DEFAULT_ACTIONABLE_SILENCE_TIMEOUT,
            /*telemetry*/ None,
            /*response_id_hint*/ None,
            /*metrics*/ None,
        )
        .await;

        let event = rx_event.recv().await.expect("event should be emitted");
        assert_matches!(event, Err(ApiError::Stream(_)));
    }

    #[test]
    fn serializes_chat_call_metrics_line() {
        let record = ChatCallMetricsRecord {
            tag: CALL_METRICS_TAG,
            call_index: 7,
            attempt_number: 2,
            request_byte_size: 1234,
            ms_to_response_headers: Some(50),
            ms_to_first_sse_byte: Some(60),
            ms_to_first_parsed_data_event: Some(70),
            ms_to_first_actionable_event: Some(80),
            total_stream_ms: 900,
            comment_frame_count: 3,
            parsed_event_count: 4,
            x_request_id: Some("req_123".to_string()),
            generation_id: Some("gen_456".to_string()),
            provider: Some("StreamLake".to_string()),
            finish_reason: "ok".to_string(),
            retry_linkage: ChatCallRetryLinkage {
                same_turn_attempt_index: 2,
            },
        };

        let value: serde_json::Value = serde_json::from_str(&serialize_chat_call_metrics(&record))
            .expect("metrics should serialize as JSON");

        assert_eq!(value["tag"], CALL_METRICS_TAG);
        assert_eq!(value["attempt_number"], 2);
        assert_eq!(value["request_byte_size"], 1234);
        assert_eq!(value["ms_to_response_headers"], 50);
        assert_eq!(value["ms_to_first_sse_byte"], 60);
        assert_eq!(value["ms_to_first_parsed_data_event"], 70);
        assert_eq!(value["ms_to_first_actionable_event"], 80);
        assert_eq!(value["total_stream_ms"], 900);
        assert_eq!(value["comment_frame_count"], 3);
        assert_eq!(value["parsed_event_count"], 4);
        assert_eq!(value["x_request_id"], "req_123");
        assert_eq!(value["generation_id"], "gen_456");
        assert_eq!(value["finish_reason"], "ok");
        assert_eq!(value["retry_linkage"]["same_turn_attempt_index"], 2);
    }

    #[test]
    fn comment_frame_counter_handles_split_lines() {
        let mut counter = CommentFrameCounter::default();

        assert_eq!(counter.push(b": OPENROUTER"), 0);
        assert_eq!(counter.push(b" PROCESSING\r\n"), 1);
        assert_eq!(counter.push(b"data: {}\n"), 0);
        assert_eq!(counter.push(b"\t: keepalive\n"), 1);
    }
}
