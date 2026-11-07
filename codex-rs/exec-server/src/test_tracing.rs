//! Test-only helpers for asserting on spans from a thread-scoped subscriber.

use tracing::Dispatch;
use tracing::Subscriber;
use tracing::subscriber::DefaultGuard;
use tracing::subscriber::NoSubscriber;

/// Thread-scoped test subscriber whose spans cannot be disabled by concurrent tests.
///
/// tracing-core caches every callsite's interest process-wide. While exactly one
/// dispatcher is registered, it computes that interest from the default
/// subscriber of whichever thread hits a callsite first (or rebuilds the
/// cache). A concurrent test thread without a subscriber can then cache
/// `Interest::never` for a shared callsite such as `codex.exec_server.request`,
/// silently disabling that span for this test (#1413). Registering a second,
/// inert dispatcher for the guard's lifetime makes tracing-core consult every
/// live dispatcher, including this one, whenever it computes interest.
///
/// A first registration that another thread already started before this
/// guard registered can still finish afterwards with its earlier result; that
/// window is limited to a callsite's first use and is microseconds wide.
pub(crate) struct ScopedTestSubscriber {
    _default: DefaultGuard,
    _second_dispatcher: Dispatch,
}

impl ScopedTestSubscriber {
    pub(crate) fn set_default<S>(subscriber: S) -> Self
    where
        S: Subscriber + Send + Sync + 'static,
    {
        let second_dispatcher = Dispatch::new(NoSubscriber::default());
        let default = tracing::subscriber::set_default(subscriber);
        tracing::callsite::rebuild_interest_cache();
        Self {
            _default: default,
            _second_dispatcher: second_dispatcher,
        }
    }
}

#[cfg(test)]
#[path = "test_tracing_tests.rs"]
mod tests;
