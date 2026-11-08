//! D311/D313 (#1542): the multi-threaded, handle-relative below-check and the
//! label walk of our own leave every object exactly as Windows' walks
//! (`SetNamedSecurityInfoW`) do. These
//! are perf-walk's equality cases: two identical trees (plus the folders
//! their links point at) go through a legacy session's D261 denies, one with
//! #1549's calls and one with [`set_delete_child_deny`]; after every step
//! every object's owner, group, DACL and label, as SDDL, must be identical,
//! including objects outside the roots. Deliberately broken versions of the
//! direct write must make some case differ.
//!
//! The `d311_label_walk_` tests run two whole legacy commands the same way
//! (protected paths labelled Medium, the root labelled Low, the denies, the
//! root's label put back), with Windows' label walk on one side and ours on
//! the other; broken versions of the label walk must make a case differ.

use super::super::MUTANT;
use super::super::Mutant;
use super::*;
use crate::integrity::walk::FALLBACKS as LABEL_FALLBACKS;
use crate::integrity::walk::MUTANT as LABEL_MUTANT;
use crate::integrity::walk::Mutant as LabelMutant;
use pretty_assertions::assert_eq;
use std::ffi::OsString;
use std::os::windows::ffi::OsStringExt;
use std::sync::MutexGuard;
use windows_sys::Win32::Foundation::ERROR_NO_MORE_FILES;
use windows_sys::Win32::Foundation::HANDLE;
use windows_sys::Win32::Security::AddMandatoryAce;
use windows_sys::Win32::Security::Authorization::ConvertStringSecurityDescriptorToSecurityDescriptorW;
use windows_sys::Win32::Security::GROUP_SECURITY_INFORMATION;
use windows_sys::Win32::Security::LABEL_SECURITY_INFORMATION;
use windows_sys::Win32::Security::OWNER_SECURITY_INFORMATION;
use windows_sys::Win32::Security::SE_SACL_PROTECTED;
use windows_sys::Win32::Security::SetSecurityDescriptorSacl;
use windows_sys::Win32::Storage::FileSystem::ExtendedFileIdType;
use windows_sys::Win32::Storage::FileSystem::FILE_ATTRIBUTE_DIRECTORY;
use windows_sys::Win32::Storage::FileSystem::FILE_ATTRIBUTE_REPARSE_POINT;
use windows_sys::Win32::Storage::FileSystem::FILE_ID_128;
use windows_sys::Win32::Storage::FileSystem::FILE_ID_DESCRIPTOR;
use windows_sys::Win32::Storage::FileSystem::FILE_ID_DESCRIPTOR_0;
use windows_sys::Win32::Storage::FileSystem::FILE_ID_EXTD_DIR_INFO;
use windows_sys::Win32::Storage::FileSystem::FILE_LIST_DIRECTORY;
use windows_sys::Win32::Storage::FileSystem::FileIdExtdDirectoryInfo;
use windows_sys::Win32::Storage::FileSystem::FileIdExtdDirectoryRestartInfo;
use windows_sys::Win32::Storage::FileSystem::GetFileInformationByHandleEx;
use windows_sys::Win32::Storage::FileSystem::OpenFileById;
use windows_sys::Win32::Storage::FileSystem::SYNCHRONIZE;

/// Stands in for another writable root's capability SID.
const OTHER_CAPABILITY: &str = "S-1-5-21-2718281828-3141592653-1618033988-1002";

/// A legacy session with `.git` and `.codex` protected: the root's deny
/// (that folder only) and each protected folder's (inherited), added, then
/// removed.
const SESSION_B: [Step; 6] = [
    ("", false, true),
    (".git", true, true),
    (".codex", true, true),
    (".codex", true, false),
    (".git", true, false),
    ("", false, false),
];

/// A case's setup: changes `root` (and may create folders beside it, in its
/// side folder); returns false if it could not run here (a symbolic link
/// without the privilege to create one).
type Setup = fn(&Path, &Path) -> bool;

fn build_tree_b(root: &Path) {
    build_tree(root);
    std::fs::create_dir_all(root.join(".codex\\skills")).expect(".codex");
    std::fs::write(root.join(".codex\\config.toml"), b"c").expect("file");
    add_entry(
        &root.join(".codex"),
        CAPABILITY,
        DENY_ACCESS,
        FILE_GENERIC_WRITE | DELETE,
        OI | CI,
    );
    // A second worktree file to move around.
    std::fs::write(root.join("src\\m\\g.rs"), b"g").expect("file");
}

/// How one case went: steps whose trees differed, and how each direct step
/// wrote.
struct Outcome {
    differing: Vec<String>,
    written: Vec<Written>,
    /// How often the label walk fell back to Windows' on the direct side.
    label_fallbacks: usize,
}

/// `SYSTEM_MANDATORY_LABEL_NO_WRITE_UP`.
const NO_WRITE_UP: u32 = 0x1;

/// Writes `path`'s label SACL protected (inheriting nothing from above),
/// with one explicit `level` entry with `flags`, or none. The entries are
/// written with Windows' walk (so the objects below inherit them), then the
/// protection on the object alone: `PROTECTED_SACL_SECURITY_INFORMATION`
/// would need `SeSecurityPrivilege`, which a normal session lacks.
fn protect_label(path: &Path, label: Option<(&str, u32)>) {
    let mut buffer = vec![0_u32; 32];
    let acl = buffer.as_mut_ptr() as *mut ACL;
    // SAFETY: a buffer of the size given, a valid SID, path and descriptor.
    unsafe {
        assert_ne!(InitializeAcl(acl, 128, 2), 0, "InitializeAcl");
        if let Some((level, flags)) = label {
            let sid = LocalSid::from_string(level).expect("label SID");
            assert_ne!(
                AddMandatoryAce(acl, 2, flags, NO_WRITE_UP, sid.as_ptr()),
                0,
                "AddMandatoryAce"
            );
        }
        let code = SetNamedSecurityInfoW(
            to_wide(path).as_ptr() as *mut u16,
            SE_FILE_OBJECT,
            LABEL_SECURITY_INFORMATION,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            acl,
        );
        assert_eq!(code, ERROR_SUCCESS, "label {}", path.display());
        let mut sd: SECURITY_DESCRIPTOR = std::mem::zeroed();
        let psd = &mut sd as *mut SECURITY_DESCRIPTOR as *mut c_void;
        assert_ne!(
            InitializeSecurityDescriptor(psd, SECURITY_DESCRIPTOR_REVISION),
            0
        );
        assert_ne!(SetSecurityDescriptorSacl(psd, 1, acl, 0), 0);
        assert_ne!(
            SetSecurityDescriptorControl(psd, SE_SACL_PROTECTED, SE_SACL_PROTECTED),
            0
        );
        assert_ne!(
            SetFileSecurityW(to_wide(path).as_ptr(), LABEL_SECURITY_INFORMATION, psd),
            0,
            "protect the label of {}: {}",
            path.display(),
            std::io::Error::last_os_error()
        );
    }
    let sddl = full_sddl(path);
    assert!(
        sddl.contains("S:P"),
        "{} is not protected: {sddl}",
        path.display()
    );
}

