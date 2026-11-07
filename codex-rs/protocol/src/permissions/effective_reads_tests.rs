//! Issue918: native read verdicts and normalization work on synthetic paths.

use super::*;
use pretty_assertions::assert_eq;

// Frozen from 78dd811d's can_read_effective_path_with_cwd. Only the method
// name/receiver are adapted: the algorithm below replaces `self` with `policy`.
fn reference_read(policy: &FileSystemSandboxPolicy, path: &Path, cwd: &Path) -> bool {
    match policy.kind {
        FileSystemSandboxKind::Unrestricted | FileSystemSandboxKind::ExternalSandbox => {
            return true;
        }
        FileSystemSandboxKind::Restricted => {}
    }
    let Some(path) = resolve_candidate_path(path, cwd) else {
        return false;
    };
    let path = normalize_effective_absolute_path(path);
    policy
        .resolved_entries_with_cwd(cwd)
        .into_iter()
        .map(|entry| ResolvedFileSystemEntry {
            path: normalize_effective_absolute_path(entry.path),
            access: entry.access,
        })
        .filter(|entry| path.as_path().starts_with(entry.path.as_path()))
        .max_by_key(resolved_entry_precedence)
        .is_some_and(|entry| entry.access.can_read())
}

fn entry(path: &Path, access: FileSystemAccessMode) -> FileSystemSandboxEntry {
    FileSystemSandboxEntry::new(
        FileSystemPath::Path {
            path: AbsolutePathBuf::from_absolute_path(path).unwrap(),
        },
        access,
    )
}

fn special(value: FileSystemSpecialPath, access: FileSystemAccessMode) -> FileSystemSandboxEntry {
    FileSystemSandboxEntry::new(FileSystemPath::Special { value }, access)
}

struct Fixture {
    _directory: tempfile::TempDir,
    written: PathBuf,
    real: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        // Exercise the real /tmp -> /private/tmp top-level alias on macOS.
        #[cfg(target_os = "macos")]
        let directory = tempfile::tempdir_in("/tmp").unwrap();
        #[cfg(not(target_os = "macos"))]
        let directory = tempfile::tempdir().unwrap();
        let written = directory.path().to_path_buf();
        let real = written.canonicalize().unwrap();
        for name in [
            "ws/open",
            "ws/denied/deeper",
            "outside",
            "ws/.git",
            "ws/tie",
        ] {
            std::fs::create_dir_all(real.join(name)).unwrap();
        }
        std::fs::write(real.join("ws/open/file"), "synthetic").unwrap();
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(real.join("outside"), real.join("ws/nested")).unwrap();
            std::os::unix::fs::symlink(real.join("missing"), real.join("dangling")).unwrap();
        }
        Self {
            _directory: directory,
            written,
            real,
        }
    }

    fn paths(&self) -> Vec<PathBuf> {
        let mut paths = Vec::new();
        for base in [&self.written, &self.real] {
            for tail in [
                "",
                "ws",
                "ws/open/file",
                "ws/open/missing",
                "ws/denied/file",
                "ws/denied/deeper/file",
                "ws/tie/file",
                "ws/.git/config",
                "ws/nested/file",
                "outside/file",
                "missing/deep/file",
                "dangling/file",
                "ws/../outside/file",
            ] {
                paths.push(base.join(tail));
            }
        }
        paths.extend(
            [
                "",
                ".",
                "open/file",
                "denied/file",
                "../outside/file",
                "missing/file",
                "bad\0path",
            ]
            .into_iter()
            .map(PathBuf::from),
        );
        paths
    }

    fn policies(&self) -> Vec<(&'static str, FileSystemSandboxPolicy)> {
        use FileSystemAccessMode::Deny;
        use FileSystemAccessMode::Read;
        use FileSystemAccessMode::Write;
        let root = || special(FileSystemSpecialPath::Root, Read);
        let tied = vec![
            root(),
            entry(&self.real.join("ws/tie"), Read),
            entry(&self.real.join("ws/tie"), Deny),
            entry(&self.real.join("ws/tie"), Write),
        ];
        let mut reversed = tied.clone();
        reversed.reverse();
        vec![
            ("unrestricted", FileSystemSandboxPolicy::unrestricted()),
            ("external", FileSystemSandboxPolicy::external_sandbox()),
            ("empty", FileSystemSandboxPolicy::restricted(Vec::new())),
            (
                "read-root",
                FileSystemSandboxPolicy::restricted(vec![root()]),
            ),
            (
                "deny-root",
                FileSystemSandboxPolicy::restricted(vec![special(
                    FileSystemSpecialPath::Root,
                    Deny,
                )]),
            ),
            (
                "nested-precedence",
                FileSystemSandboxPolicy::restricted(vec![
                    root(),
                    entry(&self.real.join("ws/denied"), Deny),
                    entry(&self.real.join("ws/denied/deeper"), Write),
                ]),
            ),
            ("ties", FileSystemSandboxPolicy::restricted(tied)),
            (
                "reversed-ties",
                FileSystemSandboxPolicy::restricted(reversed),
            ),
            (
                "duplicate-denial",
                FileSystemSandboxPolicy::restricted(vec![
                    root(),
                    entry(&self.real.join("ws/denied"), Deny),
                    entry(&self.real.join("ws/denied"), Deny),
                ]),
            ),
            (
                "missing-denial",
                FileSystemSandboxPolicy::restricted(vec![
                    root(),
                    entry(&self.real.join("missing"), Deny),
                ]),
            ),
            (
                "alias-ties",
                FileSystemSandboxPolicy::restricted(vec![
                    root(),
                    entry(&self.written.join("ws/tie"), Read),
                    entry(&self.real.join("ws/tie"), Deny),
                ]),
            ),
            (
                "nested-link",
                FileSystemSandboxPolicy::restricted(vec![
                    root(),
                    entry(&self.real.join("ws/nested"), Deny),
                    entry(&self.real.join("outside"), Write),
                ]),
            ),
            (
                "workspace",
                FileSystemSandboxPolicy::workspace_write(
                    &[AbsolutePathBuf::from_absolute_path(self.real.join("ws")).unwrap()],
                    /*exclude_tmpdir_env_var*/ true,
                    /*exclude_slash_tmp*/ true,
                ),
            ),
            (
                "special-and-glob",
                FileSystemSandboxPolicy::restricted(vec![
                    special(FileSystemSpecialPath::Minimal, Read),
                    special(
                        FileSystemSpecialPath::project_roots(/*subpath*/ None),
                        Write,
                    ),
                    special(
                        FileSystemSpecialPath::project_roots(Some("denied".to_string())),
                        Deny,
                    ),
                    special(FileSystemSpecialPath::Tmpdir, Read),
                    special(FileSystemSpecialPath::SlashTmp, Read),
                    special(
                        FileSystemSpecialPath::Unknown {
                            path: ":future".to_string(),
                            subpath: None,
                        },
                        Write,
                    ),
                    FileSystemSandboxEntry::new(
                        FileSystemPath::GlobPattern {
                            scan: None,
                            pattern: self.real.join("**/*.key").to_string_lossy().into_owned(),
                        },
                        Deny,
                    ),
                    FileSystemSandboxEntry::deny_directory_listing(
                        &AbsolutePathBuf::from_absolute_path(self.real.join("outside")).unwrap(),
                    ),
                ]),
            ),
        ]
    }

    fn cwds(&self) -> Vec<PathBuf> {
        vec![
            self.real.join("ws"),
            self.written.join("ws"),
            self.real.join("outside"),
            self.real.join("missing/cwd"),
            PathBuf::from("relative-cwd"),
        ]
    }
}

