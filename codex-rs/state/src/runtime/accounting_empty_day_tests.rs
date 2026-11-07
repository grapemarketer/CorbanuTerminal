//! FUP-02 (#352): a past day with nothing recorded says "no requests" only
//! when it ends before the conversation started; every other empty day stays
//! unproven. #1196: range buckets follow the same rule inside the daily totals.

use super::*;
use pretty_assertions::assert_eq;

const DAY: i64 = 86_400_000;
/// The default read: day 400, 00:00 UTC. Day 299 is then 101 days back,
/// past the 90-day request detail and inside a year of daily totals (kept
/// from day 36).
const NOW: i64 = 400 * DAY;

/// A UUIDv7 thread id whose embedded time is `ms`.
fn v7(ms: i64) -> ThreadId {
    let ms = u64::try_from(ms).unwrap();
    let uuid = Uuid::new_v7(uuid::Timestamp::from_unix(
        uuid::NoContext,
        ms / 1000,
        u32::try_from(ms % 1000).unwrap() * 1_000_000,
    ));
    ThreadId::from_string(&uuid.to_string()).unwrap()
}

/// A native thread row whose stored creation time is `created_ms`.
async fn thread(runtime: &StateRuntime, id: ThreadId, created_ms: i64) -> anyhow::Result<()> {
    let home = runtime.sqlite().home();
    let mut metadata = test_thread_metadata(home, id, home.to_path_buf());
    metadata.created_at = chrono::DateTime::from_timestamp_millis(created_ms).unwrap();
    runtime.upsert_thread(&metadata).await
}

async fn set(
    runtime: &StateRuntime,
    sql: &'static str,
    value: impl ToString,
) -> anyhow::Result<()> {
    sqlx::query(sql)
        .bind(value.to_string())
        .execute(runtime.pool.as_ref())
        .await?;
    Ok(())
}

/// The ledger's checkpoint, as maintenance or a write would leave it.
async fn checkpoint(runtime: &StateRuntime, ms: i64) -> anyhow::Result<()> {
    set(
        runtime,
        "UPDATE draft_accounting_retention_checkpoint SET completed_as_of_ms = CAST(? AS INTEGER), admission_active = 1",
        ms,
    )
    .await
}

/// When this home's ledger was installed, as SQLite stamped it.
async fn installed(runtime: &StateRuntime, day: i64) -> anyhow::Result<()> {
    let at = chrono::DateTime::from_timestamp_millis(day * DAY + 3_600_000).unwrap();
    set(
        runtime,
        "UPDATE _accounting_migrations SET installed_on = ? WHERE version = 1",
        at.format("%Y-%m-%d %H:%M:%S"),
    )
    .await
}

/// A ledger installed on day 0 and current to `now`, and a conversation whose
/// id and stored creation time are both `start_ms`.
async fn fixture(
    path: &Path,
    start_ms: i64,
    now: i64,
) -> anyhow::Result<(Arc<StateRuntime>, ThreadId)> {
    let runtime = open(path).await?;
    let owner = v7(start_ms);
    thread(&runtime, owner, start_ms).await?;
    AccountingStore::open(&runtime, /*as_of*/ 0).await?;
    installed(&runtime, /*day*/ 0).await?;
    checkpoint(&runtime, now).await?;
    Ok((runtime, owner))
}

async fn record(
    runtime: &StateRuntime,
    id: u128,
    thread: ThreadId,
    dispatched_ms: i64,
) -> anyhow::Result<()> {
    let mut a = attempt(id);
    a.thread_id = thread;
    a.dispatched_at_ms = dispatched_ms.try_into()?;
    AccountingStore::open(runtime, dispatched_ms)
        .await?
        .admit(thread, &a, &[], dispatched_ms)
        .await?;
    Ok(())
}

async fn empty(
    runtime: &StateRuntime,
    owner: ThreadId,
    day: i64,
    now: i64,
) -> anyhow::Result<Option<EmptyDay>> {
    let view = inspection(AccountingStore::inspect_day(runtime, owner, day, now).await?);
    assert_eq!(view.totals.attempts, 0, "day {day}");
    Ok(view.empty_day)
}