/// A fresh base folder for a case's trees, holding the legacy sessions'
/// lock (#1825). A legacy session labels the temporary folder Low and grants
/// its capability SID there while it runs, and Windows carries both into
/// every folder below that inherits. One running alongside reaches one side
/// of a case before the other; the lock keeps out this process's sessions,
/// and the nextest group `windows_sandbox_legacy_sessions` (which every
/// `d311_` test is in) other processes'.
fn serialized_base() -> (MutexGuard<'static, ()>, tempfile::TempDir) {
    let guard = crate::unified_exec::tests::legacy_process_test_guard();
    (guard, isolated_base())
}

/// A fresh base folder that inherits nothing from above. A session can also
/// leave the temporary folder labelled after it (until a later sweep, when
/// it left processes running), and an inherited Low label hides a walk that
/// labels what it should not (`LinksFollowed`). The base's DACL and label
/// SACL are protected: its DACL is [`own_dacl`]'s, and a label inherited from
/// above is dropped.
fn isolated_base() -> tempfile::TempDir {
    let base = tempfile::tempdir().expect("base dir");
    own_dacl(base.path());
    protect_label(base.path(), None);
    base
}

/// Gives the (empty) folder `path` a protected DACL of its own: full control
/// for the user, SYSTEM and Administrators, inherited below. Not a copy of
/// the temporary folder's (#1870): a legacy workspace-write session grants
/// each new writable-root capability SID Modify there and keeps it, so a test
/// machine's temporary folder gains a grant per test home (about 25 per
/// whole-crate run on CI), and with a copy every object of a case's trees
/// carried them all. The walks' time grew with them (the label walk's deep
/// and wide cases went from 22 s to 100 s after two whole-crate runs) until
/// nextest killed them at 60 s. Auto-inherited (`AI`), as a real parent is.
fn own_dacl(path: &Path) {
    let user = crate::deny_read_sessions::current_user_sid().expect("user SID");
    let sddl = to_wide(format!(
        "D:PAI(A;OICI;FA;;;SY)(A;OICI;FA;;;BA)(A;OICI;FA;;;{user})"
    ));
    let mut psd: *mut c_void = std::ptr::null_mut();
    // SAFETY: a valid SDDL string and path; the descriptor is freed below.
    unsafe {
        assert_ne!(
            ConvertStringSecurityDescriptorToSecurityDescriptorW(
                sddl.as_ptr(),
                SDDL_REVISION_1,
                &mut psd,
                std::ptr::null_mut(),
            ),
            0,
            "parse the base's descriptor"
        );
        let set = SetFileSecurityW(
            to_wide(path).as_ptr(),
            DACL_SECURITY_INFORMATION | PROTECTED_DACL_SECURITY_INFORMATION,
            psd,
        );
        let error = std::io::Error::last_os_error();
        LocalFree(psd as HLOCAL);
        assert_ne!(set, 0, "set the DACL of {}: {error}", path.display());
    }
    // The DACL only: the label, which may still be inherited here, is
    // `protect_label`'s.
    let sddl = dacl_sddl(path);
    assert!(
        sddl.starts_with("D:P") && !sddl.contains("ID;"),
        "{} does not have its own DACL: {sddl}",
        path.display()
    );
}

/// Runs [`SESSION_B`] on both sides of a fresh base folder after `setup`.
/// `None` if the setup could not run here.
fn equality_case(name: &str, setup: Setup) -> Option<Outcome> {
    let (_guard, base) = serialized_base();
    equality_case_in(base.path(), name, &setup)
}

/// [`equality_case`] in `base`.
fn equality_case_in(
    base: &Path,
    name: &str,
    setup: &dyn Fn(&Path, &Path) -> bool,
) -> Option<Outcome> {
    // Snapshots read through the long-path form; the steps get the plain
    // form, as the sandbox passes roots.
    let verbatim = std::fs::canonicalize(base)
        .expect("canonical base")
        .join("t");
    let plain = dunce::simplified(&verbatim).to_path_buf();
    let sides = [plain.join("walked"), plain.join("direct")];
    for side in &sides {
        let root = side.join("root");
        std::fs::create_dir_all(&root).expect("root");
        build_tree_b(&root);
        if !setup(&root, side) {
            println!("d311 case={name} skipped: no symbolic link privilege");
            return None;
        }
    }
    let (walked, direct) = (verbatim.join("walked"), verbatim.join("direct"));
    let start = full_diff(&full_snapshot(&walked), &full_snapshot(&direct));
    assert!(
        start.is_empty(),
        "{name}: the trees start differently: {start:?}"
    );
    let mut outcome = Outcome {
        differing: Vec::new(),
        written: Vec::new(),
        label_fallbacks: 0,
    };
    for (folder, inherit, add) in SESSION_B {
        // SAFETY: a valid SID and existing folders.
        let how = unsafe {
            reference(&sides[0].join("root").join(folder), inherit, add);
            let sid = LocalSid::from_string(EVERYONE).expect("Everyone");
            set_delete_child_deny(
                &sides[1].join("root").join(folder),
                sid.as_ptr(),
                inherit,
                add,
            )
            .expect("direct write")
        };
        outcome.written.push(how);
        let diff = full_diff(&full_snapshot(&walked), &full_snapshot(&direct));
        if !diff.is_empty() {
            outcome.differing.push(format!(
                "({folder:?}, {inherit}, {add}) via {how:?}: {} objects, {:?}",
                diff.len(),
                diff.iter().take(3).collect::<Vec<_>>()
            ));
        }
    }
    println!(
        "d311 case={name} objects={} differing_steps={} written={:?}",
        full_snapshot(&direct).len(),
        outcome.differing.len(),
        outcome.written
    );
    Some(outcome)
}

