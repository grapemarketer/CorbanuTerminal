//! Explicit, versioned accounting storage. No collection or user-facing activation switch.
use super::Journal;
use super::StateRuntime;
use anyhow::Context;
use anyhow::ensure;
use codex_protocol::ThreadId;
use sqlx::SqliteConnection;

pub use super::pricing::Basis;
pub use super::pricing::BasisSource;
pub use super::pricing::BucketQuote;
pub use super::pricing::Currency;
pub use super::pricing::Current;
pub use super::pricing::DayTotals;
pub use super::pricing::Decimal;
pub use super::pricing::DisplayAmount;
pub use super::pricing::LongContext;
pub use super::pricing::Metric;
pub use super::pricing::NewerLedgerFormat;
pub use super::pricing::ObservationQuote;
pub use super::pricing::Rates;
pub use super::pricing::RetainedDay;
pub use super::pricing::RetentionCoverage;
pub use super::pricing::Snapshot;
pub use super::pricing::SourceKind;
pub use super::pricing::Unit;
pub use super::pricing::is_newer_format;
pub use super::types::Attempt;
pub use super::types::AttemptAccount;
pub use super::types::Count;
pub use super::types::DEFAULT_ACCOUNT;
pub use super::types::Dialect;
pub use super::types::Observation;
pub use super::types::Patch;
pub use super::types::Presence;
pub use super::types::Usage;
pub use scope::DeletedAttempts;
pub use scope::OtherConversations;

pub use writer::AccountingWriter;
pub use writer::LedgerWrite;
pub use writer::PENDING_DIR;
pub use writer::WriteOutcome;

#[path = "accounting_scope.rs"]
mod scope;

#[path = "accounting_empty_day.rs"]
mod empty_day;

#[path = "accounting_indexes.rs"]
mod indexes;

#[path = "accounting_writer.rs"]
mod writer;

/// One UTC day's immutable, recorded-only explanation. No collection coverage claim.
#[derive(Debug, PartialEq, Eq)]
pub struct Inspection {
    pub owner: ThreadId,
    pub utc_day: i64,
    pub read_at_ms: i64,
    pub coverage: RetentionCoverage,
    /// Resolved root plus descendants; unresolved ancestry is excluded.
    pub totals: DayTotals,
    pub own_totals: DayTotals,
    pub descendant_totals: DayTotals,
    pub unknown_parent_totals: DayTotals,
    /// Unresolved owners whose day could not be inspected; no amount or coverage inferred.
    pub unknown_parent_unavailable_threads: usize,
    pub unknown_parent_requests: std::collections::BTreeMap<uuid::Uuid, Vec<ObservationQuote>>,
    pub requests: std::collections::BTreeMap<uuid::Uuid, Vec<ObservationQuote>>,
    /// Same-day attempts of other root conversations, outside every total above.
    /// `None` when this view did not read them (range buckets).
    pub other_conversations: Option<OtherConversations>,
    /// What the emptiness of the day, or of a range bucket's part of it,
    /// means when neither this conversation nor a resolved subagent has a
    /// recorded attempt there. `None` when they have one.
    pub empty_day: Option<EmptyDay>,
    /// #314 (D228): accounting failed to record at least one model request of
    /// this conversation or a thread of its spawn tree on this UTC day
    /// (`AccountingStore::record_gap`), so the totals above may be
    /// incomplete: that request was sent and billed but has no record.
    pub recording_gap: bool,
}

/// A day on which neither the conversation nor a resolved subagent has a
/// recorded attempt. Only `BeforeConversation` says there were no requests:
/// the ledger keeps no record of when a writer ran, and marks only the writes
/// known to have failed (#314), so an empty day inside the ledger's life
/// proves nothing.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EmptyDay {
    /// Proven: the whole UTC day ends before the conversation started, and no
    /// thread of unresolved ancestry has anything on it. `started_day` is the
    /// UTC day of the earliest of every spawn-tree thread's UUIDv7 id time, its
    /// stored creation times and every request recorded for it. As good as the
    /// clock that stamped them, the same clock that dates every request.
    BeforeConversation { started_day: i64 },
    /// Not proven: the day ends before this home's ledger was created.
    BeforeLedger { ledger_started_day: i64 },
    /// Not proven: nothing is recorded; collection coverage is unknown.
    NotRecorded,
}

/// UTC half-open requested interval and calendar-aligned grouping.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct InspectionRange {
    pub start_ms: i64,
    pub end_ms: i64,
    pub grouping: InspectionGrouping,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InspectionGrouping {
    Hour,
    Day,
    Week,
    Month,
}

impl InspectionRange {
    pub fn validate(self) -> anyhow::Result<()> {
        ensure!(
            self.start_ms >= 0,
            "invalid range: start precedes Unix epoch"
        );
        ensure!(
            self.end_ms != self.start_ms,
            "empty range: start equals end"
        );
        ensure!(
            self.end_ms > self.start_ms,
            "reversed range: end precedes start"
        );
        ensure!(
            chrono::DateTime::from_timestamp_millis(self.end_ms).is_some(),
            "invalid range: timestamp overflow"
        );
        Ok(())
    }

    /// Full aligned bounds; callers must not total a partially covered bucket.
    pub fn bucket(self, time: i64) -> anyhow::Result<(i64, i64)> {
        use chrono::Datelike;
        let date = chrono::DateTime::from_timestamp_millis(time)
            .context("invalid timestamp")?
            .date_naive();
        let (start, end) = match self.grouping {
            InspectionGrouping::Hour => {
                return Ok((
                    time.div_euclid(3_600_000) * 3_600_000,
                    (time.div_euclid(3_600_000) + 1) * 3_600_000,
                ));
            }
            InspectionGrouping::Day => (date, date.succ_opt().context("day overflow")?),
            InspectionGrouping::Week => {
                let start =
                    date - chrono::Duration::days(i64::from(date.weekday().num_days_from_monday()));
                (
                    start,
                    start
                        .checked_add_signed(chrono::Duration::days(7))
                        .context("week overflow")?,
                )
            }
            InspectionGrouping::Month => {
                let start = date.with_day(1).context("month start")?;
                (
                    start,
                    start
                        .checked_add_months(chrono::Months::new(1))
                        .context("month overflow")?,
                )
            }
        };
        Ok((
            start
                .and_hms_opt(0, 0, 0)
                .context("midnight")?
                .and_utc()
                .timestamp_millis(),
            end.and_hms_opt(0, 0, 0)
                .context("midnight")?
                .and_utc()
                .timestamp_millis(),
        ))
    }
}

