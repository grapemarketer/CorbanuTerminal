use super::*;
use crate::runtime::test_support::test_thread_metadata;
use pretty_assertions::assert_eq;
use serde_json::json;
use std::time::Duration;

fn home() -> impl std::ops::Deref<Target = std::path::PathBuf> {
    scopeguard::guard(crate::runtime::test_support::unique_temp_dir(), |path| {
        let _ = std::fs::remove_dir_all(path);
    })
}

async fn runtime_with_owner(path: &Path, owner: ThreadId) -> anyhow::Result<Arc<StateRuntime>> {
    let runtime = StateRuntime::init_for_testing(path.to_path_buf(), "synthetic".into()).await?;
    add_owner(&runtime, owner).await?;
    Ok(runtime)
}

async fn add_owner(runtime: &StateRuntime, owner: ThreadId) -> anyhow::Result<()> {
    let home = runtime.sqlite().home();
    runtime
        .upsert_thread(&test_thread_metadata(home, owner, home.to_path_buf()))
        .await
}

fn attempt(owner: ThreadId) -> Attempt {
    serde_json::from_value(json!({
        "attempt_id": Uuid::new_v4(), "request_id": Uuid::new_v4(), "thread_id": owner,
        "turn": "fixture", "retry_of": null, "provider": "synthetic", "model": "fixture",
        "scope": Uuid::nil(), "dialect": "NativeAnthropic",
        "dispatched_at_ms": chrono::Utc::now().timestamp_millis()
    }))
    .unwrap()
}

fn snapshot() -> Snapshot {
    serde_json::from_value(json!({
        "id": Uuid::new_v4(), "provider": "synthetic", "model": "fixture",
        "scope": Uuid::nil(), "currency": "USD", "unit": "PerMillionTokens",
        "rates": {"noncached": "1", "read": null, "write": null, "output": null},
        "source_reference": Uuid::new_v4(), "source_kind": "ProviderPublished",
        "observed_at_ms": 0, "approved_at_ms": 0, "effective_from_ms": 0, "effective_end_ms": null
    }))
    .unwrap()
}

fn observation(attempt: &Attempt, revision: i64) -> Observation {
    serde_json::from_value(json!({
        "revision": revision, "source": attempt.attempt_id, "sequence": revision,
        "patch": {"input": 100 * revision, "output": 10 * revision}
    }))
    .unwrap()
}

fn admit(owner: ThreadId, attempt: &Attempt) -> LedgerWrite {
    LedgerWrite::Admit {
        owner,
        attempt: attempt.clone(),
        prices: vec![snapshot()],
    }
}

fn observe(owner: ThreadId, attempt: &Attempt, revision: i64) -> LedgerWrite {
    LedgerWrite::Observe {
        owner,
        attempt: attempt.clone(),
        observations: vec![observation(attempt, revision)],
    }
}

/// (attempts, observations) in the ledger for `attempt`.
async fn recorded(runtime: &StateRuntime, attempt: &Attempt) -> anyhow::Result<(i64, i64)> {
    let id = attempt.attempt_id.to_string();
    let attempts: i64 =
        sqlx::query_scalar("SELECT count(*) FROM draft_accounting_attempts WHERE attempt_id = ?")
            .bind(&id)
            .fetch_one(runtime.pool.as_ref())
            .await?;
    let observations: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM draft_accounting_observations WHERE attempt_id = ?",
    )
    .bind(&id)
    .fetch_one(runtime.pool.as_ref())
    .await?;
    Ok((attempts, observations))
}

fn pending_records(home: &Path) -> Vec<PathBuf> {
    let mut files: Vec<PathBuf> = std::fs::read_dir(home.join(PENDING_DIR))
        .map(|listing| listing.flatten().map(|item| item.path()).collect())
        .unwrap_or_default();
    files.retain(|path| path.extension().is_some_and(|ext| ext == "json"));
    files.sort();
    files
}

