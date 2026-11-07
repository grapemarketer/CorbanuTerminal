//! This process's accounting writer.
//!
//! A developer-accounting build records every model request in the ledger in
//! the shared state DB: an admission before the request is sent, then the
//! usage it reports. Every Corbanu process on a home shares that DB and its
//! one write lock. A request used to wait for each of these writes, and when
//! another process held the lock past accounting's 15 s budget the request
//! went out unrecorded and its record was lost.
//!
//! Now a request hands its record to this writer, which applies records one at
//! a time, in order, retrying while the lock is taken. The request waits only
//! while its record goes through without meeting the lock: once the writer
//! meets it (after at most `QUICK_BUSY_TIMEOUT`), the record is put off and the
//! request goes ahead; while the writer is still retrying, later records are
//! put off at once. Nothing is dropped for contention.
//!
//! Each record is first saved to a file under the home
//! (`accounting-pending/<process>-<seq>.json`), so it survives this process
//! (not an OS crash: the file is not synced, as the state DB itself runs with
//! `synchronous=NORMAL`), and the file is removed once the record is in the
//! ledger. Records a process left behind (it exited or crashed first) are
//! claimed and applied by the next writer that starts on the home, or by a
//! running writer's periodic sweep: a process holds an advisory lock on
//! `accounting-pending/<process>.lock` for as long as it runs, and a claimant
//! holds that lock while it takes the records. A ledger write is idempotent
//! for the same record, so a record applied twice is recorded once. `exec`
//! waits at exit for what its writer can still write; the TUI leaves it saved
//! for the next writer.
//!
//! Records the ledger refuses (anything but contention, a read-only or failing
//! disk, or a row it refers to that is not written yet) are logged and
//! dropped, as before. A request still waiting for its record gets the error
//! and its turn warns of the gap; a refusal that comes later is counted per
//! conversation (`StateRuntime::accounting_refusals`) and reported by that
//! conversation's next turn in this process. A refusal of a record adopted
//! from an exited process is counted in the adopting process, which usually
//! has no session for that conversation: it is only logged.
use super::AccountingStore;
use super::AsOf;
use super::Attempt;
use super::Observation;
use super::Snapshot;
use super::is_contention;
use crate::StateRuntime;
use crate::runtime::accounting::native::NotAdmitted;
use crate::runtime::accounting::native::OwnerMissing;
use crate::runtime::accounting::native::PredecessorMissing;
use crate::runtime::busy_retry::is_read_only;
use crate::runtime::busy_retry::is_storage_failure;
use codex_protocol::ThreadId;
use serde::Deserialize;
use serde::Serialize;
use std::collections::VecDeque;
use std::path::Path;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::Weak;
use std::sync::atomic::AtomicBool;
use std::sync::atomic::AtomicU64;
use std::sync::atomic::Ordering;
use std::time::Duration;
use std::time::Instant;
use tokio::sync::mpsc;
use tokio::sync::oneshot;
use uuid::Uuid;

const LOG_TARGET: &str = "codex_core::accounting";

/// The folder under the state DB's home that holds records not yet written.
pub const PENDING_DIR: &str = "accounting-pending";

/// The longest a request waits for its record when the writer was idle and
/// the write met no lock, in case the write itself is slow.
const WAIT_CAP: Duration = Duration::from_secs(1);

/// How often a writer checks on another process's hourly validation.
const VALIDATION_POLL: Duration = Duration::from_secs(1);

/// The lease on the hour's whole-ledger validation, under `PENDING_DIR`.
const VALIDATION_LEASE: &str = "validation.lock";

/// How often a running writer looks for records an exited process left.
const SWEEP_EVERY: Duration = Duration::from_secs(300);

/// How long a record that refers to a row not written yet - its
/// conversation's `threads` row, or its attempt's admission - is retried.
const OWNER_WAIT: Duration = Duration::from_secs(600);

/// The least an adopted record waits for a row it refers to.
const ADOPTED_MIN_WAIT: Duration = Duration::from_secs(60);

/// A lock file younger than this is never swept: its process may be between
/// creating and locking it.
const LOCK_FILE_GRACE: Duration = Duration::from_secs(60);

const RETRY_FIRST: Duration = Duration::from_millis(50);
const RETRY_MAX: Duration = Duration::from_secs(2);
/// A read-only DB is not cleared by the next commit elsewhere.
const READ_ONLY_RETRY: Duration = Duration::from_secs(30);

/// Most observations one ledger write accepts.
const OBSERVATION_BATCH: usize = 256;

/// One ledger record.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum LedgerWrite {
    /// The attempt and its original prices, before it is sent.
    Admit {
        owner: ThreadId,
        attempt: Attempt,
        prices: Vec<Snapshot>,
    },
    /// Usage evidence for an attempt admitted earlier.
    Observe {
        owner: ThreadId,
        attempt: Attempt,
        observations: Vec<Observation>,
    },
}

