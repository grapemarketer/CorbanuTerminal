//! #1715: corners of our walks that #1688's and #1707's reviews found
//! untested, now compared with Windows' walks like the other D311 cases:
//! - roots and children with a protected label SACL, with stale objects
//!   below a protected folder;
//! - reparse points other than links and cloud-file placeholders (ProjFS,
//!   WCI, DFS), which both walks treat as plain files and folders. The tags
//!   are set directly, with no provider running for them; a running ProjFS
//!   or WCI provider may present its folders differently;
//! - D261's check of a root swapped for another folder (its file-ID guard)
//!   or for a link (its folder guard) between the write and the check,
//!   which must check nothing.
//!
//! A root that is a link is not here: Windows' walks write the link itself,
//! while ours follow it (#1722; production callers never pass one).

use super::*;
use crate::acl::walk::Handle;
use pretty_assertions::assert_eq;
use pretty_assertions::assert_ne;
use windows_sys::Win32::Security::AddMandatoryAce;
use windows_sys::Win32::Security::PROTECTED_SACL_SECURITY_INFORMATION;
use windows_sys::Win32::Storage::FileSystem::WRITE_DAC;
use windows_sys::Win32::System::IO::DeviceIoControl;

const MEDIUM: &str = "S-1-16-8192";
const GENERIC_WRITE: u32 = 0x4000_0000;
/// `SYSTEM_MANDATORY_LABEL_NO_WRITE_UP`.
const NO_WRITE_UP: u32 = 0x1;

/// Writes `path`'s label SACL protected (inheriting nothing from above),
/// with one explicit `level` entry with `flags`, or none.
fn protect_label(path: &Path, label: Option<(&str, u32)>) {
    let mut buffer = vec![0_u32; 32];
    let acl = buffer.as_mut_ptr() as *mut ACL;
    // SAFETY: a buffer of the size given, a valid SID and path.
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
            LABEL_SECURITY_INFORMATION | PROTECTED_SACL_SECURITY_INFORMATION,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            acl,
        );
        assert_eq!(
            code,
            ERROR_SUCCESS,
            "protect the label of {}",
            path.display()
        );
    }
    let sddl = full_sddl(path);
    assert!(
        sddl.contains("S:P"),
        "{} is not protected: {sddl}",
        path.display()
    );
}

/// Protected label SACLs below the root: a folder with a protected Medium
/// label and one with none, each with stale objects (moved in from a Low
/// root) below it, a protected file and a protected folder in `.git`.
fn protected_children(root: &Path, side: &Path) -> bool {
    let low = low_root(side);
    let protected = root.join("src\\prot");
    std::fs::create_dir_all(protected.join("a")).expect("dirs");
    std::fs::write(protected.join("a\\f.rs"), b"f").expect("file");
    protect_label(&protected, Some((MEDIUM, OI | CI)));
    std::fs::write(low.join("l.rs"), b"l").expect("file");
    rename(&low.join("l.rs"), &protected.join("a\\from-low"));
    std::fs::create_dir_all(low.join("d\\e")).expect("dirs");
    std::fs::write(low.join("d\\e\\l.rs"), b"l").expect("file");
    rename(&low.join("d"), &protected.join("from-low-dir"));
    let bare = root.join("src\\bare");
    std::fs::create_dir_all(bare.join("b")).expect("dirs");
    std::fs::write(bare.join("b\\f.rs"), b"f").expect("file");
    protect_label(&bare, None);
    std::fs::write(low.join("m.rs"), b"m").expect("file");
    rename(&low.join("m.rs"), &bare.join("b\\from-low"));
    std::fs::write(root.join("src\\m\\protected.rs"), b"p").expect("file");
    protect_label(&root.join("src\\m\\protected.rs"), Some((MEDIUM, 0)));
    protect_label(&root.join(".git\\objects\\ab"), None);
    std::fs::write(low.join("g"), b"g").expect("file");
    rename(&low.join("g"), &root.join(".git\\objects\\ab\\from-low"));
    true
}

/// The root's own label SACL protected, with no entry (`label` `None`) or
/// a Medium one, and a stale file below it.
fn protected_root(root: &Path, side: &Path, label: Option<(&str, u32)>) -> bool {
    protect_label(root, label);
    let low = low_root(side);
    std::fs::write(low.join("l.rs"), b"l").expect("file");
    rename(&low.join("l.rs"), &root.join("src\\m\\from-low"));
    true
}

/// Protected SACLs: the ones below the root and the root's own.
const PROTECTED_CASES: [(&str, Setup); 3] = [
    (
        "R1-protected-children-with-stale-objects-below",
        protected_children,
    ),
    ("R2-root-protected-without-a-label", |root, side| {
        protected_root(root, side, None)
    }),
    ("R3-root-protected-with-a-medium-label", |root, side| {
        protected_root(root, side, Some((MEDIUM, OI | CI)))
    }),
];

/// #1715: objects with a protected label SACL, and the stale objects below
/// a protected folder, end as Windows' walk leaves them, through two whole
/// legacy commands, without falling back.
#[test]
fn d311_label_walk_protected_sacls_match_windows_walk() {
    assert_commands_equal(&PROTECTED_CASES);
}