/// Hold the state DB's write lock from a connection of another pool, as
/// another Corbanu process would.
async fn hold_write_lock(
    runtime: &StateRuntime,
) -> anyhow::Result<sqlx::pool::PoolConnection<sqlx::Sqlite>> {
    let pool = runtime
        .sqlite()
        .open_read_write_pool(&runtime.sqlite().state_db_path())
        .await?;
    let mut conn = pool.acquire().await?;
    sqlx::query("BEGIN IMMEDIATE").execute(&mut *conn).await?;
    Ok(conn)
}

#[tokio::test]
async fn records_are_written_in_order_and_their_files_removed() -> anyhow::Result<()> {
    let path = home();
    let owner = ThreadId::new();
    let runtime = runtime_with_owner(&path, owner).await?;
    let first = attempt(owner);
    assert_eq!(
        runtime.record_accounting(admit(owner, &first)).await?,
        WriteOutcome::Written
    );
    assert_eq!(
        runtime
            .record_accounting(observe(owner, &first, /*revision*/ 1))
            .await?,
        WriteOutcome::Written
    );
    assert_eq!(recorded(&runtime, &first).await?, (1, 1));
    assert_eq!(pending_records(&path), Vec::<PathBuf>::new());
    // The ledger's lookup indexes exist after the first write.
    for name in super::super::indexes::NAMES {
        let present: bool =
            sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM sqlite_schema WHERE name = ?)")
                .bind(name)
                .fetch_one(runtime.pool.as_ref())
                .await?;
        assert!(present, "{name}");
    }
    runtime.close().await;
    Ok(())
}

#[tokio::test]
async fn a_busy_state_db_puts_records_off_without_waiting_and_loses_none() -> anyhow::Result<()> {
    let path = home();
    let owner = ThreadId::new();
    let runtime = runtime_with_owner(&path, owner).await?;
    // The ledger is installed and the writer running before the lock is taken.
    let warm = attempt(owner);
    runtime.record_accounting(admit(owner, &warm)).await?;

    let mut lock = hold_write_lock(&runtime).await?;
    let first = attempt(owner);
    let started = Instant::now();
    assert_eq!(
        runtime.record_accounting(admit(owner, &first)).await?,
        WriteOutcome::Deferred
    );
    let waited = started.elapsed();
    assert!(
        waited < crate::runtime::QUICK_BUSY_TIMEOUT + Duration::from_millis(750),
        "the first record waited {waited:?}"
    );
    // While the writer is retrying, later records are put off at once.
    let started = Instant::now();
    assert_eq!(
        runtime
            .record_accounting(observe(owner, &first, /*revision*/ 1))
            .await?,
        WriteOutcome::Deferred
    );
    assert!(started.elapsed() < Duration::from_millis(250));
    assert!(runtime.state_db_contended());
    // Both are saved until they are written.
    assert_eq!(pending_records(&path).len(), 2);
    assert_eq!(recorded(&runtime, &first).await?, (0, 0));

    sqlx::query("ROLLBACK").execute(&mut *lock).await?;
    drop(lock);
    assert!(runtime.flush_accounting(Duration::from_secs(30)).await);
    assert_eq!(recorded(&runtime, &first).await?, (1, 1));
    assert_eq!(pending_records(&path), Vec::<PathBuf>::new());
    assert!(!runtime.state_db_contended());
    runtime.close().await;
    Ok(())
}

