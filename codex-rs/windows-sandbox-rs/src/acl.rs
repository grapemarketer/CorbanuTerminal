use crate::winutil::to_wide;
use anyhow::Context;
use anyhow::Result;
use anyhow::anyhow;
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
use windows_sys::Win32::Security::ACCESS_ALLOWED_ACE;
use windows_sys::Win32::Security::ACCESS_DENIED_ACE;
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
use windows_sys::Win32::Security::Authorization::SetSecurityInfo;
use windows_sys::Win32::Security::Authorization::TRUSTEE_IS_SID;
use windows_sys::Win32::Security::Authorization::TRUSTEE_IS_UNKNOWN;
use windows_sys::Win32::Security::Authorization::TRUSTEE_W;
use windows_sys::Win32::Security::DACL_SECURITY_INFORMATION;
use windows_sys::Win32::Security::EqualSid;
use windows_sys::Win32::Security::GENERIC_MAPPING;
use windows_sys::Win32::Security::GetAce;
use windows_sys::Win32::Security::GetAclInformation;
use windows_sys::Win32::Security::GetSecurityDescriptorControl;
use windows_sys::Win32::Security::InitializeAcl;
use windows_sys::Win32::Security::MapGenericMask;
use windows_sys::Win32::Security::PROTECTED_DACL_SECURITY_INFORMATION;
use windows_sys::Win32::Security::SE_DACL_PROTECTED;
use windows_sys::Win32::Security::UNPROTECTED_DACL_SECURITY_INFORMATION;
use windows_sys::Win32::Storage::FileSystem::BY_HANDLE_FILE_INFORMATION;
use windows_sys::Win32::Storage::FileSystem::CreateFileW;
use windows_sys::Win32::Storage::FileSystem::DELETE;
use windows_sys::Win32::Storage::FileSystem::FILE_ALL_ACCESS;
use windows_sys::Win32::Storage::FileSystem::FILE_APPEND_DATA;
use windows_sys::Win32::Storage::FileSystem::FILE_ATTRIBUTE_DIRECTORY;
use windows_sys::Win32::Storage::FileSystem::FILE_ATTRIBUTE_NORMAL;
use windows_sys::Win32::Storage::FileSystem::FILE_ATTRIBUTE_REPARSE_POINT;
use windows_sys::Win32::Storage::FileSystem::FILE_DELETE_CHILD;
use windows_sys::Win32::Storage::FileSystem::FILE_FLAG_BACKUP_SEMANTICS;
use windows_sys::Win32::Storage::FileSystem::FILE_FLAG_OPEN_REPARSE_POINT;
use windows_sys::Win32::Storage::FileSystem::FILE_GENERIC_EXECUTE;
use windows_sys::Win32::Storage::FileSystem::FILE_GENERIC_READ;
use windows_sys::Win32::Storage::FileSystem::FILE_GENERIC_WRITE;
use windows_sys::Win32::Storage::FileSystem::FILE_ID_INFO;
use windows_sys::Win32::Storage::FileSystem::FILE_SHARE_DELETE;
use windows_sys::Win32::Storage::FileSystem::FILE_SHARE_READ;
use windows_sys::Win32::Storage::FileSystem::FILE_SHARE_WRITE;
use windows_sys::Win32::Storage::FileSystem::FILE_WRITE_ATTRIBUTES;
use windows_sys::Win32::Storage::FileSystem::FILE_WRITE_DATA;
use windows_sys::Win32::Storage::FileSystem::FILE_WRITE_EA;
use windows_sys::Win32::Storage::FileSystem::FileIdInfo;
use windows_sys::Win32::Storage::FileSystem::GetFileInformationByHandle;
use windows_sys::Win32::Storage::FileSystem::GetFileInformationByHandleEx;
use windows_sys::Win32::Storage::FileSystem::GetFinalPathNameByHandleW;
use windows_sys::Win32::Storage::FileSystem::OPEN_EXISTING;
use windows_sys::Win32::Storage::FileSystem::READ_CONTROL;
use windows_sys::Win32::Storage::FileSystem::WRITE_DAC;
const SE_KERNEL_OBJECT: u32 = 6;
const INHERIT_ONLY_ACE: u8 = 0x08;
const INHERITED_ACE: u8 = 0x10;
const ACCESS_ALLOWED_ACE_TYPE: u8 = 0;
const ACCESS_DENIED_ACE_TYPE: u8 = 1;
const GENERIC_READ_MASK: u32 = 0x8000_0000;
const GENERIC_WRITE_MASK: u32 = 0x4000_0000;
const DENY_ACCESS: i32 = 3;

/// Fetch DACL via handle-based query; caller must LocalFree the returned SD.
///
/// # Safety
/// Caller must free the returned security descriptor with `LocalFree` and pass an existing path.
pub unsafe fn fetch_dacl_handle(path: &Path) -> Result<(*mut ACL, *mut c_void)> {
    let wpath = to_wide(path);
    let h = CreateFileW(
        wpath.as_ptr(),
        READ_CONTROL,
        FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
        std::ptr::null_mut(),
        OPEN_EXISTING,
        FILE_FLAG_BACKUP_SEMANTICS,
        0,
    );
    if h == INVALID_HANDLE_VALUE {
        return Err(anyhow!("CreateFileW failed for {}", path.display()));
    }
    let mut p_sd: *mut c_void = std::ptr::null_mut();
    let mut p_dacl: *mut ACL = std::ptr::null_mut();
    let code = GetSecurityInfo(
        h,
        1, // SE_FILE_OBJECT
        DACL_SECURITY_INFORMATION,
        std::ptr::null_mut(),
        std::ptr::null_mut(),
        &mut p_dacl,
        std::ptr::null_mut(),
        &mut p_sd,
    );
    CloseHandle(h);
    if code != ERROR_SUCCESS {
        return Err(anyhow!(
            "GetSecurityInfo failed for {}: {}",
            path.display(),
            code
        ));
    }
    Ok((p_dacl, p_sd))
}

/// Fast mask-based check: does an ACE for provided SIDs grant the desired mask? Skips inherit-only.
/// When `require_all_bits` is true, all bits in `desired_mask` must be present; otherwise any bit suffices.
pub unsafe fn dacl_mask_allows(
    p_dacl: *mut ACL,
    psids: &[*mut c_void],
    desired_mask: u32,
    require_all_bits: bool,
) -> bool {
    dacl_mask_allows_with_scope(
        p_dacl,
        psids,
        desired_mask,
        require_all_bits,
        AceScope::Effective,
    )
}

#[derive(Clone, Copy)]
enum AceScope {
    Effective,
    Explicit,
}

unsafe fn dacl_mask_allows_with_scope(
    p_dacl: *mut ACL,
    psids: &[*mut c_void],
    desired_mask: u32,
    require_all_bits: bool,
    scope: AceScope,
) -> bool {
    if p_dacl.is_null() {
        return false;
    }
    let mut info: ACL_SIZE_INFORMATION = std::mem::zeroed();
    let ok = GetAclInformation(
        p_dacl as *const ACL,
        &mut info as *mut _ as *mut c_void,
        std::mem::size_of::<ACL_SIZE_INFORMATION>() as u32,
        AclSizeInformation,
    );
    if ok == 0 {
        return false;
    }
    let mapping = GENERIC_MAPPING {
        GenericRead: FILE_GENERIC_READ,
        GenericWrite: FILE_GENERIC_WRITE,
        GenericExecute: FILE_GENERIC_EXECUTE,
        GenericAll: FILE_ALL_ACCESS,
    };
    for i in 0..(info.AceCount as usize) {
        let mut p_ace: *mut c_void = std::ptr::null_mut();
        if GetAce(p_dacl as *const ACL, i as u32, &mut p_ace) == 0 {
            continue;
        }
        let hdr = &*(p_ace as *const ACE_HEADER);
        if hdr.AceType != ACCESS_ALLOWED_ACE_TYPE {
            continue; // not ACCESS_ALLOWED
        }
        if (hdr.AceFlags & INHERIT_ONLY_ACE) != 0 {
            continue;
        }
        // SET_ACCESS cannot replace an ACE inherited from an ancestor, so it cannot make
        // an explicit-only repair converge when that inherited ACE contains stale rights.
        if matches!(scope, AceScope::Explicit) && (hdr.AceFlags & INHERITED_ACE) != 0 {
            continue;
        }
        let base = p_ace as usize;
        let sid_ptr =
            (base + std::mem::size_of::<ACE_HEADER>() + std::mem::size_of::<u32>()) as *mut c_void;
        let mut matched = false;
        for sid in psids {
            if EqualSid(sid_ptr, *sid) != 0 {
                matched = true;
                break;
            }
        }
        if !matched {
            continue;
        }
        let ace = &*(p_ace as *const ACCESS_ALLOWED_ACE);
        let mut mask = ace.Mask;
        MapGenericMask(&mut mask, &mapping);
        if (require_all_bits && (mask & desired_mask) == desired_mask)
            || (!require_all_bits && (mask & desired_mask) != 0)
        {
            return true;
        }
    }
    false
}

