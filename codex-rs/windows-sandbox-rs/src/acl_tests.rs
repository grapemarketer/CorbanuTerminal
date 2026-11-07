//! PF-27-S07: removing the `CODEX_HOME` new-file deny leaves everything else.

use super::add_deny_delete_ace;
use super::add_deny_read_ace;
use super::add_deny_read_ace_for_new_files;
use super::add_deny_write_ace;
use super::ensure_explicit_deny_read_ace;
use super::has_exact_deny_read_ace_for_new_files;
use super::remove_deny_read_ace_for_new_files;
use crate::token::LocalSid;
use crate::winutil::to_wide;
use pretty_assertions::assert_eq;
use std::ffi::c_void;
use std::path::Path;
use windows_sys::Win32::Foundation::CloseHandle;
use windows_sys::Win32::Foundation::ERROR_SUCCESS;
use windows_sys::Win32::Foundation::HANDLE;
use windows_sys::Win32::Foundation::HLOCAL;
use windows_sys::Win32::Foundation::LocalFree;
use windows_sys::Win32::Security::ACL;
use windows_sys::Win32::Security::AdjustTokenPrivileges;
use windows_sys::Win32::Security::Authorization::ConvertSecurityDescriptorToStringSecurityDescriptorW;
use windows_sys::Win32::Security::Authorization::EXPLICIT_ACCESS_W;
use windows_sys::Win32::Security::Authorization::GetNamedSecurityInfoW;
use windows_sys::Win32::Security::Authorization::SDDL_REVISION_1;
use windows_sys::Win32::Security::Authorization::SE_FILE_OBJECT;
use windows_sys::Win32::Security::Authorization::SetEntriesInAclW;
use windows_sys::Win32::Security::Authorization::SetNamedSecurityInfoW;
use windows_sys::Win32::Security::Authorization::TRUSTEE_IS_SID;
use windows_sys::Win32::Security::Authorization::TRUSTEE_IS_UNKNOWN;
use windows_sys::Win32::Security::Authorization::TRUSTEE_W;
use windows_sys::Win32::Security::DACL_SECURITY_INFORMATION;
use windows_sys::Win32::Security::ImpersonateSelf;
use windows_sys::Win32::Security::PROTECTED_DACL_SECURITY_INFORMATION;
use windows_sys::Win32::Security::RevertToSelf;
use windows_sys::Win32::Security::SecurityImpersonation;
use windows_sys::Win32::Security::TOKEN_ADJUST_PRIVILEGES;
use windows_sys::Win32::Security::TOKEN_QUERY;
use windows_sys::Win32::Storage::FileSystem::FILE_GENERIC_READ;
use windows_sys::Win32::Storage::FileSystem::FILE_GENERIC_WRITE;
use windows_sys::Win32::System::Threading::GetCurrentThread;
use windows_sys::Win32::System::Threading::OpenThreadToken;

const SANDBOX_GROUP: &str = "S-1-5-21-2718281828-3141592653-1618033988-1001";
const OTHER_SID: &str = "S-1-5-21-2718281828-3141592653-1618033988-1002";

#[test]
fn pf_27_s07_removes_only_the_new_file_deny() {
    let dir = tempfile::tempdir().expect("dir");
    let dir = dir.path();
    let group = LocalSid::from_string(SANDBOX_GROUP).expect("group SID");
    let other = LocalSid::from_string(OTHER_SID).expect("other SID");
    let existing = dir.join("existing.txt");
    std::fs::write(&existing, "x").expect("existing file");
    // Unrelated entries, including other denies for the same SID.
    // SAFETY: valid SIDs and existing paths.
    unsafe {
        assert!(add_deny_read_ace(dir, other.as_ptr()).expect("other deny"));
        assert!(add_deny_write_ace(dir, group.as_ptr()).expect("write deny"));
        // S06's `add_deny_read_ace` would count the inherited write deny
        // (it overlaps in READ_CONTROL and SYNCHRONIZE) as present.
        assert!(ensure_explicit_deny_read_ace(&existing, group.as_ptr()).expect("file deny"));
    }
    let dir_before = dacl_sddl(dir);
    let existing_before = dacl_sddl(&existing);

    // SAFETY: as above.
    let added = unsafe { add_deny_read_ace_for_new_files(dir, group.as_ptr()) };
    assert!(added.expect("new-file deny"));
    let replaced = dir.join("replaced.txt");
    std::fs::write(&replaced, "y").expect("file created while protected");
    assert_ne!(dacl_sddl(dir), dir_before);
    assert_ne!(dacl_sddl(&existing), existing_before);

    // SAFETY: as above.
    let removed = unsafe { remove_deny_read_ace_for_new_files(dir, group.as_ptr()) };
    assert!(removed.expect("remove"));
    assert_eq!(dacl_sddl(dir), dir_before);
    assert_eq!(dacl_sddl(&existing), existing_before);
    let fresh = dir.join("fresh.txt");
    std::fs::write(&fresh, "z").expect("file created after removal");
    assert_eq!(dacl_sddl(&replaced), dacl_sddl(&fresh));

    // SAFETY: as above.
    let again = unsafe { remove_deny_read_ace_for_new_files(dir, group.as_ptr()) };
    assert!(!again.expect("second remove"));
    assert_eq!(dacl_sddl(dir), dir_before);
}