/// Runs `cases`; fails on any difference. Returns how each case's direct
/// steps wrote.
fn assert_equal(cases: &[(&str, Setup)]) -> BTreeMap<String, Vec<Written>> {
    let mut failed = Vec::new();
    let mut written = BTreeMap::new();
    for (name, setup) in cases {
        if let Some(outcome) = equality_case(name, *setup) {
            if !outcome.differing.is_empty() {
                failed.push(format!("{name}: {:?}", outcome.differing));
            }
            written.insert((*name).to_string(), outcome.written);
        }
    }
    assert!(failed.is_empty(), "differing cases: {failed:#?}");
    written
}

fn full_diff(a: &BTreeMap<String, String>, b: &BTreeMap<String, String>) -> Vec<String> {
    let mut out = Vec::new();
    for (key, value) in a {
        match b.get(key) {
            Some(other) if other == value => {}
            Some(other) => out.push(format!("{key:?}: walk={value} direct={other}")),
            None => out.push(format!("{key:?}: only in walked")),
        }
    }
    for key in b.keys() {
        if !a.contains_key(key) {
            out.push(format!("{key:?}: only in direct"));
        }
    }
    out
}

/// Every object's owner, group, DACL and label below `base` (and `base`),
/// links not followed, by relative path. #1715: each entry is opened by its
/// file ID from its folder's listing, never by a path through a folder: a
/// folder whose reparse tag no filter handles (DFS, say) can't be listed or
/// passed through by path.
fn full_snapshot(base: &Path) -> BTreeMap<String, String> {
    let mut out = BTreeMap::new();
    let root = SnapshotHandle::open(base, /*folder*/ true);
    out.insert(String::new(), root.sddl(base));
    let mut pending = vec![(PathBuf::new(), root)];
    while let Some((relative, dir)) = pending.pop() {
        for (name, attributes, tag, id) in dir.entries(&relative) {
            let path = relative.join(OsString::from_wide(&name));
            // A folder that is not a link (a name surrogate).
            let walk_into = attributes & FILE_ATTRIBUTE_DIRECTORY != 0
                && (attributes & FILE_ATTRIBUTE_REPARSE_POINT == 0 || tag & 0x2000_0000 == 0);
            let entry = dir.open_by_id(id, walk_into, &path);
            out.insert(path.display().to_string(), entry.sddl(&path));
            if walk_into {
                pending.push((path, entry));
            }
        }
    }
    out
}

/// Owner, group, DACL and label of the object at `path` itself (a link's
/// own), as SDDL.
fn full_sddl(path: &Path) -> String {
    SnapshotHandle::open(path, /*folder*/ false).sddl(path)
}

/// A handle [`full_snapshot`] reads an object (and lists a folder) through,
/// closed on drop.
struct SnapshotHandle(HANDLE);

impl SnapshotHandle {
    /// `path` itself (a link's own), for listing too if `folder`.
    fn open(path: &Path, folder: bool) -> Self {
        let access = if folder {
            READ_CONTROL | FILE_LIST_DIRECTORY | SYNCHRONIZE
        } else {
            READ_CONTROL
        };
        // SAFETY: a valid path; the handle is closed on drop.
        let handle = unsafe {
            CreateFileW(
                to_wide(path).as_ptr(),
                access,
                FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
                std::ptr::null_mut(),
                OPEN_EXISTING,
                FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT,
                0,
            )
        };
        assert_ne!(handle, INVALID_HANDLE_VALUE, "open {}", path.display());
        Self(handle)
    }

    /// The entry with file ID `id` on this folder's volume, itself (a
    /// link's own), for listing too if `folder`.
    fn open_by_id(&self, id: FILE_ID_128, folder: bool, path: &Path) -> Self {
        let descriptor = FILE_ID_DESCRIPTOR {
            dwSize: std::mem::size_of::<FILE_ID_DESCRIPTOR>() as u32,
            Type: ExtendedFileIdType,
            Anonymous: FILE_ID_DESCRIPTOR_0 { ExtendedFileId: id },
        };
        let access = if folder {
            READ_CONTROL | FILE_LIST_DIRECTORY | SYNCHRONIZE
        } else {
            READ_CONTROL
        };
        // SAFETY: a valid descriptor; the handle is closed on drop.
        let handle = unsafe {
            OpenFileById(
                self.0,
                &descriptor,
                access,
                FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
                std::ptr::null(),
                FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT,
            )
        };
        assert_ne!(
            handle,
            INVALID_HANDLE_VALUE,
            "open {} by ID: {}",
            path.display(),
            std::io::Error::last_os_error()
        );
        Self(handle)
    }

    /// The folder's entries without `.` and `..`: name, attributes, reparse
    /// tag and file ID.
    fn entries(&self, path: &Path) -> Vec<(Vec<u16>, u32, u32, FILE_ID_128)> {
        let mut out = Vec::new();
        let mut buffer = vec![0_u64; 8192];
        let mut class = FileIdExtdDirectoryRestartInfo;
        loop {
            // SAFETY: a folder handle and a buffer of the size given.
            let ok = unsafe {
                GetFileInformationByHandleEx(
                    self.0,
                    class,
                    buffer.as_mut_ptr() as *mut c_void,
                    (buffer.len() * 8) as u32,
                )
            };
            if ok == 0 {
                let err = std::io::Error::last_os_error();
                assert_eq!(
                    err.raw_os_error(),
                    Some(ERROR_NO_MORE_FILES as i32),
                    "list {}: {err}",
                    path.display()
                );
                return out;
            }
            class = FileIdExtdDirectoryInfo;
            let mut offset = 0_usize;
            loop {
                // SAFETY: entries the call wrote, read unaligned.
                let (info, name) = unsafe {
                    let entry = (buffer.as_ptr() as *const u8).add(offset);
                    let info = std::ptr::read_unaligned(entry as *const FILE_ID_EXTD_DIR_INFO);
                    let name = entry.add(std::mem::offset_of!(FILE_ID_EXTD_DIR_INFO, FileName))
                        as *const u16;
                    let name: Vec<u16> = (0..info.FileNameLength as usize / 2)
                        .map(|i| std::ptr::read_unaligned(name.add(i)))
                        .collect();
                    (info, name)
                };
                let dot = u16::from(b'.');
                if name != [dot] && name != [dot, dot] {
                    out.push((name, info.FileAttributes, info.ReparsePointTag, info.FileId));
                }
                if info.NextEntryOffset == 0 {
                    break;
                }
                offset += info.NextEntryOffset as usize;
            }
        }
    }

