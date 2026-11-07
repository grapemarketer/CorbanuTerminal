//! #1491: the watch thread's file watcher is created and dropped again and
//! again (a watch ends when no tree or subscriber uses its home). With
//! notify 8.2.0, dropping it on macOS could crash the process in
//! `FsEventWatcher::stop` (`CFRunLoopWakeUp` on the run loop of an FSEvents
//! thread that had already exited) when the folder was busy.
//!
//! Local reproduction (crashed with notify 8.2.0 within seconds; passes with
//! 8.2.1): `CORBANU_1491_STRESS_SECS=60 CORBANU_1491_STRESS_HOGS=20 cargo test
//! -p codex-core --lib kill_switch_watch::tests::notifications_survive`.

use std::path::Path;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::PoisonError;
use std::sync::atomic::AtomicBool;
use std::sync::atomic::AtomicUsize;
use std::sync::atomic::Ordering;
use std::sync::mpsc;
use std::time::Duration;
use std::time::Instant;

use super::STATE_FILE;
use super::WATCHES;
use super::canonical;
use super::notifications;
use super::subscribe;

fn env_number(name: &str, default: u64) -> u64 {
    std::env::var(name)
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(default)
}

/// Writes files into `home` until `done`, so FSEvents keeps waking the
/// watchers' run-loop threads (that widened the #1491 race).
fn busy_writer(
    home: PathBuf,
    done: Arc<AtomicBool>,
    name: &'static str,
) -> std::thread::JoinHandle<()> {
    std::thread::spawn(move || {
        let mut round = 0u64;
        while !done.load(Ordering::Relaxed) {
            let _ = std::fs::write(
                home.join(format!("{name}-{}", round % 8)),
                round.to_le_bytes(),
            );
            round += 1;
        }
    })
}

/// A dropping thread that is often preempted makes the race far likelier.
fn lower_priority() {
    // SAFETY: only changes the calling thread's scheduling class.
    #[cfg(target_os = "macos")]
    unsafe {
        libc::pthread_set_qos_class_self_np(libc::qos_class_t::QOS_CLASS_BACKGROUND, 0);
    }
}

/// The crash was macOS-only; elsewhere a few droppers keep the cycles
/// covered without nearing Linux's inotify instance limit.
const DROPPERS: usize = if cfg!(target_os = "macos") { 16 } else { 2 };

/// How long past the stress window the droppers get to finish a first cycle;
/// starting an FSEvents watcher can take seconds on a loaded runner (#1553).
const FIRST_CYCLE_GRACE: Duration = Duration::from_secs(60);

#[test]
fn notifications_survive_start_stop_drop_cycles_on_a_busy_home() {
    let secs = env_number("CORBANU_1491_STRESS_SECS", /*default*/ 2);
    let hogs = env_number("CORBANU_1491_STRESS_HOGS", /*default*/ 0);
    let home = tempfile::TempDir::new().unwrap();
    let home = canonical(home.path());
    let done = Arc::new(AtomicBool::new(false));
    // Cycles completed per dropper.
    let cycles: Arc<Vec<AtomicUsize>> =
        Arc::new((0..DROPPERS).map(|_| AtomicUsize::new(0)).collect());
    let mut threads = Vec::new();
    for writer in ["a", "b"] {
        threads.push(busy_writer(home.clone(), Arc::clone(&done), writer));
    }
    for _ in 0..hogs {
        let done = Arc::clone(&done);
        threads.push(std::thread::spawn(move || {
            let mut x = 0u64;
            while !done.load(Ordering::Relaxed) {
                x = std::hint::black_box(x.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1));
            }
        }));
    }
    for dropper in 0..DROPPERS {
        let (home, done, cycles) = (home.clone(), Arc::clone(&done), Arc::clone(&cycles));
        threads.push(std::thread::spawn(move || {
            lower_priority();
            while !done.load(Ordering::Relaxed) {
                let (tx, _rx) = mpsc::channel();
                let watcher = notifications(&home, tx).expect("watch the home");
                std::thread::sleep(Duration::from_millis(2));
                drop(watcher);
                cycles[dropper].fetch_add(1, Ordering::Relaxed);
            }
        }));
    }
    // Stress for the full window, then until every dropper has dropped a
    // watcher at least once (or the grace runs out).
    let window = Duration::from_secs(secs);
    std::thread::sleep(window);
    let deadline = Instant::now() + FIRST_CYCLE_GRACE;
    while cycles
        .iter()
        .any(|count| count.load(Ordering::Relaxed) == 0)
        && Instant::now() < deadline
    {
        std::thread::sleep(Duration::from_millis(20));
    }
    done.store(true, Ordering::Relaxed);
    for thread in threads {
        thread.join().unwrap();
    }
    let total: usize = cycles
        .iter()
        .map(|count| count.load(Ordering::Relaxed))
        .sum();
    assert!(
        total > 0,
        "no watcher was dropped within {window:?} plus {FIRST_CYCLE_GRACE:?}"
    );

    // The notifications still arrive: a replaced state file wakes the watch.
    let (tx, rx) = mpsc::channel();
    let _watcher = notifications(&home, tx).expect("watch the home");
    let staged = home.join("security_state.json.tmp");
    let deadline = Instant::now() + Duration::from_secs(10);
    let mut seen = false;
    while !seen && Instant::now() < deadline {
        std::fs::write(&staged, b"{}").unwrap();
        std::fs::rename(&staged, home.join(STATE_FILE)).unwrap();
        seen = rx.recv_timeout(Duration::from_millis(500)).is_ok();
    }
    assert!(seen, "no notification for the replaced state file");
}

fn watched(home: &Path) -> bool {
    WATCHES
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .contains_key(&canonical(home))
}

/// The watch threads themselves start, end (dropping their watcher) and
/// start again on busy homes.
#[test]
fn kill_switch_watch_threads_start_and_end_repeatedly_on_busy_homes() {
    let homes: Vec<tempfile::TempDir> = (0..8).map(|_| tempfile::TempDir::new().unwrap()).collect();
    let done = Arc::new(AtomicBool::new(false));
    let writers: Vec<_> = homes
        .iter()
        .map(|home| busy_writer(canonical(home.path()), Arc::clone(&done), "busy"))
        .collect();
    for _ in 0..3 {
        let subscribers: Vec<_> = homes.iter().map(|home| subscribe(home.path())).collect();
        assert!(homes.iter().all(|home| watched(home.path())));
        drop(subscribers);
        let deadline = Instant::now() + super::POLL * 5;
        while homes.iter().any(|home| watched(home.path())) {
            assert!(Instant::now() < deadline, "a watch did not end");
            std::thread::sleep(Duration::from_millis(20));
        }
    }
    done.store(true, Ordering::Relaxed);
    for writer in writers {
        writer.join().unwrap();
    }
}
