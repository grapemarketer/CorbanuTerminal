//! D285 (#1542 item 6): writes D261's Everyone `FILE_DELETE_CHILD` denies
//! without `SetNamedSecurityInfoW`'s tree walk, with the same result.
//!
//! `SetNamedSecurityInfoW` rewrites the DACL of every file and folder below
//! the one it changes, recomputing each from its parent's: about 6 s each way
//! for the workspace root and 3 s for `.git` on a 101,000-file workspace. For
//! these entries the walk changes nothing below except, for `.git`'s
//! (`CONTAINER_INHERIT` only), the copies its subfolders inherit, plus any
//! object whose inherited entries had gone stale (a file the user moved in
//! from elsewhere keeps its old DACL), which the walk repairs.
//!
//! So this writes the folder's own DACL (once Windows computes the same for
//! it from its own parent's), then reads every object below and has Windows
//! compute its DACL from its parent's
//! (`CreatePrivateObjectSecurityEx`, auto-inherit), as the walk would:
//! - equal: nothing to write;
//! - a subfolder that differs only in the Everyone delete-child copies:
//!   written (only those copies change);
//! - anything else (a stale object, a link that would change, any failure):
//!   the whole call falls back to `SetNamedSecurityInfoW`, the #1549 call,
//!   which walks, repairs and undoes any partial direct write.
//!
//! Reading is about three times cheaper than the walk's rewrite. Links are
//! checked but not followed, as the walk does. D311/D313: the reading runs on
//! several threads, and each entry is opened through its folder's open
//! handle rather than by path (`acl_walk.rs`).

use super::DenyAceKind;
use super::FILE_MAPPING;
use super::INHERITED_ACE;
use super::is_exact_delete_child_deny;
use super::walk::Handle;
use super::walk::Kind;
use super::walk::Subfolder;
use super::walk::check_threads;
use super::walk::list;
use super::walk::open_relative;
use super::walk::par_walk;
use crate::winutil::to_wide;
use anyhow::Result;
use anyhow::anyhow;
use anyhow::bail;
use std::collections::HashMap;
use std::ffi::OsString;
use std::ffi::c_void;
use std::os::windows::ffi::OsStringExt;
use std::path::Path;
use std::path::PathBuf;
use std::sync::Arc;
use windows_sys::Win32::Foundation::ERROR_SUCCESS;
use windows_sys::Win32::Foundation::GetLastError;
use windows_sys::Win32::Foundation::HANDLE;
use windows_sys::Win32::Foundation::HLOCAL;
use windows_sys::Win32::Foundation::LocalFree;
use windows_sys::Win32::Security::ACE_HEADER;
use windows_sys::Win32::Security::ACL;
use windows_sys::Win32::Security::ACL_SIZE_INFORMATION;
use windows_sys::Win32::Security::AclSizeInformation;
use windows_sys::Win32::Security::AddAce;
use windows_sys::Win32::Security::Authorization::EXPLICIT_ACCESS_W;
use windows_sys::Win32::Security::Authorization::GetNamedSecurityInfoW;
use windows_sys::Win32::Security::Authorization::GetSecurityInfo;
use windows_sys::Win32::Security::Authorization::SetEntriesInAclW;
use windows_sys::Win32::Security::Authorization::SetNamedSecurityInfoW;
use windows_sys::Win32::Security::Authorization::TRUSTEE_IS_SID;
use windows_sys::Win32::Security::Authorization::TRUSTEE_IS_UNKNOWN;
use windows_sys::Win32::Security::Authorization::TRUSTEE_W;
use windows_sys::Win32::Security::CreatePrivateObjectSecurityEx;
use windows_sys::Win32::Security::DACL_SECURITY_INFORMATION;
use windows_sys::Win32::Security::DestroyPrivateObjectSecurity;
use windows_sys::Win32::Security::EqualSid;
use windows_sys::Win32::Security::GROUP_SECURITY_INFORMATION;
use windows_sys::Win32::Security::GetAce;
use windows_sys::Win32::Security::GetAclInformation;
use windows_sys::Win32::Security::GetLengthSid;
use windows_sys::Win32::Security::GetSecurityDescriptorControl;
use windows_sys::Win32::Security::GetSecurityDescriptorDacl;
use windows_sys::Win32::Security::GetSecurityDescriptorGroup;
use windows_sys::Win32::Security::GetSecurityDescriptorOwner;
use windows_sys::Win32::Security::InitializeAcl;
use windows_sys::Win32::Security::InitializeSecurityDescriptor;
use windows_sys::Win32::Security::OWNER_SECURITY_INFORMATION;
use windows_sys::Win32::Security::PROTECTED_DACL_SECURITY_INFORMATION;
use windows_sys::Win32::Security::SE_DACL_AUTO_INHERIT_REQ;
use windows_sys::Win32::Security::SE_DACL_AUTO_INHERITED;
use windows_sys::Win32::Security::SE_DACL_PROTECTED;
use windows_sys::Win32::Security::SECURITY_DESCRIPTOR;
use windows_sys::Win32::Security::SEF_AVOID_OWNER_CHECK;
use windows_sys::Win32::Security::SEF_AVOID_PRIVILEGE_CHECK;
use windows_sys::Win32::Security::SEF_DACL_AUTO_INHERIT;
use windows_sys::Win32::Security::SetKernelObjectSecurity;
use windows_sys::Win32::Security::SetSecurityDescriptorControl;
use windows_sys::Win32::Security::SetSecurityDescriptorDacl;
use windows_sys::Win32::Security::SetSecurityDescriptorGroup;
use windows_sys::Win32::Security::SetSecurityDescriptorOwner;
use windows_sys::Win32::Security::UNPROTECTED_DACL_SECURITY_INFORMATION;
use windows_sys::Win32::Storage::FileSystem::FILE_DELETE_CHILD;
use windows_sys::Win32::Storage::FileSystem::FILE_FLAG_BACKUP_SEMANTICS;
use windows_sys::Win32::Storage::FileSystem::FILE_LIST_DIRECTORY;
use windows_sys::Win32::Storage::FileSystem::READ_CONTROL;
use windows_sys::Win32::Storage::FileSystem::SYNCHRONIZE;
use windows_sys::Win32::Storage::FileSystem::WRITE_DAC;
use windows_sys::Win32::System::SystemServices::SECURITY_DESCRIPTOR_REVISION;

