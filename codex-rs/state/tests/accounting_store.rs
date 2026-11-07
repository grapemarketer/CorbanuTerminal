//! These tests link the normal library, never include private implementation/fixture DDL.
use anyhow::Context;
use codex_protocol::ThreadId;
use codex_protocol::protocol::SessionSource;
use codex_state::SqliteConfig;
use codex_state::StateRuntime;
use codex_state::ThreadMetadataBuilder;
use codex_state::accounting::*;
use codex_utils_absolute_path::AbsolutePathBuf;
use pretty_assertions::assert_eq;
use serde_json::json;
use sqlx::Connection;
use sqlx::SqliteConnection;
use std::path::Path;
use std::sync::Arc;
use uuid::Uuid;

const DAY: i64 = 86_400_000;

fn ready(result: InspectionDay) -> Inspection {
    let InspectionDay::Ready(view) = result else {
        panic!("{result:?}")
    };
    view
}

#[tokio::test]
async fn accounting_inspect_public_reopens_twice() -> anyhow::Result<()> {
    let path = home();
    let runtime = open(&path).await?;
    let a = attempt(/*id*/ 1, /*time*/ 0)?;
    native(&runtime, a.thread_id).await?;
    let store = AccountingStore::open(&runtime, /*as_of*/ 0).await?;
    store
        .admit(a.thread_id, &a, &[snapshot()?], /*as_of*/ 0)
        .await?;
    store
        .observe(
            a.thread_id,
            &a,
            &[observation(&a, /*revision*/ 1, /*input*/ 4)?],
            /*as_of*/ 0,
        )
        .await?;
    let expected = AccountingStore::inspect_day(
        &runtime,
        a.thread_id,
        /*utc_day*/ 0,
        /*read_at_ms*/ 0,
    )
    .await?;
    runtime.close().await;
    for _ in 0..2 {
        let runtime = open(&path).await?;
        assert_eq!(
            AccountingStore::inspect_day(
                &runtime,
                a.thread_id,
                /*utc_day*/ 0,
                /*read_at_ms*/ 0
            )
            .await?,
            expected
        );
        runtime.close().await;
    }
    Ok(())
}

// Read every table/column, including schemas and checkpoint, without fixture DDL.
async fn inspection_tables(conn: &mut SqliteConnection) -> anyhow::Result<Vec<Vec<String>>> {
    let tables: Vec<String> =
        sqlx::query_scalar("SELECT name FROM sqlite_schema WHERE type = 'table' ORDER BY name")
            .fetch_all(&mut *conn)
            .await?;
    let mut rows = vec![
        sqlx::query_scalar("SELECT json_array(name, sql) FROM sqlite_schema ORDER BY name")
            .fetch_all(&mut *conn)
            .await?,
    ];
    for table in tables {
        let columns: Vec<String> =
            sqlx::query_scalar("SELECT name FROM pragma_table_info(?) ORDER BY cid")
                .bind(&table)
                .fetch_all(&mut *conn)
                .await?;
        let columns = columns
            .iter()
            .map(|c| format!("quote(\"{}\")", c.replace('"', "\"\"")))
            .collect::<Vec<_>>()
            .join(",");
        let table = table.replace('"', "\"\"");
        rows.push(
            sqlx::query_scalar(sqlx::AssertSqlSafe(format!(
                "SELECT json_array({columns}) FROM \"{table}\" ORDER BY 1"
            )))
            .fetch_all(&mut *conn)
            .await?,
        );
    }
    Ok(rows)
}

#[tokio::test]
async fn accounting_inspect_public_reads_do_not_write() -> anyhow::Result<()> {
    let path = home();
    let runtime = open(&path).await?;
    let a = attempt(/*id*/ 1, /*time*/ 0)?;
    native(&runtime, a.thread_id).await?;
    let mut conn = connection(&runtime).await?;
    let before = inspection_tables(&mut conn).await?;
    assert_eq!(
        AccountingStore::inspect_day(
            &runtime,
            a.thread_id,
            /*utc_day*/ 0,
            /*read_at_ms*/ 0
        )
        .await?,
        InspectionDay::Absent
    );
    assert_eq!(inspection_tables(&mut conn).await?, before);
    AccountingStore::open(&runtime, /*as_of*/ 0)
        .await?
        .admit(a.thread_id, &a, &[], /*as_of*/ 0)
        .await?;
    let before = inspection_tables(&mut conn).await?;
    assert_eq!(
        ready(
            AccountingStore::inspect_day(
                &runtime,
                a.thread_id,
                /*utc_day*/ 0,
                /*read_at_ms*/ 1
            )
            .await?
        )
        .totals
        .attempts,
        1
    );
    assert!(
        AccountingStore::inspect_day(
            &runtime,
            a.thread_id,
            /*utc_day*/ -1,
            /*read_at_ms*/ 0
        )
        .await
        .is_err()
    );
    // Drop a polled read at its first asynchronous database boundary. SQLite's worker thread
    // can answer every step before a loaded runner's first poll returns, so retry until a poll
    // actually suspends; a read that completes instead must still be correct.
    let mut dropped_suspended_read = false;
    for _ in 0..100 {
        let mut read = Box::pin(AccountingStore::inspect_day(
            &runtime,
            a.thread_id,
            /*utc_day*/ 0,
            /*read_at_ms*/ 0,
        ));
        let first_poll = std::future::poll_fn(|cx| {
            std::task::Poll::Ready(std::future::Future::poll(read.as_mut(), cx))
        })
        .await;
        match first_poll {
            std::task::Poll::Pending => {
                drop(read);
                dropped_suspended_read = true;
                break;
            }
            std::task::Poll::Ready(result) => {
                assert_eq!(ready(result?).totals.attempts, 1);
            }
        }
    }
    assert!(
        dropped_suspended_read,
        "no read suspended at a database boundary"
    );
    assert_eq!(inspection_tables(&mut conn).await?, before);
    conn.close().await?;
    runtime.close().await;
    Ok(())
}

#[tokio::test]
async fn accounting_inspect_public_identity_retry_scope() -> anyhow::Result<()> {
    let path = home();
    let runtime = open(&path).await?;
    let a = attempt(/*id*/ 1, DAY - 1)?;
    let mut retry = attempt(/*id*/ 2, DAY)?;
    retry.request_id = a.request_id;
    retry.retry_of = Some(a.attempt_id);
    let separate = attempt(/*id*/ 3, DAY)?;
    let mut other = attempt(/*id*/ 4, DAY)?;
    other.thread_id = ThreadId::new();
    for owner in [a.thread_id, other.thread_id] {
        native(&runtime, owner).await?;
    }
    let store = AccountingStore::open(&runtime, DAY - 1).await?;
    for a in [&a, &retry, &separate, &other] {
        store
            .admit(a.thread_id, a, &[], i64::from(a.dispatched_at_ms))
            .await?;
    }
    let mut retry2 = retry.clone();
    retry2.attempt_id = Uuid::from_u128(5);
    retry2.retry_of = Some(retry.attempt_id);
    store.admit(a.thread_id, &retry2, &[], DAY).await?;
    let view =
        ready(AccountingStore::inspect_day(&runtime, a.thread_id, /*utc_day*/ 1, DAY).await?);
    let members: std::collections::BTreeMap<_, Vec<_>> = view
        .requests
        .iter()
        .map(|(id, rows)| (*id, rows.iter().map(|q| q.attempt.attempt_id).collect()))
        .collect();
    assert_eq!(
        members,
        std::collections::BTreeMap::from([
            (a.request_id, vec![retry.attempt_id, retry2.attempt_id]),
            (separate.request_id, vec![separate.attempt_id]),
        ])
    );
    assert_eq!(view.totals.attempts, 3);
    assert_eq!(
        ready(AccountingStore::inspect_day(&runtime, a.thread_id, /*utc_day*/ 0, DAY).await?)
            .totals
            .attempts,
        1
    );
    runtime.close().await;
    Ok(())
}

#[tokio::test]
async fn accounting_inspect_public_literal_partial_goldens() -> anyhow::Result<()> {
    for (priced, patch, expected_cost, unknown, noncached) in [
        (
            true,
            json!({"input":100,"read":20,"output":40}),
            "0.00018",
            1,
            None,
        ),
        (
            true,
            json!({"input":100,"read":20,"write":0,"output":40}),
            "0.00026",
            0,
            Some(80),
        ),
        // Unpriced: zero tokens are not a zero-cost claim, so the estimate is
        // incomplete rather than a complete zero.
        (
            false,
            json!({"input":0,"read":0,"write":0,"output":0}),
            "0",
            1,
            Some(0),
        ),
        (
            false,
            json!({"input":100,"read":20,"write":0,"output":40}),
            "0",
            1,
            Some(80),
        ),
    ] {
        let path = home();
        let runtime = open(&path).await?;
        let mut a = attempt(/*id*/ 1, /*time*/ 0)?;
        a.dialect = Dialect::Inclusive;
        native(&runtime, a.thread_id).await?;
        let mut price = snapshot()?;
        price.rates =
            serde_json::from_value(json!({"noncached":"1","read":"1","write":"2","output":"4"}))?;
        let store = AccountingStore::open(&runtime, /*as_of*/ 0).await?;
        store
            .admit(
                a.thread_id,
                &a,
                if priced {
                    std::slice::from_ref(&price)
                } else {
                    &[]
                },
                /*as_of*/ 0,
            )
            .await?;
        let mut o = observation(&a, /*revision*/ 1, /*input*/ 0)?;
        o.patch = serde_json::from_value(patch)?;
        store.observe(a.thread_id, &a, &[o], /*as_of*/ 0).await?;
        let view = ready(
            AccountingStore::inspect_day(
                &runtime,
                a.thread_id,
                /*utc_day*/ 0,
                /*read_at_ms*/ 0,
            )
            .await?,
        );
        assert_eq!(
            serde_json::to_value(view.totals.known_usd)?,
            json!(expected_cost)
        );
        assert_eq!(view.totals.unknown_estimates, unknown);
        let quote = &view.requests[&a.request_id][0];
        assert_eq!(quote.usage.noncached, noncached);
        assert_eq!(quote.snapshot.is_some(), priced);
        assert_eq!(quote.all_buckets_priced.is_none(), unknown == 1);
        runtime.close().await;
    }
    Ok(())
}

