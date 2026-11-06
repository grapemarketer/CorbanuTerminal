//! D283 (#1541): `.codex` and `.agents` folders below the top of a legacy
//! session's writable roots. Core loads them from every folder between a
//! project's root and its working directory, so a sandboxed command could
//! plant one deeper in a writable root, where D262 doesn't look.
//!
//! When a session starts, [`find`] lists the ones that exist; the session
//! protects them like the top-level ones and records them with their file
//! identity. When it ends, every one found then that isn't in that list,
//! unchanged, counts as planted ([`appeared`]). While it runs,
//! [`appeared_in_session`] tells Core the same about a single folder.

use serde::Deserialize;
use serde::Serialize;
use std::os::windows::fs::MetadataExt as _;
use std::path::Path;
use std::path::PathBuf;

const PROJECT_METADATA_NAMES: [&str; 2] = [".codex", ".agents"];
const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x400;

/// A nested `.codex`/`.agents` entry and the file it was. `identity` is
/// `None` for a link (the sandbox may be able to point it elsewhere without
/// changing the link's identity) or when it can't be read; such an entry
/// never matches, so it counts as planted after every session.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct NestedMetadata {
    pub(crate) path: PathBuf,
    pub(crate) identity: Option<FileIdentity>,
}

/// Volume serial number and 128-bit file ID.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct FileIdentity {
    volume: u64,
    id: [u8; 16],
}

/// The `.codex`/`.agents` entries below the top of `roots` (canonical), of
/// any kind (a link counts). Links are never followed, and the walk doesn't
/// descend into `.git` folders or into the entries it reports: the sandbox
/// can't write inside a protected one, and Core doesn't run from inside
/// `.git`. A folder that can't be listed is skipped.
pub(crate) fn find(roots: &[PathBuf]) -> Vec<NestedMetadata> {
    let mut found = Vec::new();
    for root in roots {
        let mut stack = vec![root.clone()];
        while let Some(dir) = stack.pop() {
            let Ok(entries) = std::fs::read_dir(&dir) else {
                continue;
            };
            let top = dir == *root;
            for entry in entries.flatten() {
                let name = entry.file_name();
                let name = name.to_string_lossy();
                let metadata_name = PROJECT_METADATA_NAMES
                    .iter()
                    .any(|metadata| name.eq_ignore_ascii_case(metadata));
                // Not followed if it is a link.
                let metadata = entry.metadata();
                if metadata_name {
                    // The top-level ones are D262's.
                    if !top {
                        let path = entry.path();
                        found.push(NestedMetadata {
                            identity: (!is_link(metadata.as_ref()))
                                .then(|| identity(&path))
                                .flatten(),
                            path,
                        });
                    }
                    continue;
                }
                let Ok(metadata) = metadata else {
                    continue;
                };
                if metadata.is_dir()
                    && metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT == 0
                    && !name.eq_ignore_ascii_case(".git")
                {
                    stack.push(entry.path());
                }
            }
        }
    }
    found
}

/// The entries of `found` the session protects like the top-level ones:
/// not links (their targets are the sandbox's choice), and identified (the
/// others count as planted anyway).
pub(crate) fn to_protect(found: &[NestedMetadata]) -> Vec<PathBuf> {
    found
        .iter()
        .filter(|entry| entry.identity.is_some())
        .map(|entry| entry.path.clone())
        .collect()
}

/// The entries of `now` that aren't in `before` as the same file.
pub(crate) fn appeared(before: &[NestedMetadata], now: Vec<NestedMetadata>) -> Vec<PathBuf> {
    now.into_iter()
        .filter(|entry| {
            entry.identity.is_none()
                || !before
                    .iter()
                    .any(|old| same_path(&old.path, &entry.path) && old.identity == entry.identity)
        })
        .map(|entry| entry.path)
        .collect()
}

/// Whether `folder` (a `.codex`/`.agents` path, parent canonical) lies below
/// the top of one of `roots` and isn't in `before` as the file it is now.
pub(crate) fn appeared_in_session(
    roots: &[PathBuf],
    before: &[NestedMetadata],
    folder: &Path,
) -> bool {
    let Some(parent) = folder.parent() else {
        return false;
    };
    let nested = roots
        .iter()
        .any(|root| !same_path(parent, root) && starts_with(parent, root));
    if !nested {
        return false;
    }
    let now = identity(folder);
    now.is_none()
        || !before
            .iter()
            .any(|old| same_path(&old.path, folder) && old.identity == now)
}

/// The file `path` is (not followed if it is a link).
pub(crate) fn identity(path: &Path) -> Option<FileIdentity> {
    use std::os::windows::fs::OpenOptionsExt as _;
    use std::os::windows::io::AsRawHandle as _;
    const FILE_FLAG_BACKUP_SEMANTICS: u32 = 0x0200_0000;
    const FILE_FLAG_OPEN_REPARSE_POINT: u32 = 0x0020_0000;
    const FILE_READ_ATTRIBUTES: u32 = 0x80;
    let file = std::fs::OpenOptions::new()
        .access_mode(FILE_READ_ATTRIBUTES)
        .share_mode(0x7)
        .custom_flags(FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT)
        .open(path)
        .ok()?;
    // SAFETY: the handle is open for the call.
    let (volume, id) = unsafe { crate::acl::file_identity(file.as_raw_handle() as _) }.ok()?;
    Some(FileIdentity { volume, id })
}

/// A link or other reparse point; unknown counts as one.
fn is_link(metadata: Result<&std::fs::Metadata, &std::io::Error>) -> bool {
    metadata.map_or(true, |metadata| {
        metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0
    })
}

fn same_path(a: &Path, b: &Path) -> bool {
    a.as_os_str()
        .to_string_lossy()
        .eq_ignore_ascii_case(&b.as_os_str().to_string_lossy())
}

/// `path` is `base` or below it, ignoring case.
fn starts_with(path: &Path, base: &Path) -> bool {
    let mut path = path.components();
    base.components().all(|part| {
        path.next().is_some_and(|other| {
            other
                .as_os_str()
                .to_string_lossy()
                .eq_ignore_ascii_case(&part.as_os_str().to_string_lossy())
        })
    })
}

#[cfg(test)]
#[path = "nested_metadata_tests.rs"]
mod tests;
