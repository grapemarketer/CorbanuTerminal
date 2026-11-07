use super::*;
use crate::runtime::test_support::test_thread_metadata;
use pretty_assertions::assert_eq;
use serde_json::json;
use std::path::Path;
use std::sync::Arc;
use uuid::Uuid;

#[path = "accounting_empty_day_tests.rs"]
mod empty_day;

fn inspection(value: InspectionDay) -> Inspection {
    let InspectionDay::Ready(value) = value else {
        panic!("{value:?}")
    };
    value
}

async fn inspected(runtime: &StateRuntime, day: i64, time: i64) -> anyhow::Result<InspectionDay> {
    AccountingStore::inspect_day(runtime, attempt(/*id*/ 1).thread_id, day, time).await
}

#[tokio::test]
async fn accounting_inspect_noncanonical_own_id_is_an_error() -> anyhow::Result<()> {
    let path = home();
    let runtime = open(&path).await?;
    seed(&runtime).await?;
    let a = attempt(/*id*/ 1);
    AccountingStore::open(&runtime, /*as_of*/ 0)
        .await?
        .admit(a.thread_id, &a, &[], /*as_of*/ 0)
        .await?;
    assert_eq!(
        inspection(inspected(&runtime, /*day*/ 0, /*time*/ 0).await?)
            .totals
            .attempts,
        1
    );
    for spelling in [
        a.thread_id.to_string().replace('-', ""),
        format!("urn:uuid:{}", a.thread_id),
        format!("{{{}}}", a.thread_id),
    ] {
        assert_eq!(ThreadId::from_string(&spelling)?, a.thread_id);
        sqlx::query(
            "UPDATE draft_accounting_attempts SET payload = json_set(payload, '$.thread_id', ?)",
        )
        .bind(spelling)
        .execute(runtime.pool.as_ref())
        .await?;
        let before = rows(&runtime).await?;
        marker(
            inspected(&runtime, /*day*/ 0, /*time*/ 0)
                .await
                .unwrap_err(),
            "invalid accounting ownership",
        );
        assert_eq!(rows(&runtime).await?, before);
    }
    runtime.close().await;
    Ok(())
}

#[tokio::test]
async fn accounting_inspect_work_is_not_refunded_for_unrelated_roots() -> anyhow::Result<()> {
    let path = home();
    let runtime = open(&path).await?;
    tree_fixture(&runtime).await?;
    // All six other owners are provably unrelated roots.
    sqlx::query("DELETE FROM thread_spawn_edges")
        .execute(runtime.pool.as_ref())
        .await?;
    let mut conn = runtime.pool.acquire().await?;
    let mut work = InspectionWork::new(&mut conn).await?;
    let before = work.visits;
    let view = inspect_tree_window(
        &mut conn,
        attempt(/*id*/ 1).thread_id,
        /*day*/ 0,
        /*read_at_ms*/ 0,
        /*window*/ None,
        &mut work,
    )
    .await?;
    assert_eq!(inspection(view).totals.attempts, 1);
    // Includes the owner's quote/history work, all seven candidates and six hops.
    assert!(before - work.visits >= 13);
    // Reuse the budget just as range buckets do: the second window must refuse.
    work.visits = 12;
    assert_eq!(
        inspect_tree_window(
            &mut conn,
            attempt(/*id*/ 1).thread_id,
            /*day*/ 0,
            /*read_at_ms*/ 0,
            /*window*/ None,
            &mut work
        )
        .await?,
        InspectionDay::TooLarge
    );
    // Exhaustion on the final UNKNOWN candidate must not become unavailable+Ready.
    sqlx::query("UPDATE threads SET source = 'unknown' WHERE id = ?")
        .bind(Uuid::from_u128(13).to_string())
        .execute(&mut *conn)
        .await?;
    let mut work = InspectionWork::new(&mut conn).await?;
    work.rows = 60;
    assert_eq!(
        inspect_tree_window(
            &mut conn,
            attempt(/*id*/ 1).thread_id,
            /*day*/ 0,
            /*read_at_ms*/ 0,
            /*window*/ None,
            &mut work
        )
        .await?,
        InspectionDay::TooLarge
    );
    drop(conn);
    runtime.close().await;
    Ok(())
}

#[tokio::test]
async fn accounting_inspect_provisional_unrelated_chain_is_bounded() -> anyhow::Result<()> {
    let path = home();
    let runtime = open(&path).await?;
    tree_fixture(&runtime).await?;
    let mut tx = runtime.pool.begin().await?;
    sqlx::query("DELETE FROM thread_spawn_edges")
        .execute(&mut *tx)
        .await?;
    // Candidate 8 traverses >10,000 nodes before terminating at an unrelated root.
    sqlx::raw_sql(
        "WITH RECURSIVE n(i) AS (VALUES(10000) UNION ALL SELECT i+1 FROM n WHERE i<20001)
         INSERT INTO threads (id,rollout_path,created_at,updated_at,source,model_provider,cwd,title,sandbox_policy,approval_mode)
         SELECT printf('00000000-0000-0000-0000-%012x',i),'fixture',0,0,'cli','fixture','fixture','','','' FROM n;
         WITH RECURSIVE n(i) AS (VALUES(10000) UNION ALL SELECT i+1 FROM n WHERE i<20000)
         INSERT INTO thread_spawn_edges SELECT printf('00000000-0000-0000-0000-%012x',i+1),
         printf('00000000-0000-0000-0000-%012x',i),'closed' FROM n;
         INSERT INTO thread_spawn_edges VALUES ('00000000-0000-0000-0000-000000002710',
         '00000000-0000-0000-0000-000000000008','closed');"
    ).execute(&mut *tx).await?;
    tx.commit().await?;
    assert_eq!(
        tokio::time::timeout(
            std::time::Duration::from_secs(30),
            inspected(&runtime, /*day*/ 0, /*time*/ 0)
        )
        .await??,
        InspectionDay::TooLarge
    );
    // An unrelated root's large parsed-and-dropped source is not retained memory.
    sqlx::query("DELETE FROM thread_spawn_edges")
        .execute(runtime.pool.as_ref())
        .await?;
    sqlx::query("UPDATE threads SET source = ? WHERE id = ?")
        .bind(format!("{}\"cli\"", " ".repeat(4 * 1024 * 1024)))
        .bind(Uuid::from_u128(8).to_string())
        .execute(runtime.pool.as_ref())
        .await?;
    let view = inspection(inspected(&runtime, /*day*/ 0, /*time*/ 0).await?);
    assert_eq!(
        (view.totals.attempts, view.unknown_parent_totals.attempts),
        (1, 0)
    );
    runtime.close().await;
    Ok(())
}

#[tokio::test]
async fn accounting_inspect_coverage_divergence_with_overdue_other_day() -> anyhow::Result<()> {
    const DAY: i64 = 86_400_000;
    let path = home();
    let runtime = open(&path).await?;
    seed(&runtime).await?;
    let a = attempt(/*id*/ 1);
    let store = AccountingStore::open(&runtime, /*as_of*/ 0).await?;
    store.admit(a.thread_id, &a, &[], /*as_of*/ 0).await?;
    // Simulate an advanced checkpoint with retention still overdue on day zero.
    sqlx::query("UPDATE draft_accounting_retention_checkpoint SET completed_as_of_ms = ?")
        .bind(400 * DAY)
        .execute(runtime.pool.as_ref())
        .await?;
    let before = rows(&runtime).await?;
    assert!(matches!(
        store
            .read_day(a.thread_id, /*utc_day*/ 400, 400 * DAY)
            .await?,
        RetainedDay::NeedsMaintenance { .. }
    ));
    let view = inspection(inspected(&runtime, /*day*/ 400, 400 * DAY).await?);
    assert_eq!(
        (
            view.totals.attempts,
            view.coverage.aggregate_day_floor,
            view.coverage.oldest_recorded_day
        ),
        (0, 36, Some(0))
    );
    assert!(matches!(
        inspected(&runtime, /*day*/ 0, 400 * DAY).await?,
        InspectionDay::CheckpointLag
    ));
    assert_eq!(rows(&runtime).await?, before);
    runtime.close().await;
    Ok(())
}

#[tokio::test]
async fn accounting_inspect_absent_schema_is_read_only() -> anyhow::Result<()> {
    let path = home();
    let runtime = open(&path).await?;
    let before = rows(&runtime).await?;
    assert_eq!(
        inspected(&runtime, /*day*/ 0, /*time*/ 0).await?,
        InspectionDay::Absent
    );
    assert_eq!(rows(&runtime).await?, before);
    runtime.close().await;
    Ok(())
}

#[tokio::test]
async fn accounting_inspect_schema_rejection_matrix() -> anyhow::Result<()> {
    for mutation in [
        "DROP TABLE draft_accounting_price_bindings",
        "DROP INDEX draft_accounting_request",
        "UPDATE _accounting_migrations SET success = 0",
        "UPDATE _accounting_migrations SET checksum = X'00'",
        "UPDATE _accounting_migrations SET version = version + 10",
        "DELETE FROM _accounting_migrations WHERE version = 1",
        "CREATE TABLE draft_accounting_ledger_format (version INTEGER PRIMARY KEY NOT NULL CHECK(version = 2))",
        "DELETE FROM _accounting_migrations",
        "DROP TABLE _accounting_migrations",
    ] {
        let path = home();
        let runtime = open(&path).await?;
        seed(&runtime).await?;
        sqlx::raw_sql(mutation)
            .execute(runtime.pool.as_ref())
            .await?;
        let before = rows(&runtime).await?;
        assert!(
            inspected(&runtime, /*day*/ 0, /*time*/ 1).await.is_err(),
            "{mutation}"
        );
        assert_eq!(rows(&runtime).await?, before);
        runtime.close().await;
    }
    Ok(())
}

#[tokio::test]
async fn accounting_inspect_raw_day_reconciles_contributions() -> anyhow::Result<()> {
    let path = home();
    let runtime = open(&path).await?;
    seed(&runtime).await?;
    let a = attempt(/*id*/ 1);
    let mut retry = attempt(/*id*/ 2);
    retry.request_id = a.request_id;
    retry.retry_of = Some(a.attempt_id);
    let store = AccountingStore::open(&runtime, /*as_of*/ 0).await?;
    store
        .admit(a.thread_id, &a, &[snapshot()], /*as_of*/ 0)
        .await?;
    store
        .observe(a.thread_id, &a, &[row(/*revision*/ 1)], /*as_of*/ 0)
        .await?;
    let first = store
        .observe(a.thread_id, &a, &[row(/*revision*/ 2)], /*as_of*/ 0)
        .await?;
    let second = store.admit(a.thread_id, &retry, &[], /*as_of*/ 0).await?;
    let view = inspection(inspected(&runtime, /*day*/ 0, /*time*/ 0).await?);
    assert_eq!(
        view.requests,
        std::collections::BTreeMap::from([(a.request_id, vec![first, second])])
    );
    assert_eq!(
        view.totals,
        DayTotals {
            measured: std::array::from_fn(|index| Metric {
                known: if index == 1 { 2 } else { 0 },
                unknown: if index == 1 { 1 } else { 2 },
            }),
            known_usd: "0.000002".to_owned().try_into()?,
            unknown_estimates: 2,
            attempts: 2,
            ..Default::default()
        }
    );
    runtime.close().await;
    Ok(())
}

#[tokio::test]
async fn accounting_inspect_original_null_and_price_are_immutable() -> anyhow::Result<()> {
    let path = home();
    let runtime = open(&path).await?;
    seed(&runtime).await?;
    let store = AccountingStore::open(&runtime, /*as_of*/ 0).await?;
    for (a, prices) in [
        (attempt(/*id*/ 1), vec![snapshot()]),
        (attempt(/*id*/ 2), vec![]),
    ] {
        store.admit(a.thread_id, &a, &prices, /*as_of*/ 0).await?;
        let before = inspected(&runtime, /*day*/ 0, /*time*/ 0).await?;
        let mut later = snapshot();
        later.id = Uuid::from_u128(9999);
        later.rates.noncached = Some("99".to_owned().try_into()?);
        store.admit(a.thread_id, &a, &[later], /*as_of*/ 0).await?;
        assert_eq!(inspected(&runtime, /*day*/ 0, /*time*/ 0).await?, before);
    }
    runtime.close().await;
    Ok(())
}

#[tokio::test]
async fn accounting_inspect_missing_binding_is_not_unpriced() -> anyhow::Result<()> {
    for mutation in [
        "DELETE FROM draft_accounting_contributions; DELETE FROM draft_accounting_estimates; DELETE FROM draft_accounting_price_bindings",
        "UPDATE draft_accounting_contributions SET thread_id = '00000000-0000-0000-0000-000000000008'",
        "UPDATE draft_accounting_attempts SET request_id = '00000000-0000-0000-0000-000000000008'",
    ] {
        let path = home();
        let runtime = open(&path).await?;
        seed(&runtime).await?;
        let a = attempt(/*id*/ 1);
        AccountingStore::open(&runtime, /*as_of*/ 0)
            .await?
            .admit(a.thread_id, &a, &[], /*as_of*/ 0)
            .await?;
        assert!(
            inspection(inspected(&runtime, /*day*/ 0, /*time*/ 0).await?).requests[&a.request_id]
                [0]
            .snapshot
            .is_none()
        );
        sqlx::raw_sql(mutation)
            .execute(runtime.pool.as_ref())
            .await?;
        let before = rows(&runtime).await?;
        assert!(
            inspected(&runtime, /*day*/ 0, /*time*/ 0).await.is_err(),
            "{mutation}"
        );
        assert_eq!(rows(&runtime).await?, before);
        runtime.close().await;
    }
    Ok(())
}

#[tokio::test]
async fn accounting_inspect_checkpoint_and_stale_estimate() -> anyhow::Result<()> {
    let path = home();
    let runtime = open(&path).await?;
    seed(&runtime).await?;
    let a = attempt(/*id*/ 1);
    let store = AccountingStore::open(&runtime, /*as_of*/ 0).await?;
    store.admit(a.thread_id, &a, &[], /*as_of*/ 0).await?;
    let before = rows(&runtime).await?;
    let lagged = inspection(inspected(&runtime, /*day*/ 0, /*time*/ 12).await?);
    assert_eq!(
        (lagged.read_at_ms, lagged.coverage.completed_as_of_ms),
        (12, 0)
    );
    assert_eq!(rows(&runtime).await?, before);
    assert_eq!(
        inspected(&runtime, /*day*/ 1, /*time*/ 86_400_000).await?,
        InspectionDay::CheckpointLag
    );
    let range = range_read(
        &runtime,
        /*start*/ 86_400_000,
        /*end*/ 90_000_000,
        InspectionGrouping::Hour,
        /*now*/ 88_200_000,
    )
    .await?;
    assert!(matches!(
        range,
        InspectionDay::Range { completed_as_of_ms: Some(0), buckets, .. }
            if buckets[0].days == vec![InspectionDay::CheckpointLag]
    ));
    assert_eq!(rows(&runtime).await?, before);
    store
        .observe(a.thread_id, &a, &[row(/*revision*/ 1)], /*as_of*/ 0)
        .await?;
    sqlx::query("UPDATE draft_accounting_contributions SET evidence = '[]'")
        .execute(runtime.pool.as_ref())
        .await?;
    let stale = rows(&runtime).await?;
    assert_eq!(
        inspected(&runtime, /*day*/ 0, /*time*/ 1).await?,
        InspectionDay::NeedsRefresh
    );
    assert_eq!(rows(&runtime).await?, stale);
    sqlx::raw_sql(
        "DELETE FROM draft_accounting_contributions; DELETE FROM draft_accounting_estimates",
    )
    .execute(runtime.pool.as_ref())
    .await?;
    assert_eq!(
        inspected(&runtime, /*day*/ 0, /*time*/ 1).await?,
        InspectionDay::NeedsRefresh
    );
    runtime.close().await;
    Ok(())
}