#[tokio::test]
async fn accounting_inspect_public_delete_and_empty() -> anyhow::Result<()> {
    let path = home();
    let runtime = open(&path).await?;
    let now = chrono::Utc::now().timestamp_millis();
    let a = attempt(/*id*/ 1, now)?;
    native(&runtime, a.thread_id).await?;
    assert_eq!(
        AccountingStore::inspect_day(&runtime, a.thread_id, now / DAY, now).await?,
        InspectionDay::Absent
    );
    let store = AccountingStore::open(&runtime, now).await?;
    assert_eq!(
        ready(AccountingStore::inspect_day(&runtime, a.thread_id, now / DAY, now).await?)
            .totals
            .attempts,
        0
    );
    let mut other = attempt(/*id*/ 2, now)?;
    other.thread_id = ThreadId::new();
    native(&runtime, other.thread_id).await?;
    for a in [&a, &other] {
        store.admit(a.thread_id, a, &[], now).await?;
    }
    let other_before =
        AccountingStore::inspect_day(&runtime, other.thread_id, now / DAY, now).await?;
    runtime.delete_thread(a.thread_id).await?;
    let read_at = chrono::Utc::now().timestamp_millis();
    assert_eq!(
        AccountingStore::inspect_day(&runtime, a.thread_id, now / DAY, read_at).await?,
        InspectionDay::MissingThread
    );
    let mut other_after =
        ready(AccountingStore::inspect_day(&runtime, other.thread_id, now / DAY, read_at).await?);
    let before = ready(other_before);
    other_after.read_at_ms = before.read_at_ms;
    // Deletion can advance the store checkpoint; retained evidence must remain identical.
    assert_eq!(
        (other_after.owner, other_after.totals, other_after.requests),
        (before.owner, before.totals, before.requests)
    );
    native(&runtime, a.thread_id).await?;
    assert_eq!(
        ready(AccountingStore::inspect_day(&runtime, a.thread_id, now / DAY, read_at).await?)
            .totals
            .attempts,
        0
    );
    runtime.close().await;
    Ok(())
}

// Populate valid unrelated history without invoking maintenance for every clone.
async fn unrelated_population(runtime: &StateRuntime, count: u128) -> anyhow::Result<()> {
    let store = AccountingStore::open(runtime, /*as_of*/ 0).await?;
    let owner = attempt(/*id*/ 1, /*time*/ 0)?;
    let quote = store
        .admit(owner.thread_id, &owner, &[], /*as_of*/ 0)
        .await?;
    let threads: Vec<_> = (0..600)
        .map(|id| ThreadId::from_string(&Uuid::from_u128(100_000 + id).to_string()))
        .collect::<Result<_, _>>()?;
    for thread in &threads {
        native(runtime, *thread).await?;
    }
    let mut conn = connection(runtime).await?;
    sqlx::query("BEGIN").execute(&mut conn).await?;
    for id in 2..=count {
        let mut a = attempt(id, /*time*/ 0)?;
        a.thread_id = threads[(id as usize - 2) % threads.len()];
        let mut q = quote.clone();
        q.attempt = a.clone();
        sqlx::query("INSERT INTO draft_accounting_attempts VALUES (?, ?, ?)")
            .bind(a.attempt_id.to_string())
            .bind(a.request_id.to_string())
            .bind(serde_json::to_string(&a)?)
            .execute(&mut conn)
            .await?;
        sqlx::query("INSERT INTO draft_accounting_price_bindings VALUES (?, NULL)")
            .bind(a.attempt_id.to_string())
            .execute(&mut conn)
            .await?;
        sqlx::query("INSERT INTO draft_accounting_estimates VALUES (?, '[]', ?)")
            .bind(a.attempt_id.to_string())
            .bind(serde_json::to_string(&q)?)
            .execute(&mut conn)
            .await?;
        sqlx::query("INSERT INTO draft_accounting_contributions VALUES (?, ?, 0, '[]')")
            .bind(a.attempt_id.to_string())
            .bind(a.thread_id.to_string())
            .execute(&mut conn)
            .await?;
    }
    sqlx::query("COMMIT").execute(&mut conn).await?;
    conn.close().await?;
    Ok(())
}

#[tokio::test]
async fn accounting_inspect_small_day_on_busy_host() -> anyhow::Result<()> {
    let path = home();
    let runtime = open(&path).await?;
    let a = attempt(/*id*/ 1, /*time*/ 0)?;
    native(&runtime, a.thread_id).await?;
    unrelated_population(&runtime, /*count*/ 12_001).await?;
    let mut view = ready(
        AccountingStore::inspect_day(
            &runtime,
            a.thread_id,
            /*utc_day*/ 0,
            /*read_at_ms*/ 0,
        )
        .await?,
    );
    // All 600 other conversations are counted even though the work budget
    // cannot read every one; the unread ones are stated, never zeroed.
    let others = view
        .other_conversations
        .take()
        .expect("day reads other conversations");
    assert_eq!(others.conversations, 600);
    assert!(
        others.unavailable > 0 && others.unavailable < 600,
        "{}",
        others.unavailable
    );
    assert_eq!(
        view.totals,
        DayTotals::from_quotes(view.requests.values().flatten())?
    );
    assert_eq!(
        (
            view.totals.attempts,
            view.unknown_parent_totals.attempts,
            view.totals.unknown_estimates
        ),
        (1, 0, 1)
    );
    assert_eq!(view.totals.known_usd, "0".to_owned().try_into()?);
    assert!(
        view.totals
            .measured
            .iter()
            .all(|metric| metric.known == 0 && metric.unknown == 1)
    );
    let range = AccountingStore::inspect_range(
        &runtime,
        a.thread_id,
        InspectionRange {
            start_ms: 0,
            end_ms: DAY,
            grouping: InspectionGrouping::Day,
        },
        /*read_at_ms*/ 0,
    )
    .await?;
    let InspectionDay::Range { buckets, .. } = range else {
        panic!("{range:?}")
    };
    assert_eq!(buckets.len(), 1);
    assert_eq!(buckets[0].days.len(), 1);
    let InspectionDay::Ready(bucket_view) = &buckets[0].days[0] else {
        panic!("{buckets:?}")
    };
    // Range buckets do not read other conversations.
    assert_eq!(bucket_view, &view);
    // The unrelated attempt's observation cap must not become the owner's cap.
    let mut other = view.requests[&a.request_id][0].clone();
    other.attempt = attempt(/*id*/ 2, /*time*/ 0)?;
    other.attempt.thread_id = ThreadId::from_string(&Uuid::from_u128(100_000).to_string())?;
    other.observations = (1..=4097)
        .map(|revision| {
            let mut row = observation(&other.attempt, revision, /*input*/ 0)?;
            row.patch = Patch::default();
            Ok(row)
        })
        .collect::<anyhow::Result<_>>()?;
    let mut conn = connection(&runtime).await?;
    sqlx::query("BEGIN").execute(&mut conn).await?;
    for row in &other.observations {
        sqlx::query("INSERT INTO draft_accounting_observations VALUES (?, ?, ?, ?, ?)")
            .bind(other.attempt.attempt_id.to_string())
            .bind(i64::from(row.revision))
            .bind(row.source.to_string())
            .bind(i64::from(row.sequence))
            .bind(serde_json::to_string(row)?)
            .execute(&mut conn)
            .await?;
    }
    let evidence = serde_json::to_string(&other.observations)?;
    sqlx::query("INSERT INTO draft_accounting_estimates VALUES (?, ?, ?)")
        .bind(other.attempt.attempt_id.to_string())
        .bind(&evidence)
        .bind(serde_json::to_string(&other)?)
        .execute(&mut conn)
        .await?;
    sqlx::query("UPDATE draft_accounting_contributions SET evidence = ? WHERE attempt_id = ?")
        .bind(evidence)
        .bind(other.attempt.attempt_id.to_string())
        .execute(&mut conn)
        .await?;
    sqlx::query("COMMIT").execute(&mut conn).await?;
    let mut after = ready(
        AccountingStore::inspect_day(
            &runtime,
            a.thread_id,
            /*utc_day*/ 0,
            /*read_at_ms*/ 0,
        )
        .await?,
    );
    // An oversized other conversation is at most unread, never this day's failure.
    let after_others = after
        .other_conversations
        .take()
        .expect("day reads other conversations");
    assert_eq!(after_others.conversations, others.conversations);
    assert_eq!(after, view);
    assert_eq!(
        AccountingStore::inspect_day(
            &runtime,
            other.attempt.thread_id,
            /*utc_day*/ 0,
            /*read_at_ms*/ 0
        )
        .await?,
        InspectionDay::TooLarge
    );
    conn.close().await?;
    runtime.close().await;
    Ok(())
}

#[tokio::test]
async fn accounting_inspect_work_budget_refuses_unknown_busy_host() -> anyhow::Result<()> {
    let path = home();
    let runtime = open(&path).await?;
    let a = attempt(/*id*/ 1, /*time*/ 0)?;
    native(&runtime, a.thread_id).await?;
    unrelated_population(&runtime, /*count*/ 12_001).await?;
    let mut conn = connection(&runtime).await?;
    // The same 12,001 rows / 600 unrelated roots pass the existing busy-host test.
    // Missing lineage plus stale contributions requires repeated candidate scans;
    // no candidate can exhaust the selected 512-attempt or 4 MiB result budget.
    sqlx::query("UPDATE threads SET source = 'unknown' WHERE id != ?")
        .bind(a.thread_id.to_string())
        .execute(&mut conn)
        .await?;
    sqlx::query("DELETE FROM draft_accounting_contributions WHERE thread_id != ?")
        .bind(a.thread_id.to_string())
        .execute(&mut conn)
        .await?;
    // Upper bounds against unbounded scans; an unoptimized build on a loaded
    // Windows runner needed more than 30s to reach the budget.
    assert_eq!(
        tokio::time::timeout(
            std::time::Duration::from_secs(90),
            AccountingStore::inspect_day(
                &runtime,
                a.thread_id,
                /*utc_day*/ 0,
                /*read_at_ms*/ 0
            )
        )
        .await??,
        InspectionDay::TooLarge
    );
    assert_eq!(
        tokio::time::timeout(
            std::time::Duration::from_secs(90),
            AccountingStore::inspect_range(
                &runtime,
                a.thread_id,
                InspectionRange {
                    start_ms: 0,
                    end_ms: DAY,
                    grouping: InspectionGrouping::Day,
                },
                /*read_at_ms*/ 0
            )
        )
        .await??,
        InspectionDay::TooLarge
    );
    conn.close().await?;
    runtime.close().await;
    Ok(())
}