fn before(started_day: i64) -> Option<EmptyDay> {
    Some(EmptyDay::BeforeConversation { started_day })
}

#[tokio::test]
async fn a_day_before_the_conversation_started_has_no_requests() -> anyhow::Result<()> {
    let path = home();
    let (runtime, owner) = fixture(&path, 300 * DAY, NOW).await?;
    // #352 run Z: past the request detail, inside the daily totals. This read
    // used to say the day's detail had expired.
    assert_eq!(
        empty(&runtime, owner, /*day*/ 299, NOW).await?,
        before(/*started_day*/ 300)
    );
    // Even below the daily totals' floor: the proof needs no retained row.
    assert_eq!(
        empty(&runtime, owner, /*day*/ 0, NOW).await?,
        before(/*started_day*/ 300)
    );
    // From the day it started, nothing recorded proves nothing.
    for day in [300, 305, 350, 399, 400] {
        assert_eq!(
            empty(&runtime, owner, day, NOW).await?,
            Some(EmptyDay::NotRecorded),
            "day {day}"
        );
    }
    runtime.close().await;
    Ok(())
}

/// #314 (D228): a day a request of the conversation went unrecorded is a
/// day it sent one, so a gap marker moves the start as a record does, and
/// the day reads as possibly incomplete.
#[tokio::test]
async fn a_recording_gap_is_evidence_of_the_start() -> anyhow::Result<()> {
    let path = home();
    let (runtime, owner) = fixture(&path, 300 * DAY, NOW).await?;
    assert_eq!(
        empty(&runtime, owner, /*day*/ 298, NOW).await?,
        before(/*started_day*/ 300)
    );
    AccountingStore::record_gap(&runtime, owner, 299 * DAY + 1, 299 * DAY + 1).await?;
    assert_eq!(
        empty(&runtime, owner, /*day*/ 298, NOW).await?,
        before(/*started_day*/ 299)
    );
    let day =
        inspection(AccountingStore::inspect_day(&runtime, owner, /*utc_day*/ 299, NOW).await?);
    assert_eq!(day.empty_day, Some(EmptyDay::NotRecorded));
    assert!(day.recording_gap);
    runtime.close().await;
    Ok(())
}

#[tokio::test]
async fn a_day_with_requests_is_not_an_empty_day() -> anyhow::Result<()> {
    let path = home();
    let (runtime, owner) = fixture(&path, 300 * DAY, 300 * DAY).await?;
    record(&runtime, /*id*/ 1, owner, 350 * DAY + 5).await?;
    checkpoint(&runtime, NOW).await?;
    let view =
        inspection(AccountingStore::inspect_day(&runtime, owner, /*utc_day*/ 350, NOW).await?);
    assert_eq!((view.totals.attempts, view.empty_day), (1, None));
    runtime.close().await;
    Ok(())
}

#[tokio::test]
async fn the_start_is_compared_by_utc_day() -> anyhow::Result<()> {
    // 23:59:59.999 UTC on day 299: that day may hold requests, the one before
    // cannot. Its stored creation time is whole seconds, 23:59:59.
    let path = home();
    let (runtime, owner) = fixture(&path, 300 * DAY - 1, NOW).await?;
    assert_eq!(
        empty(&runtime, owner, /*day*/ 299, NOW).await?,
        Some(EmptyDay::NotRecorded)
    );
    assert_eq!(
        empty(&runtime, owner, /*day*/ 298, NOW).await?,
        before(/*started_day*/ 299)
    );
    runtime.close().await;
    // 00:00:00.000 UTC on day 300: day 299 ends before it.
    let path = home();
    let (runtime, owner) = fixture(&path, 300 * DAY, NOW).await?;
    assert_eq!(
        empty(&runtime, owner, /*day*/ 299, NOW).await?,
        before(/*started_day*/ 300)
    );
    assert_eq!(
        empty(&runtime, owner, /*day*/ 300, NOW).await?,
        Some(EmptyDay::NotRecorded)
    );
    runtime.close().await;
    Ok(())
}