#[tokio::test]
async fn accounting_inspect_never_maintained_is_checkpoint_lag() -> anyhow::Result<()> {
    let path = home();
    let runtime = open(&path).await?;
    seed(&runtime).await?;
    sqlx::query(
        "UPDATE draft_accounting_retention_checkpoint SET completed_as_of_ms = NULL, admission_active = 0",
    )
    .execute(runtime.pool.as_ref())
    .await?;
    let before = rows(&runtime).await?;
    assert_eq!(
        inspected(&runtime, /*day*/ 0, /*time*/ 0).await?,
        InspectionDay::CheckpointLag
    );
    let range = range_read(
        &runtime,
        /*start*/ 0,
        /*end*/ 3_600_000,
        InspectionGrouping::Hour,
        /*now*/ 0,
    )
    .await?;
    assert!(matches!(
        range,
        InspectionDay::Range { completed_as_of_ms: None, buckets, .. }
            if buckets[0].days == vec![InspectionDay::CheckpointLag]
    ));
    assert_eq!(rows(&runtime).await?, before);
    runtime.close().await;
    Ok(())
}

#[tokio::test]
async fn accounting_inspect_retention_and_compact_matrix() -> anyhow::Result<()> {
    const DAY: i64 = 86_400_000;
    let path = home();
    let runtime = open(&path).await?;
    seed(&runtime).await?;
    let a = attempt(/*id*/ 1);
    let store = AccountingStore::open(&runtime, /*as_of*/ 0).await?;
    store.admit(a.thread_id, &a, &[], /*as_of*/ 0).await?;
    assert!(matches!(
        inspected(&runtime, /*day*/ 0, 90 * DAY - 1).await?,
        InspectionDay::Ready(_)
    ));
    let before = rows(&runtime).await?;
    for now in [90 * DAY, 90 * DAY + 1, 365 * DAY] {
        assert!(matches!(
            inspected(&runtime, /*day*/ 0, now).await?,
            InspectionDay::DetailUnavailable { compact: false, .. }
        ));
    }
    assert_eq!(rows(&runtime).await?, before);
    let mut late = attempt(/*id*/ 2);
    late.dispatched_at_ms = 1.try_into()?;
    store.admit(a.thread_id, &late, &[], /*as_of*/ 1).await?;
    store.maintain(90 * DAY).await?;
    let unavailable = inspected(&runtime, /*day*/ 0, 90 * DAY).await?;
    assert!(matches!(
        unavailable,
        InspectionDay::DetailUnavailable {
            compact: true,
            coverage: RetentionCoverage {
                oldest_recorded_day: Some(0),
                ..
            },
            ..
        }
    ));
    store.maintain(90 * DAY + 1).await?;
    assert!(matches!(
        inspected(&runtime, /*day*/ 0, 90 * DAY + 1).await?,
        InspectionDay::DetailUnavailable { compact: true, .. }
    ));
    store.maintain(365 * DAY).await?;
    assert!(matches!(
        inspected(&runtime, /*day*/ 0, 365 * DAY).await?,
        InspectionDay::DetailUnavailable {
            coverage: RetentionCoverage {
                aggregate_day_floor: 1,
                oldest_recorded_day: None,
                ..
            },
            ..
        }
    ));
    runtime.close().await;
    Ok(())
}

#[tokio::test]
async fn accounting_inspect_half_open_date_bounds() -> anyhow::Result<()> {
    const DAY: i64 = 86_400_000;
    let path = home();
    let runtime = open(&path).await?;
    seed(&runtime).await?;
    let store = AccountingStore::open(&runtime, /*as_of*/ 0).await?;
    for (id, time) in [(1, 0), (2, DAY - 1), (3, DAY)] {
        let mut a = attempt(id);
        a.dispatched_at_ms = time.try_into()?;
        store.admit(a.thread_id, &a, &[], time).await?;
    }
    // Later observation admission does not change request-day ownership.
    store
        .observe(
            attempt(/*id*/ 1).thread_id,
            &attempt(/*id*/ 1),
            &[row(/*revision*/ 1)],
            DAY,
        )
        .await?;
    assert_eq!(
        inspection(inspected(&runtime, /*day*/ 0, DAY).await?)
            .totals
            .attempts,
        2
    );
    assert_eq!(
        inspection(inspected(&runtime, /*day*/ 1, DAY).await?)
            .totals
            .attempts,
        1
    );
    for (day, now) in [
        (-1, DAY),
        (2, DAY),
        (0, -1),
        (0, DAY - 1),
        (i64::MAX, i64::MAX),
    ] {
        assert!(inspected(&runtime, day, now).await.is_err());
    }
    runtime.close().await;
    Ok(())
}

#[tokio::test]
async fn accounting_inspect_corruption_and_unknowns() -> anyhow::Result<()> {
    for mutation in [
        "UPDATE draft_accounting_observations SET source = '00000000-0000-0000-0000-000000000009'",
        "UPDATE draft_accounting_observations SET sequence = 9",
        "UPDATE draft_accounting_estimates SET payload = '{}' WHERE evidence = '[]'",
        "UPDATE draft_accounting_attempts SET payload = '{}'",
    ] {
        let path = home();
        let runtime = open(&path).await?;
        seed(&runtime).await?;
        let a = attempt(/*id*/ 1);
        let store = AccountingStore::open(&runtime, /*as_of*/ 0).await?;
        store.admit(a.thread_id, &a, &[], /*as_of*/ 0).await?;
        let mut zero = row(/*revision*/ 1);
        zero.patch = serde_json::from_value(json!({"input":0,"read":null}))?;
        store
            .observe(a.thread_id, &a, &[zero.clone()], /*as_of*/ 0)
            .await?;
        let view = inspection(inspected(&runtime, /*day*/ 0, /*time*/ 0).await?);
        assert_eq!(view.requests[&a.request_id][0].observations, vec![zero]);
        assert_eq!(view.requests[&a.request_id][0].usage.noncached, Some(0));
        assert_eq!(view.requests[&a.request_id][0].usage.read, None);
        sqlx::raw_sql(mutation)
            .execute(runtime.pool.as_ref())
            .await?;
        let before = rows(&runtime).await?;
        assert!(
            inspected(&runtime, /*day*/ 0, /*time*/ 0).await.is_err(),
            "{mutation}"
        );
        assert_eq!(rows(&runtime).await?, before);
        runtime.close().await;
    }
    let path = home();
    let runtime = open(&path).await?;
    seed(&runtime).await?;
    let store = AccountingStore::open(&runtime, /*as_of*/ 0).await?;
    for id in [1, 2] {
        let a = attempt(id);
        store.admit(a.thread_id, &a, &[], /*as_of*/ 0).await?;
        let mut observation = row(/*revision*/ 1);
        observation.source = a.attempt_id;
        observation.patch =
            serde_json::from_value(json!({"input": if id == 1 { i64::MAX } else { 1 }}))?;
        sqlx::query("INSERT INTO draft_accounting_observations VALUES (?, 1, ?, 1, ?)")
            .bind(a.attempt_id.to_string())
            .bind(observation.source.to_string())
            .bind(serde_json::to_string(&observation)?)
            .execute(runtime.pool.as_ref())
            .await?;
    }
    marker(
        inspected(&runtime, /*day*/ 0, /*time*/ 0)
            .await
            .unwrap_err(),
        "overflow",
    );
    runtime.close().await;
    Ok(())
}

#[tokio::test]
async fn accounting_inspect_single_snapshot_concurrent_writer() -> anyhow::Result<()> {
    let path = home();
    let runtime = open(&path).await?;
    seed(&runtime).await?;
    let a = attempt(/*id*/ 1);
    let store = AccountingStore::open(&runtime, /*as_of*/ 0).await?;
    store
        .admit(a.thread_id, &a, &[snapshot()], /*as_of*/ 0)
        .await?;
    let mut before = inspection(inspected(&runtime, /*day*/ 0, /*time*/ 0).await?);
    // The single-owner journal read does not read other conversations.
    assert_eq!(before.other_conversations.take(), Some(Default::default()));
    let before = InspectionDay::Ready(before);
    // Establish the same read snapshot as inspect_day before releasing the writer.
    let mut tx = runtime.pool.begin().await?;
    validate_on_connection(&mut tx).await?;
    let (release, barrier) = tokio::sync::oneshot::channel();
    let writer_runtime = runtime.clone();
    let writer = tokio::spawn(async move {
        barrier.await?;
        AccountingStore::open(&writer_runtime, /*as_of*/ 0)
            .await?
            .observe(a.thread_id, &a, &[row(/*revision*/ 1)], /*as_of*/ 0)
            .await?;
        anyhow::Ok(())
    });
    release.send(()).unwrap();
    writer.await??;
    assert_eq!(
        Journal::inspect_on_connection(
            &mut tx,
            attempt(/*id*/ 1).thread_id,
            /*day*/ 0,
            /*read_at_ms*/ 0
        )
        .await?,
        before
    );
    tx.commit().await?;
    let after = inspection(inspected(&runtime, /*day*/ 0, /*time*/ 0).await?);
    assert_eq!(
        after.totals.measured[1],
        Metric {
            known: 1,
            unknown: 0
        }
    );
    assert_eq!(
        after.requests[&attempt(/*id*/ 1).request_id][0]
            .usage
            .noncached,
        Some(1)
    );
    runtime.close().await;
    Ok(())
}

// Root, closed child, grandchild, orphan, unrelated root, and cycle.
async fn tree_fixture(runtime: &StateRuntime) -> anyhow::Result<()> {
    seed(runtime).await?;
    let store = AccountingStore::open(runtime, /*as_of*/ 0).await?;
    for id in 1..=7 {
        let mut a = attempt(id);
        a.thread_id = ThreadId::from_string(&Uuid::from_u128(id + 6).to_string())?;
        runtime
            .upsert_thread(&test_thread_metadata(
                runtime.sqlite().home(),
                a.thread_id,
                runtime.sqlite().home().to_path_buf(),
            ))
            .await?;
        store
            .admit(a.thread_id, &a, &[snapshot()], /*as_of*/ 0)
            .await?;
        store
            .observe(a.thread_id, &a, &[row(id as i64)], /*as_of*/ 0)
            .await?;
    }
    for (parent, child) in [(7, 8), (8, 9), (999, 10), (12, 13), (13, 12)] {
        runtime
            .upsert_thread_spawn_edge(
                ThreadId::from_string(&Uuid::from_u128(parent).to_string())?,
                ThreadId::from_string(&Uuid::from_u128(child).to_string())?,
                crate::DirectionalThreadSpawnEdgeStatus::Closed,
            )
            .await?;
    }
    Ok(())
}

#[tokio::test]
async fn accounting_inspect_tree_reconciles_and_quarantines_unknown_parents() -> anyhow::Result<()>
{
    let path = home();
    let runtime = open(&path).await?;
    tree_fixture(&runtime).await?;
    let before = rows(&runtime).await?;
    let view = inspection(inspected(&runtime, /*day*/ 0, /*time*/ 0).await?);
    // Independent known-price calculation: one USD per million noncached tokens.
    assert_eq!(view.totals.known_usd, "0.000006".to_owned().try_into()?);
    assert_eq!(view.own_totals.known_usd, "0.000001".to_owned().try_into()?);
    assert_eq!(
        view.descendant_totals.known_usd,
        "0.000005".to_owned().try_into()?
    );
    assert_eq!(
        view.unknown_parent_totals.known_usd,
        "0.000017".to_owned().try_into()?
    );
    assert_eq!(
        (
            view.totals.attempts,
            view.own_totals.attempts,
            view.descendant_totals.attempts,
            view.unknown_parent_totals.attempts
        ),
        (3, 1, 2, 3)
    );
    let ids = |requests: &std::collections::BTreeMap<Uuid, Vec<ObservationQuote>>| {
        requests
            .values()
            .flatten()
            .map(|q| q.attempt.attempt_id)
            .collect::<Vec<_>>()
    };
    assert_eq!(ids(&view.requests), [1, 2, 3].map(Uuid::from_u128));
    assert_eq!(
        ids(&view.unknown_parent_requests),
        [4, 6, 7].map(Uuid::from_u128)
    );
    assert_eq!(rows(&runtime).await?, before);
    runtime.close().await;
    Ok(())
}

#[tokio::test]
async fn accounting_inspect_conflicting_and_missing_edges_are_unknown() -> anyhow::Result<()> {
    let path = home();
    let runtime = open(&path).await?;
    tree_fixture(&runtime).await?;
    let source =
        json!({"subagent": {"thread_spawn": {"parent_thread_id": Uuid::from_u128(7), "depth": 1}}})
            .to_string();
    // Conflicting source versus edge, then source without an edge: neither is repaired.
    for id in [9, 11] {
        sqlx::query("UPDATE threads SET source = ? WHERE id = ?")
            .bind(&source)
            .bind(Uuid::from_u128(id).to_string())
            .execute(runtime.pool.as_ref())
            .await?;
    }
    let before = rows(&runtime).await?;
    let view = inspection(inspected(&runtime, /*day*/ 0, /*time*/ 0).await?);
    assert_eq!(
        (view.totals.attempts, view.unknown_parent_totals.attempts),
        (2, 5)
    );
    assert_eq!(view.totals.known_usd, "0.000003".to_owned().try_into()?);
    assert_eq!(rows(&runtime).await?, before);
    runtime.close().await;
    Ok(())
}

#[tokio::test]
async fn accounting_inspect_malformed_source_with_edge_is_unknown() -> anyhow::Result<()> {
    let path = home();
    let runtime = open(&path).await?;
    tree_fixture(&runtime).await?;
    // A surviving edge cannot make an unparsable source authoritative.
    sqlx::query("UPDATE threads SET source = 'malformed' WHERE id = ?")
        .bind(Uuid::from_u128(8).to_string())
        .execute(runtime.pool.as_ref())
        .await?;
    let view = inspection(inspected(&runtime, /*day*/ 0, /*time*/ 0).await?);
    assert_eq!(
        (view.totals.attempts, view.unknown_parent_totals.attempts),
        (1, 5)
    );
    runtime.close().await;
    Ok(())
}

#[tokio::test]
async fn accounting_inspect_descendant_stale_and_compact_refuse_tree_total() -> anyhow::Result<()> {
    for mutation in [
        "DELETE FROM draft_accounting_contributions WHERE attempt_id = '00000000-0000-0000-0000-000000000002'",
        "DELETE FROM draft_accounting_contributions WHERE attempt_id = '00000000-0000-0000-0000-000000000002'; DELETE FROM draft_accounting_estimates WHERE attempt_id = '00000000-0000-0000-0000-000000000002'",
    ] {
        let path = home();
        let runtime = open(&path).await?;
        tree_fixture(&runtime).await?;
        sqlx::raw_sql(mutation)
            .execute(runtime.pool.as_ref())
            .await?;
        let before = rows(&runtime).await?;
        assert_eq!(
            inspected(&runtime, /*day*/ 0, /*time*/ 0).await?,
            InspectionDay::NeedsRefresh
        );
        assert_eq!(rows(&runtime).await?, before);
        runtime.close().await;
    }
    let path = home();
    let runtime = open(&path).await?;
    tree_fixture(&runtime).await?;
    let _store = AccountingStore::open(&runtime, 90 * 86_400_000).await?;
    let before = rows(&runtime).await?;
    assert!(matches!(
        AccountingStore::inspect_day(
            &runtime,
            attempt(/*id*/ 1).thread_id,
            /*utc_day*/ 0,
            90 * 86_400_000
        )
        .await?,
        InspectionDay::DetailUnavailable { compact: true, .. }
    ));
    assert_eq!(rows(&runtime).await?, before);
    runtime.close().await;
    Ok(())
}

