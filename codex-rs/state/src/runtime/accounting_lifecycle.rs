//! Contribution lifecycle shared by fixtures and atomic normal-library operations.
use super::*;
use crate::runtime::accounting::RetentionFixture;
use crate::runtime::accounting::retention_fixture_on_connection;
use codex_protocol::ThreadId;
use std::collections::BTreeMap;

const DAY_MS: i64 = 86_400_000;
const REPLAY_MS: i64 = crate::runtime::accounting::REPLAY_MS;

#[derive(Debug, PartialEq, Eq)]
pub enum Current<T> {
    Ready(T),
    NeedsEstimate,
    NeedsRefresh,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Metric {
    pub known: i64,
    pub unknown: i64,
}

impl Metric {
    #[cfg_attr(not(test), allow(dead_code))]
    fn full(&self) -> Option<i64> {
        (self.unknown == 0).then_some(self.known)
    }

    fn add(&mut self, value: Option<i64>) -> anyhow::Result<()> {
        let slot = if value.is_some() {
            &mut self.known
        } else {
            &mut self.unknown
        };
        *slot = slot
            .checked_add(value.unwrap_or(1))
            .context("metric overflow")?;
        Ok(())
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct DayTotals {
    // Input, noncached, read, write, output, reasoning, total: never price subsets twice.
    pub measured: [Metric; 7],
    /// Money actually charged. Plan work contributes nothing here, because a plan
    /// charges capacity rather than tokens.
    pub known_usd: Decimal,
    pub unknown_estimates: i64,
    pub attempts: i64,
    /// What the plan work of this day would have cost at the catalogue's API
    /// rates, and how many plan attempts could not be stated that way.
    pub equivalent_usd: Decimal,
    pub unknown_equivalents: i64,
    /// Plan consumption: total tokens scaled by the rate that applied, 1000
    /// meaning 1.0x. `unknown` counts plan attempts with no stateable figure.
    pub plan_burn_milli_tokens: Metric,
    pub plan_attempts: i64,
    /// Attempts that ran on the user's own machine (`Basis::Local`).
    pub local_attempts: i64,
    /// Attempts with no declared billing basis (`Basis::Undeclared`).
    pub undeclared_attempts: i64,
}

impl DayTotals {
    /// Sum each supplied attempt once using the ledger's exact arithmetic.
    pub fn from_quotes<'a>(
        quotes: impl IntoIterator<Item = &'a ObservationQuote>,
    ) -> anyhow::Result<Self> {
        let mut totals = Self::default();
        for quote in quotes {
            totals.add(quote)?;
        }
        Ok(totals)
    }

    /// Attempts billed per token. Every other attempt - subscription, local or
    /// undeclared - has no billed price by construction, so it is also counted
    /// in `unknown_estimates`; `per_use_unknown` takes them back out.
    pub fn per_use_attempts(&self) -> i64 {
        self.attempts
            .saturating_sub(self.plan_attempts)
            .saturating_sub(self.local_attempts)
            .saturating_sub(self.undeclared_attempts)
    }

    /// Pay-per-use attempts with no complete estimate.
    pub fn per_use_unknown(&self) -> i64 {
        self.unknown_estimates
            .saturating_sub(self.plan_attempts)
            .saturating_sub(self.local_attempts)
            .saturating_sub(self.undeclared_attempts)
    }

    #[cfg_attr(not(test), allow(dead_code))]
    fn full_usd(&self) -> Option<Decimal> {
        (self.attempts > 0 && self.unknown_estimates == 0).then_some(self.known_usd)
    }

    fn add(&mut self, quote: &ObservationQuote) -> anyhow::Result<()> {
        let u = &quote.usage;
        for (metric, value) in self.measured.iter_mut().zip([
            u.input,
            u.noncached,
            u.read,
            u.write,
            u.output,
            u.reasoning,
            u.total,
        ]) {
            metric.add(value)?;
        }
        self.known_usd = self.known_usd.add(quote.known_subtotal)?;
        self.unknown_estimates = self
            .unknown_estimates
            .checked_add(i64::from(quote.all_buckets_priced.is_none()))
            .context("estimate count overflow")?;
        match quote.basis() {
            Basis::Local => {
                self.local_attempts = self
                    .local_attempts
                    .checked_add(1)
                    .context("local attempt count overflow")?;
            }
            Basis::Undeclared => {
                self.undeclared_attempts = self
                    .undeclared_attempts
                    .checked_add(1)
                    .context("undeclared attempt count overflow")?;
            }
            Basis::Billed | Basis::PlanEquivalent => {}
        }
        let plan = quote.is_plan();
        if plan {
            self.equivalent_usd = self.equivalent_usd.add(quote.known_equivalent)?;
            self.unknown_equivalents = self
                .unknown_equivalents
                .checked_add(i64::from(quote.all_buckets_equivalent.is_none()))
                .context("equivalent count overflow")?;
            self.plan_burn_milli_tokens
                .add(quote.plan_burn_milli_tokens)?;
            self.plan_attempts = self
                .plan_attempts
                .checked_add(1)
                .context("plan attempt count overflow")?;
        }
        self.attempts = self
            .attempts
            .checked_add(1)
            .context("attempt count overflow")?;
        Ok(())
    }
}

#[cfg_attr(not(test), allow(dead_code))]
struct Lifecycle<'a> {
    estimates: EstimateStore<'a>,
}

impl<'a> Lifecycle<'a> {
    #[cfg(test)]
    async fn create_for_tests(runtime: &'a StateRuntime) -> anyhow::Result<Self> {
        let estimates = EstimateStore::create_for_tests(runtime).await?;
        sqlx::raw_sql(
            "CREATE TABLE draft_accounting_contributions (
                attempt_id TEXT PRIMARY KEY NOT NULL, thread_id TEXT NOT NULL,
                utc_day INTEGER NOT NULL CHECK(utc_day >= 0), evidence TEXT NOT NULL,
                FOREIGN KEY(attempt_id, evidence) REFERENCES draft_accounting_estimates(attempt_id, evidence));
             CREATE INDEX draft_accounting_contribution_day ON draft_accounting_contributions(thread_id, utc_day);
             CREATE TABLE draft_accounting_tombstones (
                attempt_id TEXT PRIMARY KEY NOT NULL,
                expires_at_ms INTEGER NOT NULL CHECK(expires_at_ms >= 0));",
        ).execute(runtime.pool.as_ref()).await?;
        Ok(Self { estimates })
    }