#[tokio::test]
async fn past_the_daily_totals_only_the_proven_day_is_shown() -> anyhow::Result<()> {
    // Read on day 800: daily totals are kept from day 436.
    let now = 800 * DAY;
    let path = home();
    let (runtime, owner) = fixture(&path, 300 * DAY, now).await?;
    assert_eq!(
        empty(&runtime, owner, /*day*/ 299, now).await?,
        before(/*started_day*/ 300)
    );
    assert!(matches!(
        AccountingStore::inspect_day(&runtime, owner, /*utc_day*/ 350, now).await?,
        InspectionDay::DetailUnavailable {
            compact: false,
            coverage: RetentionCoverage {
                aggregate_day_floor: 436,
                ..
            },
            ..
        }
    ));
    assert_eq!(
        empty(&runtime, owner, /*day*/ 436, now).await?,
        Some(EmptyDay::NotRecorded)
    );
    runtime.close().await;
    Ok(())
}

#[tokio::test]
async fn a_day_before_the_ledger_existed_is_not_recorded() -> anyhow::Result<()> {
    let path = home();
    let (runtime, owner) = fixture(&path, 300 * DAY, NOW).await?;
    installed(&runtime, /*day*/ 320).await?;
    assert_eq!(
        empty(&runtime, owner, /*day*/ 310, NOW).await?,
        Some(EmptyDay::BeforeLedger {
            ledger_started_day: 320
        })
    );
    assert_eq!(
        empty(&runtime, owner, /*day*/ 320, NOW).await?,
        Some(EmptyDay::NotRecorded)
    );
    // The conversation's start still proves the days before it.
    assert_eq!(
        empty(&runtime, owner, /*day*/ 299, NOW).await?,
        before(/*started_day*/ 300)
    );
    // An install time this build cannot read states nothing about the ledger.
    set(
        &runtime,
        "UPDATE _accounting_migrations SET installed_on = ? WHERE version = 1",
        "unreadable",
    )
    .await?;
    assert_eq!(
        empty(&runtime, owner, /*day*/ 310, NOW).await?,
        Some(EmptyDay::NotRecorded)
    );
    runtime.close().await;
    Ok(())
}

#[tokio::test]
async fn pending_or_lagging_accounting_is_never_an_empty_day() -> anyhow::Result<()> {
    let path = home();
    // The ledger last moved on day 250; the conversation started on day 300.
    let (runtime, owner) = fixture(&path, 300 * DAY, 250 * DAY).await?;
    // After the checkpoint nothing is verified yet, not even before the start.
    assert_eq!(
        AccountingStore::inspect_day(&runtime, owner, /*utc_day*/ 260, 301 * DAY).await?,
        InspectionDay::CheckpointLag
    );
    assert_eq!(
        empty(&runtime, owner, /*day*/ 240, 301 * DAY).await?,
        before(/*started_day*/ 300)
    );
    runtime.close().await;

    // A request older than the request detail that maintenance has not
    // compacted yet is pending, not absent.
    let path = home();
    let (runtime, owner) = fixture(&path, 300 * DAY, 300 * DAY).await?;
    record(&runtime, /*id*/ 1, owner, 305 * DAY).await?;
    checkpoint(&runtime, NOW).await?;
    assert_eq!(
        AccountingStore::inspect_day(&runtime, owner, /*utc_day*/ 305, NOW).await?,
        InspectionDay::CheckpointLag
    );
    runtime.close().await;
    Ok(())
}