#[tokio::test]
async fn accounting_inspect_unknown_unavailable_preserves_root() -> anyhow::Result<()> {
    let path = home();
    let runtime = open(&path).await?;
    tree_fixture(&runtime).await?;
    let mut expected = inspection(inspected(&runtime, /*day*/ 0, /*time*/ 0).await?);
    expected.unknown_parent_requests.clear();
    expected.unknown_parent_totals = DayTotals::default();
    expected.unknown_parent_unavailable_threads = 3;
    // Orphan and both cycle members have unavailable contributions.
    for id in [4, 6, 7] {
        sqlx::query("DELETE FROM draft_accounting_contributions WHERE attempt_id = ?")
            .bind(Uuid::from_u128(id).to_string())
            .execute(runtime.pool.as_ref())
            .await?;
    }
    let before = rows(&runtime).await?;
    assert_eq!(
        inspected(&runtime, /*day*/ 0, /*time*/ 0).await?,
        InspectionDay::Ready(expected)
    );
    assert_eq!(rows(&runtime).await?, before);
    runtime.close().await;
    Ok(())
}

// acct-scope-62: an empty or partial root must not read as the whole day. The
// unrelated root (thread 11) is read beside the tree, never into its totals.
#[tokio::test]
async fn accounting_inspect_other_conversations_are_read_beside_the_tree() -> anyhow::Result<()> {
    let path = home();
    let runtime = open(&path).await?;
    tree_fixture(&runtime).await?;
    let other = ThreadId::from_string(&Uuid::from_u128(11).to_string())?;
    let alone = inspection(
        AccountingStore::inspect_day(&runtime, other, /*utc_day*/ 0, /*read_at_ms*/ 0).await?,
    );
    assert_eq!(alone.totals.known_usd, "0.000005".to_owned().try_into()?);
    let view = inspection(inspected(&runtime, /*day*/ 0, /*time*/ 0).await?);
    assert_eq!(
        view.other_conversations,
        Some(OtherConversations {
            conversations: 1,
            unavailable: 0,
            requests: alone.requests,
            deleted_attempts: DeletedAttempts::Counted(0),
        })
    );
    assert_eq!(
        (view.totals.attempts, view.totals.known_usd),
        (3, "0.000006".to_owned().try_into()?)
    );

    // An unreadable other conversation is counted as unread; the root is unchanged.
    let mut expected = view;
    expected.other_conversations = Some(OtherConversations {
        conversations: 1,
        unavailable: 1,
        requests: Default::default(),
        deleted_attempts: DeletedAttempts::Counted(0),
    });
    sqlx::query("DELETE FROM draft_accounting_contributions WHERE attempt_id = ?")
        .bind(Uuid::from_u128(5).to_string())
        .execute(runtime.pool.as_ref())
        .await?;
    let before = rows(&runtime).await?;
    assert_eq!(
        inspected(&runtime, /*day*/ 0, /*time*/ 0).await?,
        InspectionDay::Ready(expected)
    );
    assert_eq!(rows(&runtime).await?, before);
    runtime.close().await;
    Ok(())
}

// #286: deleting a conversation removes its attempts and their cost. The day
// must still say that deleted conversations spent on it, never read as empty.
#[tokio::test]
async fn accounting_inspect_counts_deleted_conversations_attempts() -> anyhow::Result<()> {
    let path = home();
    let runtime = open(&path).await?;
    tree_fixture(&runtime).await?;
    let other = ThreadId::from_string(&Uuid::from_u128(11).to_string())?;
    let before = inspection(inspected(&runtime, /*day*/ 0, /*time*/ 0).await?)
        .other_conversations
        .expect("day view reads other conversations");
    let attempts: usize = before.requests.values().map(Vec::len).sum();
    assert_eq!((before.conversations, attempts), (1, 1));
    runtime.delete_threads_at(&[other], /*as_of_ms*/ 0).await?;
    assert_eq!(
        inspection(inspected(&runtime, /*day*/ 0, /*time*/ 0).await?).other_conversations,
        Some(OtherConversations {
            conversations: 0,
            unavailable: 0,
            requests: Default::default(),
            deleted_attempts: DeletedAttempts::Counted(attempts),
        })
    );
    runtime.close().await;
    Ok(())
}

// #286: only tombstones that cannot come from retention count as deletions.
#[tokio::test]
async fn accounting_deleted_attempts_count_only_what_retention_cannot_explain() -> anyhow::Result<()>
{
    const REPLAY: i64 = 365 * DAY;
    let path = home();
    let runtime = open(&path).await?;
    seed(&runtime).await?;
    let ahead = 200 * DAY;
    AccountingStore::open(&runtime, ahead).await?;
    // Tombstones for attempts dispatched on day 150 and day 100.
    for (id, dispatch) in [(1u128, 150 * DAY + 5), (2, 100 * DAY + 5)] {
        sqlx::query("INSERT INTO draft_accounting_tombstones VALUES (?, ?)")
            .bind(Uuid::from_u128(id).to_string())
            .bind(dispatch + REPLAY)
            .execute(runtime.pool.as_ref())
            .await?;
    }
    let count = |day: i64, read_at: i64| {
        let runtime = runtime.clone();
        async move {
            let mut conn = runtime.pool.acquire().await?;
            let mut work = InspectionWork::new(&mut conn).await?;
            anyhow::Ok(scope::deleted_attempts(&mut conn, day, read_at, &mut work).await)
        }
    };
    // Inside the detail window at the checkpoint: a deletion.
    assert_eq!(count(150, ahead).await?, DeletedAttempts::Counted(1));
    // Past it (day 100 + 90 days <= day 200): retention may have written it.
    assert_eq!(count(100, ahead).await?, DeletedAttempts::PastDetailWindow);
    // Batched expiry can run ahead of the checkpoint: the reader's clock
    // bounds the window too, exactly at the edge.
    assert_eq!(
        count(150, 240 * DAY).await?,
        DeletedAttempts::PastDetailWindow
    );
    assert_eq!(
        count(150, 240 * DAY - 1).await?,
        DeletedAttempts::Counted(1)
    );
    // A day nothing was deleted on, inside the window.
    assert_eq!(count(151, ahead).await?, DeletedAttempts::Counted(0));
    // An exhausted work budget reads as unread, never zero.
    {
        let mut conn = runtime.pool.acquire().await?;
        let mut work = InspectionWork {
            rows: 0,
            visits: 0,
            scan_rows: 1,
            started: None,
        };
        assert_eq!(
            scope::deleted_attempts(&mut conn, /*day*/ 150, ahead, &mut work).await,
            DeletedAttempts::Unread
        );
    }
    // No checkpoint yet: unread, never zero.
    sqlx::query("UPDATE draft_accounting_retention_checkpoint SET completed_as_of_ms = NULL, admission_active = 0")
        .execute(runtime.pool.as_ref())
        .await?;
    assert_eq!(count(150, ahead).await?, DeletedAttempts::Unread);
    runtime.close().await;
    Ok(())
}

// Range buckets do not read other conversations, and say so with `None`.
#[tokio::test]
async fn accounting_inspect_range_days_do_not_read_other_conversations() -> anyhow::Result<()> {
    let path = home();
    let runtime = open(&path).await?;
    tree_fixture(&runtime).await?;
    let mut buckets = range_buckets(
        range_read(
            &runtime,
            /*start*/ 0,
            /*end*/ 86_400_000,
            InspectionGrouping::Day,
            /*now*/ 0,
        )
        .await?,
    );
    let view = inspection(buckets.remove(0).days.remove(0));
    assert_eq!((view.totals.attempts, view.other_conversations), (3, None));
    runtime.close().await;
    Ok(())
}

#[tokio::test]
async fn accounting_inspect_unknown_compact_preserves_root_coverage() -> anyhow::Result<()> {
    let path = home();
    let runtime = open(&path).await?;
    tree_fixture(&runtime).await?;
    let mut expected = inspection(inspected(&runtime, /*day*/ 0, /*time*/ 0).await?);
    expected
        .unknown_parent_requests
        .remove(&attempt(/*id*/ 4).request_id);
    expected.unknown_parent_totals =
        DayTotals::from_quotes(expected.unknown_parent_requests.values().flatten())?;
    expected.unknown_parent_unavailable_threads = 1;
    // Synthetic compact evidence isolates attribution loss from the global age cutoff.
    let compact = json!({"version": 1, "known": vec![0; 7], "unknown": vec![1; 7],
        "known_usd": "0", "unknown_estimates": 1, "attempts": 1})
    .to_string();
    for owner in [10, 8] {
        sqlx::query("INSERT INTO draft_accounting_compact_days VALUES (?, 0, ?)")
            .bind(Uuid::from_u128(owner).to_string())
            .bind(&compact)
            .execute(runtime.pool.as_ref())
            .await?;
        let before = rows(&runtime).await?;
        let result = inspected(&runtime, /*day*/ 0, /*time*/ 0).await?;
        if owner == 10 {
            assert_eq!(inspection(result), expected);
        } else {
            assert!(matches!(
                result,
                InspectionDay::DetailUnavailable { compact: true, .. }
            ));
        }
        assert_eq!(rows(&runtime).await?, before);
    }
    runtime.close().await;
    Ok(())
}

#[tokio::test]
async fn accounting_inspect_candidate_cap_precedes_unavailable_candidates() -> anyhow::Result<()> {
    let path = home();
    let runtime = open(&path).await?;
    tree_fixture(&runtime).await?;
    let mut tx = runtime.pool.begin().await?;
    let mut expected = inspection(inspected(&runtime, /*day*/ 0, /*time*/ 0).await?);
    expected.unknown_parent_unavailable_threads += 512;
    // Distinct unknown owners missing contributions remain unavailable unknowns;
    // their count must not suppress the known root and descendant totals.
    for id in 20..532 {
        let mut a = attempt(id);
        a.thread_id = ThreadId::from_string(&Uuid::from_u128(id + 1000).to_string())?;
        sqlx::query("INSERT INTO draft_accounting_attempts VALUES (?, ?, ?)")
            .bind(a.attempt_id.to_string())
            .bind(a.request_id.to_string())
            .bind(serde_json::to_string(&a)?)
            .execute(&mut *tx)
            .await?;
        sqlx::query("INSERT INTO draft_accounting_price_bindings VALUES (?, NULL)")
            .bind(a.attempt_id.to_string())
            .execute(&mut *tx)
            .await?;
    }
    tx.commit().await?;
    assert_eq!(
        inspected(&runtime, /*day*/ 0, /*time*/ 0).await?,
        InspectionDay::Ready(expected)
    );
    runtime.close().await;
    Ok(())
}

#[tokio::test]
async fn accounting_inspect_tree_lineage_and_cost_share_one_snapshot() -> anyhow::Result<()> {
    let path = home();
    let runtime = open(&path).await?;
    tree_fixture(&runtime).await?;
    let before = inspected(&runtime, /*day*/ 0, /*time*/ 0).await?;
    let mut read = runtime.pool.begin().await?;
    validate_on_connection(&mut read).await?;
    sqlx::query("UPDATE thread_spawn_edges SET parent_thread_id = ? WHERE child_thread_id = ?")
        .bind(Uuid::from_u128(11).to_string())
        .bind(Uuid::from_u128(8).to_string())
        .execute(runtime.pool.as_ref())
        .await?;
    assert_eq!(
        inspect_tree(
            &mut read,
            attempt(/*id*/ 1).thread_id,
            /*day*/ 0,
            /*read_at_ms*/ 0
        )
        .await?,
        before
    );
    read.commit().await?;
    assert_eq!(
        inspection(inspected(&runtime, /*day*/ 0, /*time*/ 0).await?)
            .totals
            .attempts,
        1
    );
    runtime.close().await;
    Ok(())
}

#[tokio::test]
async fn accounting_inspect_tree_combined_attempt_cap_is_not_per_run() -> anyhow::Result<()> {
    let path = home();
    let runtime = open(&path).await?;
    tree_fixture(&runtime).await?;
    let store = AccountingStore::open(&runtime, /*as_of*/ 0).await?;
    // Batch synthetic rows to avoid hundreds of unrelated maintenance sweeps.
    let template = store
        .admit(
            attempt(/*id*/ 1).thread_id,
            &attempt(/*id*/ 19),
            &[],
            /*as_of*/ 0,
        )
        .await?;
    let mut tx = runtime.pool.begin().await?;
    for id in 20..530 {
        let mut q = template.clone();
        q.attempt = attempt(id);
        q.attempt.thread_id = ThreadId::from_string(&Uuid::from_u128(8).to_string())?;
        let a = &q.attempt;
        sqlx::query("INSERT INTO draft_accounting_attempts VALUES (?, ?, ?)")
            .bind(a.attempt_id.to_string())
            .bind(a.request_id.to_string())
            .bind(serde_json::to_string(a)?)
            .execute(&mut *tx)
            .await?;
        sqlx::query("INSERT INTO draft_accounting_price_bindings VALUES (?, NULL)")
            .bind(a.attempt_id.to_string())
            .execute(&mut *tx)
            .await?;
        sqlx::query("INSERT INTO draft_accounting_estimates VALUES (?, '[]', ?)")
            .bind(a.attempt_id.to_string())
            .bind(serde_json::to_string(&q)?)
            .execute(&mut *tx)
            .await?;
        sqlx::query("INSERT INTO draft_accounting_contributions VALUES (?, ?, 0, '[]')")
            .bind(a.attempt_id.to_string())
            .bind(a.thread_id.to_string())
            .execute(&mut *tx)
            .await?;
    }
    tx.commit().await?;
    assert_eq!(
        inspected(&runtime, /*day*/ 0, /*time*/ 0).await?,
        InspectionDay::TooLarge
    );
    runtime.close().await;
    Ok(())
}

fn range_buckets(value: InspectionDay) -> Vec<InspectionBucket> {
    let InspectionDay::Range { buckets, .. } = value else {
        panic!("{value:?}")
    };
    buckets
}

async fn range_read(
    runtime: &StateRuntime,
    start: i64,
    end: i64,
    grouping: InspectionGrouping,
    now: i64,
) -> anyhow::Result<InspectionDay> {
    AccountingStore::inspect_range(
        runtime,
        attempt(/*id*/ 1).thread_id,
        InspectionRange {
            start_ms: start,
            end_ms: end,
            grouping,
        },
        now,
    )
    .await
}

