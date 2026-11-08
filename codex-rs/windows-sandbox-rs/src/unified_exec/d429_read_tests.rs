//! D429 (#1803): what another command may read of a legacy (non-admin)
//! command. Until D429 a legacy command's default DACL gave Everyone, the
//! logon SID and the capability SIDs read and execute, so a command of either
//! backend could open it for `PROCESS_VM_READ` and its threads for
//! `THREAD_GET_CONTEXT` and read its environment, output and typed input
//! (win-1803l, win-1819). Now those SIDs get `SYNCHRONIZE` alone.
//!
//! A holding legacy command keeps a secret in its environment; a second
//! command, in another workspace, opens it and its threads and scans its
//! memory for the secret.

use super::*;
use windows_sys::Win32::System::Diagnostics::Debug::CONTEXT;
use windows_sys::Win32::System::Diagnostics::Debug::GetThreadContext;
use windows_sys::Win32::System::Diagnostics::Debug::ReadProcessMemory;
use windows_sys::Win32::System::Memory::MEM_COMMIT;
use windows_sys::Win32::System::Memory::MEMORY_BASIC_INFORMATION;
use windows_sys::Win32::System::Memory::PAGE_GUARD;
use windows_sys::Win32::System::Memory::PAGE_NOACCESS;
use windows_sys::Win32::System::Memory::VirtualQueryEx;
use windows_sys::Win32::System::Threading::OpenThread;

const READ_PROBE: &str =
    "unified_exec::tests::command_identity::elevated::d429::d429_read_probe_entry";
/// `pid=tid/tid/...` of the holding command, for the probe.
const READ_TARGET_ENV: &str = "CODEX_D429_TARGET";
/// The secret in the holding command's environment.
const SECRET_ENV: &str = "CODEX_D429_SECRET";
/// The secret the probe looks for (the probe's own environment doesn't hold
/// it under [`SECRET_ENV`]).
const NEEDLE_ENV: &str = "CODEX_D429_NEEDLE";
/// Upper bound on the bytes read from the holding command.
const SCAN_CAP: usize = 1 << 30;

/// Inside a sandboxed command: what it may read of the process and threads
/// in [`READ_TARGET_ENV`]. Does nothing otherwise.
#[test]
fn d429_read_probe_entry() {
    let Some(target) = std::env::var_os(READ_TARGET_ENV) else {
        return;
    };
    let target = target.to_string_lossy().into_owned();
    let (pid, tids) = target.split_once('=').expect("pid=tids");
    let pid: u32 = pid.parse().expect("numeric pid");
    let tids: Vec<u32> = tids.split('/').filter_map(|tid| tid.parse().ok()).collect();
    let needle = std::env::var(NEEDLE_ENV).expect("needle");
    let mut parts: Vec<String> = [
        ("synchronize", 0x0010_0000),
        ("query_limited", 0x1000),
        ("query_info", 0x0400),
        ("vm_read", 0x0010),
        ("terminate", 0x0001),
    ]
    .iter()
    .map(|(name, access)| format!("{name}={}", open_result(pid, *access)))
    .collect();
    parts.push(scan_for(pid, &needle));
    parts.push(threads_report(&tids));
    println!("\n{RESULT}{}", parts.join(" "));
}

/// `scan=open_err<code>`, or how often `pid`'s readable memory holds
/// `needle` as ASCII or UTF-16.
fn scan_for(pid: u32, needle: &str) -> String {
    let needles: [Vec<u8>; 2] = [
        needle.as_bytes().to_vec(),
        needle.encode_utf16().flat_map(u16::to_le_bytes).collect(),
    ];
    let overlap = needles.iter().map(Vec::len).max().unwrap_or(0);
    let (mut read, mut found) = (0usize, 0usize);
    // SAFETY: the handle is closed below; every buffer outlives its call.
    unsafe {
        let process = windows_sys::Win32::System::Threading::OpenProcess(0x0410, 0, pid);
        if process == 0 {
            return format!("scan=open_err{}", GetLastError());
        }
        let mut address = 0usize;
        let mut buffer = vec![0u8; 1 << 20];
        while read < SCAN_CAP {
            let mut info: MEMORY_BASIC_INFORMATION = std::mem::zeroed();
            if VirtualQueryEx(
                process,
                address as *const _,
                &mut info,
                std::mem::size_of::<MEMORY_BASIC_INFORMATION>(),
            ) == 0
            {
                break;
            }
            let (base, size) = (info.BaseAddress as usize, info.RegionSize);
            if info.State == MEM_COMMIT
                && info.Protect != 0
                && info.Protect & (PAGE_GUARD | PAGE_NOACCESS) == 0
            {
                let mut carry: Vec<u8> = Vec::new();
                let mut offset = 0usize;
                while offset < size {
                    let chunk = (size - offset).min(buffer.len());
                    let mut got = 0usize;
                    if ReadProcessMemory(
                        process,
                        (base + offset) as *const _,
                        buffer.as_mut_ptr().cast(),
                        chunk,
                        &mut got,
                    ) == 0
                        || got == 0
                    {
                        break;
                    }
                    read += got;
                    let kept = carry.len();
                    carry.extend_from_slice(&buffer[..got]);
                    found += needles
                        .iter()
                        .map(|needle| count_after(&carry, needle, kept))
                        .sum::<usize>();
                    let keep = carry.len().min(overlap.saturating_sub(1));
                    carry.drain(..carry.len() - keep);
                    offset += got;
                }
            }
            let next = base.saturating_add(size);
            if next <= address {
                break;
            }
            address = next;
        }
        CloseHandle(process);
    }
    format!("scan=ok bytes_read={read} secret_found={found}")
}