impl LedgerWrite {
    fn attempt(&self) -> &Attempt {
        match self {
            Self::Admit { attempt, .. } | Self::Observe { attempt, .. } => attempt,
        }
    }
}

/// What became of a record handed to the writer.
#[derive(Debug, PartialEq, Eq)]
pub enum WriteOutcome {
    /// It is in the ledger.
    Written,
    /// The state DB was busy: it is saved and will be written later.
    Deferred,
}

/// The saved form of a record.
#[derive(Serialize, Deserialize)]
struct Saved {
    version: u8,
    queued_at_ms: i64,
    write: LedgerWrite,
}

enum Settled {
    Deferred,
    Failed(String),
}

type Waiter = oneshot::Sender<Result<(), Settled>>;

enum Command {
    Write(Box<Entry>),
    Flush(Flush),
    /// Open (install or check) the ledger; answered like a record.
    Open(Waiter),
}

/// Answered once nothing is queued, or, with `until_blocked`, as soon as the
/// writer can go no further without another process (the write lock or the
/// hour's validation held elsewhere).
struct Flush {
    done: oneshot::Sender<()>,
    until_blocked: bool,
}

struct Entry {
    write: LedgerWrite,
    queued_at: Instant,
    file: Option<PathBuf>,
    waiter: Option<Waiter>,
    /// Taken from an exited process.
    adopted: bool,
}

/// Records for one attempt, applied in one ledger write.
struct Job {
    owner: ThreadId,
    attempt: Attempt,
    prices: Option<Vec<Snapshot>>,
    observations: Vec<Observation>,
    files: Vec<PathBuf>,
    waiters: Vec<Waiter>,
    queued_at: Instant,
    not_before: Option<Instant>,
    tries: u32,
    /// How many handed-over entries this job holds; see `Shared::pending`.
    entries: usize,
    /// Taken from an exited process, whose rows may be still to come.
    adopted: bool,
}

impl Job {
    fn new(entry: Box<Entry>) -> Self {
        let Entry {
            write,
            queued_at,
            file,
            waiter,
            adopted,
        } = *entry;
        let (owner, attempt, prices, observations) = match write {
            LedgerWrite::Admit {
                owner,
                attempt,
                prices,
            } => (owner, attempt, Some(prices), Vec::new()),
            LedgerWrite::Observe {
                owner,
                attempt,
                observations,
            } => (owner, attempt, None, observations),
        };
        Self {
            owner,
            attempt,
            prices,
            observations,
            files: file.into_iter().collect(),
            waiters: waiter.into_iter().collect(),
            queued_at,
            not_before: None,
            tries: 0,
            entries: 1,
            adopted,
        }
    }

    /// Take `entry` into this job when it is the same attempt and the result
    /// is still one ledger write: the admission first, then its observations.
    fn absorb(&mut self, entry: Box<Entry>) -> Result<(), Box<Entry>> {
        if entry.write.attempt() != &self.attempt {
            return Err(entry);
        }
        match &entry.write {
            LedgerWrite::Admit { owner, .. } => {
                // An admission after observations would reorder them.
                if *owner != self.owner || self.prices.is_some() || !self.observations.is_empty() {
                    return Err(entry);
                }
            }
            LedgerWrite::Observe {
                owner,
                observations,
                ..
            } => {
                // The same revision twice is one record (a replayed file);
                // a different one is for the ledger to refuse, on its own.
                let conflicting = observations.iter().any(|next| {
                    self.observations
                        .iter()
                        .any(|kept| kept.revision == next.revision && kept != next)
                });
                if *owner != self.owner
                    || conflicting
                    || self.observations.len() + observations.len() > OBSERVATION_BATCH
                {
                    return Err(entry);
                }
            }
        }
        let Entry {
            write,
            file,
            waiter,
            adopted,
            ..
        } = *entry;
        self.adopted &= adopted;
        match write {
            LedgerWrite::Admit { prices, .. } => self.prices = Some(prices),
            LedgerWrite::Observe { observations, .. } => {
                for next in observations {
                    if !self.observations.contains(&next) {
                        self.observations.push(next);
                    }
                }
            }
        }
        self.files.extend(file);
        self.waiters.extend(waiter);
        self.entries += 1;
        Ok(())
    }

    fn settle(&mut self, outcome: impl Fn() -> Result<(), Settled>) {
        for waiter in self.waiters.drain(..) {
            let _ = waiter.send(outcome());
        }
    }
}

/// Where records are saved, and this process's claim on its own.
struct Spool {
    dir: PathBuf,
    process: String,
    /// Held until the last owner drops; see the module docs.
    _lock: std::fs::File,
}

impl Drop for Spool {
    fn drop(&mut self) {
        let _ = self._lock.unlock();
    }
}