#[tokio::test]
async fn accounting_inspect_work_budget_spans_range_buckets() -> anyhow::Result<()> {
    let path = home();
    let runtime = open(&path).await?;
    let a = attempt(/*id*/ 1, /*time*/ 0)?;
    native(&runtime, a.thread_id).await?;
    unrelated_population(&runtime, /*count*/ 12_001).await?;
    let mut conn = connection(&runtime).await?;
    sqlx::query("UPDATE draft_accounting_retention_checkpoint SET completed_as_of_ms = ?")
        .bind(3 * DAY)
        .execute(&mut conn)
        .await?;
    // Each day and the ordinary three-day grouping must remain Ready.
    let mut days = Vec::new();
    for day in 0..3 {
        let mut view =
            ready(AccountingStore::inspect_day(&runtime, a.thread_id, day, 3 * DAY).await?);
        assert_eq!(view.totals.attempts, i64::from(day == 0));
        // Range buckets do not read other conversations; the day view does.
        // #1196: both classify an empty day.
        assert!(view.other_conversations.take().is_some());
        assert_eq!(view.empty_day.is_some(), day != 0);
        days.push(InspectionDay::Ready(view));
    }
    let range = AccountingStore::inspect_range(
        &runtime,
        a.thread_id,
        InspectionRange {
            start_ms: 0,
            end_ms: 3 * DAY,
            grouping: InspectionGrouping::Day,
        },
        3 * DAY,
    )
    .await?;
    let InspectionDay::Range { buckets, .. } = range else {
        panic!("{range:?}")
    };
    assert_eq!(buckets.len(), 3);
    for (bucket, expected) in buckets.into_iter().zip(days) {
        assert!(!bucket.partial);
        assert_eq!(bucket.days, vec![expected]);
    }
    // The hour range repeatedly scans the same retained rows. Its one total
    // attempt and 72 buckets fit all result limits, but exhaust the work budget.
    assert_eq!(
        tokio::time::timeout(
            std::time::Duration::from_secs(30),
            AccountingStore::inspect_range(
                &runtime,
                a.thread_id,
                InspectionRange {
                    start_ms: 0,
                    end_ms: 3 * DAY,
                    grouping: InspectionGrouping::Hour,
                },
                3 * DAY
            )
        )
        .await??,
        InspectionDay::TooLarge
    );
    conn.close().await?;
    runtime.close().await;
    Ok(())
}

#[tokio::test]
async fn accounting_inspect_whole_store_denominator_thresholds() -> anyhow::Result<()> {
    let path = home();
    let runtime = open(&path).await?;
    let store = AccountingStore::open(&runtime, /*as_of*/ 0).await?;
    // Obtain a genuine compact payload from an unrelated, expired raw attempt.
    let mut old = attempt(/*id*/ 1000, /*time*/ 0)?;
    old.thread_id = ThreadId::from_string(&Uuid::from_u128(1000).to_string())?;
    native(&runtime, old.thread_id).await?;
    store.admit(old.thread_id, &old, &[], /*as_of*/ 0).await?;
    store.maintain(92 * DAY).await?;
    let owner = attempt(/*id*/ 1, 91 * DAY)?.thread_id;
    for id in 1..=8 {
        let mut a = attempt(id, 91 * DAY)?;
        a.thread_id = ThreadId::from_string(&Uuid::from_u128(id + 6).to_string())?;
        native(&runtime, a.thread_id).await?;
        store.admit(a.thread_id, &a, &[], 92 * DAY).await?;
        if id > 1 {
            runtime
                .upsert_thread_spawn_edge(
                    owner,
                    a.thread_id,
                    codex_state::DirectionalThreadSpawnEdgeStatus::Closed,
                )
                .await?;
        }
    }
    let mut conn = connection(&runtime).await?;
    let payload: String = sqlx::query_scalar("SELECT payload FROM draft_accounting_compact_days")
        .fetch_one(&mut conn)
        .await?;
    let mut previous = 9i64;
    // Pin adjacent passing/refusing totals, including the eight current attempts.
    // Seven children fail first; even an empty current day eventually refuses.
    for (count, day, expected_attempts) in [
        (95_236i64, 91, Some(8)),
        (95_237, 91, None),
        // FUP-02's start-time scan for an empty day runs only with budget to
        // spare, so it moves neither threshold.
        (571_428, 92, Some(0)),
        (571_429, 92, None),
    ] {
        sqlx::query(
            "WITH RECURSIVE n(i) AS (VALUES(?1) UNION ALL SELECT i+1 FROM n WHERE i<?2)
             INSERT INTO draft_accounting_compact_days
             SELECT printf('00000000-0000-0000-0001-%012x',i),0,?3 FROM n",
        )
        .bind(previous + 1)
        .bind(count)
        .bind(&payload)
        .execute(&mut conn)
        .await?;
        assert_eq!(
            sqlx::query_scalar::<_, i64>(
                "SELECT (SELECT count(*) FROM draft_accounting_attempts) +
                 (SELECT count(*) FROM draft_accounting_compact_days)"
            )
            .fetch_one(&mut conn)
            .await?,
            count
        );
        let result = AccountingStore::inspect_day(&runtime, owner, day, 92 * DAY).await?;
        if let Some(attempts) = expected_attempts {
            let view = ready(result);
            assert_eq!(
                (view.totals.attempts, view.descendant_totals.attempts),
                (attempts, if day == 91 { 7 } else { 0 })
            );
        } else {
            assert_eq!(result, InspectionDay::TooLarge, "whole-store rows: {count}");
        }
        previous = count;
    }
    conn.close().await?;
    runtime.close().await;
    Ok(())
}

#[tokio::test]
async fn accounting_inspect_valid_ten_thousand_retained_rows() -> anyhow::Result<()> {
    let path = home();
    let runtime = open(&path).await?;
    let a = attempt(/*id*/ 1, /*time*/ 0)?;
    native(&runtime, a.thread_id).await?;
    unrelated_population(&runtime, /*count*/ 10_000).await?;
    let mut conn = connection(&runtime).await?;
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM draft_accounting_attempts")
            .fetch_one(&mut conn)
            .await?,
        10_000
    );
    let view = ready(
        AccountingStore::inspect_day(
            &runtime,
            a.thread_id,
            /*utc_day*/ 0,
            /*read_at_ms*/ 0,
        )
        .await?,
    );
    assert_eq!(
        (
            view.totals.attempts,
            view.requests.len(),
            view.unknown_parent_totals.attempts
        ),
        (1, 1, 0)
    );
    conn.close().await?;
    runtime.close().await;
    Ok(())
}

#[tokio::test]
async fn accounting_inspect_maximal_valid_packet() -> anyhow::Result<()> {
    const CEILING: usize = 4 * 1024 * 1024;
    let path = home();
    let runtime = open(&path).await?;
    let a = attempt(/*id*/ 1, /*time*/ 0)?;
    native(&runtime, a.thread_id).await?;
    let store = AccountingStore::open(&runtime, /*as_of*/ 0).await?;
    store.admit(a.thread_id, &a, &[], /*as_of*/ 0).await?;
    let template = store
        .observe(
            a.thread_id,
            &a,
            &[observation(&a, /*revision*/ 1, /*input*/ 1)?],
            /*as_of*/ 0,
        )
        .await?;
    let mut quotes: Vec<ObservationQuote> = Vec::new();
    let mut packet = 8192;
    loop {
        let mut q = template.clone();
        q.attempt = attempt(quotes.len() as u128 + 1, /*time*/ 0)?;
        q.observations.clear();
        let mut size = serde_json::to_vec(&q)?.len() + 2048;
        for revision in 1..=4096 {
            let row = observation(&q.attempt, revision, /*input*/ 1)?;
            let extra = serde_json::to_vec(&row)?.len() + usize::from(revision > 1);
            if packet + size + extra > CEILING - 1 {
                break;
            }
            size += extra;
            q.observations.push(row);
        }
        if q.observations.is_empty() {
            // The remainder no longer fits another quote skeleton plus evidence.
            // Spend it on identity metadata already inside the packet so the
            // packet still lands exactly on the ceiling.
            let mut remaining = CEILING - 1 - packet;
            for packed in &mut quotes {
                for text in [&mut packed.attempt.model, &mut packed.attempt.provider] {
                    let padding = remaining.min(128 - text.len());
                    text.push_str(&"x".repeat(padding));
                    remaining -= padding;
                    packet += padding;
                }
                if remaining == 0 {
                    break;
                }
            }
            assert_eq!(remaining, 0);
            break;
        }
        packet += size;
        if q.observations.len() < 4096 {
            // Fill the final sub-observation remainder with legal identity metadata.
            let mut remaining = CEILING - 1 - packet;
            for text in [&mut q.attempt.model, &mut q.attempt.provider] {
                let padding = remaining.min(128 - text.len());
                text.push_str(&"x".repeat(padding));
                remaining -= padding;
                packet += padding;
            }
            assert_eq!(remaining, 0);
            quotes.push(q);
            break;
        }
        quotes.push(q);
    }
    assert_eq!(packet, CEILING - 1);
    assert_eq!(
        8192 + quotes
            .iter()
            .map(|q| serde_json::to_vec(q).unwrap().len() + 2048)
            .sum::<usize>(),
        packet
    );
    let mut conn = connection(&runtime).await?;
    sqlx::query("BEGIN").execute(&mut conn).await?;
    sqlx::raw_sql(
        "DELETE FROM draft_accounting_contributions; DELETE FROM draft_accounting_estimates;
        DELETE FROM draft_accounting_observations; DELETE FROM draft_accounting_price_bindings;
        DELETE FROM draft_accounting_attempts",
    )
    .execute(&mut conn)
    .await?;
    for q in &quotes {
        let a = &q.attempt;
        let evidence = serde_json::to_string(&q.observations)?;
        sqlx::query("INSERT INTO draft_accounting_attempts VALUES (?, ?, ?)")
            .bind(a.attempt_id.to_string())
            .bind(a.request_id.to_string())
            .bind(serde_json::to_string(a)?)
            .execute(&mut conn)
            .await?;
        sqlx::query("INSERT INTO draft_accounting_price_bindings VALUES (?, NULL)")
            .bind(a.attempt_id.to_string())
            .execute(&mut conn)
            .await?;
        for row in &q.observations {
            sqlx::query("INSERT INTO draft_accounting_observations VALUES (?, ?, ?, ?, ?)")
                .bind(a.attempt_id.to_string())
                .bind(i64::from(row.revision))
                .bind(row.source.to_string())
                .bind(i64::from(row.sequence))
                .bind(serde_json::to_string(row)?)
                .execute(&mut conn)
                .await?;
        }
        sqlx::query("INSERT INTO draft_accounting_estimates VALUES (?, ?, ?)")
            .bind(a.attempt_id.to_string())
            .bind(&evidence)
            .bind(serde_json::to_string(q)?)
            .execute(&mut conn)
            .await?;
        sqlx::query("INSERT INTO draft_accounting_contributions VALUES (?, ?, 0, ?)")
            .bind(a.attempt_id.to_string())
            .bind(a.thread_id.to_string())
            .bind(evidence)
            .execute(&mut conn)
            .await?;
    }
    // Valid historical versions together exceed 4 MiB but do not enlarge the packet.
    let mut history_bytes = 0;
    for omitted in 1..=16 {
        let mut old = quotes[0].clone();
        old.observations.truncate(old.observations.len() - omitted);
        let evidence = serde_json::to_string(&old.observations)?;
        history_bytes += evidence.len();
        sqlx::query("INSERT INTO draft_accounting_estimates VALUES (?, ?, ?)")
            .bind(old.attempt.attempt_id.to_string())
            .bind(evidence)
            .bind(serde_json::to_string(&old)?)
            .execute(&mut conn)
            .await?;
    }
    assert!(history_bytes > CEILING);
    sqlx::query("COMMIT").execute(&mut conn).await?;
    let view = ready(
        AccountingStore::inspect_day(
            &runtime,
            a.thread_id,
            /*utc_day*/ 0,
            /*read_at_ms*/ 0,
        )
        .await?,
    );
    assert_eq!(view.totals, DayTotals::from_quotes(&quotes)?);
    assert_eq!(
        view.requests
            .values()
            .flatten()
            .cloned()
            .collect::<Vec<_>>(),
        quotes
    );
    // Two more legal bytes cross the same projected packet ceiling.
    let q = quotes.last_mut().unwrap();
    q.attempt.turn.push_str("xx");
    sqlx::query("UPDATE draft_accounting_attempts SET payload = ? WHERE attempt_id = ?")
        .bind(serde_json::to_string(&q.attempt)?)
        .bind(q.attempt.attempt_id.to_string())
        .execute(&mut conn)
        .await?;
    sqlx::query("UPDATE draft_accounting_estimates SET payload = ? WHERE attempt_id = ?")
        .bind(serde_json::to_string(q)?)
        .bind(q.attempt.attempt_id.to_string())
        .execute(&mut conn)
        .await?;
    assert_eq!(
        AccountingStore::inspect_day(
            &runtime,
            a.thread_id,
            /*utc_day*/ 0,
            /*read_at_ms*/ 0
        )
        .await?,
        InspectionDay::TooLarge
    );
    conn.close().await?;
    runtime.close().await;
    Ok(())
}

