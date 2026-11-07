//! Retention expiry in bounded batches, each its own short write transaction.
//!
//! Once records reach the 90-day detail horizon, some expire every hour. The
//! full sweep applies them only after reading, validating and pricing every
//! retained attempt under the write lock: 9-17 s on a real 7,000-attempt
//! ledger, past SQLite's busy timeout for every other writer sharing the state
//! DB. Here the whole-ledger validation runs on a read snapshot instead (see
//! `validate_hour_on_connection`), and each batch applies a few expiries as the
//! full sweep would, checking every record it touches. A batch commits whole or
//! not at all and only ever applies what is due, so progress survives a crash
//! and repeating a batch is harmless. Batches never move the checkpoint; see
//! `maintain_for_write_on_connection` for when the write does.
use super::*;
use std::time::Duration;
use std::time::Instant;

/// Most raw attempts, compact days and tombstones one batch retires of each kind.
pub(super) const EXPIRY_BATCH: i64 = 32;
/// A batch stops taking raw attempts once it has held the lock this long.
const EXPIRY_BATCH_TIME: Duration = Duration::from_millis(100);

/// Caller holds BEGIN IMMEDIATE, and the whole ledger was validated this hour.
/// Applies at most `limit` of each kind of expiry due at `as_of_ms`, as
/// `maintain_on_connection` applies it, and returns whether any remain due. The
/// checkpoint is left alone.
pub(super) async fn expire_batch_on_connection(
    conn: &mut SqliteConnection,
    as_of_ms: i64,
    limit: i64,
) -> anyhow::Result<bool> {
    ensure!(limit > 0, "empty expiry batch");
    let RetentionFixture::Active(checkpoint) = retention_fixture_on_connection(conn).await? else {
        anyhow::bail!("incremental expiry requires active retention");
    };
    ensure!(checkpoint <= as_of_ms, "backward retention expiry");
    let started = Instant::now();
    let mut applied = 0;
    let mut unbound = BTreeSet::new();
    let due: Vec<String> = sqlx::query_scalar(
        "SELECT attempt_id FROM draft_accounting_attempts
            WHERE json_extract(payload, '$.dispatched_at_ms') <= ?
            ORDER BY json_extract(payload, '$.dispatched_at_ms'), attempt_id LIMIT ?",
    )
    // `dispatched + DETAIL_MS <= as_of`, with the arithmetic on the bound, and
    // oldest first, so the dispatch index (`accounting_indexes.rs`) answers it
    // under the lock; ordering by `attempt_id` alone made SQLite walk the
    // primary key through every attempt.
    .bind(
        as_of_ms
            .checked_sub(DETAIL_MS)
            .context("detail expiry underflow")?,
    )
    .bind(limit)
    .fetch_all(&mut *conn)
    .await?;
    for id in due {
        if applied > 0 && started.elapsed() >= EXPIRY_BATCH_TIME {
            break;
        }
        expire_raw(conn, &id, as_of_ms, &mut unbound).await?;
        applied += 1;
    }
    let days: Vec<(String, i64)> = sqlx::query_as(
        "SELECT thread_id, utc_day FROM draft_accounting_compact_days
            WHERE utc_day * ? + ? <= ? ORDER BY thread_id, utc_day LIMIT ?",
    )
    .bind(DAY_MS)
    .bind(REPLAY_MS)
    .bind(as_of_ms)
    .bind(limit)
    .fetch_all(&mut *conn)
    .await?;
    for (thread, day) in days {
        // Checked as the full sweep checks every compact day, since one written
        // after the hour's validation can expire within the hour.
        day_key(thread.clone(), day, as_of_ms)?;
        let payload: String = sqlx::query_scalar(
            "SELECT payload FROM draft_accounting_compact_days WHERE thread_id = ? AND utc_day = ?",
        )
        .bind(&thread)
        .bind(day)
        .fetch_one(&mut *conn)
        .await?;
        CompactValues::decode(&payload)?;
        let snapshots: Vec<String> = sqlx::query_scalar(
            "SELECT snapshot_id FROM draft_accounting_compact_snapshots WHERE thread_id = ? AND utc_day = ?",
        )
        .bind(&thread)
        .bind(day)
        .fetch_all(&mut *conn)
        .await?;
        unbound.extend(snapshots);
        for sql in [
            "DELETE FROM draft_accounting_compact_snapshots WHERE thread_id = ? AND utc_day = ?",
            "DELETE FROM draft_accounting_compact_days WHERE thread_id = ? AND utc_day = ?",
        ] {
            sqlx::query(sql)
                .bind(&thread)
                .bind(day)
                .execute(&mut *conn)
                .await?;
        }
        applied += 1;
    }
    // #314: a gap marker expires with its day's totals. At most one per
    // conversation per day, a small table, so one statement does for all.
    crate::runtime::accounting::gap_days::expire(conn, as_of_ms, REPLAY_MS).await?;
    // Only snapshots this batch unbound can have become unreferenced by it. A
    // whole-table collection scans every binding per snapshot - about 1 s on a
    // real ledger - and would only differ for a snapshot already unreferenced,
    // which nothing writes: a snapshot is stored with its binding.
    for snapshot in unbound {
        sqlx::query(
            "DELETE FROM draft_accounting_price_snapshots WHERE snapshot_id = ?
                AND NOT EXISTS (SELECT 1 FROM draft_accounting_price_bindings WHERE snapshot_id = ?)
                AND NOT EXISTS (SELECT 1 FROM draft_accounting_compact_snapshots WHERE snapshot_id = ?)",
        )
        .bind(&snapshot)
        .bind(&snapshot)
        .bind(&snapshot)
        .execute(&mut *conn)
        .await?;
    }
    let tombstones: Vec<(String, i64)> = sqlx::query_as(
        "SELECT attempt_id, expires_at_ms FROM draft_accounting_tombstones
            WHERE expires_at_ms <= ? ORDER BY attempt_id LIMIT ?",
    )
    .bind(as_of_ms)
    .bind(limit)
    .fetch_all(&mut *conn)
    .await?;
    for (id, expiry) in tombstones {
        attempt_key(&id)?;
        ensure!(
            expiry
                .checked_sub(REPLAY_MS)
                .is_some_and(|dispatch| dispatch >= 0),
            "invalid replay expiry"
        );
        sqlx::query("DELETE FROM draft_accounting_tombstones WHERE attempt_id = ?")
            .bind(&id)
            .execute(&mut *conn)
            .await?;
        applied += 1;
    }
    let more = retention_due_on_connection(conn, as_of_ms).await?;
    // Every due record selected is retired or fails the batch, so a batch that
    // leaves work due has retired something; anything else would never finish.
    ensure!(applied > 0 || !more, "retention expiry made no progress");
    Ok(more)
}

