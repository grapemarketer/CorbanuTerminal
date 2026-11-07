//! D324 (#1648): D259's peer probes between elevated-backend commands. Every
//! elevated command runs as one shared sandbox account per network mode, and
//! that account is a restricting SID of its token, so its processes must not
//! grant the account (or Everyone, or the capability SIDs) write access.
//!
//! An elevated session runs the sandbox's setup itself. A normal session
//! can't (UAC), so it reuses a setup recorded by an elevated run right
//! before: `CODEX_PF27S06_SETUP_SEED` names the directory (see
//! `codex-core`'s `launch_contract_windows_workspace_write_tests.rs`). An
//! elevated run with the variable set records its setup there.

use super::*;

const SETUP_SEED_ENV: &str = "CODEX_PF27S06_SETUP_SEED";
/// Set where a skip must fail (the normal-session gate with a recorded setup).
const REQUIRE_ENV: &str = "CODEX_SEC_WIN_1648_REQUIRE";
const SETUP_FILES: [(&str, &str); 2] = [
    ("setup_marker.json", ".sandbox"),
    ("sandbox_users.json", ".sandbox-secrets"),
];

/// A codex home for an elevated-backend test, or `None` to skip it.
fn elevated_home(name: &str) -> Option<TempDir> {
    // Bazel gives unit tests no staged sandbox helpers (#691).
    if option_env!("BAZEL_PACKAGE").is_some() {
        return None;
    }
    let home = sandbox_home(name);
    if !crate::setup::is_elevated().expect("elevation") {
        let seed = std::env::var_os(SETUP_SEED_ENV)
            .map(PathBuf::from)
            .filter(|seed| {
                SETUP_FILES
                    .iter()
                    .all(|(file, _)| seed.join(file).is_file())
            });
        let Some(seed) = seed else {
            assert!(
                std::env::var_os(REQUIRE_ENV).is_none()
                    && std::env::var_os("CODEX_SEC_WIN_341_REQUIRE").is_none(),
                "sec-win-1648 needs an elevated run or a recorded setup in {SETUP_SEED_ENV}"
            );
            eprintln!("sec-win-1648: no elevated run and no recorded setup; skipped");
            return None;
        };
        for (file, dir) in SETUP_FILES {
            let dir = home.path().join(dir);
            fs::create_dir_all(&dir).expect("seed dir");
            fs::copy(seed.join(file), dir.join(file)).expect("seed elevated setup");
        }
    }
    crate::window_station::tests::stage_sandbox_helpers();
    Some(home)
}

/// After an elevated run, records its setup for a normal-session run.
fn record_setup(home: &Path) {
    let Some(seed) = std::env::var_os(SETUP_SEED_ENV).map(PathBuf::from) else {
        return;
    };
    if !crate::setup::is_elevated().expect("elevation") {
        return;
    }
    fs::create_dir_all(&seed).expect("seed dir");
    for (file, dir) in SETUP_FILES {
        fs::copy(home.join(dir).join(file), seed.join(file)).expect("record elevated setup");
    }
}

async fn spawn_elevated(
    root: &Path,
    codex_home: &Path,
    argv: Vec<String>,
    env: &[(&str, String)],
    tty: bool,
) -> codex_utils_pty::SpawnedProcess {
    // Core passes its environment on; PowerShell needs it.
    let mut env_map: HashMap<String, String> = std::env::vars().collect();
    for (key, value) in env {
        env_map.insert((*key).to_string(), value.clone());
    }
    spawn_windows_sandbox_session_elevated_for_permission_profile(
        &PermissionProfile::workspace_write(),
        workspace_roots_for(root).as_slice(),
        codex_home,
        argv,
        root,
        env_map,
        /*proxy_enforced*/ false,
        /*network_proxy_restricting_sid*/ None,
        Some(90_000),
        /*read_roots_override*/ None,
        /*read_roots_include_platform_defaults*/ true,
        /*write_roots_override*/ None,
        &crate::DenyReadTargets::default(),
        &[],
        tty,
        // A ConPTY whose input closes ends its console's processes.
        /*stdin_open*/
        tty,
        /*use_private_desktop*/ true,
    )
    .await
    .unwrap_or_else(|err| {
        panic!(
            "spawn elevated command: {err:#}\n{}",
            sandbox_log(codex_home)
        )
    })
}