/// Path-based wrapper around the mask check (single DACL fetch).
pub fn path_mask_allows(
    path: &Path,
    psids: &[*mut c_void],
    desired_mask: u32,
    require_all_bits: bool,
) -> Result<bool> {
    path_mask_allows_with_scope(
        path,
        psids,
        desired_mask,
        require_all_bits,
        AceScope::Effective,
    )
}

fn path_mask_allows_with_scope(
    path: &Path,
    psids: &[*mut c_void],
    desired_mask: u32,
    require_all_bits: bool,
    scope: AceScope,
) -> Result<bool> {
    unsafe {
        let (p_dacl, sd) = fetch_dacl_handle(path)?;
        let has = dacl_mask_allows_with_scope(p_dacl, psids, desired_mask, require_all_bits, scope);
        if !sd.is_null() {
            LocalFree(sd as HLOCAL);
        }
        Ok(has)
    }
}

pub unsafe fn dacl_has_write_allow_for_sid(p_dacl: *mut ACL, psid: *mut c_void) -> bool {
    if p_dacl.is_null() {
        return false;
    }
    let mut info: ACL_SIZE_INFORMATION = std::mem::zeroed();
    let ok = GetAclInformation(
        p_dacl as *const ACL,
        &mut info as *mut _ as *mut c_void,
        std::mem::size_of::<ACL_SIZE_INFORMATION>() as u32,
        AclSizeInformation,
    );
    if ok == 0 {
        return false;
    }
    let count = info.AceCount as usize;
    for i in 0..count {
        let mut p_ace: *mut c_void = std::ptr::null_mut();
        if GetAce(p_dacl as *const ACL, i as u32, &mut p_ace) == 0 {
            continue;
        }
        let hdr = &*(p_ace as *const ACE_HEADER);
        if hdr.AceType != ACCESS_ALLOWED_ACE_TYPE {
            continue; // ACCESS_ALLOWED_ACE_TYPE
        }
        // Ignore ACEs that are inherit-only (do not apply to the current object)
        if (hdr.AceFlags & INHERIT_ONLY_ACE) != 0 {
            continue;
        }
        let ace = &*(p_ace as *const ACCESS_ALLOWED_ACE);
        let mask = ace.Mask;
        let base = p_ace as usize;
        let sid_ptr =
            (base + std::mem::size_of::<ACE_HEADER>() + std::mem::size_of::<u32>()) as *mut c_void;
        let eq = EqualSid(sid_ptr, psid);
        if eq != 0 && (mask & FILE_GENERIC_WRITE) != 0 {
            return true;
        }
    }
    false
}

pub unsafe fn dacl_has_write_deny_for_sid(p_dacl: *mut ACL, psid: *mut c_void) -> bool {
    if p_dacl.is_null() {
        return false;
    }
    let mut info: ACL_SIZE_INFORMATION = std::mem::zeroed();
    let ok = GetAclInformation(
        p_dacl as *const ACL,
        &mut info as *mut _ as *mut c_void,
        std::mem::size_of::<ACL_SIZE_INFORMATION>() as u32,
        AclSizeInformation,
    );
    if ok == 0 {
        return false;
    }
    let deny_write_mask = FILE_GENERIC_WRITE
        | FILE_WRITE_DATA
        | FILE_APPEND_DATA
        | FILE_WRITE_EA
        | FILE_WRITE_ATTRIBUTES
        | GENERIC_WRITE_MASK
        | DELETE
        | FILE_DELETE_CHILD;
    for i in 0..info.AceCount {
        let mut p_ace: *mut c_void = std::ptr::null_mut();
        if GetAce(p_dacl as *const ACL, i, &mut p_ace) == 0 {
            continue;
        }
        let hdr = &*(p_ace as *const ACE_HEADER);
        if hdr.AceType != ACCESS_DENIED_ACE_TYPE {
            continue; // ACCESS_DENIED_ACE_TYPE
        }
        if (hdr.AceFlags & INHERIT_ONLY_ACE) != 0 {
            continue;
        }
        let ace = &*(p_ace as *const ACCESS_DENIED_ACE);
        let base = p_ace as usize;
        let sid_ptr =
            (base + std::mem::size_of::<ACE_HEADER>() + std::mem::size_of::<u32>()) as *mut c_void;
        if EqualSid(sid_ptr, psid) != 0 && (ace.Mask & deny_write_mask) != 0 {
            return true;
        }
    }
    false
}

/// True when the DACL has an explicit (not inherited) entry denying `psid`
/// read access to the object itself.
unsafe fn dacl_has_explicit_read_deny_for_sid(p_dacl: *mut ACL, psid: *mut c_void) -> bool {
    if p_dacl.is_null() {
        return false;
    }
    let mut info: ACL_SIZE_INFORMATION = std::mem::zeroed();
    if GetAclInformation(
        p_dacl as *const ACL,
        &mut info as *mut _ as *mut c_void,
        std::mem::size_of::<ACL_SIZE_INFORMATION>() as u32,
        AclSizeInformation,
    ) == 0
    {
        return false;
    }
    for i in 0..info.AceCount {
        let mut p_ace: *mut c_void = std::ptr::null_mut();
        if GetAce(p_dacl as *const ACL, i, &mut p_ace) == 0 {
            continue;
        }
        let hdr = &*(p_ace as *const ACE_HEADER);
        if hdr.AceType != ACCESS_DENIED_ACE_TYPE
            || (hdr.AceFlags & (INHERIT_ONLY_ACE | INHERITED_ACE)) != 0
        {
            continue;
        }
        let ace = &*(p_ace as *const ACCESS_DENIED_ACE);
        let sid = (p_ace as usize + std::mem::size_of::<ACE_HEADER>() + std::mem::size_of::<u32>())
            as *mut c_void;
        if EqualSid(sid, psid) != 0 && (ace.Mask & FILE_GENERIC_READ) == FILE_GENERIC_READ {
            return true;
        }
    }
    false
}

/// PF-27-S07: makes sure `path` has its own explicit read deny for `psid`
/// (an inherited one does not count). Returns whether it is present after.
///
/// # Safety
/// Caller must ensure `psid` points to a valid SID and `path` exists.
pub unsafe fn ensure_explicit_deny_read_ace(path: &Path, psid: *mut c_void) -> Result<bool> {
    add_deny_ace(path, psid, DenyAceKind::ReadExplicit)?;
    has_explicit_deny_read_ace(path, psid)
}

/// PF-27-S07: whether `path` has its own explicit read deny for `psid`.
///
/// # Safety
/// Caller must ensure `psid` points to a valid SID and `path` exists.
pub unsafe fn has_explicit_deny_read_ace(path: &Path, psid: *mut c_void) -> Result<bool> {
    let (p_dacl, p_sd) = fetch_dacl_handle(path)?;
    let present = dacl_has_explicit_read_deny_for_sid(p_dacl, psid);
    if !p_sd.is_null() {
        LocalFree(p_sd as HLOCAL);
    }
    Ok(present)
}

/// PF-27-S07: the number of hard links to an open file.
pub fn file_link_count(file: &std::fs::File) -> std::io::Result<u32> {
    use std::os::windows::io::AsRawHandle as _;
    // SAFETY: zeroed out-structure; the handle is open for the call.
    let mut info: BY_HANDLE_FILE_INFORMATION = unsafe { std::mem::zeroed() };
    if unsafe { GetFileInformationByHandle(file.as_raw_handle() as _, &mut info) } == 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(info.nNumberOfLinks)
}

/// True when the DACL already has an inherit-only, files-only read deny for
/// `psid` (see [`add_deny_read_ace_for_new_files`]).
unsafe fn dacl_has_new_file_read_deny_for_sid(p_dacl: *mut ACL, psid: *mut c_void) -> bool {
    if p_dacl.is_null() {
        return false;
    }
    let mut info: ACL_SIZE_INFORMATION = std::mem::zeroed();
    let ok = GetAclInformation(
        p_dacl as *const ACL,
        &mut info as *mut _ as *mut c_void,
        std::mem::size_of::<ACL_SIZE_INFORMATION>() as u32,
        AclSizeInformation,
    );
    if ok == 0 {
        return false;
    }
    let wanted = OBJECT_INHERIT_ACE | u32::from(INHERIT_ONLY_ACE) | NO_PROPAGATE_INHERIT_ACE;
    for i in 0..info.AceCount {
        let mut p_ace: *mut c_void = std::ptr::null_mut();
        if GetAce(p_dacl as *const ACL, i, &mut p_ace) == 0 {
            continue;
        }
        let hdr = &*(p_ace as *const ACE_HEADER);
        if hdr.AceType != ACCESS_DENIED_ACE_TYPE || (u32::from(hdr.AceFlags) & wanted) != wanted {
            continue;
        }
        let ace = &*(p_ace as *const ACCESS_DENIED_ACE);
        let base = p_ace as usize;
        let sid_ptr =
            (base + std::mem::size_of::<ACE_HEADER>() + std::mem::size_of::<u32>()) as *mut c_void;
        if EqualSid(sid_ptr, psid) != 0 && (ace.Mask & FILE_GENERIC_READ) != 0 {
            return true;
        }
    }
    false
}