#[tokio::test]
async fn records_an_exited_process_left_are_written_once() -> anyhow::Result<()> {
    let path = home();
    let owner = ThreadId::new();
    let dir = path.join(PENDING_DIR);
    std::fs::create_dir_all(&dir)?;
    let save = |process: &str, seq: u64, write: LedgerWrite| -> anyhow::Result<()> {
        let saved = Saved {
            version: 1,
            queued_at_ms: 0,
            write,
        };
        std::fs::write(
            dir.join(format!("{process}-{seq:020}.json")),
            serde_json::to_vec(&saved)?,
        )?;
        Ok(())
    };
    // A process that crashed without its lock file, one whose lock file is
    // left but unlocked, and one still running (its lock is held here).
    let crashed = Uuid::new_v4().to_string();
    let exited = Uuid::new_v4().to_string();
    let running = Uuid::new_v4().to_string();
    let (a, b, c) = (attempt(owner), attempt(owner), attempt(owner));
    save(&crashed, 0, admit(owner, &a))?;
    save(&crashed, 1, observe(owner, &a, /*revision*/ 1))?;
    // Already written once before the crash: applying it again changes nothing.
    save(&crashed, 2, observe(owner, &a, /*revision*/ 1))?;
    std::fs::write(dir.join(format!("{exited}.lock")), b"")?;
    save(&exited, 0, admit(owner, &b))?;
    let held = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(dir.join(format!("{running}.lock")))?;
    held.lock()?;
    save(&running, 0, admit(owner, &c))?;

    let runtime = runtime_with_owner(&path, owner).await?;
    // Starting the writer adopts what the exited processes left.
    runtime
        .record_accounting(admit(owner, &attempt(owner)))
        .await?;
    assert!(runtime.flush_accounting(Duration::from_secs(30)).await);
    assert_eq!(recorded(&runtime, &a).await?, (1, 1));
    assert_eq!(recorded(&runtime, &b).await?, (1, 0));
    assert_eq!(recorded(&runtime, &c).await?, (0, 0));
    assert_eq!(
        pending_records(&path),
        vec![dir.join(format!("{running}-{:020}.json", 0))]
    );
    assert!(!dir.join(format!("{exited}.lock")).exists());
    assert!(dir.join(format!("{running}.lock")).exists());
    drop(held);
    runtime.close().await;
    Ok(())
}

#[tokio::test]
async fn a_refused_record_is_an_error_and_not_kept() -> anyhow::Result<()> {
    let path = home();
    let owner = ThreadId::new();
    let runtime = runtime_with_owner(&path, owner).await?;
    // Recorded under a conversation that is not the attempt's.
    let misowned = attempt(ThreadId::new());
    let error = runtime
        .record_accounting(admit(owner, &misowned))
        .await
        .unwrap_err();
    assert!(
        format!("{error:#}").contains("native owner mismatch"),
        "{error:#}"
    );
    assert_eq!(pending_records(&path), Vec::<PathBuf>::new());
    // #314 (D228): the refused record's day is marked for the conversation
    // it was recorded under, whether or not anything still waits for it.
    let marked: Vec<(String, i64)> =
        sqlx::query_as("SELECT thread_id, utc_day FROM draft_accounting_gap_days")
            .fetch_all(runtime.pool.as_ref())
            .await?;
    assert_eq!(
        marked,
        vec![(
            owner.to_string(),
            i64::from(misowned.dispatched_at_ms).div_euclid(86_400_000)
        )]
    );
    // The writer goes on with later records.
    let next = attempt(owner);
    assert_eq!(
        runtime.record_accounting(admit(owner, &next)).await?,
        WriteOutcome::Written
    );
    runtime.close().await;
    Ok(())
}