#[test]
fn issue918_baseline_current_reads_match_frozen_reference() {
    let fixture = Fixture::new();
    let paths = fixture.paths();
    let mut answers = [0usize; 2];
    let mut comparisons = 0;
    for (name, policy) in fixture.policies() {
        for cwd in fixture.cwds() {
            let expected: Vec<_> = paths
                .iter()
                .map(|path| reference_read(&policy, path, &cwd))
                .collect();
            let actual: Vec<_> = paths
                .iter()
                .map(|path| policy.can_read_effective_path_with_cwd(path, &cwd))
                .collect();
            assert_eq!(actual, expected, "{name}");
            for verdict in actual {
                answers[usize::from(verdict)] += 1;
                comparisons += 1;
            }
        }
    }
    assert!(answers.iter().all(|count| *count > 100));
    println!(
        "ISSUE918_BASELINE_MATRIX comparisons={comparisons} denied={} readable={} alias_exercised={}",
        answers[0],
        answers[1],
        fixture.written != fixture.real
    );
}

#[test]
fn issue918_baseline_repeats_entry_normalization_per_path() {
    let fixture = Fixture::new();
    let cwd = fixture.real.join("ws");
    let policy = FileSystemSandboxPolicy::restricted(vec![
        special(FileSystemSpecialPath::Root, FileSystemAccessMode::Read),
        entry(&fixture.real.join("ws/denied"), FileSystemAccessMode::Deny),
        entry(
            &fixture.real.join("ws/denied/deeper"),
            FileSystemAccessMode::Write,
        ),
        entry(&fixture.real.join("missing"), FileSystemAccessMode::Deny),
    ]);
    let paths = fixture.paths();
    let entries = policy.resolved_entries_with_cwd(&cwd).len() as u64;
    let before = effective_path_normalizations_on_this_thread();
    let verdicts: Vec<_> = paths
        .iter()
        .map(|path| policy.can_read_effective_path_with_cwd(path, &cwd))
        .collect();
    let observed = effective_path_normalizations_on_this_thread() - before;
    assert_eq!(observed, (entries + 1) * paths.len() as u64);
    assert_eq!(
        verdicts,
        paths
            .iter()
            .map(|path| reference_read(&policy, path, &cwd))
            .collect::<Vec<_>>()
    );
    println!(
        "ISSUE918_BASELINE_WORK entries={entries} paths={} walks={observed}",
        paths.len()
    );
}

