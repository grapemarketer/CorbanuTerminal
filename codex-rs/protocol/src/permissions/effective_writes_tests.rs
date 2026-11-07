#![cfg(unix)]

//! #907: [`EffectiveWrites`] answers exactly as the per-path checks did
//! before it (a verbatim copy of them is kept here as the reference), for
//! one build reused across many paths.

use super::super::*;
use pretty_assertions::assert_eq;
use std::os::unix::fs::symlink;

/// The per-path checks as they were before #907, verbatim:
/// `can_write_effective_path_with_cwd` (`with_links` computed per call) and
/// `can_grant_write_effective_path_with_cwd`.
mod reference {
    use super::super::super::*;

    pub(super) fn can_write_effective(
        policy: &FileSystemSandboxPolicy,
        path: &Path,
        cwd: &Path,
    ) -> bool {
        can_write_normalized(policy, path, cwd, /*read_only_wins*/ false)
            || can_write_through_links(policy, path, cwd, &policy.writable_root_links_with_cwd(cwd))
    }

    pub(super) fn can_grant_write_effective(
        policy: &FileSystemSandboxPolicy,
        path: &Path,
        cwd: &Path,
    ) -> bool {
        can_write_normalized(policy, path, cwd, /*read_only_wins*/ true)
    }

    fn can_write_through_links(
        policy: &FileSystemSandboxPolicy,
        path: &Path,
        cwd: &Path,
        links: &WritableRootLinks,
    ) -> bool {
        links
            .spellings(path, cwd)
            .iter()
            .any(|spelled| policy.can_write_path_with_cwd(spelled.as_path(), cwd))
    }

    fn can_write_normalized(
        policy: &FileSystemSandboxPolicy,
        path: &Path,
        cwd: &Path,
        read_only_wins: bool,
    ) -> bool {
        match policy.kind {
            FileSystemSandboxKind::Unrestricted | FileSystemSandboxKind::ExternalSandbox => {
                return true;
            }
            FileSystemSandboxKind::Restricted => {}
        }
        let Some(path) = resolve_candidate_path(path, cwd) else {
            return false;
        };
        let cwd_absolute = AbsolutePathBuf::from_absolute_path(cwd).ok();
        let mut entries: Vec<FileSystemSandboxEntry> = policy
            .entries
            .iter()
            .map(|entry| match &entry.path {
                FileSystemPath::Special {
                    value: FileSystemSpecialPath::Root,
                } => entry.clone(),
                path => match resolve_entry_path(path, cwd_absolute.as_ref()) {
                    Some(path) => FileSystemSandboxEntry {
                        path: FileSystemPath::Path {
                            path: normalize_effective_absolute_path(path),
                        },
                        ..entry.clone()
                    },
                    None => entry.clone(),
                },
            })
            .collect();
        if read_only_wins {
            let read_only: Vec<AbsolutePathBuf> = entries
                .iter()
                .filter(|entry| entry.access == FileSystemAccessMode::Read)
                .filter_map(|entry| match &entry.path {
                    FileSystemPath::Path { path } => Some(path.clone()),
                    _ => None,
                })
                .collect();
            for entry in &mut entries {
                if entry.access == FileSystemAccessMode::Write
                    && matches!(&entry.path, FileSystemPath::Path { path } if read_only.contains(path))
                {
                    entry.access = FileSystemAccessMode::Read;
                }
            }
        }
        let normalized = FileSystemSandboxPolicy {
            kind: policy.kind,
            glob_scan_max_depth: policy.glob_scan_max_depth,
            entries,
        };
        normalized.can_write_path_with_cwd(normalize_effective_absolute_path(path).as_path(), cwd)
    }
}

fn abs(path: &Path) -> AbsolutePathBuf {
    AbsolutePathBuf::from_absolute_path(path).expect("absolute path")
}

fn entry(path: &Path, access: FileSystemAccessMode) -> FileSystemSandboxEntry {
    FileSystemSandboxEntry::new(FileSystemPath::Path { path: abs(path) }, access)
}

fn special(value: FileSystemSpecialPath, access: FileSystemAccessMode) -> FileSystemSandboxEntry {
    FileSystemSandboxEntry::new(FileSystemPath::Special { value }, access)
}

/// A workspace with git metadata, links into and out of it (top-level and
/// nested, dangling) and a folder outside it, in a temporary folder spelled
/// as the system spells it (on macOS `/var/...` for `/private/var/...`).
struct Layout {
    _dir: tempfile::TempDir,
    /// As spelled (possibly through a top-level alias).
    base: PathBuf,
    /// Its real location.
    real: PathBuf,
}