// Grant DELETE on each inheriting descendant instead of FILE_DELETE_CHILD on
// its parent. A parent delete-child grant would bypass a direct deny-write ACE
// on protected children such as `.git` or an explicit read-only subpath.
// WRITE_RESTRICTED tokens do not restrict FILE_DELETE_CHILD itself, so a parent
// whose DACL grants it to the user still opens that route; see
// `add_deny_delete_child_ace` and #158.
const WRITE_ALLOW_MASK: u32 =
    FILE_GENERIC_READ | FILE_GENERIC_WRITE | FILE_GENERIC_EXECUTE | DELETE;

unsafe fn dacl_allow_mask_needs_refresh(
    p_dacl: *mut ACL,
    psid: *mut c_void,
    allow_mask: u32,
    disallow_mask: u32,
) -> bool {
    !dacl_mask_allows(p_dacl, &[psid], allow_mask, /*require_all_bits*/ true)
        || dacl_mask_allows_with_scope(
            p_dacl,
            &[psid],
            disallow_mask,
            /*require_all_bits*/ false,
            AceScope::Explicit,
        )
}

/// Returns whether any provided SID needs its writable-root allow ACE refreshed.
pub fn path_write_aces_need_refresh(path: &Path, psids: &[*mut c_void]) -> Result<bool> {
    unsafe {
        let (p_dacl, p_sd) = fetch_dacl_handle(path)?;
        let needs_refresh = psids.iter().any(|psid| {
            dacl_allow_mask_needs_refresh(p_dacl, *psid, WRITE_ALLOW_MASK, FILE_DELETE_CHILD)
        });
        if !p_sd.is_null() {
            LocalFree(p_sd as HLOCAL);
        }
        Ok(needs_refresh)
    }
}

unsafe fn ensure_allow_mask_aces_with_inheritance_impl(
    path: &Path,
    sids: &[*mut c_void],
    allow_mask: u32,
    disallow_mask: u32,
    inheritance: u32,
) -> Result<bool> {
    let (p_dacl, p_sd) = fetch_dacl_handle(path)?;
    let mut entries: Vec<EXPLICIT_ACCESS_W> = Vec::new();
    for sid in sids {
        if !dacl_allow_mask_needs_refresh(p_dacl, *sid, allow_mask, disallow_mask) {
            continue;
        }
        entries.push(EXPLICIT_ACCESS_W {
            grfAccessPermissions: allow_mask,
            grfAccessMode: 2, // SET_ACCESS
            grfInheritance: inheritance,
            Trustee: TRUSTEE_W {
                pMultipleTrustee: std::ptr::null_mut(),
                MultipleTrusteeOperation: 0,
                TrusteeForm: TRUSTEE_IS_SID,
                TrusteeType: TRUSTEE_IS_UNKNOWN,
                ptstrName: *sid as *mut u16,
            },
        });
    }
    let mut added = false;
    if !entries.is_empty() {
        let mut p_new_dacl: *mut ACL = std::ptr::null_mut();
        let code2 = SetEntriesInAclW(
            entries.len() as u32,
            entries.as_ptr(),
            p_dacl,
            &mut p_new_dacl,
        );
        if code2 == ERROR_SUCCESS {
            let code3 = SetNamedSecurityInfoW(
                to_wide(path).as_ptr() as *mut u16,
                1,
                DACL_SECURITY_INFORMATION,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                p_new_dacl,
                std::ptr::null_mut(),
            );
            if code3 == ERROR_SUCCESS {
                added = true;
                if !p_new_dacl.is_null() {
                    LocalFree(p_new_dacl as HLOCAL);
                }
            } else {
                if !p_new_dacl.is_null() {
                    LocalFree(p_new_dacl as HLOCAL);
                }
                if !p_sd.is_null() {
                    LocalFree(p_sd as HLOCAL);
                }
                return Err(anyhow!("SetNamedSecurityInfoW failed: {code3}"));
            }
        } else {
            if !p_sd.is_null() {
                LocalFree(p_sd as HLOCAL);
            }
            return Err(anyhow!("SetEntriesInAclW failed: {code2}"));
        }
    }
    if !p_sd.is_null() {
        LocalFree(p_sd as HLOCAL);
    }
    Ok(added)
}

/// Ensure all provided SIDs have an allow ACE with the requested mask on the path.
/// Returns true if any ACE was added.
///
/// # Safety
/// Caller must pass valid SID pointers and an existing path; free the returned security descriptor with `LocalFree`.
pub unsafe fn ensure_allow_mask_aces_with_inheritance(
    path: &Path,
    sids: &[*mut c_void],
    allow_mask: u32,
    inheritance: u32,
) -> Result<bool> {
    ensure_allow_mask_aces_with_inheritance_impl(
        path,
        sids,
        allow_mask,
        /*disallow_mask*/ 0,
        inheritance,
    )
}

/// Ensure all provided SIDs have an allow ACE with the requested mask on the path.
/// Returns true if any ACE was added.
///
/// # Safety
/// Caller must pass valid SID pointers and an existing path; free the returned security descriptor with `LocalFree`.
pub unsafe fn ensure_allow_mask_aces(
    path: &Path,
    sids: &[*mut c_void],
    allow_mask: u32,
) -> Result<bool> {
    ensure_allow_mask_aces_with_inheritance(
        path,
        sids,
        allow_mask,
        CONTAINER_INHERIT_ACE | OBJECT_INHERIT_ACE,
    )
}

/// Ensure all provided SIDs have a write-capable allow ACE on the path.
/// Returns true if any ACE was added.
///
/// # Safety
/// Caller must pass valid SID pointers and an existing path; free the returned security descriptor with `LocalFree`.
pub unsafe fn ensure_allow_write_aces(path: &Path, sids: &[*mut c_void]) -> Result<bool> {
    ensure_allow_mask_aces_with_inheritance_impl(
        path,
        sids,
        WRITE_ALLOW_MASK,
        FILE_DELETE_CHILD,
        CONTAINER_INHERIT_ACE | OBJECT_INHERIT_ACE,
    )
}

/// Adds an allow ACE granting read/write/execute to the given SID on the target path.
///
/// # Safety
/// Caller must ensure `psid` points to a valid SID and `path` refers to an existing file or directory.
pub unsafe fn add_allow_ace(path: &Path, psid: *mut c_void) -> Result<bool> {
    let mut p_sd: *mut c_void = std::ptr::null_mut();
    let mut p_dacl: *mut ACL = std::ptr::null_mut();
    let code = GetNamedSecurityInfoW(
        to_wide(path).as_ptr(),
        1,
        DACL_SECURITY_INFORMATION,
        std::ptr::null_mut(),
        std::ptr::null_mut(),
        &mut p_dacl,
        std::ptr::null_mut(),
        &mut p_sd,
    );
    if code != ERROR_SUCCESS {
        return Err(anyhow!("GetNamedSecurityInfoW failed: {code}"));
    }
    // Already has write? Skip costly DACL rewrite.
    if dacl_has_write_allow_for_sid(p_dacl, psid) {
        if !p_sd.is_null() {
            LocalFree(p_sd as HLOCAL);
        }
        return Ok(false);
    }
    let mut added = false;
    // Always ensure write is present: if an allow ACE exists without write, add one with write+RX.
    let trustee = TRUSTEE_W {
        pMultipleTrustee: std::ptr::null_mut(),
        MultipleTrusteeOperation: 0,
        TrusteeForm: TRUSTEE_IS_SID,
        TrusteeType: TRUSTEE_IS_UNKNOWN,
        ptstrName: psid as *mut u16,
    };
    let mut explicit: EXPLICIT_ACCESS_W = std::mem::zeroed();
    explicit.grfAccessPermissions = FILE_GENERIC_READ | FILE_GENERIC_WRITE | FILE_GENERIC_EXECUTE;
    explicit.grfAccessMode = 2; // SET_ACCESS
    explicit.grfInheritance = CONTAINER_INHERIT_ACE | OBJECT_INHERIT_ACE;
    explicit.Trustee = trustee;
    let mut p_new_dacl: *mut ACL = std::ptr::null_mut();
    let code2 = SetEntriesInAclW(1, &explicit, p_dacl, &mut p_new_dacl);
    if code2 == ERROR_SUCCESS {
        let code3 = SetNamedSecurityInfoW(
            to_wide(path).as_ptr() as *mut u16,
            1,
            DACL_SECURITY_INFORMATION,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            p_new_dacl,
            std::ptr::null_mut(),
        );
        if code3 == ERROR_SUCCESS {
            added = !dacl_has_write_allow_for_sid(p_dacl, psid);
        }
        if !p_new_dacl.is_null() {
            LocalFree(p_new_dacl as HLOCAL);
        }
    }
    if !p_sd.is_null() {
        LocalFree(p_sd as HLOCAL);
    }
    Ok(added)
}

