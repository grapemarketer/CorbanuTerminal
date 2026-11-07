//! Coordinates asynchronous `/usage` cards in the chat widget.
//!
//! The slash command builds a composite history cell immediately, but the widget
//! keeps that cell transient while the account request runs. The transient card is
//! rendered above the composer through [`ChatWidget::pending_token_activity_output`]
//! so loading never requires clearing or rewriting transcript history. When the
//! matching response arrives, [`TokenActivityHandle`] updates the shared card state
//! and [`ChatWidget::finish_token_activity_refresh`] moves the cell into a completed
//! slot. Event dispatch commits that completed cell into history only after active
//! output and stream consolidation no longer block insertion.
//!
//! Pure chart rendering and date bucketing live in [`chart`], and the `/cost`
//! range view in [`range`]. This module owns
//! request correlation, transient/completed card state, and integration with
//! `ChatWidget` history insertion.

mod charge;
mod chart;
mod empty_day;
mod range;
mod scope;
mod warmup_resume;

use std::sync::Arc;
use std::sync::RwLock;

use chrono::NaiveDate;
use chrono::Utc;
use codex_app_server_protocol::GetAccountTokenUsageResponse;
use ratatui::style::Stylize;
use ratatui::text::Line;

use super::ChatWidget;
use crate::app_event::AppEvent;
use crate::history_cell::CompositeHistoryCell;
use crate::history_cell::HistoryCell;
use crate::history_cell::PlainHistoryCell;
use crate::history_cell::plain_lines;

pub(crate) use chart::TokenActivityView;

/// Tracks the renderable lifecycle of one token activity history cell.
#[derive(Debug)]
enum TokenActivityState {
    Loading,
    Loaded {
        response: GetAccountTokenUsageResponse,
        today: NaiveDate,
    },
    Error,
}

/// Completes an asynchronously rendered token activity history cell.
///
/// Clones share the same card state, allowing the background request path to
/// update a cell still owned by the widget's transient-output state. The widget
/// remains responsible for request-ID matching, redraws, and history insertion.
#[derive(Clone, Debug)]
pub(super) struct TokenActivityHandle {
    state: Arc<RwLock<TokenActivityState>>,
}

/// Holds the one transient token activity card waiting on its background response.
///
/// The request ID prevents late results from mutating a newer `/usage` card. The
/// cell stays out of transcript history until the matching response completes and
/// the widget confirms that active output no longer blocks insertion.
pub(super) struct PendingTokenActivityOutput {
    request_id: u64,
    cell: CompositeHistoryCell,
    handle: TokenActivityHandle,
}

impl TokenActivityHandle {
    /// Replaces the loading state with either fetched activity or an unavailable state.
    ///
    /// This method intentionally discards the error string because the TUI exposes
    /// one stable unavailable message. Calling it more than once replaces the prior
    /// terminal state, so request-ID matching should happen before completion.
    pub(super) fn finish(&self, result: Result<GetAccountTokenUsageResponse, String>) {
        self.finish_with_today(result, Utc::now().date_naive());
    }

    fn finish_with_today(
        &self,
        result: Result<GetAccountTokenUsageResponse, String>,
        today: NaiveDate,
    ) {
        let state = match result {
            Ok(response) => TokenActivityState::Loaded { response, today },
            Err(_) => TokenActivityState::Error,
        };
        #[expect(clippy::expect_used)]
        let mut current = self.state.write().expect("token activity state poisoned");
        *current = state;
    }
}

/// Renders one `/usage` card from shared asynchronous state.
#[derive(Debug)]
struct TokenActivityHistoryCell {
    view: TokenActivityView,
    state: Arc<RwLock<TokenActivityState>>,
}

/// Creates the card contents and completion handle for one `/usage` invocation.
///
/// The composite cell includes the echoed slash command and a loading card from
/// the start. Callers must retain the returned handle and complete it when the
/// matching background response arrives; otherwise the transient card stays loading.
pub(super) fn new_token_activity_output(
    view: TokenActivityView,
) -> (CompositeHistoryCell, TokenActivityHandle) {
    let command = PlainHistoryCell::new(vec![
        format!("/usage {}", view.label().to_lowercase())
            .magenta()
            .into(),
    ]);
    let state = Arc::new(RwLock::new(TokenActivityState::Loading));
    let handle = TokenActivityHandle {
        state: Arc::clone(&state),
    };
    let card = TokenActivityHistoryCell { view, state };
    (
        CompositeHistoryCell::new(vec![Box::new(command), Box::new(card)]),
        handle,
    )
}

impl HistoryCell for TokenActivityHistoryCell {
    fn display_lines(&self, width: u16) -> Vec<Line<'static>> {
        #[expect(clippy::expect_used)]
        let state = self.state.read().expect("token activity state poisoned");
        match &*state {
            TokenActivityState::Loading => {
                vec![
                    " Token activity".bold().into(),
                    "   Loading...".dim().into(),
                ]
            }
            TokenActivityState::Error => vec![
                " Token activity".bold().into(),
                "   Token activity unavailable".dim().into(),
            ],
            TokenActivityState::Loaded { response, today } => {
                chart::loaded_lines(self.view, response, *today, width)
            }
        }
    }

    fn raw_lines(&self) -> Vec<Line<'static>> {
        plain_lines(self.display_lines(u16::MAX))
    }
}

impl ChatWidget {
    /// Starts a token activity refresh and replaces the current transient card.
    ///
    /// Each invocation receives a request ID so background responses update only
    /// their own card. The card remains outside transcript history until completion,
    /// which keeps loading visible without disturbing existing transcript content.
    pub(crate) fn add_token_activity_output(&mut self, view: TokenActivityView) {
        let request_id = self.next_token_activity_request_id;
        self.next_token_activity_request_id =
            self.next_token_activity_request_id.wrapping_add(/*rhs*/ 1);
        let (cell, handle) = new_token_activity_output(view);
        self.completed_token_activity_output = None;
        self.refreshing_token_activity_output = Some(PendingTokenActivityOutput {
            request_id,
            cell,
            handle,
        });
        self.bump_active_cell_revision();
        self.request_redraw();
        self.app_event_tx
            .send(AppEvent::RefreshTokenActivity { request_id });
    }

    /// Returns the transient token activity card that should render above the composer.
    ///
    /// A loading card takes precedence over a completed card waiting for history
    /// insertion. Callers should render the returned cell but leave ownership with
    /// the widget so completion and insertion can update it safely.
    pub(super) fn pending_token_activity_output(&self) -> Option<&dyn HistoryCell> {
        self.refreshing_token_activity_output
            .as_ref()
            .map(|output| &output.cell as &dyn HistoryCell)
            .or_else(|| {
                self.completed_token_activity_output
                    .as_ref()
                    .map(|cell| cell as &dyn HistoryCell)
            })
    }

    /// Applies a background token activity result to its matching transient card.
    ///
    /// Returns `true` when the pending request matched and moved into the completed
    /// slot. Late responses return `false`, including responses for cards replaced
    /// by a newer `/usage` invocation or cleared during transcript changes.
    pub(crate) fn finish_token_activity_refresh(
        &mut self,
        request_id: u64,
        result: Result<GetAccountTokenUsageResponse, String>,
    ) -> bool {
        let Some(output) = self.refreshing_token_activity_output.take() else {
            return false;
        };
        if output.request_id != request_id {
            self.refreshing_token_activity_output = Some(output);
            return false;
        }
        output.handle.finish(result);
        self.completed_token_activity_output = Some(output.cell);
        self.bump_active_cell_revision();
        self.request_redraw();
        true
    }

    /// Reports whether completed asynchronous usage output must wait before insertion.
    ///
    /// Inserting while a stream, queued consolidation, or active transcript cell is
    /// present can reorder output relative to visible work, so callers retry once
    /// these barriers clear.
    pub(crate) fn usage_history_insertion_blocked(&self) -> bool {
        self.stream_controller.is_some()
            || self.plan_stream_controller.is_some()
            || self.pending_stream_consolidations > 0
            || self.transcript.active_cell.is_some()
            || self.active_hook_cell.is_some()
    }

    /// Records a stream consolidation barrier that delays token card insertion.
    ///
    /// Each queued consolidation should eventually call
    /// [`ChatWidget::note_stream_consolidation_completed`].
    pub(crate) fn note_stream_consolidation_queued(&mut self) {
        self.pending_stream_consolidations =
            self.pending_stream_consolidations.saturating_add(/*rhs*/ 1);
    }

    /// Releases one queued stream consolidation barrier.
    ///
    /// The counter saturates at zero so an unmatched completion does not underflow,
    /// but paired queue/completion calls are still the intended contract.
    pub(crate) fn note_stream_consolidation_completed(&mut self) {
        self.pending_stream_consolidations =
            self.pending_stream_consolidations.saturating_sub(/*rhs*/ 1);
    }

    /// Transfers the completed token activity card into the history insertion path.
    ///
    /// Callers should use this only after
    /// [`ChatWidget::usage_history_insertion_blocked`] returns `false`;
    /// taking the card removes it from the transient render area.
    pub(crate) fn take_completed_token_activity_output(&mut self) -> Option<CompositeHistoryCell> {
        let output = self.completed_token_activity_output.take()?;
        self.bump_active_cell_revision();
        Some(output)
    }

    /// Requests another insertion attempt when completed usage output is waiting.
    ///
    /// This is used after stream or history lifecycle events that may have cleared
    /// the insertion barriers without directly owning the completed output.
    pub(crate) fn request_pending_usage_output_insertion(&self) {
        if self.completed_token_activity_output.is_some()
            || self.pending_rate_limit_reset_hint().is_some()
        {
            self.app_event_tx.send(AppEvent::CommitPendingUsageOutput);
        }
    }

    pub(crate) fn request_pending_usage_output_insertion_after_stream_shutdown(&self) {
        if self.completed_token_activity_output.is_some()
            || self.pending_rate_limit_reset_hint().is_some()
        {
            self.app_event_tx
                .send(AppEvent::CommitPendingUsageOutputAfterStreamShutdown);
        }
    }

    /// Drops transient and completed token cards that must no longer update.
    ///
    /// Late background responses cannot mutate cards after a transcript reset,
    /// backtrack, or replacement flow clears this widget-owned state.
    pub(crate) fn clear_pending_token_activity_refreshes(&mut self) {
        self.accounting_inspector = None;
        self.bottom_pane.dismiss_view_by_id(INSPECTOR_VIEW);
        let cleared_refresh = self.refreshing_token_activity_output.take().is_some();
        let cleared_completed = self.completed_token_activity_output.take().is_some();
        if cleared_refresh || cleared_completed {
            self.bump_active_cell_revision();
            self.request_redraw();
        }
    }
}

// Inspector pages contain local evidence only; never insert them into history.
use crate::bottom_pane::SelectionItem;
use crate::bottom_pane::SelectionViewParams;
use codex_protocol::ThreadId;
use codex_state::accounting::BucketQuote;
use codex_state::accounting::Decimal;
use codex_state::accounting::InspectionDay;
use codex_state::accounting::InspectionGrouping;
use codex_state::accounting::InspectionRange;
use codex_state::accounting::ObservationQuote;
#[cfg(test)]
use range::NOT_CURRENT;
#[cfg(test)]
use range::NOT_STARTED;
use range::range_pages;
use uuid::Uuid;

const INSPECTOR_VIEW: &str = "recorded-requests";

pub(super) struct Inspector {
    generation: Uuid,
    thread: Option<ThreadId>,
    day: i64,
    range: Option<InspectionRange>,
    pages: Vec<InspectorPage>,
    page: usize,
    /// Pages opened before the current one, so Back and Esc return the way
    /// the user came rather than to a page's fixed parent.
    history: Vec<usize>,
    alive: Arc<std::sync::atomic::AtomicBool>,
}

struct InspectorPage {
    title: String,
    text: Vec<String>,
    links: Vec<(String, usize)>,
    parent: Option<usize>,
    selected: Arc<std::sync::atomic::AtomicUsize>,
}

fn money(value: Decimal) -> String {
    let display = value.display();
    format!(
        "${}{}{}",
        display.text,
        if display.rounded { " (rounded)" } else { "" },
        if display.nonzero_sub_micro {
            " (nonzero, less than $0.000001)"
        } else {
            ""
        }
    )
}

fn exact(value: Decimal) -> String {
    serde_json::to_value(value)
        .ok()
        .and_then(|v| v.as_str().map(str::to_owned))
        .unwrap_or_else(|| "unavailable".to_string())
}

/// The provider and model of every attempt that recorded tokens and no price
/// for any of them.
///
/// A blank money figure leaves an operator unable to tell a collector that
/// recorded nothing from a request whose tokens were recorded with no rate to
/// price them, and those are different problems. This names the second kind,
/// and only that kind: a row appears when at least one bucket carries usage
/// with no rate (`MissingRate`) and none could be priced. An attempt that
/// recorded no usage at all is a different gap and is counted elsewhere as an
/// incomplete attempt, not named here.
///
/// What it must not do is say WHY there was no rate. The ledger does not know:
/// a missing price can mean the catalogue states no rate for the row, or that
/// the route's credential could not be attributed to the account a rate is
/// quoted for, or that the turn ran on a service tier the rates are not quoted
/// for. Naming the rows is actionable; guessing the cause would send the
/// operator to fix the wrong thing.
fn unpriced_rows<'a>(
    quotes: impl IntoIterator<Item = &'a ObservationQuote>,
    named: bool,
) -> Vec<String> {
    let mut rows: Vec<(String, u64)> = Vec::new();
    for quote in quotes.into_iter().filter(|quote| has_no_price(quote)) {
        let attributed = |value: &str| {
            if value.trim().is_empty() {
                "unknown (attribution absent)".to_string()
            } else {
                value.to_string()
            }
        };
        let row = format!(
            "{} ({}/{})",
            route_name_in(quote, named),
            attributed(&quote.attempt.provider),
            attributed(&quote.attempt.model)
        );
        match rows.iter_mut().find(|(named, _)| *named == row) {
            Some((_, attempts)) => *attempts += 1,
            None => rows.push((row, 1)),
        }
    }
    rows.sort();
    if rows.is_empty() {
        Vec::new()
    } else {
        vec![format!(
            "Attempts with no price for any recorded tokens: {}. No money is stated for those attempts, which is not a zero-cost claim, and says nothing about the other attempts on the same model. A price can be absent because the catalogue states no rate for the row, because this route's credential or service tier is not one the quoted rates apply to, or because no rate was in effect at dispatch.",
            rows.iter()
                .map(|(row, attempts)| format!("{attempts} on {row}"))
                .collect::<Vec<_>>()
                .join(", ")
        )]
    }
}

