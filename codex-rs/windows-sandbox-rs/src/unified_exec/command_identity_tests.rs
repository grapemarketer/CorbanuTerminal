//! D259 (#1507): each legacy command's token carries its own identity SID,
//! and its default DACL grants write access only to that SID (plus the user
//! and SYSTEM, which a legacy token can't use for writes). So a legacy
//! command keeps full access to its own processes and token, but can no
//! longer open another legacy command's processes for writing: before D259
//! Everyone and the logon SID, restricting SIDs of every legacy token, had
//! `GENERIC_ALL` there.
//!
//! The probe entries below do nothing unless their environment variable is
//! set; the tests re-run this test binary inside legacy sessions.

use super::*;
use windows_sys::Win32::Foundation::CloseHandle;
use windows_sys::Win32::Foundation::GetLastError;
use windows_sys::Win32::Foundation::HANDLE;
use windows_sys::Win32::Security::DuplicateTokenEx;
use windows_sys::Win32::Security::GetTokenInformation;
use windows_sys::Win32::Security::RevertToSelf;
use windows_sys::Win32::Security::SecurityImpersonation;
use windows_sys::Win32::Security::SetTokenInformation;
use windows_sys::Win32::Security::TOKEN_ADJUST_DEFAULT;
use windows_sys::Win32::Security::TOKEN_DUPLICATE;
use windows_sys::Win32::Security::TOKEN_IMPERSONATE;
use windows_sys::Win32::Security::TOKEN_QUERY;
use windows_sys::Win32::Security::TokenDefaultDacl;
use windows_sys::Win32::Security::TokenImpersonation;
use windows_sys::Win32::System::Threading::GetCurrentProcess;
use windows_sys::Win32::System::Threading::OpenProcessToken;
use windows_sys::Win32::System::Threading::PROCESS_QUERY_LIMITED_INFORMATION;
use windows_sys::Win32::System::Threading::PROCESS_TERMINATE;
use windows_sys::Win32::System::Threading::SetThreadToken;
use windows_sys::Win32::System::Threading::TerminateProcess;

const PROBE_ENTRY: &str = "unified_exec::tests::command_identity::command_identity_probe_entry";
const CHILD_ENTRY: &str = "unified_exec::tests::command_identity::command_identity_child_entry";
const HOLD_ENTRY: &str = "unified_exec::tests::command_identity::command_identity_hold_entry";
const PROBE_ENV: &str = "CODEX_D259_PROBE";
const TARGET_ENV: &str = "CODEX_D259_TARGET";
const CHILD_ENV: &str = "CODEX_D259_CHILD";
const HOLD_ENV: &str = "CODEX_D259_HOLD";
const FILE_ENV: &str = "CODEX_D259_FILE";
const RESULT: &str = "d259: ";

/// Process rights that are write rights for a write-restricted token (in the
/// process object's generic-write mapping), so the restricted-SID pass
/// decides them.
const WRITE_RIGHTS: [(&str, u32); 8] = [
    ("create_process", 0x0080),
    ("create_thread", 0x0002),
    ("vm_write", 0x0020 | 0x0008),
    ("dup_handle", 0x0040),
    ("set_information", 0x0200),
    ("suspend_resume", 0x0800),
    ("write_dac", 0x0004_0000),
    ("write_owner", 0x0008_0000),
];

/// `ok`, or `err<code>` with the error `OpenProcess` gave.
fn open_result(pid: u32, access: u32) -> String {
    // SAFETY: the handle is closed right away.
    unsafe {
        let handle = windows_sys::Win32::System::Threading::OpenProcess(access, 0, pid);
        if handle == 0 {
            format!("err{}", GetLastError())
        } else {
            CloseHandle(handle);
            "ok".to_string()
        }
    }
}

