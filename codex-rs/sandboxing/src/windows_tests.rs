//! Routing of configured read-only paths to the non-admin Windows sandbox.

use super::*;
use codex_protocol::permissions::FileSystemAccessMode;
use codex_protocol::permissions::FileSystemPath;
use codex_protocol::permissions::FileSystemSandboxEntry;
use codex_protocol::permissions::FileSystemSpecialPath;
use codex_protocol::permissions::NetworkSandboxPolicy;
use pretty_assertions::assert_eq;

/// A configured read-only path that doesn't exist yet is among the extra
/// deny-write paths resolved for the legacy backend (which `command/exec`
/// and the exec server pass on); the backend creates such paths before the
/// command starts. A skip-missing entry applies only to a path that exists,
/// so it isn't passed on.
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

/// D331: a writable folder inside a read-only one is refused by both
/// backends' resolvers with the same clear message, naming both folders; a
/// deny-read profile that needs no other override (a Moderate-style
/// protected file) gets the clear "can't block reading" refusal from the
/// non-admin resolver (`command/exec`, the exec server) rather than the
/// backend's unclear error.
#[test]
fn unenforceable_profiles_get_clear_refusals() {
    let temp_dir = tempfile::TempDir::new().expect("tempdir");
    let cwd = AbsolutePathBuf::from_absolute_path(
        dunce::canonicalize(temp_dir.path()).expect("canonical temp dir"),
    )
    .expect("absolute cwd");
    let (docs, out) = (cwd.join("docs"), cwd.join("docs").join("out"));
    std::fs::create_dir_all(out.as_path()).expect("create docs/out");
    let entry = |path: &AbsolutePathBuf, access| {
        FileSystemSandboxEntry::new(FileSystemPath::Path { path: path.clone() }, access)
    };
    let root_read = FileSystemSandboxEntry::new(
        FileSystemPath::Special {
            value: FileSystemSpecialPath::Root,
        },
        FileSystemAccessMode::Read,
    );
    let profile = |entries: Vec<FileSystemSandboxEntry>| {
        PermissionProfile::from_runtime_permissions(
            &FileSystemSandboxPolicy::restricted(entries),
            NetworkSandboxPolicy::Restricted,
        )
    };
    let nested = profile(vec![
        root_read.clone(),
        entry(&cwd, FileSystemAccessMode::Write),
        entry(&docs, FileSystemAccessMode::Read),
        entry(&out, FileSystemAccessMode::Write),
    ]);
    let deny_read = profile(vec![
        root_read,
        entry(&cwd, FileSystemAccessMode::Write),
        entry(&cwd.join("secret.env"), FileSystemAccessMode::Deny),
    ]);
    let unelevated = |profile: &PermissionProfile| {
        resolve_windows_restricted_token_filesystem_overrides(
            SandboxType::WindowsRestrictedToken,
            profile,
            &cwd,
            WindowsSandboxLevel::RestrictedToken,
        )
        .err()
    };
    let refusal = writable_inside_read_only_refusal(docs.as_path(), out.as_path());

    assert_eq!(
        (
            unelevated(&nested),
            resolve_windows_elevated_filesystem_overrides(
                SandboxType::WindowsRestrictedToken,
                &nested,
                &cwd,
                /*use_windows_elevated_backend*/ true,
            )
            .err(),
            unelevated(&deny_read),
        ),
        (
            Some(refusal.clone()),
            Some(refusal),
            Some(UNELEVATED_DENY_READ_REFUSAL.to_string()),
        )
    );
}
