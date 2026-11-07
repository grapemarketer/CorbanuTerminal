//! Accounting journal behind the explicitly installed normal-library facade.
use super::StateRuntime;
use anyhow::Context;
use anyhow::ensure;
use sqlx::SqliteConnection;
use uuid::Uuid;
#[path = "accounting_store.rs"]
pub mod store;

#[path = "accounting_pricing.rs"]
mod pricing;

#[path = "accounting_native.rs"]
pub(super) mod native;

#[path = "accounting_types.rs"]
mod types;

#[path = "accounting_gap_days.rs"]
pub(in crate::runtime) mod gap_days;
use types::Attempt;
use types::Observation;
use types::replay;

#[cfg(test)]
#[path = "accounting_tests.rs"]
mod tests;

struct Journal<'a> {
    #[cfg_attr(not(test), allow(dead_code))]
    runtime: &'a StateRuntime,
}

impl<'a> Journal<'a> {
    /// Explicit fixture setup only; never called by StateRuntime initialization.
    #[cfg(test)]
    async fn create_for_tests(runtime: &'a StateRuntime) -> anyhow::Result<Self> {
        sqlx::raw_sql(
            "CREATE TABLE draft_accounting_attempts (
                attempt_id TEXT PRIMARY KEY, request_id TEXT NOT NULL, payload TEXT NOT NULL);
             CREATE INDEX draft_accounting_request ON draft_accounting_attempts(request_id);
             CREATE TABLE draft_accounting_observations (
                attempt_id TEXT NOT NULL REFERENCES draft_accounting_attempts(attempt_id),
                revision INTEGER NOT NULL CHECK(revision > 0),
                source TEXT NOT NULL, sequence INTEGER NOT NULL CHECK(sequence >= 0),
                payload TEXT NOT NULL, PRIMARY KEY(attempt_id, revision), UNIQUE(source, sequence));",
        )
        .execute(runtime.pool.as_ref())
        .await?;
        Ok(Self { runtime })
    }

    #[cfg_attr(not(test), allow(dead_code))]
    async fn begin_attempt(&self, attempt: &Attempt) -> anyhow::Result<()> {
        self.append_observation(attempt, &[]).await
    }

    /// All identity, patch and source-position writes either commit together or roll back.
    #[cfg_attr(not(test), allow(dead_code))]
    async fn append_observation(
        &self,
        attempt: &Attempt,
        batch: &[Observation],
    ) -> anyhow::Result<()> {
        attempt.validate()?;
        ensure!(batch.len() <= 256, "observation batch too large");
        let mut tx = crate::runtime::db_lock::begin_immediate(&self.runtime.pool).await?;
        match Self::append_on_connection(&mut tx, attempt, batch).await {
            Ok(()) => tx.commit().await?,
            Err(error) => {
                tx.rollback().await?;
                return Err(error);
            }
        }
        Ok(())
    }

    /// Caller holds a write transaction; the wrapper and contention tests share this body.
    async fn append_on_connection(
        conn: &mut SqliteConnection,
        attempt: &Attempt,
        batch: &[Observation],
    ) -> anyhow::Result<()> {
        attempt.validate()?;
        ensure!(batch.len() <= 256, "observation batch too large");
        if let RetentionFixture::Active(checkpoint) = retention_fixture_on_connection(conn).await? {
            let dispatch = i64::from(attempt.dispatched_at_ms);
            ensure!(dispatch <= checkpoint, "future dispatch");
            let replay_expiry = dispatch
                .checked_add(365 * 86_400_000)
                .context("replay expiry overflow")?;
            ensure!(replay_expiry > checkpoint, "expired replay import");
            let detail_expiry = dispatch
                .checked_add(90 * 86_400_000)
                .context("detail expiry overflow")?;
            ensure!(
                detail_expiry > checkpoint,
                "request dispatch time is outside the ledger's 90-day detail window; compact-only late imports are unsupported"
            );
        }
        let installed: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM sqlite_schema WHERE type = 'table' AND name = 'draft_accounting_tombstones')")
            .fetch_one(&mut *conn).await?;
        if installed {
            let deleted: bool = sqlx::query_scalar(
                "SELECT EXISTS(SELECT 1 FROM draft_accounting_tombstones WHERE attempt_id = ?)",
            )
            .bind(attempt.attempt_id.to_string())
            .fetch_one(&mut *conn)
            .await?;
            ensure!(!deleted, "deleted attempt");
        }
        let existing = sqlx::query_scalar::<_, String>(
            "SELECT payload FROM draft_accounting_attempts WHERE request_id = ?",
        )
        .bind(attempt.request_id.to_string())
        .fetch_optional(&mut *conn)
        .await?;
        if let Some(existing) = existing {
            ensure!(
                attempt.same_owner(&serde_json::from_str(&existing)?),
                "request owner conflict"
            );
        }
        if let Some(predecessor) = attempt.retry_of {
            let Some(parent) = read_attempt(conn, predecessor).await? else {
                return Err(native::PredecessorMissing.into());
            };
            ensure!(attempt.same_owner(&parent), "invalid retry predecessor");
        }
        // A record a format-1 ledger cannot express upgrades the ledger in the
        // same transaction that writes it.
        let format = batch
            .iter()
            .map(Observation::ledger_format)
            .chain([attempt.ledger_format()])
            .max()
            .unwrap_or(1);
        if format > 1 {
            crate::runtime::accounting::store::require_format_on_connection(conn, format).await?;
        }
        sqlx::query("INSERT INTO draft_accounting_attempts VALUES (?, ?, ?) ON CONFLICT(attempt_id) DO NOTHING")
            .bind(attempt.attempt_id.to_string())
            .bind(attempt.request_id.to_string())
            .bind(serde_json::to_string(attempt)?)
            .execute(&mut *conn).await?;
        ensure!(
            read_attempt(conn, attempt.attempt_id).await?.as_ref() == Some(attempt),
            "immutable identity conflict"
        );
        for observation in batch {
            ensure!(
                i64::from(observation.revision) > 0,
                "revision must be positive"
            );
            sqlx::query("INSERT INTO draft_accounting_observations VALUES (?, ?, ?, ?, ?) ON CONFLICT(attempt_id, revision) DO NOTHING")
                .bind(attempt.attempt_id.to_string())
                .bind(i64::from(observation.revision))
                .bind(observation.source.to_string())
                .bind(i64::from(observation.sequence))
                .bind(serde_json::to_string(observation)?)
                .execute(&mut *conn).await?;
            let stored: String = sqlx::query_scalar("SELECT payload FROM draft_accounting_observations WHERE attempt_id = ? AND revision = ?")
                .bind(attempt.attempt_id.to_string())
                .bind(i64::from(observation.revision))
                .fetch_one(&mut *conn).await?;
            ensure!(
                serde_json::from_str::<Observation>(&stored)? == *observation,
                "observation conflict"
            );
        }
        replay(
            attempt.dialect,
            &read_patches(conn, attempt.attempt_id).await?,
        )?;
        anyhow::Ok(())
    }

    #[cfg_attr(not(test), allow(dead_code))]
    async fn read_observations(
        &self,
        id: Uuid,
    ) -> anyhow::Result<Option<(Attempt, Vec<Observation>)>> {
        let mut tx = self.runtime.pool.begin().await?;
        let result = match read_attempt(&mut tx, id).await? {
            Some(attempt) => Some((attempt, read_patches(&mut tx, id).await?)),
            None => None,
        };
        tx.commit().await?;
        Ok(result)
    }
}