/// Another SID's new-file deny is not ours to remove.
#[test]
fn pf_27_s07_keeps_a_new_file_deny_for_another_sid() {
    let dir = tempfile::tempdir().expect("dir");
    let dir = dir.path();
    let group = LocalSid::from_string(SANDBOX_GROUP).expect("group SID");
    let other = LocalSid::from_string(OTHER_SID).expect("other SID");
    // SAFETY: valid SIDs and an existing directory.
    unsafe {
        assert!(add_deny_read_ace_for_new_files(dir, other.as_ptr()).expect("other deny"));
    }
    let before = dacl_sddl(dir);
    // SAFETY: as above.
    let removed = unsafe { remove_deny_read_ace_for_new_files(dir, group.as_ptr()) };
    assert!(!removed.expect("remove"));
    assert_eq!(dacl_sddl(dir), before);
}

/// A protected DACL keeps its protection, and a deny for the same group with
/// the same inheritance but a wider mask is not ours to remove.
#[test]
fn pf_27_s07_keeps_protection_and_wider_denies() {
    let dir = tempfile::tempdir().expect("dir");
    let dir = dir.path();
    let group = LocalSid::from_string(SANDBOX_GROUP).expect("group SID");
    protect_dacl(dir);
    let protected = dacl_sddl(dir);
    assert!(protected.starts_with("D:P"), "{protected}");

    // SAFETY: a valid SID and an existing directory.
    unsafe {
        assert!(add_deny_read_ace_for_new_files(dir, group.as_ptr()).expect("deny"));
        assert!(has_exact_deny_read_ace_for_new_files(dir, None).expect("any SID"));
        assert!(remove_deny_read_ace_for_new_files(dir, group.as_ptr()).expect("remove"));
    }
    assert_eq!(dacl_sddl(dir), protected);

    // Read and write, inherit-only for files: not the entry PF-27-S06 adds.
    add_deny(
        dir,
        group.as_ptr(),
        FILE_GENERIC_READ | FILE_GENERIC_WRITE,
        0x1 | 0x4 | 0x8,
    );
    let wider = dacl_sddl(dir);
    // SAFETY: as above.
    unsafe {
        assert!(!has_exact_deny_read_ace_for_new_files(dir, Some(group.as_ptr())).expect("exact"));
        assert!(!remove_deny_read_ace_for_new_files(dir, group.as_ptr()).expect("remove"));
    }
    assert_eq!(dacl_sddl(dir), wider);
}

/// A file whose only read deny is inherited gets its own explicit one, which
/// stays when the inherited copy goes.
#[test]
fn pf_27_s07_lock_file_gets_its_own_deny() {
    let dir = tempfile::tempdir().expect("dir");
    let dir = dir.path();
    let group = LocalSid::from_string(SANDBOX_GROUP).expect("group SID");
    let file = dir.join("lock");
    // SAFETY: a valid SID and existing paths.
    unsafe {
        assert!(add_deny_read_ace_for_new_files(dir, group.as_ptr()).expect("new-file deny"));
        std::fs::write(&file, "").expect("file");
        assert!(!explicit_deny(&file), "{}", dacl_sddl(&file));
        assert!(ensure_explicit_deny_read_ace(&file, group.as_ptr()).expect("explicit deny"));
        assert!(explicit_deny(&file), "{}", dacl_sddl(&file));
        assert!(remove_deny_read_ace_for_new_files(dir, group.as_ptr()).expect("remove"));
    }
    assert!(explicit_deny(&file), "{}", dacl_sddl(&file));
}