#[tokio::test]
async fn accounting_inspect_public_limits_no_truncation() -> anyhow::Result<()> {
    let path = home();
    let runtime = open(&path).await?;
    let a = attempt(/*id*/ 1, /*time*/ 0)?;
    native(&runtime, a.thread_id).await?;
    let store = AccountingStore::open(&runtime, /*as_of*/ 0).await?;
    store.admit(a.thread_id, &a, &[], /*as_of*/ 0).await?;
    // Clone legitimately admitted, all-unknown intent and its original NULL evidence.
    let mut conn = connection(&runtime).await?;
    let quote = ready(
        AccountingStore::inspect_day(
            &runtime,
            a.thread_id,
            /*utc_day*/ 0,
            /*read_at_ms*/ 0,
        )
        .await?,
    )
    .requests[&a.request_id][0]
        .clone();
    for id in 2..=513 {
        let a = attempt(id, /*time*/ 0)?;
        let mut q = quote.clone();
        q.attempt = a.clone();
        sqlx::query("INSERT INTO draft_accounting_attempts VALUES (?, ?, ?)")
            .bind(a.attempt_id.to_string())
            .bind(a.request_id.to_string())
            .bind(serde_json::to_string(&a)?)
            .execute(&mut conn)
            .await?;
        sqlx::query("INSERT INTO draft_accounting_price_bindings VALUES (?, NULL)")
            .bind(a.attempt_id.to_string())
            .execute(&mut conn)
            .await?;
        sqlx::query("INSERT INTO draft_accounting_estimates VALUES (?, '[]', ?)")
            .bind(a.attempt_id.to_string())
            .bind(serde_json::to_string(&q)?)
            .execute(&mut conn)
            .await?;
        sqlx::query("INSERT INTO draft_accounting_contributions VALUES (?, ?, 0, '[]')")
            .bind(a.attempt_id.to_string())
            .bind(a.thread_id.to_string())
            .execute(&mut conn)
            .await?;
        if id == 512 {
            assert_eq!(
                ready(
                    AccountingStore::inspect_day(
                        &runtime,
                        a.thread_id,
                        /*utc_day*/ 0,
                        /*read_at_ms*/ 0
                    )
                    .await?
                )
                .totals
                .attempts,
                512
            );
        }
    }
    assert_eq!(
        AccountingStore::inspect_day(
            &runtime,
            a.thread_id,
            /*utc_day*/ 0,
            /*read_at_ms*/ 0
        )
        .await?,
        InspectionDay::TooLarge
    );
    conn.close().await?;
    runtime.close().await;
    inspection_limit_edges().await?;
    Ok(())
}