/// #289: a week or month bucket that reaches past the ledger reports the
/// interval it covers, as a day bucket containing today does, not
/// "unavailable".
#[tokio::test]
async fn accounting_inspect_range_bucket_reaching_past_the_ledger_states_its_coverage()
-> anyhow::Result<()> {
    let path = home();
    let runtime = open(&path).await?;
    seed(&runtime).await?;
    // The ledger is current to 12:00 on day 6 (a Wednesday); the read is at
    // the same time. Day 4 (1970-01-05) is a Monday, so one ISO week and one
    // month bucket cover the range.
    let now = 6 * DAY + DAY / 2;
    AccountingStore::open(&runtime, now).await?;
    for grouping in [InspectionGrouping::Week, InspectionGrouping::Month] {
        let buckets = range_buckets(
            range_read(
                &runtime,
                /*start*/ 4 * DAY,
                /*end*/ 11 * DAY,
                grouping,
                now,
            )
            .await?,
        );
        assert_eq!(buckets.len(), 1, "{grouping:?}");
        assert!(buckets[0].partial);
        assert_eq!(
            buckets[0].effective,
            Some((4 * DAY, now + 1)),
            "{grouping:?}"
        );
    }
    // A bucket whose first day the ledger has not reached stays unavailable.
    let later = range_buckets(
        range_read(
            &runtime,
            /*start*/ 7 * DAY,
            /*end*/ 9 * DAY,
            InspectionGrouping::Week,
            /*now*/ 9 * DAY,
        )
        .await?,
    );
    assert_eq!(later[0].effective, None);
    runtime.close().await;
    Ok(())
}

#[tokio::test]
async fn accounting_inspect_range_invalid_empty_reversed_and_unavailable() -> anyhow::Result<()> {
    let path = home();
    let runtime = open(&path).await?;
    let before = rows(&runtime).await?;
    for (start, end, reason) in [
        (2, 1, "reversed"),
        (1, 1, "empty"),
        (-1, 1, "invalid"),
        (0, i64::MAX, "overflow"),
    ] {
        assert!(
            range_read(
                &runtime,
                start,
                end,
                InspectionGrouping::Day,
                /*now*/ 10
            )
            .await
            .unwrap_err()
            .to_string()
            .contains(reason)
        );
    }
    assert_eq!(
        range_read(
            &runtime,
            /*start*/ 0,
            /*end*/ 1,
            InspectionGrouping::Hour,
            /*now*/ 10
        )
        .await?,
        InspectionDay::Absent
    );
    assert_eq!(rows(&runtime).await?, before);
    seed(&runtime).await?;
    AccountingStore::open(&runtime, /*as_of*/ 86_400_000).await?;
    let bucket = range_buckets(
        range_read(
            &runtime,
            /*start*/ 0,
            /*end*/ 3_600_000,
            InspectionGrouping::Hour,
            /*now*/ 86_400_000,
        )
        .await?,
    )
    .remove(0);
    assert!(!bucket.partial);
    assert_eq!(
        inspection(bucket.days.into_iter().next().unwrap()).totals,
        DayTotals::default()
    );
    runtime.close().await;
    Ok(())
}

#[test]
fn accounting_inspect_range_iso_week_calendar_month_boundaries() -> anyhow::Result<()> {
    let ms = |s: &str| {
        chrono::DateTime::parse_from_rfc3339(s)
            .unwrap()
            .timestamp_millis()
    };
    for (grouping, at, start, end) in [
        (
            InspectionGrouping::Week,
            "2021-01-01T12:00:00Z",
            "2020-12-28T00:00:00Z",
            "2021-01-04T00:00:00Z",
        ),
        (
            InspectionGrouping::Week,
            "2026-04-01T00:00:00Z",
            "2026-03-30T00:00:00Z",
            "2026-04-06T00:00:00Z",
        ),
        (
            InspectionGrouping::Month,
            "2024-02-29T23:00:00Z",
            "2024-02-01T00:00:00Z",
            "2024-03-01T00:00:00Z",
        ),
        (
            InspectionGrouping::Month,
            "2025-12-31T12:00:00Z",
            "2025-12-01T00:00:00Z",
            "2026-01-01T00:00:00Z",
        ),
        (
            InspectionGrouping::Hour,
            "2026-03-08T09:30:00Z",
            "2026-03-08T09:00:00Z",
            "2026-03-08T10:00:00Z",
        ),
    ] {
        assert_eq!(
            InspectionRange {
                start_ms: ms(at),
                end_ms: ms(end),
                grouping
            }
            .bucket(ms(at))?,
            (ms(start), ms(end))
        );
    }
    Ok(())
}

#[tokio::test]
async fn accounting_inspect_range_hours_half_open_and_cutoff_suffix() -> anyhow::Result<()> {
    const HOUR: i64 = 3_600_000;
    const DAY: i64 = 86_400_000;
    let path = home();
    let runtime = open(&path).await?;
    seed(&runtime).await?;
    let store = AccountingStore::open(&runtime, /*as_of*/ 0).await?;
    for (id, time) in [(1, 0), (2, HOUR), (3, 2 * HOUR - 1), (4, 2 * HOUR)] {
        let mut a = attempt(id);
        a.dispatched_at_ms = time.try_into()?;
        store.admit(a.thread_id, &a, &[snapshot()], time).await?;
        store
            .observe(a.thread_id, &a, &[row(id as i64)], time)
            .await?;
    }
    store.maintain(90 * DAY).await?;
    let before = rows(&runtime).await?;
    let buckets = range_buckets(
        range_read(&runtime, HOUR, 2 * HOUR, InspectionGrouping::Hour, 90 * DAY).await?,
    );
    assert_eq!(
        (buckets[0].partial, buckets[0].effective),
        (false, Some((HOUR, 2 * HOUR)))
    );
    let view = inspection(buckets.into_iter().next().unwrap().days.remove(0));
    assert_eq!(
        view.requests
            .values()
            .flatten()
            .map(|q| q.attempt.attempt_id)
            .collect::<Vec<_>>(),
        vec![Uuid::from_u128(2), Uuid::from_u128(3)]
    );
    assert_eq!(view.totals.known_usd, "0.000005".to_owned().try_into()?);
    let old = range_buckets(
        range_read(
            &runtime,
            /*start*/ 0,
            HOUR,
            InspectionGrouping::Hour,
            90 * DAY,
        )
        .await?,
    );
    assert!(matches!(
        old[0].days[0],
        InspectionDay::DetailUnavailable { compact: true, .. }
    ));
    assert_eq!(rows(&runtime).await?, before);
    runtime.close().await;
    Ok(())
}

#[tokio::test]
async fn accounting_inspect_range_partial_edges_and_checkpoint_coverage() -> anyhow::Result<()> {
    const DAY: i64 = 86_400_000;
    let path = home();
    let runtime = open(&path).await?;
    seed(&runtime).await?;
    let store = AccountingStore::open(&runtime, /*as_of*/ 0).await?;
    store.maintain(3 * DAY).await?;
    let buckets = range_buckets(
        range_read(
            &runtime,
            /*start*/ 1,
            3 * DAY - 1,
            InspectionGrouping::Day,
            3 * DAY,
        )
        .await?,
    );
    assert_eq!(
        buckets
            .iter()
            .map(|b| (b.start_ms, b.end_ms, b.partial, b.effective))
            .collect::<Vec<_>>(),
        vec![
            (0, DAY, true, Some((1, DAY))),
            (DAY, 2 * DAY, false, Some((DAY, 2 * DAY))),
            (2 * DAY, 3 * DAY, true, Some((2 * DAY, 3 * DAY - 1)))
        ]
    );
    let buckets = range_buckets(
        range_read(
            &runtime,
            3 * DAY,
            4 * DAY,
            InspectionGrouping::Day,
            3 * DAY + 10,
        )
        .await?,
    );
    assert_eq!(
        (buckets[0].partial, buckets[0].effective),
        (true, Some((3 * DAY, 3 * DAY + 1)))
    );
    runtime.close().await;
    Ok(())
}

#[tokio::test]
async fn accounting_inspect_range_mixed_compaction_exact_no_double_count() -> anyhow::Result<()> {
    const DAY: i64 = 86_400_000;
    let path = home();
    let runtime = open(&path).await?;
    seed(&runtime).await?;
    let store = AccountingStore::open(&runtime, /*as_of*/ 0).await?;
    for (id, time) in [(1, 0), (2, DAY)] {
        let mut a = attempt(id);
        a.dispatched_at_ms = time.try_into()?;
        store.admit(a.thread_id, &a, &[snapshot()], time).await?;
        store
            .observe(a.thread_id, &a, &[row(id as i64)], time)
            .await?;
    }
    store.maintain(90 * DAY).await?;
    let before = rows(&runtime).await?;
    let value = range_read(
        &runtime,
        /*start*/ 0,
        2 * DAY,
        InspectionGrouping::Day,
        90 * DAY,
    )
    .await?;
    assert!(
        matches!(&value, InspectionDay::Range { aggregate_day_floor: Some(0), read_at_ms, .. } if *read_at_ms == 90 * DAY)
    );
    let mut buckets = range_buckets(value);
    assert!(matches!(
        buckets[0].days[0],
        InspectionDay::DetailUnavailable { compact: true, .. }
    ));
    let raw = inspection(buckets[1].days.remove(0));
    assert_eq!(
        (
            raw.totals.attempts,
            raw.totals.unknown_estimates,
            raw.totals.known_usd
        ),
        (1, 1, "0.000002".to_owned().try_into()?)
    );
    // The store retains the compact sum, but the inspector must never misattribute it.
    let RetainedDay::Available {
        totals: Current::Ready(compact),
        ..
    } = store
        .read_day(attempt(/*id*/ 1).thread_id, /*utc_day*/ 0, 90 * DAY)
        .await?
    else {
        panic!()
    };
    assert_eq!(compact.known_usd, "0.000001".to_owned().try_into()?);
    assert_eq!(rows(&runtime).await?, before);
    runtime.close().await;
    Ok(())
}

// #289 R3: a range states the same daily-totals floor as the day pages, even
// before anything has been compacted.
#[tokio::test]
async fn accounting_inspect_range_states_day_page_retention_floor() -> anyhow::Result<()> {
    const DAY: i64 = 86_400_000;
    let path = home();
    let runtime = open(&path).await?;
    seed(&runtime).await?;
    let time = 400 * DAY;
    let store = AccountingStore::open(&runtime, time).await?;
    let mut a = attempt(/*id*/ 1);
    a.dispatched_at_ms = time.try_into()?;
    store.admit(a.thread_id, &a, &[snapshot()], time).await?;
    store
        .observe(a.thread_id, &a, &[row(/*revision*/ 1)], time)
        .await?;
    let floor = inspection(inspected(&runtime, /*day*/ 400, time).await?)
        .coverage
        .aggregate_day_floor;
    assert_eq!(floor, 36);
    let value = range_read(&runtime, time, time + DAY, InspectionGrouping::Day, time).await?;
    assert!(
        matches!(value, InspectionDay::Range { aggregate_day_floor: Some(f), .. } if f == floor),
        "{value:?}"
    );
    runtime.close().await;
    Ok(())
}

#[tokio::test]
async fn accounting_inspect_range_week_day_slices_and_expired_prefix() -> anyhow::Result<()> {
    const DAY: i64 = 86_400_000;
    let path = home();
    let runtime = open(&path).await?;
    seed(&runtime).await?;
    let store = AccountingStore::open(&runtime, /*as_of*/ 0).await?;
    let mut expected = Vec::new();
    for (id, time) in [(1, 4 * DAY + 1), (2, 5 * DAY + 1)] {
        let mut a = attempt(id);
        a.dispatched_at_ms = time.try_into()?;
        store.admit(a.thread_id, &a, &[snapshot()], time).await?;
        let q = store
            .observe(a.thread_id, &a, &[row(id as i64)], time)
            .await?;
        expected.push(std::collections::BTreeMap::from([(a.request_id, vec![q])]));
    }
    // Monday 1970-01-05 through the following Monday, exclusively.
    store.maintain(11 * DAY).await?;
    for now in [11 * DAY, 94 * DAY + DAY / 2] {
        store.maintain(now).await?;
        let before = rows(&runtime).await?;
        let buckets = range_buckets(
            range_read(&runtime, 4 * DAY, 11 * DAY, InspectionGrouping::Week, now).await?,
        );
        assert_eq!(buckets.len(), 1);
        let bucket = &buckets[0];
        assert_eq!(
            (
                bucket.start_ms,
                bucket.end_ms,
                bucket.effective,
                bucket.partial
            ),
            (4 * DAY, 11 * DAY, Some((4 * DAY, 11 * DAY)), false)
        );
        assert_eq!(bucket.days.len(), 7);
        for (index, day) in bucket.days.iter().enumerate() {
            if now > 11 * DAY && index == 0 {
                assert!(matches!(
                    day,
                    InspectionDay::DetailUnavailable { compact: true, coverage, .. }
                        if coverage.detail_expired_through_ms == Some(4 * DAY + DAY / 2)
                ));
                continue;
            }
            let InspectionDay::Ready(view) = day else {
                panic!("{day:?}")
            };
            assert_eq!(view.utc_day, 4 + index as i64);
            let requests = expected.get(index).cloned().unwrap_or_default();
            assert_eq!(
                view.totals,
                DayTotals::from_quotes(requests.values().flatten())?
            );
            assert_eq!(view.requests, requests);
        }
        assert_eq!(rows(&runtime).await?, before);
    }
    // Aggregate expiry clips effective coverage even though the bucket spans days.
    let now = 370 * DAY;
    store.maintain(now).await?;
    let buckets = range_buckets(
        range_read(&runtime, 4 * DAY, 11 * DAY, InspectionGrouping::Week, now).await?,
    );
    assert_eq!(
        (
            buckets[0].days.len(),
            buckets[0].effective,
            buckets[0].partial
        ),
        (7, Some((6 * DAY, 11 * DAY)), true)
    );
    // The two recorded days are past the daily totals. #1196: the five empty
    // days inside them read as the day view reads them, never as zero.
    for (index, day) in buckets[0].days.iter().enumerate() {
        if index < 2 {
            assert!(
                matches!(day, InspectionDay::DetailUnavailable { .. }),
                "{day:?}"
            );
            continue;
        }
        let InspectionDay::Ready(view) = day else {
            panic!("{day:?}")
        };
        assert_eq!((view.utc_day, view.totals.attempts), (4 + index as i64, 0));
        assert!(view.requests.is_empty());
        assert!(
            matches!(view.empty_day, Some(EmptyDay::BeforeLedger { .. })),
            "{view:?}"
        );
    }
    runtime.close().await;
    Ok(())
}

#[tokio::test]
async fn accounting_inspect_range_tree_unknown_and_single_snapshot() -> anyhow::Result<()> {
    const HOUR: i64 = 3_600_000;
    let path = home();
    let runtime = Arc::new(open(&path).await?);
    tree_fixture(&runtime).await?;
    AccountingStore::open(&runtime, 2 * HOUR).await?;
    let requested = InspectionRange {
        start_ms: 0,
        end_ms: 2 * HOUR,
        grouping: InspectionGrouping::Hour,
    };
    let mut tx = runtime.pool.begin().await?;
    // Pin exactly the same snapshot that the public entrypoint owns.
    validate_on_connection(&mut tx).await?;
    let writer = runtime.clone();
    tokio::spawn(async move { AccountingStore::open(&writer, 3 * HOUR).await.map(|_| ()) })
        .await??;
    let mut buckets = range_buckets(
        inspect_buckets(&mut tx, attempt(/*id*/ 1).thread_id, requested, 3 * HOUR).await?,
    );
    let view = inspection(buckets[0].days.remove(0));
    assert_eq!(view.coverage.completed_as_of_ms, 2 * HOUR);
    assert_eq!(
        (
            view.totals.attempts,
            view.own_totals.attempts,
            view.descendant_totals.attempts,
            view.unknown_parent_totals.attempts
        ),
        (3, 1, 2, 3)
    );
    assert_eq!(view.totals.known_usd, "0.000006".to_owned().try_into()?);
    assert_eq!(
        view.unknown_parent_totals.known_usd,
        "0.000017".to_owned().try_into()?
    );
    assert_eq!(
        inspection(buckets[1].days.remove(0)).totals,
        DayTotals::default()
    );
    tx.commit().await?;
    runtime.close().await;
    Ok(())
}