/// Whether `process`'s token opens for `TOKEN_ADJUST_DEFAULT` and takes its
/// own default DACL back: `ok`, or the failing step and error. Cygwin and
/// MSYS2 (Git Bash) change their default DACL at startup.
///
/// # Safety
/// `process` must be a process handle with query access.
unsafe fn adjust_default_dacl(process: HANDLE) -> String {
    // SAFETY: per the caller; the buffer outlives both calls and the token
    // handle is closed below.
    unsafe {
        let mut token: HANDLE = 0;
        if OpenProcessToken(process, TOKEN_QUERY | TOKEN_ADJUST_DEFAULT, &mut token) == 0 {
            return format!("open_err{}", GetLastError());
        }
        let mut needed = 0;
        GetTokenInformation(
            token,
            TokenDefaultDacl,
            std::ptr::null_mut(),
            0,
            &mut needed,
        );
        let mut buffer = vec![0usize; (needed as usize).div_ceil(std::mem::size_of::<usize>())];
        let result = if needed == 0
            || GetTokenInformation(
                token,
                TokenDefaultDacl,
                buffer.as_mut_ptr().cast(),
                needed,
                &mut needed,
            ) == 0
        {
            format!("query_err{}", GetLastError())
        } else if SetTokenInformation(token, TokenDefaultDacl, buffer.as_ptr().cast(), needed) == 0
        {
            format!("set_err{}", GetLastError())
        } else {
            "ok".to_string()
        };
        CloseHandle(token);
        result
    }
}

/// What a legacy command may do to process `pid`, as `name=result` pairs.
fn access_report(pid: u32) -> String {
    let mut parts: Vec<String> = WRITE_RIGHTS
        .iter()
        .map(|(name, access)| format!("{name}={}", open_result(pid, *access)))
        .collect();
    parts.push(format!(
        "query={}",
        open_result(pid, PROCESS_QUERY_LIMITED_INFORMATION)
    ));
    parts.push(format!("terminate={}", open_result(pid, PROCESS_TERMINATE)));
    // SAFETY: the handle is closed right away.
    let token = unsafe {
        let process = windows_sys::Win32::System::Threading::OpenProcess(
            PROCESS_QUERY_LIMITED_INFORMATION,
            0,
            pid,
        );
        if process == 0 {
            format!("query_err{}", GetLastError())
        } else {
            let result = adjust_default_dacl(process);
            CloseHandle(process);
            result
        }
    };
    parts.push(format!("token_adjust_default={token}"));
    parts.join(" ")
}

/// `ok` or the OS error writing `path`.
fn write_result(path: &Path) -> String {
    match fs::write(path, "d259") {
        Ok(()) => "ok".to_string(),
        Err(err) => format!("err{}", err.raw_os_error().unwrap_or(-1)),
    }
}

/// Tries to write `path` while impersonating process `pid`'s token (which a
/// write-restricted token opens for `TOKEN_DUPLICATE` through the user SID:
/// that right isn't a write right), and without it.
fn borrow_report(pid: u32, path: &Path) -> String {
    let own = write_result(path);
    // SAFETY: every handle is closed below; the thread reverts before returning.
    let borrowed = unsafe {
        let process = windows_sys::Win32::System::Threading::OpenProcess(
            PROCESS_QUERY_LIMITED_INFORMATION,
            0,
            pid,
        );
        if process == 0 {
            return format!("own_write={own} borrow=open_process_err{}", GetLastError());
        }
        let mut token: HANDLE = 0;
        let opened = OpenProcessToken(process, TOKEN_DUPLICATE | TOKEN_QUERY, &mut token) != 0;
        let error = GetLastError();
        CloseHandle(process);
        if !opened {
            return format!("own_write={own} borrow=open_token_err{error}");
        }
        let mut duplicate: HANDLE = 0;
        let duplicated = DuplicateTokenEx(
            token,
            TOKEN_IMPERSONATE | TOKEN_QUERY,
            std::ptr::null(),
            SecurityImpersonation,
            TokenImpersonation,
            &mut duplicate,
        ) != 0;
        let error = GetLastError();
        CloseHandle(token);
        if !duplicated {
            return format!("own_write={own} borrow=duplicate_err{error}");
        }
        let impersonating = SetThreadToken(std::ptr::null(), duplicate) != 0;
        let error = GetLastError();
        CloseHandle(duplicate);
        if !impersonating {
            return format!("own_write={own} borrow=impersonate_err{error}");
        }
        let write = write_result(path);
        // With the other command's identity, its processes are writable too.
        let create_process = open_result(pid, 0x0080);
        RevertToSelf();
        format!("impersonated write={write} create_process={create_process}")
    };
    format!("own_write={own} borrow={borrowed}")
}

