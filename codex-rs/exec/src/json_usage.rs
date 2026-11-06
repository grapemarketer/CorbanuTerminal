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

const ANCESTRY_READ_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(/*secs*/ 2);

/// Keeps cumulative reports separate until completion so interleaved streams
/// cannot overwrite each other or count the same request more than once.
#[derive(Default)]
pub(crate) struct JsonUsage {
    reports: HashMap<String, ThreadTokenUsage>,
    baselines: HashMap<String, TokenUsageBreakdown>,
    children: HashMap<String, HashSet<String>>,
    forked_threads: HashSet<String>,
    incomplete: bool,
}

impl JsonUsage {
    pub(crate) fn is_valid(&self) -> bool {
        !self.incomplete
    }

    pub(crate) fn mark_forked(&mut self, thread_id: &str) {
        self.forked_threads.insert(thread_id.to_string());
    }

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
            if !self.baselines.contains_key(&notification.thread_id) {
                let Some(baseline) = inherited_prefix(usage) else {
                    self.invalidate();
                    return;
                };
                self.baselines
                    .insert(notification.thread_id.clone(), baseline);
            }
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
            // A fork's first update may just replay its parent's seeded state.
            // Metadata proves it is a fork, but not the inherited counters.
            if thread_id != root && self.forked_threads.contains(thread_id) {
                return None;
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
            result.input_tokens = result.input_tokens.checked_add(nonnegative_difference(
                usage.input_tokens,
                baseline.input_tokens,
            )?)?;
            result.cached_input_tokens =
                result
                    .cached_input_tokens
                    .checked_add(nonnegative_difference(
                        usage.cached_input_tokens,
                        baseline.cached_input_tokens,
                    )?)?;
            result.cache_write_input_tokens =
                result
                    .cache_write_input_tokens
                    .checked_add(nonnegative_difference(
                        usage.cache_write_input_tokens,
                        baseline.cache_write_input_tokens,
                    )?)?;
            result.output_tokens = result.output_tokens.checked_add(nonnegative_difference(
                usage.output_tokens,
                baseline.output_tokens,
            )?)?;
            result.reasoning_output_tokens =
                result
                    .reasoning_output_tokens
                    .checked_add(nonnegative_difference(
                        usage.reasoning_output_tokens,
                        baseline.reasoning_output_tokens,
                    )?)?;
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
    if !processor.usage_is_valid() {
        return;
    }
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
        let response = tokio::time::timeout(
            ANCESTRY_READ_TIMEOUT,
            send_request_with_response::<ThreadReadResponse>(
                client,
                ClientRequest::ThreadRead {
                    request_id: request_ids.next(),
                    params: ThreadReadParams {
                        thread_id: ancestor.clone(),
                        include_turns: false,
                    },
                },
                "thread/read",
            ),
        )
        .await;
        let response = match response {
            Ok(Ok(response)) => Ok(response),
            Ok(Err(error)) => Err(error),
            Err(_) => Err(format!(
                "thread/read timed out after {} seconds",
                ANCESTRY_READ_TIMEOUT.as_secs()
            )),
        };
        let response = match response {
            Ok(response) => response,
            Err(error) => {
                tracing::warn!("could not resolve usage thread {ancestor}: {error}");
                processor.invalidate_usage();
                processor.process_warning(format!(
                    "Usage is unknown: could not resolve subagent ancestry: {error}"
                ));
                return;
            }
        };
        resolved_threads.insert(ancestor.clone());
        if response.thread.forked_from_id.is_some() {
            processor.mark_forked_usage_thread(&ancestor);
        }
        let Some(parent) = response.thread.parent_thread_id else {
            break;
        };
        processor.register_usage_thread(&ancestor, &parent);
        ancestor = parent;
    }
    processor.observe_unselected_notification(notification, selected_thread_id);
}

fn inherited_prefix(usage: &ThreadTokenUsage) -> Option<TokenUsageBreakdown> {
    Some(TokenUsageBreakdown {
        input_tokens: nonnegative_difference(usage.total.input_tokens, usage.last.input_tokens)?,
        cached_input_tokens: nonnegative_difference(
            usage.total.cached_input_tokens,
            usage.last.cached_input_tokens,
        )?,
        cache_write_input_tokens: nonnegative_difference(
            usage.total.cache_write_input_tokens,
            usage.last.cache_write_input_tokens,
        )?,
        output_tokens: nonnegative_difference(usage.total.output_tokens, usage.last.output_tokens)?,
        reasoning_output_tokens: nonnegative_difference(
            usage.total.reasoning_output_tokens,
            usage.last.reasoning_output_tokens,
        )?,
        total_tokens: nonnegative_difference(usage.total.total_tokens, usage.last.total_tokens)?,
    })
}

fn nonnegative_difference(value: i64, baseline: i64) -> Option<i64> {
    value
        .checked_sub(baseline)
        .filter(|difference| *difference >= 0)
}

#[cfg(test)]
#[path = "json_usage_tests.rs"]
mod tests;
