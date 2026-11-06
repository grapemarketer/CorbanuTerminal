use codex_app_server_protocol::ThreadTokenUsage;
use codex_app_server_protocol::TokenUsageBreakdown;
use codex_protocol::protocol::TokenUsage;
use codex_protocol::protocol::TokenUsageInfo;
use pretty_assertions::assert_eq;

use super::RuntimeState;
use super::StatusTokenUsage;
use super::runtime_state;
use super::thousands;
use super::token_usage_text;

fn breakdown(total: i64, input: i64, output: i64) -> TokenUsageBreakdown {
    TokenUsageBreakdown {
        total_tokens: total,
        input_tokens: input,
        cached_input_tokens: 0,
        cache_write_input_tokens: 0,
        output_tokens: output,
        reasoning_output_tokens: 0,
    }
}

#[test]
fn status_priority_preserves_actionable_state() {
    assert_eq!(
        runtime_state(
            /*approvals*/ 1, /*active_turn*/ true, /*queued*/ 1,
            /*has_error*/ true
        ),
        RuntimeState::AwaitingApproval
    );
    assert_eq!(
        runtime_state(
            /*approvals*/ 0, /*active_turn*/ true, /*queued*/ 1,
            /*has_error*/ true
        ),
        RuntimeState::WorkingQueued
    );
    assert_eq!(
        runtime_state(
            /*approvals*/ 0, /*active_turn*/ true, /*queued*/ 0,
            /*has_error*/ true
        ),
        RuntimeState::Working
    );
    assert_eq!(
        runtime_state(
            /*approvals*/ 0, /*active_turn*/ false, /*queued*/ 1,
            /*has_error*/ true
        ),
        RuntimeState::Blocked
    );
    assert_eq!(
        runtime_state(
            /*approvals*/ 0, /*active_turn*/ false, /*queued*/ 1,
            /*has_error*/ false
        ),
        RuntimeState::Recovering
    );
    assert_eq!(
        runtime_state(
            /*approvals*/ 0, /*active_turn*/ false, /*queued*/ 0,
            /*has_error*/ true
        ),
        RuntimeState::Blocked
    );
    assert_eq!(
        runtime_state(
            /*approvals*/ 0, /*active_turn*/ false, /*queued*/ 0,
            /*has_error*/ false
        ),
        RuntimeState::Idle
    );
}

#[test]
fn every_status_has_a_concrete_next_action() {
    for state in [
        RuntimeState::Idle,
        RuntimeState::Working,
        RuntimeState::WorkingQueued,
        RuntimeState::AwaitingApproval,
        RuntimeState::Recovering,
        RuntimeState::Blocked,
    ] {
        assert!(!state.next_action().is_empty());
    }
}

#[test]
fn thousands_groups_every_magnitude() {
    assert_eq!(thousands(/*value*/ 0), "0");
    assert_eq!(thousands(/*value*/ 999), "999");
    assert_eq!(thousands(/*value*/ 1_000), "1,000");
    assert_eq!(thousands(/*value*/ 178_010), "178,010");
    assert_eq!(thousands(/*value*/ 683_203_904), "683,203,904");
}

/// The reported thread outgrew every signal the chat surfaced: it ran for two
/// weeks on a single thread and its cost was invisible because `/status` showed
/// state and model only. Both numbers must appear, and the cumulative one is the
/// point -- window occupancy alone stays flat while the bill does not.
#[test]
fn status_reports_window_occupancy_and_the_unbounded_thread_total() {
    let usage = ThreadTokenUsage {
        total: breakdown(
            /*total*/ 684_885_374,
            /*input*/ 683_203_904,
            /*output*/ 1_681_470,
        ),
        last: breakdown(
            /*total*/ 178_010, /*input*/ 177_000, /*output*/ 1_010,
        ),
        model_context_window: Some(353_400),
    };

    let text = token_usage_text(&StatusTokenUsage::apply(
        /*previous*/ None, THREAD, usage,
    ));

    assert!(
        text.contains("Context: 178,010 of 353,400 tokens (50%)"),
        "{text}"
    );
    assert!(
        text.contains("Thread total: 684,885,374 tokens (683,203,904 in, 1,681,470 out)"),
        "{text}"
    );
    assert!(text.contains("/new"), "{text}");
}