async fn read_attempt(conn: &mut SqliteConnection, id: Uuid) -> anyhow::Result<Option<Attempt>> {
    let payload: Option<String> =
        sqlx::query_scalar("SELECT payload FROM draft_accounting_attempts WHERE attempt_id = ?")
            .bind(id.to_string())
            .fetch_optional(conn)
            .await?;
    Ok(payload.map(|p| serde_json::from_str(&p)).transpose()?)
}

async fn read_patches(conn: &mut SqliteConnection, id: Uuid) -> anyhow::Result<Vec<Observation>> {
    let payloads: Vec<String> = sqlx::query_scalar(
        "SELECT payload FROM draft_accounting_observations WHERE attempt_id = ? ORDER BY revision",
    )
    .bind(id.to_string())
    .fetch_all(conn)
    .await?;
    Ok(payloads
        .iter()
        .map(|p| serde_json::from_str(p))
        .collect::<Result<_, _>>()?)
}

#[derive(Debug, PartialEq, Eq)]
enum RetentionFixture {
    Absent,
    Staging,
    Active(i64),
}

/// How long a day's totals and replay fences are kept: one year. Expiry and
/// the stated retention floor both use this one value.
const REPLAY_MS: i64 = 365 * 86_400_000;

/// The first UTC day whose daily totals are still kept at `checkpoint`. Day
/// pages and ranges both read it here, so they agree on how far back the
/// totals go.
fn aggregate_day_floor(checkpoint: i64) -> i64 {
    const DAY_MS: i64 = 86_400_000;
    if checkpoint < REPLAY_MS {
        0
    } else {
        (checkpoint - REPLAY_MS) / DAY_MS + 1
    }
}