#[test]
fn issue918_reused_reads_match_every_reference_verdict() {
    let fixture = Fixture::new();
    let paths = fixture.paths();
    for (name, policy) in fixture.policies() {
        for (cwd_case, cwd) in fixture.cwds().into_iter().enumerate() {
            let reads = policy.effective_reads_with_cwd(&cwd);
            let expected: Vec<_> = paths
                .iter()
                .map(|path| reference_read(&policy, path, &cwd))
                .collect();
            let actual: Vec<_> = paths
                .iter()
                .map(|path| reads.can_read_effective_path(path))
                .collect();
            assert_eq!(actual, expected, "{name}, cwd case {cwd_case}");
            let bits = |verdicts: &[bool]| {
                verdicts
                    .iter()
                    .map(|yes| if *yes { '1' } else { '0' })
                    .collect::<String>()
            };
            println!(
                "ISSUE918_EQ policy={name};cwd_case={cwd_case};expected={};actual={}",
                bits(&expected),
                bits(&actual)
            );
        }
    }
}

#[test]
fn issue918_entries_normalized_once_per_fresh_path_set() {
    let fixture = Fixture::new();
    let cwd = fixture.real.join("ws");
    let policy = FileSystemSandboxPolicy::restricted(vec![
        special(FileSystemSpecialPath::Root, FileSystemAccessMode::Read),
        entry(&fixture.real.join("ws/denied"), FileSystemAccessMode::Deny),
        entry(
            &fixture.real.join("ws/denied/deeper"),
            FileSystemAccessMode::Write,
        ),
        entry(&fixture.real.join("missing"), FileSystemAccessMode::Deny),
    ]);
    let paths = fixture.paths();
    let entries = policy.resolved_entries_with_cwd(&cwd).len() as u64;
    let before = effective_path_normalizations_on_this_thread();
    let expected: Vec<_> = paths
        .iter()
        .map(|path| policy.can_read_effective_path_with_cwd(path, &cwd))
        .collect();
    let old_walks = effective_path_normalizations_on_this_thread() - before;
    assert_eq!(old_walks, (entries + 1) * paths.len() as u64);
    for set in 0..2 {
        let before = effective_path_normalizations_on_this_thread();
        let reads = policy.effective_reads_with_cwd(&cwd);
        assert_eq!(effective_path_normalizations_on_this_thread(), before);
        let actual: Vec<_> = paths
            .iter()
            .map(|path| reads.can_read_effective_path(path))
            .collect();
        let walks = effective_path_normalizations_on_this_thread() - before;
        assert_eq!(actual, expected);
        assert_eq!(walks, entries + paths.len() as u64);
        println!(
            "ISSUE918_WORK set={set} entries={entries} paths={} old_walks={old_walks} batched_walks={walks}",
            paths.len()
        );
    }
}

#[test]
fn issue918_fast_paths_and_distinct_cwds_preserve_the_contract() {
    let fixture = Fixture::new();
    for policy in [
        FileSystemSandboxPolicy::unrestricted(),
        FileSystemSandboxPolicy::external_sandbox(),
    ] {
        let before = effective_path_normalizations_on_this_thread();
        let reads = policy.effective_reads_with_cwd(Path::new("relative-cwd"));
        for path in fixture.paths() {
            assert!(reads.can_read_effective_path(&path));
        }
        assert_eq!(effective_path_normalizations_on_this_thread(), before);
    }
    let policy = FileSystemSandboxPolicy::restricted(vec![
        special(FileSystemSpecialPath::Root, FileSystemAccessMode::Deny),
        special(
            FileSystemSpecialPath::project_roots(/*subpath*/ None),
            FileSystemAccessMode::Read,
        ),
    ]);
    // AbsolutePathBuf::from_absolute_path absolutizes a relative cwd; it is
    // not the checked constructor that rejects one. Preserve that behavior.
    let cwd = Path::new("relative-cwd");
    let path = Path::new("relative-path");
    let before = effective_path_normalizations_on_this_thread();
    let expected = reference_read(&policy, path, cwd);
    let reference_walks = effective_path_normalizations_on_this_thread() - before;
    let before = effective_path_normalizations_on_this_thread();
    let actual = policy
        .effective_reads_with_cwd(cwd)
        .can_read_effective_path(path);
    let actual_walks = effective_path_normalizations_on_this_thread() - before;
    assert_eq!((actual, actual_walks), (expected, reference_walks));
    println!(
        "ISSUE918_RELATIVE_CWD expected={expected} actual={actual} reference_walks={reference_walks} actual_walks={actual_walks}"
    );
    let cwd = fixture.real.join("ws");
    let other = fixture.real.join("outside");
    let path = cwd.join("open/file");
    assert_eq!(
        (
            policy
                .effective_reads_with_cwd(&cwd)
                .can_read_effective_path(&path),
            policy
                .effective_reads_with_cwd(&other)
                .can_read_effective_path(&path)
        ),
        (true, false)
    );
}
