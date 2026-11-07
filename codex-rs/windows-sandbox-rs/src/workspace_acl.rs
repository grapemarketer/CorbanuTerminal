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
    // and write a persistent deny onto whatever it points at.
    if is_reparse_point(path) {
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
/// never followed.
///
/// # Safety
/// Caller must ensure every pointer in `cap_sids` is a valid SID.
pub unsafe fn deny_moving_ancestors(
    path: &Path,
    root: &Path,
    cap_sids: &[*mut c_void],
    denies: &mut Vec<DeleteChildDeny>,
) -> Result<()> {
    for ancestor in &ancestors_below_root(path, root) {
        if is_reparse_point(ancestor) || !ancestor.is_dir() {
            continue;
        }
        for sid in cap_sids {
            add_deny_delete_ace(ancestor, *sid)?;
        }
        if let Some(parent) = ancestor.parent()
            && !is_reparse_point(parent)
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

pub(crate) fn is_reparse_point(path: &Path) -> bool {
    use std::os::windows::fs::MetadataExt;
    const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x400;
    std::fs::symlink_metadata(path)
        .map(|metadata| metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0)
        .unwrap_or(true)
}