#[derive(Debug, PartialEq, Eq)]
pub struct InspectionBucket {
    pub start_ms: i64,
    pub end_ms: i64,
    pub effective: Option<(i64, i64)>,
    pub partial: bool,
    pub days: Vec<InspectionDay>,
}

#[allow(clippy::large_enum_variant)]
/// Unavailability never carries a partial amount.
#[derive(Debug, PartialEq, Eq)]
pub enum InspectionDay {
    Range {
        requested: InspectionRange,
        /// First UTC day whose daily totals are kept; `None` while the ledger
        /// has no completed checkpoint.
        aggregate_day_floor: Option<i64>,
        /// Inclusive completed checkpoint from this read snapshot, also when
        /// every requested bucket starts after it. No checkpoint while staging.
        completed_as_of_ms: Option<i64>,
        read_at_ms: i64,
        buckets: Vec<InspectionBucket>,
    },
    Absent,
    MissingThread,
    /// The requested view is beyond, or has no completed, maintenance checkpoint.
    CheckpointLag,
    NeedsRefresh,
    TooLarge,
    DetailUnavailable {
        coverage: RetentionCoverage,
        read_at_ms: i64,
        compact: bool,
    },
    Ready(Inspection),
}

#[allow(clippy::large_enum_variant)]
/// Original dispatch-time evidence, never a catalog to select a replacement from.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum OriginalPriceEvidence {
    Bound(Snapshot),
    Unpriced,
}

/// A complete immutable original-evidence snapshot, not an incremental event feed.
/// Completeness is the caller's contract; interrupted attempts may contain unknowns.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RetainedImport {
    pub attempt: Attempt,
    pub observations: Vec<Observation>,
    pub original_price: OriginalPriceEvidence,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RetainedImportOutcome {
    Imported,
    /// Only UUID/expiry survives; this does not certify payload equality.
    SuppressedByReplayRecord,
    /// Conservative daily expiry discards totals, but retains the replay fence.
    ExpiredAggregateDay,
}

/// Opt-in handle over the native state database. Dropping it stops no background
/// task: there is no collector. Native deletion still cleans installed accounting.
pub struct AccountingStore<'a> {
    runtime: &'a StateRuntime,
}

/// The time a write is recorded as of.
///
/// Several Corbanu processes can share one home. A time read before waiting for
/// the write lock can fall behind a checkpoint another process commits during
/// the wait, and the store rejects backward time. `Now` is read only once the
/// write lock is held, so it cannot. `At` keeps an explicit time, which tests
/// and imports need; a backward explicit time is still rejected.
///
/// `Now` is also never behind the ledger's own checkpoint. A wall clock that
/// stepped back (NTP, a VM restore, a hand-set clock) used to make every write
/// fail as a backward checkpoint, and with it every model request. The ledger
/// time holds at the checkpoint until the clock passes it again; each attempt
/// still carries the dispatch time the clock actually read.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AsOf {
    At(i64),
    Now,
}

impl AsOf {
    fn sample(self) -> i64 {
        match self {
            Self::At(as_of_ms) => as_of_ms,
            Self::Now => chrono::Utc::now().timestamp_millis(),
        }
    }

    /// `sample`, with `Now` held at the checkpoint `conn`'s transaction sees.
    /// Conversation deletion uses the same rule as ledger writes.
    pub(in crate::runtime) async fn sample_on(
        self,
        conn: &mut SqliteConnection,
    ) -> anyhow::Result<i64> {
        let now = self.sample();
        if self != Self::Now {
            return Ok(now);
        }
        let installed: bool = sqlx::query_scalar(
            "SELECT EXISTS(SELECT 1 FROM sqlite_schema WHERE type = 'table' AND name = 'draft_accounting_retention_checkpoint')",
        )
        .fetch_one(&mut *conn)
        .await?;
        if !installed {
            return Ok(now);
        }
        let checkpoint: Option<Option<i64>> = sqlx::query_scalar(
            "SELECT completed_as_of_ms FROM draft_accounting_retention_checkpoint WHERE singleton = 1",
        )
        .fetch_optional(&mut *conn)
        .await?;
        match checkpoint.flatten() {
            Some(checkpoint) if checkpoint > now => {
                warn_clock_behind(checkpoint - now);
                Ok(checkpoint)
            }
            _ => Ok(now),
        }
    }
}

/// #1518: an inspection read at a time behind the ledger's checkpoint. The
/// ledger is intact; the system clock reads earlier than its newest record, so
/// the read is refused until the clock catches up.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ClockBehindLedger {
    pub behind_ms: i64,
}

impl std::fmt::Display for ClockBehindLedger {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "backward inspection read: the system clock is {} ms behind the accounting ledger checkpoint",
            self.behind_ms
        )
    }
}

impl std::error::Error for ClockBehindLedger {}

/// The clock-behind refusal in `error`'s chain, if that is what it is.
pub fn clock_behind_ledger(error: &anyhow::Error) -> Option<ClockBehindLedger> {
    error
        .chain()
        .find_map(|cause| cause.downcast_ref::<ClockBehindLedger>().copied())
}

/// #1522: an inspection of a UTC day that starts after the read time, e.g. a
/// `/cost` view opened while the clock read ahead and refreshed after it was
/// set back. Still refused; typed so the caller can name the clock.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ClockBeforeDay {
    pub behind_ms: i64,
}

impl std::fmt::Display for ClockBeforeDay {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "invalid or future day: the system clock is {} ms before the requested UTC day",
            self.behind_ms
        )
    }
}

impl std::error::Error for ClockBeforeDay {}

/// The future-day refusal in `error`'s chain, if that is what it is.
pub fn clock_before_day(error: &anyhow::Error) -> Option<ClockBeforeDay> {
    error
        .chain()
        .find_map(|cause| cause.downcast_ref::<ClockBeforeDay>().copied())
}

/// Say once per process that the clock is behind the ledger.
fn warn_clock_behind(behind_ms: i64) {
    static WARNED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
    if !WARNED.swap(true, std::sync::atomic::Ordering::Relaxed) {
        tracing::warn!(
            target: "codex_core::accounting",
            behind_ms,
            "accounting: the system clock is behind the accounting ledger; holding ledger write time at the checkpoint until the clock catches up; request dispatch times are unchanged. \
             Requests cannot be recorded while the clock is 90 days or more behind"
        );
    }
}

impl From<i64> for AsOf {
    fn from(as_of_ms: i64) -> Self {
        Self::At(as_of_ms)
    }
}