impl Spool {
    fn open(home: &Path) -> std::io::Result<Self> {
        let dir = home.join(PENDING_DIR);
        std::fs::create_dir_all(&dir)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700))?;
        }
        let process = Uuid::new_v4().to_string();
        let lock = std::fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(dir.join(format!("{process}.lock")))?;
        // A fresh name nobody else holds; a sweep leaves a lock file this young
        // alone (`LOCK_FILE_GRACE`).
        lock.try_lock().map_err(std::io::Error::other)?;
        Ok(Self {
            dir,
            process,
            _lock: lock,
        })
    }

    /// Remove this process's lock file when it leaves no record behind, so
    /// clean exits do not pile up lock files.
    fn retire(&self) {
        let own = format!("{}-", self.process);
        let left = std::fs::read_dir(&self.dir).is_ok_and(|listing| {
            listing.flatten().any(|item| {
                item.file_name()
                    .to_str()
                    .is_some_and(|name| name.starts_with(&own))
            })
        });
        if !left {
            let _ = std::fs::remove_file(self.dir.join(format!("{}.lock", self.process)));
        }
    }

    async fn save(&self, seq: u64, saved: &Saved) -> anyhow::Result<PathBuf> {
        let name = format!("{}-{seq:020}", self.process);
        let partial = self.dir.join(format!("{name}.tmp"));
        let path = self.dir.join(format!("{name}.json"));
        tokio::fs::write(&partial, serde_json::to_vec(saved)?).await?;
        tokio::fs::rename(&partial, &path).await?;
        Ok(path)
    }
}

/// Records saved by processes that are gone, oldest first per process. Each
/// is first renamed into this process's own records (numbered from `seq`), so
/// only one writer takes it, and one that exits before writing it leaves it
/// for the next.
fn orphans(spool: &Spool, seq: &AtomicU64) -> Vec<Entry> {
    let Ok(listing) = std::fs::read_dir(&spool.dir) else {
        return Vec::new();
    };
    let mut files: Vec<(String, PathBuf)> = Vec::new();
    let mut locks: Vec<String> = Vec::new();
    for item in listing.flatten() {
        let path = item.path();
        let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
            continue;
        };
        if let Some(process) = name.strip_suffix(".lock")
            && process.len() == 36
            && process != spool.process
        {
            locks.push(process.to_string());
            continue;
        }
        let Some((process, _)) = name.rsplit_once('-') else {
            continue;
        };
        if process.len() == 36 && process != spool.process {
            files.push((process.to_string(), path));
        }
    }
    files.sort();
    // Lock files of exited processes that left no record: removed, so clean
    // exits do not pile them up.
    for lock in locks {
        let has_records = files.iter().any(|(process, _)| *process == lock);
        // The claim (and its lock) lives until the file is gone.
        if !has_records
            && !recently_created(&spool.dir.join(format!("{lock}.lock")))
            && let Some(claim) = claim(&spool.dir, &lock)
            && let Some(path) = claim.path.as_ref()
        {
            let _ = std::fs::remove_file(path);
        }
    }
    let mut adopted = Vec::new();
    // The claim on each process seen: its lock, held while its records are
    // taken, so two writers never split one process's records between them.
    let mut claims: std::collections::HashMap<String, Option<Claim>> =
        std::collections::HashMap::new();
    for (process, path) in files {
        let claim = claims
            .entry(process.clone())
            .or_insert_with(|| claim(&spool.dir, &process));
        if claim.is_none() {
            continue;
        }
        if path.extension().is_some_and(|ext| ext == "tmp") {
            // Never completed: its record was not handed over either.
            let _ = std::fs::remove_file(&path);
            continue;
        }
        if path.extension().is_none_or(|ext| ext != "json") {
            continue;
        }
        let own = spool.dir.join(format!(
            "{}-{:020}.json",
            spool.process,
            seq.fetch_add(1, Ordering::Relaxed)
        ));
        if std::fs::rename(&path, &own).is_err() {
            continue;
        }
        let saved = std::fs::read(&own)
            .map_err(anyhow::Error::from)
            .and_then(|bytes| Ok(serde_json::from_slice::<Saved>(&bytes)?));
        match saved {
            Ok(saved) if saved.version == 1 => adopted.push(Entry {
                write: saved.write,
                // When it was first queued, so a parked record is not given
                // another full `OWNER_WAIT` each time it is adopted.
                queued_at: queued_at(saved.queued_at_ms),
                file: Some(own),
                waiter: None,
                adopted: true,
            }),
            Ok(_) | Err(_) => {
                tracing::warn!(
                    target: LOG_TARGET,
                    file = %own.display(),
                    "accounting: a saved record could not be read; left in place"
                );
            }
        }
    }
    // Every record of a claimed process is now this process's: its lock file
    // goes, then the lock.
    for claim in claims.into_values().flatten() {
        if let Some(path) = claim.path {
            let _ = std::fs::remove_file(path);
        }
    }
    adopted
}

