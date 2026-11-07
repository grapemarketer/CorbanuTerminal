//! Realtime sessions on standalone WebSocket or existing WebRTC attempts.
//!
//! A WebSocket session is admitted when it opens; WebRTC retains the attempt
//! already admitted for HTTP creation. A WebSocket session's attempt then
//! records the token counts each finished response reported (#889 D223), as
//! running sums, and finally the session's end, which states only its
//! timestamp. No rate prices a session, so its cost stays unknown, never zero.
use super::Sampling;
use super::responses::DeferredResponsesSampling;
use codex_model_provider_info::ModelProviderInfo;
use codex_protocol::auth::AuthMode;
use codex_protocol::protocol::RealtimeResponseUsage;
use codex_state::accounting::Attempt;
use codex_state::accounting::Count;
use codex_state::accounting::Patch;
use codex_state::accounting::Presence;
use std::sync::Arc;
use uuid::Uuid;

pub(crate) struct RealtimeSessionRecord {
    sampling: Arc<Sampling>,
    attempt: Attempt,
    source: Uuid,
    /// The next observation's position: each usage report, then the end.
    next: i64,
    usage: SessionUsage,
    /// A usage write failed: nothing more is recorded for this session.
    stopped: bool,
}

impl RealtimeSessionRecord {
    /// Admit the session that opened at `url` (`route` under the approved
    /// endpoint) on `deferred`, with resolved `auth_mode`. `None` when it goes
    /// unrecorded: a route or credential the accounting does not approve, or
    /// a ledger failure, each logged as a gap. Never refuses the session.
    ///
    /// The attempt binds no rate (#889 D223): its tokens are audio and text
    /// at realtime rates no catalogue row states, so even a configured model
    /// that has a catalogue row leaves the session's cost unknown.
    pub(crate) async fn begin(
        deferred: &DeferredResponsesSampling,
        provider: &ModelProviderInfo,
        auth_mode: Option<AuthMode>,
        url: &str,
        route: &str,
        model: &str,
    ) -> Option<Self> {
        let sampling = deferred
            .resolve_path(provider, auth_mode, url, route)
            .await
            .ok()
            .flatten()?;
        match sampling.admit_without_rates(model, url).await {
            Ok(attempt) => Some(Self::for_attempt(sampling, attempt)),
            Err(error) => {
                sampling.reject();
                super::gap("admit realtime session", format_args!("{error:#}"));
                None
            }
        }
    }

    /// Observe a session on an already admitted HTTP call, without counting
    /// another request. A separate source keeps its observations distinct
    /// from any made while creating that call.
    pub(super) fn for_attempt(sampling: Arc<Sampling>, attempt: Attempt) -> Self {
        Self {
            sampling,
            attempt,
            source: Uuid::new_v4(),
            next: 1,
            usage: SessionUsage::default(),
            stopped: false,
        }
    }

    /// Record the tokens one finished response reported (#889 D223): the
    /// session's running sums, as one cumulative observation. A report the
    /// sums cannot take is left out, logged as a gap (`SessionUsage::add`). A
    /// write the ledger refuses closes the sampling, logged as a gap: nothing
    /// more is recorded for the session, its end included.
    pub(crate) async fn observe_response(&mut self, usage: &RealtimeResponseUsage) {
        if self.stopped {
            return;
        }
        let Some(patch) = self.usage.add(usage) else {
            return;
        };
        let position = self.next;
        self.next += 1;
        if let Err(error) = self
            .sampling
            .observe_patch(&self.attempt, self.source, position, patch)
            .await
        {
            self.stopped = true;
            self.sampling.reject();
            super::gap("record realtime session usage", format_args!("{error:#}"));
        }
    }

    /// Record that the session ended at `ended_at_ms` (Unix ms), after every
    /// usage report; nothing, logged as a gap, once a usage write was refused
    /// (`observe_response`). The write waits only while the state DB lock is held by
    /// another connection; a busy DB defers it and the close goes ahead.
    pub(crate) async fn end(self, ended_at_ms: i64) {
        let ended = match ended_at_ms.try_into() {
            Ok(ended) => ended,
            Err(error) => {
                super::gap("record realtime session end", error);
                return;
            }
        };
        let patch = Patch {
            session_ended_at_ms: Some(ended),
            ..Patch::default()
        };
        if let Err(error) = self
            .sampling
            .observe_patch(&self.attempt, self.source, self.next, patch)
            .await
        {
            self.sampling.reject();
            super::gap("record realtime session end", format_args!("{error:#}"));
        }
    }
}

/// Running sums of the token counts a session's responses reported
/// (#889 D223), in the order input, cached input, output.
///
/// The first response that reports any of them fixes which ones the session
/// states. A later response that leaves one of them out, or whose cached
/// input exceeds its input, is not counted (logged as a gap), so every
/// recorded sum covers the same responses. The total is input plus output,
/// recorded when both are stated: the ledger holds no other total for this
/// arithmetic, so a provider total that disagrees cannot drop a response.
#[derive(Debug, Default)]
struct SessionUsage {
    stated: Option<[bool; 3]>,
    sums: [i64; 3],
}

impl SessionUsage {
    /// Add one response's report; the cumulative patch to record, or `None`
    /// when this report changes nothing that is recorded.
    fn add(&mut self, usage: &RealtimeResponseUsage) -> Option<Patch> {
        let not_counted = |why: &str| {
            super::gap(
                "record realtime response usage",
                format_args!("{why}; this response is not counted"),
            );
            None
        };
        let as_count = |value: Option<u64>| value.map(i64::try_from).transpose().ok();
        let (Some(input), Some(cached), Some(output)) = (
            as_count(usage.input_tokens),
            as_count(usage.cached_input_tokens),
            as_count(usage.output_tokens),
        ) else {
            return not_counted("a count is beyond the ledger's range");
        };
        if cached
            .zip(input)
            .is_some_and(|(cached, input)| cached > input)
        {
            return not_counted("cached input above input");
        }
        let counts = [input, cached, output];
        let stated = match self.stated {
            Some(stated) => stated,
            None if counts.iter().any(Option::is_some) => counts.map(|count| count.is_some()),
            // A report of a total alone: nothing to sum yet.
            None => return None,
        };
        let mut sums = self.sums;
        for ((sum, count), stated) in sums.iter_mut().zip(counts).zip(stated) {
            if !stated {
                continue;
            }
            let Some(count) = count else {
                return not_counted("a count earlier responses stated is missing");
            };
            let Some(next) = sum.checked_add(count) else {
                return not_counted("the session's sum overflows");
            };
            *sum = next;
        }
        let total = if stated[0] && stated[2] {
            let Some(total) = sums[0].checked_add(sums[2]) else {
                return not_counted("the session's total overflows");
            };
            Some(total)
        } else {
            None
        };
        self.stated = Some(stated);
        self.sums = sums;
        let presence = |stated: bool, sum: Option<i64>| match sum {
            Some(sum) if stated => Count::try_from(sum).map_or(Presence::Missing, Presence::Number),
            _ => Presence::Missing,
        };
        Some(Patch {
            input: presence(stated[0], Some(sums[0])),
            read: presence(stated[1], Some(sums[1])),
            output: presence(stated[2], Some(sums[2])),
            total: presence(/*stated*/ true, total),
            ..Patch::default()
        })
    }
}

#[cfg(test)]
#[path = "accounting_realtime_tests.rs"]
mod tests;