/// Waits (up to a minute) in a legacy session for its release file, so a
/// second session can probe it. Does nothing otherwise.
#[test]
fn command_identity_hold_entry() {
    let Some(dir) = std::env::var_os(HOLD_ENV).map(PathBuf::from) else {
        return;
    };
    fs::write(dir.join("hold.pid"), std::process::id().to_string()).expect("write hold pid");
    wait_for_path(&dir.join("hold.release"), Duration::from_secs(/*secs*/ 60));
}

/// A child of the `self` probe: checks it may adjust its own token's default
/// DACL, then waits to be ended by its parent. Does nothing otherwise.
#[test]
fn command_identity_child_entry() {
    let Some(dir) = std::env::var_os(CHILD_ENV).map(PathBuf::from) else {
        return;
    };
    // SAFETY: the pseudo handle needs no closing.
    let adjust = unsafe { adjust_default_dacl(GetCurrentProcess()) };
    fs::write(
        dir.join("child.report"),
        format!("child_token_adjust_default={adjust}"),
    )
    .expect("write child report");
    std::thread::sleep(Duration::from_secs(/*secs*/ 60));
}

/// Inside a legacy session. `peer`: what this command may do to the process
/// in `CODEX_D259_TARGET`, another legacy command's. `borrow`: whether it
/// can write `CODEX_D259_FILE` with that process's token. `self`: what it
/// may do to its own child and token, and that it can end the child. Prints
/// one `d259: ` line. Does nothing otherwise.
#[test]
fn command_identity_probe_entry() {
    let Some(mode) = std::env::var_os(PROBE_ENV) else {
        return;
    };
    let target = || -> u32 {
        std::env::var(TARGET_ENV)
            .expect("target pid")
            .parse()
            .expect("numeric target pid")
    };
    let line = if mode == "peer" {
        access_report(target())
    } else if mode == "borrow" {
        let file = PathBuf::from(std::env::var_os(FILE_ENV).expect("target file"));
        borrow_report(target(), &file)
    } else {
        let dir = PathBuf::from(std::env::var_os(CHILD_ENV).expect("child dir"));
        let mut child = std::process::Command::new(std::env::current_exe().expect("test exe"))
            .args(["--exact", CHILD_ENTRY, "--nocapture", "--test-threads=1"])
            .spawn()
            .expect("start child");
        let report = dir.join("child.report");
        wait_for_path(&report, Duration::from_secs(/*secs*/ 30));
        let child_report = fs::read_to_string(&report).unwrap_or_default();
        let access = access_report(child.id());
        // SAFETY: the pseudo handle needs no closing.
        let own = unsafe { adjust_default_dacl(GetCurrentProcess()) };
        // SAFETY: the handle is closed right away.
        let ended = unsafe {
            let process = windows_sys::Win32::System::Threading::OpenProcess(
                PROCESS_TERMINATE,
                0,
                child.id(),
            );
            let ended = process != 0 && TerminateProcess(process, 1) != 0;
            if process != 0 {
                CloseHandle(process);
            }
            ended
        };
        if !ended {
            let _ = child.kill();
        }
        let _ = child.wait();
        format!("{access} own_token_adjust_default={own} {child_report} ended_child={ended}")
    };
    println!("\n{RESULT}{line}");
}