    /// The object's owner, group, DACL and label, as SDDL.
    fn sddl(&self, path: &Path) -> String {
        let info = OWNER_SECURITY_INFORMATION
            | GROUP_SECURITY_INFORMATION
            | DACL_SECURITY_INFORMATION
            | LABEL_SECURITY_INFORMATION;
        // SAFETY: an open handle and valid out pointers; the buffers are
        // freed.
        unsafe {
            let mut sd: *mut c_void = std::ptr::null_mut();
            let code = GetSecurityInfo(
                self.0,
                SE_FILE_OBJECT,
                info,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                &mut sd,
            );
            assert_eq!(code, ERROR_SUCCESS, "GetSecurityInfo {}", path.display());
            let mut text: *mut u16 = std::ptr::null_mut();
            let ok = ConvertSecurityDescriptorToStringSecurityDescriptorW(
                sd,
                SDDL_REVISION_1,
                info,
                &mut text,
                std::ptr::null_mut(),
            );
            LocalFree(sd as HLOCAL);
            assert_ne!(ok, 0, "SDDL {}", path.display());
            let len = (0..).take_while(|&i| *text.add(i) != 0).count();
            let sddl = String::from_utf16_lossy(std::slice::from_raw_parts(text, len));
            LocalFree(text as HLOCAL);
            sddl
        }
    }
}

impl Drop for SnapshotHandle {
    fn drop(&mut self) {
        // SAFETY: a handle this type opened.
        unsafe {
            CloseHandle(self.0);
        }
    }
}

/// `mklink` with `args`; false if the account may not create symbolic links
/// (a normal session without developer mode), unless
/// `CORBANU_TEST_REQUIRE_SYMLINKS` is set (the elevated CI run).
fn mklink(args: &[&str], link: &Path, target: &Path) -> bool {
    let output = Command::new("cmd.exe")
        .args(["/d", "/c", "mklink"])
        .args(args)
        .arg(link)
        .arg(target)
        .output()
        .expect("run mklink");
    if output.status.success() {
        return true;
    }
    let symbolic = !args.contains(&"/J") && !args.contains(&"/H");
    assert!(
        symbolic && std::env::var_os("CORBANU_TEST_REQUIRE_SYMLINKS").is_none(),
        "mklink {args:?}: {output:?}"
    );
    false
}

/// A folder beside the root labelled Low (inherited by what is created in
/// it), as another session's root is.
fn low_root(side: &Path) -> PathBuf {
    let low = side.join("other-low-root");
    std::fs::create_dir_all(&low).expect("low root");
    // SAFETY: an existing folder.
    unsafe {
        assert!(crate::integrity::label_writable_root_low(&low).expect("label low root"));
    }
    low
}

/// A chain of `depth` folders named `name` under `src\deep`, with a file
/// every 25 levels and at the end.
fn deep_chain(root: &Path, depth: usize, name: &str) {
    let mut dir = root.join("src\\deep");
    for level in 0..depth {
        dir = dir.join(name);
        if level % 25 == 24 {
            std::fs::create_dir_all(&dir).expect("deep dir");
            std::fs::write(dir.join("f.rs"), b"d").expect("deep file");
        }
    }
    std::fs::create_dir_all(&dir).expect("deep dir");
    std::fs::write(dir.join("leaf.rs"), b"d").expect("deep file");
}

fn rename(from: &Path, to: &Path) {
    std::fs::rename(from, to).expect("move");
}

fn plain(_: &Path, _: &Path) -> bool {
    true
}

fn file_moved_into_git(root: &Path, _: &Path) -> bool {
    rename(
        &root.join("src\\m\\f.rs"),
        &root.join(".git\\objects\\ab\\moved"),
    );
    true
}

fn root_itself_stale(root: &Path, _: &Path) -> bool {
    add_stale_inherited_entry(root);
    true
}

/// perf-label's ten D298 cases (0 is the plain tree).
const BASIC_CASES: [(&str, Setup); 11] = [
    ("0-plain", plain),
    ("1-file-moved-into-git", file_moved_into_git),
    ("2-folder-moved-into-git", |root, _| {
        std::fs::write(root.join("src\\m\\deep\\f.rs"), b"d").expect("file");
        rename(
            &root.join("src\\m\\deep"),
            &root.join(".git\\refs\\heads\\deep"),
        );
        true
    }),
    ("3-file-moved-into-codex", |root, _| {
        rename(
            &root.join("src\\m\\f.rs"),
            &root.join(".codex\\skills\\moved"),
        );
        true
    }),
    ("4-stale-file-under-git", |root, _| {
        add_stale_inherited_entry(&root.join(".git\\objects\\ab\\cdef"));
        true
    }),
    ("5-root-itself-stale", root_itself_stale),
    ("6-codex-itself-stale", |root, _| {
        add_stale_inherited_entry(&root.join(".codex"));
        true
    }),
    ("7-file-moved-out-of-git", |root, _| {
        std::fs::write(root.join(".git\\objects\\ab\\out"), b"o").expect("file");
        rename(
            &root.join(".git\\objects\\ab\\out"),
            &root.join("src\\m\\head-copy"),
        );
        true
    }),
    ("8-stale-grant-on-worktree-file", |root, _| {
        add_stale_inherited_entry(&root.join("src\\m\\g.rs"));
        true
    }),
    (
        "9-file-moved-in-from-another-writable-root",
        |root, side| {
            let outside = side.join("outside9");
            std::fs::create_dir_all(&outside).expect("outside");
            add_entry(
                &outside,
                OTHER_CAPABILITY,
                GRANT_ACCESS,
                FILE_GENERIC_WRITE | DELETE,
                OI | CI,
            );
            std::fs::write(outside.join("o.rs"), b"o").expect("file");
            rename(&outside.join("o.rs"), &root.join("src\\m\\from-other"));
            true
        },
    ),
    ("10-file-moved-in-from-outside", |root, side| {
        let outside = side.join("outside10");
        std::fs::create_dir_all(&outside).expect("outside");
        std::fs::write(outside.join("p.rs"), b"p").expect("file");
        rename(&outside.join("p.rs"), &root.join("src\\m\\from-outside"));
        true
    }),
];