#[test]
fn status_omits_the_percentage_when_the_window_is_unknown() {
    let usage = ThreadTokenUsage {
        total: breakdown(
            /*total*/ 1_500, /*input*/ 1_200, /*output*/ 300,
        ),
        last: breakdown(
            /*total*/ 1_500, /*input*/ 1_200, /*output*/ 300,
        ),
        model_context_window: None,
    };

    let text = token_usage_text(&StatusTokenUsage::apply(
        /*previous*/ None, THREAD, usage,
    ));

    assert!(text.contains("Context: 1,500 tokens"), "{text}");
    assert!(!text.contains('%'), "{text}");
}

/// A window reported smaller than current usage must not render above 100%.
#[test]
fn status_clamps_an_overfull_window() {
    let usage = ThreadTokenUsage {
        total: breakdown(
            /*total*/ 400_000, /*input*/ 399_000, /*output*/ 1_000,
        ),
        last: breakdown(
            /*total*/ 400_000, /*input*/ 399_000, /*output*/ 1_000,
        ),
        model_context_window: Some(353_400),
    };

    assert!(
        token_usage_text(&StatusTokenUsage::apply(
            /*previous*/ None, THREAD, usage
        ))
        .contains("(100%)")
    );
}

const WINDOW: i64 = 353_400;
const THREAD: &str = "thread-1";

fn provider_report(input: i64, output: i64) -> TokenUsage {
    TokenUsage {
        input_tokens: input,
        cached_input_tokens: 0,
        cache_write_input_tokens: 0,
        output_tokens: output,
        reasoning_output_tokens: 0,
        total_tokens: input + output,
    }
}

/// The info `recompute_token_usage` (core/src/session/mod.rs) sends after
/// compaction or rollback: the stored totals unchanged, `last` replaced by zero
/// counters plus the history estimate.
fn recomputed(info: Option<TokenUsageInfo>, estimate: i64) -> ThreadTokenUsage {
    let mut info = info.unwrap_or(TokenUsageInfo {
        total_token_usage: TokenUsage::default(),
        last_token_usage: TokenUsage::default(),
        model_context_window: None,
    });
    info.last_token_usage = TokenUsage {
        total_tokens: estimate,
        ..TokenUsage::default()
    };
    info.model_context_window = Some(WINDOW);
    info.into()
}

/// A thread whose provider reported 684,885,374 tokens, the last turn 178,010.
fn reported_thread() -> TokenUsageInfo {
    let mut info = TokenUsageInfo::new_or_append(
        /*info*/ &None,
        &Some(provider_report(
            /*input*/ 683_203_904,
            /*output*/ 1_681_470,
        )),
        Some(WINDOW),
    )
    .expect("a report creates usage info");
    info.last_token_usage = provider_report(/*input*/ 177_000, /*output*/ 1_010);
    info
}

fn reported_total() -> Option<TokenUsageBreakdown> {
    Some(breakdown(
        /*total*/ 684_885_374,
        /*input*/ 683_203_904,
        /*output*/ 1_681_470,
    ))
}

/// What `/status` shows: context occupancy, window, thread total.
fn shown(usage: &StatusTokenUsage) -> (i64, Option<i64>, Option<TokenUsageBreakdown>) {
    (
        usage.context_tokens,
        usage.model_context_window,
        usage.thread_total.clone(),
    )
}

/// Folds a sequence of core updates for one thread, as the bridge does.
fn fold(updates: Vec<ThreadTokenUsage>) -> StatusTokenUsage {
    let mut usage = None;
    for update in updates {
        usage = Some(StatusTokenUsage::apply(usage, THREAD, update));
    }
    usage.expect("at least one update")
}