    #[cfg_attr(not(test), allow(dead_code))]
    async fn refresh_current(&self, id: Uuid) -> anyhow::Result<Current<()>> {
        let mut tx =
            crate::runtime::db_lock::begin_immediate(&self.estimates.journal.runtime.pool).await?;
        let result = Journal::refresh_contribution_on_connection(&mut tx, id).await;
        match result {
            Ok(value) => {
                tx.commit().await?;
                Ok(value)
            }
            Err(error) => {
                tx.rollback().await?;
                Err(error)
            }
        }
    }

    #[cfg_attr(not(test), allow(dead_code))]
    async fn read_day(&self, thread: ThreadId, day: i64) -> anyhow::Result<Current<DayTotals>> {
        ensure!(day >= 0, "negative day");
        let mut tx = self.estimates.journal.runtime.pool.begin().await?;
        ensure!(
            matches!(
                retention_fixture_on_connection(&mut tx).await?,
                RetentionFixture::Absent
            ),
            "installed retention requires read_retained_day"
        );
        let attempts = owned_attempts(&mut tx, thread).await?;
        let rows: Vec<(String, String)> = sqlx::query_as(
            "SELECT attempt_id, evidence FROM draft_accounting_contributions WHERE thread_id = ? AND utc_day = ?",
        ).bind(thread.to_string()).bind(day).fetch_all(&mut *tx).await?;
        let mut references: BTreeMap<_, _> = rows.into_iter().collect();
        let mut totals = DayTotals::default();
        let mut stale = false;
        for (attempt, observations) in attempts {
            if i64::from(attempt.dispatched_at_ms) / DAY_MS != day {
                continue;
            }
            let reference: Option<(String, i64, String)> = sqlx::query_as(
                "SELECT thread_id, utc_day, evidence FROM draft_accounting_contributions WHERE attempt_id = ?",
            ).bind(attempt.attempt_id.to_string()).fetch_optional(&mut *tx).await?;
            if let Some((owner, bucket, evidence)) = reference {
                let quote =
                    validate_reference(&mut tx, &attempt, &owner, bucket, &evidence).await?;
                references.remove(&attempt.attempt_id.to_string());
                stale |= evidence != serde_json::to_string(&observations)?;
                totals.add(&quote)?;
            } else {
                stale = true;
            }
        }
        ensure!(references.is_empty(), "foreign contribution attribution");
        tx.commit().await?;
        Ok(if stale {
            Current::NeedsRefresh
        } else {
            Current::Ready(totals)
        })
    }