#[tokio::test]
async fn a_record_waits_for_its_conversation_row() -> anyhow::Result<()> {
    let path = home();
    let owner = ThreadId::new();
    let runtime = runtime_with_owner(&path, owner).await?;
    // A conversation whose row the thread store put off writing.
    let late = ThreadId::new();
    runtime.note_thread_row_put_off(late, /*put_off*/ true);
    let first = attempt(late);
    assert_eq!(
        runtime.record_accounting(admit(late, &first)).await?,
        WriteOutcome::Deferred
    );
    // Other conversations' records are not held up behind it (though, with a
    // record queued ahead, they are put off rather than waited for).
    let other = attempt(owner);
    runtime.record_accounting(admit(owner, &other)).await?;
    tokio::time::timeout(Duration::from_secs(10), async {
        while recorded(&runtime, &other).await? != (1, 0) {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        anyhow::Ok(())
    })
    .await??;
    assert_eq!(recorded(&runtime, &first).await?, (0, 0));
    add_owner(&runtime, late).await?;
    assert!(runtime.flush_accounting(Duration::from_secs(30)).await);
    assert_eq!(recorded(&runtime, &first).await?, (1, 0));
    // Any other conversation without a row is refused at once, as before.
    let unknown = ThreadId::new();
    let error = runtime
        .record_accounting(admit(unknown, &attempt(unknown)))
        .await
        .unwrap_err();
    assert!(
        format!("{error:#}").contains("native owner missing"),
        "{error:#}"
    );
    runtime.close().await;
    Ok(())
}

/// Usage can reach a writer before its admission does when the records of an
/// exited process without a lock file are split between two writers: an
/// adopted record waits for the admission.
#[tokio::test]
async fn adopted_usage_waits_for_its_admission() -> anyhow::Result<()> {
    let path = home();
    let owner = ThreadId::new();
    let setup = runtime_with_owner(&path, owner).await?;
    let first = attempt(owner);
    let dir = path.join(PENDING_DIR);
    std::fs::create_dir_all(&dir)?;
    let exited = Uuid::new_v4().to_string();
    let saved = Saved {
        version: 1,
        queued_at_ms: chrono::Utc::now().timestamp_millis(),
        write: observe(owner, &first, /*revision*/ 1),
    };
    std::fs::write(
        dir.join(format!("{exited}-{:020}.json", 1)),
        serde_json::to_vec(&saved)?,
    )?;
    // A fresh runtime's writer adopts it at start.
    let runtime = StateRuntime::init_for_testing(path.to_path_buf(), "synthetic".into()).await?;
    runtime.open_accounting().await;
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert_eq!(recorded(&runtime, &first).await?, (0, 0));
    assert_eq!(
        setup.record_accounting(admit(owner, &first)).await?,
        WriteOutcome::Written
    );
    assert!(runtime.flush_accounting(Duration::from_secs(30)).await);
    assert_eq!(recorded(&runtime, &first).await?, (1, 1));
    setup.close().await;
    runtime.close().await;
    Ok(())
}

#[tokio::test]
async fn the_owner_day_query_uses_the_lookup_index() -> anyhow::Result<()> {
    let path = home();
    let owner = ThreadId::new();
    let runtime = runtime_with_owner(&path, owner).await?;
    runtime
        .record_accounting(admit(owner, &attempt(owner)))
        .await?;
    // The statements `check_day` and retention use, as written there.
    let plan: Vec<(i64, i64, i64, String)> = sqlx::query_as(
        "EXPLAIN QUERY PLAN SELECT attempt_id, payload FROM draft_accounting_attempts
            WHERE json_extract(payload, '$.thread_id') = ?
              AND json_extract(payload, '$.dispatched_at_ms') >= ?
              AND json_extract(payload, '$.dispatched_at_ms') < ?
            ORDER BY attempt_id",
    )
    .bind(owner.to_string())
    .bind(0)
    .bind(1)
    .fetch_all(runtime.pool.as_ref())
    .await?;
    assert!(
        plan.iter()
            .any(|(.., detail)| detail.contains("corbanu_lookup_attempt_owner_dispatch")),
        "{plan:?}"
    );
    let plan: Vec<(i64, i64, i64, String)> = sqlx::query_as(
        "EXPLAIN QUERY PLAN SELECT EXISTS(SELECT 1 FROM draft_accounting_attempts
            WHERE json_extract(payload, '$.dispatched_at_ms') <= ?)",
    )
    .bind(0)
    .fetch_all(runtime.pool.as_ref())
    .await?;
    assert!(
        plan.iter()
            .any(|(.., detail)| detail.contains("corbanu_lookup_attempt_dispatch")),
        "{plan:?}"
    );
    // The expiry batch's statement (`accounting_retention_incremental.rs`).
    let plan: Vec<(i64, i64, i64, String)> = sqlx::query_as(
        "EXPLAIN QUERY PLAN SELECT attempt_id FROM draft_accounting_attempts
            WHERE json_extract(payload, '$.dispatched_at_ms') <= ?
            ORDER BY json_extract(payload, '$.dispatched_at_ms'), attempt_id LIMIT ?",
    )
    .bind(0)
    .bind(10)
    .fetch_all(runtime.pool.as_ref())
    .await?;
    assert!(
        plan.iter()
            .any(|(.., detail)| detail.contains("corbanu_lookup_attempt_dispatch")),
        "{plan:?}"
    );
    runtime.close().await;
    Ok(())
}