async fn inspection_limit_edges() -> anyhow::Result<()> {
    let path = home();
    let runtime = open(&path).await?;
    let a = attempt(/*id*/ 1, /*time*/ 0)?;
    native(&runtime, a.thread_id).await?;
    let store = AccountingStore::open(&runtime, /*as_of*/ 0).await?;
    store.admit(a.thread_id, &a, &[], /*as_of*/ 0).await?;
    let observations: Vec<_> = (1..=4096)
        .map(|r| observation(&a, r, r))
        .collect::<anyhow::Result<_>>()?;
    let mut conn = connection(&runtime).await?;
    for batch in observations.chunks(256) {
        store.observe(a.thread_id, &a, batch, /*as_of*/ 0).await?;
        // This fixture retains one estimate version to isolate the observation cap.
        sqlx::query("DELETE FROM draft_accounting_estimates WHERE evidence NOT IN (SELECT evidence FROM draft_accounting_contributions)")
            .execute(&mut conn).await?;
    }
    conn.close().await?;
    assert_eq!(
        ready(
            AccountingStore::inspect_day(
                &runtime,
                a.thread_id,
                /*utc_day*/ 0,
                /*read_at_ms*/ 0
            )
            .await?
        )
        .requests[&a.request_id][0]
            .observations
            .len(),
        4096
    );
    store
        .observe(
            a.thread_id,
            &a,
            &[observation(
                &a, /*revision*/ 4097, /*input*/ 4097,
            )?],
            /*as_of*/ 0,
        )
        .await?;
    assert_eq!(
        AccountingStore::inspect_day(
            &runtime,
            a.thread_id,
            /*utc_day*/ 0,
            /*read_at_ms*/ 0
        )
        .await?,
        InspectionDay::TooLarge
    );
    runtime.close().await;
    let path = home();
    let runtime = open(&path).await?;
    native(&runtime, a.thread_id).await?;
    AccountingStore::open(&runtime, /*as_of*/ 0).await?;
    let mut conn = connection(&runtime).await?;
    // Deliberately invalid small rows prove the cap precedes materialization.
    sqlx::raw_sql("WITH RECURSIVE n(x) AS (VALUES(1) UNION ALL SELECT x+1 FROM n WHERE x<10000)
        INSERT INTO draft_accounting_attempts SELECT CAST(x AS TEXT), CAST(x AS TEXT), 'null' FROM n")
        .execute(&mut conn).await?;
    assert!(
        AccountingStore::inspect_day(
            &runtime,
            a.thread_id,
            /*utc_day*/ 0,
            /*read_at_ms*/ 0
        )
        .await
        .is_err()
    );
    sqlx::raw_sql("INSERT INTO draft_accounting_attempts VALUES ('extra','extra','null')")
        .execute(&mut conn)
        .await?;
    // Malformed ownership remains an error at any unrelated population size.
    assert!(
        AccountingStore::inspect_day(
            &runtime,
            a.thread_id,
            /*utc_day*/ 0,
            /*read_at_ms*/ 0
        )
        .await
        .is_err()
    );
    sqlx::raw_sql("DELETE FROM draft_accounting_attempts")
        .execute(&mut conn)
        .await?;
    for size in [4 * 1024 * 1024, 4 * 1024 * 1024 + 1] {
        let payload = format!("null{}", " ".repeat(size - 4));
        sqlx::query("INSERT OR REPLACE INTO draft_accounting_attempts VALUES ('bad','bad',?)")
            .bind(payload)
            .execute(&mut conn)
            .await?;
        let result = AccountingStore::inspect_day(
            &runtime,
            a.thread_id,
            /*utc_day*/ 0,
            /*read_at_ms*/ 0,
        )
        .await;
        assert!(result.is_err());
    }
    conn.close().await?;
    runtime.close().await;
    Ok(())
}

fn home() -> impl std::ops::Deref<Target = std::path::PathBuf> {
    scopeguard::guard(
        std::env::temp_dir().join(format!("accounting-store-{}", Uuid::new_v4())),
        |path| {
            let _ = std::fs::remove_dir_all(path);
        },
    )
}

async fn open(path: &Path) -> anyhow::Result<Arc<StateRuntime>> {
    StateRuntime::init(
        SqliteConfig::from_sqlite_home(AbsolutePathBuf::try_from(path.to_path_buf())?),
        "synthetic".into(),
    )
    .await
}

async fn connection(runtime: &StateRuntime) -> anyhow::Result<SqliteConnection> {
    let pool = runtime
        .sqlite()
        .open_read_write_pool(&runtime.sqlite().state_db_path())
        .await?;
    // Detach ownership before closing the pool; callers explicitly close the connection.
    let mut conn = pool.acquire().await?.detach();
    pool.close().await;
    assert_eq!(
        sqlx::query_scalar::<_, i64>("PRAGMA foreign_keys")
            .fetch_one(&mut conn)
            .await?,
        1
    );
    Ok(conn)
}

fn attempt(id: u128, time: i64) -> anyhow::Result<Attempt> {
    Ok(Attempt {
        attempt_id: Uuid::from_u128(id),
        request_id: Uuid::from_u128(id + 100),
        thread_id: ThreadId::from_string(&Uuid::from_u128(7).to_string())?,
        turn: "fixture".into(),
        retry_of: None,
        provider: "synthetic".into(),
        model: "fixture".into(),
        scope: Uuid::nil(),
        dialect: Dialect::NativeAnthropic,
        dispatched_at_ms: time.try_into()?,
        account: None,
    })
}

fn snapshot() -> anyhow::Result<Snapshot> {
    serde_json::from_value(json!({
        "id":Uuid::from_u128(1000), "provider":"synthetic", "model":"fixture",
        "scope":Uuid::nil(), "currency":"USD", "unit":"PerMillionTokens",
        "rates":{"noncached":"0.000000000000000001","read":null,"write":null,"output":null},
        "source_reference":Uuid::from_u128(1001), "source_kind":"ProviderPublished",
        "observed_at_ms":0,"approved_at_ms":0,"effective_from_ms":0,"effective_end_ms":null
    }))
    .context("synthetic original-price snapshot")
}

fn observation(a: &Attempt, revision: i64, input: i64) -> anyhow::Result<Observation> {
    Ok(Observation {
        revision: revision.try_into()?,
        source: a.attempt_id,
        sequence: revision.try_into()?,
        patch: Patch {
            input: Presence::Number(input.try_into()?),
            ..Patch::default()
        },
    })
}

async fn native(runtime: &StateRuntime, owner: ThreadId) -> anyhow::Result<()> {
    let path = runtime.sqlite().home();
    let mut builder = ThreadMetadataBuilder::new(
        owner,
        path.join("synthetic.jsonl"),
        chrono::DateTime::from_timestamp(1_700_000_000, 0).context("synthetic native timestamp")?,
        SessionSource::Cli,
    );
    builder.cwd = path.to_path_buf();
    runtime.upsert_thread(&builder.build("synthetic")).await
}

async fn ordinary(conn: &mut SqliteConnection) -> anyhow::Result<Vec<(i64, Vec<u8>)>> {
    Ok(
        sqlx::query_as("SELECT version, checksum FROM _sqlx_migrations ORDER BY version")
            .fetch_all(conn)
            .await?,
    )
}

#[tokio::test]
async fn normal_default_profiles_do_not_install_and_opt_in_keeps_ordinary_history()
-> anyhow::Result<()> {
    let path = home();
    let a = attempt(/*id*/ 1, /*time*/ 0)?;
    let runtime = open(&path).await?;
    let mut conn = connection(&runtime).await?;
    let history = ordinary(&mut conn).await?;
    for populated in [false, true] {
        if populated {
            native(&runtime, a.thread_id).await?;
        }
        assert_eq!(sqlx::query_scalar::<_, i64>(
            "SELECT count(*) FROM sqlite_schema WHERE name GLOB 'draft_accounting_*' OR name = '_accounting_migrations'",
        ).fetch_one(&mut conn).await?, 0);
    }
    let native_before = runtime.get_thread(a.thread_id).await?;
    let store = AccountingStore::open(&runtime, /*as_of*/ 0).await?;
    assert_eq!(runtime.get_thread(a.thread_id).await?, native_before);
    assert_eq!(ordinary(&mut conn).await?, history);
    assert_eq!(
        sqlx::query_scalar::<_, i64>(
            "SELECT count(*) FROM sqlite_schema WHERE name GLOB 'draft_accounting_*'",
        )
        .fetch_one(&mut conn)
        .await?,
        12
    );
    // A new ledger starts in format 1; it moves to format 2 only when a
    // record needs it.
    assert_eq!(
        sqlx::query_as::<_, (i64, bool)>("SELECT version, success FROM _accounting_migrations",)
            .fetch_all(&mut conn)
            .await?,
        vec![(1, true)]
    );
    assert_eq!(
        sqlx::query_scalar::<_, String>("PRAGMA integrity_check")
            .fetch_one(&mut conn)
            .await?,
        "ok"
    );
    let intent = store
        .admit(a.thread_id, &a, &[snapshot()?], /*as_of*/ 0)
        .await?;
    assert_eq!(intent.usage, Usage::default());
    assert_eq!(intent.all_buckets_priced, None);
    assert_eq!(intent.snapshot, Some(snapshot()?));
    let totals = store
        .read_day(a.thread_id, /*utc_day*/ 0, /*as_of_ms*/ 0)
        .await?;
    assert_eq!(
        totals,
        RetainedDay::Available {
            coverage: RetentionCoverage {
                completed_as_of_ms: 0,
                detail_expired_through_ms: None,
                aggregate_day_floor: 0,
                oldest_recorded_day: Some(0),
            },
            totals: Current::Ready(DayTotals {
                measured: std::array::from_fn(|_| Metric {
                    known: 0,
                    unknown: 1
                }),
                known_usd: Decimal::default(),
                unknown_estimates: 1,
                attempts: 1,
                ..Default::default()
            }),
        }
    );
    conn.close().await?;
    runtime.close().await;
    for _ in 0..2 {
        let reopened = open(&path).await?;
        let store = AccountingStore::open(&reopened, /*as_of*/ 0).await?;
        assert_eq!(
            store
                .read_day(a.thread_id, /*utc_day*/ 0, /*as_of_ms*/ 0)
                .await?,
            totals
        );
        let mut conn = connection(&reopened).await?;
        assert_eq!(ordinary(&mut conn).await?, history);
        conn.close().await?;
        reopened.close().await;
    }
    Ok(())
}

#[tokio::test]
async fn original_price_null_unknown_zero_and_reordered_replay_survive_restart()
-> anyhow::Result<()> {
    let path = home();
    let runtime = open(&path).await?;
    let a = attempt(/*id*/ 1, /*time*/ 0)?;
    let missing = attempt(/*id*/ 2, /*time*/ 0)?;
    native(&runtime, a.thread_id).await?;
    let store = AccountingStore::open(&runtime, /*as_of*/ 0).await?;
    store
        .admit(a.thread_id, &a, &[snapshot()?], /*as_of*/ 0)
        .await?;
    store
        .admit(missing.thread_id, &missing, &[], /*as_of*/ 0)
        .await?;
    let patch = observation(&a, /*revision*/ 2, /*input*/ 1)?;
    let quote = store
        .observe(
            a.thread_id,
            &a,
            std::slice::from_ref(&patch),
            /*as_of*/ 0,
        )
        .await?;
    assert_eq!(
        serde_json::to_value(quote.known_subtotal)?,
        json!("0.000000000000000000000001")
    );
    assert_eq!(
        quote.usage,
        Usage {
            noncached: Some(1),
            ..Usage::default()
        }
    );
    assert_eq!(quote.all_buckets_priced, None);
    store
        .observe(
            a.thread_id,
            &a,
            &[observation(&a, /*revision*/ 1, /*input*/ 0)?],
            /*as_of*/ 0,
        )
        .await?;
    let expected = store
        .observe(a.thread_id, &a, &[patch], /*as_of*/ 0)
        .await?;
    let null = store
        .admit(
            missing.thread_id,
            &missing,
            &[snapshot()?],
            /*as_of*/ 0,
        )
        .await?;
    assert_eq!(null.snapshot, None);
    let mut zero = observation(&missing, /*revision*/ 1, /*input*/ 0)?;
    zero.patch = serde_json::from_value(
        json!({"input":0,"read":0,"write":0,"output":0,"reasoning":0,"total":0}),
    )?;
    let zero_quote = store
        .observe(
            missing.thread_id,
            &missing,
            &[zero.clone()],
            /*as_of*/ 0,
        )
        .await?;
    // No price was bound, so even an all-zero attempt makes no zero-cost claim
    // (see 20034d7d2: an absent rate is not a zero cost).
    assert_eq!(zero_quote.all_buckets_priced, None);
    assert_eq!(zero_quote.snapshot, None);
    let expected_day = store
        .read_day(a.thread_id, /*utc_day*/ 0, /*as_of_ms*/ 0)
        .await?;
    runtime.close().await;
    for _ in 0..2 {
        let runtime = open(&path).await?;
        let store = AccountingStore::open(&runtime, /*as_of*/ 0).await?;
        let mut later = snapshot()?;
        later.id = Uuid::from_u128(2000);
        later.rates.noncached = Some("99".to_owned().try_into()?);
        assert_eq!(
            store
                .admit(a.thread_id, &a, &[later.clone()], /*as_of*/ 0)
                .await?,
            expected
        );
        assert_eq!(
            store
                .admit(missing.thread_id, &missing, &[later], /*as_of*/ 0)
                .await?,
            zero_quote
        );
        assert_eq!(
            store
                .observe(
                    missing.thread_id,
                    &missing,
                    &[zero.clone()],
                    /*as_of*/ 0
                )
                .await?,
            zero_quote
        );
        assert_eq!(
            store
                .read_day(a.thread_id, /*utc_day*/ 0, /*as_of_ms*/ 0)
                .await?,
            expected_day
        );
        runtime.close().await;
    }
    Ok(())
}

#[tokio::test]
async fn installed_deletion_without_store_handle_removes_raw_and_compact_in_normal_library()
-> anyhow::Result<()> {
    let path = home();
    let runtime = open(&path).await?;
    let now = chrono::Utc::now().timestamp_millis();
    let a = attempt(/*id*/ 1, now - 90 * DAY)?;
    let mut other = attempt(/*id*/ 2, now - 1)?;
    other.thread_id = ThreadId::new();
    for owner in [a.thread_id, other.thread_id] {
        native(&runtime, owner).await?;
    }
    {
        let store = AccountingStore::open(&runtime, i64::from(a.dispatched_at_ms)).await?;
        store
            .admit(
                a.thread_id,
                &a,
                &[snapshot()?],
                i64::from(a.dispatched_at_ms),
            )
            .await?;
        store
            .observe(
                a.thread_id,
                &a,
                &[observation(&a, /*revision*/ 1, /*input*/ 1)?],
                i64::from(a.dispatched_at_ms),
            )
            .await?;
        store.maintain(now - 1).await?;
        store
            .admit(other.thread_id, &other, &[snapshot()?], now - 1)
            .await?;
    }
    runtime.close().await;
    let runtime = open(&path).await?; // Collection OFF: never open an accounting handle here.
    assert_eq!(runtime.delete_thread(a.thread_id).await?, 1);
    assert_eq!(runtime.delete_threads_strict(&[other.thread_id]).await?, 1);
    assert_eq!(runtime.delete_thread(a.thread_id).await?, 0);
    assert_eq!(runtime.delete_threads_strict(&[]).await?, 0);
    runtime.close().await;
    for _ in 0..2 {
        let runtime = open(&path).await?;
        let mut conn = connection(&runtime).await?;
        for table in [
            "draft_accounting_attempts",
            "draft_accounting_price_snapshots",
            "draft_accounting_price_bindings",
            "draft_accounting_estimates",
            "draft_accounting_contributions",
            "draft_accounting_observations",
            "draft_accounting_compact_days",
            "draft_accounting_compact_snapshots",
        ] {
            assert_eq!(
                sqlx::query_scalar::<_, i64>(sqlx::AssertSqlSafe(format!(
                    "SELECT count(*) FROM {table}"
                )))
                .fetch_one(&mut conn)
                .await?,
                0
            );
        }
        assert_eq!(
            sqlx::query_scalar::<_, i64>("SELECT count(*) FROM draft_accounting_tombstones")
                .fetch_one(&mut conn)
                .await?,
            2
        );
        assert!(runtime.get_thread(a.thread_id).await?.is_none());
        assert!(runtime.get_thread(other.thread_id).await?.is_none());
        conn.close().await?;
        runtime.close().await;
    }
    Ok(())
}

#[tokio::test]
async fn normal_open_rejects_unversioned_partial_failed_checksum_and_newer_without_repair()
-> anyhow::Result<()> {
    for mutation in [
        "DROP TABLE draft_accounting_price_bindings",
        "DROP INDEX draft_accounting_request",
        "UPDATE _accounting_migrations SET success = 0",
        "UPDATE _accounting_migrations SET checksum = X'00'",
        "UPDATE _accounting_migrations SET version = version + 10",
        // Claims format 1 while holding format 2's table.
        "CREATE TABLE draft_accounting_ledger_format (version INTEGER PRIMARY KEY NOT NULL CHECK(version = 2))",
        "DELETE FROM _accounting_migrations",
        "DROP TABLE _accounting_migrations",
    ] {
        let path = home();
        let runtime = open(&path).await?;
        AccountingStore::open(&runtime, /*as_of*/ 0).await?;
        let mut conn = connection(&runtime).await?;
        sqlx::raw_sql(mutation).execute(&mut conn).await?;
        let before: Vec<(String, Option<String>)> =
            sqlx::query_as("SELECT name, sql FROM sqlite_schema ORDER BY name")
                .fetch_all(&mut conn)
                .await?;
        let history = ordinary(&mut conn).await?;
        for _ in 0..2 {
            assert!(
                AccountingStore::open(&runtime, /*as_of*/ 1).await.is_err(),
                "{mutation}"
            );
            assert_eq!(
                sqlx::query_as::<_, (String, Option<String>)>(
                    "SELECT name, sql FROM sqlite_schema ORDER BY name",
                )
                .fetch_all(&mut conn)
                .await?,
                before
            );
            assert_eq!(ordinary(&mut conn).await?, history);
        }
        conn.close().await?;
        runtime.close().await;
        for _ in 0..2 {
            let runtime = open(&path).await?; // Ordinary migrator ignores independent ledger.
            assert!(
                AccountingStore::open(&runtime, /*as_of*/ 1).await.is_err(),
                "{mutation}"
            );
            runtime.close().await;
        }
    }
    Ok(())
}

/// A ledger written before price records gained a basis (`5bae03414e^`) still
/// validates and reads on this build: every older payload is read under the
/// format it was written in.
#[tokio::test]
async fn ledger_written_before_plan_basis_validates_and_reads() -> anyhow::Result<()> {
    const T0: i64 = 1_789_473_600_000; // 2026-09-15T12:00:00Z, the fixture's day.
    let path = home();
    let runtime = open(&path).await?;
    let owner = ThreadId::from_string(&Uuid::from_u128(7).to_string())?;
    native(&runtime, owner).await?;
    let mut conn = connection(&runtime).await?;
    sqlx::raw_sql(sqlx::AssertSqlSafe(include_str!(
        "fixtures/accounting/pre-5bae-ledger.sql"
    )))
    .execute(&mut conn)
    .await?;
    conn.close().await?;
    let read_at = T0 + 60_000;
    let view = ready(AccountingStore::inspect_day(&runtime, owner, T0 / DAY, read_at).await?);
    assert_eq!(
        (
            view.totals.attempts,
            view.totals.unknown_estimates,
            view.totals.plan_attempts
        ),
        (3, 2, 0)
    );
    // 1,200 / 300 / 100 / 450 tokens at $5 / $0.50 / $6.25 / $25 per million,
    // plus the incomplete attempt's 10 input and 20 output tokens.
    assert_eq!(view.totals.known_usd.display().text, "0.018575");
    // Opening for a write validates the whole ledger and leaves its format
    // alone, so an older build sharing it keeps working.
    let store = AccountingStore::open(&runtime, read_at).await?;
    let formats = |runtime: Arc<StateRuntime>| async move {
        let mut conn = connection(&runtime).await?;
        let format: Vec<i64> =
            sqlx::query_scalar("SELECT version FROM _accounting_migrations ORDER BY version")
                .fetch_all(&mut conn)
                .await?;
        conn.close().await?;
        anyhow::Ok(format)
    };
    assert_eq!(formats(runtime.clone()).await?, vec![1]);
    // A new usage report on the old incomplete attempt mixes a legacy price
    // record and legacy estimates with a current-form estimate.
    let mut b = attempt(/*id*/ 2, T0 + 3_000)?;
    b.thread_id = owner;
    b.turn = "pre-5bae-fixture".into();
    b.provider = "anthropic".into();
    b.model = "claude-fixture".into();
    b.scope = Uuid::from_u128(9);
    let patch = Patch {
        read: Presence::Number(0.try_into()?),
        write: Presence::Number(0.try_into()?),
        ..Patch::default()
    };
    store
        .observe(
            owner,
            &b,
            &[Observation {
                revision: 3.try_into()?,
                source: b.attempt_id,
                sequence: 3.try_into()?,
                patch,
            }],
            read_at,
        )
        .await?;
    assert_eq!(formats(runtime.clone()).await?, vec![1]);
    // A record format 1 cannot express upgrades the ledger as it is written.
    let mut local = attempt(/*id*/ 10, read_at)?;
    local.thread_id = owner;
    let mut basis_only = snapshot()?;
    basis_only.id = Uuid::from_u128(2000);
    basis_only.rates = Rates::default();
    basis_only.basis = Basis::Undeclared;
    basis_only.effective_from_ms = read_at.try_into()?;
    basis_only.observed_at_ms = read_at.try_into()?;
    basis_only.approved_at_ms = read_at.try_into()?;
    store.admit(owner, &local, &[basis_only], read_at).await?;
    assert_eq!(formats(runtime.clone()).await?, vec![1, 2]);
    let after = ready(AccountingStore::inspect_day(&runtime, owner, T0 / DAY, read_at).await?);
    assert_eq!(
        (
            after.totals.attempts,
            after.totals.unknown_estimates,
            after.totals.undeclared_attempts
        ),
        (4, 2, 1)
    );
    // The completed attempt now prices 10 input and 20 output tokens.
    assert_eq!(after.totals.known_usd.display().text, "0.018575");
    // Long after, retention compacts the mixed day and it reads the same.
    let later = read_at + 100 * DAY;
    let compacted = AccountingStore::open(&runtime, later)
        .await?
        .read_day(owner, T0 / DAY, later)
        .await?;
    let RetainedDay::Available { totals, .. } = compacted else {
        panic!("{compacted:?}");
    };
    assert_eq!(totals, Current::Ready(after.totals));
    // A long-context price tier needs format 3: the format-2 ledger is upgraded
    // as the record is written, and the record reads back with its tier.
    let mut tiered = attempt(/*id*/ 11, later)?;
    tiered.thread_id = owner;
    let mut price = snapshot()?;
    price.id = Uuid::from_u128(2001);
    price.effective_from_ms = later.try_into()?;
    price.observed_at_ms = later.try_into()?;
    price.approved_at_ms = later.try_into()?;
    price.long_context = Some(LongContext {
        above_input_tokens: 272_000.try_into()?,
        rates: Rates {
            noncached: Some("10".to_string().try_into()?),
            ..Rates::default()
        },
    });
    AccountingStore::open(&runtime, later)
        .await?
        .admit(owner, &tiered, std::slice::from_ref(&price), later)
        .await?;
    assert_eq!(formats(runtime.clone()).await?, vec![1, 2, 3]);
    let mut conn = connection(&runtime).await?;
    let stored: String = sqlx::query_scalar(
        "SELECT payload FROM draft_accounting_price_snapshots WHERE snapshot_id = ?",
    )
    .bind(price.id.to_string())
    .fetch_one(&mut conn)
    .await?;
    conn.close().await?;
    assert_eq!(serde_json::from_str::<Snapshot>(&stored)?, price);
    // PF-84-S05: an attempt sent on a named account needs format 4. The ledger
    // is upgraded as it is written, the attempt reads back with its account,
    // and every earlier attempt still reads as the default account with its
    // bytes unchanged.
    let mut conn = connection(&runtime).await?;
    let before: Vec<String> =
        sqlx::query_scalar("SELECT payload FROM draft_accounting_attempts ORDER BY attempt_id")
            .fetch_all(&mut conn)
            .await?;
    conn.close().await?;
    let named_at = later + 1_000;
    let mut named = attempt(/*id*/ 12, named_at)?;
    named.thread_id = owner;
    named.account = Some(AttemptAccount {
        name: "work".into(),
        fingerprint: Some("0123456789ab".into()),
    });
    AccountingStore::open(&runtime, named_at)
        .await?
        .admit(owner, &named, std::slice::from_ref(&price), named_at)
        .await?;
    assert_eq!(formats(runtime.clone()).await?, vec![1, 2, 3, 4]);
    let mut conn = connection(&runtime).await?;
    let after: Vec<String> =
        sqlx::query_scalar("SELECT payload FROM draft_accounting_attempts ORDER BY attempt_id")
            .fetch_all(&mut conn)
            .await?;
    conn.close().await?;
    assert_eq!(after.len(), before.len() + 1);
    assert!(after.iter().take(before.len()).eq(before.iter()));
    let view =
        ready(AccountingStore::inspect_day(&runtime, owner, named_at / DAY, named_at).await?);
    let accounts: Vec<(u128, &str)> = view
        .requests
        .values()
        .flatten()
        .map(|quote| {
            (
                quote.attempt.attempt_id.as_u128(),
                quote.attempt.account_name(),
            )
        })
        .collect();
    assert_eq!(accounts, vec![(11, DEFAULT_ACCOUNT), (12, "work")]);
    runtime.close().await;
    Ok(())
}

/// PF-84-S05: an account record is validated like the rest of the identity.
#[test]
fn attempt_account_is_validated() -> anyhow::Result<()> {
    let with = |name: &str, fingerprint: Option<&str>| -> anyhow::Result<bool> {
        let mut attempt = attempt(/*id*/ 1, /*time*/ 1_789_473_600_000)?;
        attempt.account = Some(AttemptAccount {
            name: name.into(),
            fingerprint: fingerprint.map(str::to_string),
        });
        Ok(attempt.validate().is_ok())
    };
    assert!(with("work", Some("0123456789ab"))?);
    assert!(with("work", None)?);
    // `default` is the absence of an account, never a second spelling of it.
    assert!(!with(DEFAULT_ACCOUNT, None)?);
    assert!(!with("", None)?);
    assert!(!with("two words", None)?);
    assert!(!with("work", Some("0123456789AB"))?);
    assert!(!with("work", Some("0123"))?);
    Ok(())
}

/// #475: a refused attempt (HTTP 401 before any response) records the refusal
/// through ledger format 5 and reads as zero tokens and an exact $0, never as
/// incomplete usage. Earlier records keep their bytes.
#[tokio::test]
async fn a_refused_attempt_is_an_exact_zero_through_format_5() -> anyhow::Result<()> {
    let path = home();
    let runtime = open(&path).await?;
    let price = snapshot()?;
    let ok = attempt(/*id*/ 1, /*time*/ 0)?;
    native(&runtime, ok.thread_id).await?;
    let store = AccountingStore::open(&runtime, /*as_of*/ 0).await?;
    store
        .admit(
            ok.thread_id,
            &ok,
            std::slice::from_ref(&price),
            /*as_of*/ 0,
        )
        .await?;
    store
        .observe(
            ok.thread_id,
            &ok,
            &[observation(&ok, /*revision*/ 1, /*input*/ 10)?],
            /*as_of*/ 0,
        )
        .await?;
    let formats = |runtime: Arc<StateRuntime>| async move {
        let mut conn = connection(&runtime).await?;
        let format: Vec<i64> =
            sqlx::query_scalar("SELECT version FROM _accounting_migrations ORDER BY version")
                .fetch_all(&mut conn)
                .await?;
        let observations: Vec<String> =
            sqlx::query_scalar("SELECT payload FROM draft_accounting_observations")
                .fetch_all(&mut conn)
                .await?;
        conn.close().await?;
        anyhow::Ok((format, observations))
    };
    let (before, earlier) = formats(runtime.clone()).await?;
    assert_eq!(before, vec![1]);

    let refused = attempt(/*id*/ 2, /*time*/ 1)?;
    store
        .admit(
            refused.thread_id,
            &refused,
            std::slice::from_ref(&price),
            /*as_of*/ 1,
        )
        .await?;
    let refusal = Observation {
        revision: 1.try_into()?,
        source: refused.attempt_id,
        sequence: 1.try_into()?,
        patch: Patch {
            refused_status: Some(401),
            ..Patch::default()
        },
    };
    let quote = store
        .observe(
            refused.thread_id,
            &refused,
            std::slice::from_ref(&refusal),
            /*as_of*/ 1,
        )
        .await?;
    assert_eq!(quote.usage.refused_status, Some(401));
    assert_eq!(quote.usage.total, Some(0));
    assert_eq!(quote.all_buckets_priced, Some(Decimal::default()));
    let (after, observations) = formats(runtime.clone()).await?;
    assert_eq!(after, vec![1, 2, 3, 4, 5]);
    assert!(observations.iter().any(|payload| payload == &earlier[0]));

    let view = ready(
        AccountingStore::inspect_day(
            &runtime,
            ok.thread_id,
            /*utc_day*/ 0,
            /*read_at_ms*/ 2,
        )
        .await?,
    );
    assert_eq!(view.totals.attempts, 2);
    // Only the first attempt, which reported input alone, is incomplete.
    assert_eq!(view.totals.per_use_unknown(), 1);
    // A refusal states nothing else, and nothing follows it.
    let mut bad = refusal.clone();
    bad.patch.input = Presence::Number(1.try_into()?);
    let other = attempt(/*id*/ 3, /*time*/ 2)?;
    store
        .admit(
            other.thread_id,
            &other,
            std::slice::from_ref(&price),
            /*as_of*/ 2,
        )
        .await?;
    assert!(
        store
            .observe(other.thread_id, &other, &[bad], /*as_of*/ 2)
            .await
            .is_err()
    );
    assert!(
        store
            .observe(
                refused.thread_id,
                &refused,
                &[observation(
                    &refused, /*revision*/ 2, /*input*/ 5
                )?],
                /*as_of*/ 2
            )
            .await
            .is_err()
    );
    runtime.close().await;
    Ok(())
}

/// #824: a realtime session is one attempt admitted when it starts; its end
/// is recorded through ledger format 6 and changes no count, so the session
/// reads as unknown usage with no price claim of $0. A session end states
/// nothing else, and a session ends once. Earlier records keep their bytes.
#[tokio::test]
async fn a_realtime_session_end_keeps_usage_unknown_through_format_6() -> anyhow::Result<()> {
    let path = home();
    let runtime = open(&path).await?;
    let price = snapshot()?;
    let ok = attempt(/*id*/ 1, /*time*/ 0)?;
    native(&runtime, ok.thread_id).await?;
    let store = AccountingStore::open(&runtime, /*as_of*/ 0).await?;
    store
        .admit(
            ok.thread_id,
            &ok,
            std::slice::from_ref(&price),
            /*as_of*/ 0,
        )
        .await?;
    store
        .observe(
            ok.thread_id,
            &ok,
            &[observation(&ok, /*revision*/ 1, /*input*/ 10)?],
            /*as_of*/ 0,
        )
        .await?;
    let formats = |runtime: Arc<StateRuntime>| async move {
        let mut conn = connection(&runtime).await?;
        let format: Vec<i64> =
            sqlx::query_scalar("SELECT version FROM _accounting_migrations ORDER BY version")
                .fetch_all(&mut conn)
                .await?;
        let observations: Vec<String> =
            sqlx::query_scalar("SELECT payload FROM draft_accounting_observations")
                .fetch_all(&mut conn)
                .await?;
        conn.close().await?;
        anyhow::Ok((format, observations))
    };
    let (before, earlier) = formats(runtime.clone()).await?;
    assert_eq!(before, vec![1]);

    let session = attempt(/*id*/ 2, /*time*/ 1)?;
    store
        .admit(
            session.thread_id,
            &session,
            std::slice::from_ref(&price),
            /*as_of*/ 1,
        )
        .await?;
    let end = Observation {
        revision: 1.try_into()?,
        source: session.attempt_id,
        sequence: 1.try_into()?,
        patch: Patch {
            session_ended_at_ms: Some(5.try_into()?),
            ..Patch::default()
        },
    };
    let quote = store
        .observe(
            session.thread_id,
            &session,
            std::slice::from_ref(&end),
            /*as_of*/ 1,
        )
        .await?;
    assert_eq!(quote.usage.session_ended_at_ms, Some(5));
    assert_eq!(quote.usage.input, None);
    assert_eq!(quote.usage.output, None);
    assert_eq!(quote.all_buckets_priced, None);
    let (after, observations) = formats(runtime.clone()).await?;
    assert_eq!(after, vec![1, 2, 3, 4, 5, 6]);
    assert!(observations.iter().any(|payload| payload == &earlier[0]));

    let view = ready(
        AccountingStore::inspect_day(
            &runtime,
            ok.thread_id,
            /*utc_day*/ 0,
            /*read_at_ms*/ 2,
        )
        .await?,
    );
    assert_eq!(view.totals.attempts, 2);
    // Both are incomplete: the first reported input alone, and the session
    // reported nothing. Neither is a $0 claim.
    assert_eq!(view.totals.per_use_unknown(), 2);

    // A session end states nothing else.
    let other = attempt(/*id*/ 3, /*time*/ 2)?;
    store
        .admit(
            other.thread_id,
            &other,
            std::slice::from_ref(&price),
            /*as_of*/ 2,
        )
        .await?;
    let mut bad = end.clone();
    bad.source = other.attempt_id;
    bad.patch.output = Presence::Number(1.try_into()?);
    assert!(
        store
            .observe(other.thread_id, &other, &[bad], /*as_of*/ 2)
            .await
            .is_err()
    );
    // A session ends once.
    let mut again = end.clone();
    again.revision = 2.try_into()?;
    again.sequence = 2.try_into()?;
    again.patch.session_ended_at_ms = Some(6.try_into()?);
    assert!(
        store
            .observe(session.thread_id, &session, &[again], /*as_of*/ 2)
            .await
            .is_err()
    );
    runtime.close().await;
    Ok(())
}

/// #314 (D228, Travis 2026-10-28): a request accounting failed to record
/// leaves a durable marker for its conversation and UTC day, so `/cost` can
/// say that day's totals may be incomplete. The first marker upgrades the
/// ledger to format 7 and leaves every record as it was; a marker of a
/// subagent counts for its parent, another conversation's does not, and
/// deleting the conversation removes its markers.
#[tokio::test]
async fn a_recording_gap_marks_its_day_through_format_7() -> anyhow::Result<()> {
    const DAY: i64 = 86_400_000;
    let path = home();
    let runtime = open(&path).await?;
    let ok = attempt(/*id*/ 1, /*time*/ 0)?;
    native(&runtime, ok.thread_id).await?;
    let store = AccountingStore::open(&runtime, /*as_of*/ 0).await?;
    store
        .admit(ok.thread_id, &ok, &[snapshot()?], /*as_of*/ 0)
        .await?;
    store
        .observe(
            ok.thread_id,
            &ok,
            &[observation(&ok, /*revision*/ 1, /*input*/ 10)?],
            /*as_of*/ 0,
        )
        .await?;
    // Current through the days read below.
    store.maintain(2 * DAY).await?;
    let state = |runtime: Arc<StateRuntime>| async move {
        let mut conn = connection(&runtime).await?;
        let format: Vec<i64> =
            sqlx::query_scalar("SELECT version FROM _accounting_migrations ORDER BY version")
                .fetch_all(&mut conn)
                .await?;
        let records: Vec<String> = sqlx::query_scalar(
            "SELECT payload FROM draft_accounting_attempts
             UNION ALL SELECT payload FROM draft_accounting_observations",
        )
        .fetch_all(&mut conn)
        .await?;
        conn.close().await?;
        anyhow::Ok((format, records))
    };
    let (before, records) = state(runtime.clone()).await?;
    assert_eq!(before, vec![1]);
    let gap = |owner: ThreadId, day: i64| {
        let runtime = runtime.clone();
        async move {
            anyhow::Ok(
                ready(
                    AccountingStore::inspect_day(&runtime, owner, day, /*read_at_ms*/ 2 * DAY)
                        .await?,
                )
                .recording_gap,
            )
        }
    };
    // A ledger before format 7 holds no marker, and reads as before.
    assert!(!gap(ok.thread_id, 0).await?);

    AccountingStore::record_gap(
        &runtime,
        ok.thread_id,
        /*from_ms*/ DAY + 5,
        /*to_ms*/ DAY + 5,
    )
    .await?;
    let (after, unchanged) = state(runtime.clone()).await?;
    assert_eq!(after, vec![1, 2, 3, 4, 5, 6, 7]);
    assert_eq!(unchanged, records);
    assert!(gap(ok.thread_id, 1).await?, "the marked day");
    assert!(!gap(ok.thread_id, 0).await?, "another day");
    // The clock read behind the ledger's checkpoint (day 2): the true time is
    // at least the checkpoint, so its day is marked too.
    assert!(gap(ok.thread_id, 2).await?, "the checkpoint's day");
    // Twice is one marker per day.
    AccountingStore::record_gap(&runtime, ok.thread_id, DAY + 9, DAY + 9).await?;
    let mut conn = connection(&runtime).await?;
    let markers: i64 = sqlx::query_scalar("SELECT count(*) FROM draft_accounting_gap_days")
        .fetch_one(&mut conn)
        .await?;
    assert_eq!(markers, 2);
    conn.close().await?;

    // A subagent's gap is its parent's; an unrelated conversation's is not.
    let child = ThreadId::from_string(&Uuid::from_u128(8).to_string())?;
    let other = ThreadId::from_string(&Uuid::from_u128(9).to_string())?;
    for thread in [child, other] {
        native(&runtime, thread).await?;
    }
    runtime
        .upsert_thread_spawn_edge(
            ok.thread_id,
            child,
            codex_state::DirectionalThreadSpawnEdgeStatus::Open,
        )
        .await?;
    AccountingStore::record_gap(&runtime, child, /*from_ms*/ 0, /*to_ms*/ 0).await?;
    AccountingStore::record_gap(&runtime, other, /*from_ms*/ 0, /*to_ms*/ 0).await?;
    assert!(gap(ok.thread_id, 0).await?, "the subagent's day");
    assert!(!gap(other, 1).await?, "not another conversation's day");

    // Deleting a conversation removes its markers with its records; the
    // deletion's maintenance, at today's time, expires the 1970 markers with
    // their days' totals.
    let today = chrono::Utc::now().timestamp_millis();
    AccountingStore::record_gap(&runtime, ok.thread_id, today, today).await?;
    AccountingStore::record_gap(&runtime, other, today, today).await?;
    assert_eq!(runtime.delete_thread(other).await?, 1);
    let mut conn = connection(&runtime).await?;
    let left: Vec<(String, i64)> =
        sqlx::query_as("SELECT thread_id, utc_day FROM draft_accounting_gap_days")
            .fetch_all(&mut conn)
            .await?;
    assert_eq!(left, vec![(ok.thread_id.to_string(), today / DAY)]);
    conn.close().await?;
    assert!(
        AccountingStore::record_gap(
            &runtime,
            ok.thread_id,
            /*from_ms*/ -1,
            /*to_ms*/ -1
        )
        .await
        .is_err()
    );
    runtime.close().await;
    Ok(())
}

/// #314 (D228): a gap in a home with no ledger installs none (the day reads
/// unavailable, never complete, as before); once a request is recorded, the
/// next gap marks its day.
#[tokio::test]
async fn a_recording_gap_never_installs_a_ledger() -> anyhow::Result<()> {
    let path = home();
    let runtime = open(&path).await?;
    let a = attempt(/*id*/ 1, /*time*/ 0)?;
    let owner = a.thread_id;
    native(&runtime, owner).await?;
    let read = |runtime: Arc<StateRuntime>| async move {
        AccountingStore::inspect_day(&runtime, owner, /*utc_day*/ 0, /*read_at_ms*/ 1).await
    };
    assert!(matches!(
        read(runtime.clone()).await?,
        InspectionDay::Absent
    ));
    AccountingStore::record_gap(&runtime, owner, /*from_ms*/ 0, /*to_ms*/ 0).await?;
    assert!(matches!(
        read(runtime.clone()).await?,
        InspectionDay::Absent
    ));
    let mut conn = connection(&runtime).await?;
    let objects: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM sqlite_schema WHERE name GLOB 'draft_accounting_*' OR name = '_accounting_migrations'",
    )
    .fetch_one(&mut conn)
    .await?;
    assert_eq!(objects, 0, "no ledger was installed");
    conn.close().await?;
    AccountingStore::open(&runtime, /*as_of*/ 1)
        .await?
        .admit(owner, &a, &[snapshot()?], /*as_of*/ 1)
        .await?;
    assert!(!ready(read(runtime.clone()).await?).recording_gap);
    AccountingStore::record_gap(&runtime, owner, /*from_ms*/ 0, /*to_ms*/ 0).await?;
    let view = ready(read(runtime.clone()).await?);
    assert!(view.recording_gap);
    assert_eq!(view.totals.attempts, 1);
    runtime.close().await;
    Ok(())
}

