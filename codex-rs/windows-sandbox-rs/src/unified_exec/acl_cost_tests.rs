//! D285 (#1542 item 6): what a legacy command's per-command ACL work costs
//! on a large workspace (about 101,000 files, half under `.git`). Ignored:
//! run them explicitly, one at a time, to measure; they print timings and
//! assert nothing about them:
//!
//! ```text
//! cargo test -p codex-windows-sandbox --lib -- --ignored --exact --nocapture \
//!     unified_exec::tests::acl_cost::legacy_acl_cost_on_a_large_workspace
//! cargo test -p codex-windows-sandbox --lib -- --ignored --exact --nocapture \
//!     unified_exec::tests::acl_cost::legacy_acl_call_cost_on_a_large_workspace
//! ```
//!
//! Results on the hosted windows-2022 runner (elevated) are on #1542.

use super::*;

const COMMANDS: usize = 6;

/// Fills `root` with `dirs` x `files` worktree files and a `.git` with
/// `objects/00`..`ff` holding `objects` loose-object-like files each.
fn fill_large_repo(root: &Path, dirs: usize, files: usize, objects: usize) -> usize {
    let mut count = 0;
    for d in 0..dirs {
        let dir = root.join(format!("src\\m{:02}\\d{d:03}", d % 20));
        fs::create_dir_all(&dir).expect("create worktree dir");
        for f in 0..files {
            fs::write(dir.join(format!("f{f:03}.rs")), b"// d285\n").expect("write file");
            count += 1;
        }
    }
    let git = root.join(".git");
    for name in ["refs\\heads", "refs\\tags", "info", "hooks"] {
        fs::create_dir_all(git.join(name)).expect("create .git dir");
    }
    fs::write(git.join("HEAD"), b"ref: refs/heads/main\n").expect("write HEAD");
    for prefix in 0..256 {
        let dir = git.join(format!("objects\\{prefix:02x}"));
        fs::create_dir_all(&dir).expect("create objects dir");
        for o in 0..objects {
            fs::write(dir.join(format!("{o:038x}")), b"x").expect("write object");
            count += 1;
        }
    }
    count
}

/// Runs `cmd /c exit 0` [`COMMANDS`] times back to back as a legacy
/// workspace-write command on `root`; prints each command's time to start
/// and to exit, the total, and how long the last release took.
///
/// What the numbers include (both from the command's start): `spawn` is
/// the ACL setup (the #158 Low label spreading over the tree, D261's tracked
/// denies, the capability entries; the first command also labels `.git`
/// Medium once), the token and the start; `exit` adds running the command
/// and, for a command that leaves nothing running, the release (label put
/// back, denies removed). `%TEMP%` is a writable root too, as in a real
/// session; the small workspace's numbers show that fixed part. An `exit`
/// about 2 s past `spawn` means the release moved to a background thread and
/// the next command reused the lease, which under-counts.
fn time_commands(label: &str, root: &Path) {
    let runtime = current_thread_runtime();
    let codex_home = sandbox_home("d285-cost");
    let mut lines = Vec::new();
    let total = Instant::now();
    for i in 0..COMMANDS {
        let started = Instant::now();
        let (spawn_ms, exit_ms, exit_code) = runtime.block_on(async {
            let spawned = spawn_windows_sandbox_session_legacy(
                &PermissionProfile::workspace_write(),
                workspace_roots_for(root).as_slice(),
                codex_home.path(),
                vec![
                    system32_exe("cmd.exe").display().to_string(),
                    "/d".to_string(),
                    "/c".to_string(),
                    "exit 0".to_string(),
                ],
                root,
                std::env::vars().collect(),
                /*timeout_ms*/ Some(600_000),
                &[],
                &[],
                /*tty*/ false,
                /*stdin_open*/ false,
                /*use_private_desktop*/ true,
            )
            .await
            .expect("spawn legacy command");
            let spawn_ms = started.elapsed().as_millis();
            let (_stdout, exit_code) = collect_stdout_and_exit(
                spawned,
                codex_home.path(),
                Duration::from_secs(/*secs*/ 600),
            )
            .await;
            (spawn_ms, started.elapsed().as_millis(), exit_code)
        });
        lines.push(format!(
            "{i}:spawn={spawn_ms}ms,exit={exit_ms}ms,code={exit_code}"
        ));
    }
    let loop_ms = total.elapsed().as_millis();
    let release = Instant::now();
    let canonical = dunce::canonicalize(root).expect("canonical root");
    loop {
        let recorded =
            crate::integrity_leases::recorded_roots().expect("read the integrity-label records");
        if !recorded
            .iter()
            .any(|recorded| recorded == &canonical || recorded == root)
        {
            break;
        }
        assert!(
            release.elapsed() < Duration::from_secs(/*secs*/ 600),
            "the last lease was not released"
        );
        std::thread::sleep(Duration::from_millis(/*millis*/ 50));
    }
    println!(
        "\nd285-cost {label}: commands={COMMANDS} loop={loop_ms}ms per_command={}ms last_release={}ms [{}]",
        loop_ms / COMMANDS as u128,
        release.elapsed().as_millis(),
        lines.join(" ")
    );
}