/// The `Instant` a record saved at `queued_at_ms` (UTC) counts as queued from,
/// for `OWNER_WAIT`. An adopted record keeps its age, so it is not given a
/// fresh wait on every adoption, but always gets at least `ADOPTED_MIN_WAIT`:
/// the rows it refers to may be among the records adopted with it.
fn queued_at(queued_at_ms: i64) -> Instant {
    let oldest = OWNER_WAIT.saturating_sub(ADOPTED_MIN_WAIT);
    let age_ms = chrono::Utc::now()
        .timestamp_millis()
        .saturating_sub(queued_at_ms)
        .clamp(0, i64::try_from(oldest.as_millis()).unwrap_or(i64::MAX));
    let age = Duration::from_millis(u64::try_from(age_ms).unwrap_or(0));
    Instant::now().checked_sub(age).unwrap_or_else(Instant::now)
}

/// Whether `path` was modified within `LOCK_FILE_GRACE` (or cannot be read).
fn recently_created(path: &Path) -> bool {
    std::fs::metadata(path)
        .and_then(|metadata| metadata.modified())
        .ok()
        .and_then(|modified| modified.elapsed().ok())
        .is_none_or(|age| age < LOCK_FILE_GRACE)
}

/// A file lock this process holds, unlocked when dropped (#928). Closing the
/// file releases the lock only once every copy of its descriptor is closed,
/// and a child forked meanwhile (any spawn in this process) keeps a copy until
/// it starts its program, so another writer would find it still held.
struct HeldLock(std::fs::File);

impl Drop for HeldLock {
    fn drop(&mut self) {
        let _ = self.0.unlock();
    }
}

/// A gone process's records, claimed by holding its lock.
struct Claim {
    /// Its lock file, held; `None` when it had none left.
    path: Option<PathBuf>,
    _lock: Option<HeldLock>,
}

/// Claim the records of `process` if it has exited: its lock file is gone,
/// or no longer locked by it (nor by another writer claiming it).
fn claim(dir: &Path, process: &str) -> Option<Claim> {
    let path = dir.join(format!("{process}.lock"));
    match std::fs::OpenOptions::new().write(true).open(&path) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Some(Claim {
            path: None,
            _lock: None,
        }),
        Err(_) => None,
        Ok(file) => match file.try_lock() {
            Ok(()) => Some(Claim {
                path: Some(path),
                _lock: Some(HeldLock(file)),
            }),
            Err(_) => None,
        },
    }
}

/// The handle a `StateRuntime` keeps; the writer task ends with it.
pub struct AccountingWriter {
    commands: mpsc::UnboundedSender<Command>,
    spool: Option<Arc<Spool>>,
    shared: Arc<Shared>,
}

/// What a request checks before it waits for its record.
#[derive(Default)]
struct Shared {
    /// Entries handed over and not yet written or refused.
    pending: std::sync::atomic::AtomicUsize,
    /// The number of this process's next saved record.
    seq: AtomicU64,
    /// Set while the writer waits on something other than a write: the lock
    /// held elsewhere, or the hour's validation.
    waiting: AtomicBool,
    /// Turns of the writer's loop, for tests that it idles.
    #[cfg(test)]
    turns: AtomicU64,
}

impl AccountingWriter {
    fn start(runtime: &Arc<StateRuntime>) -> Self {
        let spool = match Spool::open(runtime.sqlite.home()) {
            Ok(spool) => Some(Arc::new(spool)),
            Err(error) => {
                tracing::warn!(
                    target: LOG_TARGET,
                    error = %error,
                    "accounting: cannot save pending records; records still queued when this process exits are lost"
                );
                None
            }
        };
        let (commands, receiver) = mpsc::unbounded_channel();
        let shared = Arc::new(Shared::default());
        tokio::spawn(run(
            Arc::downgrade(runtime),
            receiver,
            spool.clone(),
            Arc::clone(&shared),
        ));
        Self {
            commands,
            spool,
            shared,
        }
    }

    fn is_running(&self) -> bool {
        !self.commands.is_closed()
    }
}

impl StateRuntime {
    fn accounting_writer(self: &Arc<Self>) -> (mpsc::UnboundedSender<Command>, WriterRefs) {
        let mut slot = self
            .accounting_writer
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if !slot.as_ref().is_some_and(AccountingWriter::is_running) {
            *slot = None;
        }
        let writer = slot.get_or_insert_with(|| AccountingWriter::start(self));
        (
            writer.commands.clone(),
            WriterRefs {
                spool: writer.spool.clone(),
                seq: writer.shared.seq.fetch_add(1, Ordering::Relaxed),
                shared: Arc::clone(&writer.shared),
            },
        )
    }