/// An attempt that recorded tokens and has no price for any of them.
fn has_no_price(quote: &ObservationQuote) -> bool {
    // Subscription work is not billed per token, so it has no money to be
    // missing. Its own gap - an API equivalent with no rate or no complete
    // count - is already stated beside the plan rate that applied, and naming
    // it here would assert unstated money next to a line saying the turn was
    // never billed that way.
    if !per_use(quote) {
        return false;
    }
    // Priced from the charge its provider reported, published price or not.
    if charge::reported_charge(quote).is_some() {
        return false;
    }
    let usage = &quote.usage;
    // Only tokens that were actually recorded and are not zero count here.
    // Zero tokens cost nothing whatever the rate, and a bucket priced at zero
    // because its count was zero says nothing about whether a rate exists -
    // which is why this reads the counts rather than the bucket variants
    // alone, and why money is not the test either: a bucket priced by a real
    // rate of zero states a price, and states no money.
    let recorded = [usage.noncached, usage.read, usage.write, usage.output]
        .into_iter()
        .zip(quote.buckets)
        .filter(|(count, _)| count.is_some_and(|count| count > 0));
    let mut priced = false;
    let mut unpriced = false;
    for (_, bucket) in recorded {
        match bucket {
            BucketQuote::Priced(_) => priced = true,
            BucketQuote::MissingRate => unpriced = true,
            BucketQuote::MissingUsage => {}
        }
    }
    unpriced && !priced
}

/// `gaps` must come from the same attempts as `totals`.
fn estimate(totals: &codex_state::accounting::DayTotals, gaps: EstimateGaps) -> Vec<String> {
    if totals.attempts == 0 {
        return vec!["No recorded attempts; collection coverage unknown.".into()];
    }
    // Count the per-token-billed population on its own. A plan attempt has no
    // billed price by construction - `all_buckets_priced` is `Some` only when
    // the attempt is complete and not plan work - so every plan attempt adds
    // one to `unknown_estimates`. Reporting that against every attempt made a
    // day of pure subscription work read as "2 of 2 attempts incomplete"
    // directly above the lines stating that day's consumption exactly. Plan,
    // local and undeclared attempts are each subtracted exactly rather than
    // defensively, and compacted days carry every count.
    let known = totals.known_usd;
    let attempts = totals.per_use_attempts();
    let unknown = totals.per_use_unknown();
    // Nothing in these totals was billed per token. Saying the cost is
    // "unknown" here was false and read as a failure: the plan lines below state
    // it exactly, and there is no per-token spend that went missing. The same
    // totals back a whole day and a single attempt's page, so the sentence speaks
    // only for the attempts it was computed from, never for the day around them.
    // Undeclared work is not known to be unbilled, so it is never described as
    // not billed per token.
    if attempts == 0 {
        let mut lines = vec![if totals.undeclared_attempts > 0 {
            UNDECLARED_COST.to_string()
        } else {
            "No recorded attempt here was billed per token.".to_string()
        }];
        lines.extend(plan(totals));
        lines.extend(other_bases(totals));
        return lines;
    }
    let incomplete = format!(
        "Full recorded estimate: unavailable ({unknown} of {attempts} billed attempts incomplete)"
    );
    // A known part of zero is not a figure worth stating beside an unknown
    // estimate: "$0.000000 + unknown" reads as a zero-cost claim.
    let charges = gaps.charges.text();
    let mut lines = if let Some(split) = charges {
        // #368: reported charges are costs, not estimates.
        if unknown == 0 && gaps.charges.all_charged() {
            vec![format!(
                "Token cost for recorded attempts: {}, {split}",
                money(known)
            )]
        } else if unknown == 0 {
            vec![format!(
                "Token cost for recorded attempts: {} ({split})",
                money(known)
            )]
        } else {
            vec![
                format!(
                    "Known token cost: {} + unknown costs ({split})",
                    money(known)
                ),
                incomplete,
            ]
        }
    } else if unknown == 0 {
        vec![format!(
            "Estimated token cost for recorded attempts: {}",
            money(known)
        )]
    } else if known == Decimal::default() {
        vec!["Estimated token cost: unknown".into(), incomplete]
    } else {
        vec![
            format!(
                "Known estimated token cost: {} + unknown costs",
                money(known)
            ),
            incomplete,
        ]
    };
    lines.extend(plan(totals));
    lines.extend(other_bases(totals));
    lines
}

/// Work that is neither billed per token nor subscription work.
fn other_bases(totals: &codex_state::accounting::DayTotals) -> Vec<String> {
    let mut lines = Vec::new();
    if totals.local_attempts > 0 {
        lines.push(format!(
            "Local work: {} of {} attempts ran on your own machine, no charge",
            totals.local_attempts, totals.attempts
        ));
    }
    if totals.undeclared_attempts > 0 {
        lines.push(format!(
            "Billing basis not declared: {} of {} attempts, counted neither as money spent nor as subscription work",
            totals.undeclared_attempts, totals.attempts
        ));
    }
    lines
}

/// Plan work stated as plan work: the rate that applied, the consumption it
/// implies, and what the same tokens would have cost on the API side.
///
/// None of this is money spent, and it is never folded into the cost above.
fn plan(totals: &codex_state::accounting::DayTotals) -> Vec<String> {
    if totals.plan_attempts == 0 {
        return Vec::new();
    }
    let burn = &totals.plan_burn_milli_tokens;
    let mut lines = vec![format!(
        "Subscription capacity: {} of {} attempts, not billed per token",
        totals.plan_attempts, totals.attempts
    )];
    lines.push(if burn.unknown == 0 {
        format!(
            "Plan consumption: {} tokens at the plan rate that applied",
            rate_scaled(burn.known)
        )
    } else if burn.known == 0 {
        format!(
            "Plan consumption: unavailable — no plan rate or token total for {} of {} plan attempts",
            burn.unknown, totals.plan_attempts
        )
    } else {
        format!(
            "Plan consumption: {} tokens at the plan rate, plus {} attempts with no stateable figure",
            rate_scaled(burn.known),
            burn.unknown
        )
    });
    lines.push(if totals.unknown_equivalents == 0 {
        format!(
            "Same tokens at API rates: {}",
            money(totals.equivalent_usd)
        )
    } else if totals.equivalent_usd == Decimal::default() {
        format!(
            "Same tokens at API rates: unavailable — no API rate or complete token count for {} of {} plan attempts",
            totals.unknown_equivalents, totals.plan_attempts
        )
    } else {
        format!(
            "Same tokens at API rates: at least {}; {} of {} plan attempts have no API rate or an incomplete token count",
            money(totals.equivalent_usd),
            totals.unknown_equivalents,
            totals.plan_attempts
        )
    });
    lines
}

/// Plan consumption is carried as tokens scaled by a rate in thousandths, so a
/// 1.0x turn reads back as exactly its own token count.
fn rate_scaled(milli_tokens: i64) -> String {
    let whole = grouped(milli_tokens / 1000);
    let fraction = milli_tokens % 1000;
    if fraction == 0 {
        whole
    } else {
        format!("{whole}.{fraction:03}")
    }
}

/// The exact known subtotal, or "none" when nothing billed per token was
/// priced: a bare `0` beside an unknown estimate reads as a zero-cost claim.
fn known_exact(t: &codex_state::accounting::DayTotals, gaps: EstimateGaps) -> String {
    let unknown = t.per_use_unknown();
    if t.attempts == 0 {
        "none — no recorded attempts in this conversation".to_string()
    } else if t.plan_attempts == t.attempts {
        "none — subscription work is not billed per token".to_string()
    } else if t.per_use_attempts() == 0 {
        "none — no work here is billed per token".to_string()
    } else if t.known_usd == Decimal::default() && unknown > 0 && gaps.charges.charged_attempts == 0
    {
        match gaps.incomplete_usage.min(unknown) {
            0 => "none — no price for these attempts",
            usage if usage == unknown => "none — usage incomplete for these attempts",
            _ => "none — no price or incomplete usage for these attempts",
        }
        .to_string()
    } else {
        exact(t.known_usd)
    }
}

/// One token counter across attempts: the known count, and how many attempts
/// did not report it.
fn metric_text(m: &codex_state::accounting::Metric) -> String {
    let attempts = |n: i64| format!("{n} {}", if n == 1 { "attempt" } else { "attempts" });
    match (m.known, m.unknown) {
        (known, 0) => grouped(known),
        (0, unknown) => format!("not reported ({})", attempts(unknown)),
        (known, unknown) => format!("{} + not reported by {}", grouped(known), attempts(unknown)),
    }
}

const METRICS: [&str; 7] = [
    "Input",
    "Noncached input (derived for inclusive input)",
    "Cache read",
    "Cache write",
    "Output",
    "Reasoning (subset, not separately billed)",
    "Total (not separately billed)",
];
const BUCKETS: [&str; 4] = ["Noncached input", "Cache read", "Cache write", "Output"];

/// Why an unreported cache-write count still yields exact costs: the price
/// bound at dispatch charges nothing for cache writes, so input is exactly
/// cache hits plus misses.
const WRITES_FREE: &str = "this price charges nothing for cache writes";

/// How many attempts were priced with a noncached-input and a cache-write
/// count they never reported (see `WRITES_FREE`).
fn resolved_counts<'a>(quotes: impl IntoIterator<Item = &'a ObservationQuote>) -> (i64, i64) {
    quotes.into_iter().fold((0, 0), |(noncached, write), q| {
        let priced = q.priced_counts();
        (
            noncached + i64::from(q.usage.noncached.is_none() && priced[0].is_some()),
            write + i64::from(q.usage.write.is_none() && priced[2].is_some()),
        )
    })
}

/// The state line for a home with no ledger. The view opens only in a build
/// that records costs, which creates the ledger with the first request it
/// records, so collection is not "off" there.
fn absent_state() -> &'static str {
    "Unavailable — nothing recorded in this home yet; the accounting ledger is created when the first request is."
}

/// What `/usage requests` says in a build that records no costs, instead of
/// opening a view it could never fill.
pub(crate) const NO_COST_HISTORY: &str = "Per-request cost history is not part of this build.";

