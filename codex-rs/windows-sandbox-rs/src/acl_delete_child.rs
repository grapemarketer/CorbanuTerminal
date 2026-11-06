//! D285 (#1542 item 6): writes D261's Everyone `FILE_DELETE_CHILD` denies
//! without letting Windows walk the whole tree below the folder.
//!
//! `SetNamedSecurityInfoW` re-propagates inheritance through every file and
//! folder below the one it changes, whatever changed: about 5.6 s each way
//! for the workspace root and 2.85 s for `.git` on a 101,000-file workspace.
//! These entries need much less:
//! - The root's entry has no inheritance flags, so no child inherits it and
//!   writing the root's own DACL is all that changes.
//! - `.git`'s entry is `CONTAINER_INHERIT` only, so only its subfolders carry
//!   copies; files inherit nothing from it. Each subfolder's DACL is
//!   recomputed from its parent's by Windows (`CreatePrivateObjectSecurityEx`)
//!   and written; files are never opened.
//!
//! Each write must change nothing but these Everyone delete-child entries,
//! checked before writing and read back after. Anything else (an unexpected
//! DACL, a link among the subfolders, any failure) falls back to
//! `SetNamedSecurityInfoW` with the same DACL, the #1549 behaviour.

use super::DenyAceKind;
use super::FILE_MAPPING;
use super::INHERITED_ACE;
use super::is_exact_delete_child_deny;
use crate::winutil::to_wide;
use anyhow::Result;
use anyhow::anyhow;
use anyhow::bail;
use std::ffi::c_void;
use std::path::Path;
use std::path::PathBuf;
use windows_sys::Win32::Foundation::CloseHandle;
use windows_sys::Win32::Foundation::ERROR_SUCCESS;
use windows_sys::Win32::Foundation::GetLastError;
use windows_sys::Win32::Foundation::HANDLE;
use windows_sys::Win32::Foundation::HLOCAL;
use windows_sys::Win32::Foundation::INVALID_HANDLE_VALUE;
use windows_sys::Win32::Foundation::LocalFree;
use windows_sys::Win32::Security::ACE_HEADER;
use windows_sys::Win32::Security::ACL;
use windows_sys::Win32::Security::ACL_SIZE_INFORMATION;
use windows_sys::Win32::Security::AclSizeInformation;
use windows_sys::Win32::Security::AddAce;
use windows_sys::Win32::Security::Authorization::EXPLICIT_ACCESS_W;
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
use windows_sys::Win32::Security::GetSecurityDescriptorControl;
use windows_sys::Win32::Security::GetSecurityDescriptorDacl;
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
use windows_sys::Win32::Security::UNPROTECTED_DACL_SECURITY_INFORMATION;
use windows_sys::Win32::Storage::FileSystem::BY_HANDLE_FILE_INFORMATION;
use windows_sys::Win32::Storage::FileSystem::CreateFileW;
use windows_sys::Win32::Storage::FileSystem::FILE_ATTRIBUTE_DIRECTORY;
use windows_sys::Win32::Storage::FileSystem::FILE_ATTRIBUTE_REPARSE_POINT;
use windows_sys::Win32::Storage::FileSystem::FILE_DELETE_CHILD;
use windows_sys::Win32::Storage::FileSystem::FILE_FLAG_BACKUP_SEMANTICS;
use windows_sys::Win32::Storage::FileSystem::FILE_FLAG_OPEN_REPARSE_POINT;
use windows_sys::Win32::Storage::FileSystem::FILE_SHARE_DELETE;
use windows_sys::Win32::Storage::FileSystem::FILE_SHARE_READ;
use windows_sys::Win32::Storage::FileSystem::FILE_SHARE_WRITE;
use windows_sys::Win32::Storage::FileSystem::GetFileInformationByHandle;
use windows_sys::Win32::Storage::FileSystem::OPEN_EXISTING;
use windows_sys::Win32::Storage::FileSystem::READ_CONTROL;
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
    let opened = Handle::open(path, /*open_link*/ false)
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
        write_and_check(handle.0, &current, new_dacl.as_ptr())?;
        if inherit_to_subdirs {
            update_subfolders(path, psid)?;
        }
        Ok(())
    })();
    if direct.is_ok() {
        return Ok(Written::Direct);
    }
    // The #1549 call, with the same DACL: Windows writes it and recomputes
    // every inherited entry below, undoing any partial direct write.
    let info = if add {
        DACL_SECURITY_INFORMATION
    } else if current.control & SE_DACL_PROTECTED != 0 {
        DACL_SECURITY_INFORMATION | PROTECTED_DACL_SECURITY_INFORMATION
    } else {
        DACL_SECURITY_INFORMATION | UNPROTECTED_DACL_SECURITY_INFORMATION
    };
    drop(handle);
    let code = SetNamedSecurityInfoW(
        to_wide(path).as_ptr() as *mut u16,
        1, // SE_FILE_OBJECT
        info,
        std::ptr::null_mut(),
        std::ptr::null_mut(),
        new_dacl.as_ptr(),
        std::ptr::null_mut(),
    );
    match code {
        ERROR_SUCCESS => Ok(Written::Walked),
        _ if add => Ok(Written::Nothing),
        _ => Err(anyhow!("setting the DACL failed: {code}")),
    }
}