/// Strict fixture metadata, never an implicit install, repair or activation path.
async fn retention_fixture_on_connection(
    conn: &mut SqliteConnection,
) -> anyhow::Result<RetentionFixture> {
    let count: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM sqlite_schema WHERE name IN (
            'draft_accounting_compact_days', 'draft_accounting_compact_snapshots',
            'draft_accounting_retention_checkpoint')",
    )
    .fetch_one(&mut *conn)
    .await?;
    if count == 0 {
        return Ok(RetentionFixture::Absent);
    }
    ensure!(count == 3, "partial retention schema");
    for (table, expected) in [
        (
            "draft_accounting_compact_days",
            "thread_id:TEXT:1:1,utc_day:INTEGER:1:2,payload:TEXT:1:0",
        ),
        (
            "draft_accounting_compact_snapshots",
            "thread_id:TEXT:1:1,utc_day:INTEGER:1:2,snapshot_id:TEXT:1:3",
        ),
        (
            "draft_accounting_retention_checkpoint",
            "singleton:INTEGER:0:1,completed_as_of_ms:INTEGER:0:0,admission_active:INTEGER:1:0",
        ),
    ] {
        let is_table: bool = sqlx::query_scalar(
            "SELECT EXISTS(SELECT 1 FROM sqlite_schema WHERE name = ? AND type = 'table')",
        )
        .bind(table)
        .fetch_one(&mut *conn)
        .await?;
        let columns: Vec<(String, String, i64, i64)> = sqlx::query_as(
            "SELECT name, type, \"notnull\", pk FROM pragma_table_info(?) ORDER BY cid",
        )
        .bind(table)
        .fetch_all(&mut *conn)
        .await?;
        let shape = columns
            .iter()
            .map(|(name, kind, required, key)| format!("{name}:{kind}:{required}:{key}"))
            .collect::<Vec<_>>()
            .join(",");
        ensure!(
            is_table && shape == expected,
            "invalid retention schema: {table}"
        );
    }
    let rows: Vec<(i64, Option<i64>, i64)> = sqlx::query_as(
        "SELECT singleton, completed_as_of_ms, admission_active FROM draft_accounting_retention_checkpoint",
    ).fetch_all(conn).await?;
    ensure!(rows.len() == 1, "missing or multiple checkpoints");
    let (key, time, active) = rows[0];
    ensure!(key == 1, "invalid checkpoint key");
    ensure!(time.is_none_or(|time| time >= 0), "negative checkpoint");
    match active {
        0 => Ok(RetentionFixture::Staging),
        1 => Ok(RetentionFixture::Active(
            time.context("active NULL checkpoint")?,
        )),
        _ => anyhow::bail!("invalid admission bit"),
    }
}
