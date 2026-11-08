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
    /// person is told instead. Records that can't be read count every `.git`
    /// as planted, as they do every project folder.
    pub fn git_warning(&self, git_dir: &Path) -> Option<String> {
        match self {
            Self::Unreadable(err) => Some(format!(
                "{PLANTED_GIT_WARNING_PREFIX}{}. The records of folders that non-admin Windows sandbox commands may have planted can't be read ({err}), so Corbanu Terminal can't tell whether a command planted it. Git runs its hooks and config outside the sandbox whenever you run git in this folder; check them first.",
                without_following(git_dir).display()
            )),
            Self::Known(_) => self.contains(git_dir).then(|| {
                format!(
                    "{PLANTED_GIT_WARNING_PREFIX}{}. It appeared or changed while a non-admin Windows sandbox command ran, so a command may have planted it. Git runs its hooks and config outside the sandbox whenever you run git in this folder. Check its hooks and config, then open Corbanu Terminal in this folder and confirm it.",
                    without_following(git_dir).display()
                )
            }),
        }
    }

    /// D424 (#1775): the warnings for the `.git` entries to show at startup
    /// beyond those between the project root and `cwd` (which the config
    /// loader warns about): git runs anywhere, so also those at or below one
    /// of `writable_roots` (canonical: the folders non-admin sandbox commands
    /// may write to, `%TEMP%` included) or below `cwd`. When the records
    /// can't be read, the `.git` at the top of each writable root that
    /// exists.
    pub fn git_warnings_beyond_project(
        &self,
        writable_roots: &[PathBuf],
        cwd: &Path,
    ) -> Vec<String> {
        let candidates: Vec<PathBuf> = match self {
            Self::Unreadable(_) => writable_roots
                .iter()
                .map(|root| root.join(".git"))
                .filter(|git_dir| std::fs::symlink_metadata(git_dir).is_ok())
                .collect(),
            Self::Known(_) => self.git_dirs_to_show(writable_roots, cwd),
        };
        candidates
            .iter()
            .filter_map(|git_dir| self.git_warning(git_dir))
            .collect()
    }

    /// D424 (#1775): the recorded `.git` entries to show: those whose folder
    /// is `cwd` or above it (as before), or that lie at or below one of
    /// `writable_roots` (canonical) or below `cwd`. Recorded `.codex`/`.agents`
    /// folders are left out: Core only loads those from `cwd` and above.
    pub fn git_dirs_to_show(&self, writable_roots: &[PathBuf], cwd: &Path) -> Vec<PathBuf> {
        let Self::Known(planted) = self else {
            return Vec::new();
        };
        planted
            .iter()
            .filter(|path| is_git_dir(path) && in_git_scope(path, writable_roots, cwd))
            .cloned()
            .collect()
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

/// D424 (#1775): whether a recorded git folder at `path` is shown: its folder
/// is `cwd` or above it, or it lies below `cwd` or at or below one of
/// `writable_roots` (canonical).
fn in_git_scope(path: &Path, writable_roots: &[PathBuf], cwd: &Path) -> bool {
    let cwd = dunce::canonicalize(cwd).unwrap_or_else(|_| cwd.to_path_buf());
    path.parent()
        .is_some_and(|parent| is_at_or_below(&cwd, parent))
        || is_at_or_below(path, &cwd)
        || writable_roots.iter().any(|root| is_at_or_below(path, root))
}

/// D420 (#1775): a repository below the top of a writable root (or a
/// writable root that is a bare repository) that appeared, or whose config,
/// hooks or other files git runs programs from changed, while a non-admin
/// sandbox command ran; and what changed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PlantedGitChange {
    /// Its `.git` entry, or a bare repository's folder.
    pub path: PathBuf,
    /// For the person: `new repository`, `config changed`,
    /// `hooks/post-checkout added` ...
    pub changes: Vec<String>,
}

