use std::collections::HashMap;
use std::collections::HashSet;

use codex_app_server_client::InProcessAppServerClient;
use codex_app_server_protocol::ClientRequest;
use codex_app_server_protocol::ServerNotification;
use codex_app_server_protocol::ThreadReadParams;
use codex_app_server_protocol::ThreadReadResponse;
use codex_app_server_protocol::ThreadTokenUsage;
use codex_app_server_protocol::TokenUsageBreakdown;

use crate::RequestIdSequencer;
use crate::event_processor::EventProcessor;
use crate::exec_events::Usage;
use crate::send_request_with_response;

/// Keeps cumulative reports separate until completion so interleaved streams
/// cannot overwrite each other or count the same request more than once.
#[derive(Default)]
pub(crate) struct JsonUsage {
    reports: HashMap<String, ThreadTokenUsage>,
    baselines: HashMap<String, TokenUsageBreakdown>,
    children: HashMap<String, HashSet<String>>,
    incomplete: bool,
}

impl JsonUsage {
    pub(crate) fn invalidate(&mut self) {
        self.incomplete = true;
    }

    pub(crate) fn observe(&mut self, notification: &ServerNotification) {
        if let ServerNotification::ThreadTokenUsageUpdated(notification) = notification {
            let usage = &notification.token_usage;
            let total = &usage.total;
            // Compaction/context-window estimates are not provider reports.
            // Keep the last measured value, or unknown if there was none.
            if total.input_tokens == 0
                && total.cached_input_tokens == 0
                && total.cache_write_input_tokens == 0
                && total.output_tokens == 0
                && total.reasoning_output_tokens == 0
                && (total.total_tokens != 0 || usage.last.total_tokens != 0)
            {
                return;
            }
            // Forked children can inherit the parent's counters. Their first
            // live report contains one new request in `last`; subtract the
            // inherited prefix, then use cumulative deltas for later reports.
            self.baselines
                .entry(notification.thread_id.clone())
                .or_insert_with(|| TokenUsageBreakdown {
                    input_tokens: usage.total.input_tokens - usage.last.input_tokens,
                    cached_input_tokens: usage.total.cached_input_tokens
                        - usage.last.cached_input_tokens,
                    cache_write_input_tokens: usage.total.cache_write_input_tokens
                        - usage.last.cache_write_input_tokens,
                    output_tokens: usage.total.output_tokens - usage.last.output_tokens,
                    reasoning_output_tokens: usage.total.reasoning_output_tokens
                        - usage.last.reasoning_output_tokens,
                    total_tokens: usage.total.total_tokens - usage.last.total_tokens,
                });
            self.reports.insert(
                notification.thread_id.clone(),
                notification.token_usage.clone(),
            );
        }
    }

    pub(crate) fn register_thread(&mut self, thread_id: &str, parent_thread_id: &str) {
        self.children
            .entry(parent_thread_id.to_string())
            .or_default()
            .insert(thread_id.to_string());
    }

    pub(crate) fn for_thread(&self, thread_id: &str) -> Option<Usage> {
        if self.incomplete {
            return None;
        }
        let root = thread_id;
        let mut pending = vec![thread_id];
        let mut visited = HashSet::new();
        let mut result = Usage::default();
        while let Some(thread_id) = pending.pop() {
            if !visited.insert(thread_id) {
                continue;
            }
            // A missing report from any participant makes the total unknown,
            // rather than silently claiming its work was free.
            let usage = &self.reports.get(thread_id)?.total;
            let zero = TokenUsageBreakdown {
                input_tokens: 0,
                cached_input_tokens: 0,
                cache_write_input_tokens: 0,
                output_tokens: 0,
                reasoning_output_tokens: 0,
                total_tokens: 0,
            };
            let baseline = if thread_id == root {
                &zero
            } else {
                self.baselines.get(thread_id)?
            };
            result.input_tokens = result
                .input_tokens
                .checked_add(usage.input_tokens.checked_sub(baseline.input_tokens)?)?;
            result.cached_input_tokens = result.cached_input_tokens.checked_add(
                usage
                    .cached_input_tokens
                    .checked_sub(baseline.cached_input_tokens)?,
            )?;
            result.cache_write_input_tokens = result.cache_write_input_tokens.checked_add(
                usage
                    .cache_write_input_tokens
                    .checked_sub(baseline.cache_write_input_tokens)?,
            )?;
            result.output_tokens = result
                .output_tokens
                .checked_add(usage.output_tokens.checked_sub(baseline.output_tokens)?)?;
            result.reasoning_output_tokens = result.reasoning_output_tokens.checked_add(
                usage
                    .reasoning_output_tokens
                    .checked_sub(baseline.reasoning_output_tokens)?,
            )?;
            if let Some(children) = self.children.get(thread_id) {
                pending.extend(children.iter().map(String::as_str));
            }
        }
        Some(result)
    }
}

/// Resolve ancestry once per observed thread, including ancestors whose events
/// have not arrived yet. Only metadata is read, never child conversation items.
pub(crate) async fn observe_subagent(
    notification: &ServerNotification,
    selected_thread_id: &str,
    resolved_threads: &mut HashSet<String>,
    client: &InProcessAppServerClient,
    request_ids: &mut RequestIdSequencer,
    processor: &mut dyn EventProcessor,
) {
    let thread_id = match notification {
        ServerNotification::TurnStarted(notification) => &notification.thread_id,
        ServerNotification::TurnCompleted(notification) => &notification.thread_id,
        ServerNotification::ThreadTokenUsageUpdated(notification) => &notification.thread_id,
        _ => return,
    };
    if thread_id == selected_thread_id {
        return;
    }
    let mut ancestor = thread_id.clone();
    while ancestor != selected_thread_id && !resolved_threads.contains(&ancestor) {
        let response = send_request_with_response::<ThreadReadResponse>(
            client,
            ClientRequest::ThreadRead {
                request_id: request_ids.next(),
                params: ThreadReadParams {
                    thread_id: ancestor.clone(),
                    include_turns: false,
                },
            },
            "thread/read",
        )
        .await;
        let response = match response {
            Ok(response) => response,
            Err(error) => {
                tracing::warn!("could not resolve usage thread {ancestor}: {error}");
                processor.invalidate_usage();
                processor.process_warning(format!(
                    "Usage is unknown: could not resolve subagent ancestry: {error}"
                ));
                break;
            }
        };
        resolved_threads.insert(ancestor.clone());
        let Some(parent) = response.thread.parent_thread_id else {
            break;
        };
        processor.register_usage_thread(&ancestor, &parent);
        ancestor = parent;
    }
    processor.observe_unselected_notification(notification, selected_thread_id);
}

#[cfg(test)]
#[path = "json_usage_tests.rs"]
mod tests;