const ACCESS_DENIED_ACE_TYPE: u8 = 1;
const CONTAINER_INHERIT_ACE: u8 = 0x02;
const DENY_ACCESS: i32 = 3;

/// How [`set_delete_child_deny`] wrote the DACL.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Written {
    /// The entry was already as asked; nothing was written.
    Nothing,
    /// Written without the tree walk.
    Direct,
    /// Written by `SetNamedSecurityInfoW` (the #1549 way), because the
    /// direct write could not be shown to change only these entries.
    Walked,
}

/// Adds (`add`) or removes the explicit entry denying `psid` exactly
/// `FILE_DELETE_CHILD` on `path`, inherited by subfolders with
/// `inherit_to_subdirs`, as #1549 did but without the tree walk where it
/// isn't needed. Errors as #1549 did: adding fails only when the DACL can't
/// be read, and a write that fails leaves `Nothing` written.
///
/// # Safety
/// Caller must ensure `psid` points to a valid SID and `path` exists.
pub(super) unsafe fn set_delete_child_deny(
    path: &Path,
    psid: *mut c_void,
    inherit_to_subdirs: bool,
    add: bool,
) -> Result<Written> {
    let kind = DenyAceKind::DeleteChild { inherit_to_subdirs };
    let is_entry = |ace: *const c_void| is_exact_delete_child_deny(ace, psid, inherit_to_subdirs);
    // Follows a link at `path` itself, as `SetNamedSecurityInfoW` does;
    // callers never pass one.
    let opened = Handle::open_with(path, READ_CONTROL | WRITE_DAC, FILE_FLAG_BACKUP_SEMANTICS)
        .and_then(|handle| Descriptor::read(handle.0).map(|current| (handle, current)));
    let Ok((handle, current)) = opened else {
        // Nothing written yet: the #1549 calls as they were.
        return if add {
            super::add_deny_ace(path, psid, kind).map(|added| {
                if added {
                    Written::Walked
                } else {
                    Written::Nothing
                }
            })
        } else {
            super::remove_matching_aces(path, |_, ace| is_entry(ace)).map(|removed| {
                if removed {
                    Written::Walked
                } else {
                    Written::Nothing
                }
            })
        };
    };
    let new_dacl = if add {
        if kind.already_present(current.dacl, psid, /*is_dir*/ true) {
            return Ok(Written::Nothing);
        }
        match Acl::with_deny(current.dacl, psid, kind) {
            Ok(acl) => acl,
            Err(_) => return Ok(Written::Nothing),
        }
    } else {
        match Acl::without(current.dacl, is_entry)? {
            Some(acl) => acl,
            None => return Ok(Written::Nothing),
        }
    };
    let direct = (|| -> Result<()> {
        if !same_aces_except(current.dacl, new_dacl.as_ptr(), is_entry)? {
            bail!("the new DACL changes more than the delete-child entry");
        }
        if !is_mutant(Mutant::TargetUnchecked) {
            check_against_parent(path, &current, new_dacl.as_ptr())?;
        }
        write_and_check(handle.0, &current, new_dacl.as_ptr())?;
        sync_below(path, &handle, psid)
    })();
    drop(handle);
    if direct.is_ok() {
        return Ok(Written::Direct);
    }
    walk(path, psid, inherit_to_subdirs, add)
}

