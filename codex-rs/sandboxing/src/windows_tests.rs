//! Routing of configured read-only paths to the non-admin Windows sandbox.

use super::*;
use codex_protocol::permissions::FileSystemAccessMode;
use codex_protocol::permissions::FileSystemPath;
use codex_protocol::permissions::FileSystemSandboxEntry;
use codex_protocol::permissions::FileSystemSpecialPath;
use codex_protocol::permissions::NetworkSandboxPolicy;
use pretty_assertions::assert_eq;

/// A configured read-only path that doesn't exist yet reaches the legacy
/// backend as an extra deny-write path, which it creates before the command
/// starts, so a command can't create it writable first. A skip-missing
/// entry applies only to a path that exists, so it isn't passed on.
#[test]
fn missing_read_only_paths_reach_the_restricted_token_backend() {
    let temp_dir = tempfile::TempDir::new().expect("tempdir");
    let cwd = AbsolutePathBuf::from_absolute_path(
        dunce::canonicalize(temp_dir.path()).expect("canonical temp dir"),
    )
    .expect("absolute cwd");
    let extra = cwd.join("extra");
    std::fs::create_dir_all(extra.as_path()).expect("create extra root");
    let path = |path: AbsolutePathBuf| FileSystemPath::Path { path };
    let read_only = |path: AbsolutePathBuf| {
        FileSystemSandboxEntry::new(FileSystemPath::Path { path }, FileSystemAccessMode::Read)
    };
    let policy = FileSystemSandboxPolicy::restricted(vec![
        FileSystemSandboxEntry::new(
            FileSystemPath::Special {
                value: FileSystemSpecialPath::Root,
            },
            FileSystemAccessMode::Read,
        ),
        FileSystemSandboxEntry::new(
            FileSystemPath::Special {
                value: FileSystemSpecialPath::project_roots(/*subpath*/ None),
            },
            FileSystemAccessMode::Write,
        ),
        FileSystemSandboxEntry::new(path(extra.clone()), FileSystemAccessMode::Write),
        read_only(cwd.join("missing")),
        read_only(cwd.join("deep").join("missing")),
        read_only(cwd.join(".agents")),
        read_only(extra.join("missing")),
        FileSystemSandboxEntry::new(
            FileSystemPath::Special {
                value: FileSystemSpecialPath::project_roots(Some(format!(
                    "sub{}missing",
                    std::path::MAIN_SEPARATOR
                ))),
            },
            FileSystemAccessMode::Read,
        ),
        FileSystemSandboxEntry::skip_missing_path(
            path(cwd.join("skipped")),
            FileSystemAccessMode::Read,
        ),
    ]);
    let profile =
        PermissionProfile::from_runtime_permissions(&policy, NetworkSandboxPolicy::Restricted);

    let mut deny_write: Vec<PathBuf> = resolve_windows_restricted_token_filesystem_overrides(
        SandboxType::WindowsRestrictedToken,
        &profile,
        &cwd,
        WindowsSandboxLevel::RestrictedToken,
    )
    .expect("supported")
    .expect("overrides")
    .additional_deny_write_paths
    .into_iter()
    .map(AbsolutePathBuf::into_path_buf)
    .collect();
    deny_write.sort();
    let mut expected: Vec<PathBuf> = [
        cwd.join(".agents"),
        cwd.join("deep").join("missing"),
        extra.join("missing"),
        cwd.join("missing"),
        cwd.join("sub").join("missing"),
    ]
    .into_iter()
    .map(|path| normalize_windows_override_path(path.as_path()).expect("normalized"))
    .collect();
    expected.sort();

    assert_eq!(deny_write, expected);
}