fn probe_argv(entry: &str) -> Vec<String> {
    vec![
        std::env::current_exe()
            .expect("test exe")
            .display()
            .to_string(),
        "--exact".to_string(),
        entry.to_string(),
        "--nocapture".to_string(),
        "--test-threads=1".to_string(),
    ]
}

/// Runs `argv` as a legacy workspace-write command on `root` with `env`
/// added to this process's environment; returns its stdout and exit code.
async fn run_legacy(
    root: &Path,
    codex_home: &Path,
    argv: Vec<String>,
    env: &[(&str, String)],
) -> (String, i32) {
    run_legacy_with_tty(root, codex_home, argv, env, /*tty*/ false).await
}

async fn run_legacy_with_tty(
    root: &Path,
    codex_home: &Path,
    argv: Vec<String>,
    env: &[(&str, String)],
    tty: bool,
) -> (String, i32) {
    let mut env_map: HashMap<String, String> = std::env::vars().collect();
    for (key, value) in env {
        env_map.insert((*key).to_string(), value.clone());
    }
    let spawned = spawn_windows_sandbox_session_legacy(
        &PermissionProfile::workspace_write(),
        workspace_roots_for(root).as_slice(),
        codex_home,
        argv,
        root,
        env_map,
        /*timeout_ms*/ Some(90_000),
        &[],
        &[],
        tty,
        /*stdin_open*/ false,
        /*use_private_desktop*/ true,
    )
    .await
    .expect("spawn legacy command");
    let (stdout, exit_code) =
        collect_stdout_and_exit(spawned, codex_home, Duration::from_secs(/*secs*/ 120)).await;
    (String::from_utf8_lossy(&stdout).into_owned(), exit_code)
}

fn probe_line(stdout: &str) -> Option<String> {
    stdout
        .lines()
        .find_map(|line| line.trim().strip_prefix(RESULT))
        .map(str::to_string)
}

/// Writes the holding session's release file when dropped.
struct Release(PathBuf);

impl Drop for Release {
    fn drop(&mut self) {
        let _ = fs::write(&self.0, "");
    }
}

/// Starts a legacy session that waits in [`command_identity_hold_entry`],
/// runs `probe` with its process id, then releases it. Returns the probe's
/// result.
async fn with_holder<T>(root: &Path, codex_home: &Path, probe: impl AsyncFnOnce(u32) -> T) -> T {
    let hold_dir = root.join("hold");
    fs::create_dir_all(&hold_dir).expect("create hold dir");
    let release = Release(hold_dir.join("hold.release"));
    let mut env_map: HashMap<String, String> = std::env::vars().collect();
    env_map.insert(HOLD_ENV.to_string(), hold_dir.display().to_string());
    let holder = spawn_windows_sandbox_session_legacy(
        &PermissionProfile::workspace_write(),
        workspace_roots_for(root).as_slice(),
        codex_home,
        probe_argv(HOLD_ENTRY),
        root,
        env_map,
        /*timeout_ms*/ Some(90_000),
        &[],
        &[],
        /*tty*/ false,
        /*stdin_open*/ false,
        /*use_private_desktop*/ true,
    )
    .await
    .expect("spawn holding session");
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
            "holding session never started\n{}",
            sandbox_log(codex_home)
        );
        tokio::time::sleep(Duration::from_millis(/*millis*/ 100)).await;
    };
    let result = probe(holder_pid).await;
    drop(release);
    let _ = collect_stdout_and_exit(holder, codex_home, Duration::from_secs(/*secs*/ 90)).await;
    result
}

