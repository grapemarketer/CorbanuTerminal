//! What a `/cost` view covers, what lies outside it, and what to do when a
//! request has no price.
//!
//! The view explains one conversation: its root and resolved subagents. The
//! same UTC day can hold requests from other conversations, and a request can
//! record tokens that no published price covers. Neither may read as zero: the
//! first is stated beside this conversation's figures with its own total, the
//! second with the step that finds the real amount.

use codex_state::accounting::BucketQuote;
use codex_state::accounting::DayTotals;
use codex_state::accounting::DeletedAttempts;
use codex_state::accounting::ObservationQuote;
use codex_state::accounting::OtherConversations;

use super::EstimateGaps;
use super::OVERFLOW_NOTE;
use super::by_route;
use super::has_no_price;
use super::lacks_price;
use super::lower_first;
use super::per_use;
use super::plain_billing;
use super::provider_name;
use super::request_count;
use super::route_line;

/// #913 (D31): the OpenAI route's automatic reviewer has no published API
/// price, so its requests stay unpriced; the page names the model so the
/// gap is not read as a missing catalogue row.
const AUTO_REVIEW_NO_PRICE: &str = "OpenAI doesn't publish a price for Codex Auto Review.";

const RESUME_STEP: &str =
    "To see those requests, open that conversation with /resume and run /cost there.";
/// #912: a conversation holding only a reopen warm-up has no saved turns to resume.
const RESUME_STEP_SAVED_TURNS: &str =
    "For conversations with saved turns, open them with /resume and run /cost there.";

fn counted(n: usize, noun: &str) -> String {
    format!("{n} {noun}{}", if n == 1 { "" } else { "s" })
}

/// Same-day requests of other conversations: never part of this conversation's
/// total, and never left out silently. `None` means the view did not read them.
pub(super) fn other_conversations_lines(
    others: Option<&OtherConversations>,
    named: bool,
) -> Vec<String> {
    let Some(others) = others else {
        return vec![
            "Other conversations are not included in this view; /cost covers only the open conversation.".into(),
        ];
    };
    let deleted = deleted_line(others.deleted_attempts);
    if others.conversations == 0 {
        // Only a day with nothing deleted on it is known to be empty.
        return match deleted {
            None => vec!["No other conversation recorded requests on this day.".into()],
            Some(deleted) => vec![
                "No other saved conversation has recorded requests on this day.".into(),
                deleted,
            ],
        };
    }
    let quotes: Vec<&ObservationQuote> = others.requests.values().flatten().collect();
    let conversations = counted(others.conversations, "conversation");
    let mut lines = vec![if quotes.is_empty() {
        format!("Other conversations on this day, not included above: {conversations}.")
    } else {
        format!(
            "Other conversations on this day, not included above: {} in {conversations}.",
            request_count(&quotes)
        )
    }];
    lines.extend(
        by_route(quotes.iter().copied())
            .values()
            .map(|q| route_line(q, named)),
    );
    if !quotes.is_empty() {
        lines.push(match DayTotals::from_quotes(quotes.iter().copied()) {
            Ok(totals) => format!(
                "Other conversations' {}.",
                lower_first(&plain_billing(&totals, EstimateGaps::of(quotes.iter().copied())).1)
            ),
            Err(_) => "Other conversations' cost: unavailable — exact arithmetic overflow.".into(),
        });
    }
    let warmup_note = super::warmup_resume::note(&quotes);
    if warmup_note.is_some() {
        // #1000: name the warm-ups the note below refers to, beside the
        // requests they are part of.
        lines.push(match super::reopen_warmups(quotes.iter().copied()).1 {
            1 => "Of these, 1 request is a reopen warm-up.".into(),
            n => format!("Of these, {n} requests are reopen warm-ups."),
        });
    }
    if others.unavailable > 0 {
        lines.push(format!(
            "{} could not be read in full; their cost is unknown and not included.",
            counted(others.unavailable, "conversation")
        ));
    }
    if let Some(note) = warmup_note {
        lines.push(RESUME_STEP_SAVED_TURNS.into());
        lines.push(note);
    } else {
        lines.push(RESUME_STEP.into());
    }
    // After the resume step: deleted conversations cannot be resumed.
    lines.extend(deleted);
    lines
}