/// Adds a deny ACE to prevent write/append/delete for the given SID on the target path.
///
/// # Safety
/// Caller must ensure `psid` points to a valid SID and `path` refers to an existing file or directory.
pub unsafe fn add_deny_write_ace(path: &Path, psid: *mut c_void) -> Result<bool> {
    add_deny_ace(path, psid, DenyAceKind::Write)
}

/// As [`add_deny_write_ace`], but an error if the entry isn't there
/// afterwards (the DACL couldn't be written), for a protection a command
/// must not start without (D283).
///
/// # Safety
/// Caller must ensure `psid` points to a valid SID and `path` exists.
pub unsafe fn add_required_deny_write_ace(path: &Path, psid: *mut c_void) -> Result<bool> {
    let added = add_deny_ace(path, psid, DenyAceKind::Write)?;
    // `add_deny_ace` reports a failed write as "not added"; confirm instead.
    let (p_dacl, p_sd) = fetch_dacl_handle(path)?;
    let present = DenyAceKind::Write.already_present(p_dacl, psid, path.is_dir());
    if !p_sd.is_null() {
        LocalFree(p_sd as HLOCAL);
    }
    if !present {
        return Err(anyhow!(
            "could not deny the sandbox writes to {}",
            path.display()
        ));
    }
    Ok(added)
}

/// Denies `FILE_DELETE_CHILD` to `psid` on a directory, and on its future and
/// existing subdirectories when `inherit_to_subdirs` is set.
///
/// WRITE_RESTRICTED sandbox tokens do not consult restricting SIDs for
/// `FILE_DELETE_CHILD` (#158). A parent directory that grants it to one of the
/// token's normal SIDs (usually the user, through full control) lets the sandbox
/// delete or move a child even when the child denies the capability SID
/// `DELETE`, so this deny must name a SID from the token's normal groups.
///
/// # Safety
/// Caller must ensure `psid` points to a valid SID and `path` is an existing directory.
pub unsafe fn add_deny_delete_child_ace(
    path: &Path,
    psid: *mut c_void,
    inherit_to_subdirs: bool,
) -> Result<bool> {
    // D285: without the full-tree walk, same result.
    delete_child::set_delete_child_deny(path, psid, inherit_to_subdirs, /*add*/ true)
        .map(|written| written != delete_child::Written::Nothing)
}

/// Whether `path`'s DACL itself (not an inherited copy) denies `psid`
/// `FILE_DELETE_CHILD` as [`add_deny_delete_child_ace`] would leave it (so
/// that call would add nothing).
///
/// # Safety
/// Caller must ensure `psid` points to a valid SID.
pub unsafe fn has_deny_delete_child_ace(
    path: &Path,
    psid: *mut c_void,
    inherit_to_subdirs: bool,
) -> Result<bool> {
    let (p_dacl, p_sd) = fetch_dacl_handle(path)?;
    let present = dacl_has_deny_for_sid(
        p_dacl,
        psid,
        FILE_DELETE_CHILD,
        inherit_to_subdirs,
        /*explicit_only*/ true,
    );
    if !p_sd.is_null() {
        LocalFree(p_sd as HLOCAL);
    }
    Ok(present)
}

/// #1506 item 1 (D261): removes the explicit entry denying `psid` exactly
/// `FILE_DELETE_CHILD`, with the inheritance [`add_deny_delete_child_ace`]
/// gives it, and the copies subdirectories inherited. An entry with other
/// rights merged in is left as it is. Returns whether an entry was removed.
///
/// # Safety
/// Caller must ensure `psid` points to a valid SID and `path` exists.
pub unsafe fn remove_deny_delete_child_ace(
    path: &Path,
    psid: *mut c_void,
    inherit_to_subdirs: bool,
) -> Result<bool> {
    // D285: without the full-tree walk, same result.
    delete_child::set_delete_child_deny(path, psid, inherit_to_subdirs, /*add*/ false)
        .map(|written| written != delete_child::Written::Nothing)
}

unsafe fn is_exact_delete_child_deny(
    ace: *const c_void,
    psid: *mut c_void,
    inherit_to_subdirs: bool,
) -> bool {
    let hdr = &*(ace as *const ACE_HEADER);
    let flags = DenyAceKind::DeleteChild { inherit_to_subdirs }.inheritance();
    if hdr.AceType != ACCESS_DENIED_ACE_TYPE || u32::from(hdr.AceFlags) != flags {
        return false;
    }
    let sid = (ace as usize + std::mem::size_of::<ACE_HEADER>() + std::mem::size_of::<u32>())
        as *mut c_void;
    (*(ace as *const ACCESS_DENIED_ACE)).Mask == FILE_DELETE_CHILD && EqualSid(sid, psid) != 0
}

/// Denies `psid` `DELETE` on `path` itself (not inherited), so a sandbox
/// whose token carries `psid` can't delete, rename or move it through its own
/// `DELETE` right (#1506 item 3). Returns whether it added the entry; fails
/// if the entry isn't there afterwards (the DACL couldn't be written, #1542
/// item 3), since D263 must not start a command without it.
///
/// # Safety
/// Caller must ensure `psid` points to a valid SID and `path` exists.
pub unsafe fn add_deny_delete_ace(path: &Path, psid: *mut c_void) -> Result<bool> {
    let added = add_deny_ace(path, psid, DenyAceKind::Delete)?;
    // `add_deny_ace` reports a failed write as "not added"; confirm instead.
    let (p_dacl, p_sd) = fetch_dacl_handle(path)?;
    let present = DenyAceKind::Delete.already_present(p_dacl, psid, path.is_dir());
    if !p_sd.is_null() {
        LocalFree(p_sd as HLOCAL);
    }
    if !present {
        return Err(anyhow!(
            "could not deny the sandbox DELETE on {}",
            path.display()
        ));
    }
    Ok(added)
}

/// PF-27-S06: denies `psid` reads of every file created directly in `path`
/// from now on (inherit-only, files only, not propagated below the first
/// level), so a file replaced by rename does not lose the deny. Existing
/// files get the inherited entry as well; subdirectories are unaffected.
///
/// # Safety
/// Caller must ensure `psid` points to a valid SID and `path` is a directory.
pub unsafe fn add_deny_read_ace_for_new_files(path: &Path, psid: *mut c_void) -> Result<bool> {
    add_deny_ace(path, psid, DenyAceKind::ReadNewFiles)?;
    // `add_deny_ace` reports a failed write as "not added"; confirm instead.
    has_deny_read_ace_for_new_files(path, psid)
}

/// Whether `path` has the entry [`add_deny_read_ace_for_new_files`] adds
/// for `psid`.
///
/// # Safety
/// Caller must ensure `psid` points to a valid SID.
pub unsafe fn has_deny_read_ace_for_new_files(path: &Path, psid: *mut c_void) -> Result<bool> {
    let mut p_sd: *mut c_void = std::ptr::null_mut();
    let mut p_dacl: *mut ACL = std::ptr::null_mut();
    let code = GetNamedSecurityInfoW(
        to_wide(path).as_ptr(),
        1,
        DACL_SECURITY_INFORMATION,
        std::ptr::null_mut(),
        std::ptr::null_mut(),
        &mut p_dacl,
        std::ptr::null_mut(),
        &mut p_sd,
    );
    if code != ERROR_SUCCESS {
        return Err(anyhow!("GetNamedSecurityInfoW failed: {code}"));
    }
    let present = dacl_has_new_file_read_deny_for_sid(p_dacl, psid);
    if !p_sd.is_null() {
        LocalFree(p_sd as HLOCAL);
    }
    Ok(present)
}

