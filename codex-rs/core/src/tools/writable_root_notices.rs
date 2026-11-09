//! Issue #535: explain, where it bites, a writable root that is left out
//! because it is a link into another root's read-only path: at session start
//! for the configured roots, and before a sandboxed run for the roots its
//! additional permissions (a permission request) add. Each link is explained
//! once per thread.
//!
//! Issue #734: likewise, after a run in the non-admin Windows sandbox, a
//! writable root it couldn't label Low, so left read-only to its commands.

use std::collections::HashMap;
use std::collections::HashSet;
use std::path::Path;
use std::path::PathBuf;
use std::sync::LazyLock;
use std::sync::Mutex;

use codex_protocol::ThreadId;
use codex_protocol::permissions::FileSystemSandboxPolicy;
use codex_protocol::permissions::WritableLinkIntoReadOnlyPath;

static EXPLAINED: LazyLock<Mutex<HashSet<(ThreadId, PathBuf)>>> =
    LazyLock::new(|| Mutex::new(HashSet::new()));

/// The warnings for the links in `policy` this thread has not explained yet.
pub(crate) fn excluded_writable_link_warnings(
    thread_id: ThreadId,
    policy: &FileSystemSandboxPolicy,
    cwd: &Path,
) -> Vec<String> {
    let exclusions = policy.writable_links_into_read_only_paths(cwd);
    if exclusions.is_empty() {
        return Vec::new();
    }
    let mut explained = EXPLAINED
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    exclusions
        .into_iter()
        .filter(|exclusion| explained.insert((thread_id, exclusion.link.to_path_buf())))
        .map(|exclusion| excluded_writable_link_warning(&exclusion))
        .collect()
}

pub(crate) fn excluded_writable_link_warning(exclusion: &WritableLinkIntoReadOnlyPath) -> String {
    format!(
        "Write access to `{}` is not granted: it is, or passes through, a link into `{}`, a \
         path another writable folder protects. Name the folder by its real path if it should \
         be writable.",
        exclusion.link.display(),
        exclusion.read_only_path.display(),
    )
}

static WARNED_UNLABELLED: LazyLock<Mutex<HashSet<(ThreadId, PathBuf)>>> =
    LazyLock::new(|| Mutex::new(HashSet::new()));

/// #734: the warnings for those of `failed` (canonical roots the non-admin
/// Windows sandbox couldn't label Low during a run) that are among the run's
/// own `writable` roots (canonical, as the sandbox labels them) and this
/// thread has not been warned about yet.
pub(crate) fn unlabelled_writable_root_warnings(
    thread_id: ThreadId,
    failed: &[PathBuf],
    writable: &[PathBuf],
) -> Vec<String> {
    let mut warned = WARNED_UNLABELLED
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    failed
        .iter()
        .filter(|root| writable.contains(*root) && warned.insert((thread_id, (*root).clone())))
        .map(|root| codex_windows_sandbox::unlabelled_writable_root_warning(root))
        .collect()
}

/// D424 (#1775): per thread, the planted-records mark taken before its first
/// non-admin sandboxed run, and the records it was told about since.
static PLANTED_TOLD: LazyLock<Mutex<HashMap<ThreadId, PlantedTold>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

/// The mark before a thread's first run, and the records it was told about.
type PlantedTold = (u64, HashSet<u64>);

/// D424 (#1775): the notice for the folders recorded as possibly planted
/// since `mark` (taken before a run in either Windows sandbox), or
/// since the thread's first such run if that is earlier (a command it left
/// running may have ended meanwhile), that lie at or below one of the run's
/// own `writable` roots (canonical). Each record is told once per thread;
/// one in a root this run doesn't have waits for a run that has it.
pub(crate) fn planted_after_run_notice(
    thread_id: ThreadId,
    mark: u64,
    writable: &[PathBuf],
) -> Option<String> {
    planted_notice_from(
        thread_id,
        mark,
        writable,
        codex_windows_sandbox::planted_records_since,
    )
}

/// Whether any record this thread wasn't told about lies after its first
/// mark (or `mark`), so [`planted_after_run_notice`] is worth asking (it
/// needs the run's writable roots, which cost a look at each). Notes the
/// thread's first mark.
pub(crate) fn planted_notice_pending(thread_id: ThreadId, mark: u64) -> bool {
    let mut told = PLANTED_TOLD
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let (first, told) = told
        .entry(thread_id)
        .or_insert_with(|| (mark, HashSet::new()));
    codex_windows_sandbox::planted_records_since((*first).min(mark))
        .iter()
        .any(|record| !told.contains(&record.seq))
}

/// [`planted_after_run_notice`] with the planted-records log passed in.
fn planted_notice_from(
    thread_id: ThreadId,
    mark: u64,
    writable: &[PathBuf],
    records_since: impl FnOnce(u64) -> Vec<codex_windows_sandbox::PlantedRecord>,
) -> Option<String> {
    let mut told = PLANTED_TOLD
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let (first, told) = told
        .entry(thread_id)
        .or_insert_with(|| (mark, HashSet::new()));
    let mut paths: Vec<PathBuf> = Vec::new();
    let mut git_changes: Vec<codex_config::PlantedGitChange> = Vec::new();
    for record in records_since((*first).min(mark)) {
        if !writable
            .iter()
            .any(|root| is_at_or_below(&record.path, root))
            || !told.insert(record.seq)
        {
            continue;
        }
        if record.git_changes.is_empty() {
            if !paths.contains(&record.path) {
                paths.push(record.path);
            }
        } else if let Some(change) = git_changes
            .iter_mut()
            .find(|change| change.path == record.path)
        {
            for what in record.git_changes {
                if !change.changes.contains(&what) {
                    change.changes.push(what);
                }
            }
        } else {
            git_changes.push(codex_config::PlantedGitChange {
                path: record.path,
                changes: record.git_changes,
            });
        }
    }
    codex_config::planted_after_command_notice(&paths, &git_changes)
}

/// `path` is `base` or below it, ignoring ASCII case (Windows paths).
fn is_at_or_below(path: &Path, base: &Path) -> bool {
    let mut parts = path.components();
    base.components().all(|part| {
        parts.next().is_some_and(|other| {
            other
                .as_os_str()
                .to_string_lossy()
                .eq_ignore_ascii_case(&part.as_os_str().to_string_lossy())
        })
    })
}

// The link test needs symlinks (unix).
#[cfg(all(test, unix))]
#[path = "writable_root_notices_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "writable_root_notices_unlabelled_tests.rs"]
mod unlabelled_tests;

#[cfg(test)]
#[path = "writable_root_notices_planted_tests.rs"]
mod planted_tests;