    #[cfg_attr(not(test), allow(dead_code))]
    async fn delete_recorded_thread(&self, thread: ThreadId, as_of_ms: i64) -> anyhow::Result<()> {
        ensure!(as_of_ms >= 0, "negative deletion time");
        let mut tx =
            crate::runtime::db_lock::begin_immediate(&self.estimates.journal.runtime.pool).await?;
        let result = async {
            if !matches!(retention_fixture_on_connection(&mut tx).await?, RetentionFixture::Absent) {
                return retention_plan::reduction::atomic::delete_on_connection(&mut tx, thread, as_of_ms).await;
            }
            let attempts = owned_attempts(&mut tx, thread).await?;
            let mut snapshots = HashSet::new();
            for (attempt, _) in attempts {
                let dispatch = i64::from(attempt.dispatched_at_ms);
                ensure!(dispatch <= as_of_ms, "future dispatch");
                let expiry = dispatch.checked_add(REPLAY_MS).context("expiry overflow")?;
                let id = attempt.attempt_id.to_string();
                if let Some(Some(snapshot)) = binding(&mut tx, attempt.attempt_id).await? {
                    snapshots.insert(snapshot);
                }
                sqlx::query("INSERT INTO draft_accounting_tombstones VALUES (?, ?) ON CONFLICT(attempt_id) DO NOTHING")
                    .bind(&id).bind(expiry).execute(&mut *tx).await?;
                for sql in [
                    "DELETE FROM draft_accounting_contributions WHERE attempt_id = ?",
                    "DELETE FROM draft_accounting_estimates WHERE attempt_id = ?",
                    "DELETE FROM draft_accounting_price_bindings WHERE attempt_id = ?",
                    "DELETE FROM draft_accounting_observations WHERE attempt_id = ?",
                    "DELETE FROM draft_accounting_attempts WHERE attempt_id = ?",
                ] {
                    sqlx::query(sql)
                        .bind(&id).execute(&mut *tx).await?;
                }
            }
            for snapshot in snapshots {
                sqlx::query("DELETE FROM draft_accounting_price_snapshots WHERE snapshot_id = ? AND NOT EXISTS (SELECT 1 FROM draft_accounting_price_bindings WHERE snapshot_id = ?)")
                    .bind(&snapshot).bind(&snapshot).execute(&mut *tx).await?;
            }
            anyhow::Ok(())
        }.await;
        match result {
            Ok(()) => tx.commit().await,
            Err(error) => {
                tx.rollback().await?;
                return Err(error);
            }
        }?;
        Ok(())
    }
}

async fn validate_reference(
    conn: &mut SqliteConnection,
    attempt: &Attempt,
    thread: &str,
    day: i64,
    evidence: &str,
) -> anyhow::Result<ObservationQuote> {
    ensure!(
        thread == attempt.thread_id.to_string()
            && day == i64::from(attempt.dispatched_at_ms) / DAY_MS,
        "contribution attribution mismatch"
    );
    EstimateStore::read_on_connection(conn, attempt.attempt_id, evidence)
        .await?
        .context("missing referenced estimate")
}

async fn owned_attempts(
    conn: &mut SqliteConnection,
    thread: ThreadId,
) -> anyhow::Result<Vec<(Attempt, Vec<Observation>)>> {
    // Ownership lives in typed JSON: parse every row before filtering, never hide malformed owners.
    let ids: Vec<String> =
        sqlx::query_scalar("SELECT attempt_id FROM draft_accounting_attempts ORDER BY attempt_id")
            .fetch_all(&mut *conn)
            .await?;
    let mut owned = Vec::new();
    for id in ids {
        let parsed = Uuid::parse_str(&id)?;
        ensure!(parsed.to_string() == id, "noncanonical attempt key");
        let record = authority(conn, parsed).await?;
        if record.0.thread_id == thread {
            owned.push(record);
        }
    }
    Ok(owned)
}

#[cfg(test)]
#[path = "accounting_lifecycle_tests.rs"]
mod tests;

#[path = "accounting_compact_values.rs"]
mod compact_values;