fn home() -> impl std::ops::Deref<Target = std::path::PathBuf> {
    scopeguard::guard(crate::runtime::test_support::unique_temp_dir(), |path| {
        let _ = std::fs::remove_dir_all(path);
    })
}

fn attempt(id: u128) -> Attempt {
    serde_json::from_value(json!({
        "attempt_id":Uuid::from_u128(id), "request_id":Uuid::from_u128(id + 100),
        "thread_id":Uuid::from_u128(7), "turn":"fixture", "retry_of":null,
        "provider":"synthetic", "model":"fixture", "scope":Uuid::nil(),
        "dialect":"NativeAnthropic", "dispatched_at_ms":0
    }))
    .unwrap()
}

fn snapshot() -> Snapshot {
    serde_json::from_value(json!({
        "id":Uuid::from_u128(1000), "provider":"synthetic", "model":"fixture",
        "scope":Uuid::nil(), "currency":"USD", "unit":"PerMillionTokens",
        "rates":{"noncached":"1","read":null,"write":null,"output":null},
        "source_reference":Uuid::from_u128(1001), "source_kind":"ProviderPublished",
        "observed_at_ms":0,"approved_at_ms":0,"effective_from_ms":0,"effective_end_ms":null
    }))
    .unwrap()
}

fn row(revision: i64) -> Observation {
    serde_json::from_value(json!({
        "revision":revision,"source":Uuid::from_u128(1),
        "sequence":revision,"patch":{"input":revision}
    }))
    .unwrap()
}

async fn open(path: &Path) -> anyhow::Result<Arc<StateRuntime>> {
    StateRuntime::init_for_testing(path.to_path_buf(), "synthetic".into()).await
}

async fn seed(runtime: &StateRuntime) -> anyhow::Result<()> {
    let path = runtime.sqlite().home();
    let owner = attempt(/*id*/ 1).thread_id;
    runtime
        .upsert_thread(&test_thread_metadata(path, owner, path.to_path_buf()))
        .await?;
    sqlx::query("INSERT INTO thread_dynamic_tools (thread_id, position, name, description, input_schema) VALUES (?, 0, 'fixture', 'synthetic', '{}')")
        .bind(owner.to_string()).execute(runtime.pool.as_ref()).await?;
    runtime
        .upsert_thread_spawn_edge(
            owner,
            ThreadId::new(),
            crate::DirectionalThreadSpawnEdgeStatus::Closed,
        )
        .await?;
    AccountingStore::open(runtime, /*as_of*/ 0).await?;
    Ok(())
}

// Every column of all ten accounting and three native tables plus both ledgers.
// The caller holds one snapshot; absence is explicit for installation failures.
async fn whole(conn: &mut SqliteConnection) -> anyhow::Result<Vec<Vec<String>>> {
    let mut result = Vec::new();
    for table in [
        "draft_accounting_attempts",
        "draft_accounting_observations",
        "draft_accounting_price_snapshots",
        "draft_accounting_price_bindings",
        "draft_accounting_estimates",
        "draft_accounting_contributions",
        "draft_accounting_tombstones",
        "draft_accounting_compact_days",
        "draft_accounting_compact_snapshots",
        "draft_accounting_retention_checkpoint",
        "threads",
        "thread_spawn_edges",
        "thread_dynamic_tools",
        "_accounting_migrations",
        "_sqlx_migrations",
    ] {
        let columns: Vec<String> =
            sqlx::query_scalar("SELECT name FROM pragma_table_info(?) ORDER BY cid")
                .bind(table)
                .fetch_all(&mut *conn)
                .await?;
        if columns.is_empty() {
            result.push(vec!["<absent>".into()]);
            continue;
        }
        let columns = columns
            .iter()
            .map(|name| {
                format!(
                    "CASE WHEN typeof(\"{name}\") = 'blob' THEN hex(\"{name}\") ELSE \"{name}\" END"
                )
            })
            .collect::<Vec<_>>()
            .join(",");
        result.push(
            sqlx::query_scalar(sqlx::AssertSqlSafe(format!(
                "SELECT json_array({columns}) FROM {table} ORDER BY 1"
            )))
            .fetch_all(&mut *conn)
            .await?,
        );
    }
    Ok(result)
}

async fn rows(runtime: &StateRuntime) -> anyhow::Result<Vec<Vec<String>>> {
    let mut tx = runtime.pool.begin().await?;
    let value = whole(&mut tx).await?;
    tx.commit().await?;
    Ok(value)
}

async fn reopens(path: &Path, expected: &[Vec<String>]) -> anyhow::Result<()> {
    for _ in 0..2 {
        let runtime = open(path).await?;
        assert_eq!(rows(&runtime).await?, expected);
        runtime.close().await;
        assert!(runtime.pool.is_closed());
    }
    Ok(())
}

fn marker(error: anyhow::Error, expected: &str) {
    assert!(
        format!("{error:#}").contains(expected),
        "expected {expected}: {error:#}"
    );
}

#[tokio::test]
async fn store_sql_faults_roll_back_all_tables_then_two_reopens_and_retry() -> anyhow::Result<()> {
    for (event, predicate, observing) in [
        (
            "INSERT ON draft_accounting_attempts",
            "NEW.attempt_id IS NOT NULL AND (SELECT completed_as_of_ms FROM draft_accounting_retention_checkpoint) = 1",
            false,
        ),
        (
            "INSERT ON draft_accounting_price_snapshots",
            "EXISTS(SELECT 1 FROM draft_accounting_attempts)",
            false,
        ),
        (
            "INSERT ON draft_accounting_price_bindings",
            "EXISTS(SELECT 1 FROM draft_accounting_price_snapshots)",
            false,
        ),
        (
            "INSERT ON draft_accounting_estimates",
            "EXISTS(SELECT 1 FROM draft_accounting_price_bindings)",
            false,
        ),
        (
            "INSERT ON draft_accounting_contributions",
            "EXISTS(SELECT 1 FROM draft_accounting_estimates)",
            false,
        ),
        (
            "INSERT ON draft_accounting_observations",
            "(SELECT completed_as_of_ms FROM draft_accounting_retention_checkpoint) = 1",
            true,
        ),
        (
            "UPDATE ON draft_accounting_contributions",
            "(SELECT count(*) FROM draft_accounting_estimates) = 2",
            true,
        ),
    ] {
        let path = home();
        let runtime = open(&path).await?;
        seed(&runtime).await?;
        let a = attempt(/*id*/ 1);
        let store = AccountingStore::open(&runtime, /*as_of*/ 0).await?;
        if observing {
            store
                .admit(a.thread_id, &a, &[snapshot()], /*as_of*/ 0)
                .await?;
        }
        let before = rows(&runtime).await?;
        sqlx::raw_sql(sqlx::AssertSqlSafe(format!(
            "CREATE TRIGGER store_fault BEFORE {event} WHEN {predicate} BEGIN SELECT RAISE(ABORT, 'store-exact-fault'); END"
        ))).execute(runtime.pool.as_ref()).await?;
        let error = if observing {
            store
                .observe(a.thread_id, &a, &[row(/*revision*/ 1)], /*as_of*/ 1)
                .await
                .unwrap_err()
        } else {
            store
                .admit(a.thread_id, &a, &[snapshot()], /*as_of*/ 1)
                .await
                .unwrap_err()
        };
        marker(error, "store-exact-fault");
        assert_eq!(rows(&runtime).await?, before);
        runtime.close().await;
        reopens(&path, &before).await?;
        let runtime = open(&path).await?;
        sqlx::raw_sql("DROP TRIGGER store_fault")
            .execute(runtime.pool.as_ref())
            .await?;
        let store = AccountingStore::open(&runtime, /*as_of*/ 0).await?;
        let quote = if observing {
            store
                .observe(a.thread_id, &a, &[row(/*revision*/ 1)], /*as_of*/ 1)
                .await?
        } else {
            store
                .admit(a.thread_id, &a, &[snapshot()], /*as_of*/ 1)
                .await?
        };
        assert_eq!(quote.snapshot, Some(snapshot()));
        assert_eq!(quote.usage.noncached, observing.then_some(1));
        let success = rows(&runtime).await?;
        assert_eq!(success[0].len(), 1);
        assert_eq!(success[3].len(), 1);
        assert_eq!(success[5].len(), 1);
        assert_eq!(&success[10..], &before[10..]);
        runtime.close().await;
        reopens(&path, &success).await?;
    }
    Ok(())
}

#[tokio::test]
async fn store_commit_failure_is_not_admission_and_rolls_back_complete_state() -> anyhow::Result<()>
{
    let path = home();
    let runtime = open(&path).await?;
    seed(&runtime).await?;
    let before = rows(&runtime).await?;
    // Post-write accounting validation rejects accounting orphans before COMMIT.
    // A native FK fault still exercises COMMIT failure after the complete store body.
    sqlx::raw_sql(
        "CREATE TRIGGER store_commit AFTER INSERT ON draft_accounting_contributions BEGIN
         INSERT INTO thread_dynamic_tools (thread_id, position, name, description, input_schema)
         VALUES ('orphan', 0, 'fixture', 'synthetic', '{}'); END",
    )
    .execute(runtime.pool.as_ref())
    .await?;
    let mut tx = runtime.pool.begin_with("BEGIN IMMEDIATE").await?;
    sqlx::raw_sql("PRAGMA defer_foreign_keys = ON")
        .execute(&mut *tx)
        .await?;
    let a = attempt(/*id*/ 1);
    Journal::store_on_connection(
        &mut tx,
        a.thread_id,
        &a,
        &[],
        Some(&[snapshot()]),
        /*as_of_ms*/ 1,
        /*validated_at_ms*/ None,
    )
    .await?;
    let error = tx.commit().await.unwrap_err();
    assert_eq!(
        error
            .as_database_error()
            .and_then(sqlx::error::DatabaseError::code)
            .as_deref(),
        Some("787")
    );
    assert!(error.to_string().contains("FOREIGN KEY"));
    assert_eq!(rows(&runtime).await?, before);
    runtime.close().await;
    reopens(&path, &before).await?;
    let runtime = open(&path).await?;
    sqlx::raw_sql("DROP TRIGGER store_commit")
        .execute(runtime.pool.as_ref())
        .await?;
    AccountingStore::open(&runtime, /*as_of*/ 0)
        .await?
        .admit(a.thread_id, &a, &[snapshot()], /*as_of*/ 1)
        .await?;
    let success = rows(&runtime).await?;
    runtime.close().await;
    reopens(&path, &success).await
}

#[tokio::test]
async fn store_rejections_preserve_clock_native_rows_and_original_binding() -> anyhow::Result<()> {
    let path = home();
    let runtime = open(&path).await?;
    seed(&runtime).await?;
    let a = attempt(/*id*/ 1);
    let store = AccountingStore::open(&runtime, /*as_of*/ 0).await?;
    marker(
        store
            .observe(a.thread_id, &a, &[row(/*revision*/ 1)], /*as_of*/ 1)
            .await
            .unwrap_err(),
        "not admitted",
    );
    store
        .admit(a.thread_id, &a, &[snapshot()], /*as_of*/ 0)
        .await?;
    let before = rows(&runtime).await?;
    marker(
        store
            .admit(ThreadId::new(), &a, &[], /*as_of*/ 1)
            .await
            .unwrap_err(),
        "owner mismatch",
    );
    let mut missing = attempt(/*id*/ 2);
    missing.thread_id = ThreadId::new();
    marker(
        store
            .admit(missing.thread_id, &missing, &[], /*as_of*/ 1)
            .await
            .unwrap_err(),
        "owner missing",
    );
    let mut price = snapshot();
    price.model = "conflict".into();
    marker(
        store
            .admit(a.thread_id, &a, &[price], /*as_of*/ 1)
            .await
            .unwrap_err(),
        "snapshot conflict",
    );
    marker(
        store
            .admit(a.thread_id, &a, &[], /*as_of*/ -1)
            .await
            .unwrap_err(),
        "negative",
    );
    assert_eq!(rows(&runtime).await?, before);
    // Retention applies what is due in its own short transactions before the
    // write's, so a write rejected once detail expired leaves that expiry - and
    // nothing of its own: the checkpoint stays until a write completes.
    marker(
        store
            .admit(a.thread_id, &a, &[], 90 * 86_400_000)
            .await
            .unwrap_err(),
        "compact-only late import",
    );
    let after = rows(&runtime).await?;
    let counts: Vec<usize> = after[..9].iter().map(Vec::len).collect();
    // Attempt detail is gone, its price kept by the compact day it folded into,
    // and a tombstone guards its replay window.
    assert_eq!(counts, [0, 0, 1, 0, 0, 0, 1, 1, 1]);
    assert_eq!(after[2], before[2]);
    assert_eq!(
        after[9..],
        before[9..],
        "checkpoint and native rows unchanged"
    );
    runtime.close().await;
    reopens(&path, &after).await
}

async fn peer(runtime: &StateRuntime) -> anyhow::Result<StateRuntime> {
    let mut peer = runtime.clone();
    peer.pool = Arc::new(
        crate::sqlite::open_pool_for_testing(
            sqlx::sqlite::SqlitePoolOptions::new().max_connections(1),
            sqlx::sqlite::SqliteConnectOptions::new()
                .filename(runtime.sqlite().state_db_path())
                .foreign_keys(true)
                .busy_timeout(std::time::Duration::ZERO),
        )
        .await?,
    );
    Ok(peer)
}

