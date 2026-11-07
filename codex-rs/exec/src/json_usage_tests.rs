use super::*;
use codex_app_server_protocol::ThreadTokenUsageUpdatedNotification;
use codex_app_server_protocol::TokenUsageBreakdown;
use pretty_assertions::assert_eq;

fn counters(input: i64) -> TokenUsageBreakdown {
    TokenUsageBreakdown {
        input_tokens: input,
        cached_input_tokens: input / 10,
        cache_write_input_tokens: input / 20,
        output_tokens: input / 2,
        reasoning_output_tokens: input / 4,
        total_tokens: input + input / 2,
    }
}

fn report(thread: &str, total: i64, last: i64) -> ServerNotification {
    ServerNotification::ThreadTokenUsageUpdated(ThreadTokenUsageUpdatedNotification {
        thread_id: thread.to_string(),
        turn_id: format!("{thread}-turn"),
        token_usage: ThreadTokenUsage {
            total: counters(total),
            last: counters(last),
            model_context_window: None,
        },
        account_usage: None,
    })
}

#[test]
fn descendants_use_cumulative_deltas_without_inherited_or_duplicate_usage() {
    let mut usage = JsonUsage::default();
    // Reports can arrive before ancestry, or interleave with unrelated threads.
    for notification in [
        report("root", /*total*/ 100, /*last*/ 100),
        report("child", /*total*/ 200, /*last*/ 40),
        report("unrelated", /*total*/ 1000, /*last*/ 1000),
        report("root", /*total*/ 300, /*last*/ 200),
        report("grandchild", /*total*/ 80, /*last*/ 40),
        report("child", /*total*/ 400, /*last*/ 200),
        report("child", /*total*/ 400, /*last*/ 200),
    ] {
        usage.observe(&notification);
    }
    usage.register_thread("child", "root");
    usage.register_thread("child", "root");
    usage.register_thread("grandchild", "child");
    // Defensive cycle handling must not count any participant twice.
    usage.register_thread("child", "grandchild");
    assert_eq!(
        usage.for_thread("root"),
        Some(Usage {
            input_tokens: 580,
            cached_input_tokens: 58,
            cache_write_input_tokens: 29,
            output_tokens: 290,
            reasoning_output_tokens: 145,
        })
    );
}

#[test]
fn missing_participant_is_unknown_but_explicit_zero_is_measured() {
    let mut usage = JsonUsage::default();
    usage.register_thread("child", "root");
    assert_eq!(usage.for_thread("root"), None);
    usage.observe(&report("root", /*total*/ 0, /*last*/ 0));
    assert_eq!(usage.for_thread("root"), None);
    usage.observe(&report("child", /*total*/ 0, /*last*/ 0));
    assert_eq!(usage.for_thread("root"), Some(Usage::default()));
    usage.invalidate();
    assert_eq!(usage.for_thread("root"), None);
}

#[test]
fn seeded_fork_update_before_live_usage_keeps_total_unknown() {
    let mut usage = JsonUsage::default();
    usage.register_thread("child", "root");
    usage.mark_forked("child");
    usage.observe(&report("root", /*total*/ 1000, /*last*/ 200));
    // Rate-limit/account events can replay this exact inherited token info.
    let seeded = report("child", /*total*/ 1000, /*last*/ 200);
    let ServerNotification::ThreadTokenUsageUpdated(update) = &seeded else {
        unreachable!()
    };
    assert!(update.token_usage.total.input_tokens > update.token_usage.last.input_tokens);
    usage.observe(&seeded);
    assert_eq!(usage.for_thread("root"), None);
    usage.observe(&report("child", /*total*/ 1300, /*last*/ 300));
    // The old inference returned 1,500: it counted the parent's last 200 twice.
    assert_eq!(usage.for_thread("root"), None);
}

#[test]
fn baseline_overflow_is_unknown_instead_of_panicking() {
    let mut usage = JsonUsage::default();
    usage.register_thread("child", "root");
    usage.observe(&report("root", /*total*/ 100, /*last*/ 100));
    let mut notification = report("child", /*total*/ 0, /*last*/ 0);
    let ServerNotification::ThreadTokenUsageUpdated(update) = &mut notification else {
        unreachable!()
    };
    update.token_usage.total.input_tokens = i64::MIN;
    update.token_usage.last.input_tokens = 1;
    usage.observe(&notification);
    assert_eq!(usage.for_thread("root"), None);
}

/// #1637: the root counts from zero, so its first report needs no inherited
/// prefix. A resumed root whose first report is a compaction estimate on top of
/// its stored counters (`last` is zero counters plus an estimate larger than
/// `total.total_tokens`, as `recompute_token_usage` sends) keeps a known total,
/// and so does a descendant whose first report only has `total_tokens` below
/// `last` (`Usage` does not export it).
#[test]
fn root_first_report_and_total_tokens_do_not_need_a_baseline() {
    let mut usage = JsonUsage::default();
    usage.register_thread("child", "root");
    let mut root = report("root", /*total*/ 100, /*last*/ 0);
    let ServerNotification::ThreadTokenUsageUpdated(update) = &mut root else {
        unreachable!()
    };
    update.token_usage.last.total_tokens = 5_000;
    let mut child = report("child", /*total*/ 40, /*last*/ 40);
    let ServerNotification::ThreadTokenUsageUpdated(update) = &mut child else {
        unreachable!()
    };
    update.token_usage.last.total_tokens = update.token_usage.total.total_tokens + 1;
    usage.observe(&root);
    usage.observe(&child);
    assert_eq!(
        usage.for_thread("root"),
        Some(Usage {
            input_tokens: 140,
            cached_input_tokens: 14,
            cache_write_input_tokens: 7,
            output_tokens: 70,
            reasoning_output_tokens: 35,
        })
    );
}

#[test]
fn child_counter_below_baseline_makes_usage_unknown() {
    let counters: [fn(&mut TokenUsageBreakdown) -> &mut i64; 5] = [
        |usage| &mut usage.input_tokens,
        |usage| &mut usage.cached_input_tokens,
        |usage| &mut usage.cache_write_input_tokens,
        |usage| &mut usage.output_tokens,
        |usage| &mut usage.reasoning_output_tokens,
    ];
    for counter in counters {
        let mut usage = JsonUsage::default();
        usage.register_thread("child", "root");
        usage.observe(&report("root", /*total*/ 1000, /*last*/ 200));
        usage.observe(&report("child", /*total*/ 200, /*last*/ 40));
        let mut lower = report("child", /*total*/ 400, /*last*/ 200);
        let ServerNotification::ThreadTokenUsageUpdated(update) = &mut lower else {
            unreachable!()
        };
        *counter(&mut update.token_usage.total) = 0;
        usage.observe(&lower);
        assert_eq!(usage.for_thread("root"), None);
    }
}
