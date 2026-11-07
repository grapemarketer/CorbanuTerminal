use crate::acl::add_deny_delete_ace;
use crate::acl::add_deny_write_ace;
use crate::integrity_leases::DeleteChildDeny;
use crate::path_normalization::canonicalize_path;
use anyhow::Result;
use std::ffi::c_void;
use std::path::Path;
use std::path::PathBuf;

pub fn is_command_cwd_root(root: &Path, canonical_command_cwd: &Path) -> bool {
    canonicalize_path(root) == canonical_command_cwd
}

/// Adds to `denies` the Everyone denies the caller must add (tracked, #1506
/// item 1) to close the delete-child route.
///
/// # Safety
/// Caller must ensure `psid` is a valid SID pointer.
pub unsafe fn protect_workspace_codex_dir(
    cwd: &Path,
    psid: *mut c_void,
    denies: &mut Vec<DeleteChildDeny>,
) -> Result<bool> {
    protect_workspace_subdir(cwd, psid, ".codex", denies)
}

/// As [`protect_workspace_codex_dir`], for `.agents`.
///
/// # Safety
/// Caller must ensure `psid` is a valid SID pointer.
pub unsafe fn protect_workspace_agents_dir(
    cwd: &Path,
    psid: *mut c_void,
    denies: &mut Vec<DeleteChildDeny>,
) -> Result<bool> {
    protect_workspace_subdir(cwd, psid, ".agents", denies)
}

unsafe fn protect_workspace_subdir(
    cwd: &Path,
    psid: *mut c_void,
    subdir: &str,
    denies: &mut Vec<DeleteChildDeny>,
) -> Result<bool> {
    let path = cwd.join(subdir);
    if path.is_dir() {
        let added = add_deny_write_ace(&path, psid)?;
        denies.extend(delete_child_route_denies(&path));
        Ok(added)
    } else {
        Ok(false)
    }
}

/// The denies that close the `FILE_DELETE_CHILD` route to deleting or moving a
/// protected path from a legacy (WRITE_RESTRICTED) sandbox, which
/// capability-SID deny ACEs do not cover (#158): Everyone `FILE_DELETE_CHILD`
/// on the path's parent (that directory only) and on the path and its
/// subdirectories. The caller adds them with
/// [`crate::integrity_leases::add_tracked_delete_child_denies`], which
/// removes them again once no session needs them (#1506 item 1).
///
/// Everyone is in every sandbox token's normal groups, so the deny applies no
/// matter which user or group ACE grants the right. Unsandboxed deletes keep
/// working because they use the object's own `DELETE` right.
pub fn delete_child_route_denies(path: &Path) -> Vec<DeleteChildDeny> {
    // The sandbox may have planted a junction or symlink here; never follow it
    // and write a persistent deny onto whatever it points at. Other reparse
    // points, such as cloud placeholders, are the folder itself (#1668).
    if is_link(path) {
        return Vec::new();
    }
    let mut denies = Vec::new();
    if let Some(parent) = path.parent()
        && parent.is_dir()
    {
        denies.push(DeleteChildDeny {
            path: parent.to_path_buf(),
            inherit_to_subdirs: false,
        });
    }
    if path.is_dir() {
        denies.push(DeleteChildDeny {
            path: path.to_path_buf(),
            inherit_to_subdirs: true,
        });
    }
    denies
}

/// Folders strictly between `root` and `path`, nearest to `root` first. Both
/// are canonicalized, so a link in `path` that leaves `root` yields nothing.
pub(crate) fn ancestors_below_root(path: &Path, root: &Path) -> Vec<PathBuf> {
    let path = canonicalize_path(path);
    let root = canonicalize_path(root);
    if path == root || !path.starts_with(&root) {
        return Vec::new();
    }
    let mut ancestors: Vec<PathBuf> = path
        .ancestors()
        .skip(1)
        .take_while(|ancestor| *ancestor != root.as_path())
        .map(Path::to_path_buf)
        .collect();
    ancestors.reverse();
    ancestors
}