/// The writer of `runtime`, for tests that look at its state.
fn writer_turns(runtime: &StateRuntime) -> u64 {
    runtime
        .accounting_writer
        .lock()
        .unwrap()
        .as_ref()
        .map_or(0, |writer| writer.shared.turns.load(Ordering::Relaxed))
}

/// Records are still written when they cannot be saved first (the pending
/// folder cannot be created), and the idle writer does not spin.
#[tokio::test]
async fn without_a_pending_folder_records_are_written_and_the_writer_idles() -> anyhow::Result<()> {
    let path = home();
    let owner = ThreadId::new();
    let runtime = runtime_with_owner(&path, owner).await?;
    std::fs::write(path.join(PENDING_DIR), b"not a folder")?;
    let first = attempt(owner);
    assert_eq!(
        runtime.record_accounting(admit(owner, &first)).await?,
        WriteOutcome::Written
    );
    assert_eq!(recorded(&runtime, &first).await?, (1, 0));
    let before = writer_turns(&runtime);
    tokio::time::sleep(Duration::from_millis(300)).await;
    let idle_turns = writer_turns(&runtime) - before;
    assert!(idle_turns < 5, "the idle writer turned {idle_turns} times");
    runtime.close().await;
    Ok(())
}

/// A lock file with no records is swept once its process is gone, but never
/// while it is young: its process may be between creating and locking it.
#[tokio::test]
async fn lock_files_without_records_are_swept_only_when_old() -> anyhow::Result<()> {
    let path = home();
    let owner = ThreadId::new();
    let dir = path.join(PENDING_DIR);
    std::fs::create_dir_all(&dir)?;
    let young = dir.join(format!("{}.lock", Uuid::new_v4()));
    let old = dir.join(format!("{}.lock", Uuid::new_v4()));
    std::fs::write(&young, b"")?;
    std::fs::File::create(&old)?
        .set_modified(std::time::SystemTime::now() - Duration::from_secs(3_600))?;
    let runtime = runtime_with_owner(&path, owner).await?;
    // Starting the writer sweeps.
    runtime
        .record_accounting(admit(owner, &attempt(owner)))
        .await?;
    assert!(young.exists());
    assert!(!old.exists());
    runtime.close().await;
    Ok(())
}

/// A process exiting does not wait for records parked on a row that is not
/// there: the writer can do nothing for them now.
#[tokio::test]
async fn settling_does_not_wait_for_parked_records() -> anyhow::Result<()> {
    let path = home();
    let owner = ThreadId::new();
    let runtime = runtime_with_owner(&path, owner).await?;
    let late = ThreadId::new();
    runtime.note_thread_row_put_off(late, /*put_off*/ true);
    runtime
        .record_accounting(admit(late, &attempt(late)))
        .await?;
    let started = Instant::now();
    assert!(runtime.settle_accounting(Duration::from_secs(20)).await);
    assert!(
        started.elapsed() < Duration::from_secs(5),
        "{:?}",
        started.elapsed()
    );
    // Still saved for a later writer.
    assert_eq!(pending_records(&path).len(), 1);
    runtime.close().await;
    Ok(())
}