/// Seven kinds of link; none may be followed.
const LINK_CASES: [(&str, Setup); 7] = [
    ("L1-junction-in-worktree-to-outside", |root, side| {
        let target = side.join("junction-target");
        std::fs::create_dir_all(target.join("inner")).expect("target");
        std::fs::write(target.join("inner\\t.rs"), b"t").expect("file");
        mklink(&["/J"], &root.join("src\\m\\jn"), &target)
    }),
    ("L2-junction-in-git-to-outside", |root, side| {
        let target = side.join("junction-target");
        std::fs::create_dir_all(target.join("inner")).expect("target");
        mklink(&["/J"], &root.join(".git\\objects\\jn"), &target)
    }),
    ("L3-dir-symlink-to-outside", |root, side| {
        let target = side.join("dir-symlink-target");
        std::fs::create_dir_all(&target).expect("target");
        std::fs::write(target.join("t.rs"), b"t").expect("file");
        mklink(&["/D"], &root.join("src\\dl"), &target)
    }),
    ("L4-file-symlink-to-outside", |root, side| {
        let target = side.join("file-symlink-target.rs");
        std::fs::write(&target, b"t").expect("file");
        mklink(&[], &root.join("src\\fl.rs"), &target)
    }),
    ("L5-hard-link-to-outside-file", |root, side| {
        let target = side.join("hard-link-target.rs");
        std::fs::write(&target, b"t").expect("file");
        mklink(&["/H"], &root.join("src\\hl.rs"), &target)
    }),
    ("L6-junction-loop-to-root", |root, _| {
        mklink(&["/J"], &root.join("src\\loop"), root)
    }),
    ("L7-junction-to-git", |root, _| {
        mklink(&["/J"], &root.join("gitlink"), &root.join(".git"))
    }),
];

/// Integrity labels the DACL check must leave alone.
const LABEL_CASES: [(&str, Setup); 4] = [
    ("S1-file-moved-in-from-a-low-root", |root, side| {
        let low = low_root(side);
        std::fs::write(low.join("l.rs"), b"l").expect("file");
        rename(&low.join("l.rs"), &root.join("src\\m\\from-low"));
        true
    }),
    ("S2-folder-moved-into-git-from-a-low-root", |root, side| {
        let low = low_root(side);
        std::fs::create_dir_all(low.join("d\\e")).expect("dirs");
        std::fs::write(low.join("d\\e\\l.rs"), b"l").expect("file");
        rename(&low.join("d"), &root.join(".git\\refs\\from-low"));
        true
    }),
    ("S3-explicit-label-on-a-file-and-a-folder", |root, _| {
        // SAFETY: existing objects.
        unsafe {
            crate::integrity::label_protected_path_medium(&root.join("src\\m\\g.rs"))
                .expect("file label");
            crate::integrity::label_protected_path_medium(&root.join("src\\m\\deep"))
                .expect("folder label");
        }
        true
    }),
    ("S4-explicit-label-on-git-with-a-low-child", |root, side| {
        let low = low_root(side);
        // SAFETY: an existing folder.
        unsafe {
            crate::integrity::label_protected_path_medium(&root.join(".git\\objects"))
                .expect("folder label");
        }
        std::fs::write(low.join("s.rs"), b"s").expect("file");
        rename(&low.join("s.rs"), &root.join(".git\\objects\\stale-low"));
        true
    }),
];

fn wide_tree(root: &Path, _: &Path) -> bool {
    let wide = root.join("src\\wide");
    std::fs::create_dir_all(&wide).expect("dir");
    for f in 0..3000 {
        std::fs::write(
            wide.join(format!("file-with-a-longer-name-{f:05}.rs")),
            b"w",
        )
        .expect("file");
    }
    for d in 0..300 {
        let dir = root.join(format!("src\\many\\d{d:03}"));
        std::fs::create_dir_all(&dir).expect("dir");
        for f in 0..10 {
            std::fs::write(dir.join(format!("f{f}.rs")), b"w").expect("file");
        }
    }
    true
}

/// Deep and wide trees: D313's handle-relative opens must not fall back on
/// paths past `MAX_PATH`; a stale file listed after 3,000 others (past the
/// first listing buffer) must still be found.
const SIZE_CASES: [(&str, Setup); 4] = [
    ("D1-deep-100", |root, _| {
        deep_chain(root, 100, "d");
        true
    }),
    ("D2-deep-300-past-max-path", |root, _| {
        deep_chain(root, 300, "dddddd");
        true
    }),
    ("W1-wide-3000-files-300-folders", wide_tree),
    ("W2-stale-file-after-3000-names", |root, _| {
        // Created in place, so inherited as they should be; then one stale
        // file moved in, which sorts (upper-cased, "Z" after "F") after the
        // 3,000 others.
        let wide = root.join(".git\\objects\\wide");
        std::fs::create_dir_all(&wide).expect("dir");
        for f in 0..3000 {
            std::fs::write(
                wide.join(format!("file-with-a-longer-name-{f:05}.rs")),
                b"w",
            )
            .expect("file");
        }
        rename(&root.join("src\\m\\g.rs"), &wide.join("zzzz-stale.rs"));
        true
    }),
];

#[test]
fn d311_basic_cases_match_the_tree_walk() {
    let written = assert_equal(&BASIC_CASES);
    // Nothing stale: no fallback.
    assert_eq!(written["0-plain"], vec![Written::Direct; SESSION_B.len()]);
}

#[test]
fn d311_link_cases_match_the_tree_walk() {
    assert_equal(&LINK_CASES);
}

#[test]
fn d311_label_cases_match_the_tree_walk() {
    assert_equal(&LABEL_CASES);
}

#[test]
fn d311_deep_and_wide_cases_match_the_tree_walk_without_falling_back() {
    let written = assert_equal(&SIZE_CASES);
    for name in [
        "D1-deep-100",
        "D2-deep-300-past-max-path",
        "W1-wide-3000-files-300-folders",
    ] {
        assert_eq!(
            written[name],
            vec![Written::Direct; SESSION_B.len()],
            "{name}"
        );
    }
    // The moved-in file is stale: found at the first step, which walks.
    assert_eq!(
        written["W2-stale-file-after-3000-names"][0],
        Written::Walked
    );
}

/// Breaks the direct write on this thread until dropped.
struct Broken;

impl Broken {
    fn set(mutant: Mutant) -> Self {
        MUTANT.with(|cell| cell.set(Some(mutant)));
        Self
    }
}