/// True when `ace` is exactly the entry [`add_deny_read_ace_for_new_files`]
/// adds for `psid` (any SID when `None`): an explicit deny, inherit-only for
/// files one level down, whose mask maps to file read and nothing else.
unsafe fn is_new_file_read_deny(ace: *const c_void, psid: Option<*mut c_void>) -> bool {
    let hdr = &*(ace as *const ACE_HEADER);
    let flags = OBJECT_INHERIT_ACE | u32::from(INHERIT_ONLY_ACE) | NO_PROPAGATE_INHERIT_ACE;
    if hdr.AceType != ACCESS_DENIED_ACE_TYPE || u32::from(hdr.AceFlags) != flags {
        return false;
    }
    let mut mask = (*(ace as *const ACCESS_DENIED_ACE)).Mask;
    let mapping = GENERIC_MAPPING {
        GenericRead: FILE_GENERIC_READ,
        GenericWrite: FILE_GENERIC_WRITE,
        GenericExecute: FILE_GENERIC_EXECUTE,
        GenericAll: FILE_ALL_ACCESS,
    };
    MapGenericMask(&mut mask, &mapping);
    let sid = (ace as usize + std::mem::size_of::<ACE_HEADER>() + std::mem::size_of::<u32>())
        as *mut c_void;
    mask == FILE_GENERIC_READ && psid.is_none_or(|psid| EqualSid(sid, psid) != 0)
}

/// PF-27-S07: whether `path` has exactly the entry
/// [`add_deny_read_ace_for_new_files`] adds, for `psid` or (`None`) for any
/// SID; what [`remove_deny_read_ace_for_new_files`] would remove.
///
/// # Safety
/// Caller must ensure `psid`, if given, points to a valid SID.
pub unsafe fn has_exact_deny_read_ace_for_new_files(
    path: &Path,
    psid: Option<*mut c_void>,
) -> Result<bool> {
    let mut p_sd: *mut c_void = std::ptr::null_mut();
    let mut p_dacl: *mut ACL = std::ptr::null_mut();
    let code = GetNamedSecurityInfoW(
        to_wide(path).as_ptr(),
        1,
        DACL_SECURITY_INFORMATION,
        std::ptr::null_mut(),
        std::ptr::null_mut(),
        &mut p_dacl,
        std::ptr::null_mut(),
        &mut p_sd,
    );
    if code != ERROR_SUCCESS {
        return Err(anyhow!("GetNamedSecurityInfoW failed: {code}"));
    }
    let mut found = false;
    let mut info: ACL_SIZE_INFORMATION = std::mem::zeroed();
    if !p_dacl.is_null()
        && GetAclInformation(
            p_dacl as *const ACL,
            &mut info as *mut _ as *mut c_void,
            std::mem::size_of::<ACL_SIZE_INFORMATION>() as u32,
            AclSizeInformation,
        ) != 0
    {
        for i in 0..info.AceCount {
            let mut p_ace: *mut c_void = std::ptr::null_mut();
            if GetAce(p_dacl as *const ACL, i, &mut p_ace) != 0
                && is_new_file_read_deny(p_ace, psid)
            {
                found = true;
                break;
            }
        }
    }
    if !p_sd.is_null() {
        LocalFree(p_sd as HLOCAL);
    }
    Ok(found)
}

/// PF-27-S07: removes exactly the entry [`add_deny_read_ace_for_new_files`]
/// added for `psid` on `path`, and with it the copies files directly in
/// `path` inherited. Every other entry, and whether the DACL is protected,
/// stays as it was. Returns whether an entry was removed.
///
/// # Safety
/// Caller must ensure `psid` points to a valid SID and `path` is a directory.
pub unsafe fn remove_deny_read_ace_for_new_files(path: &Path, psid: *mut c_void) -> Result<bool> {
    remove_matching_aces(path, |_, ace| is_new_file_read_deny(ace, Some(psid)))
}

/// #304: the file-system object a deny-read entry was added to: where it
/// was (its resolved path) and which object it was (volume serial number and
/// 128-bit file ID, unique on NTFS and ReFS), so a removal can tell when
/// another object has taken its place.
#[derive(Clone, Debug, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
pub struct DenyReadObject {
    pub path: PathBuf,
    pub volume: u64,
    pub file_id: [u8; 16],
}

impl DenyReadObject {
    pub fn identity(&self) -> (u64, [u8; 16]) {
        (self.volume, self.file_id)
    }
}

/// [`add_deny_read_ace`] on the object `path` resolves to (following links,
/// as by name), through one handle. Returns whether the entry was added and
/// which object has it.
///
/// # Safety
/// Caller must ensure `psid` points to a valid SID and `path` exists.
pub unsafe fn add_deny_read_ace_to_object(
    path: &Path,
    psid: *mut c_void,
) -> Result<(bool, DenyReadObject)> {
    let raw = CreateFileW(
        to_wide(path).as_ptr(),
        READ_CONTROL | WRITE_DAC,
        FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
        std::ptr::null_mut(),
        OPEN_EXISTING,
        FILE_FLAG_BACKUP_SEMANTICS,
        0,
    );
    if raw == INVALID_HANDLE_VALUE {
        return Err(std::io::Error::last_os_error())
            .with_context(|| format!("open {}", path.display()));
    }
    let handle = OwnedFileHandle(raw);
    let info = file_info(raw).with_context(|| format!("inspect {}", path.display()))?;
    let (volume, file_id) =
        file_identity(raw).with_context(|| format!("identify {}", path.display()))?;
    let object = DenyReadObject {
        path: PathBuf::from(
            final_path(raw).with_context(|| format!("resolve {}", path.display()))?,
        ),
        volume,
        file_id,
    };
    let is_dir = info.dwFileAttributes & FILE_ATTRIBUTE_DIRECTORY != 0;
    let (p_dacl, p_sd) = security_info(handle.0)?;
    let result = (|| {
        if dacl_has_deny_read_entry(p_dacl, psid, is_dir) {
            return Ok(false);
        }
        let mut explicit: EXPLICIT_ACCESS_W = std::mem::zeroed();
        explicit.grfAccessPermissions = DenyAceKind::Read.mask();
        explicit.grfAccessMode = DENY_ACCESS;
        explicit.grfInheritance = DenyAceKind::Read.inheritance();
        explicit.Trustee = TRUSTEE_W {
            pMultipleTrustee: std::ptr::null_mut(),
            MultipleTrusteeOperation: 0,
            TrusteeForm: TRUSTEE_IS_SID,
            TrusteeType: TRUSTEE_IS_UNKNOWN,
            ptstrName: psid as *mut u16,
        };
        let mut p_new_dacl: *mut ACL = std::ptr::null_mut();
        let code = SetEntriesInAclW(1, &explicit, p_dacl, &mut p_new_dacl);
        if code != ERROR_SUCCESS {
            return Err(anyhow!("SetEntriesInAclW failed: {code}"));
        }
        let code = SetSecurityInfo(
            handle.0,
            1, // SE_FILE_OBJECT
            DACL_SECURITY_INFORMATION | dacl_protection(p_sd)?,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            p_new_dacl,
            std::ptr::null_mut(),
        );
        LocalFree(p_new_dacl as HLOCAL);
        if code != ERROR_SUCCESS {
            return Err(anyhow!("SetSecurityInfo failed: {code}"));
        }
        Ok(true)
    })();
    LocalFree(p_sd as HLOCAL);
    Ok((result?, object))
}

/// What [`remove_deny_read_ace`] found.
#[derive(Debug, PartialEq, Eq)]
pub enum DenyReadRemoval {
    Removed,
    /// The object has no such entry (left as it is).
    NoEntry,
    /// Another object is at the path now (left as it is).
    OtherObject,
}

/// #304: removes exactly the entry [`add_deny_read_ace`] adds for `psid`
/// from `object`, and with it the copies inherited below it. Every other
/// entry (allows, other denies for `psid`, entries for other SIDs, inherited
/// entries) and whether the DACL is protected stay as they were.
/// `REVOKE_ACCESS` cannot do this: it removes only allow entries.
///
/// The object at `object.path` is opened without following a link and must
/// be the one the entry was added to (same volume and file index), so a
/// junction or hard link a sandboxed command left at the path (or a junction
/// above it), or a denied object it renamed onto the path, is left alone.
///
/// # Safety
/// Caller must ensure `psid` points to a valid SID.
pub unsafe fn remove_deny_read_ace(
    object: &DenyReadObject,
    psid: *mut c_void,
) -> Result<DenyReadRemoval> {
    let handle = open_exact(&object.path, READ_CONTROL | WRITE_DAC)?;
    if (handle.volume, handle.file_id) != object.identity() {
        return Ok(DenyReadRemoval::OtherObject);
    }
    let is_dir = handle.is_dir;
    let (p_dacl, p_sd) = security_info(handle.raw)?;
    let result = remove_aces_from(DaclTarget::Handle(handle.raw), p_sd, p_dacl, |dacl, ace| {
        match deny_read_part(ace, psid) {
            Some(DenyReadPart::Whole | DenyReadPart::Inherited) => true,
            // On a directory, alone it is not that entry.
            Some(DenyReadPart::Effective) => {
                !is_dir || dacl_has_deny_read_part(dacl, psid, DenyReadPart::Inherited)
            }
            None => false,
        }
    });
    LocalFree(p_sd as HLOCAL);
    Ok(if result? {
        DenyReadRemoval::Removed
    } else {
        DenyReadRemoval::NoEntry
    })
}