#[tokio::test]
async fn public_deletion_samples_time_after_cleanup_and_writer_acquisition() -> anyhow::Result<()> {
    // Pause A at each await boundary where sampling the production time is too early.
    for pause_at_writer in [false, true] {
        let path = home();
        let runtime = open(&path).await?;
        seed(&runtime).await?;
        let mut a = attempt(/*id*/ 1);
        let mut b = attempt(/*id*/ 2);
        b.thread_id = ThreadId::new();
        let unrelated = ThreadId::new();
        for owner in [b.thread_id, unrelated] {
            runtime
                .upsert_thread(&test_thread_metadata(&path, owner, path.to_path_buf()))
                .await?;
        }
        let initial_time = chrono::Utc::now().timestamp_millis();
        a.dispatched_at_ms = Count::try_from(initial_time)?;
        b.dispatched_at_ms = Count::try_from(initial_time)?;
        let store = AccountingStore::open(&runtime, initial_time).await?;
        for attempt in [&a, &b] {
            store
                .admit(attempt.thread_id, attempt, &[], initial_time)
                .await?;
        }
        let before = rows(&runtime).await?;
        let mut delayed = runtime.as_ref().clone();
        let (held_tx, held_rx) = tokio::sync::oneshot::channel();
        let held_tx = Arc::new(std::sync::Mutex::new(Some(held_tx)));
        let release = Arc::new(tokio::sync::Notify::new());
        let gate = Arc::clone(&release);
        let options = if pause_at_writer {
            runtime.pool.connect_options()
        } else {
            runtime.logs_pool.connect_options()
        };
        // Pool checkout is a controlled async barrier, not a production clock hook.
        let blocked_pool = Arc::new(
            crate::sqlite::open_pool_for_testing(
                sqlx::sqlite::SqlitePoolOptions::new()
                    .max_connections(1)
                    .before_acquire(move |_, _| {
                        let held = held_tx.lock().unwrap().take();
                        let gate = Arc::clone(&gate);
                        Box::pin(async move {
                            if let Some(held) = held {
                                held.send(()).unwrap();
                                gate.notified().await;
                            }
                            Ok(true)
                        })
                    }),
                options.as_ref().clone(),
            )
            .await?,
        );
        if pause_at_writer {
            delayed.pool = Arc::clone(&blocked_pool);
        } else {
            delayed.logs_pool = Arc::clone(&blocked_pool);
        }
        let a_owner = a.thread_id;
        let deletion = tokio::spawn(async move { delayed.delete_threads_strict(&[a_owner]).await });
        tokio::time::timeout(std::time::Duration::from_secs(5), held_rx).await??;
        let paused_at = chrono::Utc::now().timestamp_millis();
        tokio::time::timeout(std::time::Duration::from_secs(5), async {
            while chrono::Utc::now().timestamp_millis() <= paused_at {
                tokio::time::sleep(std::time::Duration::from_millis(1)).await;
            }
        })
        .await?;
        let other = peer(&runtime).await?;
        assert_eq!(other.delete_thread(b.thread_id).await?, 1);
        let after_b = rows(&runtime).await?;
        let checkpoint: i64 = sqlx::query_scalar(
            "SELECT completed_as_of_ms FROM draft_accounting_retention_checkpoint",
        )
        .fetch_one(runtime.pool.as_ref())
        .await?;
        assert!(checkpoint > paused_at);
        assert!(runtime.get_thread(a.thread_id).await?.is_some());
        assert!(runtime.get_thread(b.thread_id).await?.is_none());
        // Explicit clocks remain strict: never max/coerce them to the checkpoint.
        marker(
            other
                .delete_threads_at(&[unrelated], checkpoint - 1)
                .await
                .unwrap_err(),
            "negative or backward checkpoint",
        );
        assert_eq!(rows(&runtime).await?, after_b);
        release.notify_one();
        assert_eq!(
            tokio::time::timeout(std::time::Duration::from_secs(5), deletion).await???,
            1
        );
        let final_time: i64 = sqlx::query_scalar(
            "SELECT completed_as_of_ms FROM draft_accounting_retention_checkpoint",
        )
        .fetch_one(runtime.pool.as_ref())
        .await?;
        assert!(final_time >= checkpoint);
        let after = rows(&runtime).await?;
        let mut expected = before;
        for index in [0, 1, 2, 3, 4, 5, 7, 8, 11, 12] {
            expected[index].clear();
        }
        expected[6] = [&a, &b]
            .into_iter()
            .map(|attempt| {
                json!([
                    attempt.attempt_id.to_string(),
                    initial_time + 365 * 86_400_000_i64
                ])
                .to_string()
            })
            .collect();
        expected[6].sort();
        expected[9] = vec![json!([1, final_time, 1]).to_string()];
        expected[10].retain(|row| row.contains(&unrelated.to_string()));
        assert_eq!(after, expected);
        blocked_pool.close().await;
        other.pool.close().await;
        runtime.close().await;
        reopens(&path, &after).await?;
    }
    Ok(())
}

#[tokio::test]
async fn absent_store_delete_locks_before_inspection_and_ordinary_writer() -> anyhow::Result<()> {
    let path = home();
    let runtime = open(&path).await?;
    let owner = attempt(/*id*/ 1).thread_id;
    let unrelated = ThreadId::new();
    for id in [owner, unrelated] {
        runtime
            .upsert_thread(&test_thread_metadata(&path, id, path.to_path_buf()))
            .await?;
    }
    let other = peer(&runtime).await?;
    let mut deletion = super::super::native::begin_delete(&runtime.pool).await?;
    let before = whole(&mut deletion).await?;
    assert_eq!(before[0], vec!["<absent>".to_owned()]);
    let write = "UPDATE threads SET title = 'concurrent ordinary writer' WHERE id = ?";
    // A real second connection must not commit between inspection and DELETE.
    // A deferred read transaction permits this commit and then cannot upgrade.
    let error = sqlx::query(write)
        .bind(unrelated.to_string())
        .execute(other.pool.as_ref())
        .await
        .unwrap_err();
    assert_eq!(
        error.as_database_error().unwrap().code().as_deref(),
        Some("5")
    );
    let error = AccountingStore::open(&other, /*as_of*/ 0)
        .await
        .err()
        .unwrap();
    assert_eq!(
        error
            .downcast_ref::<sqlx::Error>()
            .unwrap()
            .as_database_error()
            .unwrap()
            .code()
            .as_deref(),
        Some("5")
    );
    assert_eq!(whole(&mut deletion).await?, before);
    assert_eq!(
        StateRuntime::delete_threads_on_connection(
            &mut deletion,
            &[owner],
            /*as_of_ms*/ 0,
            /*validated_at_ms*/ None
        )
        .await?,
        1
    );
    deletion.commit().await?;
    assert_eq!(
        sqlx::query(write)
            .bind(unrelated.to_string())
            .execute(other.pool.as_ref())
            .await?
            .rows_affected(),
        1
    );
    // Public deletion remains usable after the ordinary writer commits, without installation.
    assert_eq!(runtime.delete_thread(unrelated).await?, 1);
    let after = rows(&runtime).await?;
    assert!(after[10].is_empty());
    for index in (0..10).chain([13]) {
        assert_eq!(after[index], vec!["<absent>".to_owned()]);
    }
    other.pool.close().await;
    runtime.close().await;
    reopens(&path, &after).await
}

#[tokio::test]
async fn resultant_day_overflow_rejects_observation_without_poisoning_native_state()
-> anyhow::Result<()> {
    let path = home();
    let runtime = open(&path).await?;
    seed(&runtime).await?;
    let unrelated = ThreadId::new();
    runtime
        .upsert_thread(&test_thread_metadata(&path, unrelated, path.to_path_buf()))
        .await?;
    let a = attempt(/*id*/ 1);
    let b = attempt(/*id*/ 2);
    let store = AccountingStore::open(&runtime, /*as_of*/ 0).await?;
    for attempt in [&a, &b] {
        store.admit(a.thread_id, attempt, &[], /*as_of*/ 0).await?;
    }
    let mut maximum = row(/*revision*/ 1);
    maximum.patch.input = Presence::Number(Count::try_from(i64::MAX)?);
    store
        .observe(a.thread_id, &a, &[maximum], /*as_of*/ 1)
        .await?;
    let total = store
        .read_day(a.thread_id, /*utc_day*/ 0, /*as_of_ms*/ 1)
        .await?;
    let RetainedDay::Available {
        totals: Current::Ready(ref totals),
        ..
    } = total
    else {
        panic!("fresh exact totals expected");
    };
    assert_eq!(
        totals.measured[1],
        Metric {
            known: i64::MAX,
            unknown: 1
        }
    );
    assert_eq!(totals.unknown_estimates, 2);
    let before = rows(&runtime).await?;
    let mut one = row(/*revision*/ 1);
    one.source = Uuid::from_u128(2);
    marker(
        store
            .observe(b.thread_id, &b, &[one], /*as_of*/ 2)
            .await
            .unwrap_err(),
        "metric overflow",
    );
    assert_eq!(rows(&runtime).await?, before);
    assert_eq!(
        store
            .read_day(a.thread_id, /*utc_day*/ 0, /*as_of_ms*/ 1)
            .await?,
        total
    );
    runtime.close().await;
    reopens(&path, &before).await?;
    let runtime = open(&path).await?;
    let store = AccountingStore::open(&runtime, /*as_of*/ 1).await?;
    assert_eq!(
        store
            .read_day(a.thread_id, /*utc_day*/ 0, /*as_of_ms*/ 1)
            .await?,
        total
    );
    assert_eq!(
        runtime
            .delete_threads_at(&[unrelated], /*as_of_ms*/ 1)
            .await?,
        1
    );
    assert_eq!(
        store
            .read_day(a.thread_id, /*utc_day*/ 0, /*as_of_ms*/ 1)
            .await?,
        total
    );
    let after = rows(&runtime).await?;
    let mut expected = before;
    expected[10].retain(|row| !row.contains(&unrelated.to_string()));
    assert_eq!(after, expected);
    runtime.close().await;
    reopens(&path, &after).await
}

#[tokio::test]
async fn price_bound_admission_and_native_delete_contend_in_both_orders_with_snapshots()
-> anyhow::Result<()> {
    for delete_first in [false, true] {
        let path = home();
        let runtime = open(&path).await?;
        seed(&runtime).await?;
        let other = peer(&runtime).await?;
        let a = attempt(/*id*/ 1);
        AccountingStore::open(&runtime, /*as_of*/ 0)
            .await?
            .admit(a.thread_id, &a, &[snapshot()], /*as_of*/ 0)
            .await?;
        let a = attempt(/*id*/ 2);
        let before = rows(&runtime).await?;
        let mut read = runtime.pool.begin().await?;
        assert_eq!(whole(&mut read).await?, before);
        let (held_tx, held_rx) = tokio::sync::oneshot::channel();
        let (release_tx, release_rx) = tokio::sync::oneshot::channel();
        let writer = Arc::clone(&runtime);
        let task = tokio::spawn(async move {
            let mut tx = writer.pool.begin_with("BEGIN IMMEDIATE").await?;
            let a = attempt(/*id*/ 2);
            if delete_first {
                assert_eq!(
                    StateRuntime::delete_threads_on_connection(
                        &mut tx,
                        &[a.thread_id],
                        /*as_of_ms*/ 1,
                        /*validated_at_ms*/ None,
                    )
                    .await?,
                    1
                );
            } else {
                Journal::store_on_connection(
                    &mut tx,
                    a.thread_id,
                    &a,
                    &[],
                    Some(&[snapshot()]),
                    /*as_of_ms*/ 1,
                    /*validated_at_ms*/ None,
                )
                .await?;
            }
            held_tx.send(()).unwrap();
            release_rx.await?;
            tx.commit().await?;
            anyhow::Ok(())
        });
        held_rx.await?;
        let error = if delete_first {
            AccountingStore { runtime: &other }
                .admit(a.thread_id, &a, &[snapshot()], /*as_of*/ 1)
                .await
                .unwrap_err()
        } else {
            other
                .delete_threads_at(&[a.thread_id], /*as_of_ms*/ 1)
                .await
                .unwrap_err()
        };
        let sql = error
            .downcast_ref::<sqlx::Error>()
            .expect("actual SQLite contention");
        assert!(matches!(
            sql.as_database_error()
                .and_then(sqlx::error::DatabaseError::code)
                .as_deref(),
            Some("5" | "6" | "517")
        ));
        assert_eq!(whole(&mut read).await?, before);
        release_tx.send(()).unwrap();
        task.await??;
        assert_eq!(whole(&mut read).await?, before);
        if !delete_first {
            assert_eq!(
                other
                    .delete_threads_at(&[a.thread_id], /*as_of_ms*/ 1)
                    .await?,
                1
            );
        }
        assert_eq!(whole(&mut read).await?, before);
        read.commit().await?;
        let after = rows(&runtime).await?;
        let mut expected = before.clone();
        for index in [0, 1, 2, 3, 4, 5, 7, 8, 10, 11, 12] {
            expected[index].clear();
        }
        expected[6] = vec![
            json!([
                attempt(/*id*/ 1).attempt_id.to_string(),
                365 * 86_400_000_i64
            ])
            .to_string(),
        ];
        if !delete_first {
            expected[6].push(json!([a.attempt_id.to_string(), 365 * 86_400_000_i64]).to_string());
        }
        expected[9] = vec!["[1,1,1]".into()];
        assert_eq!(after, expected);
        for retry in [a.clone(), attempt(/*id*/ 3)] {
            marker(
                AccountingStore { runtime: &other }
                    .admit(retry.thread_id, &retry, &[snapshot()], /*as_of*/ 1)
                    .await
                    .unwrap_err(),
                "owner missing",
            );
        }
        assert_eq!(rows(&runtime).await?, after);
        other.pool.close().await;
        runtime.close().await;
        reopens(&path, &after).await?;
    }
    Ok(())
}

#[test]
fn process_interruption_recovers_install_and_activation() -> anyhow::Result<()> {
    const CHILD_HOME: &str = "ACCOUNTING_STORE_INTERRUPT_HOME";
    const CHILD_PHASE: &str = "ACCOUNTING_STORE_INTERRUPT_PHASE";
    let executor = tokio::runtime::Runtime::new()?;
    if let Some(path) = std::env::var_os(CHILD_HOME) {
        let phase = std::env::var(CHILD_PHASE)?;
        return executor.block_on(async {
            let runtime = open(Path::new(&path)).await?;
            let mut tx = runtime.pool.begin_with("BEGIN IMMEDIATE").await?;
            install_on_connection(&mut tx).await?;
            if phase == "activation" {
                tx.commit().await?;
                tx = runtime.pool.begin_with("BEGIN IMMEDIATE").await?;
                Journal::maintain_native_on_connection(&mut tx, /*as_of_ms*/ 1).await?;
            }
            use std::io::Write;
            println!("ACCOUNTING_TRANSACTION_HELD");
            std::io::stdout().flush()?;
            let mut release = String::new();
            std::io::stdin().read_line(&mut release)?;
            anyhow::bail!("parent must kill the process before transaction completion")
        });
    }
    for phase in ["installation", "activation"] {
        let path = home();
        executor.block_on(async {
            let runtime = open(&path).await?;
            runtime.close().await;
            anyhow::Ok(())
        })?;
        let child = std::process::Command::new(std::env::current_exe()?)
            .args(["--exact", "runtime::accounting::store::tests::process_interruption_recovers_install_and_activation", "--nocapture"])
            .env(CHILD_HOME, path.as_os_str()).env(CHILD_PHASE, phase)
            .stdin(std::process::Stdio::piped()).stdout(std::process::Stdio::piped()).spawn()?;
        let mut child = scopeguard::guard(child, |mut child| {
            let _ = child.kill();
            let _ = child.wait();
        });
        let stdout = child.stdout.take().unwrap();
        let (send, receive) = std::sync::mpsc::channel();
        let reader = std::thread::spawn(move || {
            use std::io::BufRead;
            for line in std::io::BufReader::new(stdout).lines() {
                if line.unwrap().contains("ACCOUNTING_TRANSACTION_HELD") {
                    let _ = send.send(());
                    break;
                }
            }
        });
        receive.recv_timeout(std::time::Duration::from_secs(20))?;
        reader.join().unwrap();
        child.kill()?;
        assert!(!child.wait()?.success());
        executor.block_on(async {
            for _ in 0..2 {
                let runtime = open(&path).await?;
                let mut conn = runtime.pool.acquire().await?;
                if phase == "installation" {
                    assert!(!ledger_exists(&mut conn).await?);
                    assert_eq!(sqlx::query_scalar::<_, i64>(
                        "SELECT count(*) FROM sqlite_schema WHERE name GLOB 'draft_accounting_*'",
                    ).fetch_one(&mut *conn).await?, 0);
                } else {
                    validate_on_connection(&mut conn).await?;
                    assert_eq!(sqlx::query_as::<_, (Option<i64>, i64)>(
                        "SELECT completed_as_of_ms, admission_active FROM draft_accounting_retention_checkpoint",
                    ).fetch_one(&mut *conn).await?, (None, 0));
                }
                drop(conn);
                runtime.close().await;
            }
            let runtime = open(&path).await?;
            let store = AccountingStore::open(&runtime, /*as_of*/ 1).await?;
            let a = attempt(/*id*/ 1);
            let path = runtime.sqlite().home();
            runtime.upsert_thread(&test_thread_metadata(path, a.thread_id, path.to_path_buf())).await?;
            store.admit(a.thread_id, &a, &[snapshot()], /*as_of*/ 1).await?;
            let success = rows(&runtime).await?;
            runtime.close().await;
            reopens(path, &success).await
        })?;
    }
    Ok(())
}