fn attempt_text(q: &ObservationQuote) -> Vec<String> {
    let a = &q.attempt;
    let mut lines = vec![
        format!("Request: {}", a.request_id),
        format!("Attempt: {}", a.attempt_id),
        format!("Thread: {}", a.thread_id),
        format!("Turn: {}", a.turn),
        format!("Provider: {}", a.provider),
        format!(
            "Account: {}",
            a.account.as_ref().map_or_else(
                || codex_state::accounting::DEFAULT_ACCOUNT.to_string(),
                |account| match &account.fingerprint {
                    Some(fingerprint) => format!("{} (fingerprint {fingerprint})", account.name),
                    None => format!("{} (fingerprint unavailable)", account.name),
                }
            )
        ),
        format!("Model: {}", a.model),
        format!("Opaque scope: {}", a.scope),
        format!("Arithmetic dialect: {:?}", a.dialect),
        format!(
            "Admission time: {}",
            utc_instant(i64::from(a.dispatched_at_ms))
        ),
        format!(
            "Retry predecessor: {}",
            a.retry_of.map_or("none".into(), |v| v.to_string())
        ),
        match q.usage.refused_status {
            Some(status) => format!(
                "Completion/billing status: refused by the provider (HTTP {status}) before any response; no tokens, no charge. Literal wire/endpoint and usage observation wall time: unavailable"
            ),
            None => "Completion/billing status: not recorded. Literal wire/endpoint and usage observation wall time: unavailable"
                .into(),
        },
    ];
    // #824: a realtime session is admitted when it starts; its end is
    // recorded when it closes. #889 D223: its tokens are what its finished
    // responses reported, and it has no price.
    if is_realtime(q) {
        lines.push(format!(
            "Realtime session ended: {}",
            q.usage
                .session_ended_at_ms
                .map_or_else(|| "not recorded".to_string(), utc_instant)
        ));
        lines.push(format!("Realtime session: {}.", realtime_usage_text(&[q])));
    }
    // Zero counts quote as zero under a record with no rates; any money at
    // all means a rate applied.
    if per_use(q)
        && no_rates(q)
        && q.usage.refused_status.is_none()
        && charge::reported_charge(q).is_none()
        && !q.buckets.iter().any(
            |bucket| matches!(bucket, BucketQuote::Priced(amount) if *amount != Decimal::default()),
        )
    {
        lines.push("Token cost: unavailable — no applicable price; recorded usage is not a zero-cost claim.".into());
    } else {
        match codex_state::accounting::DayTotals::from_quotes([q]) {
            Ok(attempt_totals) => lines.extend(estimate(&attempt_totals, EstimateGaps::of([q]))),
            Err(_) => lines.push("Token cost: unavailable — exact arithmetic overflow".into()),
        }
    }
    if let Some(burn) = q.plan_burn_millis {
        lines.push(format!(
            "Plan rate at dispatch: {}x",
            rate_scaled(i64::from(burn))
        ));
    } else if q.is_plan() {
        lines.push("Plan rate at dispatch: not stated by the vendor".into());
    }
    lines.push(format!("Billing basis: {}", basis_text(q)));
    lines.extend(charge::price_basis(&[q]));
    lines.extend(billed_line(&[q]));
    let u = &q.usage;
    let priced = q.priced_counts();
    for (index, (label, value)) in METRICS
        .iter()
        .zip([
            u.input,
            u.noncached,
            u.read,
            u.write,
            u.output,
            u.reasoning,
            u.total,
        ])
        .enumerate()
    {
        let derived = match a.dialect {
            codex_state::accounting::Dialect::Inclusive => index == 1,
            codex_state::accounting::Dialect::NativeAnthropic => index == 0 || index == 6,
            codex_state::accounting::Dialect::UnknownCompatible => false,
        };
        // Where pricing resolved an unreported cache-write count, say what
        // each cost was computed from instead of calling it unknown.
        let resolved = match index {
            1 => priced[0].filter(|_| value.is_none()).map(|n| {
                format!(
                    "{n} (derived as input − cache read: cache writes were not reported, and {WRITES_FREE})"
                )
            }),
            3 => priced[2]
                .filter(|_| value.is_none())
                .map(|_| format!("not reported — {WRITES_FREE}")),
            _ => None,
        };
        if let Some(resolved) = resolved {
            lines.push(format!("{label}: {resolved}"));
            continue;
        }
        lines.push(format!(
            "{label}: {}",
            value.map_or("unknown — no retained numeric evidence".into(), |n| {
                if n == 0 {
                    if derived {
                        "0 (derived)"
                    } else {
                        "0 (reported)"
                    }
                    .into()
                } else {
                    format!("{n}{}", if derived { " (derived)" } else { "" })
                }
            })
        ));
    }
    // #396: a component with no rate states no money, even when it had no
    // tokens: zero tokens of an unpriced component is not a $0 price.
    // A long-context tier's rate counts too: pricing may apply it instead.
    let component_rates = q.snapshot.as_ref().map(|s| {
        let tier = s.long_context.as_ref().map(|tier| &tier.rates);
        let rated = |base: bool, long: fn(&codex_state::accounting::Rates) -> bool| {
            base || tier.is_some_and(long)
        };
        [
            rated(s.rates.noncached.is_some(), |r| r.noncached.is_some()),
            rated(s.rates.read.is_some(), |r| r.read.is_some()),
            rated(s.rates.write.is_some(), |r| r.write.is_some()),
            rated(s.rates.output.is_some(), |r| r.output.is_some()),
        ]
    });
    for (index, (label, bucket)) in BUCKETS.iter().zip(q.buckets).enumerate() {
        let rated = component_rates.is_some_and(|rated| rated[index]);
        lines.push(format!(
            "{label} cost: {}",
            match bucket {
                BucketQuote::Priced(v) if v == Decimal::default() && !rated => {
                    "none — 0 tokens".into()
                }
                BucketQuote::Priced(v) => format!("{}; exact USD {}", money(v), exact(v)),
                BucketQuote::MissingUsage => "unknown — no retained numeric evidence".into(),
                BucketQuote::MissingRate => "unknown — rate unavailable".into(),
            }
        ));
    }
    // #368: the components above are the published prices; a reported
    // charge replaces their sum.
    if charge::reported_charge(q).is_some() {
        lines.push(format!(
            "Published-price estimate (not used — {} reported its charge): {}",
            provider_name(&q.attempt.provider),
            charge::published_estimate(q)
        ));
    }
    lines.push(format!(
        "Known subtotal exact USD: {}",
        codex_state::accounting::DayTotals::from_quotes([q]).map_or_else(
            |_| exact(q.known_subtotal),
            |t| known_exact(&t, EstimateGaps::of([q]))
        )
    ));
    // A record that states no rate and no plan rate only binds the request's
    // billing basis (PF-60-S05): it is not a price, so it gets no price
    // metadata.
    let priced = q.snapshot.as_ref().filter(|s| {
        s.plan_burn_millis.is_some()
            || s.long_context.is_some()
            || s.rates != codex_state::accounting::Rates::default()
    });
    if q.snapshot.is_some() && priced.is_none() {
        lines.push(format!(
            "Price: none recorded — this request records only its billing basis: {}",
            basis_clause(q)
        ));
    } else if let Some(s) = priced {
        lines.extend([
            format!("Price ID: {}", s.id),
            format!(
                "Price source: {:?}; reference {}",
                s.source_kind, s.source_reference
            ),
            format!("Price currency/unit: {:?} / {:?}", s.currency, s.unit),
            format!(
                "Price observed/approved: {} / {}",
                utc_instant(i64::from(s.observed_at_ms)),
                utc_instant(i64::from(s.approved_at_ms))
            ),
            format!(
                "Price effective interval: [{}, {})",
                utc_instant(i64::from(s.effective_from_ms)),
                s.effective_end_ms
                    .map_or("unbounded".into(), |n| utc_instant(i64::from(n)))
            ),
        ]);
        for (label, rate) in BUCKETS.iter().zip([
            s.rates.noncached,
            s.rates.read,
            s.rates.write,
            s.rates.output,
        ]) {
            lines.push(rate.map_or_else(
                || format!("Rate unavailable for {label}"),
                |r| format!("{label} rate: {} USD per million tokens", exact(r)),
            ));
        }
        if let Some(long) = &s.long_context {
            let above = i64::from(long.above_input_tokens);
            lines.push(format!(
                "Long-context price: above {above} input tokens, the whole request is priced at:"
            ));
            for (label, rate) in BUCKETS.iter().zip([
                long.rates.noncached,
                long.rates.read,
                long.rates.write,
                long.rates.output,
            ]) {
                lines.push(rate.map_or_else(
                    || format!("  Rate unavailable for {label}"),
                    |r| format!("  {label} rate: {} USD per million tokens", exact(r)),
                ));
            }
            lines.push(match q.usage.input {
                Some(input) if input > above => {
                    format!("Long-context price applied: input {input} > {above}")
                }
                Some(input) => format!("Long-context price not applied: input {input} ≤ {above}"),
                None => "Long-context price: input unknown, so no rate applies".into(),
            });
        }
    } else {
        lines.push("Price: unavailable — no dispatch-time price snapshot".into());
    }
    for o in &q.observations {
        lines.push(format!(
            "Evidence revision {}; source {}; sequence {}; presence patch {}",
            i64::from(o.revision),
            o.source,
            i64::from(o.sequence),
            serde_json::to_string(&o.patch).unwrap_or_default()
        ));
    }
    lines
}

fn inspection_pages(result: Result<InspectionDay, String>) -> Vec<InspectorPage> {
    inspection_pages_for(result, /*period*/ None)
}

/// The command the user typed to open this view: `/cost`. The view is
/// developer-only; a default build answers `/usage requests` with
/// `NO_COST_HISTORY` and never opens it, so the other name appears only in
/// tests of the view itself.
pub(crate) fn cost_command() -> &'static str {
    if cfg!(feature = "developer-accounting") {
        "/cost"
    } else {
        "/usage requests"
    }
}

/// What the user can do when a view has no figures to show. `None` where the
/// state line already says it (a `Ready` view, or a range).
fn unavailable_next_step(state: &InspectionDay) -> Option<String> {
    let cost = cost_command();
    Some(match state {
        InspectionDay::Absent => format!(
            "Next step: send a turn in this conversation, then run {cost} again. If it stays unavailable, check your provider's bill."
        ),
        InspectionDay::MissingThread => {
            format!("Next step: open a saved conversation with /resume, then run {cost} there.")
        }
        // The ledger's checkpoint advances only when a request is recorded,
        // so waiting alone never clears this.
        InspectionDay::CheckpointLag => {
            "Next step: send a turn in this conversation to bring the ledger up to date, then select Refresh.".into()
        }
        InspectionDay::NeedsRefresh => {
            "Next step: select Refresh. If it stays unavailable, the recorded totals cannot be checked here; check your provider's bill.".into()
        }
        // The limit is per window, so a few hours of a refused day can open.
        InspectionDay::TooLarge => format!(
            "Next step: open a shorter span, such as a few hours with {cost} START END hour (UTC timestamps ending Z). If that is refused too, check your provider's bill."
        ),
        InspectionDay::DetailUnavailable { .. } => format!(
            "Next step: request detail is kept for recent days only; open a more recent day with {cost} YYYY-MM-DD, or check your provider's bill for this one."
        ),
        InspectionDay::Ready(_) | InspectionDay::Range { .. } => return None,
    })
}