/// Occurrences of `needle` in `haystack` that end past `kept` (the tail
/// carried over from the previous chunk, already searched).
fn count_after(haystack: &[u8], needle: &[u8], kept: usize) -> usize {
    if needle.is_empty() || haystack.len() < needle.len() {
        return 0;
    }
    (0..=haystack.len() - needle.len())
        .filter(|&i| i + needle.len() > kept && &haystack[i..i + needle.len()] == needle)
        .count()
}

/// How many of `threads` open for `THREAD_GET_CONTEXT`, and how many
/// contexts `GetThreadContext` reads.
fn threads_report(threads: &[u32]) -> String {
    let (mut opened, mut read) = (0, 0);
    let mut context_error = String::new();
    for thread in threads {
        // SAFETY: the handle is closed right away; the context is aligned
        // by its type.
        unsafe {
            let handle = OpenThread(/*THREAD_GET_CONTEXT*/ 0x0008, 0, *thread);
            if handle == 0 {
                continue;
            }
            opened += 1;
            let mut context: CONTEXT = std::mem::zeroed();
            // CONTEXT_CONTROL | CONTEXT_INTEGER (AMD64).
            context.ContextFlags = 0x0010_0003;
            if GetThreadContext(handle, &mut context) != 0 {
                read += 1;
            } else if context_error.is_empty() {
                context_error = format!(" context_err={}", GetLastError());
            }
            CloseHandle(handle);
        }
    }
    format!(
        "threads={} thread_get_context={opened} context_read={read}{context_error}",
        threads.len()
    )
}

#[derive(Clone, Copy, Debug)]
enum Reader {
    Legacy,
    Elevated,
}

/// Starts a legacy command on its own workspace holding a fresh secret in its
/// environment, runs `reader`'s probe against it from another workspace, and
/// returns the probe's result line.
fn read_a_legacy_command(reader: Reader, reader_home: &Path) -> String {
    let legacy_home = sandbox_home("d429-holder");
    let holder_root = user_owned_test_root();
    let root = user_owned_test_root();
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("clock")
        .as_nanos();
    let secret = format!("D429SECRET{nanos:x}{:x}", std::process::id());
    let hold_dir = holder_root.path().join("hold");
    fs::create_dir_all(&hold_dir).expect("create hold dir");
    let runtime = current_thread_runtime();
    let (stdout, exit_code) = runtime.block_on(async {
        let release = Release(hold_dir.join("hold.release"));
        let mut env_map: HashMap<String, String> = std::env::vars().collect();
        env_map.insert(HOLD_ENV.to_string(), hold_dir.display().to_string());
        env_map.insert(SECRET_ENV.to_string(), secret.clone());
        let holder = spawn_windows_sandbox_session_legacy(
            &PermissionProfile::workspace_write(),
            workspace_roots_for(holder_root.path()).as_slice(),
            legacy_home.path(),
            probe_argv(HOLD_ENTRY),
            holder_root.path(),
            env_map,
            /*timeout_ms*/ Some(90_000),
            &[],
            &[],
            /*tty*/ false,
            /*stdin_open*/ false,
            /*use_private_desktop*/ true,
        )
        .await
        .expect("spawn holding legacy command");
        let pid_path = hold_dir.join("hold.pid");
        let deadline = Instant::now() + Duration::from_secs(/*secs*/ 60);
        let pid = loop {
            if let Ok(pid) = fs::read_to_string(&pid_path)
                && let Ok(pid) = pid.trim().parse::<u32>()
            {
                break pid;
            }
            assert!(
                Instant::now() < deadline,
                "holding command never started\n{}",
                sandbox_log(legacy_home.path())
            );
            tokio::time::sleep(Duration::from_millis(/*millis*/ 100)).await;
        };
        // A legacy token's thread snapshot is filtered: list them here.
        let tids = threads_of(&std::collections::HashSet::from([pid]))
            .into_iter()
            .map(|(tid, _)| tid.to_string())
            .collect::<Vec<_>>()
            .join("/");
        let env = [
            (READ_TARGET_ENV, format!("{pid}={tids}")),
            (NEEDLE_ENV, secret.clone()),
        ];
        let argv = probe_argv(READ_PROBE);
        let result = match reader {
            Reader::Legacy => run_legacy(root.path(), reader_home, argv, &env).await,
            Reader::Elevated => {
                run_elevated(root.path(), reader_home, argv, &env, /*tty*/ false).await
            }
        };
        drop(release);
        let _ =
            collect_stdout_and_exit(holder, legacy_home.path(), Duration::from_secs(/*secs*/ 90))
                .await;
        result
    });
    let line = expect_line(&stdout, reader_home);
    println!("\nd429 {reader:?} reads legacy: exit={exit_code} {line}");
    line
}

