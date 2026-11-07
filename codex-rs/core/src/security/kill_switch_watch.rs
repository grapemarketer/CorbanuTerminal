//! #507 (Travis, 2026-10-12): the kill switch reaches every Corbanu Terminal
//! process running on the same Corbanu home, not only the one it was turned
//! on in, and not only at their next start.
//!
//! Each process watches its homes' `security_state.json` (one thread per
//! home, while a policy tree of this process or a subscriber uses it). Every
//! confirmed transition replaces the file atomically, so a change is seen by
//! a file notification (`notify`: FSEvents, inotify, ReadDirectoryChangesW)
//! at once, and at the latest by the next poll ([`POLL`]) when notifications
//! are lost or unavailable. The small file is read on each wake-up and parsed
//! only when its bytes changed; nothing spins.
//!
//! When the stored kill-switch event differs from the one a policy tree of
//! this process holds, the stored revocations merge into the tree (the later
//! kill-switch event wins, as between trees of one process). That also brings
//! the revocations saved with it (revoked grants and the like) and, like any
//! adopted change, ends the tree's grants and open confirmations. Turned on,
//! the tree's sinks stop its running work (#522, `kill_switch_stop`); turned
//! off, its turns are allowed again, but only in a tree whose switch the
//! other process had seen (a release never ends a switch it did not know,
//! e.g. one this process could not save). #842 (Travis, 2026-10-15): Moderate
//! confirmed in another process reaches this one too: a tree below it rises
//! to Moderate (Core's rules apply now, its grants and "for session"
//! approvals end), as the other sessions of the confirming process do; the
//! launch-time controls (the credential broker) wait for the next start, as
//! there. #919 (Travis, 2026-10-16, D20): so does Aggressive: a tree below it
//! rises to Aggressive, and the sessions of this process take approval for
//! every command (#505), as the other sessions of the confirming process do;
//! Aggressive's launch-time controls (its sandbox profile, network, the
//! environment rules, the broker) wait for the next start, as there. A level
//! is only ever raised while running: a lower one saved elsewhere never
//! applies before a restart. An unreadable file changes nothing while
//! running (a transient read error must not flip the switch); the next start
//! enforces it as before.
//!
//! Limits: latency is within [`POLL`] (1 s) of the save, usually at once,
//! plus the stop itself ("within about a second" in the user-facing text);
//! a switch turned on and off again within one wake-up is seen as off; the
//! file has no integrity check of its own (as at start), so whoever can write
//! the Corbanu home can also turn a running switch off (agent commands cannot
//! under Moderate and Aggressive, and none run while the switch is on).

use std::collections::HashMap;
use std::collections::HashSet;
use std::path::Path;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::LazyLock;
use std::sync::Mutex;
use std::sync::PoisonError;
use std::sync::mpsc;
use std::time::Duration;

use codex_security_policy::SecurityLevel;
use notify::RecursiveMode;
use notify::Watcher;

use super::COMMITS;
use super::SharedEffectivePolicy;
use super::TREES;
use super::adopt;
use super::canonical;
use super::notify as notify_sinks;
use crate::security::recovery::STATE_FILE;

/// The longest a change of the saved state goes unseen when no file
/// notification arrives.
pub(crate) const POLL: Duration = Duration::from_secs(1);

/// The watched homes of this process, by canonical path.
static WATCHES: LazyLock<Mutex<HashMap<PathBuf, Arc<HomeWatch>>>> = LazyLock::new(Default::default);

/// Kill-switch events this process saved itself: subscribers learn of them
/// from the confirming screen, not as a change made elsewhere.
static SAVED_HERE: LazyLock<Mutex<HashSet<String>>> = LazyLock::new(Default::default);

struct HomeWatch {
    /// Whether the kill switch is on for the home, for subscribers (the TUI
    /// stops and resumes Claude panes with it). They are told only of a
    /// change another process made.
    on: tokio::sync::watch::Sender<bool>,
    /// #842/#919: the level another process confirmed that this process's
    /// trees rose to, for subscribers (the TUI says so); signalled per rise.
    raised: tokio::sync::watch::Sender<Option<SecurityLevel>>,
}