/// Whether the DACL has the whole entry [`add_deny_read_ace`] adds for
/// `psid`, in any of the forms Windows stores it in. Inherited entries do not
/// count: they go when their parent's entry is removed.
unsafe fn dacl_has_deny_read_entry(p_dacl: *mut ACL, psid: *mut c_void, is_dir: bool) -> bool {
    let has = |part| dacl_has_deny_read_part(p_dacl, psid, part);
    has(DenyReadPart::Whole)
        || (has(DenyReadPart::Effective) && (!is_dir || has(DenyReadPart::Inherited)))
}

/// A file handle closed on drop.
struct OwnedFileHandle(HANDLE);

impl Drop for OwnedFileHandle {
    fn drop(&mut self) {
        // SAFETY: owned by this value.
        unsafe { CloseHandle(self.0) };
    }
}

/// A handle opened by [`open_exact`].
struct ExactHandle {
    raw: HANDLE,
    is_dir: bool,
    volume: u64,
    file_id: [u8; 16],
}

impl Drop for ExactHandle {
    fn drop(&mut self) {
        // SAFETY: opened by `open_exact` and owned here.
        unsafe { CloseHandle(self.raw) };
    }
}

/// The volume serial number and 128-bit file ID of the file `handle` is open
/// to (`FileIdInfo`; the 64-bit index is not unique on ReFS).
pub(crate) unsafe fn file_identity(handle: HANDLE) -> std::io::Result<(u64, [u8; 16])> {
    let mut info: FILE_ID_INFO = std::mem::zeroed();
    if GetFileInformationByHandleEx(
        handle,
        FileIdInfo,
        std::ptr::from_mut(&mut info).cast(),
        std::mem::size_of::<FILE_ID_INFO>() as u32,
    ) == 0
    {
        return Err(std::io::Error::last_os_error());
    }
    Ok((info.VolumeSerialNumber, info.FileId.Identifier))
}

/// D420 (#1775): how many names the file at `path` has (hard links), the
/// file itself if it is a link; `None` when it can't be read.
pub(crate) fn link_count(path: &Path) -> Option<u32> {
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
    unsafe { file_info(file.as_raw_handle() as _) }
        .ok()
        .map(|info| info.nNumberOfLinks)
}

unsafe fn file_info(handle: HANDLE) -> std::io::Result<BY_HANDLE_FILE_INFORMATION> {
    let mut info: BY_HANDLE_FILE_INFORMATION = std::mem::zeroed();
    if GetFileInformationByHandle(handle, &mut info) == 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(info)
}

/// The path `handle` was opened at, after links, without the `\\?\` prefix.
unsafe fn final_path(handle: HANDLE) -> std::io::Result<String> {
    let mut buffer = vec![0_u16; 1024];
    loop {
        let len =
            GetFinalPathNameByHandleW(handle, buffer.as_mut_ptr(), buffer.len() as u32, 0) as usize;
        if len == 0 {
            return Err(std::io::Error::last_os_error());
        }
        if len < buffer.len() {
            let path = String::from_utf16_lossy(&buffer[..len]);
            return Ok(if let Some(unc) = path.strip_prefix(r"\\?\UNC\") {
                format!(r"\\{unc}")
            } else {
                path.strip_prefix(r"\\?\")
                    .map(str::to_string)
                    .unwrap_or(path)
            });
        }
        buffer.resize(len + 1, 0);
    }
}

/// The DACL of the file `handle` is open to, and its descriptor (free it
/// with `LocalFree`).
unsafe fn security_info(handle: HANDLE) -> Result<(*mut ACL, *mut c_void)> {
    let mut p_sd: *mut c_void = std::ptr::null_mut();
    let mut p_dacl: *mut ACL = std::ptr::null_mut();
    let code = GetSecurityInfo(
        handle,
        1, // SE_FILE_OBJECT
        DACL_SECURITY_INFORMATION,
        std::ptr::null_mut(),
        std::ptr::null_mut(),
        &mut p_dacl,
        std::ptr::null_mut(),
        &mut p_sd,
    );
    if code != ERROR_SUCCESS {
        return Err(anyhow!("GetSecurityInfo failed: {code}"));
    }
    Ok((p_dacl, p_sd))
}

/// The flag that keeps a DACL's protection as it is in `p_sd` when set.
unsafe fn dacl_protection(p_sd: *mut c_void) -> Result<u32> {
    let mut control: u16 = 0;
    let mut revision: u32 = 0;
    if GetSecurityDescriptorControl(p_sd, &mut control, &mut revision) == 0 {
        return Err(anyhow!(
            "GetSecurityDescriptorControl failed: {}",
            GetLastError()
        ));
    }
    Ok(if control & SE_DACL_PROTECTED != 0 {
        PROTECTED_DACL_SECURITY_INFORMATION
    } else {
        UNPROTECTED_DACL_SECURITY_INFORMATION
    })
}

/// Opens the object `path` names with `access`, failing if `path` is a link
/// (a symbolic link or junction), resolves elsewhere through one above it, or
/// is a file with other hard links.
unsafe fn open_exact(path: &Path, access: u32) -> Result<ExactHandle> {
    let raw = CreateFileW(
        to_wide(path).as_ptr(),
        access,
        FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
        std::ptr::null_mut(),
        OPEN_EXISTING,
        FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT,
        0,
    );
    if raw == INVALID_HANDLE_VALUE {
        return Err(std::io::Error::last_os_error())
            .with_context(|| format!("open {}", path.display()));
    }
    let mut handle = ExactHandle {
        raw,
        is_dir: false,
        volume: 0,
        file_id: [0; 16],
    };
    let info = file_info(handle.raw).with_context(|| format!("inspect {}", path.display()))?;
    if info.dwFileAttributes & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
        return Err(anyhow!("{} is a link", path.display()));
    }
    // A hard link shares its DACL with the file's other names, and the name
    // check below cannot tell.
    let is_dir = info.dwFileAttributes & FILE_ATTRIBUTE_DIRECTORY != 0;
    if !is_dir && info.nNumberOfLinks != 1 {
        return Err(anyhow!("{} has other hard links", path.display()));
    }
    let resolved = final_path(handle.raw).with_context(|| format!("resolve {}", path.display()))?;
    if comparable_path(&resolved) != comparable_path(&path.to_string_lossy()) {
        return Err(anyhow!("{} resolves to {resolved}", path.display()));
    }
    handle.is_dir = is_dir;
    (handle.volume, handle.file_id) =
        file_identity(handle.raw).with_context(|| format!("identify {}", path.display()))?;
    Ok(handle)
}

/// A Windows path in a form two names for the same location share (apart
/// from short names and links, which [`open_exact`] refuses).
fn comparable_path(path: &str) -> String {
    let path = path.replace('/', "\\");
    let path = if let Some(unc) = path.strip_prefix("\\\\?\\UNC\\") {
        format!("\\\\{unc}")
    } else {
        path.strip_prefix("\\\\?\\")
            .map(str::to_string)
            .unwrap_or(path)
    };
    path.trim_end_matches('\\').to_lowercase()
}

/// How Windows stores the entry [`add_deny_read_ace`] adds: as given, or
/// (measured on Windows 11) split into an entry for the object itself, with
/// the generic right mapped, and an inherit-only one for its children.
#[derive(Clone, Copy, PartialEq, Eq)]
enum DenyReadPart {
    Whole,
    Effective,
    Inherited,
}

/// Which part of the [`add_deny_read_ace`] entry for `psid` `ace` is: an
/// explicit deny whose mask maps to file read and nothing else.
unsafe fn deny_read_part(ace: *const c_void, psid: *mut c_void) -> Option<DenyReadPart> {
    let hdr = &*(ace as *const ACE_HEADER);
    let flags = u32::from(hdr.AceFlags);
    if hdr.AceType != ACCESS_DENIED_ACE_TYPE || flags & u32::from(INHERITED_ACE) != 0 {
        return None;
    }
    let mut mask = (*(ace as *const ACCESS_DENIED_ACE)).Mask;
    MapGenericMask(&mut mask, &FILE_MAPPING);
    let sid = (ace as usize + std::mem::size_of::<ACE_HEADER>() + std::mem::size_of::<u32>())
        as *mut c_void;
    if mask != FILE_GENERIC_READ || EqualSid(sid, psid) == 0 {
        return None;
    }
    let inheritance = DenyAceKind::Read.inheritance();
    match flags & INHERITANCE_FLAGS {
        0 => Some(DenyReadPart::Effective),
        f if f == inheritance => Some(DenyReadPart::Whole),
        f if f == inheritance | u32::from(INHERIT_ONLY_ACE) => Some(DenyReadPart::Inherited),
        _ => None,
    }
}

