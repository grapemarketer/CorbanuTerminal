//! Issue #535: explain, where it bites, a writable root that is left out
//! because it is a link into another root's read-only path: at session start
//! for the configured roots, and before a sandboxed run for the roots its
//! additional permissions (a permission request) add. Each link is explained
//! once per thread.
//!
//! Issue #734: likewise, after a run in the non-admin Windows sandbox, a
//! writable root it couldn't label Low, so left read-only to its commands.

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

// The link test needs symlinks (unix).
#[cfg(all(test, unix))]
#[path = "writable_root_notices_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "writable_root_notices_unlabelled_tests.rs"]
mod unlabelled_tests;