/// #1542 item 3: a sandbox `DELETE` deny that can't be written (here, a
/// folder whose DACL gives its owner no `WRITE_DAC`, written without
/// privileges) is an error, so D263
/// doesn't start a command without it; one that is written, or already
/// there, is not.
#[test]
fn d263_delete_deny_that_cannot_be_written_is_an_error() {
    let dir = tempfile::tempdir().expect("dir");
    let group = LocalSid::from_string(SANDBOX_GROUP).expect("group SID");
    let open = dir.path().join("open");
    let locked = dir.path().join("locked");
    std::fs::create_dir(&open).expect("open dir");
    std::fs::create_dir(&locked).expect("locked dir");
    let whoami = std::process::Command::new("whoami.exe")
        .arg("/user")
        .output()
        .expect("run whoami");
    let whoami = String::from_utf8_lossy(&whoami.stdout);
    let user = whoami
        .split_whitespace()
        .find(|token| token.starts_with("S-1-"))
        .unwrap_or_else(|| panic!("unexpected whoami output: {whoami:?}"));
    // OWNER RIGHTS (S-1-3-4) replaces the owner's implicit WRITE_DAC.
    let output = std::process::Command::new("icacls.exe")
        .arg(&locked)
        .args(["/inheritance:r", "/grant:r", "*S-1-3-4:(RX)"])
        .arg(format!("*{user}:(RX)"))
        .output()
        .expect("run icacls");
    assert!(
        output.status.success(),
        "icacls: {}",
        String::from_utf8_lossy(&output.stdout)
    );
    // SAFETY: a valid SID and existing paths; the thread reverts to the
    // process token before the block ends.
    let (locked_result, first, second) = unsafe {
        let unprivileged = WithoutPrivileges::start();
        let locked_result =
            add_deny_delete_ace(&locked, group.as_ptr()).map_err(|err| err.to_string());
        drop(unprivileged);
        (
            locked_result,
            add_deny_delete_ace(&open, group.as_ptr()).map_err(|err| err.to_string()),
            add_deny_delete_ace(&open, group.as_ptr()).map_err(|err| err.to_string()),
        )
    };
    assert_eq!(
        (
            locked_result
                .as_ref()
                .is_err_and(|err| err.contains("could not deny the sandbox DELETE")),
            first,
            second,
        ),
        (true, Ok(true), Ok(false)),
        "locked: {locked_result:?}"
    );
}

/// Impersonates this process's own token with every privilege disabled on
/// the current thread until dropped, so an elevated runner's privileges
/// (such as restore) can't write a DACL the test made unwritable.
struct WithoutPrivileges;

impl WithoutPrivileges {
    fn start() -> Self {
        // SAFETY: plain Win32 calls on the current thread; the token handle is
        // closed here and the impersonation ends in `drop`, also when an
        // assertion below fails.
        unsafe {
            assert_ne!(ImpersonateSelf(SecurityImpersonation), 0, "ImpersonateSelf");
            let guard = Self;
            let mut token: HANDLE = 0;
            assert_ne!(
                OpenThreadToken(
                    GetCurrentThread(),
                    TOKEN_ADJUST_PRIVILEGES | TOKEN_QUERY,
                    /*openasself*/ 1,
                    &mut token,
                ),
                0,
                "OpenThreadToken"
            );
            let disabled = AdjustTokenPrivileges(
                token,
                /*disableallprivileges*/ 1,
                std::ptr::null(),
                0,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
            );
            CloseHandle(token);
            assert_ne!(disabled, 0, "AdjustTokenPrivileges");
            guard
        }
    }
}

impl Drop for WithoutPrivileges {
    fn drop(&mut self) {
        // SAFETY: ends this thread's impersonation.
        unsafe {
            RevertToSelf();
        }
    }
}

/// Whether `path`'s SDDL has an explicit (not inherited) deny for the group.
fn explicit_deny(path: &Path) -> bool {
    dacl_sddl(path)
        .split('(')
        .filter_map(|entry| entry.strip_suffix(')'))
        .map(|entry| entry.split(';').collect::<Vec<_>>())
        .any(|fields| {
            fields.len() == 6
                && fields[0] == "D"
                && !fields[1].contains("ID")
                && fields[5] == SANDBOX_GROUP
        })
}