/// One process at a time runs the hour's whole-ledger validation. While
/// another holds the lease, records are put off without waiting and an
/// exiting process does not wait; once the lease is free, this writer
/// validates and writes them.
#[tokio::test]
async fn the_hourly_validation_waits_for_the_lease() -> anyhow::Result<()> {
    let path = home();
    let owner = ThreadId::new();
    let setup = runtime_with_owner(&path, owner).await?;
    let hour_ms = 60 * 60 * 1000;
    let now_ms = chrono::Utc::now().timestamp_millis();
    // An earlier record, and the checkpoint left in an earlier hour.
    let mut old = attempt(owner);
    old.dispatched_at_ms = (now_ms - 3 * hour_ms).try_into()?;
    setup.record_accounting(admit(owner, &old)).await?;
    sqlx::query("UPDATE draft_accounting_retention_checkpoint SET completed_as_of_ms = ?")
        .bind(now_ms - 2 * hour_ms)
        .execute(setup.pool.as_ref())
        .await?;
    setup.close().await;

    let lease = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(path.join(PENDING_DIR).join(VALIDATION_LEASE))?;
    lease.lock()?;
    let runtime = StateRuntime::init_for_testing(path.to_path_buf(), "synthetic".into()).await?;
    let next = attempt(owner);
    let started = Instant::now();
    assert_eq!(
        runtime.record_accounting(admit(owner, &next)).await?,
        WriteOutcome::Deferred
    );
    assert!(
        started.elapsed() < Duration::from_secs(2),
        "{:?}",
        started.elapsed()
    );
    assert!(runtime.settle_accounting(Duration::from_secs(20)).await);
    assert!(
        started.elapsed() < Duration::from_secs(5),
        "{:?}",
        started.elapsed()
    );
    assert_eq!(recorded(&runtime, &next).await?, (0, 0));
    drop(lease);
    assert!(runtime.flush_accounting(Duration::from_secs(30)).await);
    assert_eq!(recorded(&runtime, &next).await?, (1, 0));
    let checkpoint: i64 =
        sqlx::query_scalar("SELECT completed_as_of_ms FROM draft_accounting_retention_checkpoint")
            .fetch_one(runtime.pool.as_ref())
            .await?;
    assert!(checkpoint >= now_ms - now_ms.rem_euclid(hour_ms));
    runtime.close().await;
    Ok(())
}

/// A retry adopted from an exited process before its predecessor's admission
/// is in the ledger waits for it instead of being refused, even when it was
/// saved long ago.
#[tokio::test]
async fn an_adopted_retry_waits_for_its_predecessor() -> anyhow::Result<()> {
    let path = home();
    let owner = ThreadId::new();
    let setup = runtime_with_owner(&path, owner).await?;
    let first = attempt(owner);
    let mut retry = attempt(owner);
    retry.request_id = first.request_id;
    retry.retry_of = Some(first.attempt_id);
    let dir = path.join(PENDING_DIR);
    std::fs::create_dir_all(&dir)?;
    let saved = Saved {
        version: 1,
        // Saved well past `OWNER_WAIT` ago.
        queued_at_ms: chrono::Utc::now().timestamp_millis() - 3_600_000,
        write: admit(owner, &retry),
    };
    std::fs::write(
        dir.join(format!("{}-{:020}.json", Uuid::new_v4(), 0)),
        serde_json::to_vec(&saved)?,
    )?;
    let runtime = StateRuntime::init_for_testing(path.to_path_buf(), "synthetic".into()).await?;
    runtime.open_accounting().await;
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert_eq!(recorded(&runtime, &retry).await?, (0, 0));
    assert_eq!(
        setup.record_accounting(admit(owner, &first)).await?,
        WriteOutcome::Written
    );
    assert!(runtime.flush_accounting(Duration::from_secs(30)).await);
    assert_eq!(recorded(&runtime, &retry).await?, (1, 0));
    assert_eq!(runtime.accounting_refusals(owner), 0);
    setup.close().await;
    runtime.close().await;
    Ok(())
}