impl HomeWatch {
    fn subscribed(&self) -> bool {
        self.on.receiver_count() > 0 || self.raised.receiver_count() > 0
    }
}

/// Start watching `home` unless this process already does.
pub(super) fn ensure(home: &Path) {
    // Unit tests build many trees on temporary homes and model "another
    // process" by writing the file; they start a watch explicitly.
    #[cfg(not(test))]
    watch(home, |_| {});
    #[cfg(test)]
    let _ = home;
}

/// #507: whether the kill switch is on for `home`, updated while this
/// process runs; a change another process on the home made is signalled.
pub(crate) fn subscribe(home: &Path) -> tokio::sync::watch::Receiver<bool> {
    watch(home, |watch| watch.on.subscribe())
}

/// #842/#919: signalled when Moderate or Aggressive confirmed in another
/// process on `home` was taken by this process's sessions (Core's level rose
/// to it here).
pub(crate) fn subscribe_level_raised(
    home: &Path,
) -> tokio::sync::watch::Receiver<Option<SecurityLevel>> {
    watch(home, |watch| watch.raised.subscribe())
}

/// A kill-switch event this process saved (see [`SAVED_HERE`]).
pub(super) fn saved_here(event_id: &str) {
    SAVED_HERE
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .insert(event_id.to_string());
}

/// Watch `home` (once per process); `with` runs under the lock, so a
/// subscriber is counted before the thread can decide to end.
fn watch<T>(home: &Path, with: impl FnOnce(&HomeWatch) -> T) -> T {
    let home = canonical(home);
    let mut watches = WATCHES.lock().unwrap_or_else(PoisonError::into_inner);
    if let Some(watch) = watches.get(&home) {
        return with(watch);
    }
    let (on, _) =
        tokio::sync::watch::channel(crate::security::revocation_change::kill_switch_on(&home));
    let (raised, _) = tokio::sync::watch::channel(/*init*/ None);
    let watch = Arc::new(HomeWatch { on, raised });
    let result = with(&watch);
    let thread_watch = Arc::clone(&watch);
    let thread_home = home.clone();
    let spawned = std::thread::Builder::new()
        .name("corbanu-kill-switch-watch".to_string())
        .spawn(move || run(&thread_home, &thread_watch));
    match spawned {
        Ok(_) => {
            watches.insert(home, watch);
        }
        Err(error) => tracing::warn!(
            target: "codex_core::security::transition",
            %error,
            "cannot watch the security state for a kill switch from another process"
        ),
    }
    result
}

fn run(home: &Path, watch: &Arc<HomeWatch>) {
    let (tx, rx) = mpsc::channel::<()>();
    let _watcher = match notifications(home, tx) {
        Ok(watcher) => Some(watcher),
        Err(error) => {
            tracing::warn!(
                target: "codex_core::security::transition",
                %error,
                home = %home.display(),
                "no file notifications for the security state; polling it"
            );
            None
        }
    };
    // Nothing is taken as seen: the first pass compares the file with the
    // trees at once, so a save made while the watch started is not missed.
    let mut last: Option<Option<Vec<u8>>> = None;
    loop {
        {
            let mut watches = WATCHES.lock().unwrap_or_else(PoisonError::into_inner);
            if trees_on(home).is_empty() && !watch.subscribed() {
                watches.remove(home);
                return;
            }
        }
        let contents = std::fs::read(home.join(STATE_FILE)).ok();
        if last.as_ref() != Some(&contents) {
            last = Some(contents);
            let raised = apply_saved(home).raised;
            watch.on.send_if_modified(|on| {
                let now = crate::security::revocation_change::kill_switch_on(home);
                std::mem::replace(on, now) != now && !changed_here(home)
            });
            match raised {
                Some(SecurityLevel::Aggressive) => approve_every_command(home, watch),
                Some(level) => {
                    watch.raised.send_replace(Some(level));
                }
                None => {}
            }
        }
        match rx.recv_timeout(POLL) {
            Ok(()) | Err(mpsc::RecvTimeoutError::Timeout) => {}
            // No watcher: poll only (never spin on a closed channel).
            Err(mpsc::RecvTimeoutError::Disconnected) => std::thread::sleep(POLL),
        }
        while rx.try_recv().is_ok() {}
    }
}