fn layout() -> Layout {
    let dir = tempfile::tempdir().expect("temporary folder");
    let base = dir.path().to_path_buf();
    let real = base.canonicalize().expect("real location");
    for folder in [
        "ws/.git/hooks",
        "ws/.codex",
        "ws/.agents",
        "ws/src",
        "ws/tie",
        "ws/sub",
        "outside/inner",
        "home",
    ] {
        std::fs::create_dir_all(real.join(folder)).expect("folder");
    }
    std::fs::write(real.join("outside/secret"), "x").expect("file");
    std::fs::write(real.join("ws/src/main.rs"), "x").expect("file");
    symlink(real.join("ws"), real.join("ws_link")).expect("link");
    symlink(real.join("ws"), real.join("home/ws_via_home")).expect("link");
    symlink(real.join("outside"), real.join("ws/nested_link")).expect("link");
    symlink(real.join("ws/.git"), real.join("ws/sub/linked_git")).expect("link");
    symlink(real.join("missing_target"), real.join("dangling")).expect("link");
    symlink(real.join("missing_target"), real.join("ws/dangling")).expect("link");
    Layout {
        _dir: dir,
        base,
        real,
    }
}

/// Policies of every kind and shape the protected-path checks see.
fn policies(layout: &Layout) -> Vec<(&'static str, FileSystemSandboxPolicy)> {
    use FileSystemAccessMode::Deny;
    use FileSystemAccessMode::Read;
    use FileSystemAccessMode::Write;
    let real = &layout.real;
    let base = &layout.base;
    let root = || FileSystemPath::Special {
        value: FileSystemSpecialPath::Root,
    };
    let mut denied = FileSystemSandboxPolicy::workspace_write(
        &[abs(&real.join("ws"))],
        /*exclude_tmpdir_env_var*/ false,
        /*exclude_slash_tmp*/ false,
    );
    denied.preserve_deny_read_restrictions_from(&FileSystemSandboxPolicy::restricted(vec![
        entry(&real.join("outside/secret"), Deny),
        entry(&real.join("ws/.codex"), Deny),
        FileSystemSandboxEntry::skip_missing_path(
            FileSystemPath::Path {
                path: abs(&real.join("missing/a/b")),
            },
            Deny,
        ),
        FileSystemSandboxEntry::new(
            FileSystemPath::GlobPattern {
                scan: None,
                pattern: real.join("ws/**/*.key").to_string_lossy().into_owned(),
            },
            Deny,
        ),
        FileSystemSandboxEntry::deny_directory_listing(&abs(&real.join("home"))),
        FileSystemSandboxEntry::deny_moving(&abs(&real.join("ws/src"))),
    ]));
    let mut denied_with_read_only = denied.clone();
    denied_with_read_only.entries.extend([
        entry(&real.join("ws/src"), Read),
        entry(&base.join("ws/.git/hooks"), Read),
        entry(&real.join("ws/nested_link"), Read),
    ]);
    vec![
        ("unrestricted", FileSystemSandboxPolicy::unrestricted()),
        ("external", FileSystemSandboxPolicy::external_sandbox()),
        (
            "read-only root",
            FileSystemSandboxPolicy::restricted(vec![FileSystemSandboxEntry::new(root(), Read)]),
        ),
        (
            "workspace",
            FileSystemSandboxPolicy::workspace_write(
                &[abs(&real.join("ws"))],
                /*exclude_tmpdir_env_var*/ false,
                /*exclude_slash_tmp*/ false,
            ),
        ),
        (
            "workspace spelled as the system spells it",
            FileSystemSandboxPolicy::workspace_write(
                &[abs(&base.join("ws"))],
                /*exclude_tmpdir_env_var*/ true,
                /*exclude_slash_tmp*/ true,
            ),
        ),
        (
            "workspace through a top-level link",
            FileSystemSandboxPolicy::workspace_write(
                &[abs(&real.join("ws_link"))],
                /*exclude_tmpdir_env_var*/ true,
                /*exclude_slash_tmp*/ false,
            ),
        ),
        (
            "workspace through a nested link (#652)",
            FileSystemSandboxPolicy::workspace_write(
                &[abs(&real.join("home/ws_via_home"))],
                /*exclude_tmpdir_env_var*/ false,
                /*exclude_slash_tmp*/ true,
            ),
        ),
        ("workspace with denials", denied),
        (
            "workspace with denials and read-only paths",
            denied_with_read_only,
        ),
        (
            "write root, narrowed",
            FileSystemSandboxPolicy::restricted(vec![
                FileSystemSandboxEntry::new(root(), Write),
                entry(&real.join("outside"), Read),
                entry(&real.join("outside/inner"), Write),
                entry(&real.join("ws/.git"), Deny),
                entry(&real.join("missing/x"), Read),
                entry(&real.join("dangling"), Read),
            ]),
        ),
        (
            "two spellings, writable and read-only (#652 ties)",
            FileSystemSandboxPolicy::restricted(vec![
                FileSystemSandboxEntry::new(root(), Read),
                entry(&base.join("ws/tie"), Write),
                entry(&real.join("ws/tie"), Read),
                entry(&real.join("ws_link/src"), Write),
                entry(&real.join("ws/src"), Read),
                entry(&real.join("outside"), Write),
            ]),
        ),
        (
            "explicit writes into protected metadata",
            FileSystemSandboxPolicy::restricted(vec![
                FileSystemSandboxEntry::new(root(), Read),
                entry(&real.join("ws"), Write),
                entry(&real.join("ws/.git/hooks"), Write),
                entry(&real.join("ws/.codex"), Write),
                entry(&base.join("ws/.agents"), Write),
            ]),
        ),
        (
            "special paths",
            FileSystemSandboxPolicy::restricted(vec![
                special(FileSystemSpecialPath::Minimal, Read),
                special(
                    FileSystemSpecialPath::project_roots(/*subpath*/ None),
                    Write,
                ),
                special(
                    FileSystemSpecialPath::project_roots(Some(".codex".to_string())),
                    Read,
                ),
                special(FileSystemSpecialPath::Tmpdir, Write),
                special(FileSystemSpecialPath::SlashTmp, Write),
                special(
                    FileSystemSpecialPath::Unknown {
                        path: ":future".to_string(),
                        subpath: None,
                    },
                    Write,
                ),
                entry(&real.join("outside"), Write),
                entry(&real.join("outside/secret"), Deny),
            ]),
        ),
    ]
}