unsafe fn dacl_has_deny_read_part(p_dacl: *mut ACL, psid: *mut c_void, part: DenyReadPart) -> bool {
    let mut info: ACL_SIZE_INFORMATION = std::mem::zeroed();
    if p_dacl.is_null()
        || GetAclInformation(
            p_dacl as *const ACL,
            &mut info as *mut _ as *mut c_void,
            std::mem::size_of::<ACL_SIZE_INFORMATION>() as u32,
            AclSizeInformation,
        ) == 0
    {
        return false;
    }
    (0..info.AceCount).any(|i| {
        let mut p_ace: *mut c_void = std::ptr::null_mut();
        GetAce(p_dacl as *const ACL, i, &mut p_ace) != 0
            && deny_read_part(p_ace, psid) == Some(part)
    })
}

/// Rewrites `path`'s DACL without the entries `matches` selects (keeping
/// whether it is protected); inherited copies below `path` follow. Returns
/// whether an entry was removed.
unsafe fn remove_matching_aces(
    path: &Path,
    matches: impl Fn(*mut ACL, *const c_void) -> bool,
) -> Result<bool> {
    let mut p_sd: *mut c_void = std::ptr::null_mut();
    let mut p_dacl: *mut ACL = std::ptr::null_mut();
    let code = GetNamedSecurityInfoW(
        to_wide(path).as_ptr(),
        1,
        DACL_SECURITY_INFORMATION,
        std::ptr::null_mut(),
        std::ptr::null_mut(),
        &mut p_dacl,
        std::ptr::null_mut(),
        &mut p_sd,
    );
    if code != ERROR_SUCCESS {
        return Err(anyhow!("GetNamedSecurityInfoW failed: {code}"));
    }
    let result = remove_aces_from(DaclTarget::Name(path), p_sd, p_dacl, matches);
    if !p_sd.is_null() {
        LocalFree(p_sd as HLOCAL);
    }
    result
}

/// Where [`remove_aces_from`] writes the new DACL.
enum DaclTarget<'a> {
    Name(&'a Path),
    Handle(HANDLE),
}

unsafe fn remove_aces_from(
    target: DaclTarget<'_>,
    p_sd: *mut c_void,
    p_dacl: *mut ACL,
    matches: impl Fn(*mut ACL, *const c_void) -> bool,
) -> Result<bool> {
    if p_dacl.is_null() {
        return Ok(false);
    }
    let mut info: ACL_SIZE_INFORMATION = std::mem::zeroed();
    if GetAclInformation(
        p_dacl as *const ACL,
        &mut info as *mut _ as *mut c_void,
        std::mem::size_of::<ACL_SIZE_INFORMATION>() as u32,
        AclSizeInformation,
    ) == 0
    {
        return Err(anyhow!("GetAclInformation failed: {}", GetLastError()));
    }
    let revision = u32::from((*p_dacl).AclRevision);
    let size = (*p_dacl).AclSize;
    // u32 storage keeps the ACL suitably aligned.
    let mut buffer = vec![0_u32; usize::from(size).div_ceil(4)];
    let new_dacl = buffer.as_mut_ptr() as *mut ACL;
    if InitializeAcl(new_dacl, u32::from(size), revision) == 0 {
        return Err(anyhow!("InitializeAcl failed: {}", GetLastError()));
    }
    let mut removed = false;
    for i in 0..info.AceCount {
        let mut p_ace: *mut c_void = std::ptr::null_mut();
        if GetAce(p_dacl as *const ACL, i, &mut p_ace) == 0 {
            return Err(anyhow!("GetAce failed: {}", GetLastError()));
        }
        if matches(p_dacl, p_ace) {
            removed = true;
            continue;
        }
        let ace_size = u32::from((*(p_ace as *const ACE_HEADER)).AceSize);
        if AddAce(new_dacl, revision, u32::MAX, p_ace, ace_size) == 0 {
            return Err(anyhow!("AddAce failed: {}", GetLastError()));
        }
    }
    if !removed {
        return Ok(false);
    }
    let mut control: u16 = 0;
    let mut sd_revision: u32 = 0;
    if GetSecurityDescriptorControl(p_sd, &mut control, &mut sd_revision) == 0 {
        return Err(anyhow!(
            "GetSecurityDescriptorControl failed: {}",
            GetLastError()
        ));
    }
    let protection = if control & SE_DACL_PROTECTED != 0 {
        PROTECTED_DACL_SECURITY_INFORMATION
    } else {
        UNPROTECTED_DACL_SECURITY_INFORMATION
    };
    // Either call also recomputes the children's inherited entries, dropping
    // the copies.
    let code = match target {
        DaclTarget::Name(path) => SetNamedSecurityInfoW(
            to_wide(path).as_ptr() as *mut u16,
            1,
            DACL_SECURITY_INFORMATION | protection,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            new_dacl,
            std::ptr::null_mut(),
        ),
        DaclTarget::Handle(handle) => SetSecurityInfo(
            handle,
            1, // SE_FILE_OBJECT
            DACL_SECURITY_INFORMATION | protection,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            new_dacl,
            std::ptr::null_mut(),
        ),
    };
    if code != ERROR_SUCCESS {
        return Err(anyhow!("setting the DACL failed: {code}"));
    }
    Ok(true)
}

#[derive(Clone, Copy)]
enum DenyAceKind {
    Read,
    /// Read, on the object itself only, and only an explicit entry counts as
    /// present (an inherited copy can disappear with its parent's entry).
    ReadExplicit,
    /// Read, inherited by files directly in the directory only.
    ReadNewFiles,
    Write,
    DeleteChild {
        inherit_to_subdirs: bool,
    },
    /// `DELETE` on the object itself only (#1506 item 3).
    Delete,
}

impl DenyAceKind {
    fn mask(self) -> u32 {
        match self {
            Self::DeleteChild { .. } => FILE_DELETE_CHILD,
            Self::Delete => DELETE,
            Self::Read | Self::ReadExplicit | Self::ReadNewFiles => {
                FILE_GENERIC_READ | GENERIC_READ_MASK
            }
            Self::Write => {
                FILE_GENERIC_WRITE
                    | FILE_WRITE_DATA
                    | FILE_APPEND_DATA
                    | FILE_WRITE_EA
                    | FILE_WRITE_ATTRIBUTES
                    | GENERIC_WRITE_MASK
                    | DELETE
                    | FILE_DELETE_CHILD
            }
        }
    }

    fn inheritance(self) -> u32 {
        match self {
            Self::Read | Self::Write => CONTAINER_INHERIT_ACE | OBJECT_INHERIT_ACE,
            Self::ReadExplicit | Self::Delete => 0,
            Self::ReadNewFiles => {
                OBJECT_INHERIT_ACE | u32::from(INHERIT_ONLY_ACE) | NO_PROPAGATE_INHERIT_ACE
            }
            Self::DeleteChild {
                inherit_to_subdirs: true,
            } => CONTAINER_INHERIT_ACE,
            Self::DeleteChild {
                inherit_to_subdirs: false,
            } => 0,
        }
    }

    unsafe fn already_present(self, p_dacl: *mut ACL, psid: *mut c_void, is_dir: bool) -> bool {
        match self {
            // #304: only this entry itself counts. An inherited copy goes
            // when its parent's entry is removed, and another deny (a write
            // deny shares READ_CONTROL) is not a read deny.
            Self::Read => dacl_has_deny_read_entry(p_dacl, psid, is_dir),
            Self::ReadExplicit => dacl_has_explicit_read_deny_for_sid(p_dacl, psid),
            Self::ReadNewFiles => dacl_has_new_file_read_deny_for_sid(p_dacl, psid),
            Self::Write => dacl_has_write_deny_for_sid(p_dacl, psid),
            Self::DeleteChild { inherit_to_subdirs } => {
                // Only an explicit entry counts: an inherited copy goes when
                // its parent's entry is removed (#1506 item 1).
                dacl_has_deny_for_sid(
                    p_dacl,
                    psid,
                    FILE_DELETE_CHILD,
                    inherit_to_subdirs,
                    /*explicit_only*/ true,
                )
            }
            Self::Delete => dacl_has_deny_for_sid(
                p_dacl, psid, DELETE, /*require_container_inherit*/ false,
                /*explicit_only*/ false,
            ),
        }
    }
}