/// The #1549 call, on the DACL as it is now (another writer may have changed
/// it since it was read): Windows writes it and recomputes every inherited
/// entry below, repairing stale ones and undoing any partial direct write.
/// If it fails, what was already written stays, as after a failed #1549
/// walk: adding reports `Nothing`, removing an error (the record stays).
unsafe fn walk(
    path: &Path,
    psid: *mut c_void,
    inherit_to_subdirs: bool,
    add: bool,
) -> Result<Written> {
    let kind = DenyAceKind::DeleteChild { inherit_to_subdirs };
    let mut sd: *mut c_void = std::ptr::null_mut();
    let mut dacl: *mut ACL = std::ptr::null_mut();
    let code = GetNamedSecurityInfoW(
        to_wide(path).as_ptr(),
        1, // SE_FILE_OBJECT
        DACL_SECURITY_INFORMATION,
        std::ptr::null_mut(),
        std::ptr::null_mut(),
        &mut dacl,
        std::ptr::null_mut(),
        &mut sd,
    );
    if code != ERROR_SUCCESS {
        return Err(anyhow!("GetNamedSecurityInfoW failed: {code}"));
    }
    let sd = LocalSd(sd);
    let rebuilt = if add {
        if kind.already_present(dacl, psid, /*is_dir*/ true) {
            None
        } else {
            match Acl::with_deny(dacl, psid, kind) {
                Ok(acl) => Some(acl),
                Err(_) => return Ok(Written::Nothing),
            }
        }
    } else {
        Acl::without(dacl, |ace| {
            is_exact_delete_child_deny(ace, psid, inherit_to_subdirs)
        })?
    };
    let info = if add {
        DACL_SECURITY_INFORMATION
    } else {
        let mut control: u16 = 0;
        let mut revision: u32 = 0;
        if GetSecurityDescriptorControl(sd.0, &mut control, &mut revision) == 0 {
            bail!("GetSecurityDescriptorControl failed: {}", GetLastError());
        }
        if control & SE_DACL_PROTECTED != 0 {
            DACL_SECURITY_INFORMATION | PROTECTED_DACL_SECURITY_INFORMATION
        } else {
            DACL_SECURITY_INFORMATION | UNPROTECTED_DACL_SECURITY_INFORMATION
        }
    };
    let code = SetNamedSecurityInfoW(
        to_wide(path).as_ptr() as *mut u16,
        1, // SE_FILE_OBJECT
        info,
        std::ptr::null_mut(),
        std::ptr::null_mut(),
        rebuilt.as_ref().map_or(dacl, |acl| acl.as_ptr()),
        std::ptr::null_mut(),
    );
    drop(sd);
    match code {
        ERROR_SUCCESS => Ok(Written::Walked),
        _ if add => Ok(Written::Nothing),
        _ => Err(anyhow!("setting the DACL failed: {code}")),
    }
}