/// Ten sessions on one home, each with its own runtime as a separate process
/// has, making model requests while another connection holds the write lock
/// for seconds partway through (as a long maintenance or a backup does). What
/// a request waits on in the shared state DB - the throttle check, its
/// admission, a thread-metadata write and its usage - never adds up to more
/// than about a second (a few on loaded Windows runners, still well short of
/// the hold), nothing fails, and every record ends up in the ledger.
///
/// The bounds hold when the test has the four cores its runtime uses: Bazel
/// reserves them (`cpu:4` in state's BUILD.bazel) and so does nextest
/// (`threads-required`). Scheduled as a one-core test beside compiles on a
/// 4-core Windows runner, its waits stretched past the bound (#905).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn ten_sessions_on_one_home_wait_briefly_and_lose_no_record() -> anyhow::Result<()> {
    const SESSIONS: usize = 10;
    const REQUESTS: usize = 12;
    // Longer on Windows, so its looser bounds below stay well short of a
    // request that waited out the whole hold.
    const HOLD: Duration = Duration::from_secs(if cfg!(windows) { 6 } else { 3 });
    let path = home();
    let setup = StateRuntime::init_for_testing(path.to_path_buf(), "synthetic".into()).await?;
    let owners: Vec<ThreadId> = (0..SESSIONS).map(|_| ThreadId::new()).collect();
    for owner in &owners {
        add_owner(&setup, *owner).await?;
    }
    let holder = {
        let setup = Arc::clone(&setup);
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(800)).await;
            let mut lock = hold_write_lock(&setup).await?;
            tokio::time::sleep(HOLD).await;
            sqlx::query("ROLLBACK").execute(&mut *lock).await?;
            anyhow::Ok(())
        })
    };
    let mut sessions = Vec::new();
    for owner in owners.clone() {
        let home = path.to_path_buf();
        sessions.push(tokio::spawn(async move {
            let runtime = StateRuntime::init_for_testing(home.clone(), "synthetic".into()).await?;
            let key = crate::ProviderRequestKey {
                provider_id: "synthetic".into(),
                model: "fixture".into(),
                key_fingerprint: format!("fixture:{owner}"),
            };
            let preflight = crate::ProviderRequestPreflight {
                input_tokens: 100,
                cached_input_tokens: 0,
                request_bytes: 400,
                thread_id: Some(owner.to_string()),
                turn_id: None,
            };
            let mut waits = Vec::new();
            let mut attempts = Vec::new();
            for _ in 0..REQUESTS {
                let started = Instant::now();
                let now_ms = chrono::Utc::now().timestamp_millis();
                runtime
                    .check_provider_request_throttle(&key, &preflight, now_ms)
                    .await;
                let next = attempt(owner);
                runtime.record_accounting(admit(owner, &next)).await?;
                // A best-effort metadata write, as the thread store makes it.
                if !runtime.state_db_contended() {
                    let metadata = test_thread_metadata(&home, owner, home.clone());
                    if let Err(error) = runtime.quick().upsert_thread(&metadata).await {
                        anyhow::ensure!(crate::is_state_db_busy(&error), "{error:#}");
                        runtime.note_state_db_contended();
                    }
                }
                runtime
                    .record_accounting(observe(owner, &next, /*revision*/ 1))
                    .await?;
                waits.push(started.elapsed());
                attempts.push(next);
                // The model answering.
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
            anyhow::ensure!(
                runtime.flush_accounting(Duration::from_secs(60)).await,
                "records still queued"
            );
            runtime.close().await;
            anyhow::Ok((waits, attempts))
        }));
    }
    let mut waits = Vec::new();
    let mut attempts = Vec::new();
    for session in sessions {
        let (session_waits, session_attempts) = session.await??;
        waits.extend(session_waits);
        attempts.extend(session_attempts);
    }
    holder.await??;
    waits.sort();
    let p50 = waits[waits.len() / 2];
    let p99 = waits[waits.len() * 99 / 100];
    let max = waits[waits.len() - 1];
    eprintln!("request waits: p50 {p50:?}, p99 {p99:?}, max {max:?}");
    // One short wait on the lock (`QUICK_BUSY_TIMEOUT`) plus the work of
    // ten sessions sharing four test threads. Windows runners take longer
    // to open and lock SQLite files (p99 1.6-1.9 s on nightly-ci, #636).
    let bound = Duration::from_millis(if cfg!(windows) { 3_000 } else { 1_500 });
    assert!(p99 < bound, "p99 {p99:?}, max {max:?}");
    // With 120 waits, p99 is the second-largest, so a max bound under the
    // p99 bound would override it: Windows keeps its 3 s here too (the 2.5 s
    // bound failed nightly-ci at max 2.50 s and 2.87 s, #691), half its 6 s
    // hold.
    let max_bound = Duration::from_millis(if cfg!(windows) { 3_000 } else { 2_500 });
    assert!(max < max_bound, "p99 {p99:?}, max {max:?}");
    assert_eq!(attempts.len(), SESSIONS * REQUESTS);
    for attempt in &attempts {
        assert_eq!(recorded(&setup, attempt).await?, (1, 1));
    }
    assert_eq!(pending_records(&path), Vec::<PathBuf>::new());
    setup.close().await;
    Ok(())
}