#[tokio::test]
async fn other_threads_on_the_day_can_only_withdraw_the_proof() -> anyhow::Result<()> {
    // Read on day 320, so every request below keeps its detail.
    let now = 320 * DAY;
    let path = home();
    let (runtime, owner) = fixture(&path, 300 * DAY, 290 * DAY).await?;
    // A subagent whose request is recorded on day 295, before the root's id
    // time: the day is the conversation's, and the start moves back to it.
    let child = v7(310 * DAY);
    thread(&runtime, child, 310 * DAY).await?;
    runtime
        .upsert_thread_spawn_edge(
            owner,
            child,
            crate::DirectionalThreadSpawnEdgeStatus::Closed,
        )
        .await?;
    record(&runtime, /*id*/ 1, child, 295 * DAY).await?;
    checkpoint(&runtime, now).await?;
    let view =
        inspection(AccountingStore::inspect_day(&runtime, owner, /*utc_day*/ 295, now).await?);
    assert_eq!((view.totals.attempts, view.empty_day), (1, None));
    assert_eq!(
        empty(&runtime, owner, /*day*/ 294, now).await?,
        before(/*started_day*/ 295)
    );
    // A thread of unknown ancestry with a request on day 293 may be an
    // unlinked subagent: that day is not proven.
    let stranger = v7(200 * DAY);
    let home = runtime.sqlite().home();
    let mut metadata = test_thread_metadata(home, stranger, home.to_path_buf());
    metadata.source = "unknown".to_string();
    runtime.upsert_thread(&metadata).await?;
    set(
        &runtime,
        "UPDATE draft_accounting_retention_checkpoint SET completed_as_of_ms = CAST(? AS INTEGER)",
        293 * DAY,
    )
    .await?;
    record(&runtime, /*id*/ 2, stranger, 293 * DAY).await?;
    checkpoint(&runtime, now).await?;
    let view =
        inspection(AccountingStore::inspect_day(&runtime, owner, /*utc_day*/ 293, now).await?);
    assert_eq!(
        (view.unknown_parent_totals.attempts, view.empty_day),
        (1, Some(EmptyDay::NotRecorded))
    );
    assert_eq!(
        empty(&runtime, owner, /*day*/ 292, now).await?,
        before(/*started_day*/ 295)
    );
    runtime.close().await;
    Ok(())
}

#[tokio::test]
async fn the_start_is_the_earliest_evidence_and_needs_a_time_bearing_id() -> anyhow::Result<()> {
    // A stored creation time rewritten later (a placeholder upsert) cannot
    // move the start past the id's time.
    let path = home();
    let (runtime, owner) = fixture(&path, 300 * DAY, NOW).await?;
    thread(&runtime, owner, 390 * DAY).await?;
    assert_eq!(
        empty(&runtime, owner, /*day*/ 299, NOW).await?,
        before(/*started_day*/ 300)
    );
    // An earlier stored creation time moves it back.
    thread(&runtime, owner, 280 * DAY).await?;
    assert_eq!(
        empty(&runtime, owner, /*day*/ 299, NOW).await?,
        Some(EmptyDay::NotRecorded)
    );
    assert_eq!(
        empty(&runtime, owner, /*day*/ 279, NOW).await?,
        before(/*started_day*/ 280)
    );
    // A subagent id that carries no time withdraws the proof.
    let legacy = ThreadId::from_string(&Uuid::from_u128(77).to_string())?;
    runtime
        .upsert_thread_spawn_edge(
            owner,
            legacy,
            crate::DirectionalThreadSpawnEdgeStatus::Closed,
        )
        .await?;
    assert_eq!(
        empty(&runtime, owner, /*day*/ 279, NOW).await?,
        Some(EmptyDay::NotRecorded)
    );
    runtime.close().await;
    // So does the conversation's own.
    let path = home();
    let runtime = open(&path).await?;
    thread(&runtime, legacy, 300 * DAY).await?;
    AccountingStore::open(&runtime, /*as_of*/ 0).await?;
    installed(&runtime, /*day*/ 0).await?;
    checkpoint(&runtime, NOW).await?;
    assert_eq!(
        empty(&runtime, legacy, /*day*/ 100, NOW).await?,
        Some(EmptyDay::NotRecorded)
    );
    assert!(matches!(
        AccountingStore::inspect_day(&runtime, legacy, /*utc_day*/ 0, NOW).await?,
        InspectionDay::DetailUnavailable { compact: false, .. }
    ));
    runtime.close().await;
    Ok(())
}