/// Retire one raw attempt whose detail is due, as the full sweep does: verify
/// it and its contribution, fold it into its compact day unless that day has
/// expired too, and leave a tombstone until its replay window closes.
async fn expire_raw(
    conn: &mut SqliteConnection,
    id: &str,
    as_of_ms: i64,
    unbound: &mut BTreeSet<String>,
) -> anyhow::Result<()> {
    let uuid = attempt_key(id)?;
    let collision: bool = sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM draft_accounting_tombstones WHERE attempt_id = ?)",
    )
    .bind(id)
    .fetch_one(&mut *conn)
    .await?;
    ensure!(!collision, "raw/tombstone collision");
    let quote = EstimateStore::latest_quote_on_connection(conn, uuid).await?;
    let attempt = &quote.attempt;
    let dispatch = i64::from(attempt.dispatched_at_ms);
    let detail_expiry = dispatch
        .checked_add(DETAIL_MS)
        .context("detail expiry overflow")?;
    ensure!(
        detail_expiry <= as_of_ms,
        "attempt payload does not match its expiry"
    );
    let replay_expiry = dispatch
        .checked_add(REPLAY_MS)
        .context("replay expiry overflow")?;
    let ((thread, day), day_expiry) =
        day_key(attempt.thread_id.to_string(), dispatch / DAY_MS, as_of_ms)?;
    let contribution: Option<(String, i64, String)> = sqlx::query_as(
        "SELECT thread_id, utc_day, evidence FROM draft_accounting_contributions WHERE attempt_id = ?",
    )
    .bind(id)
    .fetch_optional(&mut *conn)
    .await?;
    if let Some((owner, owner_day, evidence)) = &contribution {
        validate_reference(conn, attempt, owner, *owner_day, evidence).await?;
    }
    // Conservative whole-day expiry must not resurrect an expired raw-only day.
    if day_expiry > as_of_ms {
        let mut totals = DayTotals::default();
        totals.add(&quote)?;
        let stored: Option<String> = sqlx::query_scalar(
            "SELECT payload FROM draft_accounting_compact_days WHERE thread_id = ? AND utc_day = ?",
        )
        .bind(&thread)
        .bind(day)
        .fetch_optional(&mut *conn)
        .await?;
        let base = match stored {
            Some(payload) => CompactValues::decode(&payload)?,
            None => CompactValues::from_day_totals(&DayTotals::default())?,
        };
        let values = base.checked_add(&CompactValues::from_day_totals(&totals)?)?;
        sqlx::query("INSERT INTO draft_accounting_compact_days VALUES (?, ?, ?) ON CONFLICT(thread_id, utc_day) DO UPDATE SET payload = excluded.payload")
            .bind(&thread).bind(day).bind(values.encode()?).execute(&mut *conn).await?;
        if let Some(snapshot) = &quote.snapshot {
            sqlx::query("INSERT INTO draft_accounting_compact_snapshots VALUES (?, ?, ?) ON CONFLICT(thread_id, utc_day, snapshot_id) DO NOTHING")
                .bind(&thread).bind(day).bind(snapshot.id.to_string()).execute(&mut *conn).await?;
        }
    }
    let bound: Option<Option<String>> = sqlx::query_scalar(
        "SELECT snapshot_id FROM draft_accounting_price_bindings WHERE attempt_id = ?",
    )
    .bind(id)
    .fetch_optional(&mut *conn)
    .await?;
    unbound.extend(bound.flatten());
    remove_raw(conn, uuid, replay_expiry, as_of_ms).await
}

#[cfg(test)]
#[path = "accounting_retention_incremental_tests.rs"]
mod tests;