/// Bounds on how long the write lock is left free between expiry batches; see
/// `expire_due`.
const EXPIRY_YIELD_MIN: std::time::Duration = std::time::Duration::from_millis(10);
const EXPIRY_YIELD_MAX: std::time::Duration = std::time::Duration::from_millis(100);

/// Whether `error` is lock contention on the state database: another
/// connection - usually another Corbanu process - held it past the busy timeout,
/// or no pooled connection came free in time.
///
/// Each store call is made of whole transactions that roll back on failure and
/// are idempotent for the same arguments, so after contention the same call can
/// simply be made again. Any other error is not contention and must not be
/// retried as if it were.
pub fn is_contention(error: &anyhow::Error) -> bool {
    crate::runtime::busy_retry::is_busy(error)
        || error.chain().any(|cause| {
            matches!(
                cause.downcast_ref::<sqlx::Error>(),
                Some(sqlx::Error::PoolTimedOut)
            )
        })
}

impl<'a> AccountingStore<'a> {
    /// Inspect an existing ledger in one read transaction, without installation,
    /// maintenance, repricing or repair. All times are UTC milliseconds/days.
    pub async fn inspect_day(
        runtime: &StateRuntime,
        owner: ThreadId,
        utc_day: i64,
        read_at_ms: i64,
    ) -> anyhow::Result<InspectionDay> {
        Self::inspect(runtime, owner, utc_day, read_at_ms, /*requested*/ None).await
    }

    pub async fn inspect_range(
        runtime: &StateRuntime,
        owner: ThreadId,
        requested: InspectionRange,
        read_at_ms: i64,
    ) -> anyhow::Result<InspectionDay> {
        requested.validate()?;
        Self::inspect(
            runtime,
            owner,
            requested.start_ms / 86_400_000,
            read_at_ms,
            Some(requested),
        )
        .await
    }