/// One range bucket's days, read at `now`.
async fn bucket(
    runtime: &StateRuntime,
    owner: ThreadId,
    (start_ms, end_ms): (i64, i64),
    grouping: InspectionGrouping,
    now: i64,
) -> anyhow::Result<Vec<InspectionDay>> {
    let range = InspectionRange {
        start_ms,
        end_ms,
        grouping,
    };
    let InspectionDay::Range { mut buckets, .. } =
        AccountingStore::inspect_range(runtime, owner, range, now).await?
    else {
        panic!("not a range")
    };
    assert_eq!(buckets.len(), 1);
    Ok(buckets.remove(0).days)
}

/// The classification of a bucket's only day, which must be empty.
fn only_empty(days: &[InspectionDay]) -> Option<EmptyDay> {
    let [InspectionDay::Ready(view)] = days else {
        panic!("{days:?}")
    };
    assert_eq!(view.totals.attempts, 0);
    view.empty_day
}

const HOUR: i64 = 3_600_000;

#[tokio::test]
async fn range_buckets_classify_empty_days_as_the_day_view_does() -> anyhow::Result<()> {
    let path = home();
    let (runtime, owner) = fixture(&path, 300 * DAY, NOW).await?;
    for grouping in [InspectionGrouping::Day, InspectionGrouping::Hour] {
        let span = |day: i64| match grouping {
            InspectionGrouping::Hour => (day * DAY + 3 * HOUR, day * DAY + 4 * HOUR),
            _ => (day * DAY, (day + 1) * DAY),
        };
        // #1196: inside the request detail, as in the day view.
        assert_eq!(
            only_empty(&bucket(&runtime, owner, span(350), grouping, NOW).await?),
            Some(EmptyDay::NotRecorded),
            "{grouping:?}"
        );
        // Past the request detail, inside the daily totals: no longer
        // "detail unavailable", and proven before the conversation started.
        assert_eq!(
            only_empty(&bucket(&runtime, owner, span(299), grouping, NOW).await?),
            before(/*started_day*/ 300),
            "{grouping:?}"
        );
        // Below the daily totals' floor a range stays unavailable, even
        // where the day view proves the day.
        assert!(
            matches!(
                bucket(&runtime, owner, span(0), grouping, NOW)
                    .await?
                    .as_slice(),
                [InspectionDay::DetailUnavailable { compact: false, .. }]
            ),
            "{grouping:?}"
        );
        assert_eq!(
            empty(&runtime, owner, /*day*/ 0, NOW).await?,
            before(/*started_day*/ 300)
        );
    }
    runtime.close().await;
    Ok(())
}

#[tokio::test]
async fn a_range_window_past_the_detail_keeps_a_recorded_day_unavailable() -> anyhow::Result<()> {
    let path = home();
    let (runtime, owner) = fixture(&path, 300 * DAY, 290 * DAY).await?;
    // Day 295 has one retained raw request at 01:00, awaiting compaction.
    record(&runtime, /*id*/ 1, owner, 295 * DAY + HOUR).await?;
    checkpoint(&runtime, NOW).await?;
    // Another hour of that day is not "nothing recorded".
    assert!(matches!(
        bucket(
            &runtime,
            owner,
            (295 * DAY + 5 * HOUR, 295 * DAY + 6 * HOUR),
            InspectionGrouping::Hour,
            NOW,
        )
        .await?
        .as_slice(),
        [InspectionDay::DetailUnavailable { compact: false, .. }]
    ));
    // The day before it is empty, but the request moved the start back.
    assert_eq!(
        only_empty(
            &bucket(
                &runtime,
                owner,
                (294 * DAY, 295 * DAY),
                InspectionGrouping::Day,
                NOW
            )
            .await?
        ),
        before(/*started_day*/ 295)
    );
    runtime.close().await;
    Ok(())
}

