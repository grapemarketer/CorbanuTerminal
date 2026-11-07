#![cfg(unix)]
// The measurements print what they find.
#![allow(clippy::print_stderr)]

//! #907: what [`ReadDenials::apply`] costs in filesystem walks.

use super::*;
// `codex-protocol` is a normal dependency here, so its `cfg(test)` is off.
// With no per-package `debug-assertions` overrides, our gate matches its counter.
#[cfg(debug_assertions)]
use codex_protocol::permissions::effective_path_normalizations_on_this_thread;
use std::time::Duration;
use std::time::Instant;

fn abs(path: &Path) -> AbsolutePathBuf {
    AbsolutePathBuf::from_absolute_path(path).unwrap()
}

/// A Corbanu home and a git project in a temporary folder, and the
/// session's denials for a workspace-write profile there (the user's real
/// home is read as `for_turn` reads it).
#[cfg(debug_assertions)]
fn session() -> (tempfile::TempDir, ReadDenials, PermissionProfile) {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().canonicalize().unwrap();
    let codex_home = root.join(".corbanu");
    for folder in ["tmp/arg0", "shell_snapshots", "skills", "sessions"] {
        std::fs::create_dir_all(codex_home.join(folder)).unwrap();
    }
    for file in ["auth.json", "config.toml", "state_5.sqlite"] {
        std::fs::write(codex_home.join(file), "x").unwrap();
    }
    let cwd = abs(&root.join("project"));
    std::fs::create_dir_all(cwd.join(".git/hooks")).unwrap();
    let profile = PermissionProfile::workspace_write()
        .materialize_project_roots_with_workspace_roots(std::slice::from_ref(&cwd));
    let mut denials = ReadDenials::for_turn(
        &codex_home,
        &codex_home,
        &cwd,
        std::slice::from_ref(&cwd),
        &profile,
    );
    denials.deny_home_listing();
    (dir, denials, profile)
}

/// Filesystem normalizations one `apply` makes, with the counts it is
/// bounded by: the applied policy's entries and the read-only paths.
#[cfg(debug_assertions)]
fn normalizations(denials: &ReadDenials, profile: &PermissionProfile) -> (u64, usize, usize) {
    let before = effective_path_normalizations_on_this_thread();
    let applied = denials.apply(profile).unwrap();
    let count = effective_path_normalizations_on_this_thread() - before;
    let entries = applied.file_system_sandbox_policy().entries.len();
    let paths = denials.read_only_paths().count() + denials.home_writable_paths().count();
    (count, entries, paths)
}

#[test]
#[cfg(debug_assertions)]
fn issue_907_apply_normalizes_the_policy_entries_once_not_per_path() {
    let (_dir, denials, profile) = session();
    let (count, entries, paths) = normalizations(&denials, &profile);
    eprintln!("#907 apply: {count} normalizations, {entries} entries, {paths} read-only paths");
    assert!(paths > 5, "{paths}");
    // #918: tighten the existing linear ceiling without pinning a platform's
    // exact count or treating normalization calls as syscall/latency evidence.
    let bound = 3 * (entries + paths) as u64;
    assert!(
        count <= bound,
        "apply normalized {count} paths (bound {bound}): {entries} entries x {paths} paths"
    );
}

/// Run by hand for the #907 measurements:
/// `cargo test -p codex-core --lib issue_907_measure -- --ignored --nocapture`.
#[test]
#[cfg(debug_assertions)]
#[ignore = "measurement, run by hand"]
fn issue_907_measure_apply_latency() {
    let (_dir, denials, profile) = session();
    let runs: u32 = std::env::var("ISSUE_907_RUNS")
        .ok()
        .and_then(|runs| runs.parse().ok())
        .unwrap_or(50)
        .max(1);
    let mut times: Vec<Duration> = Vec::new();
    for _ in 0..runs {
        let start = Instant::now();
        let _ = denials.apply(&profile);
        times.push(start.elapsed());
    }
    times.sort();
    let (count, entries, paths) = normalizations(&denials, &profile);
    let ms = |time: Duration| time.as_secs_f64() * 1000.0;
    eprintln!(
        "#907 apply x{runs}: median {:.2} ms, p90 {:.2} ms, max {:.2} ms; \
         {count} normalizations, {entries} entries, {paths} read-only paths",
        ms(times[times.len() / 2]),
        ms(times[times.len() * 9 / 10]),
        ms(times[times.len() - 1]),
    );
}

