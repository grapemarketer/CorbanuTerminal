use std::collections::VecDeque;

use codex_app_server_protocol::ThreadTokenUsage;
use codex_app_server_protocol::TokenUsageBreakdown;

use super::BridgeRuntime;
use crate::conversation::ConversationKey;

impl BridgeRuntime {
    pub(super) async fn runtime_status_text(&self, conversation: ConversationKey) -> String {
        let approvals = self.sessions.pending_approval_count(conversation).await;
        let active_turn = self.sessions.turn_id(conversation).await;
        let queued = self
            .pending_inputs
            .get(&conversation)
            .map_or(0, VecDeque::len);
        let last_error = self.last_errors.get(&conversation);
        let state = runtime_state(
            approvals,
            active_turn.is_some(),
            queued,
            last_error.is_some(),
        );
        let (model, provider) = self.active_model_settings(conversation).await;
        let mut text = format!(
            "State: {}\nModel: {} via {provider}\nWorkspace: {}\nConversation: {}\nQueued messages: {queued}",
            state.label(),
            model.as_deref().unwrap_or("server default"),
            self.config.cwd.display(),
            conversation.display_label()
        );
        if let Some(usage) = self.last_token_usage.get(&conversation) {
            text.push('\n');
            text.push_str(&token_usage_text(usage));
        }
        if let Some(last_contact) = self.last_successful_contact_at.get(&conversation) {
            text.push_str(&format!(
                "\nLast successful Telegram contact: unix {last_contact}"
            ));
        }
        if let Some(last_error) = last_error {
            text.push_str(&format!("\nLast error: {last_error}"));
        }
        text.push_str("\nNext: ");
        text.push_str(state.next_action());
        text
    }
}

/// What `/status` knows about a thread's token usage.
#[derive(Debug, Clone, PartialEq)]
pub(super) struct StatusTokenUsage {
    /// The thread these numbers belong to. A total is only ever carried forward
    /// within the same thread, never into a new one (`/new`, a resumed thread).
    thread_id: String,
    /// Tokens in the context window: the last report, core's estimate, or the
    /// window itself when core filled it.
    context_tokens: i64,
    model_context_window: Option<i64>,
    /// The last update exactly as core sent it: core re-sends unchanged usage
    /// (`update_rate_limits`), and a fill adds exactly `last` to the previous
    /// `total.total_tokens` while a recompute leaves it untouched.
    update: ThreadTokenUsage,
    /// The thread's provider-reported total; `None` until a provider reports.
    thread_total: Option<TokenUsageBreakdown>,
}

impl StatusTokenUsage {
    /// Folds a thread usage update into what `/status` shows.
    ///
    /// Besides provider reports, core sends two synthetic updates (#1570):
    /// - `recompute_token_usage` after compaction or rollback leaves `total`
    ///   untouched and puts the history estimate in `last.total_tokens` with zero
    ///   counters. On a thread with no reports that means zero counters
    ///   everywhere; on a reported thread the real counters stay in `total`.
    /// - `fill_to_context_window` when the window is exceeded sets
    ///   `total.total_tokens` to the window and keeps the provider counters
    ///   (#1583); `last` gets zero counters and the remainder.
    ///
    /// `Context` follows the latest update: the window only for a fill (the total
    /// set to the window and `last` holding exactly what that added), otherwise
    /// `last` (a report, or the estimate after compaction, which is what core
    /// measures occupancy from).
    /// The thread total comes from the provider counters, never from
    /// `total.total_tokens`, which after a fill holds the window plus later
    /// reports. Without counters it stays at the last real report of the same
    /// thread, or unknown.
    pub(super) fn apply(previous: Option<Self>, thread_id: &str, update: ThreadTokenUsage) -> Self {
        let previous = previous.filter(|usage| usage.thread_id == thread_id);
        if let Some(usage) = previous.as_ref().filter(|usage| usage.update == update) {
            return usage.clone(); // re-sent unchanged (e.g. with new rate limits): nothing moved
        }
        // `fill_to_context_window` sets the total to the window and `last` to exactly what
        // it added (the window minus the previous total, 0 when the window was already
        // full). A recompute leaves the total untouched and puts its estimate in `last`,
        // so the two only look alike without the previous update (for example a resume
        // replay after a bridge restart). A recompute estimates 0 only for empty
        // instructions and history, so a zero `last` is still taken as a fill; otherwise
        // `last` is shown, which is also what core measures occupancy from (and the whole
        // window for a fill on a thread that had no total yet).
        let filled = counters_are_zero(&update.last)
            && Some(update.total.total_tokens) == update.model_context_window
            && match previous.as_ref() {
                // the same arithmetic as `fill_to_context_window`
                Some(usage) => {
                    (update.total.total_tokens - usage.update.total.total_tokens).max(0)
                        == update.last.total_tokens
                }
                None => update.last.total_tokens == 0,
            };
        let context_tokens = if filled {
            update.total.total_tokens
        } else {
            update.last.total_tokens
        };
        let thread_total = if !counters_are_zero(&update.total) {
            Some(TokenUsageBreakdown {
                total_tokens: update
                    .total
                    .input_tokens
                    .max(0)
                    .saturating_add(update.total.output_tokens.max(0)),
                ..update.total
            })
        } else if reports_usage(&update) {
            Some(update.total.clone())
        } else {
            previous.and_then(|usage| usage.thread_total)
        };
        Self {
            thread_id: thread_id.to_string(),
            context_tokens,
            model_context_window: update.model_context_window,
            thread_total,
            update,
        }
    }
}