#[tokio::test]
async fn unknown_ancestry_elsewhere_on_the_day_withdraws_a_buckets_proof() -> anyhow::Result<()> {
    // Read on day 320, so every request below keeps its detail.
    let now = 320 * DAY;
    let path = home();
    let (runtime, owner) = fixture(&path, 300 * DAY, 290 * DAY).await?;
    // A thread of unknown ancestry with a request at 01:00 on day 293 may be
    // an unlinked subagent, so the conversation may have run that day.
    let stranger = v7(200 * DAY);
    let home = runtime.sqlite().home();
    let mut metadata = test_thread_metadata(home, stranger, home.to_path_buf());
    metadata.source = "unknown".to_string();
    runtime.upsert_thread(&metadata).await?;
    record(&runtime, /*id*/ 1, stranger, 293 * DAY + HOUR).await?;
    checkpoint(&runtime, now).await?;
    let days = bucket(
        &runtime,
        owner,
        (293 * DAY + 5 * HOUR, 293 * DAY + 6 * HOUR),
        InspectionGrouping::Hour,
        now,
    )
    .await?;
    let [InspectionDay::Ready(view)] = days.as_slice() else {
        panic!("{days:?}")
    };
    // Nothing of it is in this hour, and the proof is still withdrawn.
    assert_eq!(
        (view.unknown_parent_totals.attempts, view.empty_day),
        (0, Some(EmptyDay::NotRecorded))
    );
    // The day before is still proven.
    assert_eq!(
        only_empty(
            &bucket(
                &runtime,
                owner,
                (292 * DAY + 5 * HOUR, 292 * DAY + 6 * HOUR),
                InspectionGrouping::Hour,
                now
            )
            .await?
        ),
        before(/*started_day*/ 300)
    );
    runtime.close().await;
    Ok(())
}

async fn maintain(runtime: &StateRuntime, now: i64) -> anyhow::Result<()> {
    AccountingStore::open(runtime, now)
        .await?
        .maintain(now)
        .await
}

#[tokio::test]
async fn a_subagent_compact_day_is_unavailable_and_moves_the_start() -> anyhow::Result<()> {
    let path = home();
    let (runtime, owner) = fixture(&path, 300 * DAY, 290 * DAY).await?;
    let child = v7(310 * DAY);
    thread(&runtime, child, 310 * DAY).await?;
    runtime
        .upsert_thread_spawn_edge(
            owner,
            child,
            crate::DirectionalThreadSpawnEdgeStatus::Closed,
        )
        .await?;
    record(&runtime, /*id*/ 1, child, 295 * DAY).await?;
    maintain(&runtime, NOW).await?;
    // The owner has nothing on day 295, past the request detail; the
    // subagent's compacted day still keeps the whole day unavailable.
    assert!(matches!(
        AccountingStore::inspect_day(&runtime, owner, /*utc_day*/ 295, NOW).await?,
        InspectionDay::DetailUnavailable { compact: true, .. }
    ));
    // Its compacted row is the earliest record: the start moves back to it.
    assert_eq!(
        empty(&runtime, owner, /*day*/ 294, NOW).await?,
        before(/*started_day*/ 295)
    );
    // #1196: a range reads both days the same way, in any part of the day.
    for (start, end) in [
        (295 * DAY, 296 * DAY),
        (295 * DAY + 5 * HOUR, 295 * DAY + 6 * HOUR),
    ] {
        let grouping = if end - start == DAY {
            InspectionGrouping::Day
        } else {
            InspectionGrouping::Hour
        };
        assert!(matches!(
            bucket(&runtime, owner, (start, end), grouping, NOW)
                .await?
                .as_slice(),
            [InspectionDay::DetailUnavailable { compact: true, .. }]
        ));
        assert_eq!(
            only_empty(&bucket(&runtime, owner, (start - DAY, end - DAY), grouping, NOW).await?),
            before(/*started_day*/ 295)
        );
    }
    runtime.close().await;
    Ok(())
}

#[tokio::test]
async fn an_unreadable_unknown_ancestry_day_withdraws_the_proof() -> anyhow::Result<()> {
    let path = home();
    let (runtime, owner) = fixture(&path, 300 * DAY, 290 * DAY).await?;
    let stranger = v7(200 * DAY);
    let home = runtime.sqlite().home();
    let mut metadata = test_thread_metadata(home, stranger, home.to_path_buf());
    metadata.source = "unknown".to_string();
    runtime.upsert_thread(&metadata).await?;
    record(&runtime, /*id*/ 1, stranger, 295 * DAY).await?;
    // Compacted, its day cannot be read in detail.
    maintain(&runtime, NOW).await?;
    let view =
        inspection(AccountingStore::inspect_day(&runtime, owner, /*utc_day*/ 295, NOW).await?);
    assert_eq!(
        (view.unknown_parent_unavailable_threads, view.empty_day),
        (1, Some(EmptyDay::NotRecorded))
    );
    runtime.close().await;
    Ok(())
}