/// The final spool owner releases its lock even if a duplicated descriptor
/// remains open, so another writer can claim its leftover records immediately.
#[cfg(unix)]
#[test]
fn the_final_spool_owner_releases_its_lock_with_a_retained_descriptor() {
    let home = home();
    let spool = Arc::new(Spool::open(&home).expect("open spool"));
    let dir = spool.dir.clone();
    let process = spool.process.clone();
    let retained = spool._lock.try_clone().expect("retain spool descriptor");
    let other_owner = Arc::clone(&spool);

    assert!(claim(&dir, &process).is_none(), "the live spool is held");
    drop(spool);
    assert!(
        claim(&dir, &process).is_none(),
        "another spool owner still holds the lock"
    );
    drop(other_owner);
    let claimed = claim(&dir, &process);
    assert!(
        claimed.is_some(),
        "the final owner's lock remains held through a retained descriptor"
    );
    assert_eq!(
        claimed.expect("claim released spool").path,
        Some(dir.join(format!("{process}.lock")))
    );
    drop(retained);
}

/// #928: a dropped validation lease is free at once, also while a child forked
/// meanwhile still holds a copy of its descriptor. Otherwise the next process
/// finds it held and puts its records off for another `VALIDATION_POLL`.
#[cfg(unix)]
#[test]
fn a_dropped_validation_lease_is_free_while_a_forked_child_holds_its_descriptor() {
    let home = home();
    std::fs::create_dir_all(&*home).expect("home");
    let path = home.join(VALIDATION_LEASE);
    let held = take_lease(&path).expect("open lease").expect("lease free");
    let (release, spawner) =
        crate::runtime::test_support::fork_child_holding_descriptors().expect("fork child");
    assert!(
        take_lease(&path).expect("open lease").is_none(),
        "the lease is held"
    );
    drop(held);
    assert!(
        take_lease(&path).expect("open lease").is_some(),
        "the dropped lease is still held through the forked child's copy"
    );
    drop(release);
    let status = spawner.join().expect("spawner thread").expect("status");
    assert!(status.success(), "{status:?}");
}

/// #928: a dropped claim on a gone process's records is free at once, also
/// while a child forked meanwhile still holds a copy of its descriptor.
#[cfg(unix)]
#[test]
fn a_dropped_claim_is_free_while_a_forked_child_holds_its_descriptor() {
    let home = home();
    std::fs::create_dir_all(&*home).expect("home");
    let process = Uuid::new_v4().to_string();
    std::fs::write(home.join(format!("{process}.lock")), b"").expect("lock file");
    let held = claim(&home, &process).expect("claim free");
    let (release, spawner) =
        crate::runtime::test_support::fork_child_holding_descriptors().expect("fork child");
    assert!(claim(&home, &process).is_none(), "the claim is held");
    drop(held);
    assert!(
        claim(&home, &process).is_some(),
        "the dropped claim is still held through the forked child's copy"
    );
    drop(release);
    let status = spawner.join().expect("spawner thread").expect("status");
    assert!(status.success(), "{status:?}");
}