/// `period` names the span a merged multi-day view covers; `None` means the
/// view is the single UTC day it states.
fn inspection_pages_for(
    result: Result<InspectionDay, String>,
    period: Option<String>,
) -> Vec<InspectorPage> {
    // #368: requests on a charge-reporting route are priced from that charge.
    let result = result.and_then(|day| {
        charge::priced_from_charges(day)
            .map_err(|_| "Cost unavailable — exact arithmetic overflow".to_string())
    });
    if let Ok(InspectionDay::Range {
        requested,
        aggregate_day_floor,
        completed_as_of_ms,
        read_at_ms,
        buckets,
    }) = result
    {
        return range_pages(
            requested,
            aggregate_day_floor,
            completed_as_of_ms,
            read_at_ms,
            buckets,
        );
    }
    let mut pages = vec![InspectorPage { title: "Cost — this conversation".into(), text: vec![
        "Collection coverage: unknown; recorded root and resolved descendants only. Unknown parent population excluded.".into(),
        "Logical requests may have attempts on other days; this UTC day is not their complete lifetime.".into(),
    ], links: Vec::new(), parent: None, selected: Arc::default() }];
    let ready = match result {
        Ok(InspectionDay::Ready(view)) => view,
        other => {
            if let Some(step) = other.as_ref().ok().and_then(unavailable_next_step) {
                pages[0].text.insert(0, step);
            }
            // A load error carries its next step in the same message; it gets
            // its own line, as the states above do.
            let other = match other {
                Err(message) => match message.split_once(" Next step: ") {
                    Some((reason, step)) => {
                        pages[0].text.insert(0, format!("Next step: {step}"));
                        Err(reason.to_string())
                    }
                    None => Err(message),
                },
                other => other,
            };
            pages[0].text.insert(0, match other {
                Ok(InspectionDay::Absent) => absent_state().into(),
                Ok(InspectionDay::MissingThread) => "Unavailable — native thread no longer exists.".into(),
                Ok(InspectionDay::CheckpointLag) => "Snapshot is not current; newer activity is unverified".into(),
                Ok(InspectionDay::NeedsRefresh) => "Recorded totals unavailable — stored contributions need refresh. Retry rereads only; no repair performed.".into(),
                Ok(InspectionDay::TooLarge) => "Range too large for this inspector. No total shown.".into(),
                Ok(InspectionDay::DetailUnavailable { coverage, read_at_ms, compact }) => format!(
                    "Request detail unavailable for this whole UTC day — {}. No total shown. Ledger current to: {}; {}.",
                    if compact { "compacted history lost request/provider attribution" } else { "day touches expired detail or aggregate history" },
                    utc_instant(coverage.completed_as_of_ms), retention(read_at_ms, coverage.aggregate_day_floor, coverage.oldest_recorded_day)),
                Err(message) => message,
                Ok(InspectionDay::Ready(_) | InspectionDay::Range { .. }) => unreachable!(),
            });
            return pages;
        }
    };
    if ready.read_at_ms > ready.coverage.completed_as_of_ms {
        pages[0]
            .text
            .push("Snapshot is not current; newer activity is unverified".into());
    }
    let freshness = pages[0].text.clone();
    // PF-84-S05: one decision for the whole view, the other conversations'
    // rows included, so every row and page names accounts the same way.
    let named = names_accounts(
        ready.requests.values().flatten().chain(
            ready
                .other_conversations
                .iter()
                .flat_map(|others| others.requests.values().flatten()),
        ),
    );
    // The plain overview already says when costs are estimates; the details
    // add the provider's own charge only where it stated one.
    let all: Vec<&ObservationQuote> = ready.requests.values().flatten().collect();
    if let Some(billed) = billed_figure(&all) {
        pages[0].text.insert(1, billed_detail(&billed));
    }
    let t = &ready.totals;
    pages[0].text.splice(
        0..0,
        if t.attempts == 0 {
            vec![empty_day::detail(ready.empty_day)]
        } else {
            estimate(t, EstimateGaps::of(ready.requests.values().flatten()))
        },
    );
    pages[0]
        .text
        .extend(unpriced_rows(ready.requests.values().flatten(), named));
    pages[0].text.extend([
        format!(
            "{DAY_COVERED} {}",
            interval(ready.utc_day * 86_400_000, (ready.utc_day + 1) * 86_400_000)
        ),
        format!(
            "Read at: {}; ledger current to: {} ({} ms behind)",
            utc_instant(ready.read_at_ms),
            utc_instant(ready.coverage.completed_as_of_ms),
            ready.read_at_ms - ready.coverage.completed_as_of_ms
        ),
        format!(
            "Retention: {}",
            retention(
                ready.read_at_ms,
                ready.coverage.aggregate_day_floor,
                ready.coverage.oldest_recorded_day
            )
        ),
        format!(
            "Known subtotal exact USD: {}",
            known_exact(t, EstimateGaps::of(ready.requests.values().flatten()))
        ),
    ]);
    let (noncached, write) = resolved_counts(ready.requests.values().flatten());
    // FUP-02: a day with nothing recorded lists no token counts; zeros would
    // read as no usage, which an empty day does not prove.
    let measured = if t.attempts == 0 {
        &[][..]
    } else {
        &t.measured[..]
    };
    for (index, (label, m)) in METRICS.iter().zip(measured).enumerate() {
        let n = match index {
            1 => noncached,
            3 => write,
            _ => 0,
        };
        let attempts = format!("{n} {}", if n == 1 { "attempt" } else { "attempts" });
        let note = match index {
            1 => format!(
                "costed as input − cache read for {attempts}, whose price charges nothing for cache writes"
            ),
            _ => format!("free at the price of {attempts}, so nothing was charged"),
        };
        pages[0].text.push(if n > 0 {
            format!("{label}: {} — {note}", metric_text(m))
        } else {
            format!("{label}: {}", metric_text(m))
        });
    }
    let context = pages[0].text.clone();
    let mut request_pages = std::collections::BTreeMap::new();
    let mut request_numbers = std::collections::BTreeMap::new();
    // Number requests in the order they were sent, not by their ids.
    let mut ordered: Vec<_> = ready.requests.iter().collect();
    ordered.sort_by_key(|(request, quotes)| {
        (
            quotes
                .iter()
                .map(|quote| i64::from(quote.attempt.dispatched_at_ms))
                .min(),
            **request,
        )
    });
    for (request, quotes) in ordered {
        let request_page = pages.len();
        request_pages.insert(*request, request_page);
        let number = pages[0].links.len() + 1;
        request_numbers.insert(*request, number);
        // Attempts in the order they were sent, so attempt 1 is the original.
        let mut attempts: Vec<&ObservationQuote> = quotes.iter().collect();
        attempts.sort_by_key(|q| (i64::from(q.attempt.dispatched_at_ms), q.attempt.attempt_id));
        let kind = if reopen_warmup(&attempts) {
            " · Reopen warm-up"
        } else if realtime_session(&attempts) {
            " · Realtime session"
        } else {
            ""
        };
        let label = match request_route(quotes, named) {
            Some(route) => format!(
                "Request {number}{kind} · {route} · {}",
                short_cost(&attempts)
            ),
            None => format!("Request {number}{kind}"),
        };
        pages[0].links.push((label, request_page));
        let mut text = plain_header(&attempts, named);
        if reopen_warmup(&attempts)
            && let Some(divider) = text.iter().position(|line| line == "—— Details ——")
        {
            text.insert(divider, REOPEN_WARMUP_KIND.to_string());
        }
        if realtime_session(&attempts)
            && let Some(divider) = text.iter().position(|line| line == "—— Details ——")
        {
            text.insert(divider, realtime_session_kind(&attempts));
        }
        text.push(format!("Request: {request}"));
        pages.push(InspectorPage {
            title: "Request".into(),
            text,
            links: Vec::new(),
            parent: Some(0),
            selected: Arc::default(),
        });
        for (index, quote) in attempts.iter().copied().enumerate() {
            let target = pages.len();
            pages[request_page]
                .links
                .push((format!("Technical details (attempt {})", index + 1), target));
            let mut text = plain_header(&[quote], named);
            text.extend(attempt_text(quote).into_iter().filter(|line| {
                !line.starts_with("Billed cost:") && !line.starts_with("Price basis:")
            }));
            pages.push(InspectorPage {
                title: "Attempt, components and original price".into(),
                text,
                links: Vec::new(),
                parent: Some(request_page),
                selected: Arc::default(),
            });
        }
    }
    // All navigation is over this one immutable packet; no second read.
    let root_groups = [
        (
            "Root's own attempts".to_owned(),
            ready
                .requests
                .values()
                .flatten()
                .filter(|q| q.attempt.thread_id == ready.owner)
                .collect::<Vec<_>>(),
        ),
        (
            "Descendant attempts".to_owned(),
            ready
                .requests
                .values()
                .flatten()
                .filter(|q| q.attempt.thread_id != ready.owner)
                .collect::<Vec<_>>(),
        ),
    ];
    // PF-84-S05: grouped by provider, account and model; accounts are named
    // only in a view that holds named-account work.
    let mut providers = std::collections::BTreeMap::new();
    for quote in ready.requests.values().flatten() {
        let key = (
            &quote.attempt.provider,
            quote.attempt.account_name(),
            &quote.attempt.model,
        );
        providers.entry(key).or_insert_with(Vec::new).push(quote);
    }
    let mut groups = root_groups.to_vec();
    for ((provider, account, model), quotes) in providers {
        let provider = if provider.trim().is_empty() {
            "unknown (attribution absent)"
        } else {
            provider
        };
        let model = if model.trim().is_empty() {
            "unknown (attribution absent)"
        } else {
            model
        };
        let title = if named {
            format!("Provider: {provider}; Account: {account}; Model: {model}")
        } else {
            format!("Provider: {provider}; Model: {model}")
        };
        groups.push((title, quotes));
    }
    // Keep missing attribution visible even when this schema has no such rows.
    if !ready
        .requests
        .values()
        .flatten()
        .any(|q| q.attempt.provider.trim().is_empty() || q.attempt.model.trim().is_empty())
    {
        groups.push(("Unknown provider/model attribution".into(), Vec::new()));
    }
    for (title, quotes) in groups {
        let provider_group = title.starts_with("Provider: ") && !quotes.is_empty();
        let mut text = if provider_group {
            plain_header(&quotes, named)
        } else {
            Vec::new()
        };
        text.extend(freshness.iter().cloned());
        text.extend(
            context
                .iter()
                .filter(|s| s.starts_with("Read at:") || s.starts_with(DAY_COVERED))
                .cloned(),
        );
        match codex_state::accounting::DayTotals::from_quotes(quotes.iter().copied()) {
            Ok(t) => {
                let gaps = EstimateGaps::of(quotes.iter().copied());
                text.extend(estimate(&t, gaps));
                text.push(format!("Recorded attempts: {}", t.attempts));
                if t.attempts > 0 {
                    text.push(format!(
                        "Known {} exact USD: {}",
                        if gaps.charges.charged_attempts > 0 {
                            "cost"
                        } else {
                            "estimate"
                        },
                        known_exact(&t, gaps)
                    ));
                }
            }
            Err(_) => text.push("Estimate unavailable — exact arithmetic overflow".into()),
        }
        if !provider_group {
            text.extend(billed_line(&quotes));
        }
        // In send order, numbered as on the first screen.
        let links = quotes
            .iter()
            .map(|q| {
                (
                    request_pages[&q.attempt.request_id],
                    format!("Request {}", request_numbers[&q.attempt.request_id]),
                )
            })
            .collect::<std::collections::BTreeMap<_, _>>()
            .into_iter()
            .map(|(page, label)| (label, page))
            .collect();
        let target = pages.len();
        let label = if provider_group {
            format!(
                "{} — {} ({})",
                route_name_in(quotes[0], named),
                short_cost(&quotes),
                request_count(&quotes)
            )
        } else {
            format!("{title} ({})", attempt_count(quotes.len()))
        };
        pages[0].links.push((label, target));
        pages.push(InspectorPage {
            title,
            text,
            links,
            parent: Some(0),
            selected: Arc::default(),
        });
    }
    let unknown_page = pages.len();
    let u = &ready.unknown_parent_totals;
    pages[0].links.push((
        format!(
            "Unknown parent population ({})",
            attempt_count(usize::try_from(u.attempts).unwrap_or(0))
        ),
        unknown_page,
    ));
    let mut text = vec![
        "Membership in this root is unknown. These attempts are separate from root and descendant totals; they may belong to other runs.".into(),
        format!("Recorded attempts with unresolved ancestry: {}", u.attempts),
    ];
    if ready.unknown_parent_unavailable_threads > 0 {
        let note = format!(
            "Unresolved ancestry: {} threads have unavailable day detail; their costs and retention coverage are unknown and excluded from this root.",
            ready.unknown_parent_unavailable_threads
        );
        pages[0].text.push(note.clone());
        text.push(note);
        text.push("Estimates below cover inspectable attempts only; unavailable threads may have additional unknown costs.".into());
    }
    text.extend(estimate(
        u,
        EstimateGaps::of(ready.unknown_parent_requests.values().flatten()),
    ));
    text.extend(billed_line(
        &ready
            .unknown_parent_requests
            .values()
            .flatten()
            .collect::<Vec<_>>(),
    ));
    text.extend(scope::overflow_note(
        ready.unknown_parent_requests.values().flatten(),
    ));
    text.extend(
        context
            .iter()
            .filter(|s| s.starts_with("Read at:") || s.starts_with(DAY_COVERED))
            .cloned(),
    );
    text.extend(freshness);
    pages.push(InspectorPage {
        title: "Unknown parent population".into(),
        text,
        links: Vec::new(),
        parent: Some(0),
        selected: Arc::default(),
    });
    for (request, quotes) in &ready.unknown_parent_requests {
        for quote in quotes {
            let target = pages.len();
            pages[unknown_page].links.push((
                format!("Request {request}; attempt {}", quote.attempt.attempt_id),
                target,
            ));
            pages.push(InspectorPage {
                title: "Unknown parent attempt".into(),
                text: attempt_text(quote),
                links: Vec::new(),
                parent: Some(unknown_page),
                selected: Arc::default(),
            });
        }
    }
    pages[0].text.push("Root total = own attempts + resolved descendant attempts. Provider/model groups partition the same root total. Compare exact USD, not rounded displays.".into());
    pages[0].text.push(format!(
        "Unknown parent population: {} attempts, excluded from root total",
        u.attempts
    ));
    for page in &mut pages {
        if ready.read_at_ms > ready.coverage.completed_as_of_ms
            && !page
                .text
                .iter()
                .any(|s| s == "Snapshot is not current; newer activity is unverified")
        {
            page.text
                .push("Snapshot is not current; newer activity is unverified".into());
        }
    }
    // Plain first screen: the total stays first, then one line per provider
    // and model, then the auditing detail below a divider.
    let gap_line = ready.recording_gap.then(|| {
        recording_gap_line(if period.is_some() {
            "this period"
        } else {
            "this day"
        })
    });
    let heading = period.map_or_else(
        || day_heading(ready.utc_day, Utc::now().timestamp() / 86_400),
        |period| format!("This conversation, {period} (UTC):"),
    );
    let outside = scope::other_conversations_lines(ready.other_conversations.as_ref(), named);
    let next_step = scope::no_price_next_step(
        ready.requests.values().flatten().chain(
            ready
                .other_conversations
                .iter()
                .flat_map(|others| others.requests.values().flatten()),
        ),
    );
    let outside_per_use: Vec<&ObservationQuote> = ready
        .other_conversations
        .iter()
        .flat_map(|others| others.requests.values().flatten())
        .filter(|quote| has_bill(quote))
        .collect();
    let outside_plan = ready
        .other_conversations
        .iter()
        .flat_map(|others| others.requests.values().flatten())
        .any(ObservationQuote::is_plan);
    let overview = plain_overview(
        heading,
        ready.empty_day,
        named,
        ready.requests.values().flatten(),
        outside,
        (outside_per_use, outside_plan),
        next_step,
    );
    pages[0].text.splice(0..0, overview);
    // #314 (D228): right under the heading, before any figure it qualifies.
    if let Some(line) = gap_line {
        pages[0].text.insert(1, line);
    }
    // Provider/model groups first, then each request, then the auditing
    // groups (own/descendant attempts, attribution, unknown parents).
    let mut links = std::mem::take(&mut pages[0].links);
    links.sort_by_key(|(label, target)| {
        if pages[*target].title.starts_with("Provider: ") {
            0
        } else if label.starts_with("Request ") {
            1
        } else {
            2
        }
    });
    pages[0].links = links;
    pages
}

/// Built-in providers' names, by id.
fn built_in_providers() -> &'static std::collections::HashMap<String, String> {
    static NAMES: std::sync::OnceLock<std::collections::HashMap<String, String>> =
        std::sync::OnceLock::new();
    NAMES.get_or_init(|| {
        codex_model_provider_info::built_in_model_providers(/*openai_base_url*/ None)
            .into_iter()
            .map(|(id, info)| (id, info.name))
            .collect()
    })
}

/// The provider's display name ("Claude Account", "DeepSeek"), else its id.
fn provider_name(id: &str) -> String {
    if id.trim().is_empty() {
        return "Unknown provider".to_string();
    }
    // The built-in keeps its legacy brand name as an identity; the product,
    // and /providers, call it the Corbanu API.
    // #352: both local providers share the built-in name "gpt-oss"; name
    // them by the server that runs them.
    match id {
        codex_model_provider_info::PFTERMINAL_PLAN_PROVIDER_ID => return "Corbanu API".to_string(),
        codex_model_provider_info::OLLAMA_OSS_PROVIDER_ID => return "Ollama".to_string(),
        codex_model_provider_info::LMSTUDIO_OSS_PROVIDER_ID => return "LM Studio".to_string(),
        _ => {}
    }
    built_in_providers()
        .get(id)
        .cloned()
        .unwrap_or_else(|| id.to_string())
}

/// The built-in provider a route belongs to: an Anthropic-wire route, or an
/// alias, is its operator's.
fn operator(id: &str) -> &str {
    use codex_model_provider_info as ids;
    match ids::canonical_provider_id(id) {
        ids::ZAI_ANTHROPIC_PROVIDER_ID => ids::ZAI_PROVIDER_ID,
        ids::OPENROUTER_ANTHROPIC_PROVIDER_ID => ids::OPENROUTER_PROVIDER_ID,
        ids::BASETEN_ANTHROPIC_PROVIDER_ID => ids::BASETEN_PROVIDER_ID,
        ids::VERCEL_ANTHROPIC_PROVIDER_ID | ids::VERCEL_ANTHROPIC_FAST_PROVIDER_ID => {
            ids::VERCEL_PROVIDER_ID
        }
        ids::PFTERMINAL_PLAN_ANTHROPIC_PROVIDER_ID => ids::PFTERMINAL_PLAN_PROVIDER_ID,
        other => other,
    }
}

/// The model's catalogue display name ("GPT-6 Sol"), else its slug. #352: a
/// name that carries its route's brand ("Ambient GLM 5.2") is not the name of
/// a row another operator served (`z-ai/glm-5.2` sent to OpenRouter), nor of
/// one a provider you configured served: its id says nothing about whose model
/// it serves. #1196: a name without a route brand ("Claude Opus 5.5") names
/// the model on any route.
fn model_name(provider: &str, slug: &str) -> String {
    /// Display name and owning provider, by slug.
    type Names = std::collections::HashMap<String, (String, Option<String>)>;
    static NAMES: std::sync::OnceLock<Names> = std::sync::OnceLock::new();
    if slug.trim().is_empty() {
        return "unknown model".to_string();
    }
    match NAMES
        .get_or_init(|| {
            codex_models_manager::bundled_models_response()
                .map(|catalog| {
                    catalog
                        .models
                        .into_iter()
                        .map(|model| {
                            let owner = model
                                .orchestration
                                .as_ref()
                                .map(|orchestration| orchestration.provider_id().to_string());
                            (model.slug, (model.display_name, owner))
                        })
                        .collect()
                })
                .unwrap_or_default()
        })
        .get(slug)
    {
        Some((name, owner))
            if owner.as_deref().is_none_or(|owner| {
                operator(owner) == operator(provider) || !carries_route_brand(name, owner)
            }) =>
        {
            name.clone()
        }
        _ => slug.to_string(),
    }
}