#[tokio::test]
async fn spawn_edges_without_native_rows_count_by_their_id() -> anyhow::Result<()> {
    let path = home();
    let (runtime, owner) = fixture(&path, 300 * DAY, NOW).await?;
    // A later subagent with no native row changes nothing.
    runtime
        .upsert_thread_spawn_edge(
            owner,
            v7(310 * DAY),
            crate::DirectionalThreadSpawnEdgeStatus::Closed,
        )
        .await?;
    assert_eq!(
        empty(&runtime, owner, /*day*/ 299, NOW).await?,
        before(/*started_day*/ 300)
    );
    // One whose id is earlier moves the start back to it.
    runtime
        .upsert_thread_spawn_edge(
            owner,
            v7(250 * DAY),
            crate::DirectionalThreadSpawnEdgeStatus::Closed,
        )
        .await?;
    assert_eq!(
        empty(&runtime, owner, /*day*/ 299, NOW).await?,
        Some(EmptyDay::NotRecorded)
    );
    assert_eq!(
        empty(&runtime, owner, /*day*/ 249, NOW).await?,
        before(/*started_day*/ 250)
    );
    runtime.close().await;
    Ok(())
}

#[tokio::test]
async fn a_spawn_tree_too_large_to_read_proves_nothing() -> anyhow::Result<()> {
    let path = home();
    let (runtime, owner) = fixture(&path, 300 * DAY, NOW).await?;
    // A tree of 10,000 threads (the owner and 9,999 subagents with v7 ids
    // from day 310) is still read, and proven.
    let edges = |from: i64, to: i64| {
        sqlx::query(
            "WITH RECURSIVE n(i) AS (VALUES(?2) UNION ALL SELECT i+1 FROM n WHERE i < ?3)
             INSERT INTO thread_spawn_edges (parent_thread_id, child_thread_id, status)
             SELECT ?1, printf('00063c73-6800-7000-8000-%012x', i), 'closed' FROM n",
        )
        .bind(owner.to_string())
        .bind(from)
        .bind(to)
        .execute(runtime.pool.as_ref())
    };
    edges(/*from*/ 1, /*to*/ 9_999).await?;
    assert_eq!(
        empty(&runtime, owner, /*day*/ 299, NOW).await?,
        before(/*started_day*/ 300)
    );
    // One more subagent and the tree is past the bound.
    edges(/*from*/ 10_000, /*to*/ 10_000).await?;
    assert_eq!(
        empty(&runtime, owner, /*day*/ 299, NOW).await?,
        Some(EmptyDay::NotRecorded)
    );
    runtime.close().await;
    Ok(())
}

#[tokio::test]
async fn below_the_floor_an_unknown_ancestry_row_withdraws_the_proof() -> anyhow::Result<()> {
    let path = home();
    let (runtime, owner) = fixture(&path, 300 * DAY, 290 * DAY).await?;
    let stranger = v7(200 * DAY);
    let home = runtime.sqlite().home();
    let mut metadata = test_thread_metadata(home, stranger, home.to_path_buf());
    metadata.source = "unknown".to_string();
    runtime.upsert_thread(&metadata).await?;
    record(&runtime, /*id*/ 1, stranger, 299 * DAY).await?;
    maintain(&runtime, NOW).await?;
    // The ledger moves to day 800 (floor 436) before expiry removes that row.
    let now = 800 * DAY;
    checkpoint(&runtime, now).await?;
    assert_eq!(
        empty(&runtime, owner, /*day*/ 298, now).await?,
        before(/*started_day*/ 300)
    );
    assert!(matches!(
        AccountingStore::inspect_day(&runtime, owner, /*utc_day*/ 299, now).await?,
        InspectionDay::DetailUnavailable { compact: false, .. }
    ));
    runtime.close().await;
    Ok(())
}