/// Runs `argv` as an elevated workspace-write command on `root`; returns its
/// stdout and exit code.
async fn run_elevated(
    root: &Path,
    codex_home: &Path,
    argv: Vec<String>,
    env: &[(&str, String)],
    tty: bool,
) -> (String, i32) {
    let spawned = spawn_elevated(root, codex_home, argv, env, tty).await;
    let (stdout, exit_code) =
        collect_stdout_and_exit(spawned, codex_home, Duration::from_secs(/*secs*/ 120)).await;
    (String::from_utf8_lossy(&stdout).into_owned(), exit_code)
}

/// [`with_holder`] for the elevated backend.
async fn with_elevated_holder<T>(
    root: &Path,
    codex_home: &Path,
    probe: impl AsyncFnOnce(u32) -> T,
) -> T {
    with_elevated_holder_tty(root, codex_home, /*tty*/ false, probe).await
}

/// [`with_elevated_holder`], the holding command in a ConPTY when `tty`.
async fn with_elevated_holder_tty<T>(
    root: &Path,
    codex_home: &Path,
    tty: bool,
    probe: impl AsyncFnOnce(u32) -> T,
) -> T {
    let hold_dir = root.join("hold");
    fs::create_dir_all(&hold_dir).expect("create hold dir");
    let release = Release(hold_dir.join("hold.release"));
    let holder = spawn_elevated(
        root,
        codex_home,
        probe_argv(HOLD_ENTRY),
        &[(HOLD_ENV, hold_dir.display().to_string())],
        tty,
    )
    .await;
    let pid_path = hold_dir.join("hold.pid");
    let deadline = Instant::now() + Duration::from_secs(/*secs*/ 60);
    let holder_pid = loop {
        if let Ok(pid) = fs::read_to_string(&pid_path)
            && let Ok(pid) = pid.trim().parse::<u32>()
        {
            break pid;
        }
        assert!(
            Instant::now() < deadline,
            "holding command never started\n{}",
            sandbox_log(codex_home)
        );
        tokio::time::sleep(Duration::from_millis(/*millis*/ 100)).await;
    };
    let result = probe(holder_pid).await;
    drop(release);
    let _ = collect_stdout_and_exit(holder, codex_home, Duration::from_secs(/*secs*/ 90)).await;
    result
}

/// The probe's result line, or a panic with the sandbox log.
fn expect_line(stdout: &str, codex_home: &Path) -> String {
    probe_line(stdout).unwrap_or_else(|| {
        panic!(
            "no probe result: stdout={stdout:?}\n{}",
            sandbox_log(codex_home)
        )
    })
}

/// The parts of a peer report that a closed process must show.
fn refused_parts() -> Vec<String> {
    let mut expected: Vec<String> = WRITE_RIGHTS
        .iter()
        .map(|(name, _)| format!("{name}=err5"))
        .collect();
    expected.push("query=ok".to_string());
    expected.push("token_adjust_default=open_err5".to_string());
    expected
}

fn missing_parts(line: &str) -> Vec<String> {
    refused_parts()
        .into_iter()
        .filter(|part| !line.split(' ').any(|have| have == part.as_str()))
        .collect()
}

/// Whether a `peer_parent` report is about a command runner (its file name
/// carries the version).
fn is_runner(line: &str) -> bool {
    line.to_lowercase()
        .starts_with("parent=codex-command-runner")
}