/// File notifications for `home`'s saved state, sent to `tx`. The file is
/// replaced by a rename, so its folder is watched. #1491: the watcher is
/// dropped when the watch ends; notify 8.2.1 or later is required there, as
/// 8.2.0's macOS backend could use the FSEvents thread's run loop after that
/// thread had exited (SIGTRAP/SIGSEGV in `FsEventWatcher::stop`).
fn notifications(home: &Path, tx: mpsc::Sender<()>) -> notify::Result<notify::RecommendedWatcher> {
    let mut watcher = notify::recommended_watcher(move |event: notify::Result<notify::Event>| {
        let relevant = match &event {
            Ok(event) => {
                event.need_rescan()
                    || event
                        .paths
                        .iter()
                        .any(|path| path.file_name().is_some_and(|name| name == STATE_FILE))
            }
            Err(_) => true,
        };
        if relevant {
            let _ = tx.send(());
        }
    })?;
    watcher.watch(home, RecursiveMode::NonRecursive)?;
    Ok(watcher)
}

/// Whether the kill-switch event saved for `home` was made by this process.
fn changed_here(home: &Path) -> bool {
    let Ok(Some(stored)) = crate::security::recovery::load(home) else {
        return false;
    };
    stored.revocations.kill_switch_event_id().is_some_and(|id| {
        SAVED_HERE
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .contains(id.as_str())
    })
}

fn trees_on(home: &Path) -> Vec<Arc<SharedEffectivePolicy>> {
    let mut trees = TREES.lock().unwrap_or_else(PoisonError::into_inner);
    trees.retain(|(_, tree)| tree.strong_count() > 0);
    trees
        .iter()
        .filter(|(tree_home, _)| tree_home == home)
        .filter_map(|(_, tree)| tree.upgrade())
        .collect()
}

/// What the saved state brought to this process's trees.
#[derive(Debug, Default, PartialEq, Eq)]
pub(super) struct Applied {
    /// Trees that took a kill-switch change.
    pub(super) reached: usize,
    /// #842/#919: the level trees rose to, if any did.
    pub(super) raised: Option<SecurityLevel>,
}

/// The saved kill switch reaches this process's trees on `home`. Returns how
/// many took a change.
pub(super) fn apply(home: &Path) -> usize {
    apply_saved(home).reached
}

/// #919: after this process's trees rose to Aggressive saved by another
/// process, its sessions on `home` take approval for every command from
/// their next turn (#505), and sessions it starts later start with it; then
/// subscribers are told of the rise. Off the watch thread, which keeps
/// reading the saved state (a kill switch saved meanwhile is not held up by
/// `confirmed_approvals`' wait); on it only if no thread can start.
fn approve_every_command(home: &Path, watch: &Arc<HomeWatch>) {
    fn approve(home: &Path, watch: &HomeWatch) {
        let approvals = crate::security::confirmed_approvals::confirm(home);
        tracing::info!(
            target: "codex_core::security::transition",
            held = approvals.held,
            kept = approvals.kept.len(),
            pending = approvals.pending,
            "Aggressive confirmed by another process: approval for every command"
        );
        watch.raised.send_replace(Some(SecurityLevel::Aggressive));
    }
    let thread_home = home.to_path_buf();
    let thread_watch = Arc::clone(watch);
    let spawned = std::thread::Builder::new()
        .name("corbanu-aggressive-approvals".to_string())
        .spawn(move || approve(&thread_home, &thread_watch));
    if let Err(error) = spawned {
        tracing::warn!(
            target: "codex_core::security::transition",
            %error,
            "approval for every command on the watch thread"
        );
        approve(home, watch);
    }
}

