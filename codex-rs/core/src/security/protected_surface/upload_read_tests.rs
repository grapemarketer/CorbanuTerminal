#![cfg(unix)]

use super::*;
use codex_protocol::permissions::FileSystemAccessMode;
use codex_protocol::permissions::FileSystemPath;
use codex_protocol::permissions::FileSystemSandboxEntry;
use codex_protocol::permissions::FileSystemSpecialPath;
use codex_utils_absolute_path::AbsolutePathBuf;

const LIMIT: u64 = 1024;

fn entry(path: FileSystemPath, access: FileSystemAccessMode) -> FileSystemSandboxEntry {
    FileSystemSandboxEntry::new(path, access)
}

fn path(path: &Path) -> FileSystemPath {
    FileSystemPath::Path {
        path: AbsolutePathBuf::from_absolute_path(path).unwrap(),
    }
}

fn root_read() -> FileSystemSandboxEntry {
    entry(
        FileSystemPath::Special {
            value: FileSystemSpecialPath::Root,
        },
        FileSystemAccessMode::Read,
    )
}

/// A workspace and a denied `secrets` folder holding a dummy key.
struct Fixture {
    _dir: tempfile::TempDir,
    base: std::path::PathBuf,
    workspace: std::path::PathBuf,
    secrets: std::path::PathBuf,
}

fn fixture() -> Fixture {
    let dir = tempfile::tempdir().unwrap();
    let base = dir.path().canonicalize().unwrap();
    let workspace = base.join("workspace");
    let secrets = base.join("secrets");
    std::fs::create_dir_all(workspace.join("docs")).unwrap();
    std::fs::create_dir_all(&secrets).unwrap();
    std::fs::write(workspace.join("docs/key"), "plain").unwrap();
    std::fs::write(secrets.join("key"), "DUMMY_SECRET_519").unwrap();
    Fixture {
        _dir: dir,
        base,
        workspace,
        secrets,
    }
}

fn denying(denied: &Path) -> FileSystemSandboxPolicy {
    FileSystemSandboxPolicy::restricted(vec![
        root_read(),
        entry(path(denied), FileSystemAccessMode::Deny),
    ])
}

#[test]
fn issue_519_reads_a_plain_file_and_refuses_a_denied_one() {
    let fx = fixture();
    let policy = denying(&fx.secrets);
    let cwd = fx.workspace.as_path();
    assert_eq!(
        read_checked(&policy, &cwd.join("docs/key"), cwd, LIMIT).unwrap(),
        b"plain"
    );
    std::os::unix::fs::symlink(fx.secrets.join("key"), cwd.join("link")).unwrap();
    for denied in [fx.secrets.join("key"), cwd.join("link")] {
        let error = read_checked(&policy, &denied, cwd, LIMIT).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::PermissionDenied, "{denied:?}");
    }
    let error = read_checked(&policy, &cwd.join("docs/key"), cwd, /*limit*/ 2).unwrap_err();
    assert!(error.to_string().contains("too large"), "{error}");
}

/// #519: a folder swapped for a link after the path was checked and
/// resolved: the open follows it, so the opened file is checked again.
#[test]
fn issue_519_a_folder_swapped_after_the_check_reads_nothing() {
    let fx = fixture();
    let policy = denying(&fx.secrets);
    let cwd = fx.workspace.as_path();
    let checked = cwd.join("docs/key");
    assert!(readable_under(&policy, &checked, cwd));
    std::fs::rename(cwd.join("docs"), fx.base.join("old-docs")).unwrap();
    std::os::unix::fs::symlink(&fx.secrets, cwd.join("docs")).unwrap();
    let error = read_opened(&policy, &checked, cwd, LIMIT).unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::PermissionDenied);
    // The last component swapped for a link is not followed at all.
    std::fs::remove_file(cwd.join("docs")).unwrap();
    std::fs::create_dir(cwd.join("docs")).unwrap();
    std::os::unix::fs::symlink(fx.secrets.join("key"), &checked).unwrap();
    let error = read_opened(&policy, &checked, cwd, LIMIT).unwrap_err();
    assert_ne!(error.kind(), io::ErrorKind::NotFound, "{error}");
    assert!(!error.to_string().contains("DUMMY_SECRET_519"));
}

/// #519: pattern denials apply to the upload check, also to a link to a
/// matching file and when the pattern is spelled through a linked folder.
#[test]
fn issue_519_pattern_denials_apply_to_uploads() {
    let fx = fixture();
    let cwd = fx.workspace.as_path();
    std::fs::write(cwd.join("server.pem"), "DUMMY_PEM").unwrap();
    std::os::unix::fs::symlink(cwd.join("server.pem"), cwd.join("innocent.txt")).unwrap();
    let linked = fx.base.join("linked-workspace");
    std::os::unix::fs::symlink(cwd, &linked).unwrap();
    for pattern in [
        format!("{}/**/*.pem", cwd.display()),
        format!("{}/**/*.pem", linked.display()),
    ] {
        let policy = FileSystemSandboxPolicy::restricted(vec![
            root_read(),
            entry(
                FileSystemPath::GlobPattern {
                    scan: None,
                    pattern: pattern.clone(),
                },
                FileSystemAccessMode::Deny,
            ),
        ]);
        for denied in [
            cwd.join("server.pem"),
            cwd.join("innocent.txt"),
            linked.join("server.pem"),
        ] {
            assert!(
                !readable_under(&policy, &denied, cwd),
                "{pattern}: {denied:?}"
            );
            assert!(read_checked(&policy, &denied, cwd, LIMIT).is_err());
        }
        assert!(readable_under(&policy, &cwd.join("docs/key"), cwd));
    }
}

/// #519: a denial relative to the working folder holds when that folder is
/// reached through a link and the file is named by its real path.
#[test]
fn issue_519_working_folder_denials_hold_through_a_linked_cwd() {
    let fx = fixture();
    let real = fx.workspace.as_path();
    std::fs::write(real.join(".env"), "DUMMY_ENV").unwrap();
    let linked = fx.base.join("linked-workspace");
    std::os::unix::fs::symlink(real, &linked).unwrap();
    let policy = FileSystemSandboxPolicy::restricted(vec![
        root_read(),
        entry(
            FileSystemPath::Special {
                value: FileSystemSpecialPath::project_roots(Some(".env".into())),
            },
            FileSystemAccessMode::Deny,
        ),
    ]);
    for denied in [linked.join(".env"), real.join(".env")] {
        assert!(!readable_under(&policy, &denied, &linked), "{denied:?}");
    }
    assert!(readable_under(&policy, &real.join("docs/key"), &linked));
}
