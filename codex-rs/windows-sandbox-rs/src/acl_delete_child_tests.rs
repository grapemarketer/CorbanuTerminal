//! D285: the direct write leaves every DACL in the tree exactly as #1549's
//! `SetNamedSecurityInfoW` walk does. Each test builds two identical trees,
//! applies the same step to one with #1549's calls and to the other with
//! [`set_delete_child_deny`], and compares every object's DACL (as SDDL,
//! links not followed) after each step.

use super::Written;
use super::set_delete_child_deny;
use crate::acl::DenyAceKind;
use crate::acl::add_deny_ace;
use crate::acl::is_exact_delete_child_deny;
use crate::acl::remove_matching_aces;
use crate::token::LocalSid;
use crate::winutil::to_wide;
use pretty_assertions::assert_eq;
use std::collections::BTreeMap;
use std::ffi::c_void;
use std::path::Path;
use std::path::PathBuf;
use std::process::Command;
use windows_sys::Win32::Foundation::CloseHandle;
use windows_sys::Win32::Foundation::ERROR_SUCCESS;
use windows_sys::Win32::Foundation::HLOCAL;
use windows_sys::Win32::Foundation::INVALID_HANDLE_VALUE;
use windows_sys::Win32::Foundation::LocalFree;
use windows_sys::Win32::Security::ACL;
use windows_sys::Win32::Security::AddAccessAllowedAceEx;
use windows_sys::Win32::Security::AddAce;
use windows_sys::Win32::Security::Authorization::ConvertSecurityDescriptorToStringSecurityDescriptorW;
use windows_sys::Win32::Security::Authorization::EXPLICIT_ACCESS_W;
use windows_sys::Win32::Security::Authorization::GetSecurityInfo;
use windows_sys::Win32::Security::Authorization::SDDL_REVISION_1;
use windows_sys::Win32::Security::Authorization::SE_FILE_OBJECT;
use windows_sys::Win32::Security::Authorization::SetEntriesInAclW;
use windows_sys::Win32::Security::Authorization::SetNamedSecurityInfoW;
use windows_sys::Win32::Security::Authorization::TRUSTEE_IS_SID;
use windows_sys::Win32::Security::Authorization::TRUSTEE_IS_UNKNOWN;
use windows_sys::Win32::Security::Authorization::TRUSTEE_W;
use windows_sys::Win32::Security::DACL_SECURITY_INFORMATION;
use windows_sys::Win32::Security::GetAce;
use windows_sys::Win32::Security::InitializeAcl;
use windows_sys::Win32::Security::InitializeSecurityDescriptor;
use windows_sys::Win32::Security::PROTECTED_DACL_SECURITY_INFORMATION;
use windows_sys::Win32::Security::SE_DACL_AUTO_INHERIT_REQ;
use windows_sys::Win32::Security::SE_DACL_AUTO_INHERITED;
use windows_sys::Win32::Security::SECURITY_DESCRIPTOR;
use windows_sys::Win32::Security::SetFileSecurityW;
use windows_sys::Win32::Security::SetSecurityDescriptorControl;
use windows_sys::Win32::Security::SetSecurityDescriptorDacl;
use windows_sys::Win32::Storage::FileSystem::CreateFileW;
use windows_sys::Win32::Storage::FileSystem::DELETE;
use windows_sys::Win32::Storage::FileSystem::FILE_DELETE_CHILD;
use windows_sys::Win32::Storage::FileSystem::FILE_FLAG_BACKUP_SEMANTICS;
use windows_sys::Win32::Storage::FileSystem::FILE_FLAG_OPEN_REPARSE_POINT;
use windows_sys::Win32::Storage::FileSystem::FILE_GENERIC_READ;
use windows_sys::Win32::Storage::FileSystem::FILE_GENERIC_WRITE;
use windows_sys::Win32::Storage::FileSystem::FILE_SHARE_DELETE;
use windows_sys::Win32::Storage::FileSystem::FILE_SHARE_READ;
use windows_sys::Win32::Storage::FileSystem::FILE_SHARE_WRITE;
use windows_sys::Win32::Storage::FileSystem::OPEN_EXISTING;
use windows_sys::Win32::Storage::FileSystem::READ_CONTROL;
use windows_sys::Win32::System::SystemServices::SECURITY_DESCRIPTOR_REVISION;