/// Checks every object below `root` (whose DACL was just written) against
/// the DACL Windows computes from its parent's, as the tree walk would
/// rewrite it. Writes the subfolders that differ only in the Everyone
/// delete-child copies; fails on any other difference, on a link that would
/// change and on any error, so the caller walks instead. Links are not
/// followed.
///
/// D311: folders are checked on several threads; D313: each is held open
/// while its entries are listed and opened through its handle
/// ([`open_relative`]), so what is checked and written is always an
/// entry of the folder whose descriptor was read. A subfolder still to walk
/// is opened again through its folder later and must be the same folder
/// ([`Subfolder`]); `root` must be the folder `written` (whose DACL was just
/// written).
unsafe fn sync_below(root: &Path, written: &Handle, psid: *mut c_void) -> Result<()> {
    // The folder itself, not a link, and the one whose DACL was written.
    let handle = Handle::open(root, READ_CONTROL | FILE_LIST_DIRECTORY | SYNCHRONIZE)?;
    if handle.kind()?.kind() != Kind::Folder || handle.identity()? != written.identity()? {
        bail!("{} is not the folder written", root.display());
    }
    // The SID outlives the walk, which ends before this returns.
    let sid = psid as usize;
    let mutant = mutant();
    par_walk(
        vec![Folder {
            at: Place::Open(handle),
            path: root.to_path_buf(),
        }],
        check_threads(),
        |folder: Folder, found: &mut Vec<Folder>| {
            set_mutant(mutant);
            sync_folder(folder, sid as *mut c_void, found)
        },
    )
}

/// A folder [`sync_below`] still has to check the entries of.
struct Folder {
    at: Place,
    /// For messages only; never opened.
    path: PathBuf,
}

enum Place {
    /// The folder below which the walk starts, held open.
    Open(Handle),
    /// A subfolder, opened again through its folder when walked.
    Below(Subfolder),
}

/// One folder of [`sync_below`]: checks (and writes) its entries, adding its
/// plain subfolders to `found`.
unsafe fn sync_folder(folder: Folder, psid: *mut c_void, found: &mut Vec<Folder>) -> Result<()> {
    let is_copy = |ace: *const c_void| is_inherited_delete_child_copy(ace, psid);
    let without_copies = |aces: &[Vec<u8>]| -> Vec<Vec<u8>> {
        aces.iter()
            .filter(|ace| !is_copy(ace.as_ptr() as *const c_void))
            .cloned()
            .collect()
    };
    let flags = SE_DACL_PROTECTED | SE_DACL_AUTO_INHERITED;
    let dir = Arc::new(match folder.at {
        Place::Open(handle) => handle,
        Place::Below(subfolder) => subfolder.reopen(READ_CONTROL)?,
    });
    let parent = Descriptor::read(dir.0)?;
    // What Windows computes depends only on the parent's descriptor and the
    // object's kind, owner, group, flags and explicit entries; most objects
    // in a folder share those.
    let mut computed_for: HashMap<Vec<u8>, (Vec<Vec<u8>>, u16)> = HashMap::new();
    let mut entries = list(dir.0)?;
    if is_mutant(Mutant::ListingTruncated) {
        entries.truncate(600);
    }
    for (name, listed) in entries {
        let path = folder.path.join(OsString::from_wide(&name));
        let is_dir = listed.is_dir();
        // A folder, not a link: written if needed and walked.
        let is_plain_dir = listed.kind() == Kind::Folder;
        if !is_dir && is_mutant(Mutant::FilesUnchecked) {
            continue;
        }
        // Only a plain folder may need writing (and walking).
        let access = if is_plain_dir {
            READ_CONTROL | WRITE_DAC
        } else {
            READ_CONTROL
        };
        // The entry itself, links not followed.
        let handle = open_relative(dir.0, &name, access)?;
        if !handle.kind()?.same_kind(listed) {
            bail!("{} changed while being checked", path.display());
        }
        let child = Descriptor::read(handle.0)?;
        let aces = ace_bytes(child.dacl, &|_| false)?;
        let key = computation_key(&child, &aces, is_dir)?;
        let (computed, control) = match computed_for.get(&key) {
            Some(found) => found,
            None => {
                let computed = Computed::new(parent.sd, child.sd, is_dir)?;
                let value = (
                    ace_bytes(computed.dacl()?, &|_| false)?,
                    computed.control()?,
                );
                computed_for.entry(key).or_insert(value)
            }
        };
        if control & flags != child.control & flags {
            bail!("{} would change protection or inheritance", path.display());
        }
        let walk_into = is_plain_dir && !is_mutant(Mutant::SubfoldersUnwalked);
        if aces == *computed {
            if walk_into {
                found.push(Folder {
                    at: Place::Below(Subfolder::new(&dir, name, &handle)?),
                    path,
                });
            }
            continue;
        }
        if !is_plain_dir || without_copies(&aces) != without_copies(computed) {
            if is_mutant(Mutant::StaleIgnored) {
                continue;
            }
            bail!(
                "{} would change more than the delete-child copies",
                path.display()
            );
        }
        if !is_mutant(Mutant::CopiesUnwritten) {
            let computed = Computed::new(parent.sd, child.sd, is_dir)?;
            write_and_check(handle.0, &child, computed.dacl()?)?;
        }
        if walk_into {
            found.push(Folder {
                at: Place::Below(Subfolder::new(&dir, name, &handle)?),
                path,
            });
        }
    }
    Ok(())
}