fn cwds(layout: &Layout) -> Vec<PathBuf> {
    vec![
        layout.real.join("ws"),
        layout.base.join("ws"),
        layout.real.join("ws_link"),
        layout.real.join("home/ws_via_home"),
        layout.real.join("outside"),
        layout.real.join("missing/cwd"),
    ]
}

fn paths(layout: &Layout) -> Vec<PathBuf> {
    let mut paths = Vec::new();
    for base in [&layout.real, &layout.base] {
        for relative in [
            "",
            "ws",
            "ws/file",
            "ws/src/main.rs",
            "ws/src/new/deep/file",
            "ws/.git",
            "ws/.git/config",
            "ws/.git/hooks/pre-commit",
            "ws/.GIT/config",
            "ws/.Git",
            "ws/.codex",
            "ws/.codex/config.toml",
            "ws/.CODEX/config.toml",
            "ws/.agents/x",
            "ws/.Agents",
            "ws/tie/x",
            "ws/nested_link/secret",
            "ws/nested_link/inner/x",
            "ws/sub/linked_git/hooks/x",
            "ws/dangling/x",
            "ws/../outside/x",
            "ws/./src/x",
            "ws_link",
            "ws_link/file",
            "ws_link/.git/hooks/x",
            "ws_link/.Codex/x",
            "home",
            "home/ws_via_home/f",
            "home/ws_via_home/.git/x",
            "outside",
            "outside/secret",
            "outside/inner/x",
            "dangling",
            "dangling/x",
            "missing/a/b/c",
            "missing/x/y",
            "missing_target/z",
        ] {
            paths.push(base.join(relative));
        }
    }
    for relative in [
        "rel/file",
        ".git/x",
        "./.codex/x",
        "../outside/secret",
        "../ws/x",
        "nested_link/x",
    ] {
        paths.push(PathBuf::from(relative));
    }
    let temp = std::env::temp_dir();
    paths.extend([
        PathBuf::from("/"),
        PathBuf::from("/tmp/x"),
        PathBuf::from("/private/tmp/x"),
        PathBuf::from("/etc/passwd"),
        temp.join("x"),
        temp.canonicalize().unwrap_or(temp).join("y/z"),
    ]);
    paths
}