    /// Hand one ledger record to this process's writer (see the module docs).
    ///
    /// Waits only when the writer can start on it at once, and then until the
    /// write meets another connection's write lock (at most
    /// `QUICK_BUSY_TIMEOUT`) or `WAIT_CAP`; otherwise returns `Deferred` and
    /// the record is written later. An error is a record the ledger refused.
    pub async fn record_accounting(
        self: &Arc<Self>,
        write: LedgerWrite,
    ) -> anyhow::Result<WriteOutcome> {
        let (commands, refs) = self.accounting_writer();
        let file = match refs.spool.as_ref() {
            Some(spool) => {
                let saved = Saved {
                    version: 1,
                    queued_at_ms: chrono::Utc::now().timestamp_millis(),
                    write: write.clone(),
                };
                match spool.save(refs.seq, &saved).await {
                    Ok(path) => Some(path),
                    Err(error) => {
                        tracing::warn!(
                            target: LOG_TARGET,
                            error = %format_args!("{error:#}"),
                            "accounting: could not save a pending record; it is lost if this process exits first"
                        );
                        None
                    }
                }
            }
            None => None,
        };
        let (waiter, settled) = oneshot::channel();
        let entry = Entry {
            write,
            queued_at: Instant::now(),
            file,
            waiter: Some(waiter),
            adopted: false,
        };
        let ahead = refs.shared.pending.fetch_add(1, Ordering::AcqRel);
        if commands.send(Command::Write(Box::new(entry))).is_err() {
            // The writer's runtime is gone; the saved file is picked up later.
            refs.shared.pending.fetch_sub(1, Ordering::AcqRel);
            return Ok(WriteOutcome::Deferred);
        }
        // Wait only for a write that starts now: none queued ahead, the writer
        // not held up, the lock not known to be taken.
        if ahead > 0 || refs.shared.waiting.load(Ordering::Acquire) || self.state_db_contended() {
            return Ok(WriteOutcome::Deferred);
        }
        match tokio::time::timeout(WAIT_CAP, settled).await {
            Ok(Ok(Ok(()))) => Ok(WriteOutcome::Written),
            Ok(Ok(Err(Settled::Failed(cause)))) => Err(anyhow::anyhow!(cause)),
            Ok(Ok(Err(Settled::Deferred))) | Ok(Err(_)) => Ok(WriteOutcome::Deferred),
            Err(_) => {
                // Slow, most likely waiting for the lock: the request's other
                // writes that can be put off are, rather than wait in turn.
                self.note_state_db_contended();
                Ok(WriteOutcome::Deferred)
            }
        }
    }

    /// Open (install or check) the ledger for a sampling about to record,
    /// waiting as for a record: not at all while the writer is held up, and at
    /// most `WAIT_CAP`. A ledger that cannot be opened now is opened by the
    /// first record that needs it, which reports a failure.
    pub async fn open_accounting(self: &Arc<Self>) {
        let (commands, refs) = self.accounting_writer();
        let (waiter, settled) = oneshot::channel();
        if commands.send(Command::Open(waiter)).is_err() {
            return;
        }
        // As for a record: wait only for an open that starts now.
        if refs.shared.pending.load(Ordering::Acquire) > 0
            || refs.shared.waiting.load(Ordering::Acquire)
            || self.state_db_contended()
        {
            return;
        }
        let _ = tokio::time::timeout(WAIT_CAP, settled).await;
    }

    /// Wait until this process's writer has nothing queued, at most `timeout`.
    /// Returns whether it got there. For tests.
    pub async fn flush_accounting(&self, timeout: Duration) -> bool {
        self.flush(timeout, /*until_blocked*/ false).await
    }

    /// Before the process may exit: wait, at most `timeout`, until the writer
    /// has written what it can, i.e. nothing is queued or what is left waits
    /// for another process (the write lock, or the hour's validation, held
    /// elsewhere). What is left stays saved for the next writer on the home.
    pub async fn settle_accounting(&self, timeout: Duration) -> bool {
        self.flush(timeout, /*until_blocked*/ true).await
    }

    async fn flush(&self, timeout: Duration, until_blocked: bool) -> bool {
        let commands = {
            let slot = self
                .accounting_writer
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            match slot.as_ref() {
                Some(writer) if writer.is_running() => writer.commands.clone(),
                _ => return true,
            }
        };
        let (done, flushed) = oneshot::channel();
        if commands
            .send(Command::Flush(Flush {
                done,
                until_blocked,
            }))
            .is_err()
        {
            return true;
        }
        matches!(tokio::time::timeout(timeout, flushed).await, Ok(Ok(())))
    }
}

struct WriterRefs {
    spool: Option<Arc<Spool>>,
    seq: u64,
    shared: Arc<Shared>,
}

/// What this writer has already done to the ledger.
#[derive(Default)]
struct Opened {
    /// `AccountingStore::open` (install or validate) succeeded.
    store: bool,
    /// The lookup indexes exist.
    indexes: bool,
    index_retry_at: Option<Instant>,
}