/// D259: a legacy command can't open another legacy command's process for
/// any write right or adjust its token, even on the same workspace (so the
/// same capability SIDs). It can still query it.
#[test]
fn legacy_command_cannot_write_to_another_commands_process() {
    let _guard = legacy_process_test_guard();
    let root = user_owned_test_root();
    let runtime = current_thread_runtime();
    let codex_home = sandbox_home("d259-peer");
    let (stdout, exit_code) = runtime.block_on(with_holder(
        root.path(),
        codex_home.path(),
        async |holder_pid| {
            run_legacy(
                root.path(),
                codex_home.path(),
                probe_argv(PROBE_ENTRY),
                &[
                    (PROBE_ENV, "peer".to_string()),
                    (TARGET_ENV, holder_pid.to_string()),
                ],
            )
            .await
        },
    ));
    let line = probe_line(&stdout);
    println!("\nd259 peer: exit={exit_code} {line:?}");
    let mut expected: Vec<String> = WRITE_RIGHTS
        .iter()
        .map(|(name, _)| format!("{name}=err5"))
        .collect();
    expected.push("query=ok".to_string());
    let line = line.unwrap_or_else(|| {
        panic!(
            "no probe result: stdout={stdout:?}\n{}",
            sandbox_log(codex_home.path())
        )
    });
    let missing: Vec<&String> = expected
        .iter()
        .filter(|part| !line.split(' ').any(|have| have == part.as_str()))
        .collect();
    assert!(
        missing.is_empty() && line.contains("token_adjust_default=open_err5"),
        "another legacy command's process is open to writes: {line}\nmissing={missing:?}"
    );
}

/// A legacy command on another workspace can't write there, or open that
/// workspace's legacy command's process for `PROCESS_CREATE_PROCESS` (the
/// parent-process route D259 closes), by impersonating that command's token.
/// It can, before and after D259: `TOKEN_DUPLICATE` isn't a write right, so
/// the restricting SIDs don't decide it, and the borrowed token carries the
/// other command's identity (#1560).
#[test]
#[ignore = "#1560: impersonating another workspace's command token writes there (measured on CI before and after D259); closing it needs an owner decision"]
fn legacy_command_cannot_borrow_another_workspaces_command_token() {
    let _guard = legacy_process_test_guard();
    let holder_root = user_owned_test_root();
    let root = user_owned_test_root();
    let runtime = current_thread_runtime();
    let codex_home = sandbox_home("d259-borrow");
    let target = holder_root.path().join("borrowed.txt");
    let (stdout, exit_code) = runtime.block_on(with_holder(
        holder_root.path(),
        codex_home.path(),
        async |holder_pid| {
            run_legacy(
                root.path(),
                codex_home.path(),
                probe_argv(PROBE_ENTRY),
                &[
                    (PROBE_ENV, "borrow".to_string()),
                    (TARGET_ENV, holder_pid.to_string()),
                    (FILE_ENV, target.display().to_string()),
                ],
            )
            .await
        },
    ));
    let line = probe_line(&stdout);
    println!("\nd259 borrow: exit={exit_code} {line:?}");
    let line = line.unwrap_or_else(|| {
        panic!(
            "no probe result: stdout={stdout:?}\n{}",
            sandbox_log(codex_home.path())
        )
    });
    assert!(
        line.starts_with("own_write=err")
            && !target.exists()
            && !line.contains("create_process=ok"),
        "a legacy command wrote another workspace through its command's token: {line}"
    );
}