/// `FSCTL_SET_REPARSE_POINT`.
const FSCTL_SET_REPARSE_POINT: u32 = 0x0009_00A4;
/// ProjFS (VFS for Git) placeholders; may hold children.
const TAG_PROJFS: u32 = 0x9000_001C;
/// Windows Container Isolation (WCI).
const TAG_WCI: u32 = 0x8000_0018;
/// DFS; only on an empty folder or a file.
const TAG_DFS: u32 = 0x8000_000A;

/// Makes `path` a reparse point with `tag` (a Microsoft tag, not a link) and
/// some data, as a file system filter would.
fn set_reparse_tag(path: &Path, tag: u32) {
    let mut buffer = [0_u8; 16];
    buffer[0..4].copy_from_slice(&tag.to_le_bytes());
    buffer[4..6].copy_from_slice(&8_u16.to_le_bytes());
    buffer[8..16].copy_from_slice(b"corbanu1");
    // SAFETY: a valid path and buffer; the handle is closed here.
    unsafe {
        let handle = CreateFileW(
            to_wide(path).as_ptr(),
            GENERIC_READ | GENERIC_WRITE,
            FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
            std::ptr::null_mut(),
            OPEN_EXISTING,
            FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT,
            0,
        );
        assert_ne!(handle, INVALID_HANDLE_VALUE, "open {}", path.display());
        let mut returned = 0_u32;
        let ok = DeviceIoControl(
            handle,
            FSCTL_SET_REPARSE_POINT,
            buffer.as_ptr().cast(),
            buffer.len() as u32,
            std::ptr::null_mut(),
            0,
            &mut returned,
            std::ptr::null_mut(),
        );
        let err = std::io::Error::last_os_error();
        CloseHandle(handle);
        assert_ne!(ok, 0, "tag {tag:#x} on {}: {err}", path.display());
        let tagged = Handle::open(path, READ_CONTROL)
            .and_then(|handle| handle.kind())
            .expect("read the tag");
        assert_eq!(tagged.tag, tag, "{}", path.display());
    }
}

/// Folders and files below `root`'s `folder` that are reparse points other
/// than links and placeholders: ProjFS and WCI folders with children, an
/// empty DFS folder and a file of each kind, with a stale file inside the
/// ProjFS folder if `stale`.
fn other_reparse_points(root: &Path, folder: &str, stale: bool) {
    let dir = root.join(folder);
    std::fs::create_dir_all(dir.join("projfs\\a\\b")).expect("dirs");
    std::fs::write(dir.join("projfs\\a\\b\\f.rs"), b"f").expect("file");
    std::fs::create_dir_all(dir.join("projfs\\p")).expect("dir");
    std::fs::write(dir.join("projfs\\p\\g.rs"), b"g").expect("file");
    std::fs::create_dir_all(dir.join("wci\\c")).expect("dirs");
    std::fs::write(dir.join("wci\\c\\h.rs"), b"h").expect("file");
    std::fs::create_dir_all(dir.join("dfs")).expect("dir");
    for file in ["projfs.rs", "wci.rs", "dfs.rs"] {
        std::fs::write(dir.join(file), b"r").expect("file");
    }
    if stale {
        add_stale_inherited_entry(&dir.join("projfs\\a\\b\\f.rs"));
    }
    for (path, tag) in [
        ("projfs", TAG_PROJFS),
        ("projfs\\a", TAG_PROJFS),
        ("wci", TAG_WCI),
        ("dfs", TAG_DFS),
        ("projfs.rs", TAG_PROJFS),
        ("wci.rs", TAG_WCI),
        ("dfs.rs", TAG_DFS),
    ] {
        set_reparse_tag(&dir.join(path), tag);
    }
}

/// Where the reparse points go, and whether a stale file is inside: under
/// `src`, then with the stale file, then under `.git`, where the check
/// writes the delete-child copies into them.
const REPARSE_CASES: [(&str, bool); 3] = [
    ("src\\other", false),
    ("src\\other", true),
    (".git\\objects\\other", false),
];

/// #1715: D261's check goes into folders that are reparse points other
/// than links and placeholders, as Windows' walk does, writes through them
/// without falling back and finds a stale file inside one.
#[test]
fn d311_other_reparse_points_match_the_tree_walk() {
    let mut failed = Vec::new();
    for (folder, stale) in REPARSE_CASES {
        let base = tempfile::tempdir().expect("base dir");
        let setup = |root: &Path, _: &Path| {
            other_reparse_points(root, folder, stale);
            true
        };
        let name = format!("T-{folder}{}", if stale { "-stale" } else { "" });
        let outcome = equality_case_in(base.path(), &name, &setup).expect("runs everywhere");
        // A stale file is found at the first step, which walks; otherwise
        // nothing falls back.
        let expected_first = if stale {
            Written::Walked
        } else {
            Written::Direct
        };
        if !outcome.differing.is_empty()
            || outcome.written[0] != expected_first
            || (!stale && outcome.written != vec![Written::Direct; SESSION_B.len()])
        {
            failed.push(format!(
                "{name}: written={:?} {:?}",
                outcome.written, outcome.differing
            ));
        }
    }
    assert!(failed.is_empty(), "differing cases: {failed:#?}");
}