/// Whether a catalogue name carries the brand of the route that owns its row:
/// every word of the owning operator's built-in name is a word of it ("Z.AI
/// GLM 5.2" for Z.AI, "Claude Opus 5.5 Plan" for Claude Plan), ignoring case.
/// A word that names how a route sells the model ("Plan", "(Direct)",
/// "Free") is a brand too, as are an empty name and an owner with no
/// built-in name, so such names are never borrowed.
fn carries_route_brand(name: &str, owner: &str) -> bool {
    let Some(brand) = built_in_providers().get(operator(owner)) else {
        return true;
    };
    let words: std::collections::HashSet<String> =
        name.split_whitespace().map(str::to_lowercase).collect();
    words.is_empty()
        || ["plan", "(direct)", "free"]
            .iter()
            .any(|word| words.contains(*word))
        || brand
            .split_whitespace()
            .all(|word| words.contains(&word.to_lowercase()))
}

/// "Provider · Model", or "Provider · account NAME · Model" where the view
/// names accounts (`default` included).
fn route_name_in(quote: &ObservationQuote, named: bool) -> String {
    if named {
        format!(
            "{} · account {} · {}",
            provider_name(&quote.attempt.provider),
            quote.attempt.account_name(),
            model_name(&quote.attempt.provider, &quote.attempt.model)
        )
    } else {
        format!(
            "{} · {}",
            provider_name(&quote.attempt.provider),
            model_name(&quote.attempt.provider, &quote.attempt.model)
        )
    }
}

/// PF-84-S05: whether any attempt here was sent on a named account. Only such
/// a view names accounts, so default-account work reads as it always has.
fn names_accounts<'a>(quotes: impl IntoIterator<Item = &'a ObservationQuote>) -> bool {
    quotes
        .into_iter()
        .any(|quote| quote.attempt.account.is_some())
}

/// The route of a request's attempts, or every distinct route when a retry
/// moved to another one.
fn request_route(quotes: &[ObservationQuote], named: bool) -> Option<String> {
    let mut routes: Vec<String> = Vec::new();
    for route in quotes.iter().map(|quote| route_name_in(quote, named)) {
        if !routes.contains(&route) {
            routes.push(route);
        }
    }
    match routes.len() {
        0 => None,
        1 => routes.pop(),
        _ => Some(format!("several routes ({})", routes.join(", "))),
    }
}

/// `12345` as `12,345`.
fn grouped(n: i64) -> String {
    let digits = n.unsigned_abs().to_string();
    let mut out = String::new();
    for (index, digit) in digits.chars().enumerate() {
        if index > 0 && (digits.len() - index).is_multiple_of(3) {
            out.push(',');
        }
        out.push(digit);
    }
    if n < 0 { format!("-{out}") } else { out }
}

const COVERED: &str = "Covered by your subscription (not billed per request)";
const PAY_PER_USE: &str = "Pay per use";
const LOCAL: &str = "Local (runs on your own machine)";
const LOCAL_FIGURE: &str = "No charge";
const NOT_DECLARED: &str = "Billing basis not declared";
const NOT_DECLARED_FIGURE: &str = "Counted neither as money spent nor as subscription work";
/// The cost of work whose basis is not declared: unknown, not "unbilled".
const UNDECLARED_COST: &str = "Estimated token cost: unknown — the billing basis is not declared, so whether this work was billed per token is unknown; your provider's bill is the final amount.";
/// Shown with subscription work only (option B): overflow is never counted.
const OVERFLOW_NOTE: &str = "Paid usage beyond a plan (usage credits, Extra Usage or extra credits you bought) isn't visible here and is never counted as spent; check your provider's usage page.";

/// Pay-per-use work: billed per token, whether or not a price is known.
/// Subscription, local and undeclared work is never stated as money spent.
fn per_use(quote: &ObservationQuote) -> bool {
    !quote.is_plan() && quote.basis() == codex_state::accounting::Basis::Billed
}

/// How one attempt is paid for, and where that was declared.
fn basis_text(quote: &ObservationQuote) -> String {
    use codex_state::accounting::Basis;
    // Every attempt admitted since billing bases exist carries a record of its
    // basis; one without is older, and is not given a basis after the fact.
    let Some(snapshot) = quote.snapshot.as_ref() else {
        return "not recorded (recorded before billing bases)".to_string();
    };
    let basis = match quote.basis() {
        Basis::Billed if quote.is_plan() => "subscription",
        Basis::Billed => "pay per use",
        Basis::PlanEquivalent => "subscription",
        Basis::Local => "local, no charge",
        Basis::Undeclared => "not declared",
    };
    match snapshot.basis_source {
        codex_state::accounting::BasisSource::UserConfig => format!("{basis} (set in your config)"),
        // #352: the source is recorded on every attempt; say it either way.
        // An undeclared basis has no default to name.
        codex_state::accounting::BasisSource::BuiltIn if quote.basis() != Basis::Undeclared => {
            format!("{basis} (built-in default)")
        }
        codex_state::accounting::BasisSource::BuiltIn => basis.to_string(),
    }
}

/// #396: the basis inside a sentence that already ends in it, without a
/// second pair of parentheses: "pay per use; set in your config".
fn basis_clause(quote: &ObservationQuote) -> String {
    basis_text(quote)
        .replace(" (set in your config)", "; set in your config")
        .replace(" (built-in default)", "; built-in default")
}
/// #403: the turn-label prefix core gives the warm-up a reopened or forked
/// conversation sends (core `accounting::REOPEN_PREWARM_LABEL`, pinned by
/// core's `reopen_prewarm_label_is_stable`); a label can carry an owner prefix
/// before it, such as `side:`.
const REOPEN_PREWARM_LABEL: &str = "prewarm:reopen:";

/// Whether these attempts are a reopen warm-up request.
fn reopen_warmup(quotes: &[&ObservationQuote]) -> bool {
    quotes
        .iter()
        .any(|quote| quote.attempt.turn.contains(REOPEN_PREWARM_LABEL))
}

const REOPEN_WARMUP_KIND: &str = "Kind: Reopen warm-up — sent when this conversation was reopened or forked, to prime the model's cache. Billed like any request; set skip_reopen_warmup = true in config.toml to skip it.";

/// #824: the turn-label prefix core gives a realtime WebSocket session (core
/// `accounting::REALTIME_SESSION_LABEL`, pinned by core's
/// `realtime_session_label_is_stable`); a label can carry an owner prefix
/// before it, such as `side:`. A WebRTC call (`realtime:` alone) is recorded
/// at its creation and is not named a session here: the end it may carry
/// (#955) is its client control channel's, not the media call's.
const REALTIME_SESSION_LABEL: &str = "realtime:ws:";

/// Whether this attempt is a realtime WebSocket session.
fn is_realtime(quote: &ObservationQuote) -> bool {
    let turn = &quote.attempt.turn;
    quote.usage.refused_status.is_none()
        && (turn.starts_with(REALTIME_SESSION_LABEL)
            || turn.contains(&format!(":{REALTIME_SESSION_LABEL}")))
}

/// Whether these attempts are a realtime session request.
fn realtime_session(quotes: &[&ObservationQuote]) -> bool {
    quotes.iter().any(|quote| is_realtime(quote))
}

/// Why a realtime session with no recorded tokens has no token counts or
/// cost: they are unknown, not zero. A session recorded before #889 D223, on
/// a protocol that reports no usage, or with no finished response has none.
const REALTIME_UNKNOWN: &str =
    "its token counts and cost are unknown, not zero: no token usage was recorded for it";

/// #889 D223: what a realtime session's recorded tokens are, and why it has
/// no cost figure.
const REALTIME_REPORTED: &str = "its tokens are those the provider reported at the end of each response, so they may be low: a response cut short by the session's end reports none, and one whose report could not be read is not counted; its cost is unknown, not zero: Corbanu has no realtime prices";

/// Whether a realtime attempt recorded any token count (#889 D223).
fn realtime_tokens_recorded(quote: &ObservationQuote) -> bool {
    let usage = &quote.usage;
    [usage.input, usage.read, usage.output, usage.total]
        .iter()
        .any(Option::is_some)
}

/// The realtime session's usage note: reported tokens with no price, or
/// tokens and cost unknown.
fn realtime_usage_text(quotes: &[&ObservationQuote]) -> &'static str {
    if quotes.iter().any(|quote| realtime_tokens_recorded(quote)) {
        REALTIME_REPORTED
    } else {
        REALTIME_UNKNOWN
    }
}

/// The request page's kind line: when the session started and ended.
fn realtime_session_kind(quotes: &[&ObservationQuote]) -> String {
    let started = quotes
        .iter()
        .map(|quote| i64::from(quote.attempt.dispatched_at_ms))
        .min()
        .map_or_else(|| "unknown".to_string(), utc_instant);
    let ended = quotes
        .iter()
        .filter_map(|quote| quote.usage.session_ended_at_ms)
        .max()
        .map_or_else(|| "not recorded".to_string(), utc_instant);
    format!(
        "Kind: Realtime session — started {started}, ended {ended}; {}.",
        realtime_usage_text(quotes)
    )
}

/// "Realtime: 1 session; tokens as the provider reported them …" for the
/// first screen, or nothing when no request here is one. A session's cost is
/// always unknown, never zero (#889 D223).
fn realtime_session_line<'a>(
    quotes: impl IntoIterator<Item = &'a ObservationQuote>,
) -> Option<String> {
    // Each session request, and whether it recorded any token count.
    let mut sessions: std::collections::BTreeMap<Uuid, bool> = std::collections::BTreeMap::new();
    for quote in quotes.into_iter().filter(|quote| is_realtime(quote)) {
        *sessions.entry(quote.attempt.request_id).or_default() |= realtime_tokens_recorded(quote);
    }
    let total = sessions.len();
    if total == 0 {
        return None;
    }
    let reported = sessions.values().filter(|reported| **reported).count();
    let noun = if total == 1 { "session" } else { "sessions" };
    const REPORTED: &str = "tokens as the provider reported them at the end of each response";
    const NO_PRICES: &str = "cost unknown, not zero — Corbanu has no realtime prices";
    Some(if reported == 0 {
        let pronoun = if total == 1 { "it" } else { "them" };
        format!(
            "Realtime: {total} {noun}; token counts and cost unknown, not zero — no token usage was recorded for {pronoun}."
        )
    } else if reported == total {
        format!("Realtime: {total} {noun}; {REPORTED}; {NO_PRICES}.")
    } else {
        format!(
            "Realtime: {total} {noun}; {REPORTED} for {reported}, none recorded for {}; {NO_PRICES}.",
            total - reported
        )
    })
}

/// "Reopen warm-up: 1 request, estimated $0.024812. Sent when …" for the
/// first screen, or nothing when no request here is one.
fn reopen_warmup_line<'a>(
    quotes: impl IntoIterator<Item = &'a ObservationQuote>,
) -> Option<String> {
    let (warmups, requests) = reopen_warmups(quotes);
    if requests == 0 {
        return None;
    }
    let noun = if requests == 1 { "request" } else { "requests" };
    Some(format!(
        "Reopen warm-up: {requests} {noun}, {}. Sent when this conversation was reopened or forked, to prime the model's cache.",
        short_cost(&warmups)
    ))
}

/// The reopen warm-up attempts among `quotes`, and how many requests they are.
fn reopen_warmups<'a>(
    quotes: impl IntoIterator<Item = &'a ObservationQuote>,
) -> (Vec<&'a ObservationQuote>, usize) {
    let warmups: Vec<&ObservationQuote> = quotes
        .into_iter()
        .filter(|quote| quote.attempt.turn.contains(REOPEN_PREWARM_LABEL))
        .collect();
    let mut requests: Vec<Uuid> = warmups.iter().map(|q| q.attempt.request_id).collect();
    requests.sort_unstable();
    requests.dedup();
    let count = requests.len();
    (warmups, count)
}

const NO_PRICE: &str = "Estimated cost: no price available";
/// A price applies but the provider did not report every token count it
/// needs, so no estimate is possible. Not a missing price: the next step for
/// unpriced requests does not apply to it.
const NO_USAGE: &str = "Estimated cost: not available — usage incomplete";
const DAY_COVERED: &str = "Day covered (UTC):";

/// How many pay-per-use attempts with no complete estimate lack a price, and
/// how many have one but are missing a usage count the price needs; and how
/// much of the known cost is charges a provider reported (`charge`) rather
/// than estimates from published prices.
#[derive(Clone, Copy, Default)]
struct EstimateGaps {
    incomplete_usage: i64,
    charges: ChargeSplit,
}

/// The known pay-per-use cost split by basis (#368): attempts priced from the
/// charge their provider reported, and estimates from published prices,
/// complete or in part. Refused attempts cost nothing and are neither. The two
/// sums add up to the known cost. A sum the exact type cannot hold is `None`
/// and not stated.
#[derive(Clone, Copy, Default)]
struct ChargeSplit {
    charged_attempts: i64,
    /// Whose reported charges those are.
    charged_by: charge::Chargers,
    charged: Option<Decimal>,
    estimated_attempts: i64,
    estimated: Option<Decimal>,
}

impl ChargeSplit {
    /// Every known figure is a reported charge.
    fn all_charged(self) -> bool {
        self.charged_attempts > 0 && self.estimated_attempts == 0
    }

    /// "charged by OpenRouter", or "$A charged by OpenRouter + $B estimated
    /// from published prices", naming every provider whose charge is counted;
    /// `None` when no figure is a reported charge.
    fn text(self) -> Option<String> {
        if self.charged_attempts == 0 {
            return None;
        }
        let provider = self.charged_by.names();
        if self.estimated_attempts == 0 {
            return Some(format!("charged by {provider}"));
        }
        Some(match self.charged.zip(self.estimated) {
            Some((charged, estimated)) => format!(
                "{} charged by {provider} + {} estimated from published prices",
                money(charged),
                money(estimated)
            ),
            // The parts are too large to state exactly; the basis still is.
            None => format!("partly charged by {provider}, partly estimated from published prices"),
        })
    }
}