enum Failure {
    /// The runtime's pools were closed: the process is shutting down.
    Closed,
    /// Retry after the pause; the DB was busy or read-only.
    Busy(Duration),
    /// A row it refers to is not there: the conversation's (its metadata
    /// write may have been put off) or the admission's (maybe still queued in
    /// another process's writer).
    Waiting(anyhow::Error),
    /// The ledger refused the record.
    Refused(anyhow::Error),
}

fn classify(error: anyhow::Error) -> Failure {
    if error.chain().any(|cause| {
        matches!(
            cause.downcast_ref::<sqlx::Error>(),
            Some(sqlx::Error::PoolClosed)
        )
    }) {
        Failure::Closed
    } else if is_contention(&error) {
        Failure::Busy(RETRY_FIRST)
    } else if is_read_only(&error) || is_storage_failure(&error) {
        // Not cleared by the next commit elsewhere, and not the record's fault.
        Failure::Busy(READ_ONLY_RETRY)
    } else if error.chain().any(|cause| {
        cause.is::<OwnerMissing>() || cause.is::<NotAdmitted>() || cause.is::<PredecessorMissing>()
    }) {
        Failure::Waiting(error)
    } else {
        Failure::Refused(error)
    }
}

async fn run(
    runtime: Weak<StateRuntime>,
    mut commands: mpsc::UnboundedReceiver<Command>,
    spool: Option<Arc<Spool>>,
    shared: Arc<Shared>,
) {
    let mut inbox = Inbox::default();
    let mut opened = Opened::default();
    let mut next_sweep = Instant::now();
    let mut pause = RETRY_FIRST;
    loop {
        #[cfg(test)]
        shared.turns.fetch_add(1, Ordering::Relaxed);
        // `next_sweep` moves on with or without a spool: the idle wait below
        // sleeps until it, and must not spin.
        if Instant::now() >= next_sweep {
            next_sweep = Instant::now() + SWEEP_EVERY;
            if let Some(spool) = spool.as_ref() {
                let adopted = {
                    let spool = Arc::clone(spool);
                    let shared = Arc::clone(&shared);
                    tokio::task::spawn_blocking(move || orphans(&spool, &shared.seq))
                        .await
                        .unwrap_or_default()
                };
                if !adopted.is_empty() {
                    tracing::info!(
                        target: LOG_TARGET,
                        records = adopted.len(),
                        "accounting: writing records an exited process left pending"
                    );
                }
                for entry in adopted {
                    shared.pending.fetch_add(1, Ordering::AcqRel);
                    enqueue(&mut inbox.queue, Box::new(entry));
                }
            }
        }
        if inbox.queue.is_empty() && inbox.opens.is_empty() {
            for flush in inbox.flushes.drain(..) {
                let _ = flush.done.send(());
            }
            let wake = tokio::time::Instant::from_std(next_sweep);
            tokio::select! {
                command = commands.recv() => match command {
                    Some(command) => inbox.accept(command),
                    None => return retire(spool.as_ref()),
                },
                () = tokio::time::sleep_until(wake) => {}
            }
            continue;
        }
        while let Ok(command) = commands.try_recv() {
            inbox.accept(command);
        }
        let now = Instant::now();
        let ready = inbox
            .queue
            .iter()
            .position(|job| job.not_before.is_none_or(|at| at <= now));
        if ready.is_none() && inbox.opens.is_empty() {
            // Only parked jobs: nothing this writer can do now, so an exiting
            // process need not wait for them.
            inbox.answer_blocked();
            // Wait for the first, or for a new record.
            let wake = inbox
                .queue
                .iter()
                .filter_map(|job| job.not_before)
                .min()
                .unwrap_or(now + RETRY_MAX);
            tokio::select! {
                command = commands.recv() => match command {
                    Some(command) => inbox.accept(command),
                    None => return retire(spool.as_ref()),
                },
                () = tokio::time::sleep_until(tokio::time::Instant::from_std(wake)) => {}
            }
            continue;
        }
        let Some(state) = runtime.upgrade() else {
            return retire(spool.as_ref());
        };
        // The hour's whole-ledger validation runs in one process at a time,
        // before any write needs it; meanwhile records are put off.
        let lease = spool.as_ref().map(|spool| spool.dir.join(VALIDATION_LEASE));
        if !ensure_validated(&state, lease.as_deref(), &mut inbox, &shared).await {
            drop(state);
            inbox.answer_blocked();
            tokio::time::sleep(VALIDATION_POLL).await;
            continue;
        }
        if !inbox.opens.is_empty() {
            // A sampling starting: install or check the ledger now, so the
            // first record does not wait for it.
            let result = open_ledger(state.quick(), &mut opened).await;
            match result.map_err(classify) {
                Ok(()) => {
                    state.note_state_db_written();
                    for waiter in inbox.opens.drain(..) {
                        let _ = waiter.send(Ok(()));
                    }
                }
                Err(Failure::Busy(wait)) => {
                    state.note_state_db_contended();
                    for waiter in inbox.opens.drain(..) {
                        let _ = waiter.send(Err(Settled::Deferred));
                    }
                    drop(state);
                    inbox.answer_blocked();
                    tokio::time::sleep(wait.max(pause)).await;
                    pause = (pause * 2).min(RETRY_MAX);
                    continue;
                }
                Err(Failure::Closed) => return retire(spool.as_ref()),
                Err(Failure::Waiting(_) | Failure::Refused(_)) => {
                    // Reported by the first record that needs the ledger.
                    for waiter in inbox.opens.drain(..) {
                        let _ = waiter.send(Err(Settled::Deferred));
                    }
                }
            }
            continue;
        }
        let Some(index) = ready else {
            continue;
        };
        let Some(job) = inbox.queue.get_mut(index) else {
            continue;
        };
        // The first try waits at most `QUICK_BUSY_TIMEOUT` for the lock, so a
        // request waiting on it is told soon; retries wait SQLite's full timeout.
        let target = if job.tries == 0 {
            state.quick()
        } else {
            &state
        };
        job.tries += 1;
        let result = apply(target, &mut opened, job).await;
        match result.map_err(classify) {
            Ok(()) => {
                shared.waiting.store(false, Ordering::Release);
                state.note_state_db_written();
                pause = RETRY_FIRST;
                if let Some(mut job) = inbox.queue.remove(index) {
                    for file in &job.files {
                        let _ = tokio::fs::remove_file(file).await;
                    }
                    shared.pending.fetch_sub(job.entries, Ordering::AcqRel);
                    job.settle(|| Ok(()));
                }
            }
            Err(Failure::Busy(wait)) => {
                if !shared.waiting.swap(true, Ordering::AcqRel) {
                    tracing::info!(
                        target: LOG_TARGET,
                        "accounting: state DB busy; records are written once it is free"
                    );
                }
                state.note_state_db_contended();
                job.settle(|| Err(Settled::Deferred));
                drop(state);
                inbox.answer_blocked();
                tokio::time::sleep(wait.max(pause)).await;
                pause = (pause * 2).min(RETRY_MAX);
            }
            // Shutting down: the saved records are left for the next writer.
            Err(Failure::Closed) => return retire(spool.as_ref()),
            Err(Failure::Waiting(_))
                if job.queued_at.elapsed() < OWNER_WAIT
                    && (job.adopted || state.thread_row_put_off(job.owner)) =>
            {
                job.settle(|| Err(Settled::Deferred));
                job.not_before = Some(Instant::now() + RETRY_MAX);
            }
            Err(Failure::Waiting(error)) => {
                refuse(
                    &state,
                    &mut inbox.queue,
                    index,
                    &shared,
                    format!("{error:#}"),
                )
                .await;
            }
            Err(Failure::Refused(error)) => {
                opened.store = false;
                let keep = crate::runtime::accounting::store::is_newer_format(&error);
                let cause = format!("{error:#}");
                if keep {
                    // A newer build can write it; leave the file for it.
                    if let Some(mut job) = inbox.queue.remove(index) {
                        shared.pending.fetch_sub(job.entries, Ordering::AcqRel);
                        job.settle(|| Err(Settled::Failed(cause.clone())));
                    }
                } else {
                    refuse(&state, &mut inbox.queue, index, &shared, cause).await;
                }
            }
        }
    }
}