/// The same for the label walk: such folders get the labels Windows' walk
/// gives them and what is inside them, without falling back.
#[test]
fn d311_label_walk_other_reparse_points_match_windows_walk() {
    let mut failed = Vec::new();
    for (folder, stale) in REPARSE_CASES {
        let base = tempfile::tempdir().expect("base dir");
        let setup = |root: &Path, _: &Path| {
            other_reparse_points(root, folder, stale);
            true
        };
        let name = format!("T-{folder}{}", if stale { "-stale" } else { "" });
        let outcome = command_case_in(base.path(), &name, &setup, None).expect("runs everywhere");
        if !outcome.differing.is_empty() || outcome.label_fallbacks != 0 {
            failed.push(format!(
                "{name}: fallbacks={} {:?}",
                outcome.label_fallbacks, outcome.differing
            ));
        }
    }
    assert!(failed.is_empty(), "differing cases: {failed:#?}");
}

/// Adds an entry to `path`'s DACL alone, not propagated below.
fn add_entry_here(path: &Path, sid: &str, mode: i32, mask: u32, inheritance: u32) {
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
        let mut new_sd: SECURITY_DESCRIPTOR = std::mem::zeroed();
        let psd = &mut new_sd as *mut SECURITY_DESCRIPTOR as *mut c_void;
        assert_ne!(
            InitializeSecurityDescriptor(psd, SECURITY_DESCRIPTOR_REVISION),
            0
        );
        assert_ne!(SetSecurityDescriptorDacl(psd, 1, new_dacl, 0), 0);
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
        LocalFree(new_dacl as HLOCAL);
    }
}

/// A workspace whose root denies Everyone delete-child for its subfolders,
/// which don't have the copies yet: a check of it writes them.
fn workspace_missing_copies(root: &Path) {
    std::fs::create_dir_all(root).expect("root");
    build_tree(root);
    add_entry_here(root, EVERYONE, DENY_ACCESS, FILE_DELETE_CHILD, CI);
}

/// The handle `set_delete_child_deny` writes `path`'s DACL through.
fn written_handle(path: &Path) -> Handle {
    // SAFETY: an existing folder.
    unsafe { Handle::open_with(path, READ_CONTROL | WRITE_DAC, FILE_FLAG_BACKUP_SEMANTICS) }
        .expect("open the folder written")
}

/// D261's check below the folder written through `written`, now at `root`.
fn check_below(root: &Path, written: &Handle) -> anyhow::Result<()> {
    let sid = LocalSid::from_string(EVERYONE).expect("Everyone");
    // SAFETY: a valid SID; `root` exists.
    unsafe { super::super::super::sync_below(root, written, sid.as_ptr()) }
}

/// #1715: D261's check below a root that was swapped for another folder
/// (caught by the file-ID guard), or for a junction to the folder written
/// (caught by the folder guard), between the write and the check fails (so
/// the caller walks) and writes nothing below either folder, which a check
/// of either would write.
#[test]
fn d311_root_swapped_before_the_check_is_not_checked() {
    let base = tempfile::tempdir().expect("base dir");
    let verbatim = std::fs::canonicalize(base.path())
        .expect("canonical base")
        .join("t");
    let plain = dunce::simplified(&verbatim).to_path_buf();
    let (root, moved, other, twin) = (
        plain.join("root"),
        plain.join("root-moved"),
        plain.join("other"),
        plain.join("twin"),
    );
    workspace_missing_copies(&root);
    workspace_missing_copies(&other);
    workspace_missing_copies(&twin);
    // Control: a check of such a folder (its own handle) writes below it,
    // as one of either folder would.
    let before = full_snapshot(&twin);
    check_below(&twin, &written_handle(&twin)).expect("check the twin");
    assert_ne!(
        full_snapshot(&twin),
        before,
        "the check writes the copies below the twin"
    );
    // The root swapped for the other folder after its write.
    let written = written_handle(&root);
    rename(&root, &moved);
    rename(&other, &root);
    let (swapped_in, written_tree) = (full_snapshot(&root), full_snapshot(&moved));
    let err = check_below(&root, &written).expect_err("a swapped root is not checked");
    assert!(
        err.to_string().contains("is not the folder written"),
        "{err:#}"
    );
    assert_eq!(
        full_snapshot(&root),
        swapped_in,
        "nothing was written below the swapped-in folder"
    );
    assert_eq!(
        full_snapshot(&moved),
        written_tree,
        "nor below the folder written"
    );
    // The root swapped for a junction to the folder written.
    rename(&root, &other);
    assert!(
        mklink(&["/J"], &root, &moved),
        "a junction needs no privilege"
    );
    let err = check_below(&root, &written).expect_err("a linked root is not checked");
    assert!(
        err.to_string().contains("is not the folder written"),
        "{err:#}"
    );
    assert_eq!(full_snapshot(&other), swapped_in, "nothing was written");
    assert_eq!(full_snapshot(&moved), written_tree, "nothing was written");
}