    async fn inspect(
        runtime: &StateRuntime,
        owner: ThreadId,
        utc_day: i64,
        read_at_ms: i64,
        requested: Option<InspectionRange>,
    ) -> anyhow::Result<InspectionDay> {
        let start = utc_day.checked_mul(86_400_000).context("day overflow")?;
        ensure!(utc_day >= 0, "invalid or future day");
        if read_at_ms < start {
            return Err(ClockBeforeDay {
                behind_ms: start.saturating_sub(read_at_ms),
            }
            .into());
        }
        start.checked_add(86_400_000).context("day end overflow")?;
        let mut tx = runtime.pool.begin().await?;
        if !ledger_exists(&mut tx).await? {
            let objects: i64 = sqlx::query_scalar(
                "SELECT count(*) FROM sqlite_schema WHERE name GLOB 'draft_accounting_*'",
            )
            .fetch_one(&mut *tx)
            .await?;
            ensure!(objects == 0, "unversioned accounting schema");
            return Ok(InspectionDay::Absent);
        }
        validate_on_connection(&mut tx).await?;
        let exists: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM threads WHERE id = ?)")
            .bind(owner.to_string())
            .fetch_one(&mut *tx)
            .await?;
        if !exists {
            return Ok(InspectionDay::MissingThread);
        }
        let result = match requested {
            Some(requested) => inspect_buckets(&mut tx, owner, requested, read_at_ms).await?,
            None => inspect_tree(&mut tx, owner, utc_day, read_at_ms).await?,
        };
        tx.commit().await?;
        Ok(result)
    }

    /// Atomically import 1..=64 complete attempts aged 90..365 days without raw
    /// staging. Each has at most 256 observations, with at most 4096 in total.
    /// Duplicate identical entries receive the same outcome but contribute once.
    /// An all-suppressed bundle does not advance maintenance or read coverage.
    /// Cancellation may lose a committed acknowledgement: retry the original IDs.
    pub async fn import_retained(
        &self,
        owner: ThreadId,
        bundle: &[RetainedImport],
        as_of_ms: i64,
    ) -> anyhow::Result<Vec<RetainedImportOutcome>> {
        let mut tx = crate::runtime::db_lock::begin_immediate(&self.runtime.pool).await?;
        match Journal::import_retained_on_connection(&mut tx, owner, bundle, as_of_ms).await {
            Ok(outcomes) => {
                tx.commit().await?;
                Ok(outcomes)
            }
            Err(error) => {
                tx.rollback().await?;
                Err(error)
            }
        }
    }

    /// #314 (D228): durably mark that accounting failed to record at least one
    /// of `owner`'s model requests sent between `from_ms` and `to_ms` (UTC
    /// ms, either order), so `/cost` says those days' totals may be
    /// incomplete. Every UTC day of the span is marked (the first and the 32
    /// ending on the last, for a longer span), and, when the clock reads
    /// behind the ledger's checkpoint, the checkpoint's day too: the earliest
    /// day the true time can be, which retention will not expire at once.
    /// Upgrades the ledger to format 7 in the same transaction; it
    /// runs no retention or clock check, since the failure it marks may be
    /// one. Marking a day twice is one marker. A home with no ledger is left
    /// without one: its `/cost` already reads unavailable, never complete,
    /// and an unrecorded request does not install a ledger.
    pub async fn record_gap(
        runtime: &StateRuntime,
        owner: ThreadId,
        from_ms: i64,
        to_ms: i64,
    ) -> anyhow::Result<()> {
        let (from_ms, to_ms) = (from_ms.min(to_ms), from_ms.max(to_ms));
        ensure!(from_ms >= 0, "negative gap time");
        let mut tx = crate::runtime::db_lock::begin_immediate(&runtime.pool).await?;
        let result = async {
            if !ledger_exists(&mut tx).await? {
                return Ok(());
            }
            validate_on_connection(&mut tx).await?;
            require_format_on_connection(&mut tx, super::gap_days::FORMAT).await?;
            let checkpoint = match super::retention_fixture_on_connection(&mut tx).await? {
                super::RetentionFixture::Active(checkpoint) => Some(checkpoint),
                super::RetentionFixture::Staging | super::RetentionFixture::Absent => None,
            };
            for day in super::gap_days::days(from_ms, to_ms, checkpoint) {
                super::gap_days::mark_on_connection(&mut tx, owner, day).await?;
            }
            Ok(())
        }
        .await;
        match result {
            Ok(()) => tx.commit().await?,
            Err(error) => {
                tx.rollback().await?;
                return Err(error);
            }
        }
        Ok(())
    }

    /// Refuse a ledger written in a newer format (`NewerLedgerFormat`); a
    /// missing ledger or one this build can read is fine. Read-only and cheap:
    /// it reads the applied migrations only.
    pub async fn check_format(runtime: &StateRuntime) -> anyhow::Result<()> {
        let mut conn = runtime.pool.acquire().await?;
        if !ledger_exists(&mut conn).await? {
            return Ok(());
        }
        let versions: Vec<i64> =
            sqlx::query_scalar("SELECT version FROM _accounting_migrations ORDER BY version")
                .fetch_all(&mut *conn)
                .await?;
        refuse_newer(versions)
    }

    /// Install only when wholly absent, otherwise validate without schema repair.
    /// Activation is a real complete retention sweep, never a fabricated checkpoint.
    pub async fn open(runtime: &'a StateRuntime, as_of: impl Into<AsOf>) -> anyhow::Result<Self> {
        let as_of = as_of.into();
        if let AsOf::At(as_of_ms) = as_of {
            ensure!(as_of_ms >= 0, "negative accounting time");
        }
        let mut tx = crate::runtime::db_lock::begin_immediate(&runtime.pool).await?;
        match install_on_connection(&mut tx).await {
            Ok(()) => tx.commit().await?,
            Err(error) => {
                tx.rollback().await?;
                return Err(error);
            }
        }
        let store = Self { runtime };
        store.maintain_for_write(as_of).await?;
        Ok(store)
    }

    /// Persist native intent and original price (including explicit missing price)
    /// before the caller sends. This does not prove actual dispatch or approval.
    pub async fn admit(
        &self,
        owner: ThreadId,
        attempt: &Attempt,
        original_prices: &[Snapshot],
        as_of: impl Into<AsOf>,
    ) -> anyhow::Result<ObservationQuote> {
        self.write(owner, attempt, &[], Some(original_prices), as_of.into())
            .await
    }

    /// Append cumulative evidence only to an already durably admitted attempt.
    /// Original NULL/rate bindings cannot be replaced by later catalogs.
    pub async fn observe(
        &self,
        owner: ThreadId,
        attempt: &Attempt,
        observations: &[Observation],
        as_of: impl Into<AsOf>,
    ) -> anyhow::Result<ObservationQuote> {
        self.write(
            owner,
            attempt,
            observations,
            /*original_prices*/ None,
            as_of.into(),
        )
        .await
    }

    async fn write(
        &self,
        owner: ThreadId,
        attempt: &Attempt,
        observations: &[Observation],
        original_prices: Option<&[Snapshot]>,
        as_of: AsOf,
    ) -> anyhow::Result<ObservationQuote> {
        let validated_at_ms = self.prepare_write(as_of).await?;
        let mut tx = crate::runtime::db_lock::begin_immediate(&self.runtime.pool).await?;
        let as_of_ms = match as_of.sample_on(&mut tx).await {
            Ok(as_of_ms) => as_of_ms,
            Err(error) => {
                tx.rollback().await?;
                return Err(error);
            }
        };
        let result = Journal::store_on_connection(
            &mut tx,
            owner,
            attempt,
            observations,
            original_prices,
            as_of_ms,
            validated_at_ms,
        )
        .await;
        match result {
            Ok(quote) => {
                tx.commit().await?;
                Ok(quote)
            }
            Err(error) => {
                tx.rollback().await?;
                Err(error)
            }
        }
    }

    /// The hour's whole-ledger validation, on a read snapshot that holds no lock
    /// another writer waits on. Returns when it was done this hour, if it was, so
    /// the writes only have to apply what is due, in short batches
    /// (`expire_due`), and advance the checkpoint. Done at most once per process
    /// per hour.
    async fn validate_hour(&self, as_of: AsOf) -> anyhow::Result<Option<i64>> {
        let cached = &self.runtime.accounting_validated_at_millis;
        let mut tx = self.runtime.pool.begin().await?;
        let result = async {
            // The schema read opens the snapshot before the clock is read, so the
            // snapshot's checkpoint can never be later than this reading.
            validate_on_connection(&mut tx).await?;
            let as_of_ms = as_of.sample_on(&mut tx).await?;
            let previous = cached.load(std::sync::atomic::Ordering::Acquire);
            let validated =
                Journal::validate_hour_on_connection(&mut tx, as_of_ms, previous).await?;
            if let Some(at) = validated {
                cached.store(at, std::sync::atomic::Ordering::Release);
            }
            anyhow::Ok(validated)
        }
        .await;
        tx.rollback().await?;
        result
    }

    /// Whether the next write at `as_of` would first run the hour's
    /// whole-ledger validation (`validate_hour`): this process has not run it
    /// this hour and no write moved the checkpoint into this hour yet. Reads
    /// the checkpoint only; no lock.
    async fn validation_due(&self, as_of: AsOf) -> anyhow::Result<bool> {
        let now = as_of.sample();
        let hour_start = now - now.rem_euclid(Journal::VALIDATION_INTERVAL_MS);
        let cached = self
            .runtime
            .accounting_validated_at_millis
            .load(std::sync::atomic::Ordering::Acquire);
        if (hour_start..=now).contains(&cached) {
            return Ok(false);
        }
        let mut conn = self.runtime.pool.acquire().await?;
        if !ledger_exists(&mut conn).await? {
            return Ok(false);
        }
        Ok(
            match super::retention_fixture_on_connection(&mut conn).await? {
                super::RetentionFixture::Active(checkpoint) => {
                    checkpoint < hour_start && checkpoint <= now
                }
                super::RetentionFixture::Absent | super::RetentionFixture::Staging => false,
            },
        )
    }

    /// Everything ahead of a write's own transaction: the hour's validation on a
    /// read snapshot, then retiring what is due in short batches. Repeated if
    /// the UTC hour turned meanwhile, since the write would otherwise find its
    /// validation (or the checkpoint) in the hour before and run the full sweep
    /// under the lock. That remains possible, rarely, if the hour turns while
    /// the write waits for the lock after this returns.
    async fn prepare_write(&self, as_of: AsOf) -> anyhow::Result<Option<i64>> {
        let hour = |as_of: AsOf| as_of.sample().div_euclid(Journal::VALIDATION_INTERVAL_MS);
        loop {
            let started = hour(as_of);
            let validated_at_ms = self.validate_hour(as_of).await?;
            self.expire_due(as_of, validated_at_ms).await?;
            if hour(as_of) == started {
                return Ok(validated_at_ms);
            }
        }
    }

    /// Retire what retention has due, a bounded batch per short write
    /// transaction, releasing the lock between batches so that other writers -
    /// other Corbanu processes included - are never held up for longer than one
    /// batch. Each batch commits whole, so a crash keeps every batch before it
    /// and the next write carries on. Does nothing unless the ledger was
    /// validated this hour; the write then runs the full sweep.
    async fn expire_due(&self, as_of: AsOf, validated_at_ms: Option<i64>) -> anyhow::Result<()> {
        // Most writes have nothing due: find that out without the write lock.
        let mut tx = self.runtime.pool.begin().await?;
        let due = async {
            validate_on_connection(&mut tx).await?;
            let as_of_ms = as_of.sample_on(&mut tx).await?;
            Journal::expiry_due_for_write_on_connection(&mut tx, as_of_ms, validated_at_ms).await
        }
        .await;
        tx.rollback().await?;
        if !due? {
            return Ok(());
        }
        loop {
            let mut tx = crate::runtime::db_lock::begin_immediate(&self.runtime.pool).await?;
            let held = std::time::Instant::now();
            let result = async {
                validate_on_connection(&mut tx).await?;
                let as_of_ms = as_of.sample_on(&mut tx).await?;
                Journal::expire_for_write_on_connection(&mut tx, as_of_ms, validated_at_ms).await
            }
            .await;
            match result {
                Ok(true) => tx.commit().await?,
                Ok(false) => {
                    tx.commit().await?;
                    return Ok(());
                }
                Err(error) => {
                    tx.rollback().await?;
                    return Err(error);
                }
            }
            // SQLite does not queue writers: a waiting one polls, at most 100 ms
            // apart. Leaving the lock free for as long as the batch held it lets
            // a waiter in within a few batches, at half the expiry's pace. The
            // jitter keeps the cycle from dividing the waiter's 100 ms evenly
            // and its polls from landing in the batches every time.
            let jitter = std::time::Duration::from_millis(u64::from(
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap_or_default()
                    .subsec_micros()
                    % 50_000
                    / 1_000,
            ));
            let pause = held.elapsed().clamp(EXPIRY_YIELD_MIN, EXPIRY_YIELD_MAX) + jitter;
            tokio::time::sleep(pause).await;
        }
    }

    /// What a conversation deletion does before it takes the write lock: the
    /// hour's validation on a read snapshot and the due expiries in short
    /// batches, as ahead of a write (`prepare_write`). Returns when the
    /// validation ran; `None` when there is no ledger.
    ///
    /// As for ledger writes, the whole ledger is validated at most once per
    /// hour: a deletion after another process already validated it this hour
    /// does not validate it again.
    pub(in crate::runtime) async fn prepare_delete(
        runtime: &'a StateRuntime,
        as_of: AsOf,
    ) -> anyhow::Result<Option<i64>> {
        let exists = {
            let mut conn = runtime.pool.acquire().await?;
            ledger_exists(&mut conn).await?
        };
        if !exists {
            return Ok(None);
        }
        Self { runtime }.prepare_write(as_of).await
    }

    pub async fn maintain(&self, as_of_ms: i64) -> anyhow::Result<()> {
        let mut tx = crate::runtime::db_lock::begin_immediate(&self.runtime.pool).await?;
        let result = async {
            validate_on_connection(&mut tx).await?;
            Journal::maintain_native_on_connection(&mut tx, as_of_ms).await
        }
        .await;
        match result {
            Ok(()) => tx.commit().await?,
            Err(error) => {
                tx.rollback().await?;
                return Err(error);
            }
        }
        Ok(())
    }

    /// Opening precedes every request write; keep its lock hold short.
    ///
    /// The hour's whole-ledger validation runs first on a read snapshot, which
    /// holds no lock another writer waits on; expiries are then applied in short
    /// batches, and the write transaction retires at most one more batch and
    /// advances the checkpoint (see `maintain_for_write_on_connection`).
    async fn maintain_for_write(&self, as_of: AsOf) -> anyhow::Result<()> {
        let validated_at_ms = self.prepare_write(as_of).await?;
        let mut tx = crate::runtime::db_lock::begin_immediate(&self.runtime.pool).await?;
        let result = async {
            validate_on_connection(&mut tx).await?;
            let as_of_ms = as_of.sample_on(&mut tx).await?;
            Journal::maintain_for_write_on_connection(&mut tx, as_of_ms, validated_at_ms).await
        }
        .await;
        match result {
            Ok(()) => tx.commit().await?,
            Err(error) => {
                tx.rollback().await?;
                return Err(error);
            }
        }
        Ok(())
    }

    /// A single read snapshot, with explicit stale/expired/unknown states.
    /// Reading never activates or advances maintenance.
    pub async fn read_day(
        &self,
        owner: ThreadId,
        utc_day: i64,
        as_of_ms: i64,
    ) -> anyhow::Result<RetainedDay> {
        let mut tx = self.runtime.pool.begin().await?;
        validate_on_connection(&mut tx).await?;
        let value = Journal::retained_day_on_connection(&mut tx, owner, utc_day, as_of_ms).await?;
        tx.commit().await?;
        Ok(value)
    }
}

