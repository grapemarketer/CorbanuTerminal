//! #1506 item 2 (D262): `.codex` and `.agents` folders at the top of a
//! writable root that were absent when a non-admin (legacy) Windows sandbox
//! session started and existed when it ended (or while it still runs), and
//! (D283, #1541) those deeper in a writable root that weren't there, as the
//! same folder, when the session started. A
//! sandboxed command may have planted them, so Corbanu Terminal doesn't load
//! them (project config, hooks, skills) until the user confirms them. The
//! records live with the sandbox's integrity-label records, where sandboxed
//! commands can't read or change them.

use std::path::Path;
use std::path::PathBuf;

/// Start of the warning, and of the disabled reason, for a project folder
/// that isn't loaded because it may have been planted.
pub const PLANTED_PROJECT_METADATA_WARNING_PREFIX: &str =
    "Possibly planted project folder not loaded: ";

/// D332 (#1682): start of the warning for a writable folder's top-level
/// `.git` that appeared or changed while a non-admin sandbox command ran.
pub const PLANTED_GIT_WARNING_PREFIX: &str = "Possibly planted git folder: ";

/// Whether `path` is a `.git` entry (D332's records hold only top-level
/// ones).
pub fn is_git_dir(path: &Path) -> bool {
    path.file_name()
        .is_some_and(|name| name.eq_ignore_ascii_case(".git"))
}

/// The planted folders not confirmed yet.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PlantedProjectMetadata {
    /// Canonical paths (the folder itself never resolved through a link).
    Known(Vec<PathBuf>),
    /// The records can't be read, so every project folder counts as planted.
    Unreadable(String),
}

impl PlantedProjectMetadata {
    /// Whether `folder` (a `.codex` or `.agents` folder) counts as planted:
    /// compared with its parent canonicalized but the folder itself not
    /// followed, as the sandbox records it (a planted junction stays
    /// planted), and, on Windows, ignoring case.
    ///
    /// D283 (#1541): a recorded folder that isn't a `.codex`/`.agents` one
    /// (the sandbox couldn't list it) covers everything below it. On
    /// Windows, also asks the records again now: one deeper in the writable
    /// root of a non-admin sandbox session that still runs counts unless it
    /// was there, as the same folder, when that session started, and a
    /// session that ended since this list was read may have recorded it.
    pub fn contains(&self, folder: &Path) -> bool {
        match self {
            Self::Unreadable(_) => true,
            Self::Known(planted) => {
                let folder = without_following(folder);
                planted.iter().any(|entry| is_at_or_below(&folder, entry))
                    || counts_as_planted_now(&folder)
            }
        }
    }

    /// D332: the warning for `git_dir` (a folder's `.git`) if it counts as
    /// planted: nothing stops git from running its hooks and config, so the
    /// person is told instead. Records that can't be read add none: the
    /// project folder warning already says so.
    pub fn git_warning(&self, git_dir: &Path) -> Option<String> {
        match self {
            Self::Unreadable(_) => None,
            Self::Known(_) => self.contains(git_dir).then(|| {
                format!(
                    "{PLANTED_GIT_WARNING_PREFIX}{}. It appeared or changed while a non-admin Windows sandbox command ran, so a command may have planted it. Git runs its hooks and config outside the sandbox whenever you run git in this folder. Check its hooks and config, then open Corbanu Terminal in this folder and confirm it.",
                    git_dir.display()
                )
            }),
        }
    }

    /// Why `folder` isn't loaded, and how to load it.
    pub fn reason(&self, folder: &Path) -> String {
        match self {
            Self::Known(_) => format!(
                "{PLANTED_PROJECT_METADATA_WARNING_PREFIX}{}. It appeared while a non-admin Windows sandbox command ran, so a command may have planted it; its config, hooks and skills are ignored. Check its contents, then open Corbanu Terminal in this folder and choose to load it.",
                folder.display()
            ),
            Self::Unreadable(err) => format!(
                "{PLANTED_PROJECT_METADATA_WARNING_PREFIX}{}. The records of project folders that non-admin Windows sandbox commands may have planted can't be read ({err}), so no project config, hooks or skills are loaded until they can.",
                folder.display()
            ),
        }
    }
}

/// The planted folders not confirmed yet. Always empty off Windows.
pub fn planted_project_metadata() -> PlantedProjectMetadata {
    #[cfg(target_os = "windows")]
    {
        match codex_windows_sandbox::planted_project_metadata() {
            Ok(planted) => PlantedProjectMetadata::Known(planted),
            Err(err) => {
                tracing::warn!("could not read the planted project metadata records: {err:#}");
                PlantedProjectMetadata::Unreadable(format!("{err:#}"))
            }
        }
    }
    #[cfg(not(target_os = "windows"))]
    {
        PlantedProjectMetadata::Known(Vec::new())
    }
}

/// D283: see [`PlantedProjectMetadata::contains`]. Records that can't be
/// read count as planted.
fn counts_as_planted_now(folder: &Path) -> bool {
    #[cfg(target_os = "windows")]
    {
        codex_windows_sandbox::project_metadata_counts_as_planted(folder).unwrap_or_else(|err| {
            tracing::warn!("could not read the planted project metadata records: {err:#}");
            true
        })
    }
    #[cfg(not(target_os = "windows"))]
    {
        let _ = folder;
        false
    }
}

/// `path` with its parent canonicalized and its last component kept as is.
fn without_following(path: &Path) -> PathBuf {
    match (path.parent(), path.file_name()) {
        (Some(parent), Some(name)) => dunce::canonicalize(parent)
            .unwrap_or_else(|_| parent.to_path_buf())
            .join(name),
        _ => path.to_path_buf(),
    }
}

/// `path` is `base` or below it (on Windows, ignoring case).
fn is_at_or_below(path: &Path, base: &Path) -> bool {
    let mut parts = path.components();
    base.components().all(|part| {
        parts.next().is_some_and(|other| {
            same_path(Path::new(other.as_os_str()), Path::new(part.as_os_str()))
        })
    })
}

fn same_path(a: &Path, b: &Path) -> bool {
    if cfg!(windows) {
        a.as_os_str()
            .to_string_lossy()
            .eq_ignore_ascii_case(&b.as_os_str().to_string_lossy())
    } else {
        a == b
    }
}

#[cfg(test)]
#[path = "planted_project_metadata_tests.rs"]
mod tests;