/// Deliberately broken versions of the direct write, which the regression
/// tests must catch (D311). Set per calling thread, so tests running in
/// parallel are unaffected; never set outside tests.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[cfg_attr(not(test), allow(dead_code))]
pub(super) enum Mutant {
    /// Files below the folder are not checked.
    FilesUnchecked,
    /// Subfolders are not walked.
    SubfoldersUnwalked,
    /// Objects that differ by more than the delete-child copies are left as
    /// they are instead of falling back to the walk.
    StaleIgnored,
    /// Subfolders that differ only in the delete-child copies are walked
    /// but not written.
    CopiesUnwritten,
    /// The folder itself is not checked against its parent.
    TargetUnchecked,
    /// Only a folder's first 600 entries (about one listing buffer) are
    /// checked.
    ListingTruncated,
}

#[cfg(test)]
thread_local! {
    pub(super) static MUTANT: std::cell::Cell<Option<Mutant>> =
        const { std::cell::Cell::new(None) };
}

#[cfg(test)]
fn mutant() -> Option<Mutant> {
    MUTANT.with(std::cell::Cell::get)
}

#[cfg(not(test))]
fn mutant() -> Option<Mutant> {
    None
}

#[cfg(test)]
fn set_mutant(mutant: Option<Mutant>) {
    MUTANT.with(|cell| cell.set(mutant));
}

#[cfg(not(test))]
fn set_mutant(_mutant: Option<Mutant>) {}

fn is_mutant(which: Mutant) -> bool {
    mutant() == Some(which)
}

/// The inputs besides the parent's descriptor that decide what
/// `CreatePrivateObjectSecurityEx` computes for `child`: its kind, flags,
/// owner, group and explicit (not inherited) entries, or all its entries if
/// its DACL is protected.
unsafe fn computation_key(child: &Descriptor, aces: &[Vec<u8>], is_dir: bool) -> Result<Vec<u8>> {
    let mut key = vec![u8::from(is_dir)];
    key.extend_from_slice(&child.control.to_le_bytes());
    for owner in [true, false] {
        let mut sid: *mut c_void = std::ptr::null_mut();
        let mut defaulted = 0;
        let ok = if owner {
            GetSecurityDescriptorOwner(child.sd, &mut sid, &mut defaulted)
        } else {
            GetSecurityDescriptorGroup(child.sd, &mut sid, &mut defaulted)
        };
        if ok == 0 {
            bail!("reading the owner or group failed: {}", GetLastError());
        }
        if sid.is_null() {
            key.extend_from_slice(&0_u32.to_le_bytes());
        } else {
            let len = GetLengthSid(sid) as usize;
            key.extend_from_slice(&(len as u32).to_le_bytes());
            key.extend_from_slice(std::slice::from_raw_parts(sid as *const u8, len));
        }
    }
    // A protected DACL inherits nothing; it is kept whole.
    let protected = child.control & SE_DACL_PROTECTED != 0;
    for ace in aces {
        if protected || ace[1] & INHERITED_ACE == 0 {
            key.extend_from_slice(&(ace.len() as u32).to_le_bytes());
            key.extend_from_slice(ace);
        }
    }
    Ok(key)
}

/// The copy of a `CONTAINER_INHERIT` Everyone delete-child deny that a
/// subfolder inherits.
unsafe fn is_inherited_delete_child_copy(ace: *const c_void, psid: *mut c_void) -> bool {
    let hdr = &*(ace as *const ACE_HEADER);
    if hdr.AceType != ACCESS_DENIED_ACE_TYPE
        || hdr.AceFlags != CONTAINER_INHERIT_ACE | INHERITED_ACE
    {
        return false;
    }
    let mask = *((ace as usize + std::mem::size_of::<ACE_HEADER>()) as *const u32);
    let sid = (ace as usize + std::mem::size_of::<ACE_HEADER>() + std::mem::size_of::<u32>())
        as *mut c_void;
    mask == FILE_DELETE_CHILD && EqualSid(sid, psid) != 0
}