/// A ledger in a format newer than this build's is refused by name, for reads
/// and writes alike, and left untouched.
#[tokio::test]
async fn newer_ledger_format_is_refused_by_name() -> anyhow::Result<()> {
    let path = home();
    let runtime = open(&path).await?;
    let a = attempt(/*id*/ 1, /*time*/ 0)?;
    native(&runtime, a.thread_id).await?;
    AccountingStore::open(&runtime, /*as_of*/ 0)
        .await?
        .admit(a.thread_id, &a, &[], /*as_of*/ 0)
        .await?;
    let mut conn = connection(&runtime).await?;
    sqlx::query(
        "INSERT INTO _accounting_migrations (version, description, success, checksum, execution_time)
            VALUES (99, 'future', 1, X'00', 0)",
    )
    .execute(&mut conn)
    .await?;
    let before = inspection_tables(&mut conn).await?;
    let read = AccountingStore::inspect_day(
        &runtime,
        a.thread_id,
        /*utc_day*/ 0,
        /*read_at_ms*/ 1,
    )
    .await
    .expect_err("newer format read");
    let write = AccountingStore::open(&runtime, /*as_of*/ 1)
        .await
        .err()
        .context("newer format write")?;
    for error in [&read, &write] {
        assert!(is_newer_format(error), "{error:#}");
    }
    assert_eq!(inspection_tables(&mut conn).await?, before);
    conn.close().await?;
    runtime.close().await;
    Ok(())
}