/// Deleted conversations' spend on this day. Deletion removes their recorded
/// requests and cost, but not what the provider charged for them; say so
/// rather than let the day look emptier or cheaper than it was. `None` means
/// none were deleted. A deleted subagent of this conversation counts here too:
/// its replay fence names no conversation.
fn deleted_line(deleted_attempts: DeletedAttempts) -> Option<String> {
    match deleted_attempts {
        DeletedAttempts::Counted(0) => None,
        DeletedAttempts::Counted(n) => Some(format!(
            "Deleted conversations or subagents sent {} on this day. Their recorded cost was deleted with them, so it is not included here; any cost they incurred is on your provider's bill.",
            counted(n, "request attempt")
        )),
        DeletedAttempts::PastDetailWindow => Some(
            "Days older than 90 days keep too little detail to count requests from deleted conversations; any they made are not included here.".into(),
        ),
        DeletedAttempts::Unread => Some(
            "Requests from deleted conversations could not be counted for this day; any they made are not included here.".into(),
        ),
    }
}

/// What to do about pay-per-use requests with no complete estimate: those no
/// published price covers, those priced only in part, and those with a price
/// whose provider did not report every token count. Each amount is on the
/// provider's own bill, not zero. Requests that recorded only zero counts cost
/// nothing and get no step.
pub(super) fn no_price_next_step<'a>(
    quotes: impl IntoIterator<Item = &'a ObservationQuote>,
) -> Vec<String> {
    let mut no_price: Vec<String> = Vec::new();
    let mut in_part: Vec<String> = Vec::new();
    let mut incomplete: Vec<String> = Vec::new();
    let mut undeclared: Vec<String> = Vec::new();
    let mut auto_review = false;
    for quote in quotes {
        if quote.basis() == codex_state::accounting::Basis::Undeclared {
            if !undeclared.contains(&quote.attempt.provider) {
                undeclared.push(quote.attempt.provider.clone());
            }
            continue;
        }
        if !per_use(quote) || quote.all_buckets_priced.is_some() {
            continue;
        }
        let usage = &quote.usage;
        let priced_any = [usage.noncached, usage.read, usage.write, usage.output]
            .into_iter()
            .zip(quote.buckets)
            .any(|(count, bucket)| {
                count.is_some_and(|count| count > 0) && matches!(bucket, BucketQuote::Priced(_))
            });
        // Only decides requests with no price at all: with a price, a missing
        // rate already makes the request no-price or priced in part. Without
        // it, a request that reported nothing (all counts missing) would get
        // no step.
        let unreported = quote
            .buckets
            .iter()
            .any(|bucket| matches!(bucket, BucketQuote::MissingUsage));
        let providers = if !lacks_price(quote) {
            &mut incomplete
        } else if priced_any {
            &mut in_part
        } else if has_no_price(quote) || unreported {
            auto_review |= quote.attempt.provider == codex_model_provider_info::OPENAI_PROVIDER_ID
                && quote.attempt.model
                    == codex_model_provider::DEFAULT_APPROVAL_REVIEW_PREFERRED_MODEL;
            &mut no_price
        } else {
            // Only zero counts, which cost nothing whatever the rate.
            continue;
        };
        let provider = provider_name(&quote.attempt.provider);
        if !providers.contains(&provider) {
            providers.push(provider);
        }
    }
    let joined = |providers: &[String]| match providers {
        [] => None,
        [one] => Some(one.clone()),
        [rest @ .., last] => Some(format!("{} and {last}", rest.join(", "))),
    };
    let mut steps: Vec<String> = undeclared
        .iter()
        .map(|id| {
            format!(
                "Billing basis not declared for {}. Next step: {}",
                provider_name(id),
                codex_model_provider_info::not_declared_next_step(id)
            )
        })
        .collect();
    if let Some(providers) = joined(&no_price) {
        let model = if auto_review {
            format!(" {AUTO_REVIEW_NO_PRICE}")
        } else {
            String::new()
        };
        steps.push(format!(
            "Next step for requests with no price: check the bill from {providers}. No published price covers them, so no cost is shown for them here.{model}"
        ));
    }
    if let Some(providers) = joined(&in_part) {
        steps.push(format!(
            "Next step for requests priced only in part: check the bill from {providers}. Some of their tokens have no published price, so the estimate is only a lower bound."
        ));
    }
    if let Some(providers) = joined(&incomplete) {
        steps.push(format!(
            "Next step for requests with incomplete usage: check the bill from {providers}. Their provider did not report every token count, so the estimate is missing or only a lower bound."
        ));
    }
    steps
}

/// The overflow note, when any of `quotes` is subscription work (option B):
/// paid usage beyond a plan is never counted, and this view cannot see it.
pub(super) fn overflow_note<'a>(
    quotes: impl IntoIterator<Item = &'a ObservationQuote>,
) -> Option<String> {
    quotes
        .into_iter()
        .any(ObservationQuote::is_plan)
        .then(|| OVERFLOW_NOTE.to_string())
}

#[cfg(test)]
#[path = "scope_tests.rs"]
mod tests;