const EVERYONE: &str = "S-1-1-0";
const USERS: &str = "S-1-5-32-545";
const CREATOR_OWNER: &str = "S-1-3-0";
/// Stands in for a sandbox capability SID.
const CAPABILITY: &str = "S-1-5-21-2718281828-3141592653-1618033988-1001";
const GRANT_ACCESS: i32 = 1;
const DENY_ACCESS: i32 = 3;
const OI: u32 = 0x1;
const CI: u32 = 0x2;
const NP: u32 = 0x4;
const IO: u32 = 0x8;
const GENERIC_ALL: u32 = 0x1000_0000;
const GENERIC_READ: u32 = 0x8000_0000;
const GENERIC_EXECUTE: u32 = 0x2000_0000;

/// One step of a session's denies: (folder below the tree, inherited by
/// subfolders, add).
type Step = (&'static str, bool, bool);

/// What a legacy session does: the root (that folder only) and `.git`
/// (inherited by subfolders), added and then removed.
const SESSION: [Step; 4] = [
    ("", false, true),
    (".git", true, true),
    (".git", true, false),
    ("", false, false),
];

#[test]
fn d285_delete_child_denies_match_the_tree_walk() {
    let base = tempfile::tempdir().expect("base dir");
    let walked = base.path().join("walked");
    let direct = base.path().join("direct");
    for root in [&walked, &direct] {
        build_tree(root);
    }
    let initial = snapshot(&direct);
    assert_eq!(snapshot(&walked), initial, "the trees start the same");
    let written = run_session(&walked, &direct, &SESSION);
    assert_eq!(written, vec![Written::Direct; SESSION.len()]);
    assert_eq!(
        snapshot(&direct),
        initial,
        "the session leaves nothing behind"
    );
}

/// The nested case: `.git\refs` already has its own inherited deny (a
/// protected path's), so its subfolders carry two copies while `.git`'s is
/// there.
#[test]
fn d285_nested_delete_child_denies_match_the_tree_walk() {
    let base = tempfile::tempdir().expect("base dir");
    let walked = base.path().join("walked");
    let direct = base.path().join("direct");
    for root in [&walked, &direct] {
        build_tree(root);
        // SAFETY: a valid SID and an existing folder.
        let added = unsafe { reference(&root.join(".git\\refs"), true, true) };
        assert!(added, "nested deny");
    }
    assert_eq!(snapshot(&walked), snapshot(&direct));
    let written = run_session(&walked, &direct, &SESSION);
    assert_eq!(written, vec![Written::Direct; SESSION.len()]);
}

/// A link among `.git`'s subfolders: the direct write gives up and the
/// #1549 walk does it all, whatever it does with links.
#[test]
fn d285_link_under_git_falls_back_to_the_tree_walk() {
    let base = tempfile::tempdir().expect("base dir");
    let walked = base.path().join("walked");
    let direct = base.path().join("direct");
    for root in [&walked, &direct] {
        build_tree(root);
        let target = root.join("outside");
        std::fs::create_dir_all(target.join("inner")).expect("link target");
        let status = Command::new("cmd.exe")
            .args(["/d", "/c", "mklink", "/J"])
            .arg(root.join(".git\\objects\\linked"))
            .arg(&target)
            .output()
            .expect("run mklink");
        assert!(status.status.success(), "mklink: {status:?}");
    }
    assert_eq!(snapshot(&walked), snapshot(&direct));
    let written = run_session(&walked, &direct, &SESSION);
    assert_eq!(
        written,
        vec![
            Written::Direct,
            Written::Walked,
            Written::Walked,
            Written::Direct
        ]
    );
}

/// A subfolder whose inherited entries don't match its parent's (stale): a
/// recompute would change more than the delete-child copies, so the
/// direct write gives up and the #1549 walk does it all. (Only `.git`'s
/// step: the root's walk would also refresh the stale entry, which the
/// direct root write leaves alone, as it isn't one of these entries.)
#[test]
fn d285_stale_subfolder_falls_back_to_the_tree_walk() {
    let base = tempfile::tempdir().expect("base dir");
    let walked = base.path().join("walked");
    let direct = base.path().join("direct");
    for root in [&walked, &direct] {
        build_tree(root);
        add_stale_inherited_entry(&root.join(".git\\objects\\ab"));
    }
    assert_eq!(snapshot(&walked), snapshot(&direct));
    let written = run_session(&walked, &direct, &SESSION[1..3]);
    assert_eq!(written, vec![Written::Walked, Written::Direct]);
}

/// Runs `steps` with #1549's calls on `walked` and directly on `direct`,
/// comparing both trees after each; returns how each direct step wrote.
fn run_session(walked: &Path, direct: &Path, steps: &[Step]) -> Vec<Written> {
    let mut written = Vec::new();
    for (i, (folder, inherit, add)) in steps.iter().copied().enumerate() {
        // SAFETY: a valid SID and existing folders.
        let (changed, how) = unsafe {
            let changed = reference(&walked.join(folder), inherit, add);
            let sid = LocalSid::from_string(EVERYONE).expect("Everyone");
            let how = set_delete_child_deny(&direct.join(folder), sid.as_ptr(), inherit, add)
                .expect("direct write");
            (changed, how)
        };
        assert!(changed, "step {i}: #1549's call changed nothing");
        assert_eq!(
            snapshot(walked),
            snapshot(direct),
            "step {i} ({folder:?}, inherit {inherit}, add {add}) via {how:?}"
        );
        written.push(how);
    }
    written
}

/// #1549's calls (`add_deny_ace`/`remove_matching_aces` with
/// `SetNamedSecurityInfoW`).
unsafe fn reference(path: &Path, inherit_to_subdirs: bool, add: bool) -> bool {
    let sid = LocalSid::from_string(EVERYONE).expect("Everyone");
    let psid = sid.as_ptr();
    if add {
        add_deny_ace(path, psid, DenyAceKind::DeleteChild { inherit_to_subdirs })
            .expect("#1549 add")
    } else {
        remove_matching_aces(path, |_, ace| {
            is_exact_delete_child_deny(ace, psid, inherit_to_subdirs)
        })
        .expect("#1549 remove")
    }
}

/// A workspace with a real `.git` and the entries a session and a user's
/// machine put there: generic and CREATOR OWNER entries above, a capability
/// write deny on `.git`, object-only and no-propagate entries, and a
/// protected subfolder.
fn build_tree(root: &Path) {
    std::fs::create_dir_all(root.join("src\\m\\deep")).expect("worktree dirs");
    std::fs::write(root.join("a.txt"), b"a").expect("file");
    std::fs::write(root.join("src\\m\\f.rs"), b"f").expect("file");
    let output = Command::new("git")
        .args(["init", "-q"])
        .arg(root)
        .output()
        .expect("run git init");
    assert!(output.status.success(), "git init: {output:?}");
    let git = root.join(".git");
    for dir in [
        "objects\\ab",
        "objects\\pack",
        "refs\\heads\\feature",
        "protected\\inner",
    ] {
        std::fs::create_dir_all(git.join(dir)).expect(".git dirs");
    }
    std::fs::write(git.join("objects\\ab\\cdef"), b"x").expect("object");
    std::fs::write(git.join("refs\\heads\\feature\\x"), b"0").expect("ref");
    std::fs::write(git.join("protected\\inner\\f"), b"p").expect("file");
    add_entry(root, CREATOR_OWNER, GRANT_ACCESS, GENERIC_ALL, OI | CI | IO);
    add_entry(
        root,
        USERS,
        GRANT_ACCESS,
        GENERIC_READ | GENERIC_EXECUTE,
        OI | CI,
    );
    add_entry(
        &git,
        CAPABILITY,
        DENY_ACCESS,
        FILE_GENERIC_WRITE | DELETE,
        OI | CI,
    );
    add_entry(
        &git.join("objects"),
        USERS,
        GRANT_ACCESS,
        FILE_GENERIC_READ,
        OI,
    );
    add_entry(
        &git.join("objects"),
        USERS,
        GRANT_ACCESS,
        FILE_DELETE_CHILD,
        CI | NP,
    );
    // Inherited entries become explicit; nothing above reaches below.
    let protected = git.join("protected");
    let sd = SecurityInfo::read(&protected);
    // SAFETY: a valid path and DACL.
    let code = unsafe {
        SetNamedSecurityInfoW(
            to_wide(&protected).as_ptr() as *mut u16,
            SE_FILE_OBJECT,
            DACL_SECURITY_INFORMATION | PROTECTED_DACL_SECURITY_INFORMATION,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            sd.dacl,
            std::ptr::null_mut(),
        )
    };
    assert_eq!(code, ERROR_SUCCESS, "protect");
}

/// Adds an entry with `SetEntriesInAclW` and the tree walk.
fn add_entry(path: &Path, sid: &str, mode: i32, mask: u32, inheritance: u32) {
    let sid = LocalSid::from_string(sid).expect("SID");
    let sd = SecurityInfo::read(path);
    // SAFETY: valid path, SID and DACL; the new DACL is freed below.
    unsafe {
        let mut explicit: EXPLICIT_ACCESS_W = std::mem::zeroed();
        explicit.grfAccessPermissions = mask;
        explicit.grfAccessMode = mode;
        explicit.grfInheritance = inheritance;
        explicit.Trustee = TRUSTEE_W {
            pMultipleTrustee: std::ptr::null_mut(),
            MultipleTrusteeOperation: 0,
            TrusteeForm: TRUSTEE_IS_SID,
            TrusteeType: TRUSTEE_IS_UNKNOWN,
            ptstrName: sid.as_ptr() as *mut u16,
        };
        let mut new_dacl: *mut ACL = std::ptr::null_mut();
        assert_eq!(
            SetEntriesInAclW(1, &explicit, sd.dacl, &mut new_dacl),
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
        assert_eq!(code, ERROR_SUCCESS, "add entry");
        LocalFree(new_dacl as HLOCAL);
    }
}

/// Appends an inherited-looking entry its parent doesn't give, written to
/// the folder alone.
fn add_stale_inherited_entry(path: &Path) {
    let sid = LocalSid::from_string(USERS).expect("Users");
    let sd = SecurityInfo::read(path);
    // SAFETY: valid path, SID and DACL; the buffers outlive the calls.
    unsafe {
        let size = usize::from((*sd.dacl).AclSize) + 64;
        let mut buffer = vec![0_u32; size.div_ceil(4)];
        let acl = buffer.as_mut_ptr() as *mut ACL;
        assert_ne!(InitializeAcl(acl, size as u32, 2), 0, "InitializeAcl");
        let mut info: windows_sys::Win32::Security::ACL_SIZE_INFORMATION = std::mem::zeroed();
        assert_ne!(
            windows_sys::Win32::Security::GetAclInformation(
                sd.dacl,
                &mut info as *mut _ as *mut c_void,
                std::mem::size_of_val(&info) as u32,
                windows_sys::Win32::Security::AclSizeInformation,
            ),
            0
        );
        for i in 0..info.AceCount {
            let mut ace: *mut c_void = std::ptr::null_mut();
            assert_ne!(GetAce(sd.dacl, i, &mut ace), 0, "GetAce");
            let ace_size =
                u32::from((*(ace as *const windows_sys::Win32::Security::ACE_HEADER)).AceSize);
            assert_ne!(AddAce(acl, 2, u32::MAX, ace, ace_size), 0, "AddAce");
        }
        assert_ne!(
            AddAccessAllowedAceEx(
                acl,
                2,
                0x10, /*INHERITED_ACE*/
                FILE_GENERIC_READ,
                sid.as_ptr()
            ),
            0,
            "AddAccessAllowedAceEx"
        );
        let mut new_sd: SECURITY_DESCRIPTOR = std::mem::zeroed();
        let psd = &mut new_sd as *mut SECURITY_DESCRIPTOR as *mut c_void;
        assert_ne!(
            InitializeSecurityDescriptor(psd, SECURITY_DESCRIPTOR_REVISION),
            0
        );
        assert_ne!(SetSecurityDescriptorDacl(psd, 1, acl, 0), 0);
        assert_ne!(
            SetSecurityDescriptorControl(
                psd,
                SE_DACL_AUTO_INHERITED | SE_DACL_AUTO_INHERIT_REQ,
                SE_DACL_AUTO_INHERITED | SE_DACL_AUTO_INHERIT_REQ
            ),
            0
        );
        assert_ne!(
            SetFileSecurityW(to_wide(path).as_ptr(), DACL_SECURITY_INFORMATION, psd),
            0,
            "SetFileSecurityW"
        );
    }
}

/// Every object's DACL below `root` (and `root`), links not followed, by
/// path relative to `root`.
fn snapshot(root: &Path) -> BTreeMap<String, String> {
    let mut out = BTreeMap::new();
    let mut pending: Vec<PathBuf> = vec![root.to_path_buf()];
    while let Some(path) = pending.pop() {
        let meta = std::fs::symlink_metadata(&path).expect("metadata");
        let relative = path
            .strip_prefix(root)
            .expect("below root")
            .display()
            .to_string();
        out.insert(relative, dacl_sddl(&path));
        if meta.is_dir() {
            for entry in std::fs::read_dir(&path).expect("read dir") {
                pending.push(entry.expect("entry").path());
            }
        }
    }
    out
}

/// The DACL of the object at `path` itself (a link's own), as SDDL.
fn dacl_sddl(path: &Path) -> String {
    let sd = SecurityInfo::read(path);
    let mut text: *mut u16 = std::ptr::null_mut();
    // SAFETY: `sd` is a valid descriptor; `text` is freed below.
    unsafe {
        assert_ne!(
            ConvertSecurityDescriptorToStringSecurityDescriptorW(
                sd.sd,
                SDDL_REVISION_1,
                DACL_SECURITY_INFORMATION,
                &mut text,
                std::ptr::null_mut(),
            ),
            0,
            "ConvertSecurityDescriptorToStringSecurityDescriptorW"
        );
        let len = (0..).take_while(|&i| *text.add(i) != 0).count();
        let sddl = String::from_utf16_lossy(std::slice::from_raw_parts(text, len));
        LocalFree(text as HLOCAL);
        sddl
    }
}

/// An object's own DACL (links not followed).
struct SecurityInfo {
    sd: *mut c_void,
    dacl: *mut ACL,
}

impl SecurityInfo {
    fn read(path: &Path) -> Self {
        // SAFETY: valid path and out pointers; the handle is closed here and
        // `sd` on drop.
        unsafe {
            let handle = CreateFileW(
                to_wide(path).as_ptr(),
                READ_CONTROL,
                FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
                std::ptr::null_mut(),
                OPEN_EXISTING,
                FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT,
                0,
            );
            assert_ne!(handle, INVALID_HANDLE_VALUE, "open {}", path.display());
            let mut sd: *mut c_void = std::ptr::null_mut();
            let mut dacl: *mut ACL = std::ptr::null_mut();
            let code = GetSecurityInfo(
                handle,
                SE_FILE_OBJECT,
                DACL_SECURITY_INFORMATION,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                &mut dacl,
                std::ptr::null_mut(),
                &mut sd,
            );
            CloseHandle(handle);
            assert_eq!(code, ERROR_SUCCESS, "GetSecurityInfo {}", path.display());
            Self { sd, dacl }
        }
    }
}

impl Drop for SecurityInfo {
    fn drop(&mut self) {
        // SAFETY: allocated by `GetSecurityInfo`.
        unsafe {
            LocalFree(self.sd as HLOCAL);
        }
    }
}