#[tokio::test]
async fn inactive_activation_and_installed_native_delete_failures_are_retryable()
-> anyhow::Result<()> {
    for deleting in [false, true] {
        let path = home();
        let runtime = open(&path).await?;
        let a = attempt(/*id*/ 1);
        if deleting {
            seed(&runtime).await?;
            AccountingStore::open(&runtime, /*as_of*/ 0)
                .await?
                .admit(a.thread_id, &a, &[snapshot()], /*as_of*/ 0)
                .await?;
        } else {
            let mut tx = runtime.pool.begin_with("BEGIN IMMEDIATE").await?;
            install_on_connection(&mut tx).await?;
            tx.commit().await?;
        }
        let before = rows(&runtime).await?;
        let trigger = if deleting {
            "CREATE TRIGGER store_boundary BEFORE DELETE ON threads
             WHEN NOT EXISTS(SELECT 1 FROM draft_accounting_attempts)
             AND (SELECT completed_as_of_ms FROM draft_accounting_retention_checkpoint) = 1
             BEGIN SELECT RAISE(ABORT, 'after-accounting-delete'); END"
        } else {
            "CREATE TRIGGER store_boundary BEFORE UPDATE ON draft_accounting_retention_checkpoint
             BEGIN SELECT RAISE(ABORT, 'activation-checkpoint'); END"
        };
        sqlx::raw_sql(trigger)
            .execute(runtime.pool.as_ref())
            .await?;
        if deleting {
            marker(
                runtime
                    .delete_threads_at(&[a.thread_id], /*as_of_ms*/ 1)
                    .await
                    .unwrap_err(),
                "after-accounting-delete",
            );
        } else {
            marker(
                AccountingStore::open(&runtime, /*as_of*/ 1)
                    .await
                    .err()
                    .expect("activation fails"),
                "activation-checkpoint",
            );
        }
        assert_eq!(rows(&runtime).await?, before);
        runtime.close().await;
        reopens(&path, &before).await?;
        let runtime = open(&path).await?;
        sqlx::raw_sql("DROP TRIGGER store_boundary")
            .execute(runtime.pool.as_ref())
            .await?;
        if deleting {
            assert_eq!(
                runtime
                    .delete_threads_at(&[a.thread_id], /*as_of_ms*/ 1)
                    .await?,
                1
            );
        } else {
            AccountingStore::open(&runtime, /*as_of*/ 1).await?;
        }
        let success = rows(&runtime).await?;
        assert_eq!(success[9], vec!["[1,1,1]".to_owned()]);
        if deleting {
            for index in [0, 1, 2, 3, 4, 5, 7, 8, 10, 11, 12] {
                assert!(success[index].is_empty());
            }
            assert_eq!(
                success[6],
                vec![json!([a.attempt_id.to_string(), 365 * 86_400_000_i64]).to_string()]
            );
        }
        runtime.close().await;
        reopens(&path, &success).await?;
    }
    Ok(())
}

#[tokio::test]
async fn request_writes_validate_the_whole_ledger_hourly_and_when_retention_is_due()
-> anyhow::Result<()> {
    const HOUR: i64 = 3_600_000;
    const DETAIL: i64 = 90 * 86_400_000;
    let path = home();
    let runtime = open(&path).await?;
    seed(&runtime).await?;
    let store = AccountingStore::open(&runtime, /*as_of*/ 0).await?;
    for attempt in [attempt(/*id*/ 1), attempt(/*id*/ 2)] {
        store
            .admit(attempt.thread_id, &attempt, &[], /*as_of*/ 0)
            .await?;
    }
    let checkpoint = || async {
        sqlx::query_scalar::<_, i64>(
            "SELECT completed_as_of_ms FROM draft_accounting_retention_checkpoint",
        )
        .fetch_one(runtime.pool.as_ref())
        .await
    };
    let set_payload = |payload: String| {
        sqlx::query("UPDATE draft_accounting_attempts SET payload = ? WHERE attempt_id = ?")
            .bind(payload)
            .bind(Uuid::from_u128(2).to_string())
            .execute(runtime.pool.as_ref())
    };
    let original: String =
        sqlx::query_scalar("SELECT payload FROM draft_accounting_attempts WHERE attempt_id = ?")
            .bind(Uuid::from_u128(2).to_string())
            .fetch_one(runtime.pool.as_ref())
            .await?;

    // Corruption of another attempt is invisible to later writes in the same hour,
    // which only advance the checkpoint, but explicit maintenance still rejects it.
    set_payload("{}".to_owned()).await?;
    AccountingStore::open(&runtime, HOUR - 1).await?;
    assert_eq!(checkpoint().await?, HOUR - 1);
    assert!(store.maintain(HOUR - 1).await.is_err());
    // The first write of the next hour runs the full sweep and fails visibly.
    assert!(AccountingStore::open(&runtime, HOUR).await.is_err());
    assert_eq!(checkpoint().await?, HOUR - 1);
    set_payload(original).await?;
    AccountingStore::open(&runtime, HOUR).await?;
    assert_eq!(checkpoint().await?, HOUR);

    // Detail that aged out before this hour began forces the sweep mid-hour.
    sqlx::query("UPDATE draft_accounting_retention_checkpoint SET completed_as_of_ms = ?")
        .bind(DETAIL + 10)
        .execute(runtime.pool.as_ref())
        .await?;
    AccountingStore::open(&runtime, DETAIL + 20).await?;
    let raw: i64 = sqlx::query_scalar("SELECT count(*) FROM draft_accounting_attempts")
        .fetch_one(runtime.pool.as_ref())
        .await?;
    assert_eq!((raw, checkpoint().await?), (0, DETAIL + 20));
    runtime.close().await;
    Ok(())
}

#[tokio::test]
async fn the_hours_validation_reads_a_snapshot_while_another_process_writes() -> anyhow::Result<()>
{
    const HOUR: i64 = 3_600_000;
    let path = home();
    let runtime = open(&path).await?;
    seed(&runtime).await?;
    let store = AccountingStore::open(&runtime, /*as_of*/ 0).await?;
    for attempt in [attempt(/*id*/ 1), attempt(/*id*/ 2)] {
        store
            .admit(attempt.thread_id, &attempt, &[], /*as_of*/ 0)
            .await?;
    }
    let checkpoint = || async {
        sqlx::query_scalar::<_, i64>(
            "SELECT completed_as_of_ms FROM draft_accounting_retention_checkpoint",
        )
        .fetch_one(runtime.pool.as_ref())
        .await
    };
    // Another process holds the write lock for the whole validation, which the
    // full sweep used to run under that same lock for seconds.
    let other = peer(&runtime).await?;
    let held = other.pool.begin_with("BEGIN IMMEDIATE").await?;
    let mut read = runtime.pool.begin().await?;
    validate_on_connection(&mut read).await?;
    assert_eq!(
        Journal::validate_hour_on_connection(&mut read, HOUR, i64::MIN).await?,
        Some(HOUR)
    );
    // A validation already done this hour stands; none is needed in the hour
    // the checkpoint is already in.
    assert_eq!(
        Journal::validate_hour_on_connection(&mut read, HOUR + 5, HOUR).await?,
        Some(HOUR)
    );
    assert_eq!(
        Journal::validate_hour_on_connection(&mut read, /*as_of_ms*/ 1, i64::MIN).await?,
        None
    );
    read.rollback().await?;
    held.rollback().await?;

    // The write lock is then needed only to advance the checkpoint. Corruption
    // committed after the validated snapshot waits for the next hour, exactly as
    // corruption committed after any sweep in the hour does.
    sqlx::query("UPDATE draft_accounting_attempts SET payload = '{}' WHERE attempt_id = ?")
        .bind(Uuid::from_u128(2).to_string())
        .execute(runtime.pool.as_ref())
        .await?;
    let mut tx = runtime.pool.begin_with("BEGIN IMMEDIATE").await?;
    Journal::maintain_for_write_on_connection(&mut tx, HOUR + 1, Some(HOUR)).await?;
    tx.commit().await?;
    assert_eq!(checkpoint().await?, HOUR + 1);

    // A validation from an earlier hour, or none, still means the full sweep.
    for validated in [Some(HOUR + 1), None] {
        let mut tx = runtime.pool.begin_with("BEGIN IMMEDIATE").await?;
        let error = Journal::maintain_for_write_on_connection(&mut tx, 2 * HOUR, validated)
            .await
            .expect_err("the full sweep sees the corrupt attempt");
        tx.rollback().await?;
        assert!(!is_contention(&error), "{error:#}");
    }
    assert_eq!(checkpoint().await?, HOUR + 1);
    // So does the public write path: its own snapshot validation fails visibly.
    assert!(AccountingStore::open(&runtime, 2 * HOUR).await.is_err());
    assert_eq!(checkpoint().await?, HOUR + 1);

    // Once detail has aged out there is work to apply. The snapshot validation
    // still runs first and meets the corruption before any expiry is applied;
    // without a validation this hour the write still runs the full sweep.
    const DETAIL: i64 = 90 * 86_400_000;
    let mut read = runtime.pool.begin().await?;
    assert!(
        Journal::validate_hour_on_connection(&mut read, DETAIL + 20, i64::MIN)
            .await
            .is_err(),
        "the validation met the corrupt attempt"
    );
    read.rollback().await?;
    let mut tx = runtime.pool.begin_with("BEGIN IMMEDIATE").await?;
    assert!(
        Journal::maintain_for_write_on_connection(
            &mut tx,
            DETAIL + 20,
            /*validated_at_ms*/ None
        )
        .await
        .is_err(),
        "the full sweep ran and met the corrupt attempt"
    );
    tx.rollback().await?;
    assert!(AccountingStore::open(&runtime, DETAIL + 20).await.is_err());
    assert_eq!(checkpoint().await?, HOUR + 1);
    let raw: i64 = sqlx::query_scalar("SELECT count(*) FROM draft_accounting_attempts")
        .fetch_one(runtime.pool.as_ref())
        .await?;
    assert_eq!(raw, 2, "nothing expired past a failed validation");
    runtime.close().await;
    Ok(())
}

#[tokio::test]
async fn contention_is_named_and_nothing_else_is() -> anyhow::Result<()> {
    let path = home();
    let runtime = open(&path).await?;
    seed(&runtime).await?;
    let other = peer(&runtime).await?;
    let held = runtime.pool.begin_with("BEGIN IMMEDIATE").await?;
    let busy = anyhow::Error::from(
        other
            .pool
            .begin_with("BEGIN IMMEDIATE")
            .await
            .expect_err("the lock is held"),
    );
    held.rollback().await?;
    assert!(is_contention(&busy), "{busy:#}");
    assert!(is_contention(&busy.context("accounting admit")));
    assert!(is_contention(&anyhow::Error::from(
        sqlx::Error::PoolTimedOut
    )));
    let constraint = anyhow::Error::from(
        sqlx::query("INSERT INTO threads (id) VALUES (NULL)")
            .execute(runtime.pool.as_ref())
            .await
            .expect_err("constraint"),
    );
    assert!(!is_contention(&constraint), "{constraint:#}");
    assert!(!is_contention(&anyhow::anyhow!("future dispatch")));
    runtime.close().await;
    Ok(())
}

/// A wall clock that stepped back behind the checkpoint (58 s and 6 days in the
/// PF-60-S03 acceptance run) used to fail every `Now` write as a backward
/// checkpoint, and with it every model request. The ledger now holds its time
/// at the checkpoint; the attempt keeps the dispatch time the clock read.
#[tokio::test]
async fn now_writes_hold_at_the_checkpoint_when_the_clock_is_behind_it() -> anyhow::Result<()> {
    for behind in [58_000, 6 * 86_400_000] {
        let path = home();
        let runtime = open(&path).await?;
        seed(&runtime).await?;
        let now = chrono::Utc::now().timestamp_millis();
        let ahead = now + behind;
        AccountingStore::open(&runtime, ahead).await?;
        let store = AccountingStore::open(&runtime, AsOf::Now).await?;
        let mut a = serde_json::to_value(attempt(/*id*/ 1))?;
        a["dispatched_at_ms"] = json!(now);
        let a: Attempt = serde_json::from_value(a)?;
        store.admit(a.thread_id, &a, &[], AsOf::Now).await?;
        store
            .observe(a.thread_id, &a, &[row(/*revision*/ 1)], AsOf::Now)
            .await?;
        let checkpoint: i64 = sqlx::query_scalar(
            "SELECT completed_as_of_ms FROM draft_accounting_retention_checkpoint",
        )
        .fetch_one(runtime.pool.as_ref())
        .await?;
        assert!(checkpoint >= ahead, "the checkpoint never moves backward");
        let stored: Vec<String> =
            sqlx::query_scalar("SELECT payload FROM draft_accounting_attempts")
                .fetch_all(runtime.pool.as_ref())
                .await?;
        let stored: Vec<Attempt> = stored
            .iter()
            .map(|payload| serde_json::from_str(payload))
            .collect::<Result<_, _>>()?;
        assert_eq!(stored, vec![a]);
        // Explicit times stay strict.
        marker(
            AccountingStore::open(&runtime, now)
                .await
                .err()
                .expect("stale time"),
            "backward",
        );
        runtime.close().await;
    }
    Ok(())
}

/// #1518: inspecting at a time behind the checkpoint is still refused, as a
/// typed clock-behind error the caller can name, never as damage.
#[tokio::test]
async fn inspection_behind_the_checkpoint_is_a_typed_clock_refusal() -> anyhow::Result<()> {
    let path = home();
    let runtime = open(&path).await?;
    seed(&runtime).await?;
    let a = attempt(/*id*/ 1);
    let checkpoint = 2 * 86_400_000 + 5_000;
    AccountingStore::open(&runtime, checkpoint)
        .await?
        .admit(a.thread_id, &a, &[], checkpoint)
        .await?;
    let error = inspected(&runtime, /*day*/ 2, checkpoint - 4_000)
        .await
        .expect_err("a read behind the checkpoint is refused");
    assert_eq!(
        clock_behind_ledger(&error),
        Some(ClockBehindLedger { behind_ms: 4_000 })
    );
    marker(error, "backward inspection read");
    let error = AccountingStore::inspect_range(
        &runtime,
        a.thread_id,
        InspectionRange {
            start_ms: 0,
            end_ms: 86_400_000,
            grouping: InspectionGrouping::Day,
        },
        checkpoint - 1,
    )
    .await
    .expect_err("a range read behind the checkpoint is refused");
    assert_eq!(
        clock_behind_ledger(&error),
        Some(ClockBehindLedger { behind_ms: 1 })
    );
    assert!(
        inspected(&runtime, /*day*/ 2, checkpoint).await.is_ok(),
        "the read succeeds once the clock reaches the checkpoint"
    );
    runtime.close().await;
    Ok(())
}