/// A folder outside every writable root that Authenticated Users may
/// change, as on a secondary drive's root by default: the sandbox account's
/// unrestricted token may write there, its commands' restricted tokens may
/// not.
fn authenticated_users_folder() -> TempDir {
    let dir = user_owned_test_root();
    let output = std::process::Command::new(system32_exe("icacls.exe"))
        .arg(dir.path())
        .args(["/grant", "*S-1-5-11:(OI)(CI)M"])
        .output()
        .expect("run icacls");
    assert!(output.status.success(), "icacls: {output:?}");
    dir
}

/// D324: an elevated command can't open another workspace's concurrent
/// elevated command's process for any write right (start a child under it,
/// write its memory, change its DACL) or adjust its token. It can still
/// query it (and end it, D292).
#[test]
fn sec_win_1648_elevated_command_cannot_write_to_another_workspaces_process() {
    let Some(codex_home) = elevated_home("d324-peer") else {
        return;
    };
    let _guard = legacy_process_test_guard();
    let holder_root = user_owned_test_root();
    let root = user_owned_test_root();
    let runtime = current_thread_runtime();
    let ((stdout, exit_code), (console_stdout, console_exit)) = runtime.block_on(
        with_elevated_holder(holder_root.path(), codex_home.path(), async |holder_pid| {
            let peer = run_elevated(
                root.path(),
                codex_home.path(),
                probe_argv(PROBE_ENTRY),
                &[
                    (PROBE_ENV, "peer".to_string()),
                    (TARGET_ENV, holder_pid.to_string()),
                ],
                /*tty*/ false,
            )
            .await;
            // The holding command's own console's host, if it has one.
            let console = run_elevated(
                root.path(),
                codex_home.path(),
                probe_argv(PROBE_ENTRY),
                &[
                    (PROBE_ENV, "peer_command_console".to_string()),
                    (TARGET_ENV, holder_pid.to_string()),
                ],
                /*tty*/ false,
            )
            .await;
            (peer, console)
        }),
    );
    record_setup(codex_home.path());
    let line = expect_line(&stdout, codex_home.path());
    let console_line = expect_line(&console_stdout, codex_home.path());
    println!(
        "\nd324 peer: exit={exit_code} {line:?}\nd324 peer command console: exit={console_exit} {console_line:?}"
    );
    let missing = missing_parts(&line);
    assert!(
        missing.is_empty(),
        "another elevated command's process is open to writes: {line}\nmissing={missing:?}"
    );
    assert!(
        console_line == "hosts=0"
            || console_line
                .split(" | ")
                .all(|host| missing_parts(host).is_empty()),
        "another elevated command's console host is open to writes: {console_line}"
    );
}

/// D324: nor the runner that started that command (it runs with the sandbox
/// account's unrestricted token, so a child started under it would too).
#[test]
fn sec_win_1648_elevated_command_cannot_write_to_another_commands_runner() {
    let Some(codex_home) = elevated_home("d324-runner") else {
        return;
    };
    let _guard = legacy_process_test_guard();
    let holder_root = user_owned_test_root();
    let root = user_owned_test_root();
    let runtime = current_thread_runtime();
    let (stdout, exit_code) = runtime.block_on(with_elevated_holder(
        holder_root.path(),
        codex_home.path(),
        async |holder_pid| {
            run_elevated(
                root.path(),
                codex_home.path(),
                probe_argv(PROBE_ENTRY),
                &[
                    (PROBE_ENV, "peer_parent".to_string()),
                    (TARGET_ENV, holder_pid.to_string()),
                ],
                /*tty*/ false,
            )
            .await
        },
    ));
    record_setup(codex_home.path());
    let line = expect_line(&stdout, codex_home.path());
    println!("\nd324 runner peer: exit={exit_code} {line:?}");
    let missing = missing_parts(&line);
    assert!(
        is_runner(&line) && missing.is_empty(),
        "another elevated command's runner is open to writes: {line}\nmissing={missing:?}"
    );
}

