use super::ScopedTestSubscriber;

fn shared_span_enabled() -> bool {
    !tracing::info_span!("scoped-test-subscriber-shared").is_disabled()
}

#[test]
fn span_stays_enabled_when_another_thread_registers_callsite_first() {
    let _subscriber = ScopedTestSubscriber::set_default(tracing_subscriber::registry());

    // Like a concurrent test without a subscriber, this thread hits the
    // callsite first and so registers it.
    std::thread::spawn(shared_span_enabled)
        .join()
        .expect("other thread should finish");

    assert!(shared_span_enabled());
}