/// Qualification helper, not part of the suite: validates and reads a copy of
/// a real ledger. Set `ACCT_REAL_LEDGER` to a directory holding a state DB.
#[tokio::test]
#[ignore = "reads a real ledger named by ACCT_REAL_LEDGER"]
async fn real_ledger_validates_and_reads() -> anyhow::Result<()> {
    let source = std::path::PathBuf::from(std::env::var("ACCT_REAL_LEDGER")?);
    let path = home();
    std::fs::create_dir_all(&*path)?;
    for entry in std::fs::read_dir(&source)? {
        let entry = entry?;
        std::fs::copy(entry.path(), path.join(entry.file_name()))?;
    }
    let runtime = open(&path).await?;
    let mut conn = connection(&runtime).await?;
    let days: Vec<(String, i64)> = sqlx::query_as(
        "SELECT DISTINCT json_extract(payload, '$.thread_id'),
                json_extract(payload, '$.dispatched_at_ms') / 86400000
            FROM draft_accounting_attempts ORDER BY 1, 2",
    )
    .fetch_all(&mut conn)
    .await?;
    conn.close().await?;
    let now = chrono::Utc::now().timestamp_millis();
    let mut read = Vec::new();
    for (thread, day) in &days {
        let owner = ThreadId::from_string(thread)?;
        read.push(
            AccountingStore::inspect_day(&runtime, owner, *day, now)
                .await
                .with_context(|| format!("{thread} day {day} read at {now}"))?,
        );
    }
    // Opening for a write validates the whole ledger; reads after it agree
    // with reads before it, apart from the later read time they state.
    AccountingStore::open(&runtime, AsOf::Now).await?;
    let later = chrono::Utc::now().timestamp_millis();
    for ((thread, day), before) in days.iter().zip(&read) {
        let owner = ThreadId::from_string(thread)?;
        let after = AccountingStore::inspect_day(&runtime, owner, *day, later).await?;
        match (after, before) {
            (InspectionDay::Ready(after), InspectionDay::Ready(before)) => {
                assert_eq!(
                    (&after.totals, &after.requests),
                    (&before.totals, &before.requests),
                    "{thread} day {day}"
                );
            }
            (after, before) => assert_eq!(&after, before, "{thread} day {day}"),
        }
    }
    println!("validated {} thread-days", days.len());
    runtime.close().await;
    Ok(())
}