fn counters_are_zero(usage: &TokenUsageBreakdown) -> bool {
    [
        usage.input_tokens,
        usage.cached_input_tokens,
        usage.cache_write_input_tokens,
        usage.output_tokens,
        usage.reasoning_output_tokens,
    ]
    .iter()
    .all(|count| *count == 0)
}

/// Same rule as exec's JSON output (#1576): an update whose total counters are
/// all zero but which carries a token total is an estimate, not a provider
/// report. Two corner cases follow from it, as in exec: a provider's measured
/// zero after a fill (the total then holds the window) cannot be told from an
/// estimate and is not shown as a report, and a provider that sends only
/// `total_tokens` without counters is treated as reporting nothing.
fn reports_usage(usage: &ThreadTokenUsage) -> bool {
    !counters_are_zero(&usage.total)
        || (usage.total.total_tokens == 0 && usage.last.total_tokens == 0)
}

/// Renders the two numbers a Telegram operator cannot otherwise see.
///
/// `Context` is the live window occupancy, which auto-compaction keeps bounded.
/// `Thread total` is the number that is not bounded by anything: a Telegram chat
/// resumes the same thread until someone runs `/new`, so this accumulates for
/// the life of the thread and is what a subscription is actually spent on.
fn token_usage_text(usage: &StatusTokenUsage) -> String {
    let context = usage.context_tokens.max(0);
    let mut text = match usage.model_context_window {
        Some(window) if window > 0 => format!(
            "Context: {} of {} tokens ({}%)",
            thousands(context),
            thousands(window),
            (context.saturating_mul(100) / window).min(100)
        ),
        _ => format!("Context: {} tokens", thousands(context)),
    };
    match &usage.thread_total {
        Some(total) => text.push_str(&format!(
            "\nThread total: {} tokens ({} in, {} out)",
            thousands(total.total_tokens.max(0)),
            thousands(total.input_tokens.max(0)),
            thousands(total.output_tokens.max(0)),
        )),
        None => text.push_str("\nThread total: not reported by the provider"),
    }
    text.push_str("\nReset with /new when a thread no longer needs its history.");
    text
}

/// Group digits so a seven-figure total is legible at a glance in a chat client.
fn thousands(value: i64) -> String {
    let digits = value.to_string();
    let mut grouped = String::with_capacity(digits.len() + digits.len() / 3);
    for (index, ch) in digits.chars().enumerate() {
        if index > 0 && (digits.len() - index).is_multiple_of(3) {
            grouped.push(',');
        }
        grouped.push(ch);
    }
    grouped
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RuntimeState {
    Idle,
    Working,
    WorkingQueued,
    AwaitingApproval,
    Recovering,
    Blocked,
}

impl RuntimeState {
    fn label(self) -> &'static str {
        match self {
            Self::Idle => "Idle",
            Self::Working => "Working",
            Self::WorkingQueued => "Working · follow-ups queued",
            Self::AwaitingApproval => "Awaiting approval",
            Self::Recovering => "Recovering queued input",
            Self::Blocked => "Blocked",
        }
    }

    fn next_action(self) -> &'static str {
        match self {
            Self::AwaitingApproval => "use the approval buttons or /cancel.",
            Self::Working | Self::WorkingQueued => "send a follow-up to steer, or /cancel.",
            Self::Recovering => "wait for automatic recovery, or /cancel.",
            Self::Blocked => "review the last error, then retry the task or use /new.",
            Self::Idle => "send a task, or /new for a fresh thread.",
        }
    }
}

fn runtime_state(
    approvals: usize,
    active_turn: bool,
    queued: usize,
    has_error: bool,
) -> RuntimeState {
    if approvals > 0 {
        RuntimeState::AwaitingApproval
    } else if active_turn && queued > 0 {
        RuntimeState::WorkingQueued
    } else if active_turn {
        RuntimeState::Working
    } else if has_error {
        RuntimeState::Blocked
    } else if queued > 0 {
        RuntimeState::Recovering
    } else {
        RuntimeState::Idle
    }
}

#[cfg(test)]
#[path = "status_tests.rs"]
mod tests;