#[test]
fn issue_907_effective_writes_answer_as_the_per_path_checks_did() {
    let layout = layout();
    let paths = paths(&layout);
    let mut answers = [[0usize; 2]; 2];
    for (name, policy) in policies(&layout) {
        for cwd in cwds(&layout) {
            // One build for every path, as `ReadDenials::apply` uses it.
            let writes = policy.effective_writes_with_cwd(&cwd);
            assert_eq!(
                writes.links(),
                &policy.writable_root_links_with_cwd(&cwd),
                "{name} in {cwd:?}"
            );
            for path in &paths {
                let expected = (
                    reference::can_write_effective(&policy, path, &cwd),
                    reference::can_grant_write_effective(&policy, path, &cwd),
                );
                let reused = (
                    writes.can_write_effective_path(path),
                    writes.can_grant_write_effective_path(path),
                );
                let per_path = (
                    policy.can_write_effective_path_with_cwd(path, &cwd),
                    policy.can_grant_write_effective_path_with_cwd(path, &cwd),
                );
                assert_eq!(reused, expected, "{name} in {cwd:?}: {path:?}");
                assert_eq!(per_path, expected, "{name} in {cwd:?}: {path:?}");
                answers[0][usize::from(expected.0)] += 1;
                answers[1][usize::from(expected.1)] += 1;
            }
        }
    }
    // Both answers come up often for both checks, so the comparison covers
    // writable and read-only paths alike.
    for counts in answers {
        assert!(counts.iter().all(|count| *count > 100), "{answers:?}");
    }
}

/// The checks differ between the policy's two readings: a folder named in
/// two spellings is writable for the read-only decision and read-only for a
/// grant, and the comparison above sees it.
#[test]
fn issue_907_ties_and_nested_links_differ_between_the_two_checks() {
    let layout = layout();
    let tie = &policies(&layout)
        .into_iter()
        .find(|(name, _)| name.contains("ties"))
        .unwrap()
        .1;
    let cwd = layout.real.join("ws");
    let path = layout.real.join("ws/tie/x");
    let writes = tie.effective_writes_with_cwd(&cwd);
    if layout.base != layout.real {
        assert!(reference::can_write_effective(tie, &path, &cwd));
        assert!(writes.can_write_effective_path(&path));
    }
    assert!(!reference::can_grant_write_effective(tie, &path, &cwd));
    assert!(!writes.can_grant_write_effective_path(&path));

    let nested = FileSystemSandboxPolicy::workspace_write(
        &[abs(&layout.real.join("home/ws_via_home"))],
        /*exclude_tmpdir_env_var*/ true,
        /*exclude_slash_tmp*/ true,
    );
    let writes = nested.effective_writes_with_cwd(&cwd);
    let through_root = layout.real.join("ws/file");
    assert!(reference::can_write_effective(&nested, &through_root, &cwd));
    assert!(writes.can_write_effective_path(&through_root));
    assert_eq!(
        writes.can_grant_write_effective_path(&through_root),
        reference::can_grant_write_effective(&nested, &through_root, &cwd)
    );
}

/// The entries are normalized once per build, however many paths it checks:
/// one walk per entry naming a path, then one per path.
#[test]
fn issue_907_effective_writes_normalize_each_entry_once() {
    let layout = layout();
    let (_, policy) = policies(&layout)
        .into_iter()
        .find(|(name, _)| *name == "workspace with denials and read-only paths")
        .unwrap();
    let cwd = layout.real.join("ws");
    let paths = paths(&layout);
    let named = policy
        .entries
        .iter()
        .filter(|entry| {
            !matches!(
                entry.path,
                FileSystemPath::Special {
                    value: FileSystemSpecialPath::Root
                }
            )
        })
        .filter(|entry| resolve_entry_path(&entry.path, Some(&abs(&cwd))).is_some())
        .count() as u64;
    let before = effective_path_normalizations_on_this_thread();
    let writes = policy.effective_writes_with_cwd(&cwd);
    for path in &paths {
        writes.can_grant_write_effective_path(path);
    }
    let reused = effective_path_normalizations_on_this_thread() - before;
    assert_eq!(reused, named + paths.len() as u64);

    let before = effective_path_normalizations_on_this_thread();
    for path in &paths {
        reference::can_grant_write_effective(&policy, path, &cwd);
    }
    let per_path = effective_path_normalizations_on_this_thread() - before;
    assert_eq!(per_path, (named + 1) * paths.len() as u64);
}