impl Drop for Broken {
    fn drop(&mut self) {
        MUTANT.with(|cell| cell.set(None));
    }
}

/// Each deliberately broken version of the direct write leaves some object
/// different from the walk's in the case that exercises it.
#[test]
fn d311_broken_versions_are_caught() {
    let mutants: [(Mutant, &str, Setup); 6] = [
        (
            Mutant::FilesUnchecked,
            "1-file-moved-into-git",
            file_moved_into_git,
        ),
        (Mutant::SubfoldersUnwalked, "0-plain", plain),
        (
            Mutant::StaleIgnored,
            "1-file-moved-into-git",
            file_moved_into_git,
        ),
        (Mutant::CopiesUnwritten, "0-plain", plain),
        (
            Mutant::TargetUnchecked,
            "5-root-itself-stale",
            root_itself_stale,
        ),
        (Mutant::ListingTruncated, SIZE_CASES[3].0, SIZE_CASES[3].1),
    ];
    let mut missed = Vec::new();
    for (mutant, name, setup) in mutants {
        let outcome = {
            let _broken = Broken::set(mutant);
            equality_case(&format!("{mutant:?}/{name}"), setup)
        };
        let outcome = outcome.expect("the case runs everywhere");
        if outcome.differing.is_empty() {
            missed.push(mutant);
        }
    }
    assert!(missed.is_empty(), "broken versions not caught: {missed:?}");
}

/// One step of a legacy command, done the same way on both sides; returns
/// what it reported.
type CommandStep = fn(&Path) -> String;

fn step_protect(root: &Path) -> String {
    // SAFETY: existing folders.
    unsafe {
        format!(
            "{:?}/{:?}",
            crate::integrity::label_protected_path_medium(&root.join(".git")).is_ok(),
            crate::integrity::label_protected_path_medium(&root.join(".codex")).is_ok()
        )
    }
}

fn step_label(root: &Path) -> String {
    // SAFETY: an existing folder.
    let result = unsafe { crate::integrity::label_writable_root_low(root) };
    format!("{:?}", result.map_err(|err| err.to_string()))
}

fn step_restore(root: &Path) -> String {
    // The root had no explicit label before (as in a fresh workspace).
    // SAFETY: an existing folder.
    let result = unsafe { crate::integrity::set_explicit_labels(root, &[]) };
    format!("{:?}", result.map_err(|err| err.to_string()))
}

/// Runs two legacy commands on both sides of a fresh base folder after
/// `setup`: Windows' label walk and #1549's deny calls on one, ours (with
/// `mutant`, if any) on the other. `None` if the setup could not run here.
fn command_case(name: &str, setup: Setup, mutant: Option<LabelMutant>) -> Option<Outcome> {
    let (_guard, base) = serialized_base();
    command_case_in(base.path(), name, &setup, mutant)
}

/// [`command_case`] in `base`.
fn command_case_in(
    base: &Path,
    name: &str,
    setup: &dyn Fn(&Path, &Path) -> bool,
    mutant: Option<LabelMutant>,
) -> Option<Outcome> {
    let verbatim = std::fs::canonicalize(base)
        .expect("canonical base")
        .join("t");
    let plain = dunce::simplified(&verbatim).to_path_buf();
    let sides = [plain.join("walked"), plain.join("direct")];
    // Both trees are built with Windows' walk.
    LABEL_MUTANT.with(|cell| cell.set(Some(LabelMutant::WindowsWalk)));
    for side in &sides {
        let root = side.join("root");
        std::fs::create_dir_all(&root).expect("root");
        build_tree_b(&root);
        if !setup(&root, side) {
            LABEL_MUTANT.with(|cell| cell.set(None));
            println!("d311 label case={name} skipped: no symbolic link privilege");
            return None;
        }
    }
    LABEL_MUTANT.with(|cell| cell.set(None));
    let (walked, direct) = (verbatim.join("walked"), verbatim.join("direct"));
    let (walked_root, direct_root) = (sides[0].join("root"), sides[1].join("root"));
    let start = full_diff(&full_snapshot(&walked), &full_snapshot(&direct));
    assert!(
        start.is_empty(),
        "{name}: the trees start differently: {start:?}"
    );
    let mut outcome = Outcome {
        differing: Vec::new(),
        written: Vec::new(),
        label_fallbacks: 0,
    };
    let mut log = Vec::new();
    let check = |what: String, outcome: &mut Outcome| {
        let diff = full_diff(&full_snapshot(&walked), &full_snapshot(&direct));
        if !diff.is_empty() {
            outcome.differing.push(format!(
                "{what}: {} objects, {:?}",
                diff.len(),
                diff.iter().take(3).collect::<Vec<_>>()
            ));
        }
    };
    let both = |step: CommandStep| -> String {
        LABEL_MUTANT.with(|cell| cell.set(Some(LabelMutant::WindowsWalk)));
        let w = step(&walked_root);
        LABEL_MUTANT.with(|cell| cell.set(mutant));
        let d = step(&direct_root);
        LABEL_MUTANT.with(|cell| cell.set(None));
        // `both` only reads; the count is taken around each command below.
        if w == d {
            w
        } else {
            format!("walk={w} direct={d}")
        }
    };
    let fallbacks_before = LABEL_FALLBACKS.with(std::cell::Cell::get);
    for command in 0..2 {
        if command == 0 {
            // The first command keeps `.git` and `.codex` Medium.
            log.push(format!("protect={}", both(step_protect)));
            check(format!("{command}:protect"), &mut outcome);
        }
        log.push(format!("label={}", both(step_label)));
        check(format!("{command}:label"), &mut outcome);
        for (i, (folder, inherit, add)) in SESSION_B.into_iter().enumerate() {
            // SAFETY: a valid SID and existing folders.
            let how = unsafe {
                reference(&walked_root.join(folder), inherit, add);
                let sid = LocalSid::from_string(EVERYONE).expect("Everyone");
                set_delete_child_deny(&direct_root.join(folder), sid.as_ptr(), inherit, add)
                    .expect("direct write")
            };
            outcome.written.push(how);
            check(format!("{command}:deny {i}"), &mut outcome);
        }
        log.push(format!("restore={}", both(step_restore)));
        check(format!("{command}:restore"), &mut outcome);
    }
    // Only the direct side's steps run our walk.
    outcome.label_fallbacks = LABEL_FALLBACKS.with(std::cell::Cell::get) - fallbacks_before;
    println!(
        "d311 label case={name} objects={} differing_steps={} label_fallbacks={} [{}]",
        full_snapshot(&direct).len(),
        outcome.differing.len(),
        outcome.label_fallbacks,
        log.join(" ")
    );
    Some(outcome)
}