/// The parts of a probe line that show nothing could be read.
fn unreadable_parts(line: &str) -> Vec<String> {
    let mut missing: Vec<String> = [
        "synchronize=ok",
        "query_info=err5",
        "vm_read=err5",
        "terminate=err5",
        "scan=open_err5",
        "thread_get_context=0",
    ]
    .iter()
    .filter(|part| !line.split(' ').any(|have| have == **part))
    .map(|part| (*part).to_string())
    .collect();
    if line.contains("threads=0 ") || !line.contains("threads=") {
        missing.push("threads>0".to_string());
    }
    missing
}

/// D429: an elevated command (another account, holding Everyone and the
/// logon SID) can't read a concurrent legacy command's memory or thread
/// contexts. It may still wait on it.
#[test]
fn sec_win_d429_elevated_command_cannot_read_a_legacy_command() {
    let Some(elevated_home) = elevated_home("d429-elevated") else {
        return;
    };
    let _guard = legacy_process_test_guard();
    let line = read_a_legacy_command(Reader::Elevated, elevated_home.path());
    record_setup(elevated_home.path());
    let mut missing = unreadable_parts(&line);
    // D429 gives other accounts `SYNCHRONIZE` alone.
    if !line.split(' ').any(|part| part == "query_limited=err5") {
        missing.push("query_limited=err5".to_string());
    }
    assert!(
        missing.is_empty(),
        "an elevated command read a legacy command: {line}\nmissing={missing:?}"
    );
}

/// A legacy command on another workspace can't read a concurrent legacy
/// command's memory or thread contexts. It can, before and after D429: read
/// rights pass a write-restricted token on its normal SIDs, and the user SID,
/// which every legacy command holds, keeps full access (#1838).
#[test]
#[ignore = "#1838: a legacy command reads another legacy command's memory through the user SID (measured on CI before and after D429); closing it needs an owner decision"]
fn sec_win_d429_legacy_command_cannot_read_another_legacy_command() {
    let _guard = legacy_process_test_guard();
    let reader_home = sandbox_home("d429-legacy");
    let line = read_a_legacy_command(Reader::Legacy, reader_home.path());
    let missing = unreadable_parts(&line);
    assert!(
        missing.is_empty(),
        "a legacy command read another legacy command: {line}\nmissing={missing:?}"
    );
}

/// The first `name` on `PATH`, if any; on CI it must be there.
fn on_path(name: &str) -> Option<PathBuf> {
    let found = std::env::var_os("PATH").and_then(|path| {
        std::env::split_paths(&path)
            .map(|dir| dir.join(name))
            .find(|candidate| candidate.is_file())
    });
    if found.is_none() {
        assert!(
            std::env::var_os("GITHUB_ACTIONS").is_none(),
            "{name} is missing on CI"
        );
        println!("skipping: {name} is not on PATH");
    }
    found
}