/// D259 keeps what a legacy command may do to itself: open its own child
/// for every write right, adjust its own and the child's token default DACL
/// (Cygwin/MSYS2 do), and end the child.
#[test]
fn legacy_command_keeps_write_access_to_its_own_processes() {
    let _guard = legacy_process_test_guard();
    let root = user_owned_test_root();
    let runtime = current_thread_runtime();
    let codex_home = sandbox_home("d259-self");
    let child_dir = root.path().join("child");
    fs::create_dir_all(&child_dir).expect("create child dir");
    let (stdout, exit_code) = runtime.block_on(run_legacy(
        root.path(),
        codex_home.path(),
        probe_argv(PROBE_ENTRY),
        &[
            (PROBE_ENV, "self".to_string()),
            (CHILD_ENV, child_dir.display().to_string()),
        ],
    ));
    let line = probe_line(&stdout);
    println!("\nd259 self: exit={exit_code} {line:?}");
    let line = line.unwrap_or_else(|| {
        panic!(
            "no probe result: stdout={stdout:?}\n{}",
            sandbox_log(codex_home.path())
        )
    });
    let denied = line
        .split(' ')
        .any(|part| part.contains('=') && !part.ends_with("=ok") && !part.ends_with("=true"));
    assert!(
        !denied && line.contains("child_token_adjust_default=ok"),
        "a legacy command lost access to its own processes: {line}"
    );
}

/// Git for Windows, if installed (it is on CI), under `ProgramFiles`.
fn git_for_windows(relative: &str) -> Option<PathBuf> {
    let path = PathBuf::from(std::env::var_os("ProgramFiles")?)
        .join("Git")
        .join(relative);
    if path.is_file() {
        return Some(path);
    }
    assert!(
        std::env::var_os("GITHUB_ACTIONS").is_none(),
        "{} is missing on CI",
        path.display()
    );
    println!("skipping: {} is not installed", path.display());
    None
}

/// D259 doesn't break common tools in a legacy command: a cmd pipeline,
/// Windows PowerShell calling a native program (with and without ConPTY),
/// and `git init`. (Git Bash
/// and `git commit` fail in a legacy session on the hosted runner before
/// D259 already, #1561.)
#[test]
fn legacy_command_tools_work_with_command_identity() {
    let _guard = legacy_process_test_guard();
    let Some(git) = git_for_windows("cmd\\git.exe") else {
        return;
    };
    let root = user_owned_test_root();
    let runtime = current_thread_runtime();
    let codex_home = sandbox_home("d259-tools");
    let home = codex_home.path();
    let powershell = system32_exe("WindowsPowerShell\\v1.0\\powershell.exe")
        .display()
        .to_string();
    // (name, argv, expected output, ConPTY)
    let cases: Vec<(&str, Vec<String>, &[&str], bool)> = vec![
        (
            "cmd pipeline",
            vec![
                system32_exe("cmd.exe").display().to_string(),
                "/d".to_string(),
                "/c".to_string(),
                "echo d259-pipe| findstr d259 2>&1".to_string(),
            ],
            &["d259-pipe"],
            false,
        ),
        (
            "powershell",
            vec![
                powershell.clone(),
                "-NoProfile".to_string(),
                "-Command".to_string(),
                "$x = & cmd /c echo d259-ps; $x | Select-String d259 | ForEach-Object { $_.Line }; \
                 Get-ChildItem | Out-Null; 'd259-ps-done'"
                    .to_string(),
            ],
            &["d259-ps", "d259-ps-done"],
            false,
        ),
        (
            "powershell (ConPTY)",
            vec![
                powershell,
                "-NoProfile".to_string(),
                "-Command".to_string(),
                "& cmd /c echo d259-tty; 'd259-tty-done'".to_string(),
            ],
            &["d259-tty", "d259-tty-done"],
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
    ];
    let mut failures = Vec::new();
    for (name, argv, expected, tty) in cases {
        let (stdout, exit_code) =
            runtime.block_on(run_legacy_with_tty(root.path(), home, argv, &[], tty));
        println!("\nd259 tool {name}: exit={exit_code} stdout={stdout:?}");
        if exit_code != 0 || !expected.iter().all(|part| stdout.contains(part)) {
            failures.push(format!("{name}: exit={exit_code} stdout={stdout:?}"));
        }
    }
    assert!(
        failures.is_empty() && root.path().join(".git").join("HEAD").is_file(),
        "{failures:#?}\n{}",
        sandbox_log(home)
    );
}