/// #1612 review r2: `SetNamedSecurityInfoW` also recomputes `path`'s own
/// inherited entries from its parent's. Fails unless Windows computes
/// exactly `dacl` (and the same flags) for `path` from its parent, as for
/// the objects below.
unsafe fn check_against_parent(path: &Path, current: &Descriptor, dacl: *const ACL) -> Result<()> {
    let Some(parent_path) = path.parent() else {
        bail!("{} has no parent", path.display());
    };
    // By name, as `SetNamedSecurityInfoW` finds the parent.
    let parent_handle = Handle::open_with(parent_path, READ_CONTROL, FILE_FLAG_BACKUP_SEMANTICS)?;
    let parent = Descriptor::read(parent_handle.0)?;
    let flags = SE_DACL_PROTECTED | SE_DACL_AUTO_INHERITED;
    let mut sd: SECURITY_DESCRIPTOR = std::mem::zeroed();
    let psd = &mut sd as *mut SECURITY_DESCRIPTOR as *mut c_void;
    let (mut owner, mut group): (*mut c_void, *mut c_void) =
        (std::ptr::null_mut(), std::ptr::null_mut());
    let mut defaulted = 0;
    if InitializeSecurityDescriptor(psd, SECURITY_DESCRIPTOR_REVISION) == 0
        || GetSecurityDescriptorOwner(current.sd, &mut owner, &mut defaulted) == 0
        || GetSecurityDescriptorGroup(current.sd, &mut group, &mut defaulted) == 0
        || SetSecurityDescriptorOwner(psd, owner, 0) == 0
        || SetSecurityDescriptorGroup(psd, group, 0) == 0
        || SetSecurityDescriptorDacl(psd, 1, dacl, 0) == 0
        || SetSecurityDescriptorControl(psd, flags, current.control & flags) == 0
    {
        bail!(
            "building the security descriptor failed: {}",
            GetLastError()
        );
    }
    let computed = Computed::new(parent.sd, psd, /*is_dir*/ true)?;
    if computed.control()? & flags != current.control & flags
        || !same_aces_except(computed.dacl()?, dacl, |_| false)?
    {
        bail!(
            "{} would change more than the delete-child entry",
            path.display()
        );
    }
    Ok(())
}

/// Writes `dacl` to the object alone (no propagation), keeping its
/// protection and auto-inherited flags, and checks what was written.
unsafe fn write_and_check(handle: HANDLE, current: &Descriptor, dacl: *const ACL) -> Result<()> {
    let flags = SE_DACL_PROTECTED | SE_DACL_AUTO_INHERITED;
    let mut sd: SECURITY_DESCRIPTOR = std::mem::zeroed();
    let psd = &mut sd as *mut SECURITY_DESCRIPTOR as *mut c_void;
    if InitializeSecurityDescriptor(psd, SECURITY_DESCRIPTOR_REVISION) == 0
        || SetSecurityDescriptorDacl(psd, 1, dacl, 0) == 0
    {
        bail!(
            "building the security descriptor failed: {}",
            GetLastError()
        );
    }
    let mut bits = current.control & flags;
    // Asks the file system to keep the auto-inherited flag.
    if bits & SE_DACL_AUTO_INHERITED != 0 {
        bits |= SE_DACL_AUTO_INHERIT_REQ;
    }
    if SetSecurityDescriptorControl(psd, flags | SE_DACL_AUTO_INHERIT_REQ, bits) == 0 {
        bail!("SetSecurityDescriptorControl failed: {}", GetLastError());
    }
    if SetKernelObjectSecurity(handle, DACL_SECURITY_INFORMATION, psd) == 0 {
        bail!("SetKernelObjectSecurity failed: {}", GetLastError());
    }
    let written = Descriptor::read(handle)?;
    if written.control & flags != current.control & flags
        || !same_aces_except(written.dacl, dacl, |_| false)?
    {
        bail!("the written DACL differs from the one asked for");
    }
    Ok(())
}