/// D324: nor its own runner: a child started under it would leave the
/// command's job and restricted token.
#[test]
fn sec_win_1648_elevated_command_cannot_write_to_its_own_runner() {
    let Some(codex_home) = elevated_home("d324-own-runner") else {
        return;
    };
    let _guard = legacy_process_test_guard();
    let root = user_owned_test_root();
    let runtime = current_thread_runtime();
    let (stdout, exit_code) = runtime.block_on(run_elevated(
        root.path(),
        codex_home.path(),
        probe_argv(PROBE_ENTRY),
        &[
            (PROBE_ENV, "peer_parent".to_string()),
            (TARGET_ENV, "self".to_string()),
        ],
        /*tty*/ false,
    ));
    record_setup(codex_home.path());
    let line = expect_line(&stdout, codex_home.path());
    println!("\nd324 own runner peer: exit={exit_code} {line:?}");
    let missing = missing_parts(&line);
    assert!(
        is_runner(&line) && missing.is_empty(),
        "an elevated command's own runner is open to writes: {line}\nmissing={missing:?}"
    );
}

/// D324: an elevated command can't impersonate a runner's token (its own
/// runner's, or another command's) to write where the sandbox account's
/// groups may but its restricted token may not.
#[test]
fn sec_win_1648_elevated_command_cannot_borrow_a_runner_token() {
    let Some(codex_home) = elevated_home("d324-runner-borrow") else {
        return;
    };
    let _guard = legacy_process_test_guard();
    let holder_root = user_owned_test_root();
    let root = user_owned_test_root();
    let outside = authenticated_users_folder();
    let own_target = outside.path().join("own-runner.txt");
    let other_target = outside.path().join("other-runner.txt");
    let runtime = current_thread_runtime();
    let (own, other) = runtime.block_on(with_elevated_holder(
        holder_root.path(),
        codex_home.path(),
        async |holder_pid| {
            let own = run_elevated(
                root.path(),
                codex_home.path(),
                probe_argv(PROBE_ENTRY),
                &[
                    (PROBE_ENV, "borrow_parent".to_string()),
                    (TARGET_ENV, "self".to_string()),
                    (FILE_ENV, own_target.display().to_string()),
                ],
                /*tty*/ false,
            )
            .await;
            let other = run_elevated(
                root.path(),
                codex_home.path(),
                probe_argv(PROBE_ENTRY),
                &[
                    (PROBE_ENV, "borrow_parent".to_string()),
                    (TARGET_ENV, holder_pid.to_string()),
                    (FILE_ENV, other_target.display().to_string()),
                ],
                /*tty*/ false,
            )
            .await;
            (own, other)
        },
    ));
    record_setup(codex_home.path());
    let own_line = expect_line(&own.0, codex_home.path());
    let other_line = expect_line(&other.0, codex_home.path());
    println!(
        "\nd324 own runner borrow: exit={} {own_line:?} wrote={}\nd324 other runner borrow: exit={} {other_line:?} wrote={}",
        own.1,
        own_target.exists(),
        other.1,
        other_target.exists()
    );
    let refused = |line: &str| {
        line.contains(" own_write=err")
            && !line.contains("write=ok")
            && !line.contains("primary=ok")
    };
    assert!(
        refused(&own_line)
            && refused(&other_line)
            && !own_target.exists()
            && !other_target.exists(),
        "an elevated command wrote with a runner's token:\nown: {own_line}\nother: {other_line}"
    );
}