#[path = "accounting_retention_plan.rs"]
mod retention_plan;
pub use retention_plan::reduction::atomic::RetainedDay;
pub use retention_plan::reduction::atomic::RetentionCoverage;

impl Journal<'_> {
    #[cfg(test)]
    pub(in crate::runtime::accounting) async fn inspect_on_connection(
        conn: &mut SqliteConnection,
        owner: ThreadId,
        day: i64,
        read_at_ms: i64,
    ) -> anyhow::Result<crate::accounting::InspectionDay> {
        let mut work = crate::runtime::accounting::store::InspectionWork::new(conn).await?;
        Self::inspect_window_on_connection(
            conn, owner, day, read_at_ms, /*window*/ None, &mut work,
        )
        .await
    }

    pub(in crate::runtime::accounting) async fn inspect_window_on_connection(
        conn: &mut SqliteConnection,
        owner: ThreadId,
        day: i64,
        read_at_ms: i64,
        window: Option<(i64, i64)>,
        work: &mut crate::runtime::accounting::store::InspectionWork,
    ) -> anyhow::Result<crate::accounting::InspectionDay> {
        use crate::accounting::Inspection;
        use crate::accounting::InspectionDay;
        if !work.visit() {
            return Ok(InspectionDay::TooLarge);
        }
        let checkpoint = match retention_fixture_on_connection(conn).await? {
            RetentionFixture::Active(time) => time,
            RetentionFixture::Staging => return Ok(InspectionDay::CheckpointLag),
            RetentionFixture::Absent => anyhow::bail!("missing retention schema"),
        };
        if read_at_ms < checkpoint {
            return Err(crate::runtime::accounting::store::ClockBehindLedger {
                behind_ms: checkpoint.saturating_sub(read_at_ms),
            }
            .into());
        }
        if day > checkpoint / DAY_MS {
            return Ok(InspectionDay::CheckpointLag);
        }
        let start = day.checked_mul(DAY_MS).context("day overflow")?;
        let (lower, upper) = window.unwrap_or((start, start + DAY_MS));
        if !work.scans(/*count*/ 5) {
            return Ok(InspectionDay::TooLarge);
        }
        // Reject malformed/noncanonical ownership before the SQL pre-filter;
        // typed ownership is reasserted below, never silently normalized.
        let malformed: bool = sqlx::query_scalar(
            "SELECT EXISTS(SELECT 1 FROM draft_accounting_attempts WHERE
             json_type(payload, '$.thread_id') IS NOT 'text' OR
             json_type(payload, '$.dispatched_at_ms') IS NOT 'integer' OR
             length(json_extract(payload, '$.thread_id')) != 36 OR
             substr(json_extract(payload, '$.thread_id'), 9, 1) != '-' OR
             substr(json_extract(payload, '$.thread_id'), 14, 1) != '-' OR
             substr(json_extract(payload, '$.thread_id'), 19, 1) != '-' OR
             substr(json_extract(payload, '$.thread_id'), 24, 1) != '-' OR
             length(replace(json_extract(payload, '$.thread_id'), '-', '')) != 32 OR
             replace(json_extract(payload, '$.thread_id'), '-', '') GLOB '*[^0-9a-f]*')",
        )
        .fetch_one(&mut *conn)
        .await?;
        ensure!(!malformed, "invalid accounting ownership");
        let selected: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM draft_accounting_attempts WHERE json_extract(payload, '$.thread_id') = ?
             AND json_extract(payload, '$.dispatched_at_ms') >= ? AND json_extract(payload, '$.dispatched_at_ms') < ?",
        ).bind(owner.to_string()).bind(lower).bind(upper).fetch_one(&mut *conn).await?;
        if selected > 512 {
            return Ok(InspectionDay::TooLarge);
        }
        // Inspection does not invoke the whole-store retention planner. Validate
        // only selected attempts and their original evidence in this read snapshot.
        let ids: Vec<String> = sqlx::query_scalar(
            "SELECT attempt_id FROM draft_accounting_attempts WHERE json_extract(payload, '$.thread_id') = ?
             AND json_extract(payload, '$.dispatched_at_ms') >= ? AND json_extract(payload, '$.dispatched_at_ms') < ?
             ORDER BY attempt_id",
        ).bind(owner.to_string()).bind(lower).bind(upper).fetch_all(&mut *conn).await?;
        let orphan: bool = sqlx::query_scalar(
            "SELECT EXISTS(SELECT 1 FROM draft_accounting_contributions c
             LEFT JOIN draft_accounting_attempts a ON a.attempt_id = c.attempt_id
             WHERE c.thread_id = ? AND c.utc_day = ? AND (a.attempt_id IS NULL OR
             json_extract(a.payload, '$.thread_id') != c.thread_id OR
             json_extract(a.payload, '$.dispatched_at_ms') / 86400000 != c.utc_day))",
        )
        .bind(owner.to_string())
        .bind(day)
        .fetch_one(&mut *conn)
        .await?;
        ensure!(!orphan, "foreign contribution attribution");
        let mut records = Vec::new();
        let mut totals = DayTotals::default();
        let mut stale = false;
        let mut lag = false;
        let mut materialized = 8192;
        for id in ids {
            if !work.visit() {
                return Ok(InspectionDay::TooLarge);
            }
            // Bound selected payloads and the observation vector before decoding.
            // Historical estimate versions are validated individually below.
            let (observations, bytes): (i64, i64) = sqlx::query_as(
                "SELECT (SELECT count(*) FROM draft_accounting_observations WHERE attempt_id = ?1),
                 coalesce(max(n), 0) FROM (
                 SELECT length(CAST(payload AS BLOB)) n FROM draft_accounting_attempts WHERE attempt_id = ?1 UNION ALL
                 SELECT sum(length(CAST(payload AS BLOB))) FROM draft_accounting_observations WHERE attempt_id = ?1 UNION ALL
                 SELECT length(CAST(evidence AS BLOB)) FROM draft_accounting_estimates WHERE attempt_id = ?1 UNION ALL
                 SELECT length(CAST(payload AS BLOB)) FROM draft_accounting_estimates WHERE attempt_id = ?1 UNION ALL
                 SELECT length(CAST(evidence AS BLOB)) FROM draft_accounting_contributions WHERE attempt_id = ?1 UNION ALL
                 SELECT length(CAST(payload AS BLOB)) FROM draft_accounting_price_snapshots
                 WHERE snapshot_id IN (SELECT snapshot_id FROM draft_accounting_price_bindings WHERE attempt_id = ?1))",
            ).bind(&id).fetch_one(&mut *conn).await?;
            if observations > 4096 || bytes > 4 * 1024 * 1024 {
                return Ok(InspectionDay::TooLarge);
            }
            let uuid = Uuid::parse_str(&id)?;
            ensure!(uuid.to_string() == id, "noncanonical attempt key");
            let deleted: bool = sqlx::query_scalar(
                "SELECT EXISTS(SELECT 1 FROM draft_accounting_tombstones WHERE attempt_id = ?)",
            )
            .bind(&id)
            .fetch_one(&mut *conn)
            .await?;
            ensure!(!deleted, "raw/tombstone collision");
            let (attempt, observations) = authority(conn, uuid).await?;
            ensure!(attempt.thread_id == owner, "inspection owner mismatch");
            let binding = binding(conn, uuid)
                .await?
                .context("missing original binding")?;
            // Keep corrupt old versions visible without collecting their combined
            // evidence or refusing a small packet because it has a long history.
            let mut after: Option<String> = None;
            let mut versions = 0;
            loop {
                if !work.visit() {
                    return Ok(InspectionDay::TooLarge);
                }
                let evidence: Option<String> = sqlx::query_scalar(
                    "SELECT evidence FROM draft_accounting_estimates WHERE attempt_id = ?1
                     AND (?2 IS NULL OR evidence > ?2) ORDER BY evidence LIMIT 1",
                )
                .bind(&id)
                .bind(&after)
                .fetch_optional(&mut *conn)
                .await?;
                let Some(evidence) = evidence else { break };
                EstimateStore::read_on_connection(conn, uuid, &evidence)
                    .await?
                    .context("missing retained estimate")?;
                after = Some(evidence);
                versions += 1;
            }
            // The shared helper collects versions: use it only for bounded history.
            let quote = if versions <= 1 {
                Journal::inspect_quote_on_connection(conn, uuid).await?
            } else {
                recorded_or_current(conn, &attempt, &observations, binding).await?
            };
            let dispatch = i64::from(quote.attempt.dispatched_at_ms);
            ensure!(dispatch <= checkpoint, "future dispatch");
            ensure!(
                dispatch / DAY_MS == day && dispatch >= lower && dispatch < upper,
                "inspection dispatch mismatch"
            );
            if let Some(snapshot) = &quote.snapshot {
                ensure!(
                    i64::from(snapshot.observed_at_ms) <= checkpoint
                        && i64::from(snapshot.approved_at_ms) <= checkpoint,
                    "future snapshot evidence"
                );
            }
            materialized += serde_json::to_vec(&quote)?.len() + 2048;
            if materialized > 4 * 1024 * 1024 {
                return Ok(InspectionDay::TooLarge);
            }
            let reference: Option<(String, i64, String)> = sqlx::query_as(
                "SELECT thread_id, utc_day, evidence FROM draft_accounting_contributions WHERE attempt_id = ?",
            ).bind(&id).fetch_optional(&mut *conn).await?;
            let current = if let Some((thread, bucket, evidence)) = reference {
                ensure!(
                    thread == owner.to_string() && bucket == day,
                    "contribution attribution mismatch"
                );
                if EstimateStore::read_on_connection(conn, uuid, &evidence)
                    .await?
                    .is_none()
                {
                    false
                } else {
                    evidence == serde_json::to_string(&quote.observations)?
                }
            } else {
                false
            };
            lag |= dispatch <= checkpoint - 90 * DAY_MS;
            stale |= !current && dispatch > checkpoint - 90 * DAY_MS;
            // Preserve overflow validation even when contributions need refresh.
            totals.add(&quote)?;
            records.push(quote);
        }
        if stale {
            return Ok(InspectionDay::NeedsRefresh);
        }
        if lag {
            return Ok(InspectionDay::CheckpointLag);
        }
        let coverage = RetentionCoverage {
            completed_as_of_ms: checkpoint,
            detail_expired_through_ms: (checkpoint >= 90 * DAY_MS)
                .then(|| checkpoint - 90 * DAY_MS),
            aggregate_day_floor: crate::runtime::accounting::aggregate_day_floor(checkpoint),
            oldest_recorded_day: sqlx::query_scalar(
                "SELECT min(day) FROM (
                 SELECT utc_day day FROM draft_accounting_compact_days WHERE thread_id = ?1
                 UNION ALL SELECT json_extract(payload, '$.dispatched_at_ms') / 86400000
                 FROM draft_accounting_attempts WHERE json_extract(payload, '$.thread_id') = ?1)",
            )
            .bind(owner.to_string())
            .fetch_one(&mut *conn)
            .await?,
        };
        let compact: bool = sqlx::query_scalar(
            "SELECT EXISTS(SELECT 1 FROM draft_accounting_compact_days WHERE thread_id = ? AND utc_day = ?)",
        ).bind(owner.to_string()).bind(day).fetch_one(&mut *conn).await?;
        // A compact prefix cannot supply detail, but retained raw suffixes can.
        // A whole day with nothing recorded for this owner loses no detail to
        // retention; the tree inspection decides what that emptiness shows.
        // #1196: so does a range bucket's part of such a day, inside the daily
        // totals. A raw row elsewhere on the day keeps the window unavailable,
        // and so does the floor's edge: expiry can run ahead of the checkpoint,
        // so the floor is taken at the read instead.
        let recorded = !records.is_empty() || compact;
        let expired = read_at_ms >= 90 * DAY_MS && lower <= read_at_ms - 90 * DAY_MS;
        let unrecorded_day = window.is_some()
            && !recorded
            && expired
            && day >= crate::runtime::accounting::aggregate_day_floor(read_at_ms)
            // Without budget for this check, this day's tree scan and the
            // start's, the window stays unavailable. Later days and this day's
            // candidates are not reserved for: a range near the budget can
            // still become too large, which claims nothing.
            && work.spare_scan(/*reserve*/ 2)
            && !sqlx::query_scalar::<_, bool>(
                "SELECT EXISTS(SELECT 1 FROM draft_accounting_attempts
                 WHERE json_extract(payload, '$.thread_id') = ?
                 AND json_extract(payload, '$.dispatched_at_ms') / 86400000 = ?)",
            )
            .bind(owner.to_string())
            .bind(day)
            .fetch_one(&mut *conn)
            .await?;
        if (recorded || (window.is_some() && !unrecorded_day))
            && (day < coverage.aggregate_day_floor || (compact && window.is_none()) || expired)
        {
            return Ok(InspectionDay::DetailUnavailable {
                coverage,
                read_at_ms,
                compact,
            });
        }
        let mut requests: BTreeMap<Uuid, Vec<ObservationQuote>> = BTreeMap::new();
        // Budget the packet header, totals, map keys and per-row containers too.
        let mut projected = 8192usize;
        for quote in records {
            let attempt = &quote.attempt;
            // Check logical ownership across days and threads, including retries.
            let mut after = String::new();
            loop {
                if !work.visit() {
                    return Ok(InspectionDay::TooLarge);
                }
                // A logical request may have arbitrarily many cross-day retries.
                // Validate one bounded payload at a time rather than collecting them.
                let sibling: Option<(String, Option<String>)> = sqlx::query_as(
                    "SELECT attempt_id, CASE WHEN length(CAST(payload AS BLOB)) <= 4194304 THEN payload END
                     FROM draft_accounting_attempts WHERE request_id = ? AND attempt_id > ?
                     ORDER BY attempt_id LIMIT 1",
                ).bind(attempt.request_id.to_string()).bind(&after).fetch_optional(&mut *conn).await?;
                let Some((id, payload)) = sibling else { break };
                let sibling: Attempt =
                    serde_json::from_str(&payload.context("oversized request identity")?)?;
                sibling.validate()?;
                ensure!(attempt.same_owner(&sibling), "request owner mismatch");
                after = id;
            }
            let mut predecessor = attempt.retry_of;
            let mut seen = HashSet::from([attempt.attempt_id]);
            while let Some(id) = predecessor {
                if !work.visit() {
                    return Ok(InspectionDay::TooLarge);
                }
                ensure!(seen.insert(id), "retry cycle");
                let parent = read_attempt(conn, id).await?;
                // Compaction can legitimately remove a cross-day predecessor.
                let Some(parent) = parent else { break };
                ensure!(attempt.same_owner(&parent), "retry owner mismatch");
                predecessor = parent.retry_of;
            }
            projected = projected
                .checked_add(serde_json::to_vec(&quote)?.len() + 2048)
                .context("inspection size overflow")?;
            if projected > 4 * 1024 * 1024 {
                return Ok(InspectionDay::TooLarge);
            }
            requests.entry(attempt.request_id).or_default().push(quote);
        }
        Ok(InspectionDay::Ready(Inspection {
            owner,
            utc_day: day,
            read_at_ms,
            coverage,
            own_totals: totals.clone(),
            descendant_totals: DayTotals::default(),
            unknown_parent_totals: DayTotals::default(),
            unknown_parent_unavailable_threads: 0,
            unknown_parent_requests: BTreeMap::new(),
            other_conversations: None,
            empty_day: None,
            recording_gap: false,
            totals,
            requests,
        }))
    }

    pub(in crate::runtime::accounting) async fn refresh_contribution_on_connection(
        conn: &mut SqliteConnection,
        id: Uuid,
    ) -> anyhow::Result<Current<()>> {
        let deleted: bool = sqlx::query_scalar(
            "SELECT EXISTS(SELECT 1 FROM draft_accounting_tombstones WHERE attempt_id = ?)",
        )
        .bind(id.to_string())
        .fetch_one(&mut *conn)
        .await?;
        ensure!(!deleted, "deleted attempt");
        let (attempt, observations) = authority(conn, id).await?;
        let evidence = serde_json::to_string(&observations)?;
        if EstimateStore::read_on_connection(conn, id, &evidence)
            .await?
            .is_none()
        {
            return Ok(Current::NeedsEstimate);
        }
        let old: Option<(String, i64, String)> = sqlx::query_as(
            "SELECT thread_id, utc_day, evidence FROM draft_accounting_contributions WHERE attempt_id = ?",
        ).bind(id.to_string()).fetch_optional(&mut *conn).await?;
        if let Some((thread, day, old_evidence)) = old {
            validate_reference(conn, &attempt, &thread, day, &old_evidence).await?;
            if old_evidence == evidence {
                return Ok(Current::Ready(()));
            }
        }
        sqlx::query("INSERT INTO draft_accounting_contributions VALUES (?, ?, ?, ?) ON CONFLICT(attempt_id) DO UPDATE SET evidence = excluded.evidence")
            .bind(id.to_string()).bind(attempt.thread_id.to_string())
            .bind(i64::from(attempt.dispatched_at_ms) / DAY_MS).bind(evidence)
            .execute(&mut *conn).await?;
        anyhow::Ok(Current::Ready(()))
    }
}