impl EstimateGaps {
    fn of<'a>(quotes: impl IntoIterator<Item = &'a ObservationQuote>) -> Self {
        let mut gaps = Self {
            charges: ChargeSplit {
                charged: Some(Decimal::default()),
                estimated: Some(Decimal::default()),
                ..ChargeSplit::default()
            },
            ..Self::default()
        };
        for q in quotes.into_iter().filter(|q| per_use(q)) {
            if q.all_buckets_priced.is_none() && !lacks_price(q) {
                gaps.incomplete_usage = gaps.incomplete_usage.saturating_add(1);
            }
            // Every known dollar is either a reported charge or a (possibly
            // partial) estimate, so the two parts always add up to the total.
            let split = &mut gaps.charges;
            if let Some(charge) = charge::reported_charge(q) {
                split.charged_attempts = split.charged_attempts.saturating_add(1);
                split.charged_by = split.charged_by.with(&q.attempt.provider);
                split.charged = split.charged.and_then(|sum| sum.add(charge).ok());
            } else if q.usage.refused_status.is_none()
                && (q.all_buckets_priced.is_some() || q.known_subtotal != Decimal::default())
            {
                split.estimated_attempts = split.estimated_attempts.saturating_add(1);
                split.estimated = split
                    .estimated
                    .and_then(|sum| sum.add(q.known_subtotal).ok());
            }
        }
        gaps
    }

    /// "1 attempt had no price, 2 had incomplete usage" for `unknown` attempts.
    fn reasons(self, unknown: i64) -> String {
        let usage = self.incomplete_usage.min(unknown);
        let price = unknown - usage;
        let noun = |n: i64| if n == 1 { "attempt" } else { "attempts" };
        match (price, usage) {
            (price, 0) => format!("{price} {} had no price", noun(price)),
            (0, usage) => format!("{usage} {} had incomplete usage", noun(usage)),
            (price, usage) => {
                format!(
                    "{price} {} had no price, {usage} had incomplete usage",
                    noun(price)
                )
            }
        }
    }
}

/// No rate was bound at dispatch: no record at all (an attempt from before
/// billing bases), or one that states only the attempt's basis.
fn no_rates(quote: &ObservationQuote) -> bool {
    quote
        .snapshot
        .as_ref()
        .is_none_or(|snapshot| snapshot.rates == codex_state::accounting::Rates::default())
}

/// No price applies: none was bound at dispatch, or the bound one states no
/// rate for a bucket the attempt has. An attempt with a price whose only gaps
/// are counts the provider did not report has incomplete usage instead.
fn lacks_price(quote: &ObservationQuote) -> bool {
    no_rates(quote)
        || quote
            .buckets
            .iter()
            .any(|bucket| matches!(bucket, BucketQuote::MissingRate))
}

/// How a set of attempts is paid for, and the one money figure that goes
/// with it. Subscription work is never stated as money spent: its figure is
/// what the same work would cost at API prices. `gaps` must come from the
/// same attempts as `t`.
fn plain_billing(
    t: &codex_state::accounting::DayTotals,
    gaps: EstimateGaps,
) -> (&'static str, String) {
    let billed = t.per_use_attempts();
    let unknown = t.per_use_unknown();
    let charges = gaps.charges;
    let per_use = (billed > 0).then(|| match (unknown, charges.text()) {
        (0, None) => format!("Estimated cost: {}", money(t.known_usd)),
        // #368: a figure that is the provider's own charge is a cost, not an
        // estimate, and says whose charge it is.
        (0, Some(split)) if charges.all_charged() => {
            format!("Cost: {}, {split}", money(t.known_usd))
        }
        (0, Some(split)) => format!("Cost: {} ({split})", money(t.known_usd)),
        (_, None) if t.known_usd == Decimal::default() => {
            match gaps.incomplete_usage.min(unknown) {
                0 => NO_PRICE.to_string(),
                usage if usage == unknown => NO_USAGE.to_string(),
                _ => format!("Estimated cost: not available ({})", gaps.reasons(unknown)),
            }
        }
        (_, None) => format!(
            "Estimated cost: at least {} ({})",
            money(t.known_usd),
            gaps.reasons(unknown)
        ),
        (_, Some(split)) => format!(
            "Cost: at least {} ({}; {split})",
            money(t.known_usd),
            gaps.reasons(unknown)
        ),
    });
    let covered = (t.plan_attempts > 0).then(|| {
        if t.unknown_equivalents == 0 {
            format!("Same work at API prices: {}", money(t.equivalent_usd))
        } else if t.equivalent_usd == Decimal::default() {
            "Same work at API prices: not available".to_string()
        } else {
            format!(
                "Same work at API prices: at least {}",
                money(t.equivalent_usd)
            )
        }
    });
    let local = (t.local_attempts > 0).then(|| LOCAL_FIGURE.to_string());
    let undeclared = (t.undeclared_attempts > 0).then(|| NOT_DECLARED_FIGURE.to_string());
    match (per_use, covered, local, undeclared) {
        (Some(per_use), None, None, None) => (PAY_PER_USE, per_use),
        (None, Some(covered), None, None) => (COVERED, covered),
        (Some(per_use), Some(covered), None, None) => (
            "Partly subscription, partly pay per use",
            format!("{per_use}; subscription part — {}", lower_first(&covered)),
        ),
        (None, None, Some(local), None) => (LOCAL, local),
        (None, None, None, Some(undeclared)) => (NOT_DECLARED, undeclared),
        (None, None, None, None) => (PAY_PER_USE, "Nothing recorded".to_string()),
        (per_use, covered, local, undeclared) => (
            "Mixed",
            [
                per_use.map(|figure| format!("pay per use — {}", lower_first(&figure))),
                covered.map(|figure| format!("subscription — {}", lower_first(&figure))),
                local.map(|figure| format!("local — {}", lower_first(&figure))),
                undeclared.map(|figure| format!("not declared — {}", lower_first(&figure))),
            ]
            .into_iter()
            .flatten()
            .collect::<Vec<_>>()
            .join("; "),
        ),
    }
}

/// Total tokens, or input plus output where a provider reports no total.
fn plain_tokens(t: &codex_state::accounting::DayTotals) -> String {
    let [input, noncached, read, write, output, _, total] = &t.measured;
    let (known, unknown) = if total.unknown == 0 {
        (total.known, 0)
    } else {
        // An unknown input total (a missing cache-write count, say) still has
        // the parts the provider reported; both are lower bounds of the input.
        let input_parts = noncached
            .known
            .saturating_add(read.known)
            .saturating_add(write.known);
        (
            input.known.max(input_parts).saturating_add(output.known),
            input.unknown.max(output.unknown),
        )
    };
    match (known, unknown) {
        (0, unknown) if unknown > 0 => "tokens not reported".to_string(),
        (known, 0) => format!("{} tokens", grouped(known)),
        (known, _) => format!("{}+ tokens", grouped(known)),
    }
}

/// "Same work at API prices" as "same work at API prices".
fn lower_first(text: &str) -> String {
    let mut chars = text.chars();
    chars.next().map_or_else(String::new, |first| {
        first.to_lowercase().chain(chars).collect()
    })
}

fn request_count(quotes: &[&ObservationQuote]) -> String {
    let requests = quotes
        .iter()
        .map(|q| q.attempt.request_id)
        .collect::<std::collections::BTreeSet<_>>()
        .len();
    format!(
        "{requests} {}",
        if requests == 1 { "request" } else { "requests" }
    )
}

/// The charges the provider stated for a group of pay-per-use attempts.
struct BilledCharge {
    sum: Decimal,
    /// Attempts whose charge the provider stated.
    stated: usize,
    /// Attempts that recorded no usage at all: a refused request, or one that
    /// failed before any report. Neither stated a charge, and a failed one may
    /// still have been billed, so a total that excludes them stays "at least".
    silent: usize,
    attempts: usize,
}

impl BilledCharge {
    /// Every attempt that reported usage also stated its charge.
    fn answered_all_stated(&self) -> bool {
        self.stated + self.silent == self.attempts
    }

    fn text(&self) -> String {
        let sum = money(self.sum);
        if self.stated == self.attempts {
            sum
        } else if self.answered_all_stated() {
            format!(
                "at least {sum} ({} {} reported no usage; a refused request is normally not charged)",
                self.silent,
                if self.silent == 1 {
                    "failed attempt"
                } else {
                    "failed attempts"
                }
            )
        } else {
            format!(
                "at least {sum} ({} of {} attempts stated a charge)",
                self.stated, self.attempts
            )
        }
    }
}

/// What the provider itself stated it charged for the pay-per-use attempts here,
/// or None when none stated anything. Plan work is never billed per request.
fn billed_charge(quotes: &[&ObservationQuote]) -> Option<BilledCharge> {
    let per_use: Vec<&&ObservationQuote> = quotes.iter().filter(|q| has_bill(q)).collect();
    let mut charge = BilledCharge {
        sum: Decimal::default(),
        stated: 0,
        silent: 0,
        attempts: per_use.len(),
    };
    for quote in &per_use {
        if let Some(billed) = quote.usage.billed_usd {
            charge.sum = charge.sum.add(billed).ok()?;
            charge.stated += 1;
        } else if quote.usage == codex_state::accounting::Usage::default() {
            charge.silent += 1;
        }
    }
    (charge.stated > 0).then_some(charge)
}

fn billed_figure(quotes: &[&ObservationQuote]) -> Option<String> {
    billed_charge(quotes).map(|charge| charge.text())
}

/// The provider's stated charge as a second figure beside the cost, unless
/// every stated charge here already is the cost (#368: a charge-reporting
/// route's charges price its requests), where it would only repeat it.
fn extra_billed_figure(quotes: &[&ObservationQuote]) -> Option<String> {
    let repeats_cost = quotes
        .iter()
        .filter(|quote| has_bill(quote) && quote.usage.billed_usd.is_some())
        .all(|quote| charge::reported_charge(quote).is_some());
    if repeats_cost {
        None
    } else {
        billed_figure(quotes)
    }
}

fn billed_detail(figure: &str) -> String {
    format!("Billed cost: {figure} — as stated by the provider with each response")
}

/// Whether a provider states what it charged with each response
/// ([`charge::CHARGE_REPORTING`]).
fn states_charges(provider_id: &str) -> bool {
    charge::prices_from_charge(provider_id)
}

/// Whether an attempt can appear on a provider's bill: not subscription work,
/// not a model run on the user's own machine, and not one the provider
/// refused before any response (#475).
fn has_bill(quote: &ObservationQuote) -> bool {
    !quote.is_plan()
        && quote.basis() != codex_state::accounting::Basis::Local
        && quote.usage.refused_status.is_none()
}

/// #475: "1 attempt refused by the provider (HTTP 401) — no charge" for the
/// refused attempts among `quotes`, so a refusal reads as a failure that cost
/// nothing rather than as missing usage.
fn refusal_note(quotes: &[&ObservationQuote]) -> Option<String> {
    let mut statuses: Vec<u16> = quotes
        .iter()
        .filter_map(|quote| quote.usage.refused_status)
        .collect();
    let count = statuses.len();
    if count == 0 {
        return None;
    }
    statuses.sort_unstable();
    statuses.dedup();
    let statuses = statuses
        .iter()
        .map(|status| format!("HTTP {status}"))
        .collect::<Vec<_>>()
        .join(", ");
    Some(format!(
        "{count} {} refused by the provider ({statuses}) — no charge",
        if count == 1 { "attempt" } else { "attempts" }
    ))
}

/// Display names in provider-id order, as the route lines list them.
fn provider_names<'a>(ids: impl IntoIterator<Item = &'a str>) -> Vec<String> {
    let mut ids: Vec<&str> = ids.into_iter().collect();
    ids.sort_unstable();
    ids.dedup();
    let mut names: Vec<String> = Vec::new();
    for id in ids {
        let name = provider_name(id);
        if !names.contains(&name) {
            names.push(name);
        }
    }
    names
}

fn joined(names: &[String]) -> String {
    match names {
        [] => String::new(),
        [one] => one.clone(),
        [rest @ .., last] => format!("{} and {last}", rest.join(", ")),
    }
}

/// Why the pay-per-use work among `quotes` has no stated charge, naming the
/// providers ("Z.AI doesn't report its charges"), and whose bill to check
/// ("Z.AI's bill"). A provider that reports charges but stated none here is
/// not said never to report them. `None` when every attempt stated a charge,
/// or one has no provider id.
fn unstated_charge<'a>(
    quotes: impl IntoIterator<Item = &'a ObservationQuote>,
) -> Option<(String, String)> {
    let mut never = Vec::new();
    let mut silent = Vec::new();
    for quote in quotes {
        if !has_bill(quote) || quote.usage.billed_usd.is_some() {
            continue;
        }
        let id = quote.attempt.provider.as_str();
        if id.trim().is_empty() {
            return None;
        }
        if states_charges(id) {
            silent.push(id);
        } else {
            never.push(id);
        }
    }
    let never = provider_names(never);
    let silent = provider_names(silent);
    let mut clauses = Vec::new();
    if !never.is_empty() {
        clauses.push(if never.len() == 1 {
            format!("{} doesn't report its charges", never[0])
        } else {
            format!("{} don't report their charges", joined(&never))
        });
    }
    if !silent.is_empty() {
        clauses.push(format!(
            "{} stated no charge for this work",
            joined(&silent)
        ));
    }
    let bill = match (never.as_slice(), silent.as_slice()) {
        ([], []) => return None,
        ([one], []) | ([], [one]) => format!("{one}'s bill"),
        ([one], [other]) if one == other => format!("{one}'s bill"),
        _ => "their bills".to_string(),
    };
    Some((clauses.join(" and "), bill))
}