/// The elevated twin of `legacy_command_cannot_borrow_another_workspaces_command_token`.
#[test]
#[ignore = "#1560: impersonating another workspace's elevated command token writes there (measured on CI before and after D324); closing it for both backends follows the #1560 token measurement"]
fn sec_win_1648_elevated_command_cannot_borrow_another_workspaces_command_token() {
    let Some(codex_home) = elevated_home("d324-borrow") else {
        return;
    };
    let _guard = legacy_process_test_guard();
    let holder_root = user_owned_test_root();
    let root = user_owned_test_root();
    let target = holder_root.path().join("borrowed.txt");
    let runtime = current_thread_runtime();
    let (stdout, exit_code) = runtime.block_on(with_elevated_holder(
        holder_root.path(),
        codex_home.path(),
        async |holder_pid| {
            run_elevated(
                root.path(),
                codex_home.path(),
                probe_argv(PROBE_ENTRY),
                &[
                    (PROBE_ENV, "borrow".to_string()),
                    (TARGET_ENV, holder_pid.to_string()),
                    (FILE_ENV, target.display().to_string()),
                ],
                /*tty*/ false,
            )
            .await
        },
    ));
    record_setup(codex_home.path());
    let line = expect_line(&stdout, codex_home.path());
    println!(
        "\nd324 borrow: exit={exit_code} {line:?} wrote={}",
        target.exists()
    );
    assert!(
        line.starts_with("own_write=err")
            && !target.exists()
            && !line.contains("create_process=ok")
            && !line.contains("primary=ok"),
        "an elevated command wrote another workspace through its command's token: {line}"
    );
}

/// D324 review (#1717): nor the console hosts a runner started, its own
/// console's and a ConPTY's, another workspace's or its own command's. They
/// run with the runner's unrestricted token, so their processes must refuse
/// every write right (a child started under one leaves the command's job
/// and restricted token), and their tokens must give a command nothing it
/// can write with.
#[test]
fn sec_win_1648_elevated_command_cannot_write_to_or_borrow_a_console_host() {
    let Some(codex_home) = elevated_home("d324-console") else {
        return;
    };
    let _guard = legacy_process_test_guard();
    let holder_root = user_owned_test_root();
    let root = user_owned_test_root();
    let outside = authenticated_users_folder();
    let target = outside.path().join("console-host.txt");
    let own_out = root.path().join("own-console.txt");
    let runtime = current_thread_runtime();
    let (peer, borrow, own) = runtime.block_on(with_elevated_holder_tty(
        holder_root.path(),
        codex_home.path(),
        /*tty*/ true,
        async |holder_pid| {
            let peer = run_elevated(
                root.path(),
                codex_home.path(),
                probe_argv(PROBE_ENTRY),
                &[
                    (PROBE_ENV, "peer_console".to_string()),
                    (TARGET_ENV, holder_pid.to_string()),
                ],
                /*tty*/ false,
            )
            .await;
            let borrow = run_elevated(
                root.path(),
                codex_home.path(),
                probe_argv(PROBE_ENTRY),
                &[
                    (PROBE_ENV, "borrow_console".to_string()),
                    (TARGET_ENV, holder_pid.to_string()),
                    (FILE_ENV, target.display().to_string()),
                ],
                /*tty*/ false,
            )
            .await;
            // In a ConPTY of its own, so its runner has one too.
            let own = run_elevated(
                root.path(),
                codex_home.path(),
                probe_argv(PROBE_ENTRY),
                &[
                    (PROBE_ENV, "peer_console".to_string()),
                    (TARGET_ENV, "self".to_string()),
                    (OUT_ENV, own_out.display().to_string()),
                ],
                /*tty*/ true,
            )
            .await;
            (peer, borrow, own)
        },
    ));
    record_setup(codex_home.path());
    let peer_line = expect_line(&peer.0, codex_home.path());
    let borrow_line = expect_line(&borrow.0, codex_home.path());
    let own_line = fs::read_to_string(&own_out).unwrap_or_else(|err| {
        panic!(
            "no own console probe result ({err}): exit={} stdout={:?}\n{}",
            own.1,
            own.0,
            sandbox_log(codex_home.path())
        )
    });
    println!(
        "\nd324 console peer: exit={} {peer_line:?}\nd324 console borrow: exit={} {borrow_line:?} wrote={}\nd324 own console peer: exit={} {own_line:?}",
        peer.1,
        borrow.1,
        target.exists(),
        own.1
    );
    let closed = |line: &str| {
        line.starts_with("host=") && line.split(" | ").all(|host| missing_parts(host).is_empty())
    };
    assert!(
        closed(&peer_line) && closed(&own_line),
        "a console host is open to writes:\nother: {peer_line}\nown: {own_line}"
    );
    // Its token may still open for `TOKEN_DUPLICATE` (measured), but a
    // restricted token can only identify as an unrestricted one (1346), and
    // starting a process with it needs a privilege commands lack (1314).
    assert!(
        borrow_line.starts_with("host=")
            && borrow_line.split(" | ").all(|host| {
                host.contains(" own_write=err")
                    && !host.contains("write=ok")
                    && !host.contains("create_process=ok")
                    && !host.contains("primary=ok")
            })
            && !target.exists(),
        "a command wrote with a console host's token: {borrow_line}"
    );
}

