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
/// backends' resolvers and by the agent path's check with the same clear
/// message, naming both folders; a deny-read profile that needs no other
/// override (a Moderate-style protected file) gets the clear "can't block
/// reading" refusal from the non-admin resolver (`command/exec`, the exec
/// server) rather than the backend's unclear error, and from the agent
/// path's check. A plain workspace profile passes that check.
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
        root_read.clone(),
        entry(&cwd, FileSystemAccessMode::Write),
        entry(&cwd.join("secret.env"), FileSystemAccessMode::Deny),
    ]);
    let plain = profile(vec![
        root_read,
        entry(&cwd, FileSystemAccessMode::Write),
        entry(&docs, FileSystemAccessMode::Read),
    ]);
    let agent_path = |profile: &PermissionProfile| {
        refuse_unenforceable_windows_restricted_token_profile(
            SandboxType::WindowsRestrictedToken,
            profile,
            &cwd,
            WindowsSandboxLevel::RestrictedToken,
        )
        .err()
    };
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
            agent_path(&nested),
            agent_path(&deny_read),
            agent_path(&plain),
        ),
        (
            Some(refusal.clone()),
            Some(refusal.clone()),
            Some(UNELEVATED_DENY_READ_REFUSAL.to_string()),
            Some(refusal),
            Some(UNELEVATED_DENY_READ_REFUSAL.to_string()),
            None,
        )
    );
}

/// #1801: the D331 refusal compares by real path too, in all three checks
/// (the agent path's, the non-admin resolver's and the elevated one's): a
/// writable folder inside the read-only `docs` only through a junction is
/// refused, whether the junction is on the writable folder or on the working
/// folder, or the read-only folder is spelled through one. A junction to the
/// read-only folder itself is left to #1780 (read-only wins), and one
/// elsewhere is a plain writable folder: no D331 refusal at all for those.
#[cfg(windows)]
#[test]
fn sec_win_1801_writable_inside_read_only_through_a_junction_is_refused() {
    let temp_dir = tempfile::TempDir::new().expect("tempdir");
    let real_root = dunce::canonicalize(temp_dir.path()).expect("canonical temp dir");
    let absolute = |path: &Path| AbsolutePathBuf::from_absolute_path(path).expect("absolute");
    let workspace = absolute(&real_root.join("workspace"));
    let docs = workspace.join("docs");
    std::fs::create_dir_all(docs.join("out").as_path()).expect("create docs\\out");
    std::fs::create_dir_all(workspace.join("other").as_path()).expect("create other");
    let junction = |name: &str, target: &AbsolutePathBuf| {
        let link = real_root.join(name);
        let output = std::process::Command::new("cmd.exe")
            .args(["/d", "/c", "mklink", "/J"])
            .arg(&link)
            .arg(target.as_path())
            .output()
            .expect("run mklink");
        assert!(output.status.success(), "mklink /J: {output:?}");
        absolute(&link)
    };
    let (cwd_link, docs_link, other_link) = (
        junction("cwd-link", &workspace),
        junction("docs-link", &docs),
        junction("other-link", &workspace.join("other")),
    );
    let profile =
        |cwd: &AbsolutePathBuf, read_only: &AbsolutePathBuf, writable: &AbsolutePathBuf| {
            let entry = |path: &AbsolutePathBuf, access| {
                FileSystemSandboxEntry::new(FileSystemPath::Path { path: path.clone() }, access)
            };
            PermissionProfile::from_runtime_permissions(
                &FileSystemSandboxPolicy::restricted(vec![
                    FileSystemSandboxEntry::new(
                        FileSystemPath::Special {
                            value: FileSystemSpecialPath::Root,
                        },
                        FileSystemAccessMode::Read,
                    ),
                    entry(cwd, FileSystemAccessMode::Write),
                    entry(read_only, FileSystemAccessMode::Read),
                    entry(writable, FileSystemAccessMode::Write),
                ]),
                NetworkSandboxPolicy::Restricted,
            )
        };
    let any_refusal = writable_inside_read_only_refusal(Path::new("<r>"), Path::new("<w>"));
    let any_refusal = any_refusal
        .split("<w>")
        .next()
        .unwrap_or_default()
        .to_string();
    // 2: refused naming these folders; 1: some other D331 refusal; 0: none.
    let checks = |cwd: &AbsolutePathBuf,
                  read_only: &AbsolutePathBuf,
                  writable: &AbsolutePathBuf| {
        let profile = profile(cwd, read_only, writable);
        let refusal = writable_inside_read_only_refusal(read_only.as_path(), writable.as_path());
        let refused = |result: Option<String>| match result {
            Some(err) if err == refusal => 2,
            Some(err) if err.starts_with(&any_refusal) => 1,
            _ => 0,
        };
        [
            refused(
                refuse_unenforceable_windows_restricted_token_profile(
                    SandboxType::WindowsRestrictedToken,
                    &profile,
                    cwd,
                    WindowsSandboxLevel::RestrictedToken,
                )
                .err(),
            ),
            refused(
                resolve_windows_restricted_token_filesystem_overrides(
                    SandboxType::WindowsRestrictedToken,
                    &profile,
                    cwd,
                    WindowsSandboxLevel::RestrictedToken,
                )
                .err(),
            ),
            refused(
                resolve_windows_elevated_filesystem_overrides(
                    SandboxType::WindowsRestrictedToken,
                    &profile,
                    cwd,
                    /*use_windows_elevated_backend*/ true,
                )
                .err(),
            ),
        ]
    };

    assert_eq!(
        [
            checks(&cwd_link, &docs, &docs.join("out")),
            checks(&workspace, &docs, &docs_link.join("out")),
            checks(&cwd_link, &docs, &docs_link.join("out")),
            checks(&workspace, &docs_link, &docs.join("out")),
            checks(&workspace, &docs, &docs_link),
            checks(&workspace, &docs, &other_link),
        ],
        [[2; 3], [2; 3], [2; 3], [2; 3], [0; 3], [0; 3]]
    );
}