/// D429 keeps common workloads working in a legacy command: a Python venv
/// whose interpreter starts a child, pip, a cargo build, Windows PowerShell
/// starting, listing, waiting on and ending its own children by id, and pwsh.
/// Then Core ends a running command and interrupts one in a ConPTY with
/// Ctrl-C. Reported only, as they fail on main too: `tasklist` ("Access
/// denied"), Git Bash and ensurepip (#1778); `venv` runs without pip and
/// cargo without rustup's proxy for that reason.
#[test]
fn sec_win_d429_legacy_workloads_still_work() {
    let _guard = legacy_process_test_guard();
    let pwsh = pwsh_path();
    if pwsh.is_none() {
        assert!(
            std::env::var_os("GITHUB_ACTIONS").is_none(),
            "pwsh is missing on CI"
        );
        println!("skipping: pwsh is not installed");
    }
    let (Some(python), Some(pwsh)) = (on_path("python.exe"), pwsh) else {
        return;
    };
    // Reported only; optional off CI.
    let bash = git_for_windows("bin\\bash.exe");
    // The toolchain's own cargo: rustup's proxy can't write its temp
    // folder from a sandboxed command (#1778).
    let Some(sysroot) = std::process::Command::new("rustc")
        .args(["--print", "sysroot"])
        .output()
        .ok()
        .filter(|output| output.status.success())
        .map(|output| PathBuf::from(String::from_utf8_lossy(&output.stdout).trim()))
    else {
        assert!(
            std::env::var_os("GITHUB_ACTIONS").is_none(),
            "rustc is missing on CI"
        );
        println!("skipping: no rustc");
        return;
    };
    let root = user_owned_test_root();
    let codex_home = sandbox_home("d429-workloads");
    let home = codex_home.path();
    let cmd = system32_exe("cmd.exe").display().to_string();
    let cmd_c = |line: String| vec![cmd.clone(), "/d".to_string(), "/c".to_string(), line];
    fs::write(
        root.path().join("d429.py"),
        "import subprocess, sys\n\
         out = subprocess.run([sys.executable, '-c', 'print(\"d429-py-child\")'],\n\
         \x20   capture_output=True, text=True, check=True).stdout\n\
         print(out.strip())\n",
    )
    .expect("write d429.py");
    let krate = root.path().join("d429crate");
    fs::create_dir_all(krate.join("src")).expect("crate dir");
    fs::write(
        krate.join("Cargo.toml"),
        "[package]\nname = \"d429crate\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\n[workspace]\n",
    )
    .expect("write Cargo.toml");
    fs::write(
        krate.join("src").join("main.rs"),
        "fn main() { println!(\"d429-cargo\"); }\n",
    )
    .expect("write main.rs");
    let powershell = system32_exe("WindowsPowerShell\\v1.0\\powershell.exe")
        .display()
        .to_string();
    let cargo_env = [
        (
            "CARGO_HOME",
            root.path().join("cargo-home").display().to_string(),
        ),
        (
            "CARGO_TARGET_DIR",
            krate.join("target").display().to_string(),
        ),
        (
            "RUSTC",
            sysroot.join("bin").join("rustc.exe").display().to_string(),
        ),
    ];
    // (name, argv, extra environment, expected output, required)
    let cases: Vec<(&str, Vec<String>, &[(&str, String)], &[&str], bool)> = vec![
        (
            "python venv and a child",
            cmd_c(format!(
                "({} -m venv --without-pip d429venv && d429venv\\Scripts\\python.exe d429.py) 2>&1",
                python.display()
            )),
            &[],
            &["d429-py-child"],
            true,
        ),
        (
            "pip",
            cmd_c(format!("{} -m pip --version 2>&1", python.display())),
            &[],
            &["pip "],
            true,
        ),
        (
            "cargo build and run",
            cmd_c(format!(
                "cd d429crate && {} run --offline --quiet 2>&1",
                sysroot.join("bin").join("cargo.exe").display()
            )),
            &cargo_env,
            &["d429-cargo"],
            true,
        ),
        (
            "powershell children by id",
            vec![
                powershell,
                "-NoProfile".to_string(),
                "-Command".to_string(),
                "$c = [Diagnostics.Process]::Start('cmd.exe', '/d /c exit 3'); $c.WaitForExit(); \
                 'd429-exit-' + $c.ExitCode; \
                 $p = [Diagnostics.Process]::Start('ping.exe', '-n 60 127.0.0.1'); \
                 Wait-Process -Id $p.Id -Timeout 1 -ErrorAction SilentlyContinue; \
                 (Get-Process -Id $p.Id).ProcessName; Stop-Process -Id $p.Id; \
                 $p.WaitForExit(); 'd429-stopped'"
                    .to_string(),
            ],
            &[],
            &["d429-exit-3", "PING", "d429-stopped"],
            true,
        ),
        (
            "pwsh",
            vec![
                pwsh.display().to_string(),
                "-NoProfile".to_string(),
                "-Command".to_string(),
                "& cmd /d /c echo d429-pwsh; (Get-Process -Id $PID).ProcessName; \
                 (Get-Process).Count -gt 0; 'd429-pwsh-done'"
                    .to_string(),
            ],
            &[],
            &["d429-pwsh", "pwsh", "True", "d429-pwsh-done"],
            true,
        ),
        (
            "tasklist",
            cmd_c("tasklist /v /nh 2>&1".to_string()),
            &[],
            &["tasklist.exe"],
            false,
        ),
        (
            "ensurepip",
            cmd_c("d429venv\\Scripts\\python.exe -m ensurepip 2>&1".to_string()),
            &[],
            &["Successfully installed pip"],
            false,
        ),
    ];
    let mut cases = cases;
    if let Some(bash) = bash {
        cases.push((
            "git bash",
            vec![
                bash.display().to_string(),
                "-c".to_string(),
                "echo d429-bash; ls > /dev/null; echo d429-bash-done".to_string(),
            ],
            &[],
            &["d429-bash-done"],
            false,
        ));
    }
    let runtime = current_thread_runtime();
    let mut failures = Vec::new();
    for (name, argv, env, expected, required) in cases {
        let (stdout, exit_code) = runtime.block_on(run_legacy(root.path(), home, argv, env));
        let lower = stdout.to_ascii_lowercase();
        let ok = exit_code == 0
            && expected
                .iter()
                .all(|part| lower.contains(&part.to_ascii_lowercase()));
        println!("\nd429 workload {name}: ok={ok} exit={exit_code} stdout={stdout:?}");
        if required && !ok {
            failures.push(format!("{name}: exit={exit_code} stdout={stdout:?}"));
        }
    }
    // In an elevated (admin) session Ctrl-C doesn't reach a legacy ConPTY
    // command on main either (#1842).
    let elevated = crate::setup::is_elevated().expect("elevation");
    for interrupt in [false, true] {
        let ended = runtime.block_on(end_a_running_command(root.path(), home, &cmd, interrupt));
        println!("\nd429 Core ends a command (ctrl_c={interrupt}): {ended}");
        if !ended.starts_with("ended") && !(interrupt && elevated) {
            failures.push(format!("ctrl_c={interrupt}: {ended}"));
        }
    }
    assert!(failures.is_empty(), "{failures:#?}\n{}", sandbox_log(home));
}