/// A page's one billed-cost line: the provider's own charge where it stated
/// one with its responses, else that any cost shown is only an estimate and
/// whose bill has the real amount. `None` for a page with nothing that can be
/// billed: subscription work is not billed per request, and its pages say so.
fn billed_line(quotes: &[&ObservationQuote]) -> Option<String> {
    if !quotes.iter().any(|quote| has_bill(quote)) {
        return None;
    }
    if let Some(billed) = billed_figure(quotes) {
        return Some(billed_detail(&billed));
    }
    if quotes
        .iter()
        .any(|quote| !quote.is_plan() && quote.usage.billed_usd.is_some())
    {
        return Some("Billed cost: unavailable — exact arithmetic overflow".into());
    }
    // Only refused attempts from providers that report charges: nothing was
    // charged that the provider would have stated.
    if quotes.iter().filter(|quote| has_bill(quote)).all(|quote| {
        states_charges(&quote.attempt.provider)
            && quote.usage == codex_state::accounting::Usage::default()
    }) {
        let names = provider_names(
            quotes
                .iter()
                .filter(|quote| has_bill(quote))
                .map(|quote| quote.attempt.provider.as_str()),
        );
        let bill = match names.as_slice() {
            [one] => format!("{one}'s bill"),
            _ => "their bills".to_string(),
        };
        return Some(format!(
            "Billed cost: none stated — {} reported no usage here; a refused request is normally not charged, but if it stopped mid-response, check {bill}.",
            joined(&names)
        ));
    }
    Some(match unstated_charge(quotes.iter().copied()) {
        Some((clause, bill)) => {
            format!("Billed cost: {clause}, so any cost here is an estimate — check {bill}.")
        }
        None => "Billed cost: not reported, so any cost here is an estimate — check your provider's bill.".into(),
    })
}

/// A short link label figure: the cost, or that a subscription covered it.
fn short_cost(quotes: &[&ObservationQuote]) -> String {
    match codex_state::accounting::DayTotals::from_quotes(quotes.iter().copied()) {
        Ok(t) => match (
            plain_billing(&t, EstimateGaps::of(quotes.iter().copied())),
            leading_charge(quotes, &t),
        ) {
            ((COVERED, _), _) => "covered by subscription".to_string(),
            ((LOCAL, _), _) => "no charge (local)".to_string(),
            ((NOT_DECLARED, _), _) => "billing basis not declared".to_string(),
            (("Mixed", _), None) => "mixed billing bases".to_string(),
            (_, Some(billed)) => format!("billed {}", billed.text()),
            // Only the leading per-use figure is reworded; a subscription part
            // after it keeps its own words.
            ((_, cost), None) => cost
                .replacen(NO_PRICE, "no price available", 1)
                .replacen(NO_USAGE, "usage incomplete", 1)
                .replacen("Estimated cost: not available", "cost not available", 1)
                .replacen("Estimated cost: ", "estimated ", 1)
                .replacen("Cost: ", "", 1),
        },
        Err(_) => "cost unavailable".to_string(),
    }
}

/// The provider's own charge when it is the figure to lead with: every
/// attempt that reported usage stated one, and published prices cannot
/// reproduce it.
fn leading_charge(
    quotes: &[&ObservationQuote],
    t: &codex_state::accounting::DayTotals,
) -> Option<BilledCharge> {
    billed_charge(quotes).filter(|billed| billed.answered_all_stated() && t.per_use_unknown() > 0)
}

/// The plain header of a provider, request or attempt page: what served the
/// work, how it is paid for and what it cost, then a divider above the
/// auditing detail.
fn plain_header(quotes: &[&ObservationQuote], named: bool) -> Vec<String> {
    let mut routes: Vec<(String, String, String)> = Vec::new();
    for quote in quotes {
        let route = (
            provider_name(&quote.attempt.provider),
            quote.attempt.account_name().to_string(),
            model_name(&quote.attempt.provider, &quote.attempt.model),
        );
        if !routes.contains(&route) {
            routes.push(route);
        }
    }
    let mut lines = match routes.as_slice() {
        [(provider, account, model)] => {
            let mut lines = vec![format!("Provider: {provider}")];
            if named {
                lines.push(format!("Account: {account}"));
            }
            lines.push(format!("Model: {model}"));
            lines
        }
        _ => vec![format!(
            "{}: {}",
            if named {
                "Providers, accounts and models"
            } else {
                "Providers and models"
            },
            routes
                .iter()
                .map(|(provider, account, model)| if named {
                    format!("{provider} · account {account} · {model}")
                } else {
                    format!("{provider} · {model}")
                })
                .collect::<Vec<_>>()
                .join(", ")
        )],
    };
    match codex_state::accounting::DayTotals::from_quotes(quotes.iter().copied()) {
        Ok(t) => {
            let (billing, cost) = plain_billing(&t, EstimateGaps::of(quotes.iter().copied()));
            lines.push(if billing == NOT_DECLARED {
                "Billing: not declared".to_string()
            } else {
                format!("Billing: {billing}")
            });
            lines.push(cost);
            lines.extend(charge::price_basis(quotes));
            lines.extend(scope::no_price_next_step(quotes.iter().copied()));
            if let Some(billed) = extra_billed_figure(quotes) {
                lines.push(format!("Billed by provider: {billed}"));
            }
            lines.push(format!("Tokens: {}", plain_tokens(&t)));
            lines.extend(refusal_note(quotes).map(|note| format!("Refused: {note}")));
            lines.extend(scope::overflow_note(quotes.iter().copied()));
        }
        Err(_) => lines.push("Cost unavailable".to_string()),
    }
    lines.push("—— Details ——".to_string());
    lines.extend(billed_line(quotes));
    lines
}

/// "Today (UTC) in this conversation:" for the current UTC day, else the
/// inspected date: `/cost YYYY-MM-DD` opens any past day.
fn day_heading(utc_day: i64, today: i64) -> String {
    if utc_day == today {
        return "Today (UTC) in this conversation:".to_string();
    }
    chrono::DateTime::from_timestamp(utc_day * 86_400, 0).map_or_else(
        || format!("This conversation on UTC day {utc_day}:"),
        |date| format!("This conversation on {} (UTC):", date.date_naive()),
    )
}

/// Attempts grouped by provider, account and model, in a stable order.
fn by_route<'a>(
    quotes: impl IntoIterator<Item = &'a ObservationQuote>,
) -> std::collections::BTreeMap<(String, String, String), Vec<&'a ObservationQuote>> {
    let mut groups: std::collections::BTreeMap<(String, String, String), Vec<&ObservationQuote>> =
        std::collections::BTreeMap::new();
    for quote in quotes {
        groups
            .entry((
                quote.attempt.provider.clone(),
                quote.attempt.account_name().to_string(),
                quote.attempt.model.clone(),
            ))
            .or_default()
            .push(quote);
    }
    groups
}

/// One provider and model on a first screen: how it is paid for, then its
/// requests, tokens and cost.
fn route_line(quotes: &[&ObservationQuote], named: bool) -> String {
    let route = route_name_in(quotes[0], named);
    match codex_state::accounting::DayTotals::from_quotes(quotes.iter().copied()) {
        Ok(t) => {
            let (billing, cost) = plain_billing(&t, EstimateGaps::of(quotes.iter().copied()));
            let provider = provider_name(&quotes[0].attempt.provider);
            let refused = refusal_note(quotes).map_or_else(String::new, |note| format!(" {note}."));
            match leading_charge(quotes, &t).ok_or_else(|| billed_charge(quotes)) {
                // Every request that reported usage carries the provider's own
                // charge, but some figure is still unknown (a counter the
                // estimate needs is missing, or an attempt failed without
                // usage): the charge is the figure, and a partial estimate
                // would only mislead.
                Ok(billed) => format!(
                    "• {route} — {billing}. {}, {}. Billed by {provider}: {}.{refused}",
                    request_count(quotes),
                    plain_tokens(&t),
                    billed.text()
                ),
                Err(billed) => format!(
                    "• {route} — {billing}. {}, {}. {cost}.{}{refused}",
                    request_count(quotes),
                    plain_tokens(&t),
                    billed
                        .filter(|_| extra_billed_figure(quotes).is_some())
                        .map_or_else(String::new, |billed| format!(
                            " Billed by {provider}: {}.",
                            billed.text()
                        ))
                ),
            }
        }
        Err(_) => format!("• {route} — cost unavailable."),
    }
}

/// #314 (D228): said when accounting failed to record at least one request of
/// this conversation on the day (or on a day of the period) shown, so its
/// totals never read as complete. Missing work is never free work.
fn recording_gap_line(span: &str) -> String {
    format!(
        "Totals for {span} may be incomplete: Corbanu failed to record at least one request of this conversation or its subagents, which may still have been billed. The cause is in Corbanu's accounting log; check your provider's bill."
    )
}

/// The first screen: one line per provider and model, stating how it is paid
/// for before anything else, then the totals by billing type. `outside` says
/// what this view leaves out; an empty conversation says so in its heading
/// rather than showing nothing, so it never reads as a zero-cost day.
fn plain_overview<'a>(
    heading: String,
    empty: Option<codex_state::accounting::EmptyDay>,
    named: bool,
    quotes: impl IntoIterator<Item = &'a ObservationQuote>,
    outside: Vec<String>,
    (outside_per_use, outside_plan): (Vec<&ObservationQuote>, bool),
    next_step: Vec<String>,
) -> Vec<String> {
    let groups = by_route(quotes);
    let mut lines = if groups.is_empty() {
        empty_day::overview(&heading, empty)
    } else {
        vec![heading]
    };
    lines.extend(groups.values().map(|quotes| route_line(quotes, named)));
    let all: Vec<&ObservationQuote> = groups.values().flatten().copied().collect();
    lines.extend(reopen_warmup_line(all.iter().copied()));
    lines.extend(realtime_session_line(all.iter().copied()));
    if let Ok(t) = codex_state::accounting::DayTotals::from_quotes(all.iter().copied()) {
        let billed = t.per_use_attempts();
        if billed > 0 && t.plan_attempts > 0 {
            let per_use_totals = codex_state::accounting::DayTotals::from_quotes(
                all.iter().copied().filter(|q| per_use(q)),
            );
            let covered = codex_state::accounting::DayTotals::from_quotes(
                all.iter().copied().filter(|q| q.is_plan()),
            );
            if let (Ok(per_use_totals), Ok(covered)) = (per_use_totals, covered) {
                let billed = extra_billed_figure(&all)
                    .map_or_else(String::new, |billed| format!("; billed: {billed}"));
                lines.push(format!(
                    "Pay-per-use total — {}{billed}",
                    lower_first(
                        &plain_billing(
                            &per_use_totals,
                            EstimateGaps::of(all.iter().copied().filter(|q| per_use(q)))
                        )
                        .1
                    )
                ));
                lines.push(format!(
                    "Subscription work — {}",
                    lower_first(&plain_billing(&covered, EstimateGaps::default()).1)
                ));
            }
        }
    }
    lines.extend(outside);
    lines.extend(next_step);
    // Subscription lines from other conversations on this screen get the note too.
    if outside_plan && !all.iter().any(|quote| quote.is_plan()) {
        lines.push(OVERFLOW_NOTE.to_string());
    } else {
        lines.extend(scope::overflow_note(all.iter().copied()));
    }
    let everywhere: Vec<&ObservationQuote> = all.iter().chain(&outside_per_use).copied().collect();
    lines.push(
        if everywhere
            .iter()
            .any(|quote| charge::reported_charge(quote).is_some())
        {
            charged_costs_note(&everywhere)
        } else if billed_figure(&all).is_some() {
            "Estimates use published prices; billed figures are what the provider stated with each response.".to_string()
        } else if outside_per_use.is_empty() && !all.is_empty() && all.iter().all(|quote| quote.is_plan()) {
            // No bill exists for subscription work; pointing at one would
            // suggest a charge the plan never makes. Pay-per-use figures from
            // other conversations on the screen keep the bill caveat.
            "Subscription work is not billed per request; any figure here is what it would cost at API prices.".to_string()
        } else if outside_per_use.is_empty()
            && !all.is_empty()
            && all.iter().all(|quote| !has_bill(quote))
        {
            // Only subscription and local work: neither has a bill to point
            // at. Undeclared work is not known to be unbilled, so any of it
            // keeps the bill caveat below.
            if all.iter().any(|quote| quote.is_plan()) {
                "Nothing here is billed per request: local work has no charge, and any subscription figure is what it would cost at API prices.".to_string()
            } else {
                "Local work runs on your own machine and has no charge.".to_string()
            }
        } else {
            // This page's one statement that the figures are estimates: it
            // names whose bill has the real amount where it can.
            match unstated_charge(all.iter().chain(&outside_per_use).copied()) {
                Some((clause, bill)) => format!(
                    "Costs are estimates from published prices; {clause} — check {bill}."
                ),
                None => "Costs are estimates from published prices; your provider's bill is the final amount.".to_string(),
            }
        },
    );
    if !all.is_empty() {
        lines.push("Select a provider below to see its requests.".to_string());
    }
    lines.push("—— Details ——".to_string());
    lines
}

/// #368: the first screen's one statement of what its figures are, when some
/// are charges a provider reported: those are costs, and anything else is an
/// estimate whose real amount is on the bill it names.
fn charged_costs_note(quotes: &[&ObservationQuote]) -> String {
    let chargers = charge::Chargers::of(quotes.iter().copied());
    let provider = chargers.names();
    let mut note = if chargers.several() {
        format!(
            "Costs marked \"charged by\" a provider ({provider}) are the charges that provider reported with those responses"
        )
    } else {
        format!(
            "Costs marked \"charged by {provider}\" are the charges {provider} reported with those responses"
        )
    };
    let rest: Vec<&ObservationQuote> = quotes
        .iter()
        .copied()
        .filter(|quote| has_bill(quote) && charge::reported_charge(quote).is_none())
        .collect();
    if !rest.is_empty() {
        note.push_str("; other costs are estimates from published prices");
        if let Some((clause, bill)) = unstated_charge(rest.iter().copied()) {
            note.push_str(&format!("; {clause} — check {bill}"));
        }
        if billed_figure(&rest).is_some() {
            note.push_str("; billed figures are what the provider stated with each response");
        }
    }
    note.push('.');
    note
}

/// A UTC instant in milliseconds as RFC 3339, else the raw count.
fn utc_instant(ms: i64) -> String {
    chrono::DateTime::from_timestamp_millis(ms).map_or_else(
        || format!("{ms} ms"),
        |at| at.to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
    )
}