/// Returns true when the DACL already denies `psid` a right in `mask` on the
/// object itself and, if `require_container_inherit`, propagates that deny to
/// subdirectories; with `explicit_only`, an inherited entry doesn't count.
unsafe fn dacl_has_deny_for_sid(
    p_dacl: *mut ACL,
    psid: *mut c_void,
    mask: u32,
    require_container_inherit: bool,
    explicit_only: bool,
) -> bool {
    if p_dacl.is_null() {
        return false;
    }
    let mut info: ACL_SIZE_INFORMATION = std::mem::zeroed();
    let ok = GetAclInformation(
        p_dacl as *const ACL,
        &mut info as *mut _ as *mut c_void,
        std::mem::size_of::<ACL_SIZE_INFORMATION>() as u32,
        AclSizeInformation,
    );
    if ok == 0 {
        return false;
    }
    for i in 0..info.AceCount {
        let mut p_ace: *mut c_void = std::ptr::null_mut();
        if GetAce(p_dacl as *const ACL, i, &mut p_ace) == 0 {
            continue;
        }
        let hdr = &*(p_ace as *const ACE_HEADER);
        if hdr.AceType != ACCESS_DENIED_ACE_TYPE || (hdr.AceFlags & INHERIT_ONLY_ACE) != 0 {
            continue;
        }
        if require_container_inherit && (u32::from(hdr.AceFlags) & CONTAINER_INHERIT_ACE) == 0 {
            continue;
        }
        if explicit_only && (hdr.AceFlags & INHERITED_ACE) != 0 {
            continue;
        }
        let ace = &*(p_ace as *const ACCESS_DENIED_ACE);
        let base = p_ace as usize;
        let sid_ptr =
            (base + std::mem::size_of::<ACE_HEADER>() + std::mem::size_of::<u32>()) as *mut c_void;
        if EqualSid(sid_ptr, psid) != 0 && (ace.Mask & mask) != 0 {
            return true;
        }
    }
    false
}

unsafe fn add_deny_ace(path: &Path, psid: *mut c_void, kind: DenyAceKind) -> Result<bool> {
    let mut p_sd: *mut c_void = std::ptr::null_mut();
    let mut p_dacl: *mut ACL = std::ptr::null_mut();
    let code = GetNamedSecurityInfoW(
        to_wide(path).as_ptr(),
        1,
        DACL_SECURITY_INFORMATION,
        std::ptr::null_mut(),
        std::ptr::null_mut(),
        &mut p_dacl,
        std::ptr::null_mut(),
        &mut p_sd,
    );
    if code != ERROR_SUCCESS {
        return Err(anyhow!("GetNamedSecurityInfoW failed: {code}"));
    }
    let mut added = false;
    if !kind.already_present(p_dacl, psid, path.is_dir()) {
        let trustee = TRUSTEE_W {
            pMultipleTrustee: std::ptr::null_mut(),
            MultipleTrusteeOperation: 0,
            TrusteeForm: TRUSTEE_IS_SID,
            TrusteeType: TRUSTEE_IS_UNKNOWN,
            ptstrName: psid as *mut u16,
        };
        let mut explicit: EXPLICIT_ACCESS_W = std::mem::zeroed();
        explicit.grfAccessPermissions = kind.mask();
        explicit.grfAccessMode = DENY_ACCESS;
        explicit.grfInheritance = kind.inheritance();
        explicit.Trustee = trustee;
        let mut p_new_dacl: *mut ACL = std::ptr::null_mut();
        let code2 = SetEntriesInAclW(1, &explicit, p_dacl, &mut p_new_dacl);
        if code2 == ERROR_SUCCESS {
            let code3 = SetNamedSecurityInfoW(
                to_wide(path).as_ptr() as *mut u16,
                1,
                DACL_SECURITY_INFORMATION,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                p_new_dacl,
                std::ptr::null_mut(),
            );
            if code3 == ERROR_SUCCESS {
                added = true;
            }
            if !p_new_dacl.is_null() {
                LocalFree(p_new_dacl as HLOCAL);
            }
        }
    }
    if !p_sd.is_null() {
        LocalFree(p_sd as HLOCAL);
    }
    Ok(added)
}

/// Adds a deny ACE to prevent reads for the given SID on the target path.
///
/// `SetEntriesInAclW` places newly-created deny ACEs before allow ACEs, which
/// keeps the resulting DACL in the order Windows expects for denies to win.
/// The ACE is inheritable so a deny applied to a materialized directory also
/// covers files and directories later created underneath it.
///
/// # Safety
/// Caller must ensure `psid` points to a valid SID and `path` refers to an existing file or directory.
pub unsafe fn add_deny_read_ace(path: &Path, psid: *mut c_void) -> Result<bool> {
    add_deny_ace(path, psid, DenyAceKind::Read)
}

/// Grants RX to the null device for the given SID to support stdout/stderr redirection.
///
/// # Safety
/// Caller must ensure `psid` is a valid SID pointer.
pub unsafe fn allow_null_device(psid: *mut c_void) {
    let desired = 0x00020000 | 0x00040000; // READ_CONTROL | WRITE_DAC
    let h = CreateFileW(
        to_wide(r"\\\\.\\NUL").as_ptr(),
        desired,
        FILE_SHARE_READ | FILE_SHARE_WRITE,
        std::ptr::null_mut(),
        OPEN_EXISTING,
        FILE_ATTRIBUTE_NORMAL,
        0,
    );
    if h == 0 || h == INVALID_HANDLE_VALUE {
        return;
    }
    let mut p_sd: *mut c_void = std::ptr::null_mut();
    let mut p_dacl: *mut ACL = std::ptr::null_mut();
    let code = GetSecurityInfo(
        h,
        SE_KERNEL_OBJECT as i32,
        DACL_SECURITY_INFORMATION,
        std::ptr::null_mut(),
        std::ptr::null_mut(),
        &mut p_dacl,
        std::ptr::null_mut(),
        &mut p_sd,
    );
    if code == ERROR_SUCCESS {
        let trustee = TRUSTEE_W {
            pMultipleTrustee: std::ptr::null_mut(),
            MultipleTrusteeOperation: 0,
            TrusteeForm: TRUSTEE_IS_SID,
            TrusteeType: TRUSTEE_IS_UNKNOWN,
            ptstrName: psid as *mut u16,
        };
        let mut explicit: EXPLICIT_ACCESS_W = std::mem::zeroed();
        explicit.grfAccessPermissions =
            FILE_GENERIC_READ | FILE_GENERIC_WRITE | FILE_GENERIC_EXECUTE;
        explicit.grfAccessMode = 2; // SET_ACCESS
        explicit.grfInheritance = 0;
        explicit.Trustee = trustee;
        let mut p_new_dacl: *mut ACL = std::ptr::null_mut();
        let code2 = SetEntriesInAclW(1, &explicit, p_dacl, &mut p_new_dacl);
        if code2 == ERROR_SUCCESS {
            let _ = SetSecurityInfo(
                h,
                SE_KERNEL_OBJECT as i32,
                DACL_SECURITY_INFORMATION,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                p_new_dacl,
                std::ptr::null_mut(),
            );
            if !p_new_dacl.is_null() {
                LocalFree(p_new_dacl as HLOCAL);
            }
        }
    }
    if !p_sd.is_null() {
        LocalFree(p_sd as HLOCAL);
    }
    CloseHandle(h);
}
const CONTAINER_INHERIT_ACE: u32 = 0x2;
const NO_PROPAGATE_INHERIT_ACE: u32 = 0x4;
const OBJECT_INHERIT_ACE: u32 = 0x1;
const INHERITANCE_FLAGS: u32 =
    OBJECT_INHERIT_ACE | CONTAINER_INHERIT_ACE | NO_PROPAGATE_INHERIT_ACE | INHERIT_ONLY_ACE as u32;
/// Maps generic rights to file rights.
pub(crate) const FILE_MAPPING: GENERIC_MAPPING = GENERIC_MAPPING {
    GenericRead: FILE_GENERIC_READ,
    GenericWrite: FILE_GENERIC_WRITE,
    GenericExecute: FILE_GENERIC_EXECUTE,
    GenericAll: FILE_ALL_ACCESS,
};

#[path = "acl_delete_child.rs"]
mod delete_child;

/// D311/D313: the pieces of our own tree walk.
#[path = "acl_walk.rs"]
pub(crate) mod walk;

#[cfg(test)]
#[path = "acl_tests.rs"]
mod tests;
