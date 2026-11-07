use super::*;
use crate::chatwidget::tests::helpers::render_bottom_popup;
use crate::chatwidget::tests::helpers::render_bottom_popup_with_height;
use crate::chatwidget::tests::make_chatwidget_manual;
use codex_app_server_protocol::AccountTokenUsageSummary;
use codex_state::accounting::Attempt;
use codex_state::accounting::DayTotals;
use codex_state::accounting::Dialect;
use codex_state::accounting::Inspection;
use codex_state::accounting::Metric;
use codex_state::accounting::RetentionCoverage;
use codex_state::accounting::Usage;
use crossterm::event::KeyCode;
use crossterm::event::KeyEvent;
use crossterm::event::KeyModifiers;
use pretty_assertions::assert_eq;

async fn unavailable_inspector(width: u16, args: &str) -> ChatWidget {
    let (mut chat, mut rx, mut ops) = make_chatwidget_manual(/*model_override*/ None).await;
    chat.on_terminal_resize(width);
    chat.open_accounting_command(args, NaiveDate::from_ymd_opt(2026, 9, 16).unwrap());
    let AppEvent::LoadAccountingInspector {
        generation,
        thread,
        day,
        ..
    } = rx.try_recv().unwrap()
    else {
        panic!("expected inspector load");
    };
    chat.finish_accounting_inspector(generation, thread, day, Ok(InspectionDay::Absent));
    assert!(rx.try_recv().is_err());
    assert!(ops.try_recv().is_err());
    chat
}

#[tokio::test]
async fn accounting_inspect_usability_unavailable_first_screen() {
    for args in [
        "requests 2026-09-16",
        "requests 2026-09-16T00:00:00Z 2026-09-16T01:00:00Z hour",
    ] {
        let chat = unavailable_inspector(/*width*/ 150, args).await;
        let screen = render_bottom_popup_with_height(&chat, /*width*/ 150, /*height*/ 16);
        // #289: a build that records costs never says collection is off.
        assert!(screen.contains(super::absent_state()), "{screen}");
        assert!(!screen.contains("Collection remains off."), "{screen}");
    }
}

#[tokio::test]
async fn accounting_inspect_usability_wrap_tracks_resize() {
    let mut chat = unavailable_inspector(/*width*/ 150, "requests 2026-09-16").await;
    let wide = render_bottom_popup_with_height(&chat, /*width*/ 150, /*height*/ 16);
    let first_lines = wide
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .take(3)
        .collect::<Vec<_>>()
        .join("\n");
    assert_eq!(
        first_lines,
        format!(
            "Cost — this conversation\nRequested UTC day: 2026-09-16\n› {}",
            super::absent_state()
        )
    );
    assert!(
        wide.contains("Collection coverage: unknown; recorded root and resolved descendants only."),
        "{wide}"
    );
    chat.on_terminal_resize(/*width*/ 40);
    let narrow = render_bottom_popup_with_height(&chat, /*width*/ 40, /*height*/ 16);
    assert!(!narrow.contains(super::absent_state()));
    assert!(narrow.contains("Unavailable — "), "{narrow}");
    chat.on_terminal_resize(/*width*/ 150);
    assert_eq!(
        render_bottom_popup_with_height(&chat, /*width*/ 150, /*height*/ 16),
        wide
    );
}

#[tokio::test]
async fn accounting_inspect_usability_short_day_visible_in_every_state() {
    let mut screens = Vec::new();
    for (args, date) in [
        ("requests", "2026-09-16"),
        ("requests 2026-09-15", "2026-09-15"),
    ] {
        let (mut chat, mut rx, _ops) = make_chatwidget_manual(/*model_override*/ None).await;
        chat.on_terminal_resize(/*width*/ 150);
        chat.open_accounting_command(args, NaiveDate::from_ymd_opt(2026, 9, 16).unwrap());
        let AppEvent::LoadAccountingInspector {
            generation,
            thread,
            day,
            ..
        } = rx.try_recv().unwrap()
        else {
            panic!("expected inspector load");
        };
        let label = format!("Requested UTC day: {date}");
        assert!(
            render_bottom_popup_with_height(&chat, /*width*/ 150, /*height*/ 16).contains(&label)
        );
        for result in [
            Ok(InspectionDay::Absent),
            Ok(InspectionDay::MissingThread),
            Ok(InspectionDay::CheckpointLag),
            Ok(InspectionDay::NeedsRefresh),
            Ok(InspectionDay::TooLarge),
            Err("Recorded requests unavailable — retry.".into()),
        ] {
            chat.finish_accounting_inspector(generation, thread, day, result);
            let screen =
                render_bottom_popup_with_height(&chat, /*width*/ 150, /*height*/ 16);
            assert!(screen.contains(&label), "{screen}");
            screens.push(screen);
        }
    }
    assert_ne!(screens[0], screens[6]);
}

#[tokio::test]
async fn accounting_inspect_usability_hour_lifetime_wording() {
    let chat = unavailable_inspector(
        /*width*/ 150,
        "requests 2026-09-16T00:00:00Z 2026-09-16T01:00:00Z hour",
    )
    .await;
    let screen = render_bottom_popup_with_height(&chat, /*width*/ 150, /*height*/ 16);
    assert!(
        screen.contains("this UTC hour is not their complete lifetime"),
        "{screen}"
    );
    assert!(!screen.contains("this UTC day is not their complete lifetime"));

    // Check both ready and unavailable detail through the bucket popup.
    for state in [packet(), InspectionDay::NeedsRefresh] {
        let caveat = rendered_hour_bucket_caveat(state).await;
        insta::allow_duplicates! {
            insta::assert_snapshot!(caveat, @"Logical requests may have attempts outside this bucket; this UTC hour is not their complete lifetime.");
        }
    }

    let chat = unavailable_inspector(/*width*/ 150, "requests 2026-09-15 2026-09-16 day").await;
    let screen = render_bottom_popup_with_height(&chat, /*width*/ 150, /*height*/ 16);
    assert!(
        screen.contains("the selected UTC interval is not their complete lifetime"),
        "{screen}"
    );
}