#[test]
#[ignore = "D285 measurement; run explicitly"]
fn legacy_acl_cost_on_a_large_workspace() {
    let _guard = legacy_process_test_guard();
    let small = user_owned_test_root();
    let created = Instant::now();
    let small_files = fill_large_repo(
        small.path(),
        /*dirs*/ 1,
        /*files*/ 1,
        /*objects*/ 1,
    );
    println!(
        "\nd285-cost small repo: files={small_files} in {}ms",
        created.elapsed().as_millis()
    );
    time_commands("small", small.path());
    let large = user_owned_test_root();
    let created = Instant::now();
    // 50,000 worktree files and 51,200 under .git/objects.
    let large_files = fill_large_repo(
        large.path(),
        /*dirs*/ 400,
        /*files*/ 125,
        /*objects*/ 200,
    );
    println!(
        "\nd285-cost large repo: files={large_files} in {}ms",
        created.elapsed().as_millis()
    );
    time_commands("large", large.path());
    time_commands("large-again", large.path());
}

/// Times each D261 deny call alone on the large workspace: the Everyone
/// `FILE_DELETE_CHILD` deny on the root (that folder only) and on `.git`
/// (inherited by its subfolders), added and removed.
#[test]
#[ignore = "D285 measurement; run explicitly"]
fn legacy_acl_call_cost_on_a_large_workspace() {
    let _guard = legacy_process_test_guard();
    let large = user_owned_test_root();
    let files = fill_large_repo(
        large.path(),
        /*dirs*/ 400,
        /*files*/ 125,
        /*objects*/ 200,
    );
    let mut everyone = everyone_sid();
    let everyone = everyone.as_mut_ptr().cast::<std::ffi::c_void>();
    let git = large.path().join(".git");
    let mut lines = Vec::new();
    for round in 0..3 {
        for (name, path, inherit) in [("root", large.path(), false), (".git", git.as_path(), true)]
        {
            let started = Instant::now();
            // SAFETY: a valid SID and an existing folder.
            let added = unsafe { crate::acl::add_deny_delete_child_ace(path, everyone, inherit) };
            let add_ms = started.elapsed().as_millis();
            let started = Instant::now();
            // SAFETY: as above.
            let removed =
                unsafe { crate::acl::remove_deny_delete_child_ace(path, everyone, inherit) };
            lines.push(format!(
                "{round}:{name}:add={add_ms}ms({added:?}),remove={}ms({removed:?})",
                started.elapsed().as_millis()
            ));
        }
    }
    println!("\nd285-calls files={files} [{}]", lines.join(" "));
}