// Work, unlike selected output, is never refunded for an unrelated candidate.
// Reserve table-scan row visits before SQL, and bound all candidate/chain walks.
// One budget covers the entire range, not each bucket independently.
pub(super) struct InspectionWork {
    rows: usize,
    visits: usize,
    scan_rows: usize,
    /// FUP-02: a conversation's start, read once per inspection (a range reads
    /// it for every empty bucket day).
    started: Option<(ThreadId, Option<i64>)>,
}

impl InspectionWork {
    pub(super) async fn new(conn: &mut SqliteConnection) -> anyhow::Result<Self> {
        let scanned: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM (
             SELECT 1 FROM draft_accounting_attempts
             UNION ALL SELECT 1 FROM draft_accounting_compact_days LIMIT 4000001)",
        )
        .fetch_one(&mut *conn)
        .await?;
        Ok(Self {
            rows: 4_000_000usize.saturating_sub(scanned as usize),
            visits: 20_000,
            scan_rows: scanned as usize,
            started: None,
        })
    }

    pub(super) fn scans(&mut self, count: usize) -> bool {
        let cost = self.scan_rows.saturating_mul(count);
        if cost > self.rows {
            self.rows = 0;
            return false;
        }
        self.rows -= cost;
        true
    }

    /// One scan when the budget has room for it and `reserve` more. Unlike
    /// [`Self::scans`], a refusal leaves the budget for the rest of the
    /// inspection.
    pub(super) fn spare_scan(&mut self, reserve: usize) -> bool {
        self.scan_rows.saturating_mul(reserve.saturating_add(1)) <= self.rows
            && self.scans(/*count*/ 1)
    }

    pub(super) fn visit(&mut self) -> bool {
        if self.visits == 0 || self.rows == 0 {
            self.rows = 0;
            return false;
        }
        self.visits -= 1;
        self.rows -= 1;
        true
    }
}