/// Re-sets `path`'s DACL as protected (inherited entries become explicit).
fn protect_dacl(path: &Path) {
    let mut sd: *mut c_void = std::ptr::null_mut();
    let mut dacl: *mut ACL = std::ptr::null_mut();
    // SAFETY: valid path and out pointers; `sd` is freed below.
    unsafe {
        let code = GetNamedSecurityInfoW(
            to_wide(path).as_ptr(),
            SE_FILE_OBJECT,
            DACL_SECURITY_INFORMATION,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            &mut dacl,
            std::ptr::null_mut(),
            &mut sd,
        );
        assert_eq!(code, ERROR_SUCCESS, "GetNamedSecurityInfoW");
        let code = SetNamedSecurityInfoW(
            to_wide(path).as_ptr() as *mut u16,
            SE_FILE_OBJECT,
            DACL_SECURITY_INFORMATION | PROTECTED_DACL_SECURITY_INFORMATION,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            dacl,
            std::ptr::null_mut(),
        );
        assert_eq!(code, ERROR_SUCCESS, "SetNamedSecurityInfoW");
        LocalFree(sd as HLOCAL);
    }
}

/// Adds a deny entry with an arbitrary mask and inheritance.
fn add_deny(path: &Path, sid: *mut c_void, mask: u32, inheritance: u32) {
    let mut sd: *mut c_void = std::ptr::null_mut();
    let mut dacl: *mut ACL = std::ptr::null_mut();
    // SAFETY: valid path, SID and out pointers; allocations freed below.
    unsafe {
        let code = GetNamedSecurityInfoW(
            to_wide(path).as_ptr(),
            SE_FILE_OBJECT,
            DACL_SECURITY_INFORMATION,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            &mut dacl,
            std::ptr::null_mut(),
            &mut sd,
        );
        assert_eq!(code, ERROR_SUCCESS, "GetNamedSecurityInfoW");
        let mut explicit: EXPLICIT_ACCESS_W = std::mem::zeroed();
        explicit.grfAccessPermissions = mask;
        explicit.grfAccessMode = 3; // DENY_ACCESS
        explicit.grfInheritance = inheritance;
        explicit.Trustee = TRUSTEE_W {
            pMultipleTrustee: std::ptr::null_mut(),
            MultipleTrusteeOperation: 0,
            TrusteeForm: TRUSTEE_IS_SID,
            TrusteeType: TRUSTEE_IS_UNKNOWN,
            ptstrName: sid as *mut u16,
        };
        let mut new_dacl: *mut ACL = std::ptr::null_mut();
        assert_eq!(
            SetEntriesInAclW(1, &explicit, dacl, &mut new_dacl),
            ERROR_SUCCESS
        );
        let code = SetNamedSecurityInfoW(
            to_wide(path).as_ptr() as *mut u16,
            SE_FILE_OBJECT,
            DACL_SECURITY_INFORMATION,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            new_dacl,
            std::ptr::null_mut(),
        );
        assert_eq!(code, ERROR_SUCCESS, "SetNamedSecurityInfoW");
        LocalFree(new_dacl as HLOCAL);
        LocalFree(sd as HLOCAL);
    }
}

fn dacl_sddl(path: &Path) -> String {
    let mut sd: *mut c_void = std::ptr::null_mut();
    // SAFETY: valid path; `sd` is freed below.
    let code = unsafe {
        GetNamedSecurityInfoW(
            to_wide(path).as_ptr(),
            SE_FILE_OBJECT,
            DACL_SECURITY_INFORMATION,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            &mut sd,
        )
    };
    assert_eq!(code, ERROR_SUCCESS, "GetNamedSecurityInfoW");
    let mut text: *mut u16 = std::ptr::null_mut();
    // SAFETY: `sd` is a valid descriptor; `text` is freed below.
    let ok = unsafe {
        ConvertSecurityDescriptorToStringSecurityDescriptorW(
            sd,
            SDDL_REVISION_1,
            DACL_SECURITY_INFORMATION,
            &mut text,
            std::ptr::null_mut(),
        )
    };
    assert_ne!(
        ok, 0,
        "ConvertSecurityDescriptorToStringSecurityDescriptorW"
    );
    // SAFETY: `text` is NUL-terminated.
    let sddl = unsafe {
        let len = (0..).take_while(|&i| *text.add(i) != 0).count();
        String::from_utf16_lossy(std::slice::from_raw_parts(text, len))
    };
    // SAFETY: both allocated with LocalAlloc by the calls above.
    unsafe {
        LocalFree(text as HLOCAL);
        LocalFree(sd as HLOCAL);
    }
    sddl
}