/// #1570: after compaction or rollback on a thread whose provider never reported
/// usage, `/status` claimed a measured, free thread ("0 tokens (0 in, 0 out)").
#[test]
fn issue1570_recompute_on_thread_without_reports_is_not_a_zero_total() {
    let usage = fold(vec![recomputed(
        /*info*/ None, /*estimate*/ 12_000,
    )]);

    assert_eq!(shown(&usage), (12_000, Some(WINDOW), None));
    let text = token_usage_text(&usage);
    assert!(
        text.contains(
            "Context: 12,000 of 353,400 tokens (3%)\nThread total: not reported by the provider"
        ),
        "{text}"
    );
}

/// #1570 on current main (#1583): `fill_to_context_window` keeps the provider
/// counters and sets only `total.total_tokens` to the window. `/status` shows the
/// full window as `Context` and never as the thread's cost: the token figure
/// comes from the counters.
#[test]
fn issue1570_context_window_fill_is_never_the_thread_total() {
    let mut info = reported_thread();
    let before = info.clone();
    info.fill_to_context_window(WINDOW);
    assert_eq!(info.total_token_usage.total_tokens, WINDOW);
    let usage = fold(vec![before.into(), info.into()]);

    assert_eq!(shown(&usage), (WINDOW, Some(WINDOW), reported_total()));
    let text = token_usage_text(&usage);
    assert!(
        text.contains(
            "Context: 353,400 of 353,400 tokens (100%)\nThread total: 684,885,374 tokens (683,203,904 in, 1,681,470 out)"
        ),
        "{text}"
    );
}

/// Review of #1586: after a fill, compaction recomputes. Core leaves `total` at
/// the window and puts the smaller estimate in `last`; `/status` must show the
/// estimate (core measures occupancy from `last`), not 100%.
#[test]
fn fill_then_recompute_shows_the_estimate_not_a_full_window() {
    let mut info = reported_thread();
    let before = info.clone();
    info.fill_to_context_window(WINDOW);
    let filled = info.clone();
    let usage = fold(vec![
        before.into(),
        filled.into(),
        recomputed(Some(info), /*estimate*/ 40_000),
    ]);

    assert_eq!(shown(&usage), (40_000, Some(WINDOW), reported_total()));
    let text = token_usage_text(&usage);
    assert!(
        text.contains("Context: 40,000 of 353,400 tokens (11%)"),
        "{text}"
    );
}

/// Review of #1586: after a fill the next report adds to the window in
/// `total.total_tokens`; the thread total still comes from the counters.
#[test]
fn fill_then_report_shows_the_report_and_the_counter_total() {
    let mut info = reported_thread();
    let before = info.clone();
    info.fill_to_context_window(WINDOW);
    let filled = info.clone();
    info.append_last_usage(&provider_report(/*input*/ 50_000, /*output*/ 500));
    assert_eq!(info.total_token_usage.total_tokens, WINDOW + 50_500);
    let usage = fold(vec![before.into(), filled.into(), info.into()]);

    assert_eq!(
        shown(&usage),
        (
            50_500,
            Some(WINDOW),
            Some(breakdown(
                /*total*/ 684_935_874,
                /*input*/ 683_253_904,
                /*output*/ 1_681_970,
            )),
        )
    );
}

/// #1570: the window fills before any provider report: full context, unknown total.
#[test]
fn issue1570_context_window_fill_without_reports_is_not_a_total() {
    let usage = fold(vec![TokenUsageInfo::full_context_window(WINDOW).into()]);

    assert_eq!(shown(&usage), (WINDOW, Some(WINDOW), None));
}

/// Review of #1586: a new thread (`/new`, or a provider that never reports) does
/// not inherit the previous thread's total.
#[test]
fn a_new_thread_does_not_inherit_the_previous_thread_total() {
    let old = fold(vec![reported_thread().into()]);
    assert_eq!(old.thread_total, reported_total());
    let usage = StatusTokenUsage::apply(
        Some(old),
        "thread-2",
        recomputed(/*info*/ None, /*estimate*/ 12_000),
    );

    assert_eq!(shown(&usage), (12_000, Some(WINDOW), None));
}