/// What the writer has been handed and not finished.
#[derive(Default)]
struct Inbox {
    queue: VecDeque<Job>,
    flushes: Vec<Flush>,
    /// Samplings waiting for the ledger to be opened.
    opens: Vec<Waiter>,
}

impl Inbox {
    fn accept(&mut self, command: Command) {
        match command {
            Command::Write(entry) => enqueue(&mut self.queue, entry),
            Command::Flush(flush) => self.flushes.push(flush),
            Command::Open(waiter) => self.opens.push(waiter),
        }
    }

    /// Answer the flushes that wait only until the writer is blocked.
    fn answer_blocked(&mut self) {
        let (blocked, rest): (Vec<Flush>, Vec<Flush>) = self
            .flushes
            .drain(..)
            .partition(|flush| flush.until_blocked);
        self.flushes = rest;
        for flush in blocked {
            let _ = flush.done.send(());
        }
    }
}

/// The writer is ending: drop this process's lock file if it leaves no record.
fn retire(spool: Option<&Arc<Spool>>) {
    if let Some(spool) = spool.cloned() {
        drop(tokio::task::spawn_blocking(move || spool.retire()));
    }
}

fn enqueue(queue: &mut VecDeque<Job>, entry: Box<Entry>) {
    // Only the newest job for an attempt can take more of its records;
    // anything earlier would move them ahead of records queued in between.
    let entry = match queue
        .iter_mut()
        .rev()
        .find(|job| job.attempt == *entry.write.attempt())
    {
        Some(job) => match job.absorb(entry) {
            Ok(()) => return,
            Err(entry) => entry,
        },
        None => entry,
    };
    queue.push_back(Job::new(entry));
}

