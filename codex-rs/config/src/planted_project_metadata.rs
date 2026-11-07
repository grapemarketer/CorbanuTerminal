//! #1506 item 2 (D262): `.codex` and `.agents` folders at the top of a
//! writable root that were absent when a non-admin (legacy) Windows sandbox
//! session started and existed when it ended (or while it still runs). A
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
    pub fn contains(&self, folder: &Path) -> bool {
        match self {
            Self::Unreadable(_) => true,
            Self::Known(planted) if planted.is_empty() => false,
            Self::Known(planted) => {
                let folder = without_following(folder);
                planted.iter().any(|entry| same_path(entry, &folder))
            }
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

/// `path` with its parent canonicalized and its last component kept as is.
fn without_following(path: &Path) -> PathBuf {
    match (path.parent(), path.file_name()) {
        (Some(parent), Some(name)) => dunce::canonicalize(parent)
            .unwrap_or_else(|_| parent.to_path_buf())
            .join(name),
        _ => path.to_path_buf(),
    }
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