/// The saved kill switch and a Moderate (#842) or Aggressive (#919)
/// confirmed elsewhere reach this process's trees on `home`.
pub(super) fn apply_saved(home: &Path) -> Applied {
    let home = canonical(home);
    // Never between a commit of this process saving the file and applying
    // it: the commit's own trees take it then, under its epoch.
    let _commit = COMMITS.lock().unwrap_or_else(PoisonError::into_inner);
    let Ok(Some(stored)) = crate::security::recovery::load(&home) else {
        return Applied::default();
    };
    let mut reached = 0;
    for tree in trees_on(&home) {
        let turned_on = {
            let mut guard = tree.state.write().unwrap_or_else(PoisonError::into_inner);
            let Some(state) = guard.as_mut() else {
                continue;
            };
            let held = state.persisted.revocations.kill_switch_event_id().cloned();
            if held.as_ref() == stored.revocations.kill_switch_event_id() {
                continue;
            }
            // Off here and off there (turned on and off within one wake-up):
            // nothing to stop, and nothing to end.
            if !stored.revocations.kill_switch_active
                && !state.persisted.revocations.kill_switch_active
            {
                continue;
            }
            // A release only ends the switch the releasing process had seen.
            if !stored.revocations.kill_switch_active
                && state.persisted.revocations.kill_switch_active
                && held
                    .as_ref()
                    .is_some_and(|held| !stored.revocations.has_applied(held))
            {
                continue;
            }
            let mut revocations = state.persisted.revocations.clone();
            if let Err(error) = revocations.merge(&stored.revocations) {
                tracing::warn!(
                    target: "codex_core::security::transition",
                    %error,
                    "cannot take the kill switch saved by another process"
                );
                continue;
            }
            // This tree's switch is the later one: nothing to take.
            if revocations.kill_switch_event_id() == held.as_ref() {
                continue;
            }
            let level = state.persisted.settings.level;
            if let Err(error) = adopt(state, level, revocations) {
                tracing::warn!(
                    target: "codex_core::security::transition",
                    %error,
                    "cannot take the kill switch saved by another process"
                );
                continue;
            }
            reached += 1;
            state.persisted.revocations.kill_switch_active
        };
        tracing::info!(
            target: "codex_core::security::transition",
            kill_switch_active = turned_on,
            "kill switch changed by another process on this home"
        );
        // Its sessions stop their work and the broker's channels close.
        if turned_on {
            notify_sinks(&tree);
        }
    }
    Applied {
        reached,
        raised: raise_to_saved(&home, &stored),
    }
}

/// #842/#919: Moderate or Aggressive saved by another process raises this
/// process's trees on `home` that are below it, as a confirmation raises the
/// other sessions of its own process: Core's level, so its rules apply now;
/// the epoch moves, so grants and "for session" approvals end; and the sinks
/// revoke. Never lowers a tree: Permissive saved elsewhere, or a level below
/// a tree's own, changes nothing here. Only the level is taken here (the
/// revocations stay with the kill-switch rules above). Returns the level
/// when a tree rose.
fn raise_to_saved(
    home: &Path,
    stored: &crate::security::recovery::DurableSecurityState,
) -> Option<SecurityLevel> {
    let saved = stored.level;
    if saved == SecurityLevel::Permissive {
        return None;
    }
    let mut raised = None;
    for tree in trees_on(home) {
        let rose = {
            let mut guard = tree.state.write().unwrap_or_else(PoisonError::into_inner);
            let Some(state) = guard.as_mut() else {
                continue;
            };
            if state.persisted.settings.level >= saved {
                // Its next start reads the saved level at least (a restart of
                // a window waiting for a downgrade starts at the saved level).
                state.next_start_level = state.next_start_level.max(saved);
                continue;
            }
            let revocations = state.persisted.revocations.clone();
            if let Err(error) = adopt(state, saved, revocations) {
                tracing::warn!(
                    target: "codex_core::security::transition",
                    %error,
                    level = %saved,
                    "cannot take a level confirmed by another process"
                );
                continue;
            }
            state.next_start_level = state.next_start_level.max(saved);
            true
        };
        if rose {
            tracing::info!(
                target: "codex_core::security::transition",
                level = %saved,
                "a level confirmed by another process on this home applies here now"
            );
            notify_sinks(&tree);
            raised = Some(saved);
        }
    }
    raised
}

#[cfg(test)]
pub(super) fn start_for_test(home: &Path) -> tokio::sync::watch::Receiver<bool> {
    subscribe(home)
}

#[cfg(test)]
#[path = "kill_switch_watch_tests.rs"]
mod tests;