/// #1522: a view opened while the clock read ahead into a later UTC day and
/// refreshed after the clock was set back rereads that later day. The read is
/// still refused, as a typed clock error the caller can name, never as damage.
#[tokio::test]
async fn inspection_of_a_day_after_the_clock_is_a_typed_clock_refusal() -> anyhow::Result<()> {
    let path = home();
    let runtime = open(&path).await?;
    seed(&runtime).await?;
    let a = attempt(/*id*/ 1);
    let ahead = 3 * 86_400_000 + 5_000;
    AccountingStore::open(&runtime, ahead)
        .await?
        .admit(a.thread_id, &a, &[], ahead)
        .await?;
    let set_back = 3 * 86_400_000 - 7_000;
    let error = inspected(&runtime, /*day*/ 3, set_back)
        .await
        .expect_err("a day after the clock is refused");
    assert_eq!(
        clock_before_day(&error),
        Some(ClockBeforeDay { behind_ms: 7_000 })
    );
    assert_eq!(clock_behind_ledger(&error), None);
    marker(error, "invalid or future day");
    let error = AccountingStore::inspect_range(
        &runtime,
        a.thread_id,
        InspectionRange {
            start_ms: 3 * 86_400_000,
            end_ms: 4 * 86_400_000,
            grouping: InspectionGrouping::Day,
        },
        set_back,
    )
    .await
    .expect_err("a range starting on a day after the clock is refused");
    assert_eq!(
        clock_before_day(&error),
        Some(ClockBeforeDay { behind_ms: 7_000 })
    );
    let error = inspected(&runtime, /*day*/ -1, set_back)
        .await
        .expect_err("a day before the epoch is refused");
    assert_eq!(clock_before_day(&error), None);
    marker(error, "invalid or future day");
    // Reopened at the clock's own day, the read reaches the checkpoint check.
    let error = inspected(&runtime, /*day*/ 2, set_back)
        .await
        .expect_err("the clock is still behind the checkpoint");
    assert_eq!(
        clock_behind_ledger(&error),
        Some(ClockBehindLedger { behind_ms: 12_000 })
    );
    runtime.close().await;
    Ok(())
}

/// #308: deleting a conversation with the clock behind the checkpoint (58 s and
/// 6 days in the acceptance run) failed as a backward checkpoint. It now holds
/// at the checkpoint like ledger writes, and the deleted spend is still counted
/// for "deleted conversations" (#286).
#[tokio::test]
async fn delete_holds_at_the_checkpoint_when_the_clock_is_behind_it() -> anyhow::Result<()> {
    for behind in [58_000, 6 * 86_400_000] {
        let path = home();
        let runtime = open(&path).await?;
        seed(&runtime).await?;
        let now = chrono::Utc::now().timestamp_millis();
        let ahead = now + behind;
        let store = AccountingStore::open(&runtime, ahead).await?;
        // A conversation and its subagent, deleted together.
        let child = ThreadId::from_string(&Uuid::from_u128(8).to_string())?;
        let home = runtime.sqlite().home().to_path_buf();
        runtime
            .upsert_thread(&test_thread_metadata(&home, child, home.clone()))
            .await?;
        let mut owned = Vec::new();
        for (id, owner) in [(1, None), (2, Some(child))] {
            let mut a = serde_json::to_value(attempt(id))?;
            a["dispatched_at_ms"] = json!(now);
            if let Some(owner) = owner {
                a["thread_id"] = json!(owner);
            }
            let a: Attempt = serde_json::from_value(a)?;
            store.admit(a.thread_id, &a, &[], AsOf::Now).await?;
            if owner.is_none() {
                store
                    .observe(a.thread_id, &a, &[row(/*revision*/ 1)], AsOf::Now)
                    .await?;
            }
            owned.push(a);
        }
        let (a, owners) = (&owned[0], [owned[0].thread_id, child]);

        runtime.preflight_delete_threads(&owners).await?;
        assert_eq!(
            runtime
                .get_thread(a.thread_id)
                .await?
                .map(|thread| thread.id),
            Some(a.thread_id),
            "the preflight changes nothing"
        );
        assert_eq!(runtime.delete_threads_strict(&owners).await?, 2);

        let checkpoint: i64 = sqlx::query_scalar(
            "SELECT completed_as_of_ms FROM draft_accounting_retention_checkpoint",
        )
        .fetch_one(runtime.pool.as_ref())
        .await?;
        assert!(checkpoint >= ahead, "the checkpoint never moves backward");
        let attempts: i64 = sqlx::query_scalar("SELECT count(*) FROM draft_accounting_attempts")
            .fetch_one(runtime.pool.as_ref())
            .await?;
        let tombstones: Vec<(String, i64)> =
            sqlx::query_as("SELECT attempt_id, expires_at_ms FROM draft_accounting_tombstones")
                .fetch_all(runtime.pool.as_ref())
                .await?;
        assert_eq!(
            (attempts, tombstones),
            (
                0,
                owned
                    .iter()
                    .map(|a| (a.attempt_id.to_string(), now + 365 * 86_400_000_i64))
                    .collect::<Vec<_>>()
            )
        );
        let mut conn = runtime.pool.acquire().await?;
        let mut work = InspectionWork::new(&mut conn).await?;
        assert_eq!(
            scope::deleted_attempts(&mut conn, now / 86_400_000, now, &mut work).await,
            DeletedAttempts::Counted(2)
        );
        drop(conn);
        for owner in owners {
            assert!(runtime.get_thread(owner).await?.is_none());
        }
        runtime.close().await;
    }
    Ok(())
}

/// #308: a ledger that cannot be updated fails the preflight and the delete,
/// and leaves every row as it was.
#[tokio::test]
async fn failed_delete_preflight_changes_nothing() -> anyhow::Result<()> {
    let path = home();
    let runtime = open(&path).await?;
    seed(&runtime).await?;
    let mut a = serde_json::to_value(attempt(/*id*/ 1))?;
    a["dispatched_at_ms"] = json!(chrono::Utc::now().timestamp_millis());
    let a: Attempt = serde_json::from_value(a)?;
    AccountingStore::open(&runtime, AsOf::Now)
        .await?
        .admit(a.thread_id, &a, &[], AsOf::Now)
        .await?;
    sqlx::query("CREATE TRIGGER reject_tombstone BEFORE INSERT ON draft_accounting_tombstones BEGIN SELECT RAISE(ABORT, 'fixture-ledger-failure'); END")
        .execute(runtime.pool.as_ref())
        .await?;
    let before = rows(&runtime).await?;
    marker(
        runtime
            .preflight_delete_threads(&[a.thread_id])
            .await
            .unwrap_err(),
        "fixture-ledger-failure",
    );
    assert_eq!(rows(&runtime).await?, before);
    assert!(runtime.get_thread(a.thread_id).await?.is_some());
    runtime.close().await;
    Ok(())
}

#[tokio::test]
async fn writes_read_their_clock_after_another_process_advanced_the_checkpoint()
-> anyhow::Result<()> {
    // Another process committed a checkpoint while this one waited for the write
    // lock holding a clock reading from before it.
    let path = home();
    let runtime = open(&path).await?;
    seed(&runtime).await?;
    let committed = chrono::Utc::now().timestamp_millis();
    let store = AccountingStore::open(&runtime, committed).await?;
    let stale = committed - 1;
    marker(
        AccountingStore::open(&runtime, stale)
            .await
            .err()
            .expect("stale time"),
        "backward",
    );
    // Reading the clock under the lock cannot fall behind a committed checkpoint.
    let mut a = serde_json::to_value(attempt(/*id*/ 1))?;
    a["dispatched_at_ms"] = json!(committed);
    let a: Attempt = serde_json::from_value(a)?;
    AccountingStore::open(&runtime, AsOf::Now).await?;
    store.admit(a.thread_id, &a, &[], AsOf::Now).await?;
    store
        .observe(a.thread_id, &a, &[row(/*revision*/ 1)], AsOf::Now)
        .await?;
    let checkpoint: i64 =
        sqlx::query_scalar("SELECT completed_as_of_ms FROM draft_accounting_retention_checkpoint")
            .fetch_one(runtime.pool.as_ref())
            .await?;
    assert!(
        checkpoint >= committed,
        "the checkpoint never moves backward"
    );
    runtime.close().await;
    Ok(())
}

const DAY: i64 = 86_400_000;
const DETAIL: i64 = 90 * DAY;
const STEP: i64 = DAY / 10;

/// `count` attempts, one every `STEP` from day 0, all written at `DETAIL - 1`,
/// just before the first of them reaches the detail horizon. Each has its own
/// price snapshot, as real attempts mostly do.
async fn aging_ledger(runtime: &StateRuntime, count: u128) -> anyhow::Result<()> {
    seed(runtime).await?;
    AccountingStore::open(runtime, DETAIL - 1).await?;
    // One transaction, as the request path writes each: a commit per attempt
    // made the fixture most of the test's run time.
    let mut tx = runtime.pool.begin_with("BEGIN IMMEDIATE").await?;
    for id in 1..=count {
        let mut a = serde_json::to_value(attempt(id))?;
        a["request_id"] = json!(Uuid::from_u128(id + 1_000_000));
        a["dispatched_at_ms"] = json!((id as i64 - 1) * STEP);
        let a: Attempt = serde_json::from_value(a)?;
        let mut price = serde_json::to_value(snapshot())?;
        price["id"] = json!(Uuid::from_u128(id + 2_000_000));
        let price: Snapshot = serde_json::from_value(price)?;
        Journal::store_on_connection(
            &mut tx,
            a.thread_id,
            &a,
            &[],
            Some(&[price]),
            DETAIL - 1,
            Some(DETAIL - 1),
        )
        .await?;
    }
    tx.commit().await?;
    Ok(())
}

async fn accounting_rows(runtime: &StateRuntime) -> anyhow::Result<Vec<Vec<String>>> {
    let mut rows = rows(runtime).await?;
    rows.truncate(10);
    Ok(rows)
}

/// Day 90: the first write after records start expiring retires them in short
/// batches. Another process writing all the while gets in between batches -
/// it sees the ledger part-way through the expiry, which the full sweep's one
/// transaction never let it - and never waits a second.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn expiry_at_the_detail_horizon_never_holds_up_another_writer() -> anyhow::Result<()> {
    const ATTEMPTS: i64 = 600;
    let path = home();
    let runtime = open(&path).await?;
    aging_ledger(&runtime, ATTEMPTS as u128).await?;
    // Another process with SQLite's usual busy timeout.
    let other = crate::sqlite::open_pool_for_testing(
        sqlx::sqlite::SqlitePoolOptions::new().max_connections(1),
        sqlx::sqlite::SqliteConnectOptions::new()
            .filename(runtime.sqlite().state_db_path())
            .busy_timeout(std::time::Duration::from_secs(5)),
    )
    .await?;
    let done = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let writer = tokio::spawn({
        let done = done.clone();
        async move {
            let mut writes = Vec::new();
            while !done.load(std::sync::atomic::Ordering::Acquire) {
                let started = std::time::Instant::now();
                let mut tx = other.begin_with("BEGIN IMMEDIATE").await?;
                let raw: i64 = sqlx::query_scalar("SELECT count(*) FROM draft_accounting_attempts")
                    .fetch_one(&mut *tx)
                    .await?;
                sqlx::query("UPDATE thread_dynamic_tools SET description = ?")
                    .bind(writes.len().to_string())
                    .execute(&mut *tx)
                    .await?;
                tx.commit().await?;
                writes.push((started.elapsed(), raw));
                // Pace the writes as a busy session would.
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
            anyhow::Ok(writes)
        }
    });
    // Half the ledger is past the horizon.
    let due = ATTEMPTS / 2 + 1;
    let now = DETAIL + (due - 1) * STEP;
    let started = std::time::Instant::now();
    AccountingStore::open(&runtime, now).await?;
    let expiry = started.elapsed();
    done.store(true, std::sync::atomic::Ordering::Release);
    let writes = writer.await??;
    let longest = writes
        .iter()
        .map(|(wait, _)| *wait)
        .max()
        .unwrap_or_default();
    let midway: std::collections::BTreeSet<i64> = writes
        .iter()
        .map(|(_, raw)| *raw)
        .filter(|raw| (ATTEMPTS - due + 1..ATTEMPTS).contains(raw))
        .collect();
    assert!(
        longest < std::time::Duration::from_secs(1),
        "another writer waited {longest:?} during a {expiry:?} expiry"
    );
    assert!(
        midway.len() > 1,
        "the writer got in between batches only at {midway:?} ({} writes in {expiry:?})",
        writes.len()
    );
    let raw: i64 = sqlx::query_scalar("SELECT count(*) FROM draft_accounting_attempts")
        .fetch_one(runtime.pool.as_ref())
        .await?;
    let checkpoint: i64 =
        sqlx::query_scalar("SELECT completed_as_of_ms FROM draft_accounting_retention_checkpoint")
            .fetch_one(runtime.pool.as_ref())
            .await?;
    assert_eq!((raw, checkpoint), (ATTEMPTS - due, now));
    runtime.close().await;
    Ok(())
}

/// A process that dies mid-expiry keeps every batch it committed; the next one
/// carries on and ends exactly where the full sweep would have.
#[tokio::test]
async fn expiry_interrupted_mid_sweep_resumes_to_the_full_sweeps_result() -> anyhow::Result<()> {
    // 121 of 150 attempts are due: several batches.
    let now = DETAIL + 120 * STEP;
    let (path, full_path) = (home(), home());
    let full = open(&full_path).await?;
    aging_ledger(&full, /*count*/ 150).await?;
    AccountingStore::open(&full, DETAIL - 1)
        .await?
        .maintain(now)
        .await?;
    let expected = accounting_rows(&full).await?;
    full.close().await;

    let runtime = open(&path).await?;
    aging_ledger(&runtime, /*count*/ 150).await?;
    let before = accounting_rows(&runtime).await?;
    // The hour's validation, then two committed batches and one cut short.
    let mut read = runtime.pool.begin().await?;
    assert_eq!(
        Journal::validate_hour_on_connection(&mut read, now, i64::MIN).await?,
        Some(now)
    );
    read.rollback().await?;
    for commit in [true, true, false] {
        let mut tx = runtime.pool.begin_with("BEGIN IMMEDIATE").await?;
        assert!(Journal::expire_for_write_on_connection(&mut tx, now, Some(now)).await?);
        if commit {
            tx.commit().await?;
        }
    }
    let interrupted = accounting_rows(&runtime).await?;
    assert_ne!(interrupted, before);
    assert_ne!(interrupted, expected);
    assert_eq!(
        interrupted[9], before[9],
        "the checkpoint waits for the sweep"
    );
    runtime.close().await;

    let runtime = open(&path).await?;
    assert_eq!(accounting_rows(&runtime).await?, interrupted);
    AccountingStore::open(&runtime, now).await?;
    assert_eq!(accounting_rows(&runtime).await?, expected);
    runtime.close().await;
    Ok(())
}

/// The unapplied-migration check reads object names from `CREATE` statements;
/// a migration whose statements it cannot see would make that check vacuous.
#[test]
fn every_accounting_migration_creates_named_objects() {
    for migration in crate::migrations::accounting_migrator().iter() {
        assert!(
            !super::created_objects(migration.sql.as_str()).is_empty(),
            "migration {} names no objects",
            migration.version
        );
    }
}
