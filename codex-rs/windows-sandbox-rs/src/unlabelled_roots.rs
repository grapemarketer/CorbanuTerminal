//! #734: writable roots the legacy (unelevated) sandbox couldn't label Low
//! (#158). Such a root stays as it is, so sandboxed commands can't write
//! there; the reason was only in the sandbox log, so the user saw a plain
//! "access denied". Core takes a [`unlabelled_roots_mark`] before a run,
//! asks for the roots that failed during it ([`unlabelled_roots_since`])
//! among the run's own ([`legacy_writable_roots`]), and warns once per
//! thread. Display only: it never changes how commands run.

use std::collections::BTreeMap;
use std::path::Path;
use std::path::PathBuf;
use std::sync::LazyLock;
use std::sync::Mutex;
use std::sync::MutexGuard;

use codex_protocol::permissions::FileSystemSandboxPolicy;
use codex_utils_absolute_path::AbsolutePathBuf;

#[derive(Default)]
struct Failures {
    /// Counts failed labellings in this process.
    seq: u64,
    /// Each root whose latest labelling failed, with that failure's number.
    latest: BTreeMap<PathBuf, u64>,
}

static FAILURES: LazyLock<Mutex<Failures>> = LazyLock::new(Default::default);

fn failures() -> MutexGuard<'static, Failures> {
    FAILURES
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// Records whether `root` (canonical) could be labelled Low just now: a root
/// fixed since (its owner changed, say) stops being reported.
#[cfg_attr(not(target_os = "windows"), allow(dead_code))]
pub(crate) fn record(root: &Path, labelled: bool) {
    let mut failures = failures();
    if labelled {
        failures.latest.remove(root);
    } else {
        failures.seq += 1;
        let seq = failures.seq;
        failures.latest.insert(root.to_path_buf(), seq);
    }
}

/// Marks now, for [`unlabelled_roots_since`].
pub fn unlabelled_roots_mark() -> u64 {
    failures().seq
}

/// The canonical roots whose labelling failed after `mark` and hasn't
/// succeeded since (always none off Windows).
pub fn unlabelled_roots_since(mark: u64) -> Vec<PathBuf> {
    failures()
        .latest
        .iter()
        .filter(|(_, seq)| **seq > mark)
        .map(|(root, _)| root.clone())
        .collect()
}

/// The canonical writable roots the legacy sandbox labels for a run under
/// `policy` in `cwd` (`%TEMP%`/`%TMP%` as this process has them); empty off
/// Windows.
#[cfg(target_os = "windows")]
pub fn legacy_writable_roots(
    policy: &FileSystemSandboxPolicy,
    workspace_roots: &[AbsolutePathBuf],
    cwd: &Path,
) -> Vec<PathBuf> {
    let permissions =
        crate::resolved_permissions::ResolvedWindowsSandboxPermissions::from_file_system_policy(
            policy.clone(),
            workspace_roots,
        );
    crate::allow::compute_allow_paths_for_permissions(
        &permissions,
        cwd,
        &std::collections::HashMap::new(),
    )
    .allow
    .into_iter()
    .collect()
}

/// The canonical writable roots the legacy sandbox labels; empty off Windows.
#[cfg(not(target_os = "windows"))]
pub fn legacy_writable_roots(
    _policy: &FileSystemSandboxPolicy,
    _workspace_roots: &[AbsolutePathBuf],
    _cwd: &Path,
) -> Vec<PathBuf> {
    Vec::new()
}

/// What to tell the person about such a `root`.
pub fn unlabelled_writable_root_warning(root: &Path) -> String {
    format!(
        "`{}` is read-only to commands in the non-admin Windows sandbox, although it is a \
         writable folder: that sandbox runs commands at Low integrity, and the folder could not \
         be labelled Low. Labelling needs permission to change the folder's owner settings \
         (which owning it gives) on a file system that holds integrity labels, such as NTFS. To \
         let commands write there, own the folder, or switch to the default sandbox by running \
         /setup-default-sandbox, or set `sandbox = \"elevated\"` under `[windows]` in config.toml.",
        root.display()
    )
}

#[cfg(test)]
#[path = "unlabelled_roots_tests.rs"]
mod tests;