/// #1810: a profile in the older workspace-write format can hold D331's shape
/// too: a writable folder inside a protected `.git`, `.codex` or `.agents`
/// folder of another writable folder. Those profiles skip the non-admin
/// checks' direct-enforcement path, so they must be refused before it, as
/// the elevated check already refuses them. Not refused: `.git` itself named
/// writable (the explicit rule replaces its protection), and a plain folder.
#[test]
fn sec_win_1810_legacy_format_profile_with_writable_inside_protected_folder_is_refused() {
    use codex_protocol::protocol::SandboxPolicy;

    let temp_dir = tempfile::TempDir::new().expect("tempdir");
    let workspace = dunce::canonicalize(temp_dir.path())
        .expect("canonical temp dir")
        .join("workspace");
    for (parent, child) in [(".git", "hooks"), (".codex", "sub"), (".agents", "x")] {
        std::fs::create_dir_all(workspace.join(parent).join(child)).expect("create fixture folder");
    }
    let cwd = AbsolutePathBuf::from_absolute_path(&workspace).expect("absolute cwd");
    let checks = |writable: &[&str]| {
        let writable = writable
            .iter()
            .fold(cwd.clone(), |path, part| path.join(part));
        let profile = PermissionProfile::from_legacy_sandbox_policy_for_cwd(
            &SandboxPolicy::WorkspaceWrite {
                writable_roots: vec![writable.clone()],
                network_access: false,
                exclude_tmpdir_env_var: true,
                exclude_slash_tmp: true,
            },
            cwd.as_path(),
        );
        let (file_system, network) = profile.to_runtime_permissions();
        let read_only = writable.parent().expect("parent").to_path_buf();
        let refusal = writable_inside_read_only_refusal(&read_only, writable.as_path());
        let refused = |result: Option<String>| result.is_some_and(|err| err == refusal);
        (
            file_system.needs_direct_runtime_enforcement(network, cwd.as_path()),
            [
                refused(
                    refuse_unenforceable_windows_restricted_token_profile(
                        SandboxType::WindowsRestrictedToken,
                        &profile,
                        &cwd,
                        WindowsSandboxLevel::RestrictedToken,
                    )
                    .err(),
                ),
                refused(
                    resolve_windows_restricted_token_filesystem_overrides(
                        SandboxType::WindowsRestrictedToken,
                        &profile,
                        &cwd,
                        WindowsSandboxLevel::RestrictedToken,
                    )
                    .err(),
                ),
                refused(
                    resolve_windows_elevated_filesystem_overrides(
                        SandboxType::WindowsRestrictedToken,
                        &profile,
                        &cwd,
                        /*use_windows_elevated_backend*/ true,
                    )
                    .err(),
                ),
            ],
        )
    };

    assert_eq!(
        [
            checks(&[".git", "hooks"]),
            checks(&[".codex", "sub"]),
            checks(&[".agents", "x"]),
            checks(&[".git"]),
        ],
        [
            (false, [true; 3]),
            (false, [true; 3]),
            (false, [true; 3]),
            (false, [false; 3]),
        ]
    );
    assert_eq!(
        resolve_windows_restricted_token_filesystem_overrides(
            SandboxType::WindowsRestrictedToken,
            &PermissionProfile::from_legacy_sandbox_policy_for_cwd(
                &SandboxPolicy::new_workspace_write_policy(),
                cwd.as_path(),
            ),
            &cwd,
            WindowsSandboxLevel::RestrictedToken,
        ),
        Ok(None)
    );
}