async fn rendered_hour_bucket_caveat(state: InspectionDay) -> String {
    let (mut chat, mut rx, mut ops) = make_chatwidget_manual(/*model_override*/ None).await;
    chat.on_terminal_resize(/*width*/ 150);
    chat.open_accounting_command(
        "requests 1970-01-01T00:00:00Z 1970-01-01T01:00:00Z hour",
        NaiveDate::from_ymd_opt(2026, 9, 16).unwrap(),
    );
    let AppEvent::LoadAccountingInspector {
        generation,
        thread,
        day,
        ..
    } = rx.try_recv().unwrap()
    else {
        panic!("expected inspector load");
    };
    chat.finish_accounting_inspector(
        generation,
        thread,
        day,
        Ok(range_packet(/*partial*/ false, state)),
    );
    // Reach and open the bucket using the rendered label, not InspectorPage text.
    for _ in 0..60 {
        let screen = render_bottom_popup_with_height(&chat, /*width*/ 150, /*height*/ 16);
        if screen
            .lines()
            .any(|line| line.trim_start().starts_with("› → Hour ["))
        {
            chat.handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
            let AppEvent::NavigateAccountingInspector { generation, page } = rx.try_recv().unwrap()
            else {
                panic!("expected bucket navigation: {screen}");
            };
            chat.navigate_accounting_inspector(generation, page);
            break;
        }
        chat.handle_key_event(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
    }
    for _ in 0..60 {
        let screen = render_bottom_popup_with_height(&chat, /*width*/ 150, /*height*/ 16);
        if let Some(line) = screen
            .lines()
            .find(|line| line.contains("Logical requests may"))
        {
            assert!(rx.try_recv().is_err());
            assert!(ops.try_recv().is_err());
            return line.trim().trim_start_matches('›').trim().to_owned();
        }
        chat.handle_key_event(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
    }
    panic!(
        "caveat not reachable: {}",
        render_bottom_popup_with_height(&chat, /*width*/ 150, /*height*/ 16)
    );
}

#[tokio::test]
async fn accounting_inspect_usability_unavailable_hour_bucket_lifetime_wording() {
    for state in [
        InspectionDay::Absent,
        InspectionDay::MissingThread,
        InspectionDay::NeedsRefresh,
        InspectionDay::TooLarge,
        InspectionDay::DetailUnavailable {
            coverage: RetentionCoverage {
                completed_as_of_ms: 90 * 86_400_000,
                detail_expired_through_ms: Some(0),
                aggregate_day_floor: 0,
                oldest_recorded_day: Some(0),
            },
            read_at_ms: 90 * 86_400_000,
            compact: true,
        },
    ] {
        let caveat = rendered_hour_bucket_caveat(state).await;
        insta::allow_duplicates! {
            insta::assert_snapshot!(caveat, @"Logical requests may have attempts outside this bucket; this UTC hour is not their complete lifetime.");
        }
    }
}

fn decimal(text: &str) -> Decimal {
    text.to_owned().try_into().unwrap()
}

fn quote() -> ObservationQuote {
    let known = decimal("0.00018");
    ObservationQuote {
        attempt: Attempt {
            attempt_id: Uuid::from_u128(1),
            request_id: Uuid::from_u128(2),
            thread_id: ThreadId::from_string(&Uuid::from_u128(3).to_string()).unwrap(),
            turn: "synthetic".into(),
            retry_of: Some(Uuid::from_u128(4)),
            provider: "synthetic".into(),
            model: "synthetic-model".into(),
            scope: Uuid::from_u128(5),
            dialect: Dialect::Inclusive,
            dispatched_at_ms: 0.try_into().unwrap(),
            account: None,
        },
        observations: vec![],
        snapshot: None,
        usage: Usage {
            input: Some(100),
            read: Some(20),
            output: Some(40),
            ..Usage::default()
        },
        buckets: [
            BucketQuote::MissingUsage,
            BucketQuote::Priced(decimal("0.00002")),
            BucketQuote::MissingUsage,
            BucketQuote::Priced(decimal("0.00016")),
        ],
        known_subtotal: known,
        all_buckets_priced: None,
        subtotal_display: known.display(),
        known_equivalent: Decimal::default(),
        all_buckets_equivalent: None,
        plan_burn_millis: None,
        plan_burn_milli_tokens: None,
        pricing_rules: 1,
    }
}

fn packet() -> InspectionDay {
    let q = quote();
    InspectionDay::Ready(Inspection {
        owner: q.attempt.thread_id,
        utc_day: 0,
        read_at_ms: 1,
        coverage: RetentionCoverage {
            completed_as_of_ms: 0,
            detail_expired_through_ms: None,
            aggregate_day_floor: 0,
            oldest_recorded_day: Some(0),
        },
        totals: DayTotals {
            known_usd: q.known_subtotal,
            unknown_estimates: 1,
            attempts: 1,
            measured: [Some(100), None, Some(20), None, Some(40), None, None].map(|n| Metric {
                known: n.unwrap_or(0),
                unknown: i64::from(n.is_none()),
            }),
            ..Default::default()
        },
        own_totals: DayTotals::from_quotes([&q]).unwrap(),
        descendant_totals: DayTotals::default(),
        unknown_parent_totals: DayTotals::default(),
        unknown_parent_unavailable_threads: 0,
        unknown_parent_requests: Default::default(),
        requests: std::collections::BTreeMap::from([(q.attempt.request_id, vec![q])]),
        other_conversations: Some(Default::default()),
        empty_day: None,
        recording_gap: false,
    })
}

fn range_packet(partial: bool, day: InspectionDay) -> InspectionDay {
    InspectionDay::Range {
        requested: InspectionRange {
            start_ms: i64::from(partial),
            end_ms: 3_600_000,
            grouping: InspectionGrouping::Hour,
        },
        aggregate_day_floor: Some(0),
        completed_as_of_ms: None,
        read_at_ms: 90 * 86_400_000,
        buckets: vec![codex_state::accounting::InspectionBucket {
            start_ms: 0,
            end_ms: 3_600_000,
            effective: Some((i64::from(partial), 3_600_000)),
            partial,
            days: vec![day],
        }],
    }
}

#[test]
fn accounting_inspect_range_partial_no_amount_and_explicit_coverage() {
    let InspectionDay::Ready(mut view) = packet() else {
        panic!()
    };
    view.read_at_ms = view.coverage.completed_as_of_ms;
    let pages = inspection_pages(Ok(range_packet(
        /*partial*/ true,
        InspectionDay::Ready(view),
    )));
    let text = pages
        .iter()
        .flat_map(|p| &p.text)
        .cloned()
        .collect::<Vec<_>>()
        .join("\n");
    assert!(!text.contains('$'));
    assert!(text.contains("Partial bucket — excluded from totals"));
    assert!(text.contains("Range total unavailable"));
    assert!(
        pages
            .iter()
            .all(|p| p.text.iter().any(|t| t.contains("timezone: UTC")))
    );
    insta::assert_snapshot!(pages[0].text.join("\n"), @"
    Requested: [1970-01-01T00:00:00.001Z, 1970-01-01T01:00:00.000Z); timezone: UTC; grouping: Hour
    Retention: request detail kept since 1970-01-01T00:00:00.000Z; daily totals kept since 1970-01-01
    Collection coverage: unknown. Range estimate covers root and resolved descendants; unknown ancestry stays separate in bucket breakdowns.
    Range: Unknown parent population: 0 inspectable attempts, excluded from range total
    Other conversations are not included in this view; /cost covers only the open conversation.
    Range total unavailable — partial or unavailable buckets excluded; no partial total.
    Effective coverage (requested ∩ aggregate retention ∩ snapshot) for [1970-01-01T00:00:00.000Z, 1970-01-01T01:00:00.000Z): [1970-01-01T00:00:00.001Z, 1970-01-01T01:00:00.000Z)
    ");
}

/// #314 (D228, Travis 2026-10-28): a day on which accounting failed to record
/// a request of this conversation says its totals may be incomplete, right
/// under the heading, so the requests it did record never read as the whole
/// day. Missing data never looks like free work.
#[test]
fn accounting_recording_gap_says_the_day_may_be_incomplete() {
    let InspectionDay::Ready(mut view) = packet() else {
        unreachable!()
    };
    let complete = inspection_pages(Ok(packet()));
    assert!(
        !complete[0]
            .text
            .iter()
            .any(|line| line.contains("may be incomplete")),
        "{:#?}",
        complete[0].text
    );
    view.recording_gap = true;
    let pages = inspection_pages(Ok(InspectionDay::Ready(view)));
    let screen: Vec<&str> = pages[0]
        .text
        .iter()
        .map(String::as_str)
        .take_while(|line| *line != "—— Details ——")
        .collect();
    insta::assert_snapshot!(screen.join("\n"), @"
    This conversation on 1970-01-01 (UTC):
    Totals for this day may be incomplete: Corbanu failed to record at least one request of this conversation or its subagents, which may still have been billed. The cause is in Corbanu's accounting log; check your provider's bill.
    • synthetic · synthetic-model — Pay per use. 1 request, 140 tokens. Estimated cost: at least $0.000180 (1 attempt had no price).
    No other conversation recorded requests on this day.
    Next step for requests priced only in part: check the bill from synthetic. Some of their tokens have no published price, so the estimate is only a lower bound.
    Costs are estimates from published prices; synthetic doesn't report its charges — check synthetic's bill.
    Select a provider below to see its requests.
    ");
}

/// #314 (D228): an empty day with a gap says both that nothing was recorded
/// and that this is not a complete account.
#[test]
fn accounting_recording_gap_on_an_empty_day() {
    let InspectionDay::Ready(mut view) = packet() else {
        unreachable!()
    };
    view.requests.clear();
    view.totals = DayTotals::default();
    view.own_totals = DayTotals::default();
    view.empty_day = Some(codex_state::accounting::EmptyDay::NotRecorded);
    view.recording_gap = true;
    let pages = inspection_pages(Ok(InspectionDay::Ready(view)));
    assert!(
        pages[0].text[1].starts_with("Totals for this day may be incomplete:"),
        "{:#?}",
        pages[0].text
    );
}

/// #314 (D228): a range with a gap day says so on its overview, and the
/// bucket's own page says it for its period.
#[test]
fn accounting_recording_gap_in_a_range() {
    let InspectionDay::Ready(mut view) = packet() else {
        unreachable!()
    };
    view.recording_gap = true;
    view.read_at_ms = view.coverage.completed_as_of_ms;
    let pages = inspection_pages(Ok(range_packet(
        /*partial*/ false,
        InspectionDay::Ready(view),
    )));
    assert!(
        pages[0]
            .text
            .iter()
            .any(|line| line.starts_with("Totals for this range may be incomplete:")),
        "{:#?}",
        pages[0].text
    );
    let texts: Vec<&Vec<String>> = pages.iter().map(|page| &page.text).collect();
    assert!(
        texts[1..].iter().any(|text| text
            .iter()
            .any(|line| line.starts_with("Totals for this period may be incomplete:"))),
        "{texts:#?}"
    );
}

/// A week read on its third day: day 0 recorded up to the checkpoint, the
/// rest not reached yet. A nonzero `start` cuts the bucket's coverage short
/// at its start, as retention or the requested range would.
fn week_reaching_today(start: i64) -> Vec<InspectorPage> {
    const DAY: i64 = 86_400_000;
    let InspectionDay::Ready(mut view) = packet() else {
        panic!()
    };
    view.coverage.completed_as_of_ms = 1_000;
    view.read_at_ms = 2 * DAY;
    let mut days = vec![InspectionDay::Ready(view)];
    days.extend((1..7).map(|_| InspectionDay::CheckpointLag));
    range_pages(
        InspectionRange {
            start_ms: 0,
            end_ms: 7 * DAY,
            grouping: InspectionGrouping::Week,
        },
        Some(0),
        /*completed_as_of_ms*/ None,
        /*read_at*/ 2 * DAY,
        vec![codex_state::accounting::InspectionBucket {
            start_ms: 0,
            end_ms: 7 * DAY,
            effective: Some((start, 1_001)),
            partial: true,
            days,
        }],
    )
}

// #289 R2: a bucket that reaches today is in progress. It shows what is
// recorded so far and offers no next step that cannot change anything.
#[test]
fn accounting_inspect_range_bucket_reaching_today_is_in_progress() {
    let pages = week_reaching_today(/*start*/ 0);
    let so_far = "In progress — totals so far, recorded through 1970-01-01T00:00:01.000Z";
    let range = &pages[0].text;
    assert!(range.iter().any(|s| s == so_far), "{range:#?}");
    assert!(
        range
            .iter()
            .any(|s| s.starts_with("Estimated token cost")
                || s.starts_with("Full recorded estimate")),
        "{range:#?}"
    );
    assert!(
        !range
            .iter()
            .any(|s| s.starts_with("Range total unavailable"))
    );
    let (label, target) = &pages[0].links[0];
    assert!(label.ends_with(" (in progress)"), "{label}");
    let bucket = &pages[*target];
    assert_eq!(bucket.title, "Cost so far — this conversation");
    assert!(
        bucket.text.iter().any(|s| s == so_far),
        "{:#?}",
        bucket.text
    );
    assert!(
        bucket
            .text
            .iter()
            .any(|s| s.starts_with("This conversation, so far in [")),
        "{:#?}",
        bucket.text
    );
    let all = pages
        .iter()
        .flat_map(|p| p.text.iter().chain([&p.title]))
        .cloned()
        .collect::<Vec<_>>()
        .join("\n");
    for absent in [
        "Bucket unavailable",
        "Partial bucket",
        "Next step: send a turn",
        "excluded from totals",
        "unavailable slices may contain",
        NOT_CURRENT,
    ] {
        assert!(!all.contains(absent), "{absent}: {all}");
    }

    // Cut short at its start instead: still partial, and still withheld.
    let pages = week_reaching_today(/*start*/ 500);
    let bucket = &pages[pages[0].links[0].1];
    assert_eq!(bucket.title, "Bucket unavailable");
    assert!(
        bucket
            .text
            .iter()
            .any(|s| s == "Partial bucket — excluded from totals")
    );
    assert!(
        pages[0]
            .text
            .iter()
            .any(|s| s.starts_with("Range total unavailable"))
    );
}

fn bucket(
    start_ms: i64,
    end_ms: i64,
    effective: Option<(i64, i64)>,
    days: Vec<InspectionDay>,
) -> codex_state::accounting::InspectionBucket {
    codex_state::accounting::InspectionBucket {
        start_ms,
        end_ms,
        partial: effective != Some((start_ms, end_ms)),
        effective,
        days,
    }
}

fn ready_at(checkpoint: i64, read_at: i64) -> InspectionDay {
    let InspectionDay::Ready(mut view) = packet() else {
        panic!()
    };
    view.coverage.completed_as_of_ms = checkpoint;
    view.read_at_ms = read_at;
    InspectionDay::Ready(view)
}

fn joined(pages: &[InspectorPage]) -> String {
    pages
        .iter()
        .flat_map(|p| p.text.iter().chain([&p.title]))
        .cloned()
        .collect::<Vec<_>>()
        .join("\n")
}

// A bucket reaching today stays partial, and withheld, unless the read time
// is all that cuts it short.
#[test]
fn accounting_inspect_range_bucket_reaching_today_stays_partial_when_unverified() {
    const DAY: i64 = 86_400_000;
    let week = |end_ms: i64, effective: (i64, i64), first: InspectionDay| {
        let mut days = vec![first];
        days.extend((1..7).map(|_| InspectionDay::CheckpointLag));
        range_pages(
            InspectionRange {
                start_ms: 0,
                end_ms,
                grouping: InspectionGrouping::Week,
            },
            Some(0),
            /*completed_as_of_ms*/ None,
            /*read_at*/ 2 * DAY,
            vec![bucket(/*start_ms*/ 0, 7 * DAY, Some(effective), days)],
        )
    };
    let detail = InspectionDay::DetailUnavailable {
        coverage: RetentionCoverage {
            completed_as_of_ms: 1_000,
            detail_expired_through_ms: None,
            aggregate_day_floor: 0,
            oldest_recorded_day: Some(0),
        },
        read_at_ms: 2 * DAY,
        compact: false,
    };
    for (label, pages) in [
        (
            "needs refresh",
            week(7 * DAY, (0, 1_001), InspectionDay::NeedsRefresh),
        ),
        ("detail unavailable", week(7 * DAY, (0, 1_001), detail)),
        // A lagging day inside the coverage is unverified, not "not yet".
        (
            "lag before end",
            week(7 * DAY, (0, 1_001), InspectionDay::CheckpointLag),
        ),
        // The requested range ends at the read time: cut short by the request.
        (
            "request ends",
            week(2 * DAY, (0, 2 * DAY), ready_at(2 * DAY, 2 * DAY)),
        ),
    ] {
        let text = joined(&pages);
        assert!(
            text.contains("Partial bucket — excluded from totals"),
            "{label}: {text}"
        );
        assert!(text.contains("Range total unavailable"), "{label}: {text}");
        assert!(!text.contains("In progress"), "{label}: {text}");
    }
}

// Hour grouping: a whole hour and the current one. The range total is their
// sum, and says it is so far.
#[test]
fn accounting_inspect_range_hour_in_progress_total_sums_whole_and_current() {
    const HOUR: i64 = 3_600_000;
    let checkpoint = HOUR + 1_000;
    let pages = range_pages(
        InspectionRange {
            start_ms: 0,
            end_ms: 2 * HOUR,
            grouping: InspectionGrouping::Hour,
        },
        Some(0),
        /*completed_as_of_ms*/ None,
        /*read_at*/ HOUR + HOUR / 2,
        vec![
            bucket(
                /*start_ms*/ 0,
                HOUR,
                Some((0, HOUR)),
                vec![ready_at(checkpoint, HOUR + HOUR / 2)],
            ),
            bucket(
                HOUR,
                2 * HOUR,
                Some((HOUR, checkpoint + 1)),
                vec![ready_at(checkpoint, HOUR + HOUR / 2)],
            ),
        ],
    );
    let q = quote();
    let sum = DayTotals::from_quotes([&q, &q]).unwrap();
    let range = &pages[0].text;
    assert!(
        range.contains(
            &"In progress — totals so far, recorded through 1970-01-01T01:00:01.000Z".to_string()
        ),
        "{range:#?}"
    );
    for line in estimate(&sum, EstimateGaps::default()) {
        assert!(range.contains(&line), "{line}: {range:#?}");
    }
    assert!(!range.iter().any(|s| s == NOT_CURRENT), "{range:#?}");
    let labels: Vec<_> = pages[0].links.iter().map(|(l, _)| l.clone()).collect();
    assert!(!labels[0].contains("(in progress)"), "{labels:?}");
    assert!(labels[1].ends_with(" (in progress)"), "{labels:?}");
}

// Weeks after today have not started: they say so, offer no next step and do
// not withhold the total of the week in progress.
#[test]
fn accounting_inspect_range_future_buckets_have_not_started() {
    const DAY: i64 = 86_400_000;
    let lag = || {
        (0..7)
            .map(|_| InspectionDay::CheckpointLag)
            .collect::<Vec<_>>()
    };
    let mut current = vec![ready_at(/*checkpoint*/ 1_000, 2 * DAY)];
    current.extend((1..7).map(|_| InspectionDay::CheckpointLag));
    let pages = range_pages(
        InspectionRange {
            start_ms: 0,
            end_ms: 21 * DAY,
            grouping: InspectionGrouping::Week,
        },
        Some(0),
        /*completed_as_of_ms*/ None,
        /*read_at*/ 2 * DAY,
        vec![
            bucket(/*start_ms*/ 0, 7 * DAY, Some((0, 1_001)), current),
            bucket(7 * DAY, 14 * DAY, /*effective*/ None, lag()),
            bucket(14 * DAY, 21 * DAY, /*effective*/ None, lag()),
        ],
    );
    let range = &pages[0].text;
    assert!(
        range
            .iter()
            .any(|s| s.starts_with("In progress — totals so far")),
        "{range:#?}"
    );
    assert!(
        !range
            .iter()
            .any(|s| s.starts_with("Range total unavailable")),
        "{range:#?}"
    );
    let links = &pages[0].links;
    assert!(links[0].0.ends_with(" (in progress)"));
    for (label, target) in &links[1..] {
        assert!(label.ends_with(" (not started)"), "{label}");
        let page = &pages[*target];
        assert_eq!(page.title, "Not started yet");
        assert!(
            page.text.iter().any(|s| s == NOT_STARTED),
            "{:#?}",
            page.text
        );
    }
    let text = joined(&pages);
    for absent in [
        "Bucket unavailable",
        "Next step: send a turn",
        "Partial bucket",
        NOT_CURRENT,
    ] {
        assert!(!text.contains(absent), "{absent}: {text}");
    }
}

// An older bucket with expired detail beside the week in progress: the
// ledger being behind the read time is still that week's "recorded through",
// not a stale snapshot.
#[test]
fn accounting_inspect_range_expired_bucket_beside_in_progress_is_not_stale() {
    const DAY: i64 = 86_400_000;
    let expired = InspectionDay::DetailUnavailable {
        coverage: RetentionCoverage {
            completed_as_of_ms: 100 * DAY + 1_000,
            detail_expired_through_ms: Some(10 * DAY),
            aggregate_day_floor: 0,
            oldest_recorded_day: Some(0),
        },
        read_at_ms: 102 * DAY,
        compact: true,
    };
    let mut current = vec![ready_at(100 * DAY + 1_000, 102 * DAY)];
    current.extend((1..7).map(|_| InspectionDay::CheckpointLag));
    let pages = range_pages(
        InspectionRange {
            start_ms: 0,
            end_ms: 107 * DAY,
            grouping: InspectionGrouping::Day,
        },
        /*aggregate_day_floor*/ None,
        /*completed_as_of_ms*/ None,
        /*read_at*/ 102 * DAY,
        vec![
            bucket(/*start_ms*/ 0, DAY, Some((0, DAY)), vec![expired]),
            bucket(
                100 * DAY,
                107 * DAY,
                Some((100 * DAY, 100 * DAY + 1_001)),
                current,
            ),
        ],
    );
    let text = joined(&pages);
    assert!(!text.contains(NOT_CURRENT), "{text}");
    assert!(text.contains("In progress — totals so far"), "{text}");
    assert!(
        pages[0]
            .text
            .iter()
            .any(|s| s.starts_with("Retention:")
                && s.ends_with("; daily totals kept: not known yet")),
        "{:#?}",
        pages[0].text
    );
}

// The same, from what the store actually returns for a week reaching today
// and the weeks after it.
#[tokio::test]
async fn accounting_inspect_store_range_reaching_today_reads_as_in_progress() -> anyhow::Result<()>
{
    use codex_state::accounting::AccountingStore;
    const DAY: i64 = 86_400_000;
    // 1970-04-11, a Saturday; its ISO week starts on day 95.
    let time = 100 * DAY;
    let home = tempfile::tempdir()?;
    let runtime = codex_state::StateRuntime::init(
        codex_state::SqliteConfig::from_sqlite_home(
            codex_utils_absolute_path::AbsolutePathBuf::try_from(home.path().to_path_buf())?,
        ),
        "synthetic".into(),
    )
    .await?;
    let mut attempt = quote().attempt;
    attempt.retry_of = None;
    attempt.dispatched_at_ms = time.try_into()?;
    let owner = attempt.thread_id;
    let metadata = codex_state::ThreadMetadataBuilder::new(
        owner,
        home.path().join("synthetic.jsonl"),
        chrono::Utc::now(),
        codex_protocol::protocol::SessionSource::Cli,
    );
    runtime.upsert_thread(&metadata.build("synthetic")).await?;
    let store = AccountingStore::open(&runtime, time).await?;
    store.admit(owner, &attempt, &[], time).await?;
    let result = AccountingStore::inspect_range(
        &runtime,
        owner,
        InspectionRange {
            start_ms: 95 * DAY,
            end_ms: 116 * DAY,
            grouping: InspectionGrouping::Week,
        },
        time + 3_600_000,
    )
    .await?;
    let pages = inspection_pages(Ok(result));
    let titles: Vec<_> = pages[0]
        .links
        .iter()
        .map(|(_, target)| pages[*target].title.clone())
        .collect();
    assert_eq!(
        titles,
        vec![
            "Cost so far — this conversation",
            "Not started yet",
            "Not started yet"
        ]
    );
    assert!(
        pages[0]
            .text
            .iter()
            .any(|s| s == "In progress — totals so far, recorded through 1970-04-11T00:00:00.000Z"),
        "{:#?}",
        pages[0].text
    );
    let text = joined(&pages);
    for absent in [
        "Bucket unavailable",
        "Next step: send a turn",
        "Partial bucket",
    ] {
        assert!(!text.contains(absent), "{absent}: {text}");
    }
    runtime.close().await;
    Ok(())
}

// #289 R3: the range header and the day pages state the same floor.
#[test]
fn accounting_inspect_range_retention_matches_day_pages() {
    let floor = 36;
    let pages = range_pages(
        InspectionRange {
            start_ms: 40 * 86_400_000,
            end_ms: 41 * 86_400_000,
            grouping: InspectionGrouping::Day,
        },
        Some(floor),
        /*completed_as_of_ms*/ None,
        /*read_at*/ 400 * 86_400_000,
        Vec::new(),
    );
    let phrase = "daily totals kept since 1970-02-06";
    assert!(
        pages[0]
            .text
            .iter()
            .any(|s| s.starts_with("Retention:") && s.contains(phrase)),
        "{:#?}",
        pages[0].text
    );
    assert!(retention(400 * 86_400_000, floor, /*oldest_recorded_day*/ None).contains(phrase));
    assert!(
        !pages[0]
            .text
            .iter()
            .any(|s| s.contains("oldest daily total"))
    );
}

#[test]
fn accounting_inspect_range_coverage_names_intersection_for_request_and_retention_clips() {
    let day_ms = 86_400_000;
    for (requested_start, aggregate_floor, effective_start) in [(1, 0, 1), (0, 1, day_ms)] {
        let pages = range_pages(
            InspectionRange {
                start_ms: requested_start,
                end_ms: 31 * day_ms,
                grouping: InspectionGrouping::Month,
            },
            Some(aggregate_floor),
            /*completed_as_of_ms*/ None,
            31 * day_ms,
            vec![codex_state::accounting::InspectionBucket {
                start_ms: 0,
                end_ms: 31 * day_ms,
                effective: Some((effective_start, 31 * day_ms)),
                partial: true,
                days: vec![InspectionDay::DetailUnavailable {
                    coverage: RetentionCoverage {
                        completed_as_of_ms: 31 * day_ms,
                        detail_expired_through_ms: None,
                        aggregate_day_floor: aggregate_floor,
                        oldest_recorded_day: Some(aggregate_floor),
                    },
                    read_at_ms: 31 * day_ms,
                    compact: false,
                }],
            }],
        );
        let bounds = interval(/*start*/ 0, 31 * day_ms);
        let effective = interval(effective_start, 31 * day_ms);
        assert!(pages[0].text.contains(&format!(
            "Effective coverage (requested ∩ aggregate retention ∩ snapshot) for {bounds}: {effective}"
        )));
        let bucket = &pages[pages[0].links[0].1];
        assert!(bucket.text.contains(&format!(
            "Bucket: {bounds}; effective coverage (requested ∩ aggregate retention ∩ snapshot): {effective}"
        )));
        assert!(
            !pages
                .iter()
                .flat_map(|p| &p.text)
                .any(|s| s.contains("effective aggregate retention coverage")
                    || s.contains("Effective aggregate retention coverage"))
        );
        assert!(
            bucket
                .text
                .iter()
                .any(|s| s == "Partial bucket — excluded from totals")
        );
    }
}

#[test]
fn accounting_inspect_range_bucket_ancestry_counts_have_explicit_scopes() {
    let mut buckets = Vec::new();
    for index in 0..2 {
        let InspectionDay::Ready(mut view) = breakdown_packet() else {
            panic!()
        };
        view.unknown_parent_unavailable_threads = (index + 2) as usize;
        buckets.push(codex_state::accounting::InspectionBucket {
            start_ms: index * 3_600_000,
            end_ms: (index + 1) * 3_600_000,
            effective: Some((index * 3_600_000, (index + 1) * 3_600_000)),
            partial: false,
            days: vec![InspectionDay::Ready(view)],
        });
    }
    let pages = range_pages(
        InspectionRange {
            start_ms: 0,
            end_ms: 7_200_000,
            grouping: InspectionGrouping::Hour,
        },
        /*aggregate_day_floor*/ None,
        /*completed_as_of_ms*/ None,
        /*read_at*/ 7_200_000,
        buckets,
    );
    let bucket = &pages[pages[0].links[0].1];
    let counts = bucket
        .text
        .iter()
        .filter(|s| s.contains("Unknown parent population:") || s.contains("Unresolved ancestry:"))
        .cloned()
        .collect::<Vec<_>>();
    assert_eq!(
        counts,
        vec![
            "Range: Unknown parent population: 2 inspectable attempts, excluded from range total",
            "Range: Unresolved ancestry: 5 thread-slice entries have unavailable detail; their costs and retention coverage are unknown and excluded from this root.",
            "Bucket: Unresolved ancestry: 2 thread-day entries have unavailable detail; their costs and retention coverage are unknown and excluded from this root.",
            "Bucket: Unknown parent population: 1 attempts, excluded from root total",
        ]
    );
}

#[test]
fn accounting_inspect_range_aggregate_coverage_does_not_claim_raw_detail() {
    let day = InspectionDay::DetailUnavailable {
        coverage: RetentionCoverage {
            completed_as_of_ms: 90 * 86_400_000 + 1_800_000,
            detail_expired_through_ms: Some(1_800_000),
            aggregate_day_floor: 0,
            oldest_recorded_day: Some(0),
        },
        read_at_ms: 90 * 86_400_000 + 1_800_000,
        compact: true,
    };
    let pages = inspection_pages(Ok(range_packet(/*partial*/ false, day)));
    let bucket = &pages[pages[0].links[0].1];
    assert_eq!(bucket.title, "Bucket unavailable");
    assert!(bucket.text.iter().any(|s| s == "Whole bucket within aggregate retention coverage; detail availability checked separately"));
    assert!(!pages.iter().flat_map(|p| &p.text).any(|s| s.contains('$')));
    insta::assert_snapshot!(
        bucket.text.iter().filter(|s| s.starts_with("Bucket:") || s.starts_with("Whole bucket")).cloned().collect::<Vec<_>>().join("\n"),
        @"
    Bucket: [1970-01-01T00:00:00.000Z, 1970-01-01T01:00:00.000Z); effective coverage (requested ∩ aggregate retention ∩ snapshot): [1970-01-01T00:00:00.000Z, 1970-01-01T01:00:00.000Z)
    Whole bucket within aggregate retention coverage; detail availability checked separately
    "
    );
}

#[test]
fn accounting_inspect_range_overview_discloses_unknown_population_before_total() {
    let mut buckets = Vec::new();
    for index in 0..2 {
        let InspectionDay::Ready(mut view) = breakdown_packet() else {
            panic!()
        };
        view.unknown_parent_unavailable_threads = (index + 2) as usize;
        buckets.push(codex_state::accounting::InspectionBucket {
            start_ms: index * 3_600_000,
            end_ms: (index + 1) * 3_600_000,
            effective: Some((index * 3_600_000, (index + 1) * 3_600_000)),
            partial: false,
            days: vec![InspectionDay::Ready(view)],
        });
    }
    let pages = range_pages(
        InspectionRange {
            start_ms: 0,
            end_ms: 2 * 3_600_000,
            grouping: InspectionGrouping::Hour,
        },
        /*aggregate_day_floor*/ None,
        /*completed_as_of_ms*/ None,
        2 * 3_600_000,
        buckets,
    );
    let text = &pages[0].text;
    let total = text.iter().position(|s| s.contains("$0.000014")).unwrap();
    let count =
        "Range: Unknown parent population: 2 inspectable attempts, excluded from range total";
    let note = "Range: Unresolved ancestry: 5 thread-slice entries have unavailable detail; their costs and retention coverage are unknown and excluded from this root.";
    assert!(text.iter().position(|s| s == count).unwrap() < total);
    assert!(text.iter().position(|s| s == note).unwrap() < total);
    insta::assert_snapshot!(
        text.iter().filter(|s| s.starts_with("Range: Unknown parent population:") || s.starts_with("Range: Unresolved ancestry:")).cloned().collect::<Vec<_>>().join("\n"),
        @"
    Range: Unknown parent population: 2 inspectable attempts, excluded from range total
    Range: Unresolved ancestry: 5 thread-slice entries have unavailable detail; their costs and retention coverage are unknown and excluded from this root.
    "
    );
}

#[test]
fn accounting_inspect_range_breakdowns_and_navigation_reconcile() {
    let pages = inspection_pages(Ok(range_packet(/*partial*/ false, breakdown_packet())));
    assert!(pages[0].text.iter().any(|t| t.contains("$0.000007")));
    let bucket = pages[0].links[0].1;
    assert_eq!(pages[bucket].parent, Some(0));
    for p in &pages[1..] {
        assert!(p.text.iter().any(|s| s.starts_with("Requested:")));
        assert!(p.text.iter().any(|s| s.starts_with("Bucket:")
            && s.contains("effective coverage (requested ∩ aggregate retention ∩ snapshot):")));
        assert!(
            !p.text
                .iter()
                .any(|s| s.starts_with("UTC admission interval:"))
        );
        for (_, target) in &p.links {
            assert!(*target > bucket && *target < pages.len());
        }
    }
    for (title, amount) in [
        ("Root's own attempts", "0.000001"),
        ("Descendant attempts", "0.000006"),
        ("Unknown parent population", "$0.000008"),
        (
            "Provider: unknown (attribution absent); Model: unknown (attribution absent)",
            "0.000004",
        ),
    ] {
        let p = pages.iter().find(|p| p.title == title).unwrap();
        assert!(p.text.iter().any(|s| s.contains(amount)));
    }
}

#[test]
fn accounting_inspect_range_week_and_month_merge_exact_attempts() {
    for grouping in [InspectionGrouping::Week, InspectionGrouping::Month] {
        let start = 4 * 86_400_000; // Monday, 1970-01-05.
        let requested = InspectionRange {
            start_ms: start,
            end_ms: start + 1,
            grouping,
        };
        let (start_ms, end_ms) = requested.bucket(start).unwrap();
        let mut days = Vec::new();
        for index in 0..2 {
            let InspectionDay::Ready(mut view) = breakdown_packet() else {
                panic!()
            };
            view.utc_day = start_ms / 86_400_000 + index;
            view.read_at_ms = end_ms;
            view.coverage.completed_as_of_ms = end_ms;
            for q in view
                .requests
                .values_mut()
                .chain(view.unknown_parent_requests.values_mut())
                .flatten()
            {
                q.attempt.attempt_id =
                    Uuid::from_u128(q.attempt.attempt_id.as_u128() + index as u128 * 100);
                q.attempt.dispatched_at_ms = (start_ms + index * 86_400_000).try_into().unwrap();
            }
            days.push(InspectionDay::Ready(view));
        }
        let pages = inspection_pages(Ok(InspectionDay::Range {
            requested: InspectionRange {
                start_ms,
                end_ms,
                grouping,
            },
            aggregate_day_floor: None,
            completed_as_of_ms: None,
            read_at_ms: end_ms,
            buckets: vec![codex_state::accounting::InspectionBucket {
                start_ms,
                end_ms,
                effective: Some((start_ms, end_ms)),
                partial: false,
                days,
            }],
        }));
        assert!(pages[0].text.iter().any(|s| s.contains("$0.000014")));
        // The merged bucket names its whole span, never a single day.
        let heading = format!("This conversation, {} (UTC):", interval(start_ms, end_ms));
        let texts: Vec<&String> = pages.iter().flat_map(|p| &p.text).collect();
        assert!(texts.contains(&&heading), "{texts:?}");
        assert!(
            !texts
                .iter()
                .any(|s| s.starts_with("This conversation on ") || s.starts_with("Today (UTC)"))
        );
        for (title, amount) in [
            ("Root's own attempts", "0.000002"),
            ("Descendant attempts", "0.000012"),
            ("Unknown parent population", "$0.000016"),
        ] {
            assert!(
                pages
                    .iter()
                    .find(|p| p.title == title)
                    .unwrap()
                    .text
                    .iter()
                    .any(|s| s.contains(amount))
            );
        }
        assert_eq!(
            pages
                .iter()
                .filter(|p| p.title == "Attempt, components and original price")
                .count(),
            6
        );
    }
}

#[test]
fn accounting_inspect_range_unavailable_precision_and_freshness() {
    for state in [
        InspectionDay::CheckpointLag,
        InspectionDay::NeedsRefresh,
        InspectionDay::DetailUnavailable {
            coverage: RetentionCoverage {
                completed_as_of_ms: 90 * 86_400_000,
                detail_expired_through_ms: Some(0),
                aggregate_day_floor: 0,
                oldest_recorded_day: Some(0),
            },
            read_at_ms: 90 * 86_400_000,
            compact: true,
        },
    ] {
        let expected = match state {
            InspectionDay::CheckpointLag => "Snapshot is not current",
            InspectionDay::NeedsRefresh => "stored contributions need refresh",
            _ => {
                "Whole UTC-day bounds offered: [1970-01-01T00:00:00.000Z, 1970-01-02T00:00:00.000Z)"
            }
        };
        let pages = inspection_pages(Ok(range_packet(/*partial*/ false, state)));
        let text = pages
            .iter()
            .flat_map(|p| &p.text)
            .cloned()
            .collect::<Vec<_>>()
            .join("\n");
        assert!(text.contains(expected));
        assert!(!text.contains('$'));
    }
    let pages = inspection_pages(Ok(range_packet(/*partial*/ false, packet())));
    assert!(
        pages[1..]
            .iter()
            .all(|p| p.text.iter().any(|s| s.contains("Snapshot is not current")))
    );
}

#[tokio::test]
async fn accounting_inspect_range_command_refresh_and_refusal() {
    let (mut chat, mut rx, _ops) = make_chatwidget_manual(/*model_override*/ None).await;
    let today = NaiveDate::from_ymd_opt(2026, 9, 16).unwrap();
    for grouping in ["hour", "day", "week", "month"] {
        chat.open_accounting_command(&format!("requests 2026-09-01 2026-09-02 {grouping}"), today);
        let AppEvent::LoadAccountingInspector {
            generation, range, ..
        } = rx.try_recv().unwrap()
        else {
            panic!()
        };
        let range = range.unwrap();
        assert_eq!(format!("{:?}", range.grouping).to_lowercase(), grouping);
        chat.refresh_accounting_inspector(generation);
        let AppEvent::LoadAccountingInspector {
            range: refreshed,
            generation: next,
            ..
        } = rx.try_recv().unwrap()
        else {
            panic!()
        };
        assert_eq!(refreshed, Some(range));
        assert_ne!(next, generation);
    }
    for (args, message) in [
        ("requests 2026-09-02 2026-09-01 hour", "reversed"),
        ("requests 2026-09-01 2026-09-01 day", "empty"),
        ("requests bad 2026-09-01 day", "Range refused"),
        (
            "requests 2026-09-01T00:00:00.0001Z 2026-09-02 hour",
            "finer than milliseconds",
        ),
        ("requests 2026-09-01T00:00:00-07:00 2026-09-02 day", "UTC Z"),
        ("requests 2026-09-01 2026-09-02 year", "grouping must"),
    ] {
        chat.open_accounting_command(args, today);
        let AppEvent::InsertHistoryCell(cell) = rx.try_recv().unwrap() else {
            panic!()
        };
        let text = cell
            .display_lines(/*width*/ 120)
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join("\n");
        assert!(text.contains(message), "{text}");
        assert!(rx.try_recv().is_err());
    }
}

/// A day carrying only the money figures these copy tests are about.
fn money_day(known: Decimal, unknown: i64, attempts: i64) -> DayTotals {
    DayTotals {
        known_usd: known,
        unknown_estimates: unknown,
        attempts,
        ..Default::default()
    }
}

/// A day of subscription work: plan attempts, the consumption they imply, and
/// the API equivalent, with `unknown_equivalents` for attempts with no API rate
/// or an incomplete token count.
fn plan_day(equivalent: Decimal, burn_milli_tokens: i64, unknown_equivalents: i64) -> DayTotals {
    DayTotals {
        attempts: 2,
        unknown_estimates: 2,
        equivalent_usd: equivalent,
        unknown_equivalents,
        plan_burn_milli_tokens: Metric {
            known: burn_milli_tokens,
            unknown: 0,
        },
        plan_attempts: 2,
        ..Default::default()
    }
}

#[test]
fn accounting_inspect_plan_work_is_never_reported_as_money_spent() {
    // Plan work with an API equivalent: the plan rate, what it consumed, and
    // what the same tokens would have cost - none of it spend.
    insta::assert_snapshot!(estimate(&plan_day(decimal("0.00161"), /*burn_milli_tokens*/ 140_000, /*unknown_equivalents*/ 0), EstimateGaps::default()).join("\n"), @"
    No recorded attempt here was billed per token.
    Subscription capacity: 2 of 2 attempts, not billed per token
    Plan consumption: 140 tokens at the plan rate that applied
    Same tokens at API rates: $0.001610
    ");
    // A plan row with no API rate recorded says so, rather than
    // reporting zero.
    insta::assert_snapshot!(estimate(&plan_day(Decimal::default(), /*burn_milli_tokens*/ 280_000, /*unknown_equivalents*/ 2), EstimateGaps::default()).join("\n"), @"
    No recorded attempt here was billed per token.
    Subscription capacity: 2 of 2 attempts, not billed per token
    Plan consumption: 280 tokens at the plan rate that applied
    Same tokens at API rates: unavailable — no API rate or complete token count for 2 of 2 plan attempts
    ");
    // #352: a plan attempt with API rates but no cache-write count (OpenAI on
    // a ChatGPT login reports none) has a known part and no complete figure.
    let mut q = quote();
    q.snapshot = Some(native_snapshot(
        serde_json::json!({"noncached":"1","read":"0.1","write":"1.25","output":"4"}),
        "PlanEquivalent",
        /*basis_source*/ None,
        /*plan_burn_millis*/ Some(1000),
    ));
    q.known_subtotal = Decimal::default();
    q.subtotal_display = Decimal::default().display();
    q.known_equivalent = decimal("0.00018");
    q.all_buckets_equivalent = None;
    q.plan_burn_millis = Some(1000);
    q.plan_burn_milli_tokens = Some(160_000);
    let partial = DayTotals::from_quotes([&q]).unwrap();
    assert_eq!(
        plan(&partial).last().unwrap(),
        "Same tokens at API rates: at least $0.000180; 1 of 1 plan attempts have no API rate or an incomplete token count"
    );
    // A day with no plan work says nothing about plans at all.
    assert_eq!(
        estimate(
            &money_day(decimal("0.00018"), /*unknown*/ 0, /*attempts*/ 1),
            EstimateGaps::default()
        ),
        vec!["Estimated token cost for recorded attempts: $0.000180"]
    );
    // A mixed day is the case the two populations exist for. Three attempts,
    // two of them plan work: the plan pair must not be counted as billed
    // attempts whose price went missing, so the billed line speaks for the one
    // attempt that was actually charged per token.
    let mixed = DayTotals {
        attempts: 3,
        // Every plan attempt contributes one here by construction, and the
        // billed attempt was priced in full.
        unknown_estimates: 2,
        known_usd: decimal("0.00018"),
        plan_attempts: 2,
        plan_burn_milli_tokens: Metric {
            known: 140_000,
            unknown: 0,
        },
        equivalent_usd: decimal("0.00161"),
        ..Default::default()
    };
    insta::assert_snapshot!(estimate(&mixed, EstimateGaps::default()).join("\n"), @"
    Estimated token cost for recorded attempts: $0.000180
    Subscription capacity: 2 of 3 attempts, not billed per token
    Plan consumption: 140 tokens at the plan rate that applied
    Same tokens at API rates: $0.001610
    ");
    // And the same day with its one billed attempt unpriced counts exactly
    // that one, not all three.
    let mixed_unpriced = DayTotals {
        unknown_estimates: 3,
        known_usd: Decimal::default(),
        ..mixed
    };
    insta::assert_snapshot!(estimate(&mixed_unpriced, EstimateGaps::default()).join("\n"), @"
    Estimated token cost: unknown
    Full recorded estimate: unavailable (1 of 1 billed attempts incomplete)
    Subscription capacity: 2 of 3 attempts, not billed per token
    Plan consumption: 140 tokens at the plan rate that applied
    Same tokens at API rates: $0.001610
    ");
    // A subscription attempt on a row with API rates and no published plan
    // rate states the equivalent and says the consumption is not stated.
    let unstated_rate = DayTotals {
        attempts: 1,
        plan_attempts: 1,
        unknown_estimates: 1,
        plan_burn_milli_tokens: Metric {
            known: 0,
            unknown: 1,
        },
        equivalent_usd: decimal("0.00161"),
        ..Default::default()
    };
    insta::assert_snapshot!(plan(&unstated_rate).join("\n"), @"
    Subscription capacity: 1 of 1 attempts, not billed per token
    Plan consumption: unavailable — no plan rate or token total for 1 of 1 plan attempts
    Same tokens at API rates: $0.001610
    ");
    // The per-attempt page states the rate that applied at dispatch.
    let mut q = quote();
    q.plan_burn_millis = Some(500);
    let page = attempt_text(&q).join("\n");
    assert!(page.contains("Plan rate at dispatch: 0.500x"));
    // A single plan attempt's page speaks for that attempt only. The same day
    // may hold billed attempts, so a day-wide claim here could be false.
    assert!(page.contains("No recorded attempt here was billed per token."));
    assert!(!page.contains("this day"), "{page}");
}

#[test]
fn accounting_inspect_partial_and_unknown_copy() {
    insta::assert_snapshot!(estimate(&money_day(decimal("0.00018"), /*unknown*/ 1, /*attempts*/ 1), EstimateGaps::default()).join("\n"), @"
    Known estimated token cost: $0.000180 + unknown costs
    Full recorded estimate: unavailable (1 of 1 billed attempts incomplete)
    ");
    let pages = inspection_pages(Ok(packet()));
    let summary = pages[0].text.join("\n");
    for label in METRICS {
        assert!(summary.contains(label));
    }
    assert!(summary.contains("Input: 100\n"), "{summary}");
    assert!(
        summary.contains("Cache write: not reported (1 attempt)"),
        "{summary}"
    );
    assert!(summary.contains(SYNTHETIC_NOT_STATED), "{summary}");
    assert!(summary.contains("Collection coverage: unknown"));
    assert!(
        attempt_text(&quote())
            .join("\n")
            .contains("Cache write: unknown — no retained numeric evidence")
    );
}

#[test]
fn accounting_inspect_unpriced_zero_and_missing_rate() {
    insta::assert_snapshot!(estimate(&money_day(Decimal::default(), /*unknown*/ 1, /*attempts*/ 1), EstimateGaps::default()).join("\n"), @"
    Estimated token cost: unknown
    Full recorded estimate: unavailable (1 of 1 billed attempts incomplete)
    ");
    assert_eq!(
        estimate(
            &money_day(Decimal::default(), /*unknown*/ 0, /*attempts*/ 1),
            EstimateGaps::default()
        ),
        vec!["Estimated token cost for recorded attempts: $0.000000"]
    );
    let mut q = quote();
    q.usage.write = Some(0);
    q.buckets[1] = BucketQuote::MissingRate;
    let text = attempt_text(&q).join("\n");
    assert!(text.contains("Cache write: 0"));
    assert!(text.contains("Cache read cost: unknown — rate unavailable"));
    assert!(text.contains("Price: unavailable — no dispatch-time price snapshot"));
}

#[test]
fn accounting_inspect_exact_rounding_reconciliation() {
    for (exact_value, displayed) in [
        (
            "0.0000005",
            "$0.000000 (rounded) (nonzero, less than $0.000001)",
        ),
        (
            "0.0000006",
            "$0.000001 (rounded) (nonzero, less than $0.000001)",
        ),
        ("0.0000015", "$0.000002 (rounded)"),
        ("0.0000025", "$0.000002 (rounded)"),
    ] {
        assert_eq!(money(decimal(exact_value)), displayed);
        assert_eq!(exact(decimal(exact_value)), exact_value);
    }
    // Two exact half-micro components round only after their stored subtotal.
    let mut q = quote();
    q.buckets = [BucketQuote::Priced(decimal("0.0000005")); 4];
    q.known_subtotal = decimal("0.000002");
    let text = attempt_text(&q).join("\n");
    assert_eq!(text.matches("exact USD 0.0000005").count(), 4);
    assert!(text.contains("Known subtotal exact USD: 0.000002"));
}

#[test]
fn accounting_inspect_request_attempt_price_detail() {
    let mut q = quote();
    q.snapshot = Some(serde_json::from_value(serde_json::json!({
        "id":Uuid::from_u128(6),"provider":"synthetic","model":"synthetic-model","scope":Uuid::from_u128(5),
        "currency":"USD","unit":"PerMillionTokens","rates":{"noncached":"1","read":null,"write":"2","output":"4"},
        "source_reference":Uuid::from_u128(7),"source_kind":"ProviderPublished",
        "observed_at_ms":0,"approved_at_ms":0,"effective_from_ms":0,"effective_end_ms":1000
    })).unwrap());
    let text = attempt_text(&q).join("\n");
    for id in 1..=7 {
        assert!(text.contains(&Uuid::from_u128(id).to_string()));
    }
    for required in [
        "Turn: synthetic",
        "Provider: synthetic",
        "Model: synthetic-model",
        "Opaque scope:",
        "Price source: ProviderPublished",
        "Price effective interval: [1970-01-01T00:00:00.000Z, 1970-01-01T00:00:01.000Z)",
        "Rate unavailable for Cache read",
        "Literal wire/endpoint",
        "Completion/billing status: not recorded",
    ] {
        assert!(text.contains(required), "{required}");
    }
}

/// A price record in the shape the native catalogue binds: `rates`, `basis`,
/// `basis_source` and `plan_burn_millis` as given.
fn native_snapshot(
    rates: serde_json::Value,
    basis: &str,
    basis_source: Option<&str>,
    plan_burn_millis: Option<i64>,
) -> codex_state::accounting::Snapshot {
    let mut value = serde_json::json!({
        "id":Uuid::from_u128(6),"provider":"synthetic","model":"synthetic-model","scope":Uuid::from_u128(5),
        "currency":"USD","unit":"PerMillionTokens","rates":rates,
        "source_reference":Uuid::from_u128(7),"source_kind":"NativeCatalog",
        "observed_at_ms":0,"approved_at_ms":0,"effective_from_ms":0,"effective_end_ms":null,
        "basis":basis
    });
    if let Some(source) = basis_source {
        value["basis_source"] = serde_json::json!(source);
    }
    if let Some(burn) = plan_burn_millis {
        value["plan_burn_millis"] = serde_json::json!(burn);
    }
    serde_json::from_value(value).unwrap()
}

const PRICE_METADATA: [&str; 6] = [
    "Price ID",
    "Price source",
    "Price currency/unit",
    "Price observed/approved",
    "Price effective interval",
    "Rate unavailable for",
];

/// PF-60-S04: a request whose record states only its billing basis - no rate
/// and no plan rate - has no price, so its detail says so instead of showing a
/// price source, price metadata and per-rate gaps as if a price existed.
#[test]
fn accounting_inspect_basis_only_request_shows_no_price_metadata() {
    let none = serde_json::json!({"noncached":null,"read":null,"write":null,"output":null});
    for (basis, source, shown) in [
        ("PlanEquivalent", None, "subscription (built-in default)"),
        ("Billed", None, "pay per use (built-in default)"),
        ("Local", None, "local, no charge (built-in default)"),
        // An undeclared basis has no default to name.
        ("Undeclared", None, "not declared"),
        (
            "PlanEquivalent",
            Some("UserConfig"),
            "subscription (set in your config)",
        ),
    ] {
        let mut q = quote();
        q.snapshot = Some(native_snapshot(
            none.clone(),
            basis,
            source,
            /*plan_burn_millis*/ None,
        ));
        let lines = attempt_text(&q);
        let text = lines.join("\n");
        assert!(lines.contains(&format!("Billing basis: {shown}")), "{text}");
        // #396: no parentheses inside the sentence's own clause.
        let clause = shown
            .replace(" (set in your config)", "; set in your config")
            .replace(" (built-in default)", "; built-in default");
        assert!(
            text.contains(&format!(
                "Price: none recorded — this request records only its billing basis: {clause}"
            )),
            "{text}"
        );
        assert!(!text.contains("(set in your config))"), "{text}");
        assert!(!text.contains("(built-in default))"), "{text}");
        for absent in PRICE_METADATA
            .iter()
            .chain(&["no dispatch-time price snapshot"])
        {
            assert!(!text.contains(absent), "{basis}: {absent}\n{text}");
        }
    }
}

/// PF-60-S04: a record that states any price - an API rate, or only a plan
/// rate (a subscription row with no API twin) - keeps its full price block.
#[test]
fn accounting_inspect_priced_or_plan_rate_request_keeps_price_metadata() {
    let none = serde_json::json!({"noncached":null,"read":null,"write":null,"output":null});
    let one_rate = serde_json::json!({"noncached":"1","read":null,"write":null,"output":null});
    for snapshot in [
        native_snapshot(
            none,
            "PlanEquivalent",
            /*basis_source*/ None,
            Some(1000),
        ),
        native_snapshot(
            one_rate.clone(),
            "Billed",
            /*basis_source*/ None,
            /*plan_burn_millis*/ None,
        ),
        native_snapshot(
            one_rate,
            "PlanEquivalent",
            /*basis_source*/ None,
            /*plan_burn_millis*/ None,
        ),
    ] {
        let mut q = quote();
        // A request copies the plan rate from its record.
        q.plan_burn_millis = snapshot.plan_burn_millis;
        q.snapshot = Some(snapshot);
        let text = attempt_text(&q).join("\n");
        if q.plan_burn_millis.is_some() {
            assert!(text.contains("Plan rate at dispatch: 1x"), "{text}");
        }
        for present in &PRICE_METADATA[..5] {
            assert!(text.contains(present), "{present}\n{text}");
        }
        assert!(!text.contains("Price: none recorded"), "{text}");
    }
}

#[tokio::test]
async fn accounting_inspect_narrow_and_long_fields() {
    for width in [40, 80] {
        let (mut chat, _rx, _ops) = make_chatwidget_manual(/*model_override*/ None).await;
        let mut q = quote();
        q.attempt.provider = format!("provider\u{1b}[31m{}", "LONG".repeat(30));
        let page = InspectorPage {
            title: "Attempt".into(),
            text: attempt_text(&q),
            links: vec![],
            parent: None,
            selected: Arc::default(),
        };
        let inspector = Inspector {
            generation: Uuid::new_v4(),
            thread: None,
            day: 0,
            range: None,
            pages: vec![page],
            page: 0,
            history: Vec::new(),
            alive: Arc::new(std::sync::atomic::AtomicBool::new(true)),
        };
        let params = inspector.params(usize::from(width));
        let names: Vec<_> = params.items.iter().map(|i| i.name.clone()).collect();
        assert!(names.iter().all(|s| !s.chars().any(char::is_control)));
        chat.bottom_pane.show_selection_view(params);
        // Every selectable fragment can actually be reached in a small viewport.
        for name in names {
            let screen = render_bottom_popup_with_height(&chat, width, /*height*/ 12);
            assert!(
                screen.contains(&name),
                "{name:?} missing at {width}: {screen}"
            );
            chat.handle_key_event(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
        }
    }
}

#[tokio::test]
async fn accounting_inspect_empty_day_after_checkpoint_renders_lag() -> anyhow::Result<()> {
    use codex_state::accounting::AccountingStore;

    let home = tempfile::tempdir()?;
    // Stored before the id is minted, so it is the conversation's start.
    let created = chrono::Utc::now();
    let owner = ThreadId::new();
    let runtime = codex_state::StateRuntime::init(
        codex_state::SqliteConfig::from_sqlite_home(
            codex_utils_absolute_path::AbsolutePathBuf::try_from(home.path().to_path_buf())?,
        ),
        "synthetic".into(),
    )
    .await?;
    let metadata = codex_state::ThreadMetadataBuilder::new(
        owner,
        home.path().join("synthetic.jsonl"),
        created,
        codex_protocol::protocol::SessionSource::Cli,
    );
    runtime.upsert_thread(&metadata.build("synthetic")).await?;
    AccountingStore::open(&runtime, /*as_of*/ 0).await?;
    let empty =
        AccountingStore::inspect_day(&runtime, owner, /*utc_day*/ 0, /*read_at_ms*/ 0).await?;
    let InspectionDay::Ready(view) = &empty else {
        panic!("{empty:?}");
    };
    assert_eq!(view.totals, DayTotals::default());
    assert!(view.requests.is_empty());
    assert_eq!(view.coverage.completed_as_of_ms, 0);
    // FUP-02: the conversation started today, so 1970-01-01 provably had
    // none of its requests; and no other conversation recorded anything.
    assert_eq!(
        inspection_pages(Ok(empty))[0].text[..2].to_vec(),
        vec![
            format!(
                "This conversation on 1970-01-01 (UTC): no requests recorded — this conversation started on {} (UTC), after this day.",
                created.date_naive()
            ),
            "No other conversation recorded requests on this day.".to_string(),
        ]
    );

    // The next UTC day has no requests and opening the inspector must not maintain it.
    for _ in 0..2 {
        let lagged = AccountingStore::inspect_day(
            &runtime, owner, /*utc_day*/ 1, /*read_at_ms*/ 86_400_000,
        )
        .await?;
        let text = inspection_pages(Ok(lagged))[0].text.join("\n");
        assert!(
            text.contains("Snapshot is not current; newer activity is unverified"),
            "{text}"
        );
        assert!(
            !text.contains("stored contributions need refresh"),
            "{text}"
        );
        assert!(!text.contains('$'), "{text}");
    }
    runtime.close().await;
    Ok(())
}

// Admit synthetic evidence, then model a late raw import and interrupted
// estimate/contribution persistence without adding a production mutation API.
async fn maintenance_inspection(mutation: &str) -> anyhow::Result<InspectionDay> {
    use codex_state::accounting::AccountingStore;
    use codex_state::accounting::RetainedDay;

    const CHECKPOINT: i64 = 100 * 86_400_000;
    let home = tempfile::tempdir()?;
    let runtime = codex_state::StateRuntime::init(
        codex_state::SqliteConfig::from_sqlite_home(
            codex_utils_absolute_path::AbsolutePathBuf::try_from(home.path().to_path_buf())?,
        ),
        "synthetic".into(),
    )
    .await?;
    let mut recent = quote().attempt;
    recent.retry_of = None;
    recent.dispatched_at_ms = CHECKPOINT.try_into()?;
    let owner = recent.thread_id;
    let metadata = codex_state::ThreadMetadataBuilder::new(
        owner,
        home.path().join("synthetic.jsonl"),
        chrono::Utc::now(),
        codex_protocol::protocol::SessionSource::Cli,
    );
    runtime.upsert_thread(&metadata.build("synthetic")).await?;
    let store = AccountingStore::open(&runtime, CHECKPOINT).await?;
    store.admit(owner, &recent, &[], CHECKPOINT).await?;
    let mut late = recent.clone();
    late.attempt_id = Uuid::from_u128(8);
    late.request_id = Uuid::from_u128(9);
    store.admit(owner, &late, &[], CHECKPOINT).await?;
    let mutation = format!(
        "UPDATE draft_accounting_attempts SET payload = json_set(payload, '$.dispatched_at_ms', 0) WHERE attempt_id = '{id}';
         UPDATE draft_accounting_estimates SET payload = json_set(payload, '$.attempt.dispatched_at_ms', 0) WHERE attempt_id = '{id}';
         UPDATE draft_accounting_contributions SET utc_day = 0 WHERE attempt_id = '{id}';
         {mutation}",
        id = late.attempt_id,
    );
    // TUI has no SQL dependency. Python is already required by the guarded test
    // runner; its standard sqlite3 module touches only this disposable fixture.
    let output = tokio::process::Command::new(if cfg!(windows) { "python" } else { "python3" })
        .arg("-c")
        .arg("import sqlite3, sys; c = sqlite3.connect(sys.argv[1]); c.executescript(sys.argv[2]); c.close()")
        .arg(runtime.sqlite().state_db_path())
        .arg(mutation)
        .output()
        .await?;
    assert!(output.status.success(), "{output:?}");
    assert_eq!(
        store.read_day(owner, /*utc_day*/ 100, CHECKPOINT).await?,
        RetainedDay::NeedsMaintenance {
            completed_as_of_ms: CHECKPOINT
        }
    );
    let result = AccountingStore::inspect_day(&runtime, owner, /*utc_day*/ 100, CHECKPOINT).await?;
    assert_eq!(
        AccountingStore::inspect_day(&runtime, owner, /*utc_day*/ 100, CHECKPOINT).await?,
        result
    );
    runtime.close().await;
    Ok(result)
}

#[tokio::test]
async fn accounting_inspect_maintenance_with_stale_raw_renders_refresh() -> anyhow::Result<()> {
    for mutation in [
        "DELETE FROM draft_accounting_contributions WHERE utc_day = 100",
        "DELETE FROM draft_accounting_contributions WHERE utc_day = 100; DELETE FROM draft_accounting_estimates WHERE attempt_id = '00000000-0000-0000-0000-000000000001'",
    ] {
        let result = maintenance_inspection(mutation).await?;
        let text = inspection_pages(Ok(result))[0].text.join("\n");
        insta::allow_duplicates! {
            insta::assert_snapshot!(text, @"
            Recorded totals unavailable — stored contributions need refresh. Retry rereads only; no repair performed.
            Next step: select Refresh. If it stays unavailable, the recorded totals cannot be checked here; check your provider's bill.
            Collection coverage: unknown; recorded root and resolved descendants only. Unknown parent population excluded.
            Logical requests may have attempts on other days; this UTC day is not their complete lifetime.
            ");
        }
    }
    Ok(())
}

#[tokio::test]
async fn accounting_inspect_maintenance_with_current_contributions_renders_raw_totals()
-> anyhow::Result<()> {
    let result = maintenance_inspection("").await?;
    let text = inspection_pages(Ok(result))[0].text.join("\n");
    // What this pins changed with `5c0634ed3`, which bounded inspection by the
    // selected scope instead of consulting the stored aggregate's
    // `RetainedDay`. Before it, a day whose aggregate needed maintenance
    // returned `CheckpointLag` and the page said only that the snapshot was
    // not current - which is what the old assertion here held, correctly, when
    // it was written. It kept asserting that for six days after the behaviour
    // moved, and I reported the failure as pre-existing rather than reading it.
    //
    // The behaviour that replaced it is the one worth having: the totals come
    // from the raw attempts in scope, so a pending aggregate does not make
    // them wrong, and an attempt whose contribution is NOT current still
    // returns `NeedsRefresh` - the sibling test pins that. What must never
    // happen is this page presenting an authoritative total for a day it
    // cannot complete, so the assertions below pin the disclosure rather than
    // only the pixels.
    insta::assert_snapshot!(text, @"
    This conversation on 1970-04-11 (UTC):
    • synthetic · synthetic-model — Pay per use. 1 request, tokens not reported. Estimated cost: no price available.
    No other conversation recorded requests on this day.
    Next step for requests with no price: check the bill from synthetic. No published price covers them, so no cost is shown for them here.
    Costs are estimates from published prices; synthetic doesn't report its charges — check synthetic's bill.
    Select a provider below to see its requests.
    —— Details ——
    Estimated token cost: unknown
    Full recorded estimate: unavailable (1 of 1 billed attempts incomplete)
    Collection coverage: unknown; recorded root and resolved descendants only. Unknown parent population excluded.
    Logical requests may have attempts on other days; this UTC day is not their complete lifetime.
    Day covered (UTC): [1970-04-11T00:00:00.000Z, 1970-04-12T00:00:00.000Z)
    Read at: 1970-04-11T00:00:00.000Z; ledger current to: 1970-04-11T00:00:00.000Z (0 ms behind)
    Retention: request detail kept since 1970-01-11T00:00:00.000Z; daily totals kept since 1970-01-01; oldest recorded day in this conversation 1970-01-01
    Known subtotal exact USD: none — no price for these attempts
    Input: not reported (1 attempt)
    Noncached input (derived for inclusive input): not reported (1 attempt)
    Cache read: not reported (1 attempt)
    Cache write: not reported (1 attempt)
    Output: not reported (1 attempt)
    Reasoning (subset, not separately billed): not reported (1 attempt)
    Total (not separately billed): not reported (1 attempt)
    Root total = own attempts + resolved descendant attempts. Provider/model groups partition the same root total. Compare exact USD, not rounded displays.
    Unknown parent population: 0 attempts, excluded from root total
    ");
    // The fixture's day-100 attempt was admitted with no observations, so the
    // page must say it cannot complete the estimate rather than presenting the
    // zero it could compute.
    assert!(
        text.contains("Estimated token cost: unknown")
            && text.contains(
                "Full recorded estimate: unavailable (1 of 1 billed attempts incomplete)"
            ),
        "a day it cannot complete is never presented as a total: {text}"
    );
    // And it states no staleness, because the read and the checkpoint are the
    // same instant. A page that warned here would be crying wolf on every
    // healthy day.
    assert!(
        !text.contains("Snapshot is not current"),
        "nothing is stale at the checkpoint: {text}"
    );
    Ok(())
}

#[test]
fn accounting_inspect_availability_state_snapshots() {
    let states = [
        InspectionDay::Absent,
        InspectionDay::MissingThread,
        InspectionDay::CheckpointLag,
        InspectionDay::NeedsRefresh,
        InspectionDay::TooLarge,
    ];
    let text: Vec<_> = states
        .into_iter()
        .map(|s| inspection_pages(Ok(s))[0].text.first().unwrap().clone())
        .collect();
    assert_eq!(text[0], super::absent_state());
    insta::assert_snapshot!(text[1..].join("\n"), @"
    Unavailable — native thread no longer exists.
    Snapshot is not current; newer activity is unverified
    Recorded totals unavailable — stored contributions need refresh. Retry rereads only; no repair performed.
    Range too large for this inspector. No total shown.
    ");
    let InspectionDay::Ready(mut empty) = packet() else {
        unreachable!()
    };
    empty.totals = DayTotals::default();
    empty.requests.clear();
    assert_eq!(
        inspection_pages(Ok(InspectionDay::Ready(empty)))[0].text[..2],
        [
            "This conversation on 1970-01-01 (UTC): nothing recorded.",
            super::empty_day::NOT_PROOF,
        ]
    );
    let InspectionDay::Ready(view) = packet() else {
        unreachable!()
    };
    let page = inspection_pages(Ok(InspectionDay::DetailUnavailable {
        coverage: view.coverage,
        read_at_ms: 90 * 86_400_000,
        compact: true,
    }));
    let text = page[0].text.join("\n");
    assert!(text.contains("compacted history lost request/provider attribution"));
    assert!(!text.contains('$'));
    for error in [
        "remote connection",
        "corrupt evidence",
        "inspection timed out",
    ] {
        assert!(
            inspection_pages(Err(error.into()))[0]
                .text
                .contains(&error.to_owned())
        );
    }
}

#[test]
fn accounting_inspect_coverage_never_claims_run_complete() {
    let InspectionDay::Ready(mut view) = packet() else {
        unreachable!()
    };
    view.totals.unknown_estimates = 0;
    let text = inspection_pages(Ok(InspectionDay::Ready(view)))[0]
        .text
        .join("\n");
    // The plain overview leads; the ledger's own totals open the detail.
    let (overview, details) = text.split_once("—— Details ——\n").unwrap();
    assert!(overview.starts_with("This conversation on 1970-01-01 (UTC):"));
    assert!(details.starts_with("Estimated token cost for recorded attempts:"));
    for caveat in [
        "Collection coverage: unknown",
        "resolved descendants only",
        SYNTHETIC_NOT_STATED,
        "other days",
        "Snapshot is not current; newer activity is unverified",
    ] {
        assert!(text.contains(caveat));
    }
}

#[tokio::test]
async fn accounting_inspect_snapshot_navigation_roundtrip() {
    let (mut chat, mut rx, _ops) = make_chatwidget_manual(/*model_override*/ None).await;
    chat.open_accounting_inspector(/*day*/ 0);
    let AppEvent::LoadAccountingInspector {
        generation,
        thread,
        day,
        ..
    } = rx.try_recv().unwrap()
    else {
        panic!()
    };
    chat.finish_accounting_inspector(generation, thread, day, Ok(packet()));
    let original = chat.accounting_inspector.as_ref().unwrap().pages[0]
        .text
        .clone();
    for page in [1, 2, 1, 0] {
        chat.navigate_accounting_inspector(generation, page);
        assert_eq!(chat.accounting_inspector.as_ref().unwrap().page, page);
        chat.handle_key_event(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
    }
    assert_eq!(
        chat.accounting_inspector.as_ref().unwrap().pages[0].text,
        original
    );
    chat.navigate_accounting_inspector(generation, /*page*/ 2);
    chat.navigate_accounting_inspector(generation, /*page*/ 0);
    assert_eq!(
        chat.bottom_pane
            .selected_index_for_active_view(INSPECTOR_VIEW),
        Some(1)
    );
    assert!(render_bottom_popup(&chat, /*width*/ 40).contains("Cost — this conversation"));
}

fn breakdown_packet() -> InspectionDay {
    let InspectionDay::Ready(mut view) = packet() else {
        unreachable!()
    };
    view.requests.clear();
    // Renderer boundary fixture: the installed ledger currently requires nonempty
    // attribution; missing strings here exercise the explicit unknown fallback.
    for (id, micros, provider, model) in [
        (1, 1, "alpha", "one"),
        (2, 2, "alpha", "two"),
        (3, 4, "", ""),
        (4, 8, "beta", "three"),
    ] {
        let mut q = quote();
        q.attempt.attempt_id = Uuid::from_u128(id);
        q.attempt.request_id = Uuid::from_u128(100 + id);
        q.attempt.thread_id = if id == 1 {
            view.owner
        } else {
            ThreadId::from_string(&Uuid::from_u128(200 + id).to_string()).unwrap()
        };
        q.attempt.provider = provider.into();
        q.attempt.model = model.into();
        q.known_subtotal = decimal(&format!("0.00000{micros}"));
        q.all_buckets_priced = Some(q.known_subtotal);
        if id == 4 {
            view.unknown_parent_requests
                .insert(q.attempt.request_id, vec![q]);
        } else {
            view.requests.insert(q.attempt.request_id, vec![q]);
        }
    }
    view.totals = DayTotals::from_quotes(view.requests.values().flatten()).unwrap();
    view.own_totals = DayTotals::from_quotes(
        view.requests
            .values()
            .flatten()
            .filter(|q| q.attempt.thread_id == view.owner),
    )
    .unwrap();
    view.descendant_totals = DayTotals::from_quotes(
        view.requests
            .values()
            .flatten()
            .filter(|q| q.attempt.thread_id != view.owner),
    )
    .unwrap();
    view.unknown_parent_totals =
        DayTotals::from_quotes(view.unknown_parent_requests.values().flatten()).unwrap();
    InspectionDay::Ready(view)
}

#[test]
fn accounting_inspect_rendered_tree_and_provider_model_reconcile() {
    let pages = inspection_pages(Ok(breakdown_packet()));
    let group = |title: &str| pages.iter().find(|p| p.title == title).unwrap();
    let exact_micros = |p: &InspectorPage| -> u64 {
        let line = p
            .text
            .iter()
            .find_map(|s| s.strip_prefix("Known estimate exact USD: "))
            .unwrap();
        // Independent integer parsing of the rendered fixture values.
        line.strip_prefix("0.").unwrap().parse::<u64>().unwrap() * 10_u64.pow(8 - line.len() as u32)
    };
    assert_eq!(exact_micros(group("Root's own attempts")), 1);
    assert_eq!(exact_micros(group("Descendant attempts")), 6);
    assert!(
        pages[0]
            .text
            .contains(&"Known subtotal exact USD: 0.000007".into())
    );
    let provider_pages: Vec<_> = pages
        .iter()
        .filter(|p| p.title.starts_with("Provider:"))
        .collect();
    assert_eq!(
        provider_pages.iter().map(|p| exact_micros(p)).sum::<u64>(),
        7
    );
    assert_eq!(
        provider_pages.iter().map(|p| p.links.len()).sum::<usize>(),
        3
    );
    assert_eq!(
        exact_micros(group(
            "Provider: unknown (attribution absent); Model: unknown (attribution absent)"
        )),
        4
    );
    let unknown = group("Unknown parent population");
    assert_eq!(unknown.links.len(), 1);
    assert!(unknown.text.iter().any(|s| s.contains("$0.000008")));
    assert!(
        pages[unknown.links[0].1]
            .text
            .contains(&format!("Attempt: {}", Uuid::from_u128(4)))
    );
    for group in provider_pages {
        for (_, request) in &group.links {
            assert_eq!(pages[*request].title, "Request");
            assert_eq!(
                pages[pages[*request].links[0].1].title,
                "Attempt, components and original price"
            );
        }
    }
    insta::assert_snapshot!(pages.iter().filter(|p| p.title.starts_with("Provider:") || p.title.ends_with("attempts")).map(|p| format!("{}\n{}", p.title, p.text.iter().filter(|s| s.starts_with("Estimated token cost") || s.starts_with("Known estimate exact") || s.starts_with("Recorded attempts:")).cloned().collect::<Vec<_>>().join("\n"))).collect::<Vec<_>>().join("\n"), @"Root's own attempts\nEstimated token cost for recorded attempts: $0.000001\nRecorded attempts: 1\nKnown estimate exact USD: 0.000001\nDescendant attempts\nEstimated token cost for recorded attempts: $0.000006\nRecorded attempts: 2\nKnown estimate exact USD: 0.000006\nProvider: unknown (attribution absent); Model: unknown (attribution absent)\nEstimated token cost for recorded attempts: $0.000004\nRecorded attempts: 1\nKnown estimate exact USD: 0.000004\nProvider: alpha; Model: one\nEstimated token cost for recorded attempts: $0.000001\nRecorded attempts: 1\nKnown estimate exact USD: 0.000001\nProvider: alpha; Model: two\nEstimated token cost for recorded attempts: $0.000002\nRecorded attempts: 1\nKnown estimate exact USD: 0.000002");
}

#[test]
fn accounting_inspect_group_partial_price_stays_unknown() {
    let InspectionDay::Ready(mut view) = breakdown_packet() else {
        unreachable!()
    };
    view.requests.get_mut(&Uuid::from_u128(102)).unwrap()[0].all_buckets_priced = None;
    view.totals = DayTotals::from_quotes(view.requests.values().flatten()).unwrap();
    view.descendant_totals.unknown_estimates = 1;
    let pages = inspection_pages(Ok(InspectionDay::Ready(view)));
    for title in ["Descendant attempts", "Provider: alpha; Model: two"] {
        let page = pages.iter().find(|p| p.title == title).unwrap();
        assert!(
            page.text.iter().any(|s| s.contains("+ unknown costs")),
            "{title}"
        );
        assert!(
            page.text
                .iter()
                .any(|s| s.starts_with("Full recorded estimate: unavailable")),
            "{title}"
        );
    }
}

#[test]
fn accounting_inspect_unknown_unavailable_note() {
    let InspectionDay::Ready(mut view) = packet() else {
        unreachable!()
    };
    view.unknown_parent_unavailable_threads = 3;
    let pages = inspection_pages(Ok(InspectionDay::Ready(view)));
    let unknown = pages
        .iter()
        .find(|p| p.title == "Unknown parent population")
        .unwrap();
    let note = "Unresolved ancestry: 3 threads have unavailable day detail; their costs and retention coverage are unknown and excluded from this root.";
    assert!(pages[0].text.iter().any(|s| s == note));
    assert!(unknown.text.iter().any(|s| s == note));
    insta::assert_snapshot!(unknown.text.iter().filter(|s| s.starts_with("Unresolved ancestry:") || s.starts_with("Estimates below")).cloned().collect::<Vec<_>>().join("\n"), @"
    Unresolved ancestry: 3 threads have unavailable day detail; their costs and retention coverage are unknown and excluded from this root.
    Estimates below cover inspectable attempts only; unavailable threads may have additional unknown costs.
    ");
}

#[test]
fn accounting_zero_plan_usage_renders_money_unavailable() {
    let mut q = quote();
    q.attempt.provider = "claude-plan".into();
    q.usage = Usage {
        input: Some(0),
        noncached: Some(0),
        read: Some(0),
        write: Some(0),
        output: Some(0),
        ..Usage::default()
    };
    q.buckets = [BucketQuote::MissingRate; 4];
    q.known_subtotal = Decimal::default();
    q.all_buckets_priced = None;
    q.subtotal_display = Decimal::default().display();
    let lines = attempt_text(&q);
    assert!(!lines.iter().any(|line| line.contains("$0.000000")));
    insta::assert_snapshot!(lines.iter().filter(|line| line.starts_with("Token cost:")).cloned().collect::<Vec<_>>().join("\n"),
        @"Token cost: unavailable — no applicable price; recorded usage is not a zero-cost claim.");
}

/// The overview's one billing line for the `packet()` day, whose provider
/// states no charge.
const SYNTHETIC_NOT_STATED: &str = "Costs are estimates from published prices; synthetic doesn't report its charges — check synthetic's bill.";

#[test]
fn accounting_inspect_estimate_only_never_invents_billed_or_difference() {
    let pages = inspection_pages(Ok(breakdown_packet()));
    // The first screen says it once, in its overview (one provider here is
    // unnamed, so it names none); every other page with attempts says it once
    // in its details, naming whose bill to check where it can.
    let alpha = "Billed cost: alpha doesn't report its charges, so any cost here is an estimate — check alpha's bill.";
    assert!(
        pages[0].text.contains(
            &"Costs are estimates from published prices; your provider's bill is the final amount."
                .into()
        ),
        "{:#?}",
        pages[0].text
    );
    assert!(!pages[0].text.iter().any(|s| s.starts_with("Billed cost:")));
    for page in &pages {
        let billed: Vec<_> = page
            .text
            .iter()
            .filter(|s| s.starts_with("Billed cost:"))
            .collect();
        assert!(billed.len() <= 1, "{}: {billed:?}", page.title);
        assert!(
            billed.iter().all(|s| !s.starts_with("Billed cost: $")),
            "{billed:?}"
        );
        assert!(
            !page
                .text
                .iter()
                .any(|s| s.contains("settlement") || s.contains("billed difference")),
            "{}",
            page.title
        );
    }
    assert!(pages.iter().any(|page| page.text.contains(&alpha.into())));
    assert!(
        pages[0]
            .text
            .contains(&"Estimated token cost for recorded attempts: $0.000007".into())
    );
    let mut empty = quote();
    empty.known_subtotal = Decimal::default();
    empty.all_buckets_priced = None;
    assert!(attempt_text(&empty).contains(&"Estimated token cost: unknown".into()));
}

#[test]
fn accounting_inspect_breakdown_freshness_and_empty_attribution() {
    let pages = inspection_pages(Ok(packet()));
    for page in &pages {
        assert!(
            page.text
                .contains(&"Snapshot is not current; newer activity is unverified".into()),
            "{}",
            page.title
        );
    }
    let unknown = pages
        .iter()
        .find(|p| p.title == "Unknown provider/model attribution")
        .unwrap();
    assert!(
        unknown
            .text
            .iter()
            .any(|s| s.contains("No recorded attempts"))
    );
    assert!(!unknown.text.iter().any(|s| s.contains('$')));
    for state in [
        InspectionDay::NeedsRefresh,
        InspectionDay::CheckpointLag,
        InspectionDay::TooLarge,
    ] {
        let pages = inspection_pages(Ok(state));
        assert_eq!(pages.len(), 1);
        assert!(pages[0].links.is_empty());
        assert!(!pages[0].text.iter().any(|s| s.contains('$')));
    }
}

#[tokio::test]
async fn accounting_inspect_breakdown_navigation_reuses_packet() {
    let (mut chat, mut rx, _ops) = make_chatwidget_manual(/*model_override*/ None).await;
    chat.open_accounting_inspector(/*day*/ 0);
    let AppEvent::LoadAccountingInspector {
        generation,
        thread,
        day,
        ..
    } = rx.try_recv().unwrap()
    else {
        panic!()
    };
    chat.finish_accounting_inspector(generation, thread, day, Ok(breakdown_packet()));
    let links = chat.accounting_inspector.as_ref().unwrap().pages[0]
        .links
        .clone();
    for (_, target) in links {
        chat.navigate_accounting_inspector(generation, target);
        assert_eq!(chat.accounting_inspector.as_ref().unwrap().page, target);
        chat.navigate_accounting_inspector(generation, /*page*/ 0);
    }
    assert!(!matches!(
        rx.try_recv(),
        Ok(AppEvent::LoadAccountingInspector { .. })
    ));
}

#[test]
fn loaded_state_freezes_chart_anchor_date_at_completion() {
    let state = Arc::new(RwLock::new(TokenActivityState::Loading));
    let handle = TokenActivityHandle {
        state: Arc::clone(&state),
    };
    let today =
        NaiveDate::from_ymd_opt(/*year*/ 2026, /*month*/ 5, /*day*/ 29).expect("valid date");

    handle.finish_with_today(
        Ok(GetAccountTokenUsageResponse {
            summary: AccountTokenUsageSummary {
                lifetime_tokens: None,
                peak_daily_tokens: None,
                longest_running_turn_sec: None,
                current_streak_days: None,
                longest_streak_days: None,
            },
            daily_usage_buckets: None,
        }),
        today,
    );

    let state = state.read().expect("token activity state poisoned");
    match &*state {
        TokenActivityState::Loaded {
            today: loaded_today,
            ..
        } => {
            assert_eq!(*loaded_today, today);
        }
        other => panic!("expected loaded state, got {other:?}"),
    }
}

/// A blank money figure has two very different causes - an attempt that
/// recorded no tokens, and tokens recorded with no rate to price them - and
/// they are fixed in different places. The view names the second kind, and
/// says nothing about why the rate was absent, because the ledger does not
/// know: a catalogue with no rate for the row, an unattributable credential
/// and a service tier the rates are not quoted for all look identical here.
#[test]
fn accounting_inspect_names_the_rows_whose_tokens_have_no_price() {
    let priced = quote();
    let mut unpriced = quote();
    unpriced.attempt.provider = "openai".into();
    unpriced.attempt.model = "gpt-5.5".into();
    unpriced.snapshot = None;
    unpriced.known_subtotal = Decimal::default();
    unpriced.known_equivalent = Decimal::default();
    // The pricer reads `noncached`, not `input`; the buckets below are only
    // meaningful beside the counts they priced.
    unpriced.usage = Usage {
        input: Some(120),
        noncached: Some(100),
        read: Some(20),
        output: Some(40),
        ..Usage::default()
    };
    unpriced.buckets = [
        BucketQuote::MissingRate,
        BucketQuote::MissingRate,
        BucketQuote::MissingRate,
        BucketQuote::MissingRate,
    ];
    let mut also_unpriced = unpriced.clone();
    also_unpriced.attempt.model = "gpt-6-astra".into();
    // The same row twice is named once.
    let duplicate = unpriced.clone();
    // An attempt that recorded no tokens at all is a different gap: nothing
    // was recorded, so there is nothing here to say went unpriced.
    let mut no_usage = unpriced.clone();
    no_usage.attempt.model = "recorded-nothing".into();
    no_usage.usage = Usage::default();
    no_usage.buckets = [
        BucketQuote::MissingUsage,
        BucketQuote::MissingUsage,
        BucketQuote::MissingUsage,
        BucketQuote::MissingUsage,
    ];
    // A bucket priced at zero is not money stated. A provider that reports a
    // cache bucket as `0` rather than omitting it must not hide a row whose
    // real tokens went unpriced.
    let mut zero_bucket = unpriced.clone();
    zero_bucket.attempt.model = "zero-cache-bucket".into();
    zero_bucket.usage = Usage {
        read: Some(0),
        ..unpriced.usage
    };
    zero_bucket.buckets = [
        BucketQuote::MissingRate,
        BucketQuote::Priced(Decimal::default()),
        BucketQuote::Priced(Decimal::default()),
        BucketQuote::MissingRate,
    ];
    // An attempt whose recorded counts are all zero cost nothing whatever the
    // rate, so it is not named either.
    let mut zero_usage = unpriced.clone();
    zero_usage.attempt.model = "recorded-zeroes".into();
    zero_usage.usage = Usage {
        input: Some(0),
        noncached: Some(0),
        read: Some(0),
        output: Some(0),
        ..Usage::default()
    };
    // A rate of zero is a price. An attempt whose recorded tokens were priced
    // by one states no money and is not missing a price.
    let mut zero_rate = unpriced.clone();
    zero_rate.attempt.model = "priced-at-zero".into();
    zero_rate.buckets = [
        BucketQuote::Priced(Decimal::default()),
        BucketQuote::MissingRate,
        BucketQuote::MissingRate,
        BucketQuote::MissingRate,
    ];
    // The same model can have priced and unpriced attempts in one day - a rate
    // whose window starts mid-day, or a different credential - so the claim is
    // about attempts, never about the model.
    let mut mixed_priced = unpriced.clone();
    mixed_priced.attempt.model = "mixed".into();
    mixed_priced.buckets = [
        BucketQuote::Priced(decimal("0.00002")),
        BucketQuote::Priced(decimal("0.00001")),
        BucketQuote::MissingUsage,
        BucketQuote::Priced(decimal("0.00016")),
    ];
    mixed_priced.known_subtotal = decimal("0.00019");
    let mut mixed_unpriced = unpriced.clone();
    mixed_unpriced.attempt.model = "mixed".into();
    // Subscription work is not billed per token. Its gap is a missing API
    // equivalent, which the plan lines state in their own terms, and calling
    // it missing money would contradict them.
    let mut plan_attempt = unpriced.clone();
    plan_attempt.attempt.model = "plan-burn-only".into();
    plan_attempt.plan_burn_millis = Some(1000);
    plan_attempt.plan_burn_milli_tokens = Some(160_000);
    // A partly priced attempt is not named: its money is stated, and the
    // structural cache-write gap would otherwise name almost every row.
    let mut partly_priced = unpriced.clone();
    partly_priced.attempt.model = "partly-priced".into();
    partly_priced.known_subtotal = decimal("0.00002");
    partly_priced.buckets = [
        BucketQuote::Priced(decimal("0.00002")),
        BucketQuote::MissingRate,
        BucketQuote::MissingRate,
        BucketQuote::MissingRate,
    ];

    let lines = super::unpriced_rows(
        [
            &priced,
            &unpriced,
            &also_unpriced,
            &duplicate,
            &no_usage,
            &zero_bucket,
            &zero_usage,
            &plan_attempt,
            &zero_rate,
            &mixed_priced,
            &mixed_unpriced,
            &partly_priced,
        ],
        /*named*/ false,
    );

    assert_eq!(
        lines.len(),
        1,
        "one line names every unpriced row: {lines:?}"
    );
    assert!(
        lines[0].contains("openai/gpt-5.5") && lines[0].contains("openai/gpt-6-astra"),
        "both rows are named: {lines:?}"
    );
    assert!(
        !lines[0].contains("synthetic-model"),
        "a priced row is not named as unpriced: {lines:?}"
    );
    assert!(
        !lines[0].contains("recorded-nothing"),
        "an attempt that recorded no tokens is a different gap: {lines:?}"
    );
    assert!(
        lines[0].contains("zero-cache-bucket"),
        "a bucket priced at zero does not hide an unpriced row: {lines:?}"
    );
    assert!(
        !lines[0].contains("recorded-zeroes"),
        "zero tokens cost nothing whatever the rate: {lines:?}"
    );
    assert!(
        !lines[0].contains("priced-at-zero"),
        "a rate of zero is a price: {lines:?}"
    );
    // The mixed model is named, and named as attempts rather than as the row,
    // because some of its attempts were priced.
    assert!(
        lines[0].contains("1 on OpenAI · mixed (openai/mixed)"),
        "the claim counts attempts, not models: {lines:?}"
    );
    assert!(
        lines[0].contains("says nothing about the other attempts on the same model"),
        "and says so: {lines:?}"
    );
    assert!(
        !lines[0].contains("plan-burn-only"),
        "subscription work is not billed per token and has no missing money: {lines:?}"
    );
    assert!(
        !lines[0].contains("partly-priced"),
        "a partly priced attempt states money and is not named: {lines:?}"
    );
    assert_eq!(lines[0].matches("openai/gpt-5.5").count(), 1);
    // The line says what is missing, never why. Every cause the ledger cannot
    // tell apart is offered, and none is asserted.
    for cause in [
        "catalogue states no rate",
        "credential",
        "in effect at dispatch",
    ] {
        assert!(
            lines[0].contains(cause),
            "the cause `{cause}` is offered: {lines:?}"
        );
    }
    assert!(
        lines[0].contains("or because"),
        "the causes are alternatives, not an assertion: {lines:?}"
    );
    assert!(
        lines[0].contains("not a zero-cost claim"),
        "a blank is not a zero: {lines:?}"
    );

    // Nothing to say when everything carried a rate.
    assert!(super::unpriced_rows([&priced], /*named*/ false).is_empty());
}

/// The line has to land on the page the operator actually reads - the one
/// showing the blank it explains - not only on the group pages beneath it.
/// Pinning the helper alone let the placement regress silently once already.
#[test]
fn accounting_inspect_day_entry_page_names_rows_whose_tokens_have_no_price() {
    let InspectionDay::Ready(mut view) = packet() else {
        panic!("packet is a ready day")
    };
    let quote = view
        .requests
        .values_mut()
        .flatten()
        .next()
        .expect("the fixture has one attempt");
    quote.attempt.provider = "openai".into();
    quote.attempt.model = "gpt-5.5".into();
    quote.snapshot = None;
    quote.buckets = [BucketQuote::MissingRate; 4];
    quote.known_subtotal = Decimal::default();
    quote.known_equivalent = Decimal::default();

    let pages = inspection_pages(Ok(InspectionDay::Ready(view)));
    let entry = pages[0].text.join("\n");

    assert!(
        entry.contains("openai/gpt-5.5"),
        "the entry page names the row: {entry}"
    );
    assert!(
        entry.contains("not a zero-cost claim"),
        "and says a blank is not a zero: {entry}"
    );
}

/// The same, for the range view's total page.
#[test]
fn accounting_inspect_range_entry_page_names_rows_whose_tokens_have_no_price() {
    let InspectionDay::Ready(mut view) = packet() else {
        panic!("packet is a ready day")
    };
    view.read_at_ms = view.coverage.completed_as_of_ms;
    let quote = view
        .requests
        .values_mut()
        .flatten()
        .next()
        .expect("the fixture has one attempt");
    quote.attempt.provider = "openai".into();
    quote.attempt.model = "gpt-6-astra".into();
    quote.snapshot = None;
    quote.buckets = [BucketQuote::MissingRate; 4];
    quote.known_subtotal = Decimal::default();
    quote.known_equivalent = Decimal::default();

    let pages = inspection_pages(Ok(range_packet(
        /*partial*/ false,
        InspectionDay::Ready(view),
    )));
    let entry = pages[0].text.join("\n");

    assert!(
        entry.contains("openai/gpt-6-astra"),
        "the range total page names the row: {entry}"
    );
}

#[test]
fn accounting_inspect_first_screen_names_provider_model_and_billing_type() {
    let InspectionDay::Ready(mut view) = breakdown_packet() else {
        unreachable!()
    };
    let mut plan = quote();
    plan.attempt.attempt_id = Uuid::from_u128(5);
    plan.attempt.request_id = Uuid::from_u128(105);
    plan.attempt.thread_id = view.owner;
    plan.attempt.provider = "claude-plan".into();
    plan.attempt.model = "opus".into();
    plan.known_subtotal = Decimal::default();
    plan.plan_burn_millis = Some(1000);
    plan.known_equivalent = decimal("0.5");
    plan.all_buckets_equivalent = Some(decimal("0.5"));
    view.requests.insert(plan.attempt.request_id, vec![plan]);
    view.totals = DayTotals::from_quotes(view.requests.values().flatten()).unwrap();
    let pages = inspection_pages(Ok(InspectionDay::Ready(view)));
    let root = &pages[0];
    let claude = provider_name("claude-plan");
    assert_eq!(claude, "Claude Plan");

    // The first thing on screen: each route with how it is paid for, then the
    // totals by billing type. Nothing technical above the divider.
    let details = root.text.iter().position(|s| s == "—— Details ——").unwrap();
    assert_eq!(
        root.text[..details].to_vec(),
        vec![
            "This conversation on 1970-01-01 (UTC):".to_string(),
            format!(
                "• Unknown provider · unknown model — Pay per use. 1 request, 140 tokens. Estimated cost: {}.",
                money(decimal("0.000004"))
            ),
            format!(
                "• alpha · one — Pay per use. 1 request, 140 tokens. Estimated cost: {}.",
                money(decimal("0.000001"))
            ),
            format!(
                "• alpha · two — Pay per use. 1 request, 140 tokens. Estimated cost: {}.",
                money(decimal("0.000002"))
            ),
            format!(
                "• {claude} · opus — Covered by your subscription (not billed per request). 1 request, 140 tokens. Same work at API prices: {}.",
                money(decimal("0.5"))
            ),
            format!(
                "Pay-per-use total — estimated cost: {}",
                money(decimal("0.000007"))
            ),
            format!(
                "Subscription work — same work at API prices: {}",
                money(decimal("0.5"))
            ),
            "No other conversation recorded requests on this day.".to_string(),
            super::OVERFLOW_NOTE.to_string(),
            "Costs are estimates from published prices; your provider's bill is the final amount."
                .to_string(),
            "Select a provider below to see its requests.".to_string(),
        ]
    );
    for technical in ["Collection coverage:", "Known subtotal exact USD:"] {
        let at = root
            .text
            .iter()
            .position(|s| s.starts_with(technical))
            .unwrap();
        assert!(at > details, "{technical} stays below the divider");
    }

    // Provider groups first, each naming its billing; requests last.
    let labels: Vec<_> = root.links.iter().map(|(label, _)| label.as_str()).collect();
    let covered = format!("{claude} · opus — covered by subscription (1 request)");
    let alpha = format!(
        "alpha · one — estimated {} (1 request)",
        money(decimal("0.000001"))
    );
    assert!(labels[..4].contains(&covered.as_str()), "{labels:?}");
    assert!(labels[..4].contains(&alpha.as_str()), "{labels:?}");
    // Then the requests, then the auditing groups.
    let first_request = labels
        .iter()
        .position(|l| l.starts_with("Request "))
        .unwrap();
    assert_eq!(first_request, 4, "{labels:?}");
    assert!(
        labels[4..8].iter().all(|l| l.starts_with("Request ")),
        "{labels:?}"
    );
    assert_eq!(
        labels.last(),
        Some(&"Unknown parent population (1 attempt)"),
        "{labels:?}"
    );
    let request_one = format!(
        "Request 1 · alpha · one · estimated {}",
        money(decimal("0.000001"))
    );
    assert!(labels.contains(&request_one.as_str()), "{labels:?}");

    // A request page, and the attempt page under it, open on the plain header.
    let (_, page) = root
        .links
        .iter()
        .find(|(label, _)| label.starts_with("Request ") && label.contains("opus"))
        .unwrap();
    let request = &pages[*page];
    assert_eq!(
        request.text[..5].to_vec(),
        vec![
            format!("Provider: {claude}"),
            "Model: opus".to_string(),
            "Billing: Covered by your subscription (not billed per request)".to_string(),
            format!("Same work at API prices: {}", money(decimal("0.5"))),
            "Tokens: 140 tokens".to_string(),
        ]
    );
    assert_eq!(request.links[0].0, "Technical details (attempt 1)");
    let attempt = &pages[request.links[0].1];
    assert_eq!(attempt.text[..3], request.text[..3]);
}

#[test]
fn accounting_operator_preserves_alias_wire_and_custom_route_identity() {
    let cases = [
        ("corbanu-plan", "pfterminal-plan"),
        ("corbanu-terminal-plan", "pfterminal-plan"),
        ("corbanu-plan-anthropic", "pfterminal-plan"),
        ("pfterminal-plan-anthropic", "pfterminal-plan"),
        ("zai-anthropic", "zai"),
        ("openrouter-anthropic", "openrouter"),
        ("baseten-anthropic", "baseten"),
        ("vercel-anthropic", "vercel"),
        ("vercel-anthropic-fast", "vercel"),
        ("pfterminal-plan", "pfterminal-plan"),
        ("openai", "openai"),
        ("anthropic", "anthropic"),
    ];
    let actual = cases.map(|(route, _)| (route, operator(route)));
    assert_eq!(actual, cases);

    // Custom IDs remain exact: sharing alias normalization must not start
    // treating different case, whitespace, or similar names as built-ins.
    let mut custom = vec![String::new(), "private/自分のモデル".to_string()];
    for (route, _) in cases {
        custom.extend([
            route.to_uppercase(),
            format!(" {route}"),
            format!("{route} "),
            format!("{route}-custom"),
        ]);
    }
    let unchanged = custom.iter().map(String::as_str).collect::<Vec<_>>();
    let actual = custom
        .iter()
        .map(|route| operator(route))
        .collect::<Vec<_>>();
    assert_eq!(actual, unchanged);
}

#[test]
fn accounting_model_names_resolve_corbanu_alias_routes() {
    let slug = "corbanu/deepseek-v4.1-flash";
    let routes = [
        "pfterminal-plan",
        "pfterminal-plan-anthropic",
        "corbanu-plan",
        "corbanu-terminal-plan",
        "corbanu-plan-anthropic",
    ];
    let actual = routes.map(|route| model_name(route, slug));
    assert_eq!(actual, ["DeepSeek V4.1 Flash"; 5]);
}

#[test]
fn accounting_inspect_plain_wording_counts_attempts_and_names_every_route() {
    let mut first = quote();
    first.all_buckets_priced = None;
    first.buckets[3] = BucketQuote::MissingRate;
    first.known_subtotal = decimal("0.00001");
    let mut retry = quote();
    retry.attempt.attempt_id = Uuid::from_u128(9);
    retry.attempt.model = "other-model".into();
    retry.all_buckets_priced = None;
    retry.buckets[3] = BucketQuote::MissingRate;
    retry.known_subtotal = decimal("0.00001");
    let quotes = vec![first, retry];
    let totals = DayTotals::from_quotes(quotes.iter()).unwrap();
    assert_eq!(
        plain_billing(&totals, EstimateGaps::of(quotes.iter())),
        (
            "Pay per use",
            format!(
                "Estimated cost: at least {} (2 attempts had no price)",
                money(decimal("0.00002"))
            )
        )
    );
    assert_eq!(
        request_route(&quotes, /*named*/ false).as_deref(),
        Some("several routes (synthetic · synthetic-model, synthetic · other-model)")
    );
    assert_eq!(
        request_route(&quotes[..1], /*named*/ false).as_deref(),
        Some("synthetic · synthetic-model")
    );
    // Real routes read by their display names.
    assert_eq!(model_name("openai", "gpt-6-sol"), "GPT-6 Sol");
    assert_eq!(provider_name("deepseek"), "DeepSeek");
    // #352: the two local providers are told apart.
    assert_eq!(provider_name("ollama"), "Ollama");
    assert_eq!(provider_name("lmstudio"), "LM Studio");
    // #352: another provider's catalogue row is not this route's name.
    assert_eq!(model_name("ambient", "z-ai/glm-5.2"), "Ambient GLM 5.2");
    assert_eq!(model_name("openrouter", "z-ai/glm-5.2"), "z-ai/glm-5.2");
    assert_eq!(
        model_name("openrouter", "z-ai/glm-5.3-flash"),
        "OpenRouter GLM 5.3 Flash"
    );
    // An Anthropic-wire route is its operator's.
    assert_eq!(model_name("zai-anthropic", "glm-5.2"), "Z.AI GLM 5.2");
    assert_eq!(
        model_name("vercel-anthropic-fast", "zai/glm-5.2"),
        "Vercel GLM 5.2"
    );
    // A provider you configured shows the slug: the name's brand is unknown.
    assert_eq!(model_name("my-zai", "glm-5.2"), "glm-5.2");
    // #1196: a name without a route brand names the model on any route.
    assert_eq!(
        model_name("my-claude-proxy", "claude-opus-5-5"),
        "Claude Opus 5.5"
    );
    assert_eq!(
        model_name("openrouter", "claude-opus-5-5"),
        "Claude Opus 5.5"
    );
    assert_eq!(model_name("my-openai", "gpt-6-sol"), "GPT-6 Sol");
    // A brand of several words counts only when every word is in the name.
    assert_eq!(
        model_name("anthropic", "claude-opus-5-5-plan"),
        "claude-opus-5-5-plan"
    );
    assert_eq!(model_name("kimi-code", "k3"), "Kimi Code K3");
    assert_eq!(model_name("openrouter", "k3"), "k3");
    assert_eq!(
        model_name("openrouter", "deepseek-v4-pro"),
        "deepseek-v4-pro"
    );
    // The brand is the operator's: Vercel owns its Anthropic-wire fast route.
    assert_eq!(
        model_name("openrouter", "zai/glm-5.2-fast"),
        "zai/glm-5.2-fast"
    );
    // No catalogue name read on another route says how a route sells the model.
    let catalogue = codex_models_manager::bundled_models_response().unwrap();
    for model in catalogue.models {
        let name = model_name("my-route", &model.slug);
        let lower = name.to_lowercase();
        assert!(
            name == model.slug
                || !lower
                    .split_whitespace()
                    .any(|word| ["plan", "(direct)", "free"].contains(&word)),
            "{} reads as {name:?} on another route",
            model.slug
        );
    }
}

#[test]
fn accounting_inspect_heading_names_the_inspected_day() {
    assert_eq!(
        day_heading(/*utc_day*/ 20_000, /*today*/ 20_000),
        "Today (UTC) in this conversation:"
    );
    assert_eq!(
        day_heading(/*utc_day*/ 19_999, /*today*/ 20_000),
        "This conversation on 2024-10-03 (UTC):"
    );
}

#[test]
fn accounting_inspect_multi_day_bucket_heading_names_the_whole_bucket() {
    let InspectionDay::Ready(view) = packet() else {
        unreachable!()
    };
    let pages = inspection_pages_for(
        Ok(InspectionDay::Ready(view)),
        Some(interval(/*start*/ 0, 2 * 86_400_000)),
    );
    assert_eq!(
        pages[0].text[0],
        "This conversation, [1970-01-01T00:00:00.000Z, 1970-01-03T00:00:00.000Z) (UTC):"
    );
}

/// #368: OpenRouter's stated charge prices its request, so it is the cost
/// rather than a second figure beside an estimate; a response that stated
/// none keeps its estimate.
#[test]
fn accounting_inspect_states_the_providers_billed_charge() {
    let InspectionDay::Ready(mut view) = breakdown_packet() else {
        unreachable!()
    };
    // OpenRouter states its charges: here on model one's response, not on
    // model two's.
    for quote in view.requests.values_mut().flatten() {
        if quote.attempt.provider == "alpha" {
            quote.attempt.provider = "openrouter".into();
        }
        if quote.attempt.model == "one" {
            quote.usage.billed_usd = Some(decimal("0.0123312"));
        }
    }
    view.totals = DayTotals::from_quotes(view.requests.values().flatten()).unwrap();
    let pages = inspection_pages(Ok(InspectionDay::Ready(view)));
    let root = &pages[0];
    let billed = money(decimal("0.0123312"));
    let line = root
        .text
        .iter()
        .find(|s| s.starts_with("• OpenRouter · one"))
        .unwrap();
    assert!(
        line.ends_with(&format!(" Cost: {billed}, charged by OpenRouter.")),
        "{line}"
    );
    assert!(!line.contains("Billed"), "{line}");
    let other = root
        .text
        .iter()
        .find(|s| s.starts_with("• OpenRouter · two"))
        .unwrap();
    assert!(!other.contains("Billed"), "{other}");
    // Only one of the day's three pay-per-use attempts stated a charge.
    let detail = format!(
        "Billed cost: at least {billed} (1 of 3 attempts stated a charge) — as stated by the provider with each response"
    );
    assert!(root.text.contains(&detail), "{:?}", root.text);
    assert!(!root.text.iter().any(|s| s.contains("report its charges")));
    // The request page states it plainly, and no longer claims the
    // difference is unknowable next to a stated charge.
    let (_, page) = root
        .links
        .iter()
        .find(|(label, _)| label.starts_with("Request ") && label.contains("OpenRouter · one"))
        .unwrap();
    let request = &pages[*page];
    assert!(
        request
            .text
            .contains(&format!("Cost: {billed}, charged by OpenRouter")),
        "{:#?}",
        request.text
    );
    assert!(request.text.contains(&billed_detail(&billed)));
    assert!(
        !request
            .text
            .iter()
            .any(|s| s.starts_with("Estimate versus billed difference"))
    );
    let attempt = &pages[request.links[0].1];
    assert_eq!(
        attempt
            .text
            .iter()
            .filter(|s| s.starts_with("Billed cost:"))
            .count(),
        1
    );
    // A request whose response stated nothing says so, without claiming
    // OpenRouter never reports its charges.
    let (_, page) = root
        .links
        .iter()
        .find(|(label, _)| label.starts_with("Request ") && label.contains("OpenRouter · two"))
        .unwrap();
    assert!(
        pages[*page]
            .text
            .contains(&"Billed cost: OpenRouter stated no charge for this work, so any cost here is an estimate — check OpenRouter's bill.".to_string())
    );
}

/// Every page with work that can be billed has exactly one billing line; the
/// first screen's is its overview estimate line, so its details add none.
#[test]
fn accounting_inspect_one_billing_line_per_page() {
    let pages = inspection_pages(Ok(breakdown_packet()));
    let overview_lines = pages[0]
        .text
        .iter()
        .filter(|s| s.starts_with("Costs are estimates") || s.starts_with("Billed cost:"))
        .count();
    assert_eq!(overview_lines, 1, "{:#?}", pages[0].text);
    for page in &pages[1..] {
        let lines = page
            .text
            .iter()
            .filter(|s| s.starts_with("Billed cost:"))
            .count();
        // Every page past the first covers at least one pay-per-use attempt.
        assert_eq!(lines, 1, "{}: {:#?}", page.title, page.text);
    }
}

fn billed_quote(provider: &str) -> ObservationQuote {
    let mut quote = quote();
    quote.attempt.provider = provider.into();
    quote
}

#[test]
fn accounting_billed_line_names_whose_bill_to_check() {
    let line = |quotes: &[ObservationQuote]| billed_line(&quotes.iter().collect::<Vec<_>>());
    // A provider that never states its charge.
    assert_eq!(
        line(&[billed_quote("zai")]).unwrap(),
        "Billed cost: Z.AI doesn't report its charges, so any cost here is an estimate — check Z.AI's bill."
    );
    // One that does, but stated none on these responses.
    assert_eq!(
        line(&[billed_quote("openrouter")]).unwrap(),
        "Billed cost: OpenRouter stated no charge for this work, so any cost here is an estimate — check OpenRouter's bill."
    );
    // Both kinds, and several names.
    assert_eq!(
        line(&[billed_quote("zai"), billed_quote("openrouter")]).unwrap(),
        "Billed cost: Z.AI doesn't report its charges and OpenRouter stated no charge for this work, so any cost here is an estimate — check their bills."
    );
    assert_eq!(
        line(&[billed_quote("c"), billed_quote("a"), billed_quote("b")]).unwrap(),
        "Billed cost: a, b and c don't report their charges, so any cost here is an estimate — check their bills."
    );
    // The Corbanu API's two routes are one provider.
    assert_eq!(
        line(&[
            billed_quote("pfterminal-plan"),
            billed_quote("pfterminal-plan-anthropic")
        ])
        .unwrap(),
        "Billed cost: Corbanu API stated no charge for this work, so any cost here is an estimate — check Corbanu API's bill."
    );
    // A refused attempt that reported nothing was normally not charged.
    let mut refused = billed_quote("openrouter");
    refused.usage = Usage::default();
    assert_eq!(
        line(&[refused]).unwrap(),
        "Billed cost: none stated — OpenRouter reported no usage here; a refused request is normally not charged, but if it stopped mid-response, check OpenRouter's bill."
    );
    // No provider id: nothing to name.
    assert_eq!(
        line(&[billed_quote("")]).unwrap(),
        "Billed cost: not reported, so any cost here is an estimate — check your provider's bill."
    );
    // Subscription work has no bill to check, and an empty page no line.
    let mut plan = billed_quote("claude-plan");
    plan.plan_burn_millis = Some(1000);
    assert_eq!(line(&[plan]), None);
    assert_eq!(line(&[]), None);
}

/// A complete range states the billing basis once on its overview; a partial
/// one shows no total, so no billing line either.
#[test]
fn accounting_inspect_range_overview_billing_line_only_with_a_total() {
    let current = || {
        let InspectionDay::Ready(mut view) = packet() else {
            unreachable!()
        };
        view.read_at_ms = view.coverage.completed_as_of_ms;
        InspectionDay::Ready(view)
    };
    let complete = inspection_pages(Ok(range_packet(/*partial*/ false, current())));
    assert!(
        complete[0].text.contains(
            &"Billed cost: synthetic doesn't report its charges, so any cost here is an estimate — check synthetic's bill."
                .to_string()
        ),
        "{:#?}",
        complete[0].text
    );
    let partial = inspection_pages(Ok(range_packet(/*partial*/ true, current())));
    assert!(
        !partial[0]
            .text
            .iter()
            .any(|s| s.starts_with("Billed cost:"))
    );
}

/// A provider that states every charge but omits a counter the estimate needs
/// (the Vercel gateway reports no cache writes): the overview gives the charge
/// alone, and names a refused attempt that reported nothing rather than
/// calling the charge partial.
#[test]
fn accounting_inspect_leads_with_a_complete_stated_charge() {
    let InspectionDay::Ready(mut view) = breakdown_packet() else {
        unreachable!()
    };
    let mut refused = None;
    for quote in view.requests.values_mut().flatten() {
        if quote.attempt.model == "one" {
            quote.usage.billed_usd = Some(decimal("0.00116615"));
            quote.all_buckets_priced = None;
            let mut zero = quote.clone();
            zero.attempt.attempt_id = Uuid::from_u128(9);
            zero.attempt.request_id = Uuid::from_u128(109);
            zero.observations.clear();
            zero.usage = codex_state::accounting::Usage::default();
            zero.known_subtotal = decimal("0");
            refused = Some(zero);
        }
    }
    let refused = refused.unwrap();
    view.requests
        .insert(refused.attempt.request_id, vec![refused]);
    view.totals = DayTotals::from_quotes(view.requests.values().flatten()).unwrap();
    let pages = inspection_pages(Ok(InspectionDay::Ready(view)));
    let line = pages[0]
        .text
        .iter()
        .find(|s| s.starts_with("• alpha · one"))
        .unwrap();
    assert!(
        line.ends_with(&format!(
            " Billed by alpha: at least {} (1 failed attempt reported no usage; a refused request is normally not charged).",
            money(decimal("0.00116615"))
        )),
        "{line}"
    );
    assert!(!line.contains("Estimated cost"), "{line}");
}

#[test]
fn accounting_names_the_corbanu_api_as_the_product_does() {
    assert_eq!(provider_name("pfterminal-plan"), "Corbanu API");
    assert_eq!(provider_name("pfterminal-plan-anthropic"), "Corbanu API");
}

/// The cost screens read the same for every provider: a pay-per-use route
/// with a stated charge and no published price (the Corbanu API case), one
/// with neither, a priced one, and subscription work. No page leaks Rust
/// debug output or raw epoch counts, and no page states `$0` beside an
/// unknown estimate.
#[test]
fn accounting_inspect_reads_plainly_for_every_provider() {
    let InspectionDay::Ready(mut view) = packet() else {
        unreachable!()
    };
    view.requests.clear();
    view.read_at_ms = 20_724 * 86_400_000 + 28_000_000;
    view.coverage.completed_as_of_ms = view.read_at_ms - 29_669;
    view.coverage.aggregate_day_floor = 20_360;
    view.coverage.oldest_recorded_day = Some(20_724);
    view.utc_day = 20_724;
    let unpriced = |q: &mut ObservationQuote| {
        q.buckets = [BucketQuote::MissingRate; 4];
        q.known_subtotal = Decimal::default();
        q.all_buckets_priced = None;
    };
    for (id, provider, model) in [
        (1, "pfterminal-plan", "corbanu/glm-5.3-flash"),
        (2, "deepseek", "deepseek-chat"),
        (3, "openai", "gpt-6-sol"),
        (4, "openrouter", "some/unlisted-model"),
        (5, "claude-plan", "opus"),
    ] {
        let mut q = quote();
        q.attempt.attempt_id = Uuid::from_u128(id);
        q.attempt.request_id = Uuid::from_u128(100 + id);
        q.attempt.thread_id = view.owner;
        q.attempt.provider = provider.into();
        q.attempt.model = model.into();
        q.attempt.dispatched_at_ms = (view.read_at_ms - 60_000).try_into().unwrap();
        match id {
            1 | 2 => {
                unpriced(&mut q);
                q.usage.billed_usd = Some(decimal("0.002246"));
            }
            3 => q.all_buckets_priced = Some(q.known_subtotal),
            4 => unpriced(&mut q),
            _ => {
                unpriced(&mut q);
                q.plan_burn_millis = Some(1000);
            }
        }
        view.requests.insert(q.attempt.request_id, vec![q]);
    }
    view.totals = DayTotals::from_quotes(view.requests.values().flatten()).unwrap();
    view.own_totals = view.totals.clone();
    let pages = inspection_pages(Ok(InspectionDay::Ready(view)));
    for page in &pages {
        for line in page.text.iter().chain(page.links.iter().map(|(l, _)| l)) {
            for leak in [
                "Some(",
                "None",
                "ms since Unix epoch",
                "ms UTC",
                "$0.000000 + unknown",
                "known + unknown in",
                "estimated no price",
            ] {
                assert!(!line.contains(leak), "`{leak}` on {}: {line}", page.title);
            }
            assert_ne!(line, "Known subtotal exact USD: 0", "{}", page.title);
            assert_ne!(line, "Known estimate exact USD: 0", "{}", page.title);
        }
    }
    let labels: Vec<_> = pages[0].links.iter().map(|(l, _)| l.as_str()).collect();
    let billed = money(decimal("0.002246"));
    assert!(
        labels
            .contains(&format!("DeepSeek · deepseek-chat — billed {billed} (1 request)").as_str()),
        "{labels:?}"
    );
    // The Corbanu API's reported charge is the cost (Travis, 2026-10-14).
    assert!(
        labels.contains(
            &format!(
                "Corbanu API · corbanu/glm-5.3-flash — {billed}, charged by Corbanu API (1 request)"
            )
            .as_str()
        ),
        "{labels:?}"
    );
    assert!(
        labels.contains(&"OpenRouter · some/unlisted-model — no price available (1 request)")
            || labels
                .iter()
                .any(|l| l.ends_with("some/unlisted-model — no price available (1 request)")),
        "{labels:?}"
    );
    assert!(labels.contains(&"Descendant attempts (none)"), "{labels:?}");
    assert!(
        labels.contains(&"Unknown parent population (none)"),
        "{labels:?}"
    );
    let root = pages[0].text.join("\n");
    assert!(
        root.contains("Retention: request detail kept since 2026-06-30T"),
        "{root}"
    );
    assert!(
        root.contains(
            "daily totals kept since 2025-09-29; oldest recorded day in this conversation 2026-09-28"
        ),
        "{root}"
    );
}

/// Back and Esc return the way the user came: a request opened from a
/// provider's page goes back to that provider, not to the request's fixed
/// parent (the overview).
#[tokio::test]
async fn accounting_inspect_back_returns_the_way_the_user_came() {
    let (mut chat, mut rx, _ops) = make_chatwidget_manual(/*model_override*/ None).await;
    chat.open_accounting_inspector(/*day*/ 0);
    let AppEvent::LoadAccountingInspector {
        generation,
        thread,
        day,
        ..
    } = rx.try_recv().unwrap()
    else {
        panic!()
    };
    chat.finish_accounting_inspector(generation, thread, day, Ok(breakdown_packet()));
    let pages = &chat.accounting_inspector.as_ref().unwrap().pages;
    let (_, provider) = pages[0]
        .links
        .iter()
        .find(|(label, _)| label.starts_with("alpha · one"))
        .cloned()
        .unwrap();
    let request = pages[provider].links[0].1;
    let attempt = pages[request].links[0].1;
    assert_eq!(pages[request].parent, Some(0));

    for target in [provider, request, attempt] {
        chat.navigate_accounting_inspector(generation, target);
    }
    let back = |chat: &ChatWidget| {
        let view = chat.accounting_inspector.as_ref().unwrap();
        (
            view.page,
            view.params(/*width*/ 80)
                .items
                .iter()
                .any(|i| i.name == "Back"),
        )
    };
    assert_eq!(back(&chat), (attempt, true));
    // Each Back (or Esc) sends the page it came from; follow them.
    for expected in [request, provider, 0] {
        let view = chat.accounting_inspector.as_ref().unwrap();
        let target = view
            .history
            .last()
            .copied()
            .or(view.pages[view.page].parent);
        chat.navigate_accounting_inspector(generation, target.unwrap());
        assert_eq!(chat.accounting_inspector.as_ref().unwrap().page, expected);
    }
    assert_eq!(
        back(&chat),
        (0, false),
        "the overview has nowhere to go back to"
    );
    assert!(
        chat.accounting_inspector
            .as_ref()
            .unwrap()
            .history
            .is_empty()
    );
}

#[test]
fn accounting_inspect_plan_consumption_groups_thousands() {
    assert_eq!(rate_scaled(/*milli_tokens*/ 24_337_000), "24,337");
    assert_eq!(rate_scaled(/*milli_tokens*/ 1_234_567_500), "1,234,567.500");
    assert_eq!(rate_scaled(/*milli_tokens*/ 500), "0.500");
}

/// #289: an inclusive attempt that did not report cache writes, bound to a
/// price that charges nothing for them, is costed from input − cache read.
/// Its lines say so instead of calling the count unknown beside its cost.
fn free_write_quote() -> ObservationQuote {
    let mut q = quote();
    q.snapshot = Some(
        serde_json::from_value(serde_json::json!({
            "id": Uuid::from_u128(1000), "provider": "synthetic", "model": "synthetic-model",
            "scope": Uuid::from_u128(5), "currency": "USD", "unit": "PerMillionTokens",
            "rates": {"noncached": "1.4", "read": "0.26", "write": "0", "output": "4.4"},
            "source_reference": Uuid::from_u128(1001), "source_kind": "ProviderPublished",
            "observed_at_ms": 0, "approved_at_ms": 0, "effective_from_ms": 0,
            "effective_end_ms": null
        }))
        .unwrap(),
    );
    q
}

#[test]
fn accounting_inspect_free_cache_writes_show_the_counts_their_costs_used() {
    let q = free_write_quote();
    assert_eq!(q.priced_counts(), [Some(80), Some(20), Some(0), Some(40)]);
    let lines = super::attempt_text(&q);
    for expected in [
        "Noncached input (derived for inclusive input): 80 (derived as input − cache read: cache writes were not reported, and this price charges nothing for cache writes)",
        "Cache write: not reported — this price charges nothing for cache writes",
    ] {
        assert!(
            lines.iter().any(|line| line == expected),
            "{expected}\n{lines:#?}"
        );
    }
    assert!(
        !lines.iter().any(
            |line| line.contains("unknown — no retained numeric evidence")
                && (line.starts_with("Noncached input (") || line.starts_with("Cache write:"))
        ),
        "{lines:#?}"
    );

    let InspectionDay::Ready(mut view) = packet() else {
        unreachable!()
    };
    view.requests = std::collections::BTreeMap::from([(q.attempt.request_id, vec![q])]);
    let text = inspection_pages(Ok(InspectionDay::Ready(view)))[0]
        .text
        .join("\n");
    assert!(
        text.contains("Noncached input (derived for inclusive input): not reported (1 attempt) — costed as input − cache read for 1 attempt, whose price charges nothing for cache writes"),
        "{text}"
    );
    assert!(
        text.contains("Cache write: not reported (1 attempt) — free at the price of 1 attempt, so nothing was charged"),
        "{text}"
    );
}

/// #289: requests are numbered in the order they were sent.
#[test]
fn accounting_inspect_requests_are_numbered_in_dispatch_order() {
    let InspectionDay::Ready(mut view) = packet() else {
        unreachable!()
    };
    view.requests.clear();
    // Ids sort the other way round from dispatch time.
    for (id, sent) in [(1u128, 30_000i64), (2, 10_000), (3, 20_000)] {
        let mut q = quote();
        q.attempt.attempt_id = Uuid::from_u128(id);
        q.attempt.request_id = Uuid::from_u128(100 + id);
        q.attempt.thread_id = view.owner;
        q.attempt.dispatched_at_ms = sent.try_into().unwrap();
        view.requests.insert(q.attempt.request_id, vec![q]);
    }
    let pages = inspection_pages(Ok(InspectionDay::Ready(view)));
    let requests: Vec<_> = pages[0]
        .links
        .iter()
        .filter(|(label, _)| label.starts_with("Request "))
        .map(|(label, page)| {
            let id = pages[*page]
                .text
                .iter()
                .find_map(|line| line.strip_prefix("Request: "))
                .unwrap()
                .to_string();
            (label.split(' ').nth(1).unwrap().to_string(), id)
        })
        .collect();
    assert_eq!(
        requests,
        [(2, 102), (3, 103), (1, 101)]
            .into_iter()
            .enumerate()
            .map(|(n, (_, id))| ((n + 1).to_string(), Uuid::from_u128(id).to_string()))
            .collect::<Vec<_>>()
    );
    // Group pages list the same requests in the same order and numbering.
    let root = pages
        .iter()
        .find(|page| page.title == "Root's own attempts")
        .expect("root group page");
    assert_eq!(
        root.links
            .iter()
            .map(|(label, _)| label.as_str())
            .collect::<Vec<_>>(),
        ["Request 1", "Request 2", "Request 3"]
    );
}

/// #289: a valid date after today is refused with the reason, not the syntax.
#[tokio::test]
async fn accounting_inspect_future_day_says_why() {
    let (mut chat, mut rx, _ops) = make_chatwidget_manual(/*model_override*/ None).await;
    chat.open_accounting_command(
        "requests 2026-09-17",
        NaiveDate::from_ymd_opt(2026, 9, 16).unwrap(),
    );
    let mut text = String::new();
    while let Ok(event) = rx.try_recv() {
        if let AppEvent::InsertHistoryCell(cell) = event {
            for line in cell.display_lines(/*width*/ 200) {
                text.push_str(&line.to_string());
                text.push('\n');
            }
        }
    }
    assert!(
        text.contains("2026-09-17 is after today (2026-09-16, UTC)"),
        "{text}"
    );
    assert!(!text.contains("Usage:"), "{text}");
}

/// #475: a request the provider refused with 401 before any response reads
/// as a failure with no charge: never as incomplete usage, a lower bound or a
/// reason to check the provider's bill.
#[test]
fn accounting_inspect_refused_request_reads_as_failed_with_no_charge() {
    // With a price bound at dispatch, and with none (review S6).
    let priced = native_snapshot(
        serde_json::json!({"noncached":"1","read":"1","write":null,"output":"4"}),
        "Billed",
        /*basis_source*/ None,
        /*plan_burn_millis*/ None,
    );
    for snapshot in [Some(priced), None] {
        let mut q = quote();
        q.attempt.provider = "zai".into();
        q.attempt.account = Some(codex_state::accounting::AttemptAccount {
            name: "fake".into(),
            fingerprint: Some("0123456789ab".into()),
        });
        q.snapshot = snapshot;
        q.usage = Usage {
            input: Some(0),
            noncached: Some(0),
            read: Some(0),
            write: Some(0),
            output: Some(0),
            reasoning: Some(0),
            total: Some(0),
            billed_usd: None,
            refused_status: Some(401),
            session_ended_at_ms: None,
        };
        q.buckets = [BucketQuote::Priced(Decimal::default()); 4];
        q.known_subtotal = Decimal::default();
        q.all_buckets_priced = Some(Decimal::default());
        let quotes = [&q];

        let route = route_line(&quotes, /*named*/ true);
        assert!(
            route.contains("1 attempt refused by the provider (HTTP 401) — no charge"),
            "{route}"
        );
        let header = plain_header(&quotes, /*named*/ true).join("\n");
        assert!(
            header.contains("Refused: 1 attempt refused by the provider (HTTP 401) — no charge")
        );
        let attempt = attempt_text(&q).join("\n");
        assert!(
            attempt.contains(
                "Completion/billing status: refused by the provider (HTTP 401) before any response; no tokens, no charge"
            ),
            "{attempt}"
        );
        for text in [&route, &header, &attempt] {
            for misleading in [
                "incomplete",
                "at least",
                "check",
                "not reported",
                "no price",
                "no applicable price",
            ] {
                assert!(!text.contains(misleading), "{misleading}: {text}");
            }
        }
    }
}

/// #396 item 1: a component with no rate states no money even when it had no
/// tokens ("none — 0 tokens", never "$0.000000"); a component with a rate
/// keeps its zero figure.
#[test]
fn accounting_unpriced_zero_token_component_states_no_money() {
    let mut q = quote();
    q.usage.write = Some(0);
    q.buckets = [
        BucketQuote::MissingRate,
        BucketQuote::MissingRate,
        BucketQuote::Priced(Decimal::default()),
        BucketQuote::MissingRate,
    ];
    q.known_subtotal = Decimal::default();
    q.all_buckets_priced = None;
    q.subtotal_display = Decimal::default().display();
    q.snapshot = Some(native_snapshot(
        serde_json::json!({"noncached":null,"read":null,"write":null,"output":null}),
        "Billed",
        /*basis_source*/ None,
        /*plan_burn_millis*/ None,
    ));
    let unpriced = attempt_text(&q);
    assert!(
        unpriced
            .iter()
            .any(|line| line == "Cache write cost: none — 0 tokens"),
        "{unpriced:#?}"
    );
    assert!(
        !unpriced.iter().any(|line| line.contains("$0.000000")),
        "{unpriced:#?}"
    );
    q.snapshot = Some(native_snapshot(
        serde_json::json!({"noncached":"1","read":"0.1","write":"1.25","output":"8"}),
        "Billed",
        /*basis_source*/ None,
        /*plan_burn_millis*/ None,
    ));
    let priced = attempt_text(&q);
    assert!(
        priced
            .iter()
            .any(|line| line == "Cache write cost: $0.000000; exact USD 0"),
        "{priced:#?}"
    );
}

/// #403 (Travis, 2026-10-14, option C): `/cost` names the warm-up a reopened
/// conversation sent, on the first screen, in the request's link and on its
/// page, with its cost; an ordinary request is not named so.
#[test]
fn accounting_reopen_warmup_is_named_with_its_cost() {
    let InspectionDay::Ready(mut view) = packet() else {
        unreachable!()
    };
    let mut warmup = quote();
    warmup.attempt.attempt_id = Uuid::from_u128(11);
    warmup.attempt.request_id = Uuid::from_u128(12);
    warmup.attempt.retry_of = None;
    warmup.attempt.turn = format!("side:prewarm:reopen:{}", Uuid::from_u128(13));
    warmup.attempt.dispatched_at_ms = 1.try_into().unwrap();
    view.requests
        .insert(warmup.attempt.request_id, vec![warmup]);
    view.totals = DayTotals::from_quotes(view.requests.values().flatten()).unwrap();
    view.own_totals = view.totals.clone();
    let pages = inspection_pages(Ok(InspectionDay::Ready(view)));
    let screen: Vec<&String> = pages[0]
        .text
        .iter()
        .take_while(|line| line.as_str() != "—— Details ——")
        .collect();
    assert!(
        screen.iter().any(|line| line.as_str()
            == "Reopen warm-up: 1 request, estimated at least $0.000180 (1 attempt had no price). Sent when this conversation was reopened or forked, to prime the model's cache."),
        "{screen:#?}"
    );
    let labels: Vec<&str> = pages[0]
        .links
        .iter()
        .map(|(label, _)| label.as_str())
        .collect();
    let warmup_links: Vec<&&str> = labels
        .iter()
        .filter(|label| label.contains("Reopen warm-up"))
        .collect();
    assert_eq!(warmup_links.len(), 1, "{labels:?}");
    assert!(
        warmup_links[0].starts_with("Request 2 · Reopen warm-up · "),
        "{labels:?}"
    );
    assert!(
        labels
            .iter()
            .any(|label| label.starts_with("Request 1 · ") && !label.contains("warm-up")),
        "{labels:?}"
    );
    let (_, index) = pages[0]
        .links
        .iter()
        .find(|(label, _)| label.contains("Reopen warm-up"))
        .unwrap();
    assert!(
        pages[*index].text.iter().any(|line| line.starts_with(
            "Kind: Reopen warm-up — sent when this conversation was reopened or forked"
        )),
        "{:#?}",
        pages[*index].text
    );
}

/// A recorded realtime WebSocket session (#824): one request, admitted at
/// `started` and ended at `ended`, with no token count and only a billing
/// basis (no catalogue price for realtime models).
fn realtime_quote(started: i64, ended: Option<i64>) -> ObservationQuote {
    let mut q = quote();
    q.attempt.attempt_id = Uuid::from_u128(21);
    q.attempt.request_id = Uuid::from_u128(22);
    q.attempt.retry_of = None;
    q.attempt.turn = format!("realtime:ws:{}", Uuid::from_u128(23));
    q.attempt.provider = "openai".into();
    q.attempt.model = "gpt-realtime-1.5".into();
    q.attempt.dispatched_at_ms = started.try_into().unwrap();
    q.usage = Usage {
        session_ended_at_ms: ended,
        ..Usage::default()
    };
    q.buckets = [BucketQuote::MissingUsage; 4];
    q.known_subtotal = Decimal::default();
    q.all_buckets_priced = None;
    q.subtotal_display = Decimal::default().display();
    q.snapshot = Some(native_snapshot(
        serde_json::json!({"noncached":null,"read":null,"write":null,"output":null}),
        "Billed",
        /*basis_source*/ None,
        /*plan_burn_millis*/ None,
    ));
    q
}

/// #824 (Travis, 2026-10-15, option a): `/cost` names a realtime session on
/// the first screen, in its request's link and on its pages, with its start
/// and end times and its tokens and cost unknown - never a $0 figure.
#[test]
fn accounting_realtime_session_reads_unknown_not_zero() {
    let InspectionDay::Ready(mut view) = packet() else {
        unreachable!()
    };
    let session = realtime_quote(/*started*/ 1_000, Some(61_000));
    view.requests =
        std::collections::BTreeMap::from([(session.attempt.request_id, vec![session.clone()])]);
    view.totals = DayTotals::from_quotes(view.requests.values().flatten()).unwrap();
    view.own_totals = view.totals.clone();
    let pages = inspection_pages(Ok(InspectionDay::Ready(view)));
    let screen: Vec<&String> = pages[0]
        .text
        .iter()
        .take_while(|line| line.as_str() != "—— Details ——")
        .collect();
    assert!(
        screen.iter().any(|line| line.as_str()
            == "Realtime: 1 session; token counts and cost unknown, not zero — no token usage was recorded for it."),
        "{screen:#?}"
    );
    let (label, index) = pages[0]
        .links
        .iter()
        .find(|(label, _)| label.contains("Realtime session"))
        .expect("a realtime request link");
    assert!(
        label.starts_with("Request 1 · Realtime session · "),
        "{label}"
    );
    let request = &pages[*index].text;
    assert!(
        request.iter().any(|line| line
            == "Kind: Realtime session — started 1970-01-01T00:00:01.000Z, ended 1970-01-01T00:01:01.000Z; its token counts and cost are unknown, not zero: no token usage was recorded for it."),
        "{request:#?}"
    );
    let attempt = attempt_text(&session);
    assert!(
        attempt
            .iter()
            .any(|line| line == "Admission time: 1970-01-01T00:00:01.000Z"),
        "{attempt:#?}"
    );
    assert!(
        attempt
            .iter()
            .any(|line| line == "Realtime session ended: 1970-01-01T00:01:01.000Z"),
        "{attempt:#?}"
    );
    // Nothing anywhere states a money figure for it.
    for text in pages
        .iter()
        .flat_map(|page| {
            page.text
                .iter()
                .chain(page.links.iter().map(|(label, _)| label))
        })
        .chain(attempt.iter())
    {
        assert!(!text.contains('$'), "{text}");
    }
}

/// #889 D223: a recorded realtime session with the tokens its responses
/// reported (332 input, 214 of them cached, 151 output) and no rate, as the
/// ledger quotes it: the reported parts priced by nothing.
fn realtime_reported_quote(started: i64, ended: Option<i64>) -> ObservationQuote {
    let mut q = realtime_quote(started, ended);
    q.usage = Usage {
        input: Some(332),
        read: Some(214),
        output: Some(151),
        total: Some(483),
        session_ended_at_ms: ended,
        ..Usage::default()
    };
    // Noncached input needs the unreported cache-write count.
    q.buckets = [
        BucketQuote::MissingUsage,
        BucketQuote::MissingRate,
        BucketQuote::MissingUsage,
        BucketQuote::MissingRate,
    ];
    q
}

/// #889 D223 (Travis, 2026-10-28): `/cost` shows a realtime session's
/// reported tokens and says its cost is unknown, not zero - never a dollar
/// figure, on any page.
#[test]
fn accounting_realtime_session_shows_reported_tokens_and_no_cost() {
    let InspectionDay::Ready(mut view) = packet() else {
        unreachable!()
    };
    let session = realtime_reported_quote(/*started*/ 1_000, Some(61_000));
    view.requests =
        std::collections::BTreeMap::from([(session.attempt.request_id, vec![session.clone()])]);
    view.totals = DayTotals::from_quotes(view.requests.values().flatten()).unwrap();
    view.own_totals = view.totals.clone();
    let pages = inspection_pages(Ok(InspectionDay::Ready(view)));
    let screen: Vec<&str> = pages[0]
        .text
        .iter()
        .map(String::as_str)
        .take_while(|line| *line != "—— Details ——")
        .collect();
    insta::assert_snapshot!(screen.join("\n"), @"
    This conversation on 1970-01-01 (UTC):
    • OpenAI · gpt-realtime-1.5 — Pay per use. 1 request, 483 tokens. Estimated cost: no price available.
    Realtime: 1 session; tokens as the provider reported them at the end of each response; cost unknown, not zero — Corbanu has no realtime prices.
    No other conversation recorded requests on this day.
    Next step for requests with no price: check the bill from OpenAI. No published price covers them, so no cost is shown for them here.
    Costs are estimates from published prices; OpenAI doesn't report its charges — check OpenAI's bill.
    Select a provider below to see its requests.
    ");
    let (_, index) = pages[0]
        .links
        .iter()
        .find(|(label, _)| label.contains("Realtime session"))
        .expect("a realtime request link");
    assert!(
        pages[*index].text.iter().any(|line| line
            == "Kind: Realtime session — started 1970-01-01T00:00:01.000Z, ended 1970-01-01T00:01:01.000Z; its tokens are those the provider reported at the end of each response, so they may be low: a response cut short by the session's end reports none, and one whose report could not be read is not counted; its cost is unknown, not zero: Corbanu has no realtime prices."),
        "{:#?}",
        pages[*index].text
    );
    let attempt = attempt_text(&session);
    assert!(
        attempt.iter().any(|line| line
            == "Token cost: unavailable — no applicable price; recorded usage is not a zero-cost claim."),
        "{attempt:#?}"
    );
    for text in pages
        .iter()
        .flat_map(|page| {
            page.text
                .iter()
                .chain(page.links.iter().map(|(label, _)| label))
        })
        .chain(attempt.iter())
    {
        assert!(!text.contains('$'), "{text}");
    }
}

/// #889 D223: with sessions both with and without recorded tokens, the first
/// screen says how many of each.
#[test]
fn accounting_realtime_sessions_with_and_without_tokens_say_which() {
    let old = realtime_quote(/*started*/ 1_000, Some(2_000));
    let mut reported = realtime_reported_quote(/*started*/ 3_000, Some(4_000));
    reported.attempt.attempt_id = Uuid::from_u128(31);
    reported.attempt.request_id = Uuid::from_u128(32);
    assert_eq!(
        realtime_session_line([&old, &reported]).as_deref(),
        Some(
            "Realtime: 2 sessions; tokens as the provider reported them at the end of each response for 1, none recorded for 1; cost unknown, not zero — Corbanu has no realtime prices."
        )
    );
    assert_eq!(
        realtime_session_line([&reported]).as_deref(),
        Some(
            "Realtime: 1 session; tokens as the provider reported them at the end of each response; cost unknown, not zero — Corbanu has no realtime prices."
        )
    );
}

/// #824: a session whose end was never recorded (the process stopped
/// mid-session) says so rather than inventing a time.
#[test]
fn accounting_realtime_session_without_an_end_says_so() {
    let session = realtime_quote(/*started*/ 1_000, /*ended*/ None);
    let attempt = attempt_text(&session);
    assert!(
        attempt
            .iter()
            .any(|line| line == "Realtime session ended: not recorded"),
        "{attempt:#?}"
    );
    let kind = realtime_session_kind(&[&session]);
    assert!(
        kind.starts_with(
            "Kind: Realtime session — started 1970-01-01T00:00:01.000Z, ended not recorded;"
        ),
        "{kind}"
    );
    // An owner prefix keeps it a realtime session; an ordinary turn and a
    // WebRTC call are not. A WebRTC call is recorded at its creation; the end
    // it may carry (#955) is its client control channel's, not the media
    // call's, so it is not shown as a session end (#1000).
    let mut side = session.clone();
    side.attempt.turn = format!("side:realtime:ws:{}", Uuid::from_u128(24));
    assert!(realtime_session(&[&side]));
    assert!(!realtime_session(&[&quote()]));
    let mut call = session;
    call.attempt.turn = format!("realtime:{}", Uuid::from_u128(25));
    call.usage.session_ended_at_ms = Some(61_000);
    assert!(!realtime_session(&[&call]));
    assert!(
        !attempt_text(&call)
            .iter()
            .any(|line| line.starts_with("Realtime session")),
    );
}

/// #352 item 3: a request with 9,908 noncached input tokens, no cache reads, a
/// missing cache-write count and 1 output token showed `1+ tokens`. The input
/// total is unknown, but the parts the provider reported still count.
#[test]
fn plain_tokens_lower_bound_counts_reported_input_parts() {
    let day = DayTotals {
        // Input, noncached, read, write, output, reasoning, total.
        measured: [
            Metric {
                known: 0,
                unknown: 1,
            },
            Metric {
                known: 9_908,
                unknown: 0,
            },
            Metric {
                known: 0,
                unknown: 0,
            },
            Metric {
                known: 0,
                unknown: 1,
            },
            Metric {
                known: 1,
                unknown: 0,
            },
            Metric {
                known: 0,
                unknown: 0,
            },
            Metric {
                known: 0,
                unknown: 1,
            },
        ],
        ..Default::default()
    };
    assert_eq!(plain_tokens(&day), "9,909+ tokens");
}

/// A known input total with no breakdown still bounds the request from below:
/// the reported parts never lower it.
#[test]
fn plain_tokens_lower_bound_keeps_a_known_input_total() {
    let day = DayTotals {
        // Input, noncached, read, write, output, reasoning, total.
        measured: [
            Metric {
                known: 10_000,
                unknown: 0,
            },
            Metric {
                known: 0,
                unknown: 1,
            },
            Metric {
                known: 0,
                unknown: 1,
            },
            Metric {
                known: 0,
                unknown: 1,
            },
            Metric {
                known: 0,
                unknown: 1,
            },
            Metric {
                known: 0,
                unknown: 0,
            },
            Metric {
                known: 0,
                unknown: 1,
            },
        ],
        ..Default::default()
    };
    assert_eq!(plain_tokens(&day), "10,000+ tokens");
}

#[path = "tokens/warmup_resume_tests.rs"]
mod warmup_resume_tests;
