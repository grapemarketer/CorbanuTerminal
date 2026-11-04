use super::*;
use codex_app_server_protocol::ThreadTokenUsageUpdatedNotification;
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
        report("root", 100, 100),
        report("child", 200, 40),
        report("unrelated", 1000, 1000),
        report("root", 300, 200),
        report("grandchild", 80, 40),
        report("child", 400, 200),
        report("child", 400, 200),
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
    usage.observe(&report("root", 0, 0));
    assert_eq!(usage.for_thread("root"), None);
    usage.observe(&report("child", 0, 0));
    assert_eq!(usage.for_thread("root"), Some(Usage::default()));
    usage.invalidate();
    assert_eq!(usage.for_thread("root"), None);
}