/// Whether `a` and `b` hold the same entries in the same order once those
/// `skip` selects are left out of both.
unsafe fn same_aces_except(
    a: *const ACL,
    b: *const ACL,
    skip: impl Fn(*const c_void) -> bool,
) -> Result<bool> {
    let left = ace_bytes(a, &skip)?;
    let right = ace_bytes(b, &skip)?;
    Ok(left == right)
}

unsafe fn ace_bytes(
    acl: *const ACL,
    skip: &impl Fn(*const c_void) -> bool,
) -> Result<Vec<Vec<u8>>> {
    if acl.is_null() {
        bail!("no DACL");
    }
    let mut out = Vec::new();
    for i in 0..ace_count(acl)? {
        let mut ace: *mut c_void = std::ptr::null_mut();
        if GetAce(acl, i, &mut ace) == 0 {
            bail!("GetAce failed: {}", GetLastError());
        }
        if skip(ace) {
            continue;
        }
        let size = usize::from((*(ace as *const ACE_HEADER)).AceSize);
        out.push(std::slice::from_raw_parts(ace as *const u8, size).to_vec());
    }
    Ok(out)
}

unsafe fn ace_count(acl: *const ACL) -> Result<u32> {
    let mut info: ACL_SIZE_INFORMATION = std::mem::zeroed();
    if GetAclInformation(
        acl,
        &mut info as *mut _ as *mut c_void,
        std::mem::size_of::<ACL_SIZE_INFORMATION>() as u32,
        AclSizeInformation,
    ) == 0
    {
        bail!("GetAclInformation failed: {}", GetLastError());
    }
    Ok(info.AceCount)
}

/// An object's owner, group and DACL as `GetSecurityInfo` returns them.
struct Descriptor {
    sd: *mut c_void,
    dacl: *mut ACL,
    control: u16,
}

impl Descriptor {
    unsafe fn read(handle: HANDLE) -> Result<Self> {
        let mut sd: *mut c_void = std::ptr::null_mut();
        let mut dacl: *mut ACL = std::ptr::null_mut();
        let code = GetSecurityInfo(
            handle,
            1, // SE_FILE_OBJECT
            OWNER_SECURITY_INFORMATION | GROUP_SECURITY_INFORMATION | DACL_SECURITY_INFORMATION,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            &mut dacl,
            std::ptr::null_mut(),
            &mut sd,
        );
        if code != ERROR_SUCCESS {
            bail!("GetSecurityInfo failed: {code}");
        }
        let mut descriptor = Self {
            sd,
            dacl,
            control: 0,
        };
        let mut revision: u32 = 0;
        if GetSecurityDescriptorControl(sd, &mut descriptor.control, &mut revision) == 0 {
            bail!("GetSecurityDescriptorControl failed: {}", GetLastError());
        }
        if dacl.is_null() {
            bail!("the object has no DACL");
        }
        Ok(descriptor)
    }
}

impl Drop for Descriptor {
    fn drop(&mut self) {
        if !self.sd.is_null() {
            // SAFETY: allocated by `GetSecurityInfo`.
            unsafe {
                LocalFree(self.sd as HLOCAL);
            }
        }
    }
}

/// A descriptor `LocalFree` releases.
struct LocalSd(*mut c_void);

impl Drop for LocalSd {
    fn drop(&mut self) {
        if !self.0.is_null() {
            // SAFETY: allocated by `GetNamedSecurityInfoW`.
            unsafe {
                LocalFree(self.0 as HLOCAL);
            }
        }
    }
}

/// An object's descriptor as Windows computes it from its parent's
/// (inherited entries) and its own (explicit entries, owner and group).
struct Computed(*mut c_void);

impl Computed {
    unsafe fn new(parent: *mut c_void, current: *mut c_void, is_dir: bool) -> Result<Self> {
        let mut sd: *mut c_void = std::ptr::null_mut();
        if CreatePrivateObjectSecurityEx(
            parent,
            current,
            &mut sd,
            std::ptr::null(),
            i32::from(is_dir),
            SEF_DACL_AUTO_INHERIT | SEF_AVOID_OWNER_CHECK | SEF_AVOID_PRIVILEGE_CHECK,
            0,
            &FILE_MAPPING,
        ) == 0
        {
            bail!("CreatePrivateObjectSecurityEx failed: {}", GetLastError());
        }
        Ok(Self(sd))
    }