impl PlantedGitChange {
    /// `path (what changed)`, at most five changes and how many more.
    pub fn describe(&self) -> String {
        const SHOWN: usize = 5;
        let mut changes = self
            .changes
            .iter()
            .take(SHOWN)
            .cloned()
            .collect::<Vec<_>>()
            .join(", ");
        if self.changes.len() > SHOWN {
            changes.push_str(&format!(" and {} more", self.changes.len() - SHOWN));
        }
        format!("{} ({changes})", self.path.display())
    }

    /// The startup warning for it.
    pub fn warning(&self) -> String {
        format!(
            "{PLANTED_GIT_WARNING_PREFIX}{}. It appeared or changed while a non-admin Windows sandbox command ran, so a command may have planted it. Git runs a repository's hooks and config outside the sandbox whenever you run git there. Check them, then confirm it when Corbanu Terminal asks.",
            self.describe()
        )
    }
}

/// D420 (#1775): the repositories that appeared or changed and aren't
/// confirmed yet. Always empty off Windows; empty too when the records
/// can't be read ([`planted_project_metadata`] reports that).
pub fn planted_git_changes() -> Vec<PlantedGitChange> {
    #[cfg(target_os = "windows")]
    {
        match codex_windows_sandbox::planted_git_changes() {
            Ok(changes) => changes
                .into_iter()
                .map(|(path, changes)| PlantedGitChange { path, changes })
                .collect(),
            Err(err) => {
                tracing::warn!("could not read the planted repository records: {err:#}");
                Vec::new()
            }
        }
    }
    #[cfg(not(target_os = "windows"))]
    {
        Vec::new()
    }
}

/// D420/D424 (#1775): those of `changes` to show at startup: in a folder
/// non-admin sandbox commands may write to (`writable_roots`, canonical),
/// below `cwd`, or in `cwd` or a folder above it.
pub fn git_changes_to_show(
    changes: Vec<PlantedGitChange>,
    writable_roots: &[PathBuf],
    cwd: &Path,
) -> Vec<PlantedGitChange> {
    changes
        .into_iter()
        .filter(|change| in_git_scope(&change.path, writable_roots, cwd))
        .collect()
}

/// D424 (#1775): what to tell the person right after a command when
/// `paths` (canonical) were recorded as possibly planted while it ran, and
/// (D420) `git_changes`: at most 20 of each kind, and how many more. `None`
/// when there are none.
pub fn planted_after_command_notice(
    paths: &[PathBuf],
    git_changes: &[PlantedGitChange],
) -> Option<String> {
    const SHOWN: usize = 20;
    if paths.is_empty() && git_changes.is_empty() {
        return None;
    }
    let (git_dirs, folders): (Vec<&PathBuf>, Vec<&PathBuf>) =
        paths.iter().partition(|path| is_git_dir(path));
    let git_dirs: Vec<String> = git_dirs
        .iter()
        .map(|path| path.display().to_string())
        .chain(git_changes.iter().map(PlantedGitChange::describe))
        .collect();
    let folders: Vec<String> = folders
        .iter()
        .map(|path| path.display().to_string())
        .collect();
    let list = |items: &[String]| {
        let mut list = items
            .iter()
            .take(SHOWN)
            .cloned()
            .collect::<Vec<_>>()
            .join(", ");
        if items.len() > SHOWN {
            list.push_str(&format!(" and {} more", items.len() - SHOWN));
        }
        list
    };
    let mut parts = Vec::new();
    if !git_dirs.is_empty() {
        parts.push(format!(
            "{PLANTED_GIT_WARNING_PREFIX}{}. It appeared or changed while a non-admin Windows sandbox command ran (this one, or another that wrote there), so that command may have planted it. Git runs its hooks and config outside the sandbox whenever you (or your editor) run git there: check them before you do. Corbanu Terminal asks you to confirm it when it next starts.",
            list(&git_dirs)
        ));
    }
    if !folders.is_empty() {
        parts.push(format!(
            "{PLANTED_PROJECT_METADATA_WARNING_PREFIX}{}. It appeared or changed while a non-admin Windows sandbox command ran (this one, or another that wrote there), so that command may have planted it. Its config, hooks and skills aren't loaded until you confirm it.",
            list(&folders)
        ));
    }
    Some(parts.join("\n"))
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