/// Recomputes the DACL of every folder below `root` from its parent's, as
/// the tree walk would, depth first. Files are skipped: only
/// `CONTAINER_INHERIT` changed, which files never inherit. Fails on a link
/// or anything but the Everyone delete-child copies changing.
unsafe fn update_subfolders(root: &Path, psid: *mut c_void) -> Result<()> {
    let is_copy = |ace: *const c_void| is_inherited_delete_child_copy(ace, psid);
    let mut pending: Vec<PathBuf> = vec![root.to_path_buf()];
    while let Some(parent) = pending.pop() {
        let parent_handle = Handle::open(&parent, /*open_link*/ true)?;
        if !parent_handle.is_plain_dir()? {
            bail!("{} is not a plain folder", parent.display());
        }
        let parent_sd = Descriptor::read(parent_handle.0)?;
        drop(parent_handle);
        for entry in std::fs::read_dir(&parent)? {
            let entry = entry?;
            use std::os::windows::fs::MetadataExt;
            // Not followed: the entry's own attributes.
            let attributes = entry.metadata()?.file_attributes();
            if attributes & FILE_ATTRIBUTE_DIRECTORY == 0 {
                continue;
            }
            if attributes & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
                bail!("{} is a link", entry.path().display());
            }
            let path = entry.path();
            let handle = Handle::open(&path, /*open_link*/ true)?;
            if !handle.is_plain_dir()? {
                bail!("{} is not a plain folder", path.display());
            }
            let child = Descriptor::read(handle.0)?;
            let computed = Computed::new(parent_sd.sd, child.sd)?;
            let new_dacl = computed.dacl()?;
            if !same_aces_except(child.dacl, new_dacl, is_copy)? {
                bail!(
                    "{} would change more than the delete-child copies",
                    path.display()
                );
            }
            if !same_aces_except(child.dacl, new_dacl, |_| false)? {
                write_and_check(handle.0, &child, new_dacl)?;
            }
            pending.push(path);
        }
    }
    Ok(())
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

/// A handle opened for reading and writing the DACL.
struct Handle(HANDLE);

impl Handle {
    unsafe fn open(path: &Path, open_link: bool) -> Result<Self> {
        let mut flags = FILE_FLAG_BACKUP_SEMANTICS;
        if open_link {
            flags |= FILE_FLAG_OPEN_REPARSE_POINT;
        }
        let handle = CreateFileW(
            to_wide(path).as_ptr(),
            READ_CONTROL | WRITE_DAC,
            FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
            std::ptr::null_mut(),
            OPEN_EXISTING,
            flags,
            0,
        );
        if handle == INVALID_HANDLE_VALUE {
            let err = std::io::Error::last_os_error();
            bail!("open {}: {err}", path.display());
        }
        Ok(Self(handle))
    }

    unsafe fn is_plain_dir(&self) -> Result<bool> {
        let mut info: BY_HANDLE_FILE_INFORMATION = std::mem::zeroed();
        if GetFileInformationByHandle(self.0, &mut info) == 0 {
            bail!("GetFileInformationByHandle failed: {}", GetLastError());
        }
        Ok(info.dwFileAttributes & FILE_ATTRIBUTE_DIRECTORY != 0
            && info.dwFileAttributes & FILE_ATTRIBUTE_REPARSE_POINT == 0)
    }
}

impl Drop for Handle {
    fn drop(&mut self) {
        // SAFETY: a handle this type opened.
        unsafe {
            CloseHandle(self.0);
        }
    }
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

/// A folder's descriptor as Windows computes it from its parent's
/// (inherited entries) and its own (explicit entries, owner and group).
struct Computed(*mut c_void);

impl Computed {
    unsafe fn new(parent: *mut c_void, current: *mut c_void) -> Result<Self> {
        let mut sd: *mut c_void = std::ptr::null_mut();
        if CreatePrivateObjectSecurityEx(
            parent,
            current,
            &mut sd,
            std::ptr::null(),
            /*iscontainerobject*/ 1,
            SEF_DACL_AUTO_INHERIT | SEF_AVOID_OWNER_CHECK | SEF_AVOID_PRIVILEGE_CHECK,
            0,
            &FILE_MAPPING,
        ) == 0
        {
            bail!("CreatePrivateObjectSecurityEx failed: {}", GetLastError());
        }
        Ok(Self(sd))
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