/// Run by hand for the #907 measurements: one Aggressive denial
/// explanation (#442), the lookup `protected_reads` bounds at 2 s, timed
/// end to end.
/// `cargo test -p codex-core --lib issue_907_measure -- --ignored --nocapture`.
#[tokio::test]
#[ignore = "measurement, run by hand"]
async fn issue_907_measure_lookup_latency() {
    use crate::security::protected_reads::denial_explanation;
    use codex_security_policy::SecurityLevel;

    let (session, turn) = crate::session::tests::make_session_and_context().await;
    #[allow(deprecated)]
    let cwd = turn.cwd.clone();
    let home = turn.config.codex_home.to_path_buf();
    std::fs::create_dir_all(&home).unwrap();
    std::fs::write(home.join("auth.json"), "x").unwrap();
    let command = vec![
        "/bin/zsh".to_string(),
        "-lc".to_string(),
        format!("cat '{}'", home.join("auth.json").display()),
    ];
    let client = (*session.services.model_client())
        .clone()
        .with_ingress_level(SecurityLevel::Aggressive)
        .with_source_envelopes(/*enabled*/ true);
    session.services.replace_model_client(client);
    let runs: u32 = std::env::var("ISSUE_907_RUNS")
        .ok()
        .and_then(|runs| runs.parse().ok())
        .unwrap_or(30)
        .max(1);
    let mut times: Vec<Duration> = Vec::new();
    for _ in 0..runs {
        let start = Instant::now();
        let explanation = denial_explanation(
            &session,
            &turn,
            &command,
            cwd.as_path(),
            Some(1),
            "cat: auth.json: Operation not permitted\n",
        )
        .await;
        times.push(start.elapsed());
        assert!(explanation.is_some(), "explained under Aggressive");
    }
    times.sort();
    let ms = |time: Duration| time.as_secs_f64() * 1000.0;
    eprintln!(
        "#907 lookup x{runs}: median {:.2} ms, p90 {:.2} ms, max {:.2} ms",
        ms(times[times.len() / 2]),
        ms(times[times.len() * 9 / 10]),
        ms(times[times.len() - 1]),
    );
}