// Reuse the single-owner validator inside the caller's deferred snapshot. Never
// open a store or refresh contributions while assembling the tree explanation.
async fn inspect_tree(
    conn: &mut SqliteConnection,
    owner: ThreadId,
    day: i64,
    read_at_ms: i64,
) -> anyhow::Result<InspectionDay> {
    let mut work = InspectionWork::new(conn).await?;
    inspect_tree_window(
        conn, owner, day, read_at_ms, /*window*/ None, &mut work,
    )
    .await
}

async fn inspect_tree_window(
    conn: &mut SqliteConnection,
    owner: ThreadId,
    day: i64,
    read_at_ms: i64,
    window: Option<(i64, i64)>,
    work: &mut InspectionWork,
) -> anyhow::Result<InspectionDay> {
    use codex_protocol::protocol::SessionSource;
    use std::collections::BTreeMap;
    use std::collections::HashSet;
    let first =
        Journal::inspect_window_on_connection(conn, owner, day, read_at_ms, window, work).await?;
    let InspectionDay::Ready(mut view) = first else {
        return Ok(first);
    };
    // FUP-02: a single day with nothing recorded for the owner. Before the
    // aggregate floor it stays unavailable, as before, unless the conversation
    // had not started yet; that is decided here, ahead of the tree.
    let mut started = None;
    if window.is_none() && view.totals.attempts == 0 && day < view.coverage.aggregate_day_floor {
        let early = started_ms(conn, owner, work).await?;
        if !empty_day::proven_before(day, early) {
            return Ok(InspectionDay::DetailUnavailable {
                coverage: view.coverage,
                read_at_ms,
                compact: false,
            });
        }
        started = Some(early);
    }
    if !work.scans(/*count*/ 1) {
        return Ok(InspectionDay::TooLarge);
    }
    let candidates: Vec<String> = sqlx::query_scalar(
        "SELECT DISTINCT json_extract(payload, '$.thread_id') FROM draft_accounting_attempts
         WHERE json_extract(payload, '$.dispatched_at_ms') / 86400000 = ?
         UNION SELECT thread_id FROM draft_accounting_compact_days WHERE utc_day = ?",
    )
    .bind(day)
    .bind(day)
    .fetch_all(&mut *conn)
    .await?;
    // Cache only ancestry visited by selected-day owners. A malformed source,
    // absent ancestor, conflict or cycle is an unknown population, never another root.
    let mut ancestry: BTreeMap<String, (Option<String>, bool)> = BTreeMap::new();
    let mut graph_bytes = 0usize;
    let mut packet_bytes = 8192usize;
    let mut attempts = view.totals.attempts;
    for quotes in view.requests.values() {
        for quote in quotes {
            packet_bytes += serde_json::to_vec(quote)?.len() + 2048;
        }
    }
    let mut unrelated = Vec::new();
    // A thread of unknown ancestry with anything on this UTC day, inside a
    // range bucket's window or not, may be an unlinked subagent.
    let mut unknown_on_day = false;
    for candidate in candidates {
        if !work.visit() {
            return Ok(InspectionDay::TooLarge);
        }
        if candidate == owner.to_string() {
            continue;
        }
        let mut cursor = candidate.clone();
        let mut seen = HashSet::new();
        let mut reached = false;
        let mut discovered = BTreeMap::new();
        let relation = loop {
            if !work.visit() {
                return Ok(InspectionDay::TooLarge);
            }
            if !seen.insert(cursor.clone()) {
                break None;
            }
            reached |= cursor == owner.to_string();
            if !ancestry.contains_key(&cursor) && !discovered.contains_key(&cursor) {
                let row: Option<(String, Option<String>)> = sqlx::query_as(
                    "SELECT t.source, e.parent_thread_id FROM threads t
                     LEFT JOIN thread_spawn_edges e ON e.child_thread_id = t.id WHERE t.id = ?",
                )
                .bind(&cursor)
                .fetch_optional(&mut *conn)
                .await?;
                let (parent, terminal) = if let Some((source, edge)) = row {
                    let parsed = serde_json::from_str::<SessionSource>(&source)
                        .or_else(|_| serde_json::from_value(serde_json::Value::String(source)))
                        .ok();
                    let source_parent = parsed
                        .as_ref()
                        .and_then(SessionSource::parent_thread_id)
                        .map(|id| id.to_string());
                    if matches!(parsed.as_ref(), None | Some(SessionSource::Unknown))
                        || (source_parent.is_some() && edge != source_parent)
                    {
                        (None, false)
                    } else {
                        let terminal = matches!(
                            parsed,
                            Some(
                                SessionSource::Cli
                                    | SessionSource::VSCode
                                    | SessionSource::Exec
                                    | SessionSource::Mcp
                            )
                        );
                        (edge, terminal)
                    }
                } else {
                    (None, false)
                };
                discovered.insert(cursor.clone(), (parent, terminal));
                if ancestry.len() + discovered.len() > 10_000 {
                    return Ok(InspectionDay::TooLarge);
                }
            }
            let (parent, terminal) = ancestry
                .get(&cursor)
                .unwrap_or_else(|| &discovered[&cursor]);
            match parent {
                Some(parent) => cursor = parent.clone(),
                None => {
                    break terminal.then_some(reached);
                }
            }
        };
        if relation == Some(false) {
            // Unrelated roots cannot consume the selected ancestry/packet budget.
            // They are read after the tree, outside its total; `cursor` is
            // their terminal root.
            unrelated.push((candidate, cursor));
            continue;
        }
        // Source text is parsed and dropped; only retained ancestry strings count.
        // Provisional memory is bounded above by the node limit before rollback.
        graph_bytes += discovered
            .iter()
            .map(|(id, (parent, _))| id.len() + parent.as_ref().map_or(0, String::len))
            .sum::<usize>();
        if packet_bytes + graph_bytes > 4 * 1024 * 1024 {
            return Ok(InspectionDay::TooLarge);
        }
        ancestry.extend(discovered);
        unknown_on_day |= relation.is_none();
        let other = Journal::inspect_window_on_connection(
            conn,
            ThreadId::from_string(&candidate)?,
            day,
            read_at_ms,
            window,
            work,
        )
        .await?;
        if work.rows == 0 {
            return Ok(InspectionDay::TooLarge);
        }
        let InspectionDay::Ready(other) = other else {
            if relation.is_none() {
                view.unknown_parent_unavailable_threads += 1;
                continue;
            }
            return Ok(other);
        };
        attempts = attempts
            .checked_add(other.totals.attempts)
            .context("attempt count overflow")?;
        if attempts > 512 {
            return Ok(InspectionDay::TooLarge);
        }
        let target = if relation == Some(true) {
            &mut view.requests
        } else {
            &mut view.unknown_parent_requests
        };
        for (request, quotes) in other.requests {
            for quote in &quotes {
                packet_bytes += serde_json::to_vec(quote)?.len() + 2048;
            }
            if packet_bytes + graph_bytes > 4 * 1024 * 1024 {
                return Ok(InspectionDay::TooLarge);
            }
            ensure!(
                target.insert(request, quotes).is_none(),
                "request owner mismatch"
            );
        }
    }
    view.totals = DayTotals::from_quotes(view.requests.values().flatten())?;
    view.descendant_totals = DayTotals::from_quotes(
        view.requests
            .values()
            .flatten()
            .filter(|q| q.attempt.thread_id != owner),
    )?;
    view.unknown_parent_totals =
        DayTotals::from_quotes(view.unknown_parent_requests.values().flatten())?;
    if window.is_none() {
        view.other_conversations =
            Some(scope::other_conversations(conn, unrelated, day, read_at_ms, work).await);
    }
    // #314 (D228): a request of this tree that day went unrecorded.
    view.recording_gap = empty_day::gap_recorded(conn, owner, day).await?;
    // Last, so the start's scan never takes budget the rest of the view needs.
    // #1196: a range bucket's part of the day is classified the same way.
    if view.totals.attempts == 0 {
        let started = match started {
            Some(started) => started,
            None => started_ms(conn, owner, work).await?,
        };
        let unknown_clear = !unknown_on_day
            && view.unknown_parent_totals.attempts == 0
            && view.unknown_parent_unavailable_threads == 0;
        let empty = empty_day::classify(
            day,
            started,
            empty_day::ledger_started_day(conn).await?,
            unknown_clear,
        );
        if day < view.coverage.aggregate_day_floor
            && !matches!(empty, EmptyDay::BeforeConversation { .. })
        {
            return Ok(InspectionDay::DetailUnavailable {
                coverage: view.coverage,
                read_at_ms,
                compact: false,
            });
        }
        view.empty_day = Some(empty);
    }
    Ok(InspectionDay::Ready(view))
}

