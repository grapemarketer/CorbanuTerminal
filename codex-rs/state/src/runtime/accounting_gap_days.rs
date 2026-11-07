//! #314 (D228): the UTC days on which accounting failed to record at least
//! one of a conversation's model requests.
//!
//! Such a request was sent and billed but has no record, so the day's totals
//! may be incomplete; `/cost` says so instead of letting the missing work read
//! as free. A marker names a conversation and a UTC day only. The table comes
//! with ledger format 7, which a ledger takes on only when its first marker is
//! written; every read here first checks that the table exists.
use anyhow::ensure;
use codex_protocol::ThreadId;
use sqlx::SqliteConnection;

const DAY_MS: i64 = 86_400_000;

/// The ledger format that holds gap markers.
pub(in crate::runtime) const FORMAT: i64 = 7;

/// Whether this ledger has the gap-marker table (format 7 or later).
pub(in crate::runtime) async fn installed(conn: &mut SqliteConnection) -> anyhow::Result<bool> {
    Ok(sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM sqlite_schema WHERE name = 'draft_accounting_gap_days' AND type = 'table')",
    )
    .fetch_one(conn)
    .await?)
}

/// A longer span marks its first day and the days from `SPAN_DAYS` before
/// its last through the last (32 days).
const SPAN_DAYS: i64 = 31;

/// The UTC days a gap between `from_ms` and `to_ms` (ordered, nonnegative)
/// marks: the first, the last and up to `SPAN_DAYS` before it, and the
/// checkpoint's day when the clock read behind it.
pub(in crate::runtime) fn days(from_ms: i64, to_ms: i64, checkpoint: Option<i64>) -> Vec<i64> {
    let (first, last) = (from_ms.div_euclid(DAY_MS), to_ms.div_euclid(DAY_MS));
    let mut days = vec![first];
    days.extend(first.max(last - SPAN_DAYS)..=last);
    if let Some(checkpoint) = checkpoint.filter(|checkpoint| *checkpoint > to_ms) {
        days.push(checkpoint.div_euclid(DAY_MS));
    }
    days.sort_unstable();
    days.dedup();
    days
}

/// Mark `owner`'s UTC `day`. The caller holds the write transaction and has
/// upgraded the ledger to `FORMAT`. Marking a day twice is one marker.
pub(in crate::runtime) async fn mark_on_connection(
    conn: &mut SqliteConnection,
    owner: ThreadId,
    day: i64,
) -> anyhow::Result<()> {
    ensure!(day >= 0, "negative gap day");
    sqlx::query(
        "INSERT INTO draft_accounting_gap_days VALUES (?, ?) ON CONFLICT(thread_id, utc_day) DO NOTHING",
    )
    .bind(owner.to_string())
    .bind(day)
    .execute(conn)
    .await?;
    Ok(())
}

/// Remove a deleted conversation's markers with the rest of its accounting.
pub(in crate::runtime) async fn remove_owner(
    conn: &mut SqliteConnection,
    thread: &str,
) -> anyhow::Result<()> {
    if installed(conn).await? {
        sqlx::query("DELETE FROM draft_accounting_gap_days WHERE thread_id = ?")
            .bind(thread)
            .execute(conn)
            .await?;
    }
    Ok(())
}

/// Remove the markers of days whose totals retention has expired at
/// `as_of_ms`, as it expires a compact day: `utc_day * DAY + replay <= as_of`.
pub(in crate::runtime) async fn expire(
    conn: &mut SqliteConnection,
    as_of_ms: i64,
    replay_ms: i64,
) -> anyhow::Result<()> {
    if installed(conn).await? {
        sqlx::query("DELETE FROM draft_accounting_gap_days WHERE utc_day * ? + ? <= ?")
            .bind(DAY_MS)
            .bind(replay_ms)
            .bind(as_of_ms)
            .execute(conn)
            .await?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::DAY_MS;
    use super::days;
    use pretty_assertions::assert_eq;

    /// #314 (D228): a gap marks every day its span covers (the first and the
    /// last 31 of a longer one), and the checkpoint's day when the clock read
    /// behind it.
    #[test]
    fn a_gap_marks_the_days_of_its_span() {
        assert_eq!(
            days(
                /*from_ms*/ 5, /*to_ms*/ 5, /*checkpoint*/ None
            ),
            vec![0]
        );
        assert_eq!(
            days(DAY_MS - 1, 3 * DAY_MS, /*checkpoint*/ None),
            vec![0, 1, 2, 3]
        );
        let long = days(/*from_ms*/ 0, 100 * DAY_MS, /*checkpoint*/ None);
        assert_eq!(long.first(), Some(&0));
        assert_eq!(long[1..], (69..=100).collect::<Vec<_>>());
        assert_eq!(
            days(/*from_ms*/ 0, /*to_ms*/ 0, Some(9 * DAY_MS)),
            vec![0, 9]
        );
        assert_eq!(days(9 * DAY_MS, 9 * DAY_MS, Some(0)), vec![9]);
    }
}