/// Run by hand on the code before and after #907 to show `apply` builds the
/// same sandbox policy: writes every scenario's applied profile (`Debug`)
/// under `$ISSUE_907_DUMP`, for a layout made afresh at the fixed folder
/// `$ISSUE_907_BASE`, so the two outputs can be compared byte for byte.
#[test]
#[ignore = "measurement, run by hand"]
fn issue_907_dump_applied_policies() {
    use codex_protocol::permissions::FileSystemAccessMode;
    use codex_protocol::permissions::FileSystemPath;
    use codex_protocol::permissions::FileSystemSandboxEntry;
    use codex_protocol::permissions::FileSystemSandboxPolicy;
    use codex_protocol::permissions::FileSystemSpecialPath;
    use codex_protocol::permissions::NetworkSandboxPolicy;
    use std::os::unix::fs::symlink;

    let (Some(base), Some(dump)) = (
        std::env::var_os("ISSUE_907_BASE").map(PathBuf::from),
        std::env::var_os("ISSUE_907_DUMP").map(PathBuf::from),
    ) else {
        panic!("set ISSUE_907_BASE and ISSUE_907_DUMP");
    };
    // Only a folder this test made is replaced.
    let marker = base.join(".issue-907-dump");
    if base.exists() {
        assert!(
            marker.exists(),
            "{base:?} exists and was not made by this test"
        );
        std::fs::remove_dir_all(&base).unwrap();
    }
    std::fs::create_dir_all(&dump).unwrap();
    let real = {
        std::fs::create_dir_all(&base).unwrap();
        std::fs::write(&marker, "").unwrap();
        base.canonicalize().unwrap()
    };
    let home = real.join(".corbanu");
    for folder in [
        "tmp/arg0",
        "shell_snapshots",
        "skills",
        "sessions",
        "worktrees/w",
        "rules",
    ] {
        std::fs::create_dir_all(home.join(folder)).unwrap();
    }
    for file in ["auth.json", "config.toml", "state_5.sqlite", "AGENTS.md"] {
        std::fs::write(home.join(file), "x").unwrap();
    }
    let project = real.join("project");
    for folder in [".git/hooks", ".codex", ".agents", "tie", "src"] {
        std::fs::create_dir_all(project.join(folder)).unwrap();
    }
    std::fs::create_dir_all(real.join("users/me")).unwrap();
    symlink(&project, real.join("project_link")).unwrap();
    symlink(&project, real.join("users/me/ws")).unwrap();
    symlink(&home, real.join("users/me/.corbanu")).unwrap();
    symlink(&home, project.join("home_link")).unwrap();
    let path = |path: &Path| abs(path);
    let write = |paths: &[&Path]| {
        PermissionProfile::from_runtime_permissions(
            &FileSystemSandboxPolicy::restricted(
                std::iter::once(FileSystemSandboxEntry::new(
                    FileSystemPath::Special {
                        value: FileSystemSpecialPath::Root,
                    },
                    FileSystemAccessMode::Read,
                ))
                .chain(paths.iter().map(|folder| {
                    FileSystemSandboxEntry::new(
                        FileSystemPath::Path { path: abs(folder) },
                        FileSystemAccessMode::Write,
                    )
                }))
                .collect(),
            ),
            NetworkSandboxPolicy::Restricted,
        )
    };
    let workspace = |root: &Path| {
        PermissionProfile::workspace_write()
            .materialize_project_roots_with_workspace_roots(&[abs(root)])
    };
    let tie = {
        let mut profile = write(&[&base.join("project/tie"), &project]);
        let (mut file_system, network) = profile.to_runtime_permissions();
        file_system.entries.push(FileSystemSandboxEntry::new(
            FileSystemPath::Path {
                path: abs(&project.join("tie")),
            },
            FileSystemAccessMode::Read,
        ));
        profile = PermissionProfile::from_runtime_permissions(&file_system, network);
        profile
    };
    let scenarios: Vec<(&str, PathBuf, PermissionProfile)> = vec![
        ("workspace", project.clone(), workspace(&project)),
        ("read-only", project.clone(), PermissionProfile::read_only()),
        ("disabled", project.clone(), PermissionProfile::Disabled),
        ("in the home", home.clone(), workspace(&home)),
        (
            "workspace through a link",
            real.join("project_link"),
            workspace(&real.join("project_link")),
        ),
        (
            "nested-link root (#652)",
            project.clone(),
            write(&[&real.join("users/me/ws")]),
        ),
        ("writable above the home", project.clone(), write(&[&real])),
        (
            "home through a link",
            project.clone(),
            write(&[&project, &real.join("users/me/.corbanu")]),
        ),
        ("two spellings (#652 ties)", project.clone(), tie),
        (
            "system spelling",
            base.join("project"),
            write(&[&base.join("project"), &base.join(".corbanu")]),
        ),
    ];
    let mut out = String::new();
    for (name, cwd, profile) in scenarios {
        let cwd = path(&cwd);
        let roots = std::slice::from_ref(&cwd);
        let denials = || ReadDenials::for_turn(&home, &home, &cwd, roots, &profile);
        let variants: Vec<(&str, ReadDenials)> = vec![
            ("plain", denials()),
            ("aggressive", {
                let mut denials = denials();
                denials.deny_home_listing();
                denials.deny_keychain_services();
                denials
            }),
            ("lifted", {
                let mut denials = denials();
                denials.lift_write_rules();
                denials
            }),
            ("moderate", {
                let mut denials = denials();
                denials.mark_moderate(&home);
                denials
            }),
            ("runtime only", denials().runtime_only()),
        ];
        for (variant, denials) in variants {
            out.push_str(&format!(
                "== {name} / {variant}\n{:#?}\n",
                denials.apply(&profile)
            ));
        }
    }
    std::fs::write(dump.join("applied.txt"), out).unwrap();
}