async fn refuse(
    state: &StateRuntime,
    queue: &mut VecDeque<Job>,
    index: usize,
    shared: &Shared,
    cause: String,
) {
    let Some(mut job) = queue.remove(index) else {
        return;
    };
    shared.pending.fetch_sub(job.entries, Ordering::AcqRel);
    // A request no longer waiting for this record learns of it through the
    // count (`StateRuntime::accounting_refusals`).
    state.note_accounting_refused(job.owner);
    tracing::error!(
        target: LOG_TARGET,
        step = "write record",
        cause = %cause,
        "developer accounting failed; this record is not in the ledger"
    );
    // #314 (D228): every refused record passes here, whoever sent it and
    // whether or not anything still waits for it, so its day is marked here.
    let dispatched = i64::from(job.attempt.dispatched_at_ms);
    if let Err(error) = AccountingStore::record_gap(state, job.owner, dispatched, dispatched).await
    {
        tracing::warn!(
            target: LOG_TARGET,
            cause = %format_args!("{error:#}"),
            "accounting: could not mark this record's day as incomplete"
        );
    }
    for file in &job.files {
        let _ = tokio::fs::remove_file(file).await;
    }
    job.settle(|| Err(Settled::Failed(cause.clone())));
}

/// Make sure the ledger was validated whole this hour before a write needs
/// it, holding the validation lease (`lease`) while this process runs it.
/// Returns false while another process holds the lease: the caller retries
/// after `VALIDATION_POLL`. The validation reads every retained attempt; on a
/// large ledger it takes seconds (more in a debug build), so queued records
/// are put off rather than waited for, and every process running it at once
/// only slowed it down: each process validated, none got as far as writing,
/// and none moved the checkpoint that tells the others it was done.
async fn ensure_validated(
    runtime: &StateRuntime,
    lease: Option<&Path>,
    inbox: &mut Inbox,
    shared: &Shared,
) -> bool {
    let store = AccountingStore { runtime };
    if !store.validation_due(AsOf::Now).await.unwrap_or(false) {
        return true;
    }
    shared.waiting.store(true, Ordering::Release);
    for job in inbox.queue.iter_mut() {
        job.settle(|| Err(Settled::Deferred));
    }
    for waiter in inbox.opens.drain(..) {
        let _ = waiter.send(Err(Settled::Deferred));
    }
    let _lease = match lease.map(take_lease) {
        Some(Ok(Some(file))) => Some(file),
        Some(Ok(None)) => return false,
        // Without a lease file this process validates on its own.
        Some(Err(_)) | None => None,
    };
    // The previous holder may have just finished.
    if !store.validation_due(AsOf::Now).await.unwrap_or(false) {
        return true;
    }
    let started = Instant::now();
    // Moving the checkpoint into this hour tells the other processes it was
    // done. If that commit fails (busy), the validation is still kept for the
    // hour (`validate_hour`), so this process's next write moves it; any other
    // failure is that write's to report.
    match store.maintain_for_write(AsOf::Now).await {
        Ok(()) => tracing::info!(
            target: LOG_TARGET,
            elapsed_ms = started.elapsed().as_millis() as u64,
            "accounting: validated the ledger for this hour"
        ),
        Err(error) => tracing::debug!(
            target: LOG_TARGET,
            error = %format_args!("{error:#}"),
            "accounting: the hour's ledger validation was not recorded yet"
        ),
    }
    true
}

/// The validation lease, if no other process holds it.
fn take_lease(path: &Path) -> std::io::Result<Option<HeldLock>> {
    let file = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(path)?;
    match file.try_lock() {
        Ok(()) => Ok(Some(HeldLock(file))),
        Err(std::fs::TryLockError::WouldBlock) => Ok(None),
        Err(std::fs::TryLockError::Error(error)) => Err(error),
    }
}

/// Install or check the ledger once per writer, then its lookup indexes.
async fn open_ledger(runtime: &StateRuntime, opened: &mut Opened) -> anyhow::Result<()> {
    if !opened.store {
        AccountingStore::open(runtime, AsOf::Now).await?;
        opened.store = true;
    }
    if !opened.indexes && opened.index_retry_at.is_none_or(|at| at <= Instant::now()) {
        opened.indexes = super::indexes::ensure(runtime).await;
        opened.index_retry_at = Some(Instant::now() + SWEEP_EVERY);
    }
    Ok(())
}

async fn apply(runtime: &StateRuntime, opened: &mut Opened, job: &Job) -> anyhow::Result<()> {
    open_ledger(runtime, opened).await?;
    let store = AccountingStore { runtime };
    store
        .write(
            job.owner,
            &job.attempt,
            &job.observations,
            job.prices.as_deref(),
            AsOf::Now,
        )
        .await?;
    Ok(())
}

#[cfg(test)]
#[path = "accounting_writer_tests.rs"]
mod tests;