/// Measurement for #1721, not a gate: a command racing the start-up of 20
/// other elevated commands' runners, counting runners it opened for
/// `PROCESS_CREATE_PROCESS` or whose token it opened for
/// `TOKEN_ADJUST_DEFAULT` before they restricted themselves.
#[test]
#[ignore = "#1721: measures the runner start-up window D324 leaves open; run with --include-ignored"]
fn sec_win_1648_measure_runner_startup_race() {
    let Some(codex_home) = elevated_home("d324-race") else {
        return;
    };
    let _guard = legacy_process_test_guard();
    let racer_root = user_owned_test_root();
    let root = user_owned_test_root();
    let race_dir = racer_root.path().join("race");
    fs::create_dir_all(&race_dir).expect("create race dir");
    let runtime = current_thread_runtime();
    let (line, exit_code) = runtime.block_on(async {
        let racer = spawn_elevated(
            racer_root.path(),
            codex_home.path(),
            probe_argv(PROBE_ENTRY),
            &[
                (PROBE_ENV, "race".to_string()),
                (CHILD_ENV, race_dir.display().to_string()),
            ],
            /*tty*/ false,
        )
        .await;
        assert!(
            wait_for_path(
                &race_dir.join("race.ready"),
                Duration::from_secs(/*secs*/ 60)
            ),
            "racing command never started\n{}",
            sandbox_log(codex_home.path())
        );
        let started = Instant::now();
        let cmd = system32_exe("cmd.exe").display().to_string();
        for _ in 0..20 {
            run_elevated(
                root.path(),
                codex_home.path(),
                vec![
                    cmd.clone(),
                    "/d".to_string(),
                    "/c".to_string(),
                    "exit 0".to_string(),
                ],
                &[],
                /*tty*/ false,
            )
            .await;
        }
        println!("\nd324 race: 20 commands in {:?}", started.elapsed());
        fs::write(race_dir.join("race.stop"), "stop").expect("write race stop");
        let (stdout, exit_code) =
            collect_stdout_and_exit(racer, codex_home.path(), Duration::from_secs(/*secs*/ 120))
                .await;
        (String::from_utf8_lossy(&stdout).into_owned(), exit_code)
    });
    record_setup(codex_home.path());
    let line = expect_line(&line, codex_home.path());
    println!("\nd324 race: exit={exit_code} {line:?}");
    assert!(line.starts_with("race seen="), "{line}");
}