fn assert_commands_equal(cases: &[(&str, Setup)]) {
    let mut failed = Vec::new();
    for (name, setup) in cases {
        if let Some(outcome) = command_case(name, *setup, None)
            && (!outcome.differing.is_empty() || outcome.label_fallbacks != 0)
        {
            // Our walk must have done the work, not Windows' fallback.
            failed.push(format!(
                "{name}: fallbacks={} {:?}",
                outcome.label_fallbacks, outcome.differing
            ));
        }
    }
    assert!(failed.is_empty(), "differing cases: {failed:#?}");
}

#[test]
fn d311_label_walk_basic_cases_match_windows_walk() {
    assert_commands_equal(&BASIC_CASES);
}

#[test]
fn d311_label_walk_link_cases_match_windows_walk() {
    assert_commands_equal(&LINK_CASES);
}

#[test]
fn d311_label_walk_label_cases_match_windows_walk() {
    assert_commands_equal(&LABEL_CASES);
}

#[test]
fn d311_label_walk_deep_and_wide_cases_match_windows_walk() {
    assert_commands_equal(&SIZE_CASES);
}

/// Each deliberately broken version of the label walk leaves some object
/// different from Windows' walk.
#[test]
fn d311_label_walk_broken_versions_are_caught() {
    let (name, junction) = LINK_CASES[0];
    let mut missed = Vec::new();
    for mutant in [
        LabelMutant::FilesUnwritten,
        LabelMutant::SubfoldersUnwalked,
        LabelMutant::AutoInheritDropped,
        LabelMutant::LinksFollowed,
    ] {
        let outcome = command_case(&format!("{mutant:?}/{name}"), junction, Some(mutant))
            .expect("a junction needs no privilege");
        if outcome.differing.is_empty() {
            missed.push(mutant);
        }
    }
    assert!(missed.is_empty(), "broken versions not caught: {missed:?}");
}

/// Cloud-file (OneDrive) placeholder folders are reparse points but not
/// links: Windows' walk goes into them, and so must the check, which also
/// finds a stale file inside one.
#[test]
fn d311_cloud_placeholder_folders_match_the_tree_walk() {
    use crate::unified_exec::tests::cloud_placeholder::ExposePlaceholders;
    use crate::unified_exec::tests::cloud_placeholder::SyncRoot;
    use crate::unified_exec::tests::cloud_placeholder::is_placeholder;
    let _guard = crate::unified_exec::tests::legacy_process_test_guard();
    let _exposed = ExposePlaceholders::new();
    // Under `src`, then with a stale file inside, then under `.git`, where
    // the check writes the delete-child copies through placeholders.
    for (folder, stale) in [
        ("src\\cloud", false),
        ("src\\cloud", true),
        (".git\\objects\\cloud", false),
    ] {
        let base = isolated_base();
        let sync_roots = std::cell::RefCell::new(Vec::new());
        let setup = |root: &Path, _: &Path| {
            let cloud = root.join(folder);
            std::fs::create_dir_all(&cloud).expect("sync root");
            let sync_root = SyncRoot::register(&cloud);
            sync_root.placeholder_folder("a");
            sync_root.placeholder_folder("p");
            std::fs::create_dir_all(cloud.join("a\\b")).expect("folder");
            std::fs::write(cloud.join("a\\b\\f.rs"), b"f").expect("file");
            std::fs::write(cloud.join("p\\g.rs"), b"g").expect("file");
            assert!(is_placeholder(&cloud) && is_placeholder(&cloud.join("a")));
            if stale {
                add_stale_inherited_entry(&cloud.join("a\\b\\f.rs"));
            }
            sync_roots.borrow_mut().push(sync_root);
            true
        };
        let name = format!("{folder}{}", if stale { "-stale" } else { "" });
        let outcome = equality_case_in(base.path(), &name, &setup).expect("runs everywhere");
        drop(sync_roots);
        assert!(
            outcome.differing.is_empty(),
            "{name}: {:?}",
            outcome.differing
        );
        if stale {
            assert_eq!(outcome.written[0], Written::Walked, "{name}");
        } else {
            // Nothing falls back, writes through placeholders included.
            assert_eq!(
                outcome.written,
                vec![Written::Direct; SESSION_B.len()],
                "{name}"
            );
        }
    }
}

/// The same for the label walk: placeholder folders get the labels Windows'
/// walk gives what is inside them.
#[test]
fn d311_label_walk_cloud_placeholder_folders_match_windows_walk() {
    use crate::unified_exec::tests::cloud_placeholder::ExposePlaceholders;
    use crate::unified_exec::tests::cloud_placeholder::SyncRoot;
    let _guard = crate::unified_exec::tests::legacy_process_test_guard();
    let _exposed = ExposePlaceholders::new();
    let base = isolated_base();
    let sync_roots = std::cell::RefCell::new(Vec::new());
    let setup = |root: &Path, _: &Path| {
        let cloud = root.join("src\\cloud");
        std::fs::create_dir_all(&cloud).expect("sync root");
        let sync_root = SyncRoot::register(&cloud);
        sync_root.placeholder_folder("a");
        std::fs::create_dir_all(cloud.join("a\\b")).expect("folder");
        std::fs::write(cloud.join("a\\b\\f.rs"), b"f").expect("file");
        sync_roots.borrow_mut().push(sync_root);
        true
    };
    let outcome =
        command_case_in(base.path(), "P1-placeholders", &setup, None).expect("runs everywhere");
    drop(sync_roots);
    assert!(outcome.differing.is_empty(), "{:?}", outcome.differing);
    assert_eq!(outcome.label_fallbacks, 0, "our walk did the work");
}

/// #1707 review: when our walk fails partway (some labels already written),
/// Windows' walk run after it leaves every object as it alone would.
#[test]
fn d311_label_walk_failing_partway_is_repaired_by_the_fallback() {
    let cases: [(&str, Setup); 4] = [BASIC_CASES[0], BASIC_CASES[2], LINK_CASES[0], SIZE_CASES[2]];
    let mut failed = Vec::new();
    for (name, setup) in cases {
        let outcome = command_case(
            &format!("FailsPartway/{name}"),
            setup,
            Some(LabelMutant::FailsPartway),
        )
        .expect("no privilege needed");
        if !outcome.differing.is_empty() || outcome.label_fallbacks == 0 {
            failed.push(format!(
                "{name}: fallbacks={} {:?}",
                outcome.label_fallbacks, outcome.differing
            ));
        }
    }
    assert!(failed.is_empty(), "{failed:#?}");
}