/// `empty_day::started_ms`, charged to the work budget: one scan of the
/// accounting rows for the tree's earliest recorded request, once per
/// inspection. Without budget left for it there is no proof, and the view stays
/// as it would have been.
async fn started_ms(
    conn: &mut SqliteConnection,
    owner: ThreadId,
    work: &mut InspectionWork,
) -> anyhow::Result<Option<i64>> {
    if let Some((read_for, started)) = work.started
        && read_for == owner
    {
        return Ok(started);
    }
    if !work.spare_scan(/*reserve*/ 0) {
        return Ok(None);
    }
    let started = empty_day::started_ms(conn, owner).await?;
    work.started = Some((owner, started));
    Ok(started)
}

// All buckets reuse the already-open deferred snapshot, including ancestry.
async fn inspect_buckets(
    conn: &mut SqliteConnection,
    owner: ThreadId,
    requested: InspectionRange,
    read_at_ms: i64,
) -> anyhow::Result<InspectionDay> {
    let mut work = InspectionWork::new(conn).await?;
    if !work.scans(/*count*/ 1) {
        return Ok(InspectionDay::TooLarge);
    }
    // The same floor the day pages state, not the oldest compacted day: a
    // ledger that has compacted nothing yet still keeps a year of totals.
    let completed_as_of_ms = match super::retention_fixture_on_connection(conn).await? {
        super::RetentionFixture::Active(checkpoint) => Some(checkpoint),
        super::RetentionFixture::Staging | super::RetentionFixture::Absent => None,
    };
    let aggregate_day_floor = completed_as_of_ms.map(super::aggregate_day_floor);
    let mut buckets = Vec::new();
    let mut cursor = requested.start_ms;
    let mut attempts = 0;
    let mut bytes = 0;
    while cursor < requested.end_ms {
        if buckets.len() >= 10_000 {
            return Ok(InspectionDay::TooLarge);
        }
        let (start_ms, end_ms) = requested.bucket(cursor)?;
        let lower = start_ms.max(requested.start_ms);
        let upper = end_ms.min(requested.end_ms);
        let mut effective = Some((lower, upper));
        let mut days = Vec::new();
        let mut day = lower / 86_400_000;
        while day <= (upper - 1) / 86_400_000 {
            let window = (
                lower.max(day * 86_400_000),
                upper.min((day + 1) * 86_400_000),
            );
            let value =
                inspect_tree_window(conn, owner, day, read_at_ms, Some(window), &mut work).await?;
            let coverage = match &value {
                InspectionDay::Ready(view) => {
                    attempts += view.totals.attempts + view.unknown_parent_totals.attempts;
                    for quote in view
                        .requests
                        .values()
                        .chain(view.unknown_parent_requests.values())
                        .flatten()
                    {
                        bytes += serde_json::to_vec(quote)?.len() + 2048;
                    }
                    Some(&view.coverage)
                }
                InspectionDay::DetailUnavailable { coverage, .. } => Some(coverage),
                InspectionDay::TooLarge => return Ok(value),
                _ => None,
            };
            match (effective, coverage) {
                (Some((from, to)), Some(coverage)) => {
                    let from = from.max(coverage.aggregate_day_floor * 86_400_000);
                    let to = to
                        .min(coverage.completed_as_of_ms.saturating_add(1))
                        .min(read_at_ms.saturating_add(1));
                    effective = (from < to).then_some((from, to));
                }
                // A day the ledger has not reached yet, after days it has: the
                // bucket's coverage ends where the previous day's did, exactly
                // as a day bucket that contains today reports it. Keep it only
                // if coverage already ends before this day; a lagging day inside
                // the ledger (detail awaiting compaction) is unverified.
                (Some((_, to)), None)
                    if to <= day * 86_400_000 && matches!(value, InspectionDay::CheckpointLag) => {}
                _ => effective = None,
            }
            // Include empty-bucket/header costs in the unchanged packet ceiling.
            bytes += 8192;
            if attempts > 512 || bytes > 4 * 1024 * 1024 {
                return Ok(InspectionDay::TooLarge);
            }
            days.push(value);
            day += 1;
        }
        buckets.push(InspectionBucket {
            start_ms,
            end_ms,
            effective,
            partial: effective != Some((start_ms, end_ms)),
            days,
        });
        cursor = end_ms;
    }
    Ok(InspectionDay::Range {
        requested,
        aggregate_day_floor,
        completed_as_of_ms,
        read_at_ms,
        buckets,
    })
}