/// D324 keeps what an elevated command may do to itself: open its own child
/// for every write right, adjust its own and the child's token default DACL,
/// and end the child.
#[test]
fn sec_win_1648_elevated_command_keeps_write_access_to_its_own_processes() {
    let Some(codex_home) = elevated_home("d324-self") else {
        return;
    };
    let _guard = legacy_process_test_guard();
    let root = user_owned_test_root();
    let child_dir = root.path().join("child");
    fs::create_dir_all(&child_dir).expect("create child dir");
    let runtime = current_thread_runtime();
    let (stdout, exit_code) = runtime.block_on(run_elevated(
        root.path(),
        codex_home.path(),
        probe_argv(PROBE_ENTRY),
        &[
            (PROBE_ENV, "self".to_string()),
            (CHILD_ENV, child_dir.display().to_string()),
        ],
        /*tty*/ false,
    ));
    record_setup(codex_home.path());
    let line = expect_line(&stdout, codex_home.path());
    println!("\nd324 self: exit={exit_code} {line:?}");
    let denied = line
        .split(' ')
        .any(|part| part.contains('=') && !part.ends_with("=ok") && !part.ends_with("=true"));
    assert!(
        !denied && line.contains("child_token_adjust_default=ok"),
        "an elevated command lost access to its own processes: {line}"
    );
}

/// D324 doesn't break common tools in an elevated command: a cmd pipeline,
/// Windows PowerShell calling a native program (with and without ConPTY),
/// and `git init` and `git status`.
#[test]
fn sec_win_1648_elevated_tools_work_with_command_identity() {
    let Some(codex_home) = elevated_home("d324-tools") else {
        return;
    };
    let Some(git) = git_for_windows("cmd\\git.exe") else {
        return;
    };
    let _guard = legacy_process_test_guard();
    let root = user_owned_test_root();
    let runtime = current_thread_runtime();
    let powershell = system32_exe("WindowsPowerShell\\v1.0\\powershell.exe")
        .display()
        .to_string();
    let cmd = system32_exe("cmd.exe").display().to_string();
    let cmd_c = |line: &str| {
        vec![
            cmd.clone(),
            "/d".to_string(),
            "/c".to_string(),
            line.to_string(),
        ]
    };
    // (name, argv, expected output, ConPTY)
    let cases: Vec<(&str, Vec<String>, &[&str], bool)> = vec![
        (
            "cmd pipeline",
            cmd_c("echo d324-pipe| findstr d324 2>&1"),
            &["d324-pipe"],
            false,
        ),
        (
            "powershell",
            vec![
                powershell.clone(),
                "-NoProfile".to_string(),
                "-Command".to_string(),
                "$x = & cmd /c echo d324-ps; $x | Select-String d324 | ForEach-Object { $_.Line }; \
                 Get-ChildItem | Out-Null; 'd324-ps-done'"
                    .to_string(),
            ],
            &["d324-ps", "d324-ps-done"],
            false,
        ),
        (
            "powershell (ConPTY)",
            vec![
                powershell,
                "-NoProfile".to_string(),
                "-Command".to_string(),
                "& cmd /c echo d324-tty; 'd324-tty-done'".to_string(),
            ],
            &["d324-tty", "d324-tty-done"],
            true,
        ),
        (
            "git init",
            vec![
                git.display().to_string(),
                "init".to_string(),
                "-q".to_string(),
            ],
            &[],
            false,
        ),
        (
            // `.git` is read-only to sandboxed commands, so no commit.
            "git status",
            cmd_c("(echo d324> d324.txt && git status --porcelain) 2>&1"),
            &["?? d324.txt"],
            false,
        ),
    ];
    let mut failures = Vec::new();
    for (name, argv, expected, tty) in cases {
        let (stdout, exit_code) =
            runtime.block_on(run_elevated(root.path(), codex_home.path(), argv, &[], tty));
        println!("\nd324 tool {name}: exit={exit_code} stdout={stdout:?}");
        if exit_code != 0 || !expected.iter().all(|part| stdout.contains(part)) {
            failures.push(format!("{name}: exit={exit_code} stdout={stdout:?}"));
        }
    }
    record_setup(codex_home.path());
    assert!(
        failures.is_empty(),
        "{failures:#?}\n{}",
        sandbox_log(codex_home.path())
    );
}