/// The label registry's Medium no-read/write/execute-up label and putting
/// back a root's earlier explicit label, as Windows' walk leaves them.
#[test]
fn d311_label_walk_private_folder_and_earlier_label_match_windows_walk() {
    use crate::integrity::LabelAce;
    let (_guard, base) = serialized_base();
    let verbatim = std::fs::canonicalize(base.path())
        .expect("canonical base")
        .join("t");
    let plain = dunce::simplified(&verbatim).to_path_buf();
    let roots = [plain.join("walked\\root"), plain.join("direct\\root")];
    LABEL_MUTANT.with(|cell| cell.set(Some(LabelMutant::WindowsWalk)));
    for root in &roots {
        std::fs::create_dir_all(root).expect("root");
        build_tree_b(root);
    }
    LABEL_MUTANT.with(|cell| cell.set(None));
    let (walked, direct) = (verbatim.join("walked"), verbatim.join("direct"));
    let medium = LabelAce {
        flags: 0x3, // OBJECT_INHERIT_ACE | CONTAINER_INHERIT_ACE
        mask: 0x1,  // SYSTEM_MANDATORY_LABEL_NO_WRITE_UP
        sid: "S-1-16-8192".to_string(),
    };
    type Step = Box<dyn Fn(&Path) -> String>;
    let steps: Vec<(&str, Step)> = vec![
        (
            "private",
            Box::new(|root: &Path| {
                // SAFETY: an existing folder.
                let result =
                    unsafe { crate::integrity::label_private_dir_medium(&root.join(".codex")) };
                format!("{:?}", result.map_err(|err| err.to_string()))
            }),
        ),
        (
            "earlier-medium",
            Box::new(move |root: &Path| {
                // SAFETY: an existing folder.
                let result = unsafe {
                    crate::integrity::set_explicit_labels(root, std::slice::from_ref(&medium))
                };
                format!("{:?}", result.map_err(|err| err.to_string()))
            }),
        ),
        ("label", Box::new(|root: &Path| step_label(root))),
        (
            "restore-medium",
            Box::new(|root: &Path| {
                let medium = LabelAce {
                    flags: 0x3,
                    mask: 0x1,
                    sid: "S-1-16-8192".to_string(),
                };
                // SAFETY: an existing folder.
                let result = unsafe {
                    crate::integrity::set_explicit_labels(root, std::slice::from_ref(&medium))
                };
                format!("{:?}", result.map_err(|err| err.to_string()))
            }),
        ),
    ];
    let mut differing = Vec::new();
    let fallbacks_before = LABEL_FALLBACKS.with(std::cell::Cell::get);
    for (name, step) in &steps {
        LABEL_MUTANT.with(|cell| cell.set(Some(LabelMutant::WindowsWalk)));
        let w = step(&roots[0]);
        LABEL_MUTANT.with(|cell| cell.set(None));
        let d = step(&roots[1]);
        assert_eq!(w, d, "{name}");
        let diff = full_diff(&full_snapshot(&walked), &full_snapshot(&direct));
        if !diff.is_empty() {
            differing.push(format!(
                "{name}: {:?}",
                diff.iter().take(3).collect::<Vec<_>>()
            ));
        }
    }
    assert!(differing.is_empty(), "{differing:#?}");
    assert_eq!(
        LABEL_FALLBACKS.with(std::cell::Cell::get) - fallbacks_before,
        0,
        "our walk did the work"
    );
}

/// #1707 review r2: the order a legacy command uses (the root labelled Low
/// first, then its protected paths Medium below it), a protected path that
/// is a file (a worktree's `.git`) and a root nested in a Low root, as
/// Windows' walk leaves them.
#[test]
fn d311_label_walk_production_order_and_file_roots_match_windows_walk() {
    let (_guard, base) = serialized_base();
    let verbatim = std::fs::canonicalize(base.path())
        .expect("canonical base")
        .join("t");
    let plain = dunce::simplified(&verbatim).to_path_buf();
    let roots = [plain.join("walked\\root"), plain.join("direct\\root")];
    LABEL_MUTANT.with(|cell| cell.set(Some(LabelMutant::WindowsWalk)));
    for root in &roots {
        std::fs::create_dir_all(root.join("wt\\src")).expect("worktree");
        build_tree_b(root);
        std::fs::write(root.join("wt\\.git"), b"gitdir: ../.git/worktrees/wt\n")
            .expect(".git file");
        std::fs::write(root.join("wt\\src\\w.rs"), b"w").expect("file");
    }
    LABEL_MUTANT.with(|cell| cell.set(None));
    let (walked, direct) = (verbatim.join("walked"), verbatim.join("direct"));
    let steps: [(&str, CommandStep); 5] = [
        ("label", step_label),
        ("protect", step_protect),
        ("protect-file", |root| {
            // SAFETY: an existing file.
            let result =
                unsafe { crate::integrity::label_protected_path_medium(&root.join("wt\\.git")) };
            format!("{:?}", result.map_err(|err| err.to_string()))
        }),
        ("nested-root", |root| step_label(&root.join("wt"))),
        ("restore", step_restore),
    ];
    let mut differing = Vec::new();
    let fallbacks_before = LABEL_FALLBACKS.with(std::cell::Cell::get);
    for (name, step) in steps {
        LABEL_MUTANT.with(|cell| cell.set(Some(LabelMutant::WindowsWalk)));
        let w = step(&roots[0]);
        LABEL_MUTANT.with(|cell| cell.set(None));
        let d = step(&roots[1]);
        assert_eq!(w, d, "{name}");
        let diff = full_diff(&full_snapshot(&walked), &full_snapshot(&direct));
        if !diff.is_empty() {
            differing.push(format!(
                "{name}: {:?}",
                diff.iter().take(3).collect::<Vec<_>>()
            ));
        }
    }
    assert!(differing.is_empty(), "{differing:#?}");
    assert_eq!(
        LABEL_FALLBACKS.with(std::cell::Cell::get) - fallbacks_before,
        0,
        "our walk did the work"
    );
}

#[path = "acl_delete_child_walk_corner_tests.rs"]
mod corners;