/// Guard: on a thread that did report, a recompute keeps the reported total and
/// moves `Context` to the estimate (compaction shrank the history).
#[test]
fn recompute_after_reports_keeps_total_and_shows_the_estimate() {
    let info = reported_thread();
    let usage = fold(vec![
        info.clone().into(),
        recomputed(Some(info), /*estimate*/ 40_000),
    ]);

    assert_eq!(shown(&usage), (40_000, Some(WINDOW), reported_total()));
}

/// Guard: a provider's measured zero is a report, not unknown usage.
#[test]
fn provider_reported_zero_usage_is_still_shown_as_zero() {
    let zero = breakdown(/*total*/ 0, /*input*/ 0, /*output*/ 0);
    let usage = fold(vec![ThreadTokenUsage {
        total: zero.clone(),
        last: zero.clone(),
        model_context_window: Some(WINDOW),
    }]);

    assert_eq!(usage.thread_total, Some(zero));
    let text = token_usage_text(&usage);
    assert!(
        text.contains("Thread total: 0 tokens (0 in, 0 out)"),
        "{text}"
    );
}

/// Corner case shared with exec (#1576): a provider that sends only
/// `total_tokens` reports no counters, so no thread total; its `last` is still
/// the context, and a growing total that is not the window is not a fill.
#[test]
fn a_total_only_report_is_context_but_not_a_thread_total() {
    let only_total = |total: i64, last: i64| ThreadTokenUsage {
        total: breakdown(total, /*input*/ 0, /*output*/ 0),
        last: breakdown(last, /*input*/ 0, /*output*/ 0),
        model_context_window: Some(WINDOW),
    };
    let usage = fold(vec![only_total(30_000, 30_000), only_total(55_000, 25_000)]);

    assert_eq!(shown(&usage), (25_000, Some(WINDOW), None));
}

/// Review of #1586: a second fill while the window is already full adds
/// nothing (`last` is 0); the window stays full.
#[test]
fn a_repeated_fill_keeps_the_window_full() {
    let mut info = reported_thread();
    let before = info.clone();
    info.fill_to_context_window(WINDOW);
    let first = info.clone();
    info.fill_to_context_window(WINDOW);
    assert_eq!(info.last_token_usage.total_tokens, 0);
    let usage = fold(vec![before.into(), first.into(), info.into()]);

    assert_eq!(shown(&usage), (WINDOW, Some(WINDOW), reported_total()));
}

/// Review of #1586: without the previous update (a bridge that started after the
/// fill), a recompute's estimate is shown, never a full window.
#[test]
fn a_recompute_after_a_fill_without_history_shows_the_estimate() {
    let mut info = reported_thread();
    info.fill_to_context_window(WINDOW);
    let usage = fold(vec![recomputed(Some(info), /*estimate*/ 40_000)]);

    assert_eq!(shown(&usage), (40_000, Some(WINDOW), reported_total()));
}

/// Review of #1586: core re-sends unchanged usage when rate limits update
/// (`update_rate_limits`); a re-sent fill must not look like a recompute that
/// added nothing.
#[test]
fn a_resent_update_changes_nothing() {
    let mut info = TokenUsageInfo::new_or_append(
        /*info*/ &None,
        &Some(provider_report(
            /*input*/ 99_000, /*output*/ 1_000,
        )),
        Some(WINDOW),
    )
    .expect("a report creates usage info");
    let before = info.clone();
    info.fill_to_context_window(WINDOW);
    assert_eq!(info.last_token_usage.total_tokens, WINDOW - 100_000);
    let filled: ThreadTokenUsage = info.into();
    let once = fold(vec![before.clone().into(), filled.clone()]);
    let twice = fold(vec![before.into(), filled.clone(), filled]);

    assert_eq!(shown(&once).0, WINDOW);
    assert_eq!(once, twice);
}