pub(super) async fn ledger_exists(conn: &mut SqliteConnection) -> anyhow::Result<bool> {
    Ok(sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM sqlite_schema WHERE name = '_accounting_migrations')",
    )
    .fetch_one(conn)
    .await?)
}

// Caller owns BEGIN IMMEDIATE: schema and independent ledger commit together.
//
// A new ledger is installed in format 1, and an existing one is left in the
// format it has: it is upgraded only when a record that needs a newer format is
// first written (`require_format_on_connection`). An older build sharing the
// state DB keeps reading the ledger until then.
async fn install_on_connection(conn: &mut SqliteConnection) -> anyhow::Result<()> {
    if !ledger_exists(conn).await? {
        let objects: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM sqlite_schema WHERE name GLOB 'draft_accounting_*'",
        )
        .fetch_one(&mut *conn)
        .await?;
        ensure!(objects == 0, "unversioned accounting schema");
        crate::migrations::accounting_migrator()
            .run_direct(/*target*/ Some(1), conn, /*skip*/ false)
            .await?;
    }
    validate_on_connection(conn).await
}

/// Upgrade the ledger to at least `format` before writing a record that needs
/// it. Caller owns the write transaction, so the upgrade commits with the
/// record or not at all. An unknown or damaged ledger is refused, not migrated.
pub(crate) async fn require_format_on_connection(
    conn: &mut SqliteConnection,
    format: i64,
) -> anyhow::Result<()> {
    // Callers write inside a transaction that already validated the ledger.
    if applied_format(conn).await? >= format {
        return Ok(());
    }
    validate_on_connection(conn).await?;
    if applied_format(conn).await? < format {
        crate::migrations::accounting_migrator()
            .run_direct(/*target*/ Some(format), conn, /*skip*/ false)
            .await?;
        validate_on_connection(conn).await?;
    }
    ensure!(
        applied_format(conn).await? >= format,
        "accounting ledger was not upgraded"
    );
    Ok(())
}

async fn applied_format(conn: &mut SqliteConnection) -> anyhow::Result<i64> {
    Ok(
        sqlx::query_scalar("SELECT count(*) FROM _accounting_migrations")
            .fetch_one(conn)
            .await?,
    )
}

/// `NewerLedgerFormat` when any applied migration is newer than this build's.
fn refuse_newer(applied: impl IntoIterator<Item = i64>) -> anyhow::Result<()> {
    let supported = crate::migrations::accounting_migrator()
        .iter()
        .map(|migration| migration.version)
        .max()
        .unwrap_or(0);
    match applied
        .into_iter()
        .filter(|version| *version > supported)
        .max()
    {
        Some(found) => Err(NewerLedgerFormat { found, supported }.into()),
        None => Ok(()),
    }
}

/// Read-only version and physical-schema validation; no migration adoption/repair.
///
/// A ledger in an older format validates against the migrations it applied:
/// it is read as written. One in a newer format is refused with
/// `NewerLedgerFormat`.
pub(super) async fn validate_on_connection(conn: &mut SqliteConnection) -> anyhow::Result<()> {
    ensure!(ledger_exists(conn).await?, "accounting ledger missing");
    let migrator = crate::migrations::accounting_migrator();
    let rows: Vec<(i64, bool, Vec<u8>)> = sqlx::query_as(
        "SELECT version, success, checksum FROM _accounting_migrations ORDER BY version",
    )
    .fetch_all(&mut *conn)
    .await?;
    let expected: Vec<_> = migrator
        .iter()
        .map(|m| (m.version, true, m.checksum.to_vec()))
        .collect();
    refuse_newer(rows.iter().map(|(version, _, _)| *version))?;
    ensure!(
        !rows.is_empty() && expected.starts_with(&rows),
        "unsupported, failed or checksum-mismatched accounting migration"
    );
    // A migration not yet applied must have left nothing behind: a ledger that
    // claims an older format while holding a newer one's objects could hold
    // records that format cannot express.
    for migration in migrator.iter().skip(rows.len()) {
        for name in created_objects(migration.sql.as_str()) {
            let present: bool =
                sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM sqlite_schema WHERE name = ?)")
                    .bind(name)
                    .fetch_one(&mut *conn)
                    .await?;
            ensure!(
                !present,
                "accounting ledger holds objects of a later format: {name}"
            );
        }
    }
    // Compare every owned table/index definition, not just a ledger claim.
    // SQLite preserves CREATE SQL; whitespace is not schema identity.
    for migration in migrator.iter().take(rows.len()) {
        for statement in migration
            .sql
            .as_str()
            .split(';')
            .map(str::trim)
            .filter(|sql| sql.starts_with("CREATE "))
        {
            let name = statement
                .split_whitespace()
                .nth(2)
                .context("embedded accounting CREATE statement missing object name")?;
            let actual: Option<String> =
                sqlx::query_scalar("SELECT sql FROM sqlite_schema WHERE name = ?")
                    .bind(name)
                    .fetch_optional(&mut *conn)
                    .await?;
            ensure!(
                actual
                    .as_deref()
                    .map(|sql| sql.split_whitespace().collect::<Vec<_>>())
                    == Some(statement.split_whitespace().collect()),
                "partial or changed accounting schema: {name}"
            );
        }
    }
    super::retention_fixture_on_connection(conn).await?;
    Ok(())
}

/// The names of the objects a migration's `CREATE` statements make. Every
/// accounting migration starts with its `CREATE` statements (a test checks
/// each yields at least one), so a leading comment cannot hide an object.
fn created_objects(sql: &str) -> Vec<&str> {
    sql.split(';')
        .map(str::trim)
        .filter(|statement| statement.starts_with("CREATE "))
        .filter_map(|statement| statement.split_whitespace().nth(2))
        .collect()
}

#[cfg(test)]
#[path = "accounting_store_tests.rs"]
mod tests;