/// A day counted from the Unix epoch as its UTC date, else the raw count.
fn utc_date(day: i64) -> String {
    chrono::DateTime::from_timestamp(day.saturating_mul(86_400), 0)
        .map_or_else(|| format!("day {day}"), |at| at.date_naive().to_string())
}

/// What the ledger still holds: per-request detail for 90 days of wall-clock
/// time, daily totals from the aggregate floor on.
fn retention(
    read_at_ms: i64,
    aggregate_day_floor: i64,
    oldest_recorded_day: Option<i64>,
) -> String {
    format!(
        "request detail kept since {}; daily totals kept since {}; oldest recorded day in this conversation {}",
        read_at_ms
            .checked_sub(90 * 86_400_000)
            .filter(|n| *n >= 0)
            .map_or_else(|| "the start".to_string(), utc_instant),
        utc_date(aggregate_day_floor),
        oldest_recorded_day.map_or_else(|| "none".to_string(), utc_date)
    )
}

fn attempt_count(n: usize) -> String {
    match n {
        0 => "none".to_string(),
        1 => "1 attempt".to_string(),
        n => format!("{n} attempts"),
    }
}

fn interval(start: i64, end: i64) -> String {
    format!("[{}, {})", utc_instant(start), utc_instant(end))
}

impl Inspector {
    fn params(&self, width: usize) -> SelectionViewParams {
        let page = &self.pages[self.page];
        let generation = self.generation;
        // Selectable wrapped lines keep long fields reachable at the current terminal width.
        let mut items: Vec<SelectionItem> = page
            .text
            .iter()
            .flat_map(|text| {
                let clean: String = text.chars().filter(|c| !c.is_control()).collect();
                textwrap::wrap(&clean, width.saturating_sub(6).max(1))
                    .into_iter()
                    .map(|part| SelectionItem {
                        name: part.into_owned(),
                        ..Default::default()
                    })
                    .collect::<Vec<_>>()
            })
            .collect();
        if !page.links.is_empty() {
            items.push(SelectionItem {
                name: "—— Open ——".into(),
                ..Default::default()
            });
        }
        for (label, target) in &page.links {
            let page = *target;
            items.push(SelectionItem {
                name: label.clone(),
                name_prefix_spans: vec!["→ ".cyan()],
                actions: vec![Box::new(move |tx| {
                    tx.send(AppEvent::NavigateAccountingInspector { generation, page })
                })],
                ..Default::default()
            });
        }
        let back = self.history.last().copied().or(page.parent);
        if let Some(page) = back {
            items.push(SelectionItem {
                name: "Back".into(),
                actions: vec![Box::new(move |tx| {
                    tx.send(AppEvent::NavigateAccountingInspector { generation, page })
                })],
                ..Default::default()
            });
        }
        items.push(SelectionItem {
            name: "Refresh".into(),
            actions: vec![Box::new(move |tx| {
                tx.send(AppEvent::RefreshAccountingInspector { generation })
            })],
            ..Default::default()
        });
        let close_alive = self.alive.clone();
        items.push(SelectionItem {
            name: "Close".into(),
            dismiss_on_select: true,
            actions: vec![Box::new(move |tx| {
                close_alive.store(false, std::sync::atomic::Ordering::Release);
                tx.send(AppEvent::CloseAccountingInspector { generation });
            })],
            ..Default::default()
        });
        let alive = self.alive.clone();
        let parent = back;
        let selected = page.selected.clone();
        SelectionViewParams {
            initial_selected_idx: Some(selected.load(std::sync::atomic::Ordering::Relaxed)),
            on_selection_changed: Some(Box::new(move |index, _| {
                selected.store(index, std::sync::atomic::Ordering::Relaxed);
            })),
            view_id: Some(INSPECTOR_VIEW),
            title: Some(page.title.clone()),
            subtitle: self.range.is_none().then(|| {
                chrono::DateTime::from_timestamp(self.day * 86_400, 0).map_or_else(
                    || format!("Requested UTC day: day {} since the epoch", self.day),
                    |date| format!("Requested UTC day: {}", date.date_naive()),
                )
            }),
            items,
            allow_number_shortcuts: false,
            // The accepted bounds, on screen rather than only in an error.
            // Only a developer-accounting build opens this view, as `/cost`.
            footer_note: Some(Line::from(
                format!(
                    "Other dates: /cost {}",
                    crate::slash_command::COST_ARGUMENT_HINT
                )
                .dim(),
            )),
            footer_hint: Some("↑↓ scroll · Enter open · Esc back/close".into()),
            on_cancel: Some(Box::new(move |tx| {
                if let Some(page) = parent {
                    tx.send(AppEvent::NavigateAccountingInspector { generation, page });
                } else {
                    alive.store(false, std::sync::atomic::Ordering::Release);
                    tx.send(AppEvent::CloseAccountingInspector { generation });
                }
            })),
            ..Default::default()
        }
    }
}

impl ChatWidget {
    pub(super) fn reflow_accounting_inspector(&mut self, width: u16) {
        if let Some(view) = &self.accounting_inspector {
            self.bottom_pane
                .replace_selection_view_if_present(INSPECTOR_VIEW, view.params(usize::from(width)));
        }
    }

    pub(super) fn invalidate_accounting_inspector_for_thread(&mut self) {
        if self
            .accounting_inspector
            .as_ref()
            .is_some_and(|v| v.thread != self.thread_id())
        {
            self.accounting_inspector = None;
            self.bottom_pane.dismiss_view_by_id(INSPECTOR_VIEW);
        }
    }

    pub(super) fn open_accounting_command(&mut self, args: &str, today: NaiveDate) {
        self.open_accounting_command_as("/usage requests", args, today);
    }

    /// `/cost [args]`: the same view, with errors that name `/cost`.
    pub(super) fn open_cost_command(&mut self, args: &str, today: NaiveDate) {
        let args = format!("requests {args}");
        self.open_accounting_command_as("/cost", args.trim_end(), today);
    }

    /// `command` is how the user asked, so a usage error names what they typed.
    fn open_accounting_command_as(&mut self, command: &str, args: &str, today: NaiveDate) {
        let parts: Vec<_> = args.split_whitespace().collect();
        if let ["requests", start, end, grouping] = parts.as_slice() {
            let parsed = (|| -> anyhow::Result<InspectionRange> {
                let parse = |value: &str| -> anyhow::Result<i64> {
                    if value.len() == 10 {
                        let date = NaiveDate::parse_from_str(value, "%Y-%m-%d")?;
                        anyhow::ensure!(date.to_string() == value, "invalid UTC date");
                        return Ok(date
                            .and_hms_opt(0, 0, 0)
                            .ok_or_else(|| anyhow::anyhow!("invalid UTC date"))?
                            .and_utc()
                            .timestamp_millis());
                    }
                    anyhow::ensure!(value.ends_with('Z'), "timestamps must use UTC Z");
                    let time = chrono::DateTime::parse_from_rfc3339(value)?;
                    anyhow::ensure!(
                        time.timestamp_subsec_nanos() < 1_000_000_000
                            && time.timestamp_subsec_nanos() % 1_000_000 == 0,
                        "precision finer than milliseconds is unsupported"
                    );
                    Ok(time.timestamp_millis())
                };
                let range = InspectionRange {
                    start_ms: parse(start)?,
                    end_ms: parse(end)?,
                    grouping: match *grouping {
                        "hour" => InspectionGrouping::Hour,
                        "day" => InspectionGrouping::Day,
                        "week" => InspectionGrouping::Week,
                        "month" => InspectionGrouping::Month,
                        _ => anyhow::bail!("grouping must be hour, day, week or month"),
                    },
                };
                range.validate()?;
                let today_start = today
                    .and_hms_opt(0, 0, 0)
                    .ok_or_else(|| anyhow::anyhow!("invalid UTC date"))?;
                anyhow::ensure!(
                    range.start_ms / 86_400_000 <= today_start.and_utc().timestamp() / 86_400,
                    "future start is unavailable"
                );
                Ok(range)
            })();
            match parsed {
                Ok(range) => self.open_accounting_range(range.start_ms / 86_400_000, Some(range)),
                Err(error) => self.add_error_message(format!(
                    "Range refused: {error}. Usage: {command} START END hour|day|week|month"
                )),
            }
            return;
        }
        let date = match parts.as_slice() {
            ["requests"] => Some(today),
            ["requests", date] if date.len() == 10 => NaiveDate::parse_from_str(date, "%Y-%m-%d")
                .ok()
                .filter(|d| d.to_string() == *date),
            _ => None,
        };
        if let Some(date) = date.filter(|d| *d <= today)
            && let Some(time) = date.and_hms_opt(0, 0, 0)
            && time.and_utc().timestamp() >= 0
        {
            self.open_accounting_inspector(time.and_utc().timestamp() / 86_400);
        } else if let Some(date) = date.filter(|d| *d > today) {
            // Valid syntax: say what is wrong with it, not how to type it.
            self.add_error_message(format!(
                "{date} is after today ({today}, UTC); there is nothing recorded for it yet. Run {command} for today."
            ));
        } else {
            self.add_error_message(
                format!("Usage: {command} [YYYY-MM-DD] (UTC, no future dates). Custom: {command} START END hour|day|week|month; dates or UTC timestamps ending Z; end exclusive."),
            );
        }
    }

    pub(crate) fn open_accounting_inspector(&mut self, day: i64) {
        self.open_accounting_range(day, /*range*/ None);
    }

    fn open_accounting_range(&mut self, day: i64, range: Option<InspectionRange>) {
        let generation = Uuid::new_v4();
        let thread = self.thread_id();
        let inspector = Inspector {
            generation,
            thread,
            day,
            range,
            pages: vec![InspectorPage {
                title: "Cost — this conversation".into(),
                text: vec![match range {
                    Some(r) => format!(
                        "Loading recorded requests… Requested: {}; timezone: UTC; grouping: {:?}; effective coverage and bucket boundaries pending",
                        interval(r.start_ms, r.end_ms),
                        r.grouping
                    ),
                    None => "Loading recorded requests…".into(),
                }],
                links: Vec::new(),
                parent: None,
                selected: Arc::default(),
            }],
            page: 0,
            history: Vec::new(),
            alive: Arc::new(std::sync::atomic::AtomicBool::new(true)),
        };
        let params = inspector.params(self.last_rendered_width.get().unwrap_or(80));
        if !self.bottom_pane.replace_selection_view_if_present(
            INSPECTOR_VIEW,
            inspector.params(self.last_rendered_width.get().unwrap_or(80)),
        ) {
            self.bottom_pane.show_selection_view(params);
        }
        self.accounting_inspector = Some(inspector);
        self.app_event_tx.send(AppEvent::LoadAccountingInspector {
            generation,
            thread,
            day,
            range,
        });
        self.request_redraw();
    }

    pub(crate) fn finish_accounting_inspector(
        &mut self,
        generation: Uuid,
        thread: Option<ThreadId>,
        day: i64,
        result: Result<InspectionDay, String>,
    ) {
        let current_thread = self.thread_id();
        let Some(view) = self.accounting_inspector.as_mut().filter(|v| {
            v.generation == generation
                && v.thread == thread
                && v.day == day
                && v.thread == current_thread
                && v.alive.load(std::sync::atomic::Ordering::Acquire)
        }) else {
            return;
        };
        let is_range = matches!(result, Ok(InspectionDay::Range { .. }));
        view.pages = inspection_pages(result);
        if let Some(range) = view.range
            && !is_range
        {
            view.pages[0].text.insert(1, format!("Requested: {}; timezone: UTC; grouping: {:?}; effective coverage: unavailable; bucket boundaries unavailable", interval(range.start_ms, range.end_ms), range.grouping));
            let scope = if range.end_ms - range.start_ms == 3_600_000 {
                "this UTC hour"
            } else {
                "the selected UTC interval"
            };
            for text in &mut view.pages[0].text {
                if text.starts_with("Logical requests may have attempts") {
                    *text = format!(
                        "Logical requests may have attempts outside the selected interval; {scope} is not their complete lifetime."
                    );
                }
            }
        }
        view.page = 0;
        view.history.clear();
        if !self.bottom_pane.replace_selection_view_if_present(
            INSPECTOR_VIEW,
            view.params(self.last_rendered_width.get().unwrap_or(80)),
        ) {
            self.accounting_inspector = None;
        }
        self.request_redraw();
    }

    pub(crate) fn navigate_accounting_inspector(&mut self, generation: Uuid, page: usize) {
        let thread = self.thread_id();
        let Some(view) = self.accounting_inspector.as_mut().filter(|v| {
            v.thread == thread
                && v.generation == generation
                && v.alive.load(std::sync::atomic::Ordering::Acquire)
        }) else {
            return;
        };
        if page >= view.pages.len() {
            return;
        }
        // Returning to the page we came from unwinds one step; anything else
        // is a step forward.
        if view.history.last() == Some(&page) {
            view.history.pop();
        } else if page != view.page {
            view.history.push(view.page);
        }
        view.page = page;
        if !self.bottom_pane.replace_selection_view_if_present(
            INSPECTOR_VIEW,
            view.params(self.last_rendered_width.get().unwrap_or(80)),
        ) {
            self.bottom_pane
                .show_selection_view(view.params(self.last_rendered_width.get().unwrap_or(80)));
        }
        self.request_redraw();
    }

    pub(crate) fn close_accounting_inspector(&mut self, generation: Uuid) {
        if self
            .accounting_inspector
            .as_ref()
            .is_some_and(|v| v.generation == generation)
        {
            self.accounting_inspector = None;
            self.bottom_pane.dismiss_view_by_id(INSPECTOR_VIEW);
        }
    }

    pub(crate) fn refresh_accounting_inspector(&mut self, generation: Uuid) {
        if let Some(view) = &self.accounting_inspector
            && view.generation == generation
            && view.alive.load(std::sync::atomic::Ordering::Acquire)
        {
            self.open_accounting_range(view.day, view.range);
        }
    }
}

#[cfg(test)]
#[path = "tokens_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "tokens_checkpoint_tests.rs"]
mod checkpoint_tests;

#[cfg(test)]
#[path = "tokens/code_review_tests.rs"]
mod code_review_tests;