/// Starts `cmd` running `ping` in a legacy command and ends it as Core does:
/// with Ctrl-C typed into its ConPTY when `interrupt`, otherwise by
/// terminating the session. Reports whether it exited within 20 seconds.
async fn end_a_running_command(
    root: &Path,
    codex_home: &Path,
    cmd: &str,
    interrupt: bool,
) -> String {
    let spawned = spawn_windows_sandbox_session_legacy(
        &PermissionProfile::workspace_write(),
        workspace_roots_for(root).as_slice(),
        codex_home,
        vec![
            cmd.to_string(),
            "/d".to_string(),
            "/c".to_string(),
            "echo d429-running&& ping -n 120 127.0.0.1".to_string(),
        ],
        root,
        std::env::vars().collect(),
        /*timeout_ms*/ None,
        &[],
        &[],
        /*tty*/ interrupt,
        /*stdin_open*/ interrupt,
        /*use_private_desktop*/ true,
    )
    .await
    .expect("spawn legacy command to end");
    let codex_utils_pty::SpawnedProcess {
        session,
        mut stdout_rx,
        stderr_rx: _stderr_rx,
        exit_rx,
    } = spawned;
    let started = timeout(Duration::from_secs(/*secs*/ 30), async {
        let mut stdout = Vec::new();
        while let Some(chunk) = stdout_rx.recv().await {
            stdout.extend(chunk);
            // ping's first line: it runs, and has its console set up.
            if String::from_utf8_lossy(&stdout).contains("Pinging") {
                return true;
            }
        }
        false
    })
    .await
    .unwrap_or(false);
    if !started {
        session.terminate();
        return "never started".to_string();
    }
    let drain = tokio::spawn(async move { while stdout_rx.recv().await.is_some() {} });
    if !interrupt {
        session.request_terminate();
    }
    let writer = session.writer_sender();
    let typing = tokio::spawn(async move {
        // Ctrl-C, again every few seconds in case one arrives too early.
        for _ in 0..interrupt as usize * 5 {
            let _ = writer.send(vec![0x03]).await;
            tokio::time::sleep(Duration::from_secs(/*secs*/ 3)).await;
        }
    });
    let result = match timeout(Duration::from_secs(/*secs*/ 20), exit_rx).await {
        Ok(code) => format!("ended exit={code:?}"),
        Err(_) => "still running after 20s".to_string(),
    };
    typing.abort();
    session.terminate();
    drain.abort();
    result
}
