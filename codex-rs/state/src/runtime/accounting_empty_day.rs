//! FUP-02 (#352): what a past day with nothing recorded can be said to mean.
//!
//! The ledger has no durable record of when a writer was running, and marks
//! only the days on which a write is known to have failed (#314), so an
//! empty day never proves on its own that no request was sent. Only a day
//! that ends before the conversation started does: no request of a
//! conversation, or of its subagents, can precede it.

use super::EmptyDay;
use anyhow::Context;
use codex_protocol::ThreadId;
use sqlx::SqliteConnection;

const DAY_MS: i64 = 86_400_000;
/// Same spawn-tree bound as the tree inspection's ancestry cache.
const TREE_LIMIT: usize = 10_000;
/// Below this a stored `created_at` is seconds, as migration 0025 reads it.
const SECONDS_BELOW: i64 = 1_577_836_800_000;

/// The conversation and every thread reachable from it by spawn edges, a
/// superset of the resolved subagents the day view counts.
macro_rules! tree {
    () => {
        "WITH RECURSIVE tree(id) AS (
         SELECT ?1 UNION SELECT e.child_thread_id FROM thread_spawn_edges e
         JOIN tree ON e.parent_thread_id = tree.id LIMIT 10001) "
    };
}

/// The earliest instant (UTC ms) at which this conversation or a subagent can
/// have sent a request: the minimum of every thread's UUIDv7 id time, its
/// stored creation times, and every request recorded for any of them. Taking
/// the minimum means one late or rewritten timestamp cannot move it later.
/// `None` when it cannot be established: a thread id that is not UUIDv7 (it
/// carries no time) or a spawn tree too large to read.
pub(super) async fn started_ms(
    conn: &mut SqliteConnection,
    owner: ThreadId,
) -> anyhow::Result<Option<i64>> {
    let threads: Vec<(String, Option<i64>, Option<i64>)> = sqlx::query_as(concat!(
        tree!(),
        "SELECT tree.id, t.created_at, t.created_at_ms FROM tree LEFT JOIN threads t ON t.id = tree.id"
    ))
    .bind(owner.to_string())
    .fetch_all(&mut *conn)
    .await?;
    if threads.len() > TREE_LIMIT {
        return Ok(None);
    }
    let mut started = i64::MAX;
    for (id, created_at, created_at_ms) in threads {
        let Some(id_ms) = v7_ms(&id) else {
            return Ok(None);
        };
        started = started.min(id_ms);
        if let Some(created) = created_at {
            started = started.min(if created < SECONDS_BELOW {
                created.saturating_mul(1000)
            } else {
                created
            });
        }
        if let Some(created) = created_at_ms {
            started = started.min(created);
        }
    }
    let recorded: Option<i64> = sqlx::query_scalar(concat!(
        tree!(),
        "SELECT min(ms) FROM (
         SELECT utc_day * 86400000 ms FROM draft_accounting_compact_days WHERE thread_id IN (SELECT id FROM tree)
         UNION ALL SELECT json_extract(payload, '$.dispatched_at_ms') FROM draft_accounting_attempts
         WHERE json_extract(payload, '$.thread_id') IN (SELECT id FROM tree))"
    ))
    .bind(owner.to_string())
    .fetch_one(&mut *conn)
    .await?;
    // #314: a day a request of the tree went unrecorded is a day it sent one.
    let unrecorded: Option<i64> = if super::super::gap_days::installed(conn).await? {
        sqlx::query_scalar(concat!(
            tree!(),
            "SELECT min(utc_day) * 86400000 FROM draft_accounting_gap_days
             WHERE thread_id IN (SELECT id FROM tree)"
        ))
        .bind(owner.to_string())
        .fetch_one(&mut *conn)
        .await?
    } else {
        None
    };
    Ok(Some(
        [recorded, unrecorded]
            .into_iter()
            .flatten()
            .fold(started, i64::min),
    ))
}

/// #314 (D228): whether accounting failed to record a request of this
/// conversation or a thread of its spawn tree on `day`. `false` on a ledger
/// before format 7, which can hold no marker.
pub(super) async fn gap_recorded(
    conn: &mut SqliteConnection,
    owner: ThreadId,
    day: i64,
) -> anyhow::Result<bool> {
    if !super::super::gap_days::installed(conn).await? {
        return Ok(false);
    }
    Ok(sqlx::query_scalar(concat!(
        tree!(),
        "SELECT EXISTS(SELECT 1 FROM draft_accounting_gap_days
         WHERE utc_day = ?2 AND thread_id IN (SELECT id FROM tree))"
    ))
    .bind(owner.to_string())
    .bind(day)
    .fetch_one(&mut *conn)
    .await?)
}

fn v7_ms(id: &str) -> Option<i64> {
    let uuid = uuid::Uuid::parse_str(id).ok()?;
    if uuid.get_version_num() != 7 {
        return None;
    }
    let (secs, nanos) = uuid.get_timestamp()?.to_unix();
    i64::try_from(secs)
        .ok()?
        .checked_mul(1000)?
        .checked_add(i64::from(nanos / 1_000_000))
}

/// The UTC day this home's ledger was created: the first recorded request
/// creates it. `None` when the install time cannot be read.
pub(super) async fn ledger_started_day(conn: &mut SqliteConnection) -> anyhow::Result<Option<i64>> {
    let installed: Option<String> = sqlx::query_scalar(
        "SELECT CAST(installed_on AS TEXT) FROM _accounting_migrations WHERE version = 1",
    )
    .fetch_optional(&mut *conn)
    .await
    .context("read ledger install time")?
    .flatten();
    Ok(installed.and_then(|text| {
        chrono::NaiveDateTime::parse_from_str(&text, "%Y-%m-%d %H:%M:%S")
            .ok()
            .map(|time| time.and_utc().timestamp_millis().div_euclid(DAY_MS))
    }))
}

/// `unknown_clear`: no thread of unresolved ancestry has anything on the day,
/// so none of it can be an unlinked subagent of this conversation.
pub(super) fn classify(
    day: i64,
    started_ms: Option<i64>,
    ledger_started_day: Option<i64>,
    unknown_clear: bool,
) -> EmptyDay {
    if let Some(started_day) = started_ms.map(|ms| ms.div_euclid(DAY_MS))
        && unknown_clear
        && day < started_day
    {
        return EmptyDay::BeforeConversation { started_day };
    }
    match ledger_started_day {
        Some(ledger_started_day) if day < ledger_started_day => {
            EmptyDay::BeforeLedger { ledger_started_day }
        }
        _ => EmptyDay::NotRecorded,
    }
}

/// Whether a day before the aggregate floor may still be shown as empty.
pub(super) fn proven_before(day: i64, started_ms: Option<i64>) -> bool {
    started_ms.is_some_and(|ms| day < ms.div_euclid(DAY_MS))
}

#[cfg(test)]
mod tests {
    #[test]
    fn the_tree_query_reads_one_thread_past_the_bound() {
        assert!(tree!().contains(&format!("LIMIT {})", super::TREE_LIMIT + 1)));
    }
}
