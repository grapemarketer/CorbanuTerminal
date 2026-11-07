use super::SessionUsage;
use codex_protocol::protocol::RealtimeResponseUsage;
use codex_state::accounting::Patch;
use codex_state::accounting::Presence;
use pretty_assertions::assert_eq;

/// Input, cached input, output and total, as a response reports them.
fn usage([input, cached, output, total]: [Option<u64>; 4]) -> RealtimeResponseUsage {
    RealtimeResponseUsage {
        input_tokens: input,
        cached_input_tokens: cached,
        output_tokens: output,
        total_tokens: total,
    }
}

/// Input, cached input, output and total, as a patch states them.
fn counts(patch: &Patch) -> [Option<i64>; 4] {
    [patch.input, patch.read, patch.output, patch.total].map(|presence| match presence {
        Presence::Number(count) => Some(i64::from(count)),
        Presence::Missing => None,
        Presence::Null => panic!("a realtime patch never states null"),
    })
}

/// #889 D223: each response's report adds to the session's running sums,
/// recorded as one cumulative patch that states tokens and nothing else.
#[test]
fn realtime_usage_sums_each_reported_response() {
    let mut session = SessionUsage::default();
    let first = session
        .add(&usage([Some(132), Some(64), Some(121), Some(253)]))
        .expect("first report");
    assert_eq!(counts(&first), [Some(132), Some(64), Some(121), Some(253)]);
    let second = session
        .add(&usage([Some(200), Some(150), Some(30), Some(230)]))
        .expect("second report");
    assert_eq!(
        counts(&second),
        [Some(332), Some(214), Some(151), Some(483)]
    );
    assert_eq!(
        Patch {
            input: Presence::Missing,
            read: Presence::Missing,
            output: Presence::Missing,
            total: Presence::Missing,
            ..second
        },
        Patch::default(),
        "a usage patch states no charge, refusal or session end"
    );
}

/// What the first report left out stays unknown, never zero; a later report
/// that leaves out a count the session states is not counted, so every sum
/// covers the same responses.
#[test]
fn realtime_usage_keeps_the_first_reports_shape() {
    let mut session = SessionUsage::default();
    // A total alone fixes nothing.
    assert_eq!(session.add(&usage([None, None, None, Some(9)])), None);
    let first = session
        .add(&usage([Some(100), None, Some(10), None]))
        .expect("first report");
    assert_eq!(counts(&first), [Some(100), None, Some(10), Some(110)]);
    // Extra counts are ignored; the stated ones still add.
    let second = session
        .add(&usage([Some(50), Some(20), Some(5), Some(55)]))
        .expect("second report");
    assert_eq!(counts(&second), [Some(150), None, Some(15), Some(165)]);
    // Output missing: not counted at all.
    assert_eq!(session.add(&usage([Some(70), None, None, None])), None);
    let third = session
        .add(&usage([Some(1), None, Some(1), None]))
        .expect("third report");
    assert_eq!(counts(&third), [Some(151), None, Some(16), Some(167)]);

    // Input alone: no total, since output is unknown.
    let mut input_only = SessionUsage::default();
    let report = input_only
        .add(&usage([Some(7), Some(2), None, Some(7)]))
        .expect("input only");
    assert_eq!(counts(&report), [Some(7), Some(2), None, None]);
}

/// Reports the ledger could not hold as stated are left out: cached input
/// above input and counts beyond the ledger's range. A provider total that
/// disagrees with input plus output changes nothing: the recorded total is
/// their sum.
#[test]
fn realtime_usage_rejects_inconsistent_reports() {
    let mut session = SessionUsage::default();
    assert_eq!(
        session.add(&usage([Some(10), Some(11), Some(1), None])),
        None
    );
    let first = session
        .add(&usage([Some(10), Some(5), Some(1), Some(12)]))
        .expect("first report");
    assert_eq!(counts(&first), [Some(10), Some(5), Some(1), Some(11)]);
    assert_eq!(
        session.add(&usage([Some(u64::MAX), Some(0), Some(1), None])),
        None
    );
    assert_eq!(
        session.add(&usage([Some(i64::MAX as u64), Some(0), Some(1), None])),
        None,
        "a sum past the ledger's range is not counted"
    );
    let second = session
        .add(&usage([Some(2), Some(0), Some(2), Some(99)]))
        .expect("second report");
    assert_eq!(counts(&second), [Some(12), Some(5), Some(3), Some(15)]);
}