    unsafe fn control(&self) -> Result<u16> {
        let mut control: u16 = 0;
        let mut revision: u32 = 0;
        if GetSecurityDescriptorControl(self.0, &mut control, &mut revision) == 0 {
            bail!("GetSecurityDescriptorControl failed: {}", GetLastError());
        }
        Ok(control)
    }

    unsafe fn dacl(&self) -> Result<*const ACL> {
        let mut present = 0;
        let mut defaulted = 0;
        let mut dacl: *mut ACL = std::ptr::null_mut();
        if GetSecurityDescriptorDacl(self.0, &mut present, &mut dacl, &mut defaulted) == 0
            || present == 0
            || dacl.is_null()
        {
            bail!("the computed descriptor has no DACL");
        }
        Ok(dacl)
    }
}

impl Drop for Computed {
    fn drop(&mut self) {
        // SAFETY: allocated by `CreatePrivateObjectSecurityEx`.
        unsafe {
            DestroyPrivateObjectSecurity(&self.0);
        }
    }
}

/// A DACL this module built.
enum Acl {
    /// From `SetEntriesInAclW` (`LocalFree`).
    Local(*mut ACL),
    /// Built in place; `u32` storage keeps it aligned.
    Owned(Vec<u32>),
}

impl Acl {
    /// `dacl` with an entry denying `psid` what `kind` denies.
    unsafe fn with_deny(dacl: *mut ACL, psid: *mut c_void, kind: DenyAceKind) -> Result<Self> {
        let mut explicit: EXPLICIT_ACCESS_W = std::mem::zeroed();
        explicit.grfAccessPermissions = kind.mask();
        explicit.grfAccessMode = DENY_ACCESS;
        explicit.grfInheritance = kind.inheritance();
        explicit.Trustee = TRUSTEE_W {
            pMultipleTrustee: std::ptr::null_mut(),
            MultipleTrusteeOperation: 0,
            TrusteeForm: TRUSTEE_IS_SID,
            TrusteeType: TRUSTEE_IS_UNKNOWN,
            ptstrName: psid as *mut u16,
        };
        let mut new_dacl: *mut ACL = std::ptr::null_mut();
        let code = SetEntriesInAclW(1, &explicit, dacl, &mut new_dacl);
        if code != ERROR_SUCCESS {
            bail!("SetEntriesInAclW failed: {code}");
        }
        Ok(Self::Local(new_dacl))
    }

    /// `dacl` without the entries `matches` selects, or `None` if there are
    /// none.
    unsafe fn without(
        dacl: *mut ACL,
        matches: impl Fn(*const c_void) -> bool,
    ) -> Result<Option<Self>> {
        let revision = u32::from((*dacl).AclRevision);
        let size = (*dacl).AclSize;
        let mut buffer = vec![0_u32; usize::from(size).div_ceil(4)];
        let new_dacl = buffer.as_mut_ptr() as *mut ACL;
        if InitializeAcl(new_dacl, u32::from(size), revision) == 0 {
            bail!("InitializeAcl failed: {}", GetLastError());
        }
        let mut removed = false;
        for i in 0..ace_count(dacl)? {
            let mut ace: *mut c_void = std::ptr::null_mut();
            if GetAce(dacl, i, &mut ace) == 0 {
                bail!("GetAce failed: {}", GetLastError());
            }
            if matches(ace) {
                removed = true;
                continue;
            }
            let ace_size = u32::from((*(ace as *const ACE_HEADER)).AceSize);
            if AddAce(new_dacl, revision, u32::MAX, ace, ace_size) == 0 {
                bail!("AddAce failed: {}", GetLastError());
            }
        }
        Ok(removed.then_some(Self::Owned(buffer)))
    }

    fn as_ptr(&self) -> *mut ACL {
        match self {
            Self::Local(acl) => *acl,
            Self::Owned(buffer) => buffer.as_ptr() as *mut ACL,
        }
    }
}

impl Drop for Acl {
    fn drop(&mut self) {
        if let Self::Local(acl) = self
            && !acl.is_null()
        {
            // SAFETY: allocated by `SetEntriesInAclW`.
            unsafe {
                LocalFree(*acl as HLOCAL);
            }
        }
    }
}

#[cfg(test)]
#[path = "acl_delete_child_tests.rs"]
mod tests;
