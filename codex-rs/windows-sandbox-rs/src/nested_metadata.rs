//! D283 (#1541): `.codex` and `.agents` folders below the top of a legacy
//! session's writable roots. Core loads them from every folder between a
//! project's root and its working directory, so a sandboxed command could
//! plant one deeper in a writable root, where D262 doesn't look.
//!
//! When a session starts, [`find`] lists the ones that exist; the session
//! protects them (and the folders above them) like the top-level ones and
//! records them with their file identity. When it ends, every one found then
//! that isn't in that list, unchanged, counts as planted ([`appeared`]), and
//! so does every folder the walk couldn't list. While it runs,
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

/// What a walk of the writable roots found.
#[derive(Default)]
pub(crate) struct Found {
    pub(crate) entries: Vec<NestedMetadata>,
    /// Folders the walk couldn't list (or entries it couldn't inspect): what
    /// lies below them is unknown.
    pub(crate) unlistable: Vec<PathBuf>,
}

/// The `.codex`/`.agents` entries below the top of `roots` (canonical), of
/// any kind (a link counts), recorded under the name Core opens. Links are
/// never followed; other reparse points, such as ProjFS folders or cloud
/// placeholders, are walked like plain folders (#1672). The walk doesn't descend into `.git` folders or into
/// the entries it reports: the sandbox can't write inside a protected one,
/// and Core doesn't run from inside `.git`. Names match the way NTFS
/// compares them (each character upper-cased), so `.agentſ` counts; such a
/// name never matches a snapshot.
pub(crate) fn find(roots: &[PathBuf]) -> Found {
    let mut found = Found::default();
    for root in roots {
        let mut stack = vec![root.clone()];
        while let Some(dir) = stack.pop() {
            let entries = match std::fs::read_dir(&dir) {
                Ok(entries) => entries,
                // Gone. A folder that is still there but says "not found"
                // (a ProjFS or Cloud Files provider can) is unlistable.
                Err(err)
                    if err.kind() == std::io::ErrorKind::NotFound
                        && std::fs::symlink_metadata(&dir)
                            .is_err_and(|err| err.kind() == std::io::ErrorKind::NotFound) =>
                {
                    continue;
                }
                Err(_) => {
                    found.unlistable.push(dir);
                    continue;
                }
            };
            let top = dir == *root;
            for entry in entries {
                let Ok(entry) = entry else {
                    found.unlistable.push(dir.clone());
                    break;
                };
                let name = entry.file_name();
                let name = name.to_string_lossy();
                // Not followed if it is a link. A listing that shows a
                // reparse point may not carry its tag, which tells links
                // apart: then the entry itself is asked (#1672).
                let metadata = match entry.metadata() {
                    Ok(metadata)
                        if metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0 =>
                    {
                        std::fs::symlink_metadata(entry.path())
                    }
                    other => other,
                };
                if let Some((metadata_name, exact)) = metadata_name(&name) {
                    // The top-level ones are D262's.
                    if top && exact {
                        continue;
                    }
                    if !top {
                        let path = dir.join(metadata_name);
                        let identity = (exact && !is_link(metadata.as_ref()))
                            .then(|| identity(&entry.path()))
                            .flatten();
                        found.entries.push(NestedMetadata { path, identity });
                        if exact {
                            continue;
                        }
                    }
                }
                let Ok(metadata) = metadata else {
                    found.unlistable.push(entry.path());
                    continue;
                };
                // `is_dir` is false for links.
                if metadata.is_dir()
                    // ASCII only: skipping a folded name (`.gıt`) would
                    // leave an ordinary folder unwatched.
                    && !name.eq_ignore_ascii_case(".git")
                {
                    stack.push(entry.path());
                }
            }
        }
    }
    found
}

/// Which of `.codex`/`.agents` `name` is, and whether it is that name up to
/// ASCII case.
fn metadata_name(name: &str) -> Option<(&'static str, bool)> {
    PROJECT_METADATA_NAMES.into_iter().find_map(|metadata| {
        if name.eq_ignore_ascii_case(metadata) {
            Some((metadata, true))
        } else if folds_to(name, metadata) {
            Some((metadata, false))
        } else {
            None
        }
    })
}

/// Whether `name` upper-cases, character by character as NTFS compares
/// names, to `target` (ASCII).
fn folds_to(name: &str, target: &str) -> bool {
    let mut folded = name.chars().map(|c| {
        let mut upper = c.to_uppercase();
        match (upper.next(), upper.next()) {
            (Some(single), None) => single,
            _ => c,
        }
    });
    target
        .chars()
        .all(|t| folded.next() == Some(t.to_ascii_uppercase()))
        && folded.next().is_none()
}

/// The entries of `found` the session protects like the top-level ones:
/// not links (their targets are the sandbox's choice), and identified (the
/// others count as planted anyway).
pub(crate) fn to_protect(found: &[NestedMetadata]) -> Vec<NestedMetadata> {
    found
        .iter()
        .filter(|entry| entry.identity.is_some())
        .cloned()
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
/// the top of one of `roots` and isn't in `before` as the file it is now
/// (one that doesn't exist counts: it may appear right after this check).
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

/// D332 (#1682): the top-level `.git` of each of `roots` (canonical) that
/// exists, of any kind, with the file it is. A link (Google's `repo` tool
/// makes one; a sandboxed command could point it elsewhere) or one that
/// can't be inspected has no identity, so it counts as changed after every
/// session. Git runs a `.git`'s hooks and config outside the sandbox, so one
/// that appears or changes while a session runs counts as planted, as new
/// `.codex`/`.agents` folders do; a missing one may still be made
/// (`git init` must work).
pub(crate) fn top_level_git(roots: &[PathBuf]) -> Vec<NestedMetadata> {
    roots
        .iter()
        .filter_map(|root| {
            let path = root.join(".git");
            let identity = match std::fs::symlink_metadata(&path) {
                Err(err) if err.kind() == std::io::ErrorKind::NotFound => return None,
                Ok(metadata) if !metadata.file_type().is_symlink() => identity(&path),
                _ => None,
            };
            Some(NestedMetadata { path, identity })
        })
        .collect()
}

/// D332: whether `folder` is the top-level `.git` of one of `roots` and
/// isn't in `before` (from [`top_level_git`]) as the file it is now (one
/// that doesn't exist counts: it may appear right after this check).
pub(crate) fn git_appeared_in_session(
    roots: &[PathBuf],
    before: &[NestedMetadata],
    folder: &Path,
) -> bool {
    let top_level = folder
        .file_name()
        .is_some_and(|name| name.eq_ignore_ascii_case(".git"))
        && folder
            .parent()
            .is_some_and(|parent| roots.iter().any(|root| same_path(parent, root)));
    if !top_level {
        return false;
    }
    let now = identity(folder).filter(|_| !crate::workspace_acl::is_link(folder));
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

/// A link (a name-surrogate reparse point); unknown counts as one. Other
/// reparse points, such as ProjFS folders or cloud placeholders, are the
/// folder itself (#1672).
fn is_link(metadata: Result<&std::fs::Metadata, &std::io::Error>) -> bool {
    metadata.map_or(true, |metadata| metadata.file_type().is_symlink())
}

fn same_path(a: &Path, b: &Path) -> bool {
    a.as_os_str()
        .to_string_lossy()
        .eq_ignore_ascii_case(&b.as_os_str().to_string_lossy())
}

/// `path` is `base` or below it, ignoring case.
pub(crate) fn starts_with(path: &Path, base: &Path) -> bool {
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