/// #1506 item 3 (D263): keeps a legacy sandbox from moving or renaming a
/// folder between writable root `root` and the explicit read-only carve-out
/// `path`, which would carry the carve-out away from its deny entries and let
/// the command recreate it. Each such folder denies `cap_sids` `DELETE` on
/// itself, and its parent denies Everyone `FILE_DELETE_CHILD` (that directory
/// only; added to `denies` for the caller to add tracked), so neither route
/// to a rename is open. Writes inside those folders keep working; the
/// sandbox just can't delete or rename the folders themselves. Unsandboxed
/// renames keep working through the user's own `DELETE` right. Links are
/// never followed (D293 refuses them there); other reparse points, such as
/// cloud placeholders, get the denies like plain folders (#1668), and a
/// folder that can't be inspected must take them too.
///
/// # Safety
/// Caller must ensure every pointer in `cap_sids` is a valid SID.
pub unsafe fn deny_moving_ancestors(
    path: &Path,
    root: &Path,
    cap_sids: &[*mut c_void],
    denies: &mut Vec<DeleteChildDeny>,
) -> Result<()> {
    deny_moving_folders(&ancestors_below_root(path, root), cap_sids, denies)
}

/// D283: as [`deny_moving_ancestors`], and `path` (a folder) can't be moved
/// or renamed either.
///
/// # Safety
/// Caller must ensure every pointer in `cap_sids` is a valid SID.
pub unsafe fn deny_moving_path_and_ancestors(
    path: &Path,
    root: &Path,
    cap_sids: &[*mut c_void],
    denies: &mut Vec<DeleteChildDeny>,
) -> Result<()> {
    let mut folders = ancestors_below_root(path, root);
    let canonical = canonicalize_path(path);
    if canonical.starts_with(canonicalize_path(root)) && canonical != canonicalize_path(root) {
        folders.push(canonical);
    }
    deny_moving_folders(&folders, cap_sids, denies)
}

unsafe fn deny_moving_folders(
    folders: &[PathBuf],
    cap_sids: &[*mut c_void],
    denies: &mut Vec<DeleteChildDeny>,
) -> Result<()> {
    for ancestor in folders {
        if std::fs::symlink_metadata(ancestor)
            .is_ok_and(|metadata| metadata.file_type().is_symlink() || !metadata.is_dir())
        {
            continue;
        }
        for sid in cap_sids {
            add_deny_delete_ace(ancestor, *sid)?;
        }
        if let Some(parent) = ancestor.parent()
            && !is_link(parent)
            && parent.is_dir()
        {
            denies.push(DeleteChildDeny {
                path: parent.to_path_buf(),
                inherit_to_subdirs: false,
            });
        }
    }
    Ok(())
}

/// D293 (#1542 item 1): a link (junction, symbolic link or mount point)
/// above the explicit read-only path `path` that a sandboxed command could
/// remove and replace, because the folder holding it lies inside one of the
/// writable `roots` (canonical): then the read-only path could be recreated
/// behind it, away from D263's denies, which land on the link's target. Each
/// folder above `path` as spelled counts, and so does each folder on the way
/// to a link's target (a link above the roots, or a root that is a link, may
/// point at one). Unreadable folders inside a root count as links; a chain
/// of more than 32 links counts as one.
pub(crate) fn link_above_carve_out(path: &Path, roots: &[PathBuf]) -> Option<PathBuf> {
    let inside_a_root = |folder: &Path| {
        let canonical = canonicalize_path(folder);
        roots.iter().any(|root| canonical.starts_with(root))
    };
    // (spelling to check, whether to check the spelling itself too)
    let mut pending = vec![(path.to_path_buf(), false)];
    let mut links = 0;
    while let Some((spelled, with_itself)) = pending.pop() {
        for folder in spelled.ancestors().skip(usize::from(!with_itself)) {
            let Some(parent) = folder.parent() else {
                continue;
            };
            let replaceable = inside_a_root(parent);
            let is_link = match std::fs::symlink_metadata(folder) {
                Ok(metadata) => metadata.file_type().is_symlink(),
                Err(_) => replaceable,
            };
            if !is_link {
                continue;
            }
            if replaceable {
                return Some(folder.to_path_buf());
            }
            links += 1;
            if links > 32 {
                return Some(folder.to_path_buf());
            }
            if let Ok(target) = std::fs::read_link(folder) {
                let target = dunce::simplified(&target).to_path_buf();
                pending.push((parent.join(target), true));
            }
        }
    }
    None
}

/// Whether `path` is a link (a junction, symbolic link, mount point or other
/// name-surrogate reparse point), which names another location. Other
/// reparse points, such as cloud-file (OneDrive) placeholders, are the
/// object itself. A path that can't be inspected counts as a link.
pub(crate) fn is_link(path: &Path) -> bool {
    std::fs::symlink_metadata(path)
        .map(|metadata| metadata.file_type().is_symlink())
        .unwrap_or(true)
}

pub(crate) fn is_reparse_point(path: &Path) -> bool {
    use std::os::windows::fs::MetadataExt;
    const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x400;
    std::fs::symlink_metadata(path)
        .map(|metadata| metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0)
        .unwrap_or(true)
}
