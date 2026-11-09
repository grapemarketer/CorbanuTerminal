#![cfg(target_os = "windows")]

use super::spawn_windows_sandbox_session_elevated_for_permission_profile;
use super::spawn_windows_sandbox_session_legacy;
use crate::WindowsSandboxCancellationToken;
use crate::ipc_framed::Message;
use crate::ipc_framed::decode_bytes;
use crate::ipc_framed::read_frame;
use crate::run_windows_sandbox_capture;
use base64::Engine;
use base64::engine::general_purpose::STANDARD as BASE64;
use codex_protocol::models::PermissionProfile;
use codex_utils_absolute_path::AbsolutePathBuf;
use codex_utils_pty::ProcessDriver;
use codex_utils_pty::ProcessSignal;
use pretty_assertions::assert_eq;
use std::collections::HashMap;
use std::fs;
use std::fs::OpenOptions;
use std::io::Seek;
use std::io::SeekFrom;
use std::os::windows::io::AsRawHandle;
use std::os::windows::io::FromRawHandle;
use std::os::windows::io::OwnedHandle;
use std::path::Path;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::MutexGuard;
use std::sync::atomic::AtomicU64;
use std::sync::atomic::Ordering;
use std::time::Duration;
use std::time::Instant;
use tempfile::TempDir;
use tokio::runtime::Builder;
use tokio::sync::broadcast;
use tokio::sync::mpsc;
use tokio::sync::oneshot;
use tokio::time::timeout;
use windows_sys::Win32::Foundation::WAIT_FAILED;
use windows_sys::Win32::Foundation::WAIT_OBJECT_0;
use windows_sys::Win32::Foundation::WAIT_TIMEOUT;
use windows_sys::Win32::System::Threading::OpenProcess;
use windows_sys::Win32::System::Threading::PROCESS_SYNCHRONIZE;
use windows_sys::Win32::System::Threading::WaitForSingleObject;

static TEST_HOME_COUNTER: AtomicU64 = AtomicU64::new(0);
static LEGACY_PROCESS_TEST_LOCK: Mutex<()> = Mutex::new(());

/// The lock guards nothing but the order of the tests, so a test that
/// failed holding it doesn't fail the others that share the process (the
/// D311 walk tests take it too since #1825).
pub(crate) fn legacy_process_test_guard() -> MutexGuard<'static, ()> {
    LEGACY_PROCESS_TEST_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// Whether a test in this process holds `legacy_process_test_guard()`. The
/// elevated setup checks it (#1827): it sets the sandbox users' passwords, so
/// two tests running it at once make one's logons fail (1326). Under nextest,
/// where each test has its own process, this is exact.
pub(crate) fn legacy_process_test_guard_is_held() -> bool {
    matches!(
        LEGACY_PROCESS_TEST_LOCK.try_lock(),
        Err(std::sync::TryLockError::WouldBlock)
    )
}

fn current_thread_runtime() -> tokio::runtime::Runtime {
    Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("build tokio runtime")
}

fn pwsh_path() -> Option<PathBuf> {
    let program_files = std::env::var_os("ProgramFiles")?;
    let path = PathBuf::from(program_files).join("PowerShell\\7\\pwsh.exe");
    path.is_file().then_some(path)
}

fn sandbox_cwd() -> PathBuf {
    if let Ok(workspace_root) = std::env::var("INSTA_WORKSPACE_ROOT") {
        return PathBuf::from(workspace_root);
    }

    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("repo root")
        .to_path_buf()
}

fn sandbox_home(name: &str) -> SandboxHome {
    let id = TEST_HOME_COUNTER.fetch_add(1, Ordering::Relaxed);
    let path = std::env::temp_dir().join(format!("codex-windows-sandbox-{name}-{id}"));
    let _ = fs::remove_dir_all(&path);
    fs::create_dir_all(&path).expect("create sandbox home");
    SandboxHome {
        home: tempfile::TempDir::new_in(&path).expect("create sandbox home tempdir"),
        // Both: a legacy session grants each.
        real_temps: ["TEMP", "TMP"]
            .into_iter()
            .filter_map(std::env::var_os)
            .map(PathBuf::from)
            .collect(),
        checkout: sandbox_cwd(),
    }
}

/// A test's Codex home (#1890). A legacy (non-admin) session grants its
/// writable roots, `TEMP`/`TMP` among them, to capability SIDs recorded in
/// the home, and the grants stay: one lasting entry per root for a user's
/// one home. Each test's home is new, so a test whose session used the real
/// temp folder left a new lasting entry there on every run, and everything
/// made there later inherited them all (#1870). So the tests give their
/// legacy sessions a temp folder of the home's own ([`session_temp_env`]),
/// and dropping the home checks that the real one got no entry for its SIDs.
/// The same goes for the checkout's `codex-rs` folder ([`sandbox_cwd`]) as a
/// workspace (#1906): tests use a folder of their own
/// ([`user_owned_test_root`]), and the drop checks the checkout too.
pub(crate) struct SandboxHome {
    home: TempDir,
    real_temps: Vec<PathBuf>,
    checkout: PathBuf,
}

impl std::ops::Deref for SandboxHome {
    type Target = TempDir;

    fn deref(&self) -> &TempDir {
        &self.home
    }
}

impl Drop for SandboxHome {
    fn drop(&mut self) {
        let granted: Vec<(&PathBuf, Vec<String>)> = self
            .real_temps
            .iter()
            .map(|temp| (temp, real_temp_grants(self.home.path(), temp)))
            .filter(|(_, sids)| !sids.is_empty())
            .collect();
        let checkout = real_temp_grants(self.home.path(), &self.checkout);
        let _ = fs::remove_dir_all(session_temp_dir(self.home.path()));
        if std::thread::panicking() {
            return;
        }
        if !granted.is_empty() {
            panic!(
                "a legacy session granted the real temp folder to this test's SIDs for good \
                 (#1890): {granted:?}; give it `session_temp_env(codex_home)`"
            );
        }
        if !checkout.is_empty() {
            panic!(
                "a legacy session granted the checkout {} to this test's SIDs for good \
                 (#1906): {checkout:?}; give it a workspace of its own (`user_owned_test_root()`)",
                self.checkout.display()
            );
        }
    }
}

/// The capability SIDs recorded in `codex_home` that may write `real_temp`.
fn real_temp_grants(codex_home: &Path, real_temp: &Path) -> Vec<String> {
    let Ok(text) = fs::read_to_string(crate::cap::cap_sid_file(codex_home)) else {
        return Vec::new();
    };
    let Ok(caps) = serde_json::from_str::<crate::cap::CapSids>(&text) else {
        return Vec::new();
    };
    [caps.workspace, caps.readonly]
        .into_iter()
        .chain(caps.workspace_by_cwd.into_values())
        .chain(caps.writable_root_by_path.into_values())
        .filter(|sid| {
            crate::token::LocalSid::from_string(sid).is_ok_and(|psid| {
                crate::acl::path_mask_allows(
                    real_temp,
                    &[psid.as_ptr()],
                    windows_sys::Win32::Storage::FileSystem::FILE_WRITE_DATA,
                    /*require_all_bits*/ true,
                )
                .unwrap_or(false)
            })
        })
        .collect()
}

/// The temp folder of the test home `codex_home` (from [`sandbox_home`]):
/// beside it, not in it, and removed with it.
fn session_temp_dir(codex_home: &Path) -> PathBuf {
    let mut name = codex_home.file_name().expect("home name").to_os_string();
    name.push("-temp");
    codex_home.with_file_name(name)
}

/// [`session_temp_dir`], made if missing.
fn session_temp(codex_home: &Path) -> PathBuf {
    let temp = session_temp_dir(codex_home);
    fs::create_dir_all(&temp).expect("create session temp folder");
    temp
}

/// `TEMP` and `TMP` for a legacy session of the test home `codex_home`: its
/// own temp folder, not the real one (#1890).
pub(crate) fn session_temp_env(codex_home: &Path) -> HashMap<String, String> {
    with_session_temp(codex_home, HashMap::new())
}

/// `env_map` with `TEMP` and `TMP` (in any spelling) replaced by the test
/// home's own temp folder (#1890).
pub(crate) fn with_session_temp(
    codex_home: &Path,
    env_map: HashMap<String, String>,
) -> HashMap<String, String> {
    with_temp(env_map, &session_temp(codex_home))
}

/// `env_map` with `TEMP` and `TMP` (in any spelling) replaced by `temp`.
pub(crate) fn with_temp(
    mut env_map: HashMap<String, String>,
    temp: &Path,
) -> HashMap<String, String> {
    let temp = temp.to_string_lossy().into_owned();
    env_map.retain(|key, _| !["TEMP", "TMP"].iter().any(|k| key.eq_ignore_ascii_case(k)));
    env_map.insert("TEMP".to_string(), temp.clone());
    env_map.insert("TMP".to_string(), temp);
    env_map
}

fn system32_exe(name: &str) -> PathBuf {
    let system_root = std::env::var_os("SystemRoot").unwrap_or_else(|| "C:\\Windows".into());
    PathBuf::from(system_root).join("System32").join(name)
}

fn current_user_sid() -> String {
    let output = std::process::Command::new(system32_exe("whoami.exe"))
        .arg("/user")
        .output()
        .expect("run whoami");
    let stdout = String::from_utf8_lossy(&output.stdout);
    stdout
        .split_whitespace()
        .find(|token| token.starts_with("S-1-"))
        .unwrap_or_else(|| panic!("unexpected whoami output: {stdout:?}"))
        .to_string()
}

/// Creates a test root with a protected DACL granting full control only to SYSTEM,
/// Administrators, and the current user, like a typical user-profile directory.
/// This keeps sandbox filesystem tests independent of the ACLs a runner puts on
/// the checkout (#158).
fn user_owned_test_root() -> TempDir {
    let root = TempDir::new_in(sandbox_cwd()).expect("create user-owned test root");
    let output = std::process::Command::new(system32_exe("icacls.exe"))
        .arg(root.path())
        .args([
            "/inheritance:r",
            "/grant:r",
            "*S-1-5-18:(OI)(CI)F",
            "*S-1-5-32-544:(OI)(CI)F",
        ])
        .arg(format!("*{}:(OI)(CI)F", current_user_sid()))
        .output()
        .expect("run icacls");
    assert!(
        output.status.success(),
        "icacls failed: {}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    // An explicit Medium label: a legacy session that had the checkout as a
    // writable root labelled it Low, which children would inherit (#158).
    let output = std::process::Command::new(system32_exe("icacls.exe"))
        .arg(root.path())
        .args(["/setintegritylevel", "(OI)(CI)M"])
        .output()
        .expect("run icacls /setintegritylevel");
    assert!(
        output.status.success(),
        "icacls /setintegritylevel failed: {}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    root
}

fn sandbox_log(codex_home: &Path) -> String {
    let log_path = crate::current_log_file_path(&codex_home.join(".sandbox"));
    fs::read_to_string(&log_path)
        .unwrap_or_else(|err| format!("failed to read {}: {err}", log_path.display()))
}

fn workspace_roots_for(root: &Path) -> Vec<AbsolutePathBuf> {
    vec![AbsolutePathBuf::from_absolute_path(root).expect("absolute workspace root")]
}

fn powershell_literal(path: &Path) -> String {
    path.to_string_lossy().replace('\'', "''")
}

fn start_powershell_child(
    pwsh: &Path,
    stdio_dir: &Path,
    child_command: &str,
    parent_tail: &str,
) -> String {
    let encoded = BASE64.encode(
        child_command
            .encode_utf16()
            .flat_map(u16::to_le_bytes)
            .collect::<Vec<_>>(),
    );
    format!(
        "Start-Process -WindowStyle Hidden -FilePath '{}' -ArgumentList '-NoProfile','-EncodedCommand','{encoded}' -RedirectStandardOutput '{}' -RedirectStandardError '{}'; {parent_tail}",
        powershell_literal(pwsh),
        powershell_literal(&stdio_dir.join("descendant.stdout")),
        powershell_literal(&stdio_dir.join("descendant.stderr")),
    )
}

fn wait_for_path(path: &Path, timeout: Duration) -> bool {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if path.exists() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(25));
    }
    path.exists()
}

fn open_process_for_wait(pid: u32) -> std::io::Result<OwnedHandle> {
    let handle = unsafe { OpenProcess(PROCESS_SYNCHRONIZE, 0, pid) };
    if handle == 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(unsafe { OwnedHandle::from_raw_handle(handle as _) })
}

fn wait_for_process_exit(process: &OwnedHandle, timeout: Duration) -> std::io::Result<()> {
    let timeout_ms = u32::try_from(timeout.as_millis())
        .map_err(|_| std::io::Error::other("process wait timeout exceeds u32"))?;
    let result = unsafe { WaitForSingleObject(process.as_raw_handle() as _, timeout_ms) };
    match result {
        WAIT_OBJECT_0 => Ok(()),
        WAIT_TIMEOUT => Err(std::io::Error::new(
            std::io::ErrorKind::TimedOut,
            "timed out waiting for process to exit",
        )),
        WAIT_FAILED => Err(std::io::Error::last_os_error()),
        result => Err(std::io::Error::other(format!(
            "unexpected process wait result: {result}"
        ))),
    }
}

fn wait_for_frame_count(frames_path: &Path, expected_frames: usize) -> Vec<Message> {
    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        let mut reader = OpenOptions::new()
            .read(true)
            .open(frames_path)
            .expect("open frame file for read");
        reader
            .seek(SeekFrom::Start(0))
            .expect("seek to start of frame file");

        let mut frames = Vec::new();
        loop {
            match read_frame(&mut reader) {
                Ok(Some(frame)) => frames.push(frame.message),
                Ok(None) => break,
                Err(_) => break,
            }
        }

        if frames.len() >= expected_frames {
            return frames;
        }
        assert!(
            Instant::now() < deadline,
            "timed out waiting for {expected_frames} frames, saw {}",
            frames.len()
        );
        std::thread::sleep(Duration::from_millis(10));
    }
}

async fn collect_stdout_and_exit(
    spawned: codex_utils_pty::SpawnedProcess,
    codex_home: &Path,
    timeout_duration: Duration,
) -> (Vec<u8>, i32) {
    let codex_utils_pty::SpawnedProcess {
        session: _session,
        mut stdout_rx,
        stderr_rx: _stderr_rx,
        exit_rx,
    } = spawned;
    let stdout_task = tokio::spawn(async move {
        let mut stdout = Vec::new();
        while let Some(chunk) = stdout_rx.recv().await {
            stdout.extend(chunk);
        }
        stdout
    });
    let exit_code = timeout(timeout_duration, exit_rx)
        .await
        .unwrap_or_else(|_| panic!("timed out waiting for exit\n{}", sandbox_log(codex_home)))
        .unwrap_or(-1);
    let stdout = timeout(timeout_duration, stdout_task)
        .await
        .unwrap_or_else(|_| {
            panic!(
                "timed out waiting for stdout task\n{}",
                sandbox_log(codex_home)
            )
        })
        .expect("stdout task join");
    (stdout, exit_code)
}

#[test]
fn legacy_non_tty_cmd_emits_output() {
    let _guard = legacy_process_test_guard();
    let runtime = current_thread_runtime();
    runtime.block_on(async move {
        // #1906: a folder of its own, not the checkout: a legacy session's
        // grant on its workspace lasts.
        let workspace = user_owned_test_root();
        let cwd = workspace.path().to_path_buf();
        let codex_home = sandbox_home("legacy-non-tty-cmd");
        println!("cmd codex_home={}", codex_home.path().display());
        let permission_profile = PermissionProfile::workspace_write();
        let spawned = spawn_windows_sandbox_session_legacy(
            &permission_profile,
            workspace_roots_for(cwd.as_path()).as_slice(),
            codex_home.path(),
            vec![
                "C:\\Windows\\System32\\cmd.exe".to_string(),
                "/c".to_string(),
                "echo LEGACY-NONTTY-CMD".to_string(),
            ],
            cwd.as_path(),
            session_temp_env(codex_home.path()),
            Some(5_000),
            &[],
            &[],
            /*tty*/ false,
            /*stdin_open*/ false,
            /*use_private_desktop*/ true,
        )
        .await
        .expect("spawn legacy non-tty cmd session");
        println!("cmd spawn returned");
        let (stdout, exit_code) =
            collect_stdout_and_exit(spawned, codex_home.path(), Duration::from_secs(10)).await;
        println!("cmd collect returned exit_code={exit_code}");
        let stdout = String::from_utf8_lossy(&stdout);
        assert_eq!(exit_code, 0, "stdout={stdout:?}");
        assert!(stdout.contains("LEGACY-NONTTY-CMD"), "stdout={stdout:?}");
    });
}

#[test]
fn elevated_non_tty_cmd_forwards_env_output_and_exit() {
    let _guard = legacy_process_test_guard();
    let runtime = current_thread_runtime();
    runtime.block_on(async move {
        // #1906: a folder of its own, not the checkout: a legacy session's
        // grant on its workspace lasts.
        let workspace = user_owned_test_root();
        let cwd = workspace.path().to_path_buf();
        let codex_home = sandbox_home("elevated-non-tty-cmd");
        let permission_profile = PermissionProfile::workspace_write();
        let env_map = HashMap::from([(
            "CODEX_ELEVATED_TEST".to_string(),
            "ELEVATED-ENV-OK".to_string(),
        )]);
        let spawned = spawn_windows_sandbox_session_elevated_for_permission_profile(
            &permission_profile,
            workspace_roots_for(cwd.as_path()).as_slice(),
            codex_home.path(),
            vec![
                "C:\\Windows\\System32\\cmd.exe".to_string(),
                "/d".to_string(),
                "/c".to_string(),
                "echo %CODEX_ELEVATED_TEST% & exit /b 23".to_string(),
            ],
            cwd.as_path(),
            env_map,
            /*proxy_enforced*/ false,
            /*network_proxy_restricting_sid*/ None,
            Some(5_000),
            /*read_roots_override*/ None,
            /*read_roots_include_platform_defaults*/ true,
            /*write_roots_override*/ None,
            &crate::DenyReadTargets::default(),
            &[],
            /*tty*/ false,
            /*stdin_open*/ false,
            /*use_private_desktop*/ true,
        )
        .await
        .expect("spawn elevated non-tty cmd session");
        let (stdout, exit_code) =
            collect_stdout_and_exit(spawned, codex_home.path(), Duration::from_secs(10)).await;
        let stdout = String::from_utf8_lossy(&stdout);
        assert_eq!(exit_code, 23, "stdout={stdout:?}");
        assert!(stdout.contains("ELEVATED-ENV-OK"), "stdout={stdout:?}");
    });
}

#[test]
fn legacy_non_tty_cmd_rejects_deny_read_overrides() {
    let _guard = legacy_process_test_guard();
    let runtime = current_thread_runtime();
    runtime.block_on(async move {
        // #1906: a folder of its own, not the checkout: a legacy session's
        // grant on its workspace lasts.
        let workspace = user_owned_test_root();
        let cwd = workspace.path().to_path_buf();
        let codex_home = sandbox_home("legacy-non-tty-deny-read");
        let secret_path =
            AbsolutePathBuf::from_absolute_path(cwd.join("legacy-non-tty-deny-read-secret.env"))
                .expect("absolute deny-read fixture path");
        let permission_profile = PermissionProfile::workspace_write();
        let err = spawn_windows_sandbox_session_legacy(
            &permission_profile,
            workspace_roots_for(cwd.as_path()).as_slice(),
            codex_home.path(),
            vec![
                "C:\\Windows\\System32\\cmd.exe".to_string(),
                "/c".to_string(),
                "echo deny-read".to_string(),
            ],
            cwd.as_path(),
            session_temp_env(codex_home.path()),
            Some(5_000),
            std::slice::from_ref(&secret_path),
            &[],
            /*tty*/ false,
            /*stdin_open*/ false,
            /*use_private_desktop*/ true,
        )
        .await
        .expect_err("legacy deny-read should require the elevated backend");
        assert!(
            err.to_string()
                .contains("deny-read overrides require the elevated Windows sandbox backend"),
            "unexpected error: {err:#}"
        );
    });
}

#[test]
fn legacy_non_tty_powershell_interrupt_terminates_process() {
    let Some(pwsh) = pwsh_path() else {
        return;
    };
    let _guard = legacy_process_test_guard();
    let runtime = current_thread_runtime();
    runtime.block_on(async move {
        // #1906: a folder of its own, not the checkout: a legacy session's
        // grant on its workspace lasts.
        let workspace = user_owned_test_root();
        let cwd = workspace.path().to_path_buf();
        let codex_home = sandbox_home("legacy-non-tty-pwsh");
        println!("pwsh codex_home={}", codex_home.path().display());
        let permission_profile = PermissionProfile::workspace_write();
        let spawned = spawn_windows_sandbox_session_legacy(
            &permission_profile,
            workspace_roots_for(cwd.as_path()).as_slice(),
            codex_home.path(),
            vec![
                pwsh.display().to_string(),
                "-NoProfile".to_string(),
                "-Command".to_string(),
                "Write-Output LEGACY-NONTTY-DIRECT; [System.Threading.ManualResetEvent]::new($false).WaitOne()".to_string(),
            ],
            cwd.as_path(),
            session_temp_env(codex_home.path()),
            /*timeout_ms*/ None,
            &[],
            &[],
            /*tty*/ false,
            /*stdin_open*/ false,
            /*use_private_desktop*/ true,
        )
        .await
        .expect("spawn legacy non-tty powershell session");
        println!("pwsh spawn returned");
        let codex_utils_pty::SpawnedProcess {
            session,
            mut stdout_rx,
            stderr_rx: _stderr_rx,
            exit_rx,
        } = spawned;
        timeout(Duration::from_secs(5), async {
            let mut stdout = Vec::new();
            while let Some(chunk) = stdout_rx.recv().await {
                stdout.extend(chunk);
                if String::from_utf8_lossy(&stdout).contains("LEGACY-NONTTY-DIRECT") {
                    return;
                }
            }
            panic!(
                "PowerShell exited before emitting its readiness marker\n{}",
                sandbox_log(codex_home.path())
            );
        })
        .await
        .unwrap_or_else(|_| {
            panic!(
                "timed out waiting for PowerShell readiness marker\n{}",
                sandbox_log(codex_home.path())
            )
        });

        session
            .signal(ProcessSignal::Interrupt)
            .expect("interrupt should terminate the restricted-token process job");
        let exit_code = timeout(Duration::from_secs(5), exit_rx)
            .await
            .unwrap_or_else(|_| panic!("timed out waiting for exit\n{}", sandbox_log(codex_home.path())))
            .expect("interrupted process should report an exit code");
        assert_eq!(exit_code, 1);
    });
}

#[test]
fn finish_driver_spawn_keeps_stdin_open_when_requested() {
    let runtime = current_thread_runtime();
    runtime.block_on(async move {
        let (writer_tx, mut writer_rx) = mpsc::channel::<Vec<u8>>(1);
        let (_stdout_tx, stdout_rx) = broadcast::channel::<Vec<u8>>(1);
        let (exit_tx, exit_rx) = oneshot::channel::<i32>();
        drop(exit_tx);

        let spawned = super::finish_driver_spawn(
            ProcessDriver {
                writer_tx,
                stdout_rx,
                stderr_rx: None,
                exit_rx,
                terminator: None,
                writer_handle: None,
                resizer: None,
                tty: false,
            },
            /*stdin_open*/ true,
        );

        spawned
            .session
            .writer_sender()
            .send(b"open".to_vec())
            .await
            .expect("stdin should stay open");
        assert_eq!(writer_rx.recv().await, Some(b"open".to_vec()));
    });
}

#[test]
fn finish_driver_spawn_closes_stdin_when_not_requested() {
    let runtime = current_thread_runtime();
    runtime.block_on(async move {
        let (writer_tx, _writer_rx) = mpsc::channel::<Vec<u8>>(1);
        let (_stdout_tx, stdout_rx) = broadcast::channel::<Vec<u8>>(1);
        let (exit_tx, exit_rx) = oneshot::channel::<i32>();
        drop(exit_tx);

        let spawned = super::finish_driver_spawn(
            ProcessDriver {
                writer_tx,
                stdout_rx,
                stderr_rx: None,
                exit_rx,
                terminator: None,
                writer_handle: None,
                resizer: None,
                tty: false,
            },
            /*stdin_open*/ false,
        );

        assert!(
            spawned
                .session
                .writer_sender()
                .send(b"closed".to_vec())
                .await
                .is_err(),
            "stdin should be closed when streaming input is disabled"
        );
    });
}

#[test]
fn runner_stdin_writer_sends_close_stdin_after_input_eof() {
    let runtime = current_thread_runtime();
    runtime.block_on(async move {
        let tempdir = TempDir::new().expect("create tempdir");
        let frames_path = tempdir.path().join("runner-stdin-frames.bin");
        let file = OpenOptions::new()
            .create(true)
            .truncate(true)
            .read(true)
            .write(true)
            .open(&frames_path)
            .expect("create frame file");
        let outbound_tx = super::start_runner_pipe_writer(file);
        let (writer_tx, writer_rx) = mpsc::channel::<Vec<u8>>(1);
        let writer_handle = super::start_runner_stdin_writer(
            writer_rx,
            outbound_tx,
            /*normalize_newlines*/ false,
            /*stdin_open*/ true,
        );

        writer_tx
            .send(b"hello".to_vec())
            .await
            .expect("send stdin bytes");
        drop(writer_tx);
        writer_handle.await.expect("join stdin writer");

        let frames = wait_for_frame_count(&frames_path, 2);

        match &frames[0] {
            Message::Stdin { payload } => {
                let bytes = decode_bytes(&payload.data_b64).expect("decode stdin payload");
                assert_eq!(bytes, b"hello".to_vec());
            }
            other => panic!("expected stdin frame, got {other:?}"),
        }

        match &frames[1] {
            Message::CloseStdin { .. } => {}
            other => panic!("expected close-stdin frame, got {other:?}"),
        }
    });
}

#[test]
fn runner_resizer_sends_resize_frame() {
    let runtime = current_thread_runtime();
    runtime.block_on(async move {
        let tempdir = TempDir::new().expect("create tempdir");
        let frames_path = tempdir.path().join("runner-resize-frames.bin");
        let file = OpenOptions::new()
            .create(true)
            .truncate(true)
            .read(true)
            .write(true)
            .open(&frames_path)
            .expect("create frame file");
        let outbound_tx = super::start_runner_pipe_writer(file);
        let mut resizer = super::make_runner_resizer(outbound_tx);

        resizer(codex_utils_pty::TerminalSize {
            rows: 45,
            cols: 132,
        })
        .expect("send resize frame");

        let frames = wait_for_frame_count(&frames_path, 1);
        match &frames[0] {
            Message::Resize { payload } => {
                assert_eq!(payload.rows, 45);
                assert_eq!(payload.cols, 132);
            }
            other => panic!("expected resize frame, got {other:?}"),
        }
    });
}

/// #1826, #1861: a legacy capture or session returns once the command exits
/// while a descendant it left running holds the output pipes, and leaves that
/// descendant running. `run` runs the argv it is given in the legacy sandbox
/// with the cwd and codex home it is given, returning the command's stdout
/// and exit code.
fn check_legacy_command_returns_while_descendant_runs(
    label: &str,
    run: impl FnOnce(&Path, &Path, Vec<String>) -> (String, i32),
) {
    let Some(pwsh) = pwsh_path() else {
        return;
    };
    let _guard = legacy_process_test_guard();
    // #1906: a folder of its own, not the checkout: a legacy session's
    // grant on its workspace lasts.
    let workspace = user_owned_test_root();
    let cwd = workspace.path().to_path_buf();
    let codex_home = sandbox_home(&format!("legacy-{}-pwsh", label.replace(' ', "-")));
    // What the sandboxed commands write goes in the folder they may write
    // (#1890): the home is not one.
    let scratch = session_temp(codex_home.path());
    println!("{label} pwsh codex_home={}", codex_home.path().display());
    let ready_marker = scratch.join("descendant-started");
    let release_marker = scratch.join("release-descendant");
    let survival_marker = scratch.join("descendant-survived");
    let descendant_command = format!(
        "$deadline=(Get-Date).AddSeconds(30); Set-Content -LiteralPath '{}' -Value $PID; while (-not (Test-Path -LiteralPath '{}')) {{ if ((Get-Date) -ge $deadline) {{ exit 3 }}; Start-Sleep -Milliseconds 25 }}; Set-Content -LiteralPath '{}' -Value survived",
        powershell_literal(&ready_marker),
        powershell_literal(&release_marker),
        powershell_literal(&survival_marker),
    );
    let parent_tail = format!(
        "while (-not (Test-Path -LiteralPath '{}')) {{ Start-Sleep -Milliseconds 25 }}",
        powershell_literal(&ready_marker),
    );
    let parent_command = format!(
        "Write-Output LEGACY-CAPTURE-DIRECT; {}",
        start_powershell_child(&pwsh, &scratch, &descendant_command, &parent_tail),
    );
    let argv = vec![
        pwsh.display().to_string(),
        "-NoProfile".to_string(),
        "-Command".to_string(),
        parent_command,
    ];
    let started = Instant::now();
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        run(cwd.as_path(), codex_home.path(), argv)
    }));
    let took = started.elapsed();
    // Opened before the release so its exit can be awaited; `None` if it has
    // already exited (it gives up after its own deadline).
    let descendant_process = fs::read_to_string(&ready_marker)
        .ok()
        .and_then(|pid| pid.trim().parse().ok())
        .and_then(|pid| open_process_for_wait(pid).ok());
    // Released before any check, so the descendant never outlives the test
    // by more than its own deadline.
    let release = fs::write(&release_marker, "release");
    let (stdout, exit_code) = match result {
        Ok(output) => output,
        Err(panic) => std::panic::resume_unwind(panic),
    };
    release.expect("release descendant after root exit");
    println!("{label} pwsh returned after {took:?}");
    println!("{label} pwsh exit_code={exit_code}");
    assert_eq!(exit_code, 0, "stdout={stdout:?}");
    assert!(
        stdout.contains("LEGACY-CAPTURE-DIRECT"),
        "stdout={stdout:?}"
    );
    assert!(
        wait_for_path(&survival_marker, Duration::from_secs(10)),
        "the {label} descendant did not survive the command's exit; \
         the command returned after {took:?}"
    );
    let descendant_process = descendant_process.expect("open descendant after normal legacy exit");
    wait_for_process_exit(&descendant_process, Duration::from_secs(10))
        .expect("sandbox descendant did not exit after release");
    // #1826: the command now returns while the descendant runs, so the
    // session's labels (#158) are put back on a thread of the backend's once
    // it exits. Wait for that before the test (and maybe its process) ends.
    for root in [session_temp_dir(codex_home.path()), cwd] {
        let deadline = Instant::now() + Duration::from_secs(15);
        // SAFETY: an existing folder.
        while !matches!(
            unsafe { crate::integrity::effective_label_is_low(&root) },
            Ok(false)
        ) {
            assert!(
                Instant::now() < deadline,
                "{} still labelled Low after the descendant exited",
                root.display()
            );
            std::thread::sleep(Duration::from_millis(50));
        }
    }
    // The release goes on (the recorded denies) after the labels.
    crate::integrity_leases::wait_for_release_in_progress().expect("wait for the release");
}

#[test]
fn legacy_capture_emits_output_and_preserves_descendant_after_normal_exit() {
    check_legacy_command_returns_while_descendant_runs("capture", |cwd, codex_home, argv| {
        let permission_profile = PermissionProfile::workspace_write();
        let result = run_windows_sandbox_capture(
            &permission_profile,
            workspace_roots_for(cwd).as_slice(),
            codex_home,
            argv,
            cwd,
            session_temp_env(codex_home),
            Some(10_000),
            /*cancellation*/ None,
            /*use_private_desktop*/ true,
        )
        .expect("run legacy capture powershell");
        println!("capture pwsh timed_out={}", result.timed_out);
        println!(
            "capture pwsh stderr={:?}",
            String::from_utf8_lossy(&result.stderr)
        );
        (
            String::from_utf8_lossy(&result.stdout).into_owned(),
            result.exit_code,
        )
    });
}

/// #1861: the same for a legacy unified exec session (pipes, not a ConPTY),
/// whose exit is reported once its output readers finish.
#[test]
fn legacy_session_exits_and_preserves_descendant_after_normal_exit() {
    check_legacy_command_returns_while_descendant_runs(
        "legacy session",
        |cwd, codex_home, argv| {
            current_thread_runtime().block_on(async {
                let permission_profile = PermissionProfile::workspace_write();
                let spawned = spawn_windows_sandbox_session_legacy(
                    &permission_profile,
                    workspace_roots_for(cwd).as_slice(),
                    codex_home,
                    argv,
                    cwd,
                    session_temp_env(codex_home),
                    Some(10_000),
                    &[],
                    &[],
                    /*tty*/ false,
                    /*stdin_open*/ false,
                    /*use_private_desktop*/ true,
                )
                .await
                .expect("spawn legacy session powershell");
                // Longer than the descendant's own deadline, so the old wait
                // shows as a late return rather than a timeout.
                let (stdout, exit_code) =
                    collect_stdout_and_exit(spawned, codex_home, Duration::from_secs(60)).await;
                (String::from_utf8_lossy(&stdout).into_owned(), exit_code)
            })
        },
    );
}

/// #1830: the elevated runner, like the legacy capture (#1826), returns once
/// the command exits while a descendant it left running holds the output
/// pipes, and leaves that descendant running. `run` runs the argv it is given
/// in the elevated sandbox on the root it is given, returning the command's
/// stdout and exit code.
fn check_elevated_command_returns_while_descendant_runs(
    backend: &str,
    run: impl FnOnce(&Path, &Path, Vec<String>) -> (String, i32),
) {
    let Some(pwsh) = pwsh_path() else {
        return;
    };
    let Some(codex_home) =
        command_identity::elevated::elevated_home(&format!("elevated-{backend}-pwsh"))
    else {
        return;
    };
    let _guard = legacy_process_test_guard();
    // The sandbox account may write here, not in its codex home.
    let root = user_owned_test_root();
    let ready_marker = root.path().join("descendant-started");
    let release_marker = root.path().join("release-descendant");
    let survival_marker = root.path().join("descendant-survived");
    let descendant_command = format!(
        "$deadline=(Get-Date).AddSeconds(30); Set-Content -LiteralPath '{}' -Value $PID; while (-not (Test-Path -LiteralPath '{}')) {{ if ((Get-Date) -ge $deadline) {{ exit 3 }}; Start-Sleep -Milliseconds 25 }}; Set-Content -LiteralPath '{}' -Value survived",
        powershell_literal(&ready_marker),
        powershell_literal(&release_marker),
        powershell_literal(&survival_marker),
    );
    let parent_tail = format!(
        "while (-not (Test-Path -LiteralPath '{}')) {{ Start-Sleep -Milliseconds 25 }}",
        powershell_literal(&ready_marker),
    );
    let parent_command = format!(
        "Write-Output ELEVATED-CAPTURE-DIRECT; {}",
        start_powershell_child(&pwsh, root.path(), &descendant_command, &parent_tail),
    );
    let argv = vec![
        pwsh.display().to_string(),
        "-NoProfile".to_string(),
        "-Command".to_string(),
        parent_command,
    ];
    let started = Instant::now();
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        run(root.path(), codex_home.path(), argv)
    }));
    let took = started.elapsed();
    // Released before any check, so the descendant never outlives the test
    // by more than its own deadline.
    let release = fs::write(&release_marker, "release");
    let (stdout, exit_code) = match result {
        Ok(output) => output,
        Err(panic) => std::panic::resume_unwind(panic),
    };
    release.expect("release descendant after root exit");
    println!("elevated {backend} pwsh returned after {took:?}");
    println!("elevated {backend} pwsh exit_code={exit_code}");
    let survived = wait_for_path(&survival_marker, Duration::from_secs(10));
    // The root is in the repository: remove it once the descendant has let
    // go of its files (its redirected output), which it may hold a moment
    // after writing the marker.
    let deadline = Instant::now() + Duration::from_secs(15);
    while let Err(err) = fs::remove_dir_all(root.path()) {
        assert!(
            Instant::now() < deadline,
            "remove {}: {err}",
            root.path().display()
        );
        std::thread::sleep(Duration::from_millis(100));
    }
    assert_eq!(
        exit_code,
        0,
        "stdout={stdout:?}\n{}",
        sandbox_log(codex_home.path())
    );
    assert!(
        stdout.contains("ELEVATED-CAPTURE-DIRECT"),
        "stdout={stdout:?}"
    );
    assert!(
        survived,
        "#1830: the elevated {backend} descendant did not survive the command's exit; \
         the command returned after {took:?}"
    );
}

#[test]
fn elevated_capture_emits_output_and_preserves_descendant_after_normal_exit() {
    check_elevated_command_returns_while_descendant_runs("capture", |root, codex_home, argv| {
        let permission_profile = PermissionProfile::workspace_write();
        let workspace_roots = workspace_roots_for(root);
        let result = crate::run_windows_sandbox_capture_for_permission_profile_elevated(
            crate::ElevatedSandboxProfileCaptureRequest {
                permission_profile: &permission_profile,
                workspace_roots: workspace_roots.as_slice(),
                codex_home,
                command: argv,
                cwd: root,
                // Core passes its environment on; PowerShell needs it.
                env_map: std::env::vars().collect(),
                timeout_ms: Some(10_000),
                cancellation: None,
                use_private_desktop: true,
                proxy_enforced: false,
                network_proxy_restricting_sid: None,
                read_roots_override: None,
                read_roots_include_platform_defaults: true,
                write_roots_override: None,
                deny_read_override: &crate::DenyReadTargets::default(),
                deny_write_paths_override: &[],
            },
        )
        .unwrap_or_else(|err| {
            panic!(
                "run elevated capture powershell: {err:#}\n{}",
                sandbox_log(codex_home)
            )
        });
        println!("elevated capture pwsh timed_out={}", result.timed_out);
        (
            String::from_utf8_lossy(&result.stdout).into_owned(),
            result.exit_code,
        )
    });
}

/// The same for an elevated unified exec session (pipes, not a ConPTY), which
/// reports its exit when the runner sends it.
#[test]
fn elevated_session_exits_and_preserves_descendant_after_normal_exit() {
    check_elevated_command_returns_while_descendant_runs("session", |root, codex_home, argv| {
        current_thread_runtime().block_on(command_identity::elevated::run_elevated(
            root,
            codex_home,
            argv,
            &[],
            /*tty*/ false,
        ))
    });
}

/// Observed state after a legacy workspace-write session tries to delete one
/// fixture inside each writable root (workspace, TEMP, TMP), one outside every
/// writable root, and the protected `.git` directory.
#[derive(Debug, PartialEq, Eq)]
struct LegacyDeleteOutcome {
    exit_code: i32,
    workspace_file_exists: bool,
    temp_file_exists: bool,
    tmp_file_exists: bool,
    outside_file_contents: Option<String>,
    outside_move_target_exists: bool,
    protected_git_dir_exists: bool,
    nested_existing_contents: Option<String>,
    outside_dir_exists: bool,
    /// What any other Low-integrity program of the user may write after the
    /// session: nothing of the workspace, since its label is put back when
    /// the session ends (#158), including what the session made there.
    low_can_write_git_dir: bool,
    low_can_write_codex_file: bool,
    low_can_write_workspace_file: bool,
    low_can_write_made_file: bool,
    /// The session made `made\new.txt` (workspace-write only).
    made_file_exists: bool,
    /// A file outside the roots, reached through a junction in the
    /// workspace: labelling the root does not follow the junction.
    linked_file_exists: bool,
    /// The workspace's own label is back and no longer recorded.
    workspace_label_restored: bool,
    /// A Low-labelled folder outside the roots that a planted `.git`
    /// junction in a second workspace root points at keeps its label: the
    /// host relabels only inside writable roots, never through a link. (A
    /// top-level `.codex`/`.agents` junction stops the command, #1682.)
    low_can_write_junction_target: bool,
}

/// Whether a plain Low-integrity copy of this process's token (not the
/// sandbox token) can open `path` for writing.
fn low_integrity_can_open_for_write(path: &Path) -> bool {
    use windows_sys::Win32::Storage::FileSystem::FILE_SHARE_DELETE;
    use windows_sys::Win32::Storage::FileSystem::FILE_SHARE_READ;
    use windows_sys::Win32::Storage::FileSystem::FILE_SHARE_WRITE;
    use windows_sys::Win32::Storage::FileSystem::FILE_WRITE_DATA;
    low_integrity_can_open(
        path,
        FILE_WRITE_DATA,
        FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
    )
}

/// Whether a plain Low-integrity copy of this process's token can open
/// `path` (a file or folder) for `access`, sharing `share`.
fn low_integrity_can_open(path: &Path, access: u32, share: u32) -> bool {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Foundation::CloseHandle;
    use windows_sys::Win32::Foundation::HANDLE;
    use windows_sys::Win32::Foundation::INVALID_HANDLE_VALUE;
    use windows_sys::Win32::Security::DuplicateTokenEx;
    use windows_sys::Win32::Security::RevertToSelf;
    use windows_sys::Win32::Security::SecurityImpersonation;
    use windows_sys::Win32::Security::TOKEN_ALL_ACCESS;
    use windows_sys::Win32::Security::TOKEN_DUPLICATE;
    use windows_sys::Win32::Security::TOKEN_QUERY;
    use windows_sys::Win32::Security::TokenImpersonation;
    use windows_sys::Win32::Storage::FileSystem::CreateFileW;
    use windows_sys::Win32::Storage::FileSystem::FILE_FLAG_BACKUP_SEMANTICS;
    use windows_sys::Win32::Storage::FileSystem::OPEN_EXISTING;
    use windows_sys::Win32::System::Threading::GetCurrentProcess;
    use windows_sys::Win32::System::Threading::OpenProcessToken;
    use windows_sys::Win32::System::Threading::SetThreadToken;

    // SAFETY: token handles are opened and closed here; the impersonation is
    // reverted before returning.
    unsafe {
        let mut process_token: HANDLE = 0;
        assert_ne!(
            OpenProcessToken(
                GetCurrentProcess(),
                TOKEN_DUPLICATE | TOKEN_QUERY,
                &mut process_token
            ),
            0,
            "open the process token"
        );
        let mut low: HANDLE = 0;
        let duplicated = DuplicateTokenEx(
            process_token,
            TOKEN_ALL_ACCESS,
            std::ptr::null(),
            SecurityImpersonation,
            TokenImpersonation,
            &mut low,
        );
        CloseHandle(process_token);
        assert_ne!(duplicated, 0, "duplicate the process token");
        crate::integrity::set_token_low_integrity(low).expect("lower the token");
        assert_ne!(SetThreadToken(std::ptr::null(), low), 0, "impersonate");
        let wide: Vec<u16> = path
            .as_os_str()
            .encode_wide()
            .chain(std::iter::once(0))
            .collect();
        let handle = CreateFileW(
            wide.as_ptr(),
            access,
            share,
            std::ptr::null(),
            OPEN_EXISTING,
            FILE_FLAG_BACKUP_SEMANTICS,
            0,
        );
        RevertToSelf();
        CloseHandle(low);
        if handle == INVALID_HANDLE_VALUE {
            return false;
        }
        CloseHandle(handle);
        true
    }
}

/// Runs the legacy delete fixture and returns its outcome plus diagnostics.
fn run_legacy_delete_fixture(
    permission_profile: PermissionProfile,
) -> (LegacyDeleteOutcome, String) {
    let _guard = legacy_process_test_guard();
    let runtime = current_thread_runtime();
    runtime.block_on(async move {
        // Keep writable roots out of USERPROFILE exclusions such as AppData, and
        // give the root a user-owned DACL so the result does not depend on the
        // ACLs the runner put on the checkout.
        let test_root = user_owned_test_root();
        let codex_home = sandbox_home("legacy-delete-writable-roots");
        let workspace = test_root.path().join("workspace");
        let temp_root = test_root.path().join("temp");
        let tmp_root = test_root.path().join("tmp");
        let outside_root = test_root.path().join("outside");
        let second_root = test_root.path().join("second");
        for directory in [
            &workspace,
            &temp_root,
            &tmp_root,
            &outside_root,
            &second_root,
        ] {
            fs::create_dir_all(directory).expect("create legacy delete test directory");
        }
        let protected_git_dir = workspace.join(".git");
        fs::create_dir(&protected_git_dir).expect("create protected .git directory");
        let codex_file = workspace.join(".codex").join("config.toml");
        fs::create_dir(workspace.join(".codex")).expect("create protected .codex directory");
        fs::write(&codex_file, "# before").expect("seed protected .codex file");
        let junction_target = test_root.path().join("elsewhere-low");
        fs::create_dir(&junction_target).expect("create junction target");
        let junction_target_file = junction_target.join("file.txt");
        fs::write(&junction_target_file, "low").expect("seed junction target file");
        for args in [
            vec![
                junction_target.display().to_string(),
                "/setintegritylevel".to_string(),
                "(OI)(CI)L".to_string(),
            ],
            vec![
                junction_target_file.display().to_string(),
                "/setintegritylevel".to_string(),
                "L".to_string(),
            ],
        ] {
            let output = std::process::Command::new(system32_exe("icacls.exe"))
                .args(&args)
                .output()
                .expect("run icacls");
            assert!(output.status.success(), "icacls {args:?} failed");
        }
        let output = std::process::Command::new(system32_exe("cmd.exe"))
            .args(["/d", "/c", "mklink", "/J"])
            .arg(second_root.join(".git"))
            .arg(&junction_target)
            .output()
            .expect("run mklink");
        assert!(output.status.success(), "mklink /J failed");

        let workspace_file = workspace.join("workspace-delete.txt");
        let temp_file = temp_root.join("temp-delete.txt");
        let tmp_file = tmp_root.join("tmp-delete.txt");
        let outside_file = outside_root.join("outside-delete.txt");
        // A file that predates the session, below a subdirectory: Low-integrity
        // writes there need the inherited label (#158).
        let nested_existing = workspace.join("sub").join("existing.txt");
        fs::create_dir(workspace.join("sub")).expect("create workspace subdirectory");
        fs::write(&nested_existing, "before").expect("seed nested workspace file");
        let outside_dir = outside_root.join("outside-dir");
        fs::create_dir(&outside_dir).expect("create outside directory");
        let outside_move_target = workspace.join("moved-in.txt");
        let made_dir = workspace.join("made");
        let made_file = made_dir.join("new.txt");
        let linked_dir = outside_root.join("linked");
        fs::create_dir(&linked_dir).expect("create linked directory");
        fs::write(linked_dir.join("linked.txt"), "linked").expect("seed linked file");
        let output = std::process::Command::new(system32_exe("cmd.exe"))
            .args(["/d", "/c", "mklink", "/J"])
            .arg(workspace.join("link-out"))
            .arg(&linked_dir)
            .output()
            .expect("run mklink");
        assert!(output.status.success(), "mklink /J link-out failed");
        let linked_delete = workspace.join("link-out").join("linked.txt");
        // SAFETY: reads the label of a folder this test made.
        let workspace_label_before =
            unsafe { crate::integrity::read_explicit_labels(&workspace) }.expect("read label");
        fs::write(&workspace_file, "workspace").expect("seed workspace file");
        fs::write(&temp_file, "temp").expect("seed TEMP file");
        fs::write(&tmp_file, "tmp").expect("seed TMP file");
        fs::write(&outside_file, "outside").expect("seed outside file");

        let script = workspace.join("delete-fixtures.cmd");
        fs::write(
            &script,
            concat!(
                "@echo off\r\n",
                "del /f /q \"%WORKSPACE_DELETE%\"\r\n",
                "del /f /q \"%TEMP_DELETE%\"\r\n",
                "del /f /q \"%TMP_DELETE%\"\r\n",
                "move /y \"%OUTSIDE_DELETE%\" \"%OUTSIDE_MOVE_TARGET%\"\r\n",
                "del /f /q \"%OUTSIDE_DELETE%\"\r\n",
                "rmdir \"%OUTSIDE_DIR%\"\r\n",
                "rmdir \"%PROTECTED_GIT_DIR%\"\r\n",
                ">\"%NESTED_EXISTING%\" echo after\r\n",
                "del /f /q \"%LINKED_DELETE%\"\r\n",
                "mkdir \"%MADE_DIR%\"\r\n",
                ">\"%MADE_FILE%\" echo made\r\n",
                "exit /b 0\r\n",
            ),
        )
        .expect("write delete script");

        let env_map = HashMap::from([
            ("TEMP".to_string(), temp_root.to_string_lossy().into_owned()),
            ("TMP".to_string(), tmp_root.to_string_lossy().into_owned()),
            (
                "WORKSPACE_DELETE".to_string(),
                workspace_file.to_string_lossy().into_owned(),
            ),
            (
                "TEMP_DELETE".to_string(),
                temp_file.to_string_lossy().into_owned(),
            ),
            (
                "TMP_DELETE".to_string(),
                tmp_file.to_string_lossy().into_owned(),
            ),
            (
                "OUTSIDE_DELETE".to_string(),
                outside_file.to_string_lossy().into_owned(),
            ),
            (
                "PROTECTED_GIT_DIR".to_string(),
                protected_git_dir.to_string_lossy().into_owned(),
            ),
            (
                "NESTED_EXISTING".to_string(),
                nested_existing.to_string_lossy().into_owned(),
            ),
            (
                "OUTSIDE_DIR".to_string(),
                outside_dir.to_string_lossy().into_owned(),
            ),
            (
                "OUTSIDE_MOVE_TARGET".to_string(),
                outside_move_target.to_string_lossy().into_owned(),
            ),
            (
                "LINKED_DELETE".to_string(),
                linked_delete.to_string_lossy().into_owned(),
            ),
            (
                "MADE_DIR".to_string(),
                made_dir.to_string_lossy().into_owned(),
            ),
            (
                "MADE_FILE".to_string(),
                made_file.to_string_lossy().into_owned(),
            ),
        ]);

        let spawned = spawn_windows_sandbox_session_legacy(
            &permission_profile,
            &[workspace.as_path(), second_root.as_path()]
                .map(|root| AbsolutePathBuf::from_absolute_path(root).expect("absolute root")),
            codex_home.path(),
            vec![
                "C:\\Windows\\System32\\cmd.exe".to_string(),
                "/d".to_string(),
                "/c".to_string(),
                script.display().to_string(),
            ],
            workspace.as_path(),
            env_map,
            /*timeout_ms*/ Some(5_000),
            &[],
            &[],
            /*tty*/ false,
            /*stdin_open*/ false,
            /*use_private_desktop*/ true,
        )
        .await
        .expect("spawn legacy delete session");
        let (stdout, exit_code) =
            collect_stdout_and_exit(spawned, codex_home.path(), Duration::from_secs(/*secs*/ 10))
                .await;
        let stdout = String::from_utf8_lossy(&stdout);
        let canonical_workspace = dunce::canonicalize(&workspace).expect("canonical workspace");
        // SAFETY: reads the label of a folder this test made.
        let workspace_label_after =
            unsafe { crate::integrity::read_explicit_labels(&workspace) }.expect("read label");
        let recorded = crate::integrity_leases::recorded_roots().expect("read the label records");

        (
            LegacyDeleteOutcome {
                exit_code,
                workspace_file_exists: workspace_file.exists(),
                temp_file_exists: temp_file.exists(),
                tmp_file_exists: tmp_file.exists(),
                outside_file_contents: fs::read_to_string(&outside_file).ok(),
                outside_move_target_exists: outside_move_target.exists(),
                protected_git_dir_exists: protected_git_dir.is_dir(),
                nested_existing_contents: fs::read_to_string(&nested_existing).ok(),
                outside_dir_exists: outside_dir.is_dir(),
                low_can_write_git_dir: low_integrity_can_open_for_write(&protected_git_dir),
                low_can_write_codex_file: low_integrity_can_open_for_write(&codex_file),
                low_can_write_workspace_file: low_integrity_can_open_for_write(&nested_existing),
                low_can_write_made_file: made_file.exists()
                    && low_integrity_can_open_for_write(&made_file),
                made_file_exists: made_file.exists(),
                linked_file_exists: linked_dir.join("linked.txt").exists(),
                workspace_label_restored: workspace_label_after == workspace_label_before
                    && !recorded.contains(&canonical_workspace),
                low_can_write_junction_target: low_integrity_can_open_for_write(&junction_target),
            },
            format!("stdout={stdout:?}\n{}", sandbox_log(codex_home.path())),
        )
    })
}

#[test]
fn legacy_workspace_write_can_delete_inside_writable_roots() {
    let (outcome, diagnostics) = run_legacy_delete_fixture(PermissionProfile::workspace_write());
    assert_eq!(
        (
            outcome.exit_code,
            outcome.workspace_file_exists,
            outcome.temp_file_exists,
            outcome.tmp_file_exists,
            outcome.nested_existing_contents.as_deref().map(str::trim),
        ),
        (0, false, false, false, Some("after")),
        "{diagnostics}"
    );
}

#[test]
fn legacy_workspace_write_cannot_delete_or_replace_protected_git_dir() {
    let _guard = legacy_process_test_guard();
    let runtime = current_thread_runtime();
    runtime.block_on(async move {
        // A user-owned root grants the user FILE_DELETE_CHILD everywhere, which a
        // WRITE_RESTRICTED token does not check against capability SIDs (#158).
        let test_root = user_owned_test_root();
        let codex_home = sandbox_home("legacy-protected-git");
        let workspace = test_root.path().join("workspace");
        let git_dir = workspace.join(".git");
        let hooks_dir = git_dir.join("hooks");
        fs::create_dir_all(&hooks_dir).expect("create .git/hooks");
        let git_config = git_dir.join("config");
        let hook = hooks_dir.join("pre-commit");
        fs::write(&git_config, "original").expect("seed .git/config");
        fs::write(&hook, "original").expect("seed hook");
        let workspace_file = workspace.join("workspace-delete.txt");
        fs::write(&workspace_file, "workspace").expect("seed workspace file");

        let script = codex_home.path().join("replace-git.cmd");
        fs::write(
            &script,
            concat!(
                "@echo off\r\n",
                "del /f /q \"%WORKSPACE_DELETE%\"\r\n",
                "move \"%GIT_DIR%\\config\" \"%WORKSPACE%\\stolen-config\"\r\n",
                "move \"%GIT_DIR%\\hooks\" \"%WORKSPACE%\\stolen-hooks\"\r\n",
                "del /f /q \"%GIT_DIR%\\hooks\\pre-commit\"\r\n",
                "rd /s /q \"%GIT_DIR%\"\r\n",
                "ren \"%GIT_DIR%\" git-renamed\r\n",
                "mkdir \"%GIT_DIR%\"\r\n",
                "echo planted> \"%GIT_DIR%\\config\"\r\n",
                "exit /b 0\r\n",
            ),
        )
        .expect("write replace-git script");
        let env_map = HashMap::from([
            (
                "WORKSPACE".to_string(),
                workspace.to_string_lossy().into_owned(),
            ),
            (
                "WORKSPACE_DELETE".to_string(),
                workspace_file.to_string_lossy().into_owned(),
            ),
            (
                "GIT_DIR".to_string(),
                git_dir.to_string_lossy().into_owned(),
            ),
        ]);

        let spawned = spawn_windows_sandbox_session_legacy(
            &PermissionProfile::workspace_write(),
            workspace_roots_for(workspace.as_path()).as_slice(),
            codex_home.path(),
            vec![
                "C:\\Windows\\System32\\cmd.exe".to_string(),
                "/d".to_string(),
                "/c".to_string(),
                script.display().to_string(),
            ],
            workspace.as_path(),
            with_session_temp(codex_home.path(), env_map),
            /*timeout_ms*/ Some(5_000),
            &[],
            &[],
            /*tty*/ false,
            /*stdin_open*/ false,
            /*use_private_desktop*/ true,
        )
        .await
        .expect("spawn legacy replace-git session");
        let (stdout, exit_code) =
            collect_stdout_and_exit(spawned, codex_home.path(), Duration::from_secs(/*secs*/ 10))
                .await;
        let stdout = String::from_utf8_lossy(&stdout);
        let sandbox_outcome = (
            exit_code,
            workspace_file.exists(),
            fs::read_to_string(&git_config).ok(),
            fs::read_to_string(&hook).ok(),
            workspace.join("stolen-config").exists(),
            workspace.join("stolen-hooks").exists(),
            workspace.join("git-renamed").exists(),
        );
        // The deny must not stop the unsandboxed user from deleting their own files.
        let host_delete = fs::remove_dir_all(&git_dir).map_err(|err| err.to_string());

        assert_eq!(
            (sandbox_outcome, host_delete),
            (
                (
                    0,
                    false,
                    Some("original".to_string()),
                    Some("original".to_string()),
                    false,
                    false,
                    false,
                ),
                Ok(()),
            ),
            "stdout={stdout:?}\n{}",
            sandbox_log(codex_home.path())
        );
    });
}

/// #1506 item 3 (D263): moving a folder between a writable root and an
/// explicit nested read-only carve-out carried the carve-out away and let the
/// command recreate it with its own contents. Those folders can't be moved or
/// renamed now; writing inside them still works.
#[test]
fn legacy_workspace_write_cannot_move_ancestor_of_nested_carve_out() {
    let _guard = legacy_process_test_guard();
    let runtime = current_thread_runtime();
    runtime.block_on(async move {
        let test_root = user_owned_test_root();
        let codex_home = sandbox_home("legacy-nested-carve-out");
        let workspace = test_root.path().join("workspace");
        let outer = workspace.join("a");
        let inner = outer.join("b");
        let carve_out = inner.join("c");
        fs::create_dir_all(&carve_out).expect("create nested carve-out");
        let kept = carve_out.join("keep.txt");
        fs::write(&kept, "original").expect("seed carve-out file");
        let outer_file = outer.join("other.txt");
        fs::write(&outer_file, "before").expect("seed outer file");

        let script = codex_home.path().join("move-ancestor.cmd");
        fs::write(
            &script,
            concat!(
                "@echo off\r\n",
                "echo after> \"%OUTER%\\other.txt\"\r\n",
                "move \"%OUTER%\\b\" \"%WORKSPACE%\\b-moved\"\r\n",
                "ren \"%OUTER%\\b\" b-renamed\r\n",
                "move \"%OUTER%\" \"%WORKSPACE%\\a-moved\"\r\n",
                "ren \"%OUTER%\" a-renamed\r\n",
                "mkdir \"%OUTER%\\b\\c\"\r\n",
                "echo planted> \"%OUTER%\\b\\c\\keep.txt\"\r\n",
                "exit /b 0\r\n",
            ),
        )
        .expect("write move-ancestor script");
        let env_map = HashMap::from([
            (
                "WORKSPACE".to_string(),
                workspace.to_string_lossy().into_owned(),
            ),
            ("OUTER".to_string(), outer.to_string_lossy().into_owned()),
        ]);
        let carve_outs =
            [AbsolutePathBuf::from_absolute_path(&carve_out).expect("absolute carve-out")];

        let spawned = spawn_windows_sandbox_session_legacy(
            &PermissionProfile::workspace_write(),
            workspace_roots_for(workspace.as_path()).as_slice(),
            codex_home.path(),
            vec![
                "C:\\Windows\\System32\\cmd.exe".to_string(),
                "/d".to_string(),
                "/c".to_string(),
                script.display().to_string(),
            ],
            workspace.as_path(),
            with_session_temp(codex_home.path(), env_map),
            /*timeout_ms*/ Some(5_000),
            &[],
            &carve_outs,
            /*tty*/ false,
            /*stdin_open*/ false,
            /*use_private_desktop*/ true,
        )
        .await
        .expect("spawn legacy move-ancestor session");
        let (stdout, exit_code) =
            collect_stdout_and_exit(spawned, codex_home.path(), Duration::from_secs(/*secs*/ 10))
                .await;
        let stdout = String::from_utf8_lossy(&stdout);
        let sandbox_outcome = (
            exit_code,
            fs::read_to_string(&kept).ok(),
            fs::read_to_string(&outer_file).ok(),
            ["b-moved", "a-moved", "a-renamed"]
                .iter()
                .filter(|name| workspace.join(name).exists())
                .copied()
                .collect::<Vec<_>>(),
            outer.join("b-renamed").exists(),
        );
        // The unsandboxed user can still rename their own folders.
        let host_rename =
            fs::rename(&outer, workspace.join("a-host")).map_err(|err| err.to_string());

        assert_eq!(
            (sandbox_outcome, host_rename),
            (
                (
                    0,
                    Some("original".to_string()),
                    Some("after\r\n".to_string()),
                    Vec::<&str>::new(),
                    false,
                ),
                Ok(()),
            ),
            "stdout={stdout:?}\n{}",
            sandbox_log(codex_home.path())
        );
    });
}

/// #158: WRITE_RESTRICTED tokens do not restrict FILE_DELETE_CHILD, so the
/// parent's ACL let the sandbox delete or move anything the user could; the
/// Low-integrity token keeps deletes and moves inside the writable roots.
#[test]
fn legacy_workspace_write_delete_is_limited_to_writable_roots() {
    let (outcome, diagnostics) = run_legacy_delete_fixture(PermissionProfile::workspace_write());
    assert_eq!(
        outcome,
        LegacyDeleteOutcome {
            exit_code: 0,
            workspace_file_exists: false,
            temp_file_exists: false,
            tmp_file_exists: false,
            outside_file_contents: Some("outside".to_string()),
            outside_move_target_exists: false,
            protected_git_dir_exists: true,
            nested_existing_contents: Some("after\r\n".to_string()),
            outside_dir_exists: true,
            low_can_write_git_dir: false,
            low_can_write_codex_file: false,
            low_can_write_workspace_file: false,
            low_can_write_made_file: false,
            made_file_exists: true,
            linked_file_exists: true,
            workspace_label_restored: true,
            low_can_write_junction_target: true,
        },
        "{diagnostics}"
    );
}

/// #158: a read-only legacy session deleted the workspace, `.git` and files
/// outside it through their parents; at Low integrity it deletes nothing.
#[test]
fn legacy_read_only_cannot_delete_anything() {
    let (outcome, diagnostics) = run_legacy_delete_fixture(PermissionProfile::read_only());
    assert_eq!(
        (
            outcome.exit_code,
            outcome.workspace_file_exists,
            outcome.temp_file_exists,
            outcome.tmp_file_exists,
            outcome.outside_file_contents.as_deref(),
            outcome.outside_move_target_exists,
            outcome.protected_git_dir_exists,
            outcome.nested_existing_contents.as_deref(),
            outcome.outside_dir_exists,
            outcome.made_file_exists,
            outcome.linked_file_exists,
            outcome.workspace_label_restored,
        ),
        (
            0,
            true,
            true,
            true,
            Some("outside"),
            false,
            true,
            Some("before"),
            true,
            false,
            true,
            true,
        ),
        "{diagnostics}"
    );
}

/// Runs `script` (cmd lines) in a legacy session on `workspace`, with
/// `TEMP`/`TMP` set to `temp`; returns its exit code.
fn run_legacy_script(
    permission_profile: &PermissionProfile,
    workspace: &Path,
    temp: &Path,
    script: &str,
) -> i32 {
    let runtime = current_thread_runtime();
    runtime.block_on(async move {
        let codex_home = sandbox_home("legacy-script");
        let script_path = temp.join("legacy-script.cmd");
        fs::write(&script_path, format!("@echo off\r\n{script}exit /b 0\r\n"))
            .expect("write script");
        let env_map = HashMap::from([
            ("TEMP".to_string(), temp.to_string_lossy().into_owned()),
            ("TMP".to_string(), temp.to_string_lossy().into_owned()),
        ]);
        let spawned = spawn_windows_sandbox_session_legacy(
            permission_profile,
            workspace_roots_for(workspace).as_slice(),
            codex_home.path(),
            vec![
                "C:\\Windows\\System32\\cmd.exe".to_string(),
                "/d".to_string(),
                "/c".to_string(),
                script_path.display().to_string(),
            ],
            workspace,
            env_map,
            /*timeout_ms*/ Some(5_000),
            &[],
            &[],
            /*tty*/ false,
            /*stdin_open*/ false,
            /*use_private_desktop*/ true,
        )
        .await
        .expect("spawn legacy session");
        let (_stdout, exit_code) =
            collect_stdout_and_exit(spawned, codex_home.path(), Duration::from_secs(/*secs*/ 10))
                .await;
        exit_code
    })
}

/// #158 option (b): the labels a workspace-write session needed are put
/// back when it ends, so a read-only session started after it on the same
/// workspace and `TEMP` deletes nothing, including what the first session
/// made there. With the labels left in place it could.
#[test]
fn legacy_read_only_after_workspace_write_cannot_delete() {
    let _guard = legacy_process_test_guard();
    let test_root = user_owned_test_root();
    let workspace = test_root.path().join("workspace");
    let temp = test_root.path().join("temp");
    for directory in [&workspace, &temp] {
        fs::create_dir_all(directory).expect("create test directory");
    }
    let workspace_file = workspace.join("existing.txt");
    let temp_file = temp.join("existing.txt");
    fs::write(&workspace_file, "before").expect("seed workspace file");
    fs::write(&temp_file, "before").expect("seed TEMP file");
    let made_dir = workspace.join("made");
    let made_file = made_dir.join("new.txt");
    let temp_made = temp.join("made-in-temp.txt");

    let write_exit = run_legacy_script(
        &PermissionProfile::workspace_write(),
        &workspace,
        &temp,
        &format!(
            "mkdir \"{made}\"\r\n>\"{made_file}\" echo made\r\n>\"{temp_made}\" echo made\r\n>\"{existing}\" echo after\r\n",
            made = made_dir.display(),
            made_file = made_file.display(),
            temp_made = temp_made.display(),
            existing = workspace_file.display(),
        ),
    );
    let after_write = (
        write_exit,
        made_file.exists(),
        temp_made.exists(),
        fs::read_to_string(&workspace_file)
            .ok()
            .map(|contents| contents.trim().to_string()),
    );
    assert_eq!(
        after_write,
        (0, true, true, Some("after".to_string())),
        "the workspace-write session could not write its roots"
    );

    let read_exit = run_legacy_script(
        &PermissionProfile::read_only(),
        &workspace,
        &temp,
        &format!(
            "del /f /q \"{existing}\"\r\ndel /f /q \"{made_file}\"\r\ndel /f /q \"{temp_made}\"\r\ndel /f /q \"{temp_file}\"\r\nmove /y \"{temp_file}\" \"{moved}\"\r\n",
            existing = workspace_file.display(),
            made_file = made_file.display(),
            temp_made = temp_made.display(),
            temp_file = temp_file.display(),
            moved = workspace.join("moved.txt").display(),
        ),
    );
    assert_eq!(
        (
            read_exit,
            workspace_file.exists(),
            made_file.exists(),
            temp_made.exists(),
            temp_file.exists(),
            workspace.join("moved.txt").exists(),
            low_integrity_can_open_for_write(&workspace_file),
            low_integrity_can_open_for_write(&made_file),
            low_integrity_can_open_for_write(&temp_file),
        ),
        (0, true, true, true, true, false, false, false, false),
    );
}

/// #158 option (b): no Low-integrity process (a sandboxed command, the
/// credential broker) may read, change or hold open the label records, which
/// would keep a release from ever putting the labels back. `FILE_EXECUTE`
/// counts as a read for file sharing, so it is tried too.
#[test]
fn legacy_label_registry_is_closed_to_low_integrity() {
    use windows_sys::Win32::Storage::FileSystem::DELETE;
    use windows_sys::Win32::Storage::FileSystem::FILE_EXECUTE;
    use windows_sys::Win32::Storage::FileSystem::FILE_READ_DATA;
    use windows_sys::Win32::Storage::FileSystem::FILE_WRITE_DATA;
    use windows_sys::Win32::System::SystemServices::SYSTEM_MANDATORY_LABEL_NO_EXECUTE_UP;
    use windows_sys::Win32::System::SystemServices::SYSTEM_MANDATORY_LABEL_NO_READ_UP;
    use windows_sys::Win32::System::SystemServices::SYSTEM_MANDATORY_LABEL_NO_WRITE_UP;
    let _guard = legacy_process_test_guard();
    let test_root = user_owned_test_root();
    let root = test_root.path().join("root");
    fs::create_dir_all(&root).expect("create root");
    let canonical_root = dunce::canonicalize(&root).expect("canonical root");
    // A lease creates the registry and its files.
    drop(
        crate::integrity_leases::acquire(
            std::slice::from_ref(&canonical_root),
            /*log_dir*/ None,
        )
        .expect("lease"),
    );
    let parent = crate::integrity_leases::registry_parent_dir().expect("registry parent");
    let registry = parent.join("integrity-labels");
    // A registry with an older label (no no-execute-up) is relabelled.
    let older = crate::integrity::LabelAce {
        flags: 0x3,
        mask: SYSTEM_MANDATORY_LABEL_NO_WRITE_UP | SYSTEM_MANDATORY_LABEL_NO_READ_UP,
        sid: "S-1-16-8192".to_string(),
    };
    for dir in [&parent, &registry] {
        // SAFETY: relabels the registry folders this test's lease made.
        unsafe { crate::integrity::set_explicit_labels(dir, std::slice::from_ref(&older)) }
            .expect("set the older label");
    }
    drop(crate::integrity_leases::acquire(&[canonical_root], /*log_dir*/ None).expect("lease"));
    for dir in [&parent, &registry] {
        // SAFETY: reads the label of a registry folder.
        let labels = unsafe { crate::integrity::read_explicit_labels(dir) }.expect("read label");
        assert!(
            labels
                .iter()
                .any(|ace| ace.mask & SYSTEM_MANDATORY_LABEL_NO_EXECUTE_UP != 0),
            "{} was not relabelled: {labels:?}",
            dir.display()
        );
    }
    // Positive control: a Medium folder without no-execute-up can be held
    // open for execute by a Low token.
    assert!(
        low_integrity_can_open(test_root.path(), FILE_EXECUTE, /*share*/ 0),
        "the Low token can't open an ordinary Medium folder for execute"
    );
    let mut opened = Vec::new();
    for path in [
        registry.join("registry.lock"),
        registry.join("labels.json"),
        registry,
        parent,
    ] {
        assert!(path.exists(), "{} is missing", path.display());
        for (name, access) in [
            ("execute", FILE_EXECUTE),
            ("read", FILE_READ_DATA),
            ("write", FILE_WRITE_DATA),
            ("delete", DELETE),
        ] {
            if low_integrity_can_open(&path, access, /*share*/ 0) {
                opened.push(format!("{name} {}", path.display()));
            }
        }
    }
    assert_eq!(opened, Vec::<String>::new());
}

/// Measures labelling and putting back a tree of
/// `CODEX_158_COST_FILES` files (default 20000). Evidence for the PR, not a
/// gate.
#[test]
#[ignore = "measurement"]
fn legacy_integrity_label_cost() {
    let count: usize = std::env::var("CODEX_158_COST_FILES")
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(20_000);
    let _guard = legacy_process_test_guard();
    let test_root = user_owned_test_root();
    let root = test_root.path().join("tree");
    for i in 0..count {
        let dir = root.join(format!("d{}", i / 100));
        if i % 100 == 0 {
            fs::create_dir_all(&dir).expect("create dir");
        }
        fs::write(dir.join(format!("f{i}.txt")), "x").expect("write file");
    }
    let canonical = dunce::canonicalize(&root).expect("canonical root");
    let started = Instant::now();
    let lease = crate::integrity_leases::acquire(&[canonical], /*log_dir*/ None).expect("lease");
    let labelled = started.elapsed();
    let started = Instant::now();
    drop(lease);
    let restored = started.elapsed();
    eprintln!("integrity label cost: {count} files, label {labelled:?}, put back {restored:?}");
}

/// #734: a root a lease can't label is reported for core's warning, and not
/// once a later lease labels it. (A missing folder can't be labelled; the
/// lease still starts, as #705 requires.)
#[test]
fn legacy_unlabelled_root_is_reported_until_labelled() {
    let _guard = legacy_process_test_guard();
    let test_root = user_owned_test_root();
    let root = dunce::canonicalize(test_root.path())
        .expect("canonical test root")
        .join("not-yet");
    let mark = crate::unlabelled_roots_mark();
    drop(
        crate::integrity_leases::acquire(std::slice::from_ref(&root), /*log_dir*/ None)
            .expect("lease"),
    );
    assert!(
        crate::unlabelled_roots_since(mark).contains(&root),
        "{} is not reported",
        root.display()
    );
    fs::create_dir_all(&root).expect("create root");
    drop(
        crate::integrity_leases::acquire(std::slice::from_ref(&root), /*log_dir*/ None)
            .expect("lease"),
    );
    assert!(
        !crate::unlabelled_roots_since(mark).contains(&root),
        "{} is still reported",
        root.display()
    );
}

/// #734: core matches failures against the roots the sandbox itself labels:
/// canonical, every workspace root (not only the working directory), and
/// `%TEMP%` for a writable temp entry.
#[test]
fn legacy_writable_roots_are_the_labelled_roots() {
    use codex_protocol::permissions::FileSystemAccessMode;
    use codex_protocol::permissions::FileSystemPath;
    use codex_protocol::permissions::FileSystemSandboxEntry;
    use codex_protocol::permissions::FileSystemSandboxPolicy;
    use codex_protocol::permissions::FileSystemSpecialPath;
    let test_root = user_owned_test_root();
    let base = dunce::canonicalize(test_root.path()).expect("canonical test root");
    let (cwd, other) = (base.join("cwd"), base.join("other"));
    fs::create_dir_all(&cwd).expect("cwd");
    fs::create_dir_all(&other).expect("other");
    let special = |value| {
        FileSystemSandboxEntry::new(
            FileSystemPath::Special { value },
            FileSystemAccessMode::Write,
        )
    };
    let policy = FileSystemSandboxPolicy::restricted(vec![
        special(FileSystemSpecialPath::project_roots(/*subpath*/ None)),
        special(FileSystemSpecialPath::Tmpdir),
    ]);
    let workspace_roots = [
        AbsolutePathBuf::from_absolute_path(&cwd).expect("absolute"),
        AbsolutePathBuf::from_absolute_path(&other).expect("absolute"),
    ];
    let roots = crate::legacy_writable_roots(&policy, &workspace_roots, &cwd);
    let temp = dunce::canonicalize(std::env::var_os("TEMP").expect("TEMP")).expect("TEMP");
    for want in [&cwd, &other, &temp] {
        assert!(
            roots.contains(want),
            "{} missing from {roots:?}",
            want.display()
        );
    }
}

const BREAKAWAY_PROBE_ENV: &str = "CODEX_733_BREAKAWAY_PROBE";
const BREAKAWAY_PROBE_TEST: &str = "unified_exec::tests::legacy_breakaway_probe_entry";
const BREAKAWAY_PROBE_RESULT: &str = "breakaway-probe: ";

/// The command of [`legacy_command_cannot_break_away_from_its_job`]: starts
/// a short `cmd.exe` normally, then with `CREATE_BREAKAWAY_FROM_JOB`, and
/// reports whether Windows allowed each. Does nothing otherwise.
#[test]
fn legacy_breakaway_probe_entry() {
    use windows_sys::Win32::Foundation::CloseHandle;
    use windows_sys::Win32::Foundation::GetLastError;
    use windows_sys::Win32::System::Threading::CREATE_BREAKAWAY_FROM_JOB;
    use windows_sys::Win32::System::Threading::CREATE_NO_WINDOW;
    use windows_sys::Win32::System::Threading::CreateProcessW;
    use windows_sys::Win32::System::Threading::PROCESS_INFORMATION;
    use windows_sys::Win32::System::Threading::STARTUPINFOW;

    if std::env::var_os(BREAKAWAY_PROBE_ENV).is_none() {
        return;
    }
    let start = |flags: u32| -> (bool, u32) {
        let mut command_line = crate::winutil::to_wide(format!(
            "\"{}\" /d /c exit 0",
            system32_exe("cmd.exe").display()
        ));
        // SAFETY: zeroed plain-data structs; the call fills in `info`.
        let mut startup: STARTUPINFOW = unsafe { std::mem::zeroed() };
        startup.cb = std::mem::size_of::<STARTUPINFOW>() as u32;
        let mut info: PROCESS_INFORMATION = unsafe { std::mem::zeroed() };
        // SAFETY: every pointer refers to a live buffer above; nothing is inherited.
        let started = unsafe {
            CreateProcessW(
                std::ptr::null(),
                command_line.as_mut_ptr(),
                std::ptr::null(),
                std::ptr::null(),
                /*binherithandles*/ 0,
                CREATE_NO_WINDOW | flags,
                std::ptr::null(),
                std::ptr::null(),
                &startup,
                &mut info,
            )
        } != 0;
        if !started {
            // SAFETY: read right after the call.
            return (false, unsafe { GetLastError() });
        }
        // SAFETY: returned by the call above and not used again.
        unsafe {
            CloseHandle(info.hThread);
            CloseHandle(info.hProcess);
        }
        (true, 0)
    };
    // The control shows a refusal below comes from the job, not from the
    // token or the desktop.
    let (control, control_error) = start(/*flags*/ 0);
    let (started, error) = start(CREATE_BREAKAWAY_FROM_JOB);
    // Own line: libtest prints the test name without a newline first.
    println!(
        "\n{BREAKAWAY_PROBE_RESULT}control={control}/{control_error} breakaway={started}/{error}"
    );
}

/// #733: a legacy command can't start a child outside its job. With
/// breakaway allowed, such a child outlived the job, so neither cancellation
/// nor the integrity lease's release (which waits for the job to empty)
/// reached it, and it kept running at Low integrity in roots whose label
/// was being put back.
#[test]
fn legacy_command_cannot_break_away_from_its_job() {
    let _guard = legacy_process_test_guard();
    let test_root = user_owned_test_root();
    let runtime = current_thread_runtime();
    let (stdout, exit_code, log) = runtime.block_on(async {
        let codex_home = sandbox_home("legacy-breakaway");
        let system_root = std::env::var("SystemRoot").unwrap_or_else(|_| "C:\\Windows".into());
        let env_map = HashMap::from([
            (BREAKAWAY_PROBE_ENV.to_string(), "1".to_string()),
            ("SystemRoot".to_string(), system_root),
        ]);
        let spawned = spawn_windows_sandbox_session_legacy(
            &PermissionProfile::workspace_write(),
            workspace_roots_for(test_root.path()).as_slice(),
            codex_home.path(),
            vec![
                std::env::current_exe()
                    .expect("test exe")
                    .display()
                    .to_string(),
                "--exact".to_string(),
                BREAKAWAY_PROBE_TEST.to_string(),
                "--nocapture".to_string(),
                "--test-threads=1".to_string(),
            ],
            test_root.path(),
            with_session_temp(codex_home.path(), env_map),
            /*timeout_ms*/ Some(60_000),
            &[],
            &[],
            /*tty*/ false,
            /*stdin_open*/ false,
            /*use_private_desktop*/ true,
        )
        .await
        .expect("spawn legacy breakaway probe");
        let (stdout, exit_code) =
            collect_stdout_and_exit(spawned, codex_home.path(), Duration::from_secs(/*secs*/ 90))
                .await;
        (
            String::from_utf8_lossy(&stdout).into_owned(),
            exit_code,
            sandbox_log(codex_home.path()),
        )
    });
    let result = stdout
        .lines()
        .find_map(|line| line.trim().strip_prefix(BREAKAWAY_PROBE_RESULT));
    // 5 is ERROR_ACCESS_DENIED: the job refuses the breakaway.
    assert_eq!(
        (exit_code, result),
        (0, Some("control=true/0 breakaway=false/5")),
        "stdout={stdout:?}\n{log}"
    );
}

/// D256 (#1507): an elevated-backend command can't start a child outside
/// its job either, with or without ConPTY. With breakaway allowed, such a
/// child outlived cancellation and timeouts (the runner ends the job, not
/// the process tree). Elevated: the sandbox's setup needs it.
#[test]
fn sec_win_1507_elevated_command_cannot_break_away_from_its_job() {
    // Bazel gives unit tests no staged sandbox helpers (#691).
    if option_env!("BAZEL_PACKAGE").is_some() {
        return;
    }
    if !crate::setup::is_elevated().expect("elevation") {
        // The elevated Windows gates set this, so a skip there fails.
        assert!(
            std::env::var_os("CODEX_SEC_WIN_341_REQUIRE").is_none(),
            "sec-win-1507 needs an elevated run"
        );
        eprintln!("sec-win-1507: the sandbox's setup needs an elevated run; skipped");
        return;
    }
    crate::window_station::tests::stage_sandbox_helpers();
    let _guard = legacy_process_test_guard();
    let runtime = current_thread_runtime();
    let cwd = std::env::current_dir().expect("cwd");
    let mut results = Vec::new();
    for tty in [false, true] {
        let codex_home = sandbox_home("elevated-breakaway");
        let env_map = HashMap::from([
            (BREAKAWAY_PROBE_ENV.to_string(), "1".to_string()),
            (
                "SystemRoot".to_string(),
                std::env::var("SystemRoot").expect("SystemRoot"),
            ),
        ]);
        let (stdout, exit_code) = runtime.block_on(async {
            let spawned = spawn_windows_sandbox_session_elevated_for_permission_profile(
                &PermissionProfile::read_only(),
                workspace_roots_for(cwd.as_path()).as_slice(),
                codex_home.path(),
                vec![
                    std::env::current_exe()
                        .expect("test exe")
                        .display()
                        .to_string(),
                    "--exact".to_string(),
                    BREAKAWAY_PROBE_TEST.to_string(),
                    "--nocapture".to_string(),
                    "--test-threads=1".to_string(),
                ],
                cwd.as_path(),
                env_map,
                /*proxy_enforced*/ false,
                /*network_proxy_restricting_sid*/ None,
                Some(60_000),
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
            .expect("spawn elevated breakaway probe");
            collect_stdout_and_exit(spawned, codex_home.path(), Duration::from_secs(/*secs*/ 90))
                .await
        });
        let stdout = String::from_utf8_lossy(&stdout).into_owned();
        // Under ConPTY the line carries terminal escapes.
        let result = stdout.lines().find_map(|line| {
            line.find(BREAKAWAY_PROBE_RESULT).map(|start| {
                line[start + BREAKAWAY_PROBE_RESULT.len()..]
                    .chars()
                    .take_while(|c| !c.is_control())
                    .collect::<String>()
                    .trim()
                    .to_string()
            })
        });
        eprintln!("sec-win-1507: tty {tty}: exit {exit_code}, {result:?}");
        results.push((tty, exit_code, result, stdout));
    }
    // 5 is ERROR_ACCESS_DENIED: the job refuses the breakaway.
    let refused = |tty| (tty, 0, Some("control=true/0 breakaway=false/5".to_string()));
    assert_eq!(
        results
            .iter()
            .map(|(tty, code, result, _)| (*tty, *code, result.clone()))
            .collect::<Vec<_>>(),
        vec![refused(false), refused(true)],
        "{results:?}"
    );
}

/// D256 (#1507): common tools still work in an elevated-backend command
/// whose job refuses breakaway: a cmd pipeline, Windows PowerShell calling a
/// native program (with and without ConPTY), and `git init` and `git status`.
#[test]
fn sec_win_1507_elevated_tools_work_without_breakaway() {
    if option_env!("BAZEL_PACKAGE").is_some() {
        return;
    }
    if !crate::setup::is_elevated().expect("elevation") {
        // The elevated Windows gates set this, so a skip there fails.
        assert!(
            std::env::var_os("CODEX_SEC_WIN_341_REQUIRE").is_none(),
            "sec-win-1507 needs an elevated run"
        );
        eprintln!("sec-win-1507: the sandbox's setup needs an elevated run; skipped");
        return;
    }
    let Some(git) = command_identity::git_for_windows("cmd\\git.exe") else {
        return;
    };
    crate::window_station::tests::stage_sandbox_helpers();
    let _guard = legacy_process_test_guard();
    let root = user_owned_test_root();
    let runtime = current_thread_runtime();
    let powershell = system32_exe("WindowsPowerShell\\v1.0\\powershell.exe")
        .display()
        .to_string();
    let git = git.display().to_string();
    let cmd = system32_exe("cmd.exe").display().to_string();
    // (name, argv, expected output, ConPTY)
    let cases: Vec<(&str, Vec<String>, &[&str], bool)> = vec![
        (
            "cmd pipeline",
            vec![
                cmd.clone(),
                "/d".to_string(),
                "/c".to_string(),
                "echo d256-pipe| findstr d256 2>&1".to_string(),
            ],
            &["d256-pipe"],
            false,
        ),
        (
            "powershell",
            vec![
                powershell.clone(),
                "-NoProfile".to_string(),
                "-Command".to_string(),
                "$x = & cmd /c echo d256-ps; $x | Select-String d256 | ForEach-Object { $_.Line }; \
                 Get-ChildItem | Out-Null; 'd256-ps-done'"
                    .to_string(),
            ],
            &["d256-ps", "d256-ps-done"],
            false,
        ),
        (
            "powershell (ConPTY)",
            vec![
                powershell,
                "-NoProfile".to_string(),
                "-Command".to_string(),
                "& cmd /c echo d256-tty; 'd256-tty-done'".to_string(),
            ],
            &["d256-tty", "d256-tty-done"],
            true,
        ),
        (
            "git init",
            vec![git, "init".to_string(), "-q".to_string()],
            &[],
            false,
        ),
        (
            // `.git` is read-only to sandboxed commands, so no commit.
            "git status",
            vec![
                cmd,
                "/d".to_string(),
                "/c".to_string(),
                // Git for Windows' `cmd` folder is on PATH (CI, and its
                // installer's default).
                "(echo d256> d256.txt && git status --porcelain) 2>&1".to_string(),
            ],
            &["?? d256.txt"],
            false,
        ),
    ];
    // One home, as in a real session: each new one runs the sandbox's setup.
    let codex_home = sandbox_home("elevated-tools");
    let mut failures = Vec::new();
    for (name, argv, expected, tty) in cases {
        let (stdout, exit_code) = runtime.block_on(async {
            let spawned = spawn_windows_sandbox_session_elevated_for_permission_profile(
                &PermissionProfile::workspace_write(),
                workspace_roots_for(root.path()).as_slice(),
                codex_home.path(),
                argv,
                root.path(),
                // Core passes its environment on; PowerShell needs it.
                std::env::vars().collect(),
                /*proxy_enforced*/ false,
                /*network_proxy_restricting_sid*/ None,
                Some(60_000),
                /*read_roots_override*/ None,
                /*read_roots_include_platform_defaults*/ true,
                /*write_roots_override*/ None,
                &crate::DenyReadTargets::default(),
                &[],
                tty,
                /*stdin_open*/ tty,
                /*use_private_desktop*/ true,
            )
            .await
            .unwrap_or_else(|err| {
                panic!(
                    "spawn elevated tool {name}: {err:#}\n{}",
                    sandbox_log(codex_home.path())
                )
            });
            collect_stdout_and_exit(spawned, codex_home.path(), Duration::from_secs(/*secs*/ 90))
                .await
        });
        let stdout = String::from_utf8_lossy(&stdout).into_owned();
        println!("\nd256 tool {name}: exit={exit_code} stdout={stdout:?}");
        if exit_code != 0 || !expected.iter().all(|part| stdout.contains(part)) {
            failures.push(format!("{name}: exit={exit_code} stdout={stdout:?}"));
        }
    }
    assert!(failures.is_empty(), "{failures:#?}");
}

const CRASH_ROOTS_ENV: &str = "CODEX_158_CRASH_ROOTS";
const CRASH_CHILD_TEST: &str = "unified_exec::tests::legacy_integrity_crash_child_entry";
const CRASH_CHILD_READY: &str = "integrity-crash-child: leased";
/// Protected paths whose delete-child denies the crash child adds (#1506 item 1).
const CRASH_DENY_PATHS_ENV: &str = "CODEX_1506_CRASH_DENY_PATHS";

/// The child of [`legacy_crashed_session_labels_are_repaired`]: takes a lease
/// on the roots it is given and waits to be killed. Does nothing otherwise.
#[test]
fn legacy_integrity_crash_child_entry() {
    let Some(roots) = std::env::var_os(CRASH_ROOTS_ENV) else {
        return;
    };
    let roots: Vec<PathBuf> = std::env::split_paths(&roots)
        .map(|root| dunce::canonicalize(root).expect("canonical root"))
        .collect();
    let lease = crate::integrity_leases::acquire(&roots, /*log_dir*/ None).expect("lease");
    if let Some(paths) = std::env::var_os(CRASH_DENY_PATHS_ENV) {
        let denies: Vec<_> = std::env::split_paths(&paths)
            .flat_map(|path| crate::workspace_acl::delete_child_route_denies(&path))
            .collect();
        crate::integrity_leases::add_tracked_delete_child_denies(
            &denies, /*required*/ false, /*log_dir*/ None,
        )
        .expect("add denies");
    }
    // Own line: libtest prints the test name without a newline first.
    println!("\n{CRASH_CHILD_READY}");
    use std::io::Write as _;
    std::io::stdout().flush().expect("flush");
    std::thread::sleep(Duration::from_secs(/*secs*/ 300));
    drop(lease);
}

/// #158 option (b): a session killed while it holds a lease leaves its roots
/// labelled Low; the startup repair puts each root's own label back (none,
/// or an explicit one), and leaves a live session's roots alone.
#[test]
fn legacy_crashed_session_labels_are_repaired() {
    use std::io::BufRead as _;
    let _guard = legacy_process_test_guard();
    let test_root = user_owned_test_root();
    let crashed = test_root.path().join("crashed");
    let kept = test_root.path().join("kept");
    let live = test_root.path().join("live");
    for directory in [&crashed, &kept, &live] {
        fs::create_dir_all(directory).expect("create test directory");
        fs::write(directory.join("file.txt"), "before").expect("seed file");
    }
    // `kept` has its own explicit label, which must come back as it was.
    let output = std::process::Command::new(system32_exe("icacls.exe"))
        .arg(&kept)
        .args(["/setintegritylevel", "(OI)(CI)M"])
        .output()
        .expect("run icacls");
    assert!(output.status.success(), "icacls failed");
    // SAFETY: reads labels of folders this test made.
    let labels = |path: &Path| unsafe { crate::integrity::read_explicit_labels(path) }.unwrap();
    let crashed_before = labels(&crashed);
    let kept_before = labels(&kept);
    assert_eq!((crashed_before.len(), kept_before.len()), (0, 1));
    let canonical = |path: &Path| dunce::canonicalize(path).expect("canonical");

    let mut child = std::process::Command::new(std::env::current_exe().expect("test exe"))
        .args([
            "--exact",
            CRASH_CHILD_TEST,
            "--nocapture",
            "--test-threads=1",
        ])
        .env(
            CRASH_ROOTS_ENV,
            std::env::join_paths([&crashed, &kept]).expect("join roots"),
        )
        .stdout(std::process::Stdio::piped())
        .spawn()
        .expect("start the crash child");
    let stdout = child.stdout.take().expect("child stdout");
    let mut ready = false;
    for line in std::io::BufReader::new(stdout).lines() {
        let line = line.expect("read child stdout");
        if line.trim() == CRASH_CHILD_READY {
            ready = true;
            break;
        }
    }
    let leased = (
        ready,
        low_integrity_can_open_for_write(&crashed.join("file.txt")),
        low_integrity_can_open_for_write(&kept.join("file.txt")),
    );
    child.kill().expect("kill the crash child");
    child.wait().expect("wait for the crash child");
    assert_eq!(
        leased,
        (true, true, true),
        "the child did not label its roots"
    );

    let repaired = crate::repair_stale_integrity_labels().expect("repair");
    let recorded = crate::integrity_leases::recorded_roots().expect("records");
    // Another sweep (any legacy command of this user) may have repaired them
    // first, so the count is not checked.
    assert_eq!(
        (
            low_integrity_can_open_for_write(&crashed.join("file.txt")),
            low_integrity_can_open_for_write(&kept.join("file.txt")),
            labels(&crashed) == crashed_before,
            labels(&kept) == kept_before,
            recorded.contains(&canonical(&crashed)),
            recorded.contains(&canonical(&kept)),
        ),
        (false, false, true, true, false, false),
        "repaired {repaired}: {recorded:?}"
    );

    // A live session's roots are left alone.
    let live_lease =
        crate::integrity_leases::acquire(&[canonical(&live)], /*log_dir*/ None).expect("lease");
    crate::repair_stale_integrity_labels().expect("repair");
    let recorded = crate::integrity_leases::recorded_roots().expect("records");
    assert_eq!(
        (
            low_integrity_can_open_for_write(&live.join("file.txt")),
            recorded.contains(&canonical(&live)),
        ),
        (true, true),
        "{recorded:?}"
    );

    drop(live_lease);
    let recorded = crate::integrity_leases::recorded_roots().expect("records");
    assert_eq!(
        (
            low_integrity_can_open_for_write(&live.join("file.txt")),
            recorded.contains(&canonical(&live)),
        ),
        (false, false)
    );
}

fn everyone_sid() -> Vec<u8> {
    // SAFETY: builds the well-known Everyone SID.
    unsafe { crate::token::world_sid() }.expect("Everyone SID")
}

/// Whether `path` has the Everyone delete-child deny a legacy session adds.
fn has_everyone_delete_child_deny(path: &Path, inherit_to_subdirs: bool) -> bool {
    let mut everyone = everyone_sid();
    // SAFETY: a valid SID and an existing path.
    unsafe {
        crate::acl::has_deny_delete_child_ace(
            path,
            everyone.as_mut_ptr() as *mut std::ffi::c_void,
            inherit_to_subdirs,
        )
    }
    .expect("read DACL")
}

/// #1506 item 1 (D261): the Everyone `FILE_DELETE_CHILD` denies a legacy
/// session adds on `.git`, the paths above an explicit nested read-only path
/// and their parents protect them while it runs, and are gone once it ends.
#[test]
fn legacy_delete_child_denies_are_removed_after_the_session() {
    let _guard = legacy_process_test_guard();
    let runtime = current_thread_runtime();
    runtime.block_on(async move {
        let test_root = user_owned_test_root();
        let codex_home = sandbox_home("legacy-delete-child-cleanup");
        let workspace = test_root.path().join("workspace");
        let git_dir = workspace.join(".git");
        fs::create_dir_all(git_dir.join("hooks")).expect("create .git/hooks");
        let outer = workspace.join("a");
        let inner = outer.join("b");
        let carve_out = inner.join("c");
        fs::create_dir_all(&carve_out).expect("create nested carve-out");
        let carve_outs =
            [AbsolutePathBuf::from_absolute_path(&carve_out).expect("absolute carve-out")];
        let script = codex_home.path().join("delete-protected.cmd");
        fs::write(
            &script,
            concat!(
                "@echo off\r\n",
                "rd /s /q \"%WORKSPACE%\\.git\"\r\n",
                "ren \"%WORKSPACE%\\a\" a-renamed\r\n",
                "exit /b 0\r\n",
            ),
        )
        .expect("write script");
        let env_map = HashMap::from([(
            "WORKSPACE".to_string(),
            workspace.to_string_lossy().into_owned(),
        )]);

        let spawned = spawn_windows_sandbox_session_legacy(
            &PermissionProfile::workspace_write(),
            workspace_roots_for(workspace.as_path()).as_slice(),
            codex_home.path(),
            vec![
                "C:\\Windows\\System32\\cmd.exe".to_string(),
                "/d".to_string(),
                "/c".to_string(),
                script.display().to_string(),
            ],
            workspace.as_path(),
            with_session_temp(codex_home.path(), env_map),
            /*timeout_ms*/ Some(5_000),
            &[],
            &carve_outs,
            /*tty*/ false,
            /*stdin_open*/ false,
            /*use_private_desktop*/ true,
        )
        .await
        .expect("spawn legacy session");
        let (stdout, exit_code) =
            collect_stdout_and_exit(spawned, codex_home.path(), Duration::from_secs(/*secs*/ 10))
                .await;
        let stdout = String::from_utf8_lossy(&stdout);
        let denies_left: Vec<(PathBuf, bool)> = [
            (workspace.clone(), false),
            (git_dir.clone(), true),
            (outer.clone(), false),
            (inner.clone(), false),
            (carve_out.clone(), true),
        ]
        .into_iter()
        .filter(|(path, inherit)| has_everyone_delete_child_deny(path, *inherit))
        .collect();

        assert_eq!(
            (exit_code, git_dir.exists(), outer.exists(), denies_left,),
            (0, true, true, Vec::new()),
            "stdout={stdout:?}\n{}",
            sandbox_log(codex_home.path())
        );
    });
}

/// #1506 item 1 (D261): a recorded deny stays while any live session's root
/// holds it, a crashed session's denies go at the next sweep, and a deny that
/// was there before is never removed.
#[test]
fn legacy_delete_child_denies_follow_live_sessions() {
    use crate::integrity_leases::DeleteChildDeny;
    use std::io::BufRead as _;
    let _guard = legacy_process_test_guard();
    let test_root = user_owned_test_root();
    let canonical = |path: &Path| dunce::canonicalize(path).expect("canonical");
    let root = test_root.path().join("root");
    let protected = root.join("protected");
    let crash_root = test_root.path().join("crash-root");
    let crash_protected = crash_root.join("protected");
    let preexisting = test_root.path().join("preexisting");
    for dir in [&protected, &crash_protected, &preexisting] {
        fs::create_dir_all(dir).expect("create test directory");
    }
    let mut everyone = everyone_sid();
    let everyone = everyone.as_mut_ptr() as *mut std::ffi::c_void;
    // SAFETY: a valid SID and an existing folder.
    assert!(
        unsafe {
            crate::acl::add_deny_delete_child_ace(
                &preexisting,
                everyone,
                /*inherit_to_subdirs*/ false,
            )
        }
        .expect("add preexisting deny")
    );
    let mut route = crate::workspace_acl::delete_child_route_denies(&protected);
    route.push(DeleteChildDeny {
        path: preexisting.clone(),
        inherit_to_subdirs: false,
    });
    let state = || {
        (
            has_everyone_delete_child_deny(&root, /*inherit_to_subdirs*/ false),
            has_everyone_delete_child_deny(&protected, /*inherit_to_subdirs*/ true),
            has_everyone_delete_child_deny(&preexisting, /*inherit_to_subdirs*/ false),
        )
    };
    let recorded_here = || {
        crate::integrity_leases::recorded_delete_child_denies()
            .expect("records")
            .into_iter()
            .filter(|deny| deny.path.starts_with(canonical(test_root.path())))
            .map(|deny| deny.path)
            .collect::<Vec<_>>()
    };

    let first = crate::integrity_leases::acquire(&[canonical(&root)], /*log_dir*/ None)
        .expect("first lease");
    crate::integrity_leases::add_tracked_delete_child_denies(
        &route, /*required*/ false, /*log_dir*/ None,
    )
    .expect("add denies");
    let second = crate::integrity_leases::acquire(&[canonical(&root)], /*log_dir*/ None)
        .expect("second lease");
    crate::integrity_leases::add_tracked_delete_child_denies(
        &route, /*required*/ false, /*log_dir*/ None,
    )
    .expect("add denies");
    let both_live = (state(), recorded_here());
    drop(first);
    let one_live = state();
    drop(second);
    let none_live = (state(), recorded_here());

    let mut child = std::process::Command::new(std::env::current_exe().expect("test exe"))
        .args([
            "--exact",
            CRASH_CHILD_TEST,
            "--nocapture",
            "--test-threads=1",
        ])
        .env(CRASH_ROOTS_ENV, &crash_root)
        .env(CRASH_DENY_PATHS_ENV, &crash_protected)
        .stdout(std::process::Stdio::piped())
        .spawn()
        .expect("start the crash child");
    let stdout = child.stdout.take().expect("child stdout");
    let ready = std::io::BufReader::new(stdout)
        .lines()
        .map_while(Result::ok)
        .any(|line| line.trim() == CRASH_CHILD_READY);
    crate::repair_stale_integrity_labels().expect("repair while live");
    let crash_live = (
        ready,
        has_everyone_delete_child_deny(&crash_root, /*inherit_to_subdirs*/ false),
        has_everyone_delete_child_deny(&crash_protected, /*inherit_to_subdirs*/ true),
    );
    child.kill().expect("kill the crash child");
    child.wait().expect("wait for the crash child");
    crate::repair_stale_integrity_labels().expect("repair");
    let crash_repaired = (
        has_everyone_delete_child_deny(&crash_root, /*inherit_to_subdirs*/ false),
        has_everyone_delete_child_deny(&crash_protected, /*inherit_to_subdirs*/ true),
        recorded_here(),
    );
    // SAFETY: as above.
    let _ = unsafe {
        crate::acl::remove_deny_delete_child_ace(
            &preexisting,
            everyone,
            /*inherit_to_subdirs*/ false,
        )
    };

    let mut recorded_live = vec![canonical(&root), canonical(&protected)];
    recorded_live.sort();
    let mut both_live = both_live;
    both_live.1.sort();
    assert_eq!(
        (both_live, one_live, none_live, crash_live, crash_repaired),
        (
            ((true, true, true), recorded_live),
            (true, true, true),
            ((false, false, true), Vec::new()),
            (true, true, true),
            (false, false, Vec::new()),
        )
    );
}

/// #1506 item 2 (D262): a `.codex` folder that appears while a session runs
/// counts as planted at once (the session may still be running), stays
/// recorded after that session crashes, and stops counting once confirmed.
#[test]
fn legacy_planted_project_metadata_survives_a_crash() {
    use std::io::BufRead as _;
    let _guard = legacy_process_test_guard();
    let test_root = user_owned_test_root();
    let root = test_root.path().join("root");
    fs::create_dir_all(&root).expect("create root");
    let canonical_root = dunce::canonicalize(&root).expect("canonical");
    let dot_codex = canonical_root.join(".codex");
    let dot_agents = canonical_root.join(".agents");
    let planted = || {
        let all = crate::planted_project_metadata().expect("planted records");
        (all.contains(&dot_codex), all.contains(&dot_agents))
    };

    let mut child = std::process::Command::new(std::env::current_exe().expect("test exe"))
        .args([
            "--exact",
            CRASH_CHILD_TEST,
            "--nocapture",
            "--test-threads=1",
        ])
        .env(CRASH_ROOTS_ENV, &root)
        .stdout(std::process::Stdio::piped())
        .spawn()
        .expect("start the crash child");
    let stdout = child.stdout.take().expect("child stdout");
    let ready = std::io::BufReader::new(stdout)
        .lines()
        .map_while(Result::ok)
        .any(|line| line.trim() == CRASH_CHILD_READY);
    let before = planted();
    fs::create_dir(&dot_codex).expect("plant .codex");
    let live = planted();
    child.kill().expect("kill the crash child");
    child.wait().expect("wait for the crash child");
    crate::repair_stale_integrity_labels().expect("repair");
    let crashed = planted();
    let confirmed = crate::confirm_planted_project_metadata(&dot_codex).expect("confirm");
    let after_confirming = planted();

    assert_eq!(
        (ready, before, live, crashed, confirmed, after_confirming),
        (
            true,
            (false, false),
            (true, false),
            (true, false),
            true,
            (false, false),
        )
    );
}

/// #1506 item 1 (D261): a root nested inside another session's protected
/// folder gets its own deny even though it inherits a copy, so it keeps one
/// when the outer session ends and its entry goes.
#[test]
fn legacy_delete_child_denies_nested_root_keeps_its_own() {
    let _guard = legacy_process_test_guard();
    let test_root = user_owned_test_root();
    let canonical = |path: &Path| dunce::canonicalize(path).expect("canonical");
    let outer = test_root.path().join("outer");
    let protected = outer.join("protected");
    let nested = protected.join("nested");
    let inner = nested.join("inner");
    fs::create_dir_all(&inner).expect("create test directories");

    let outer_lease = crate::integrity_leases::acquire(&[canonical(&outer)], /*log_dir*/ None)
        .expect("outer lease");
    crate::integrity_leases::add_tracked_delete_child_denies(
        &crate::workspace_acl::delete_child_route_denies(&protected),
        /*required*/ false,
        /*log_dir*/ None,
    )
    .expect("outer denies");
    let nested_lease =
        crate::integrity_leases::acquire(&[canonical(&nested)], /*log_dir*/ None)
            .expect("nested lease");
    crate::integrity_leases::add_tracked_delete_child_denies(
        &crate::workspace_acl::delete_child_route_denies(&inner),
        /*required*/ true,
        /*log_dir*/ None,
    )
    .expect("nested denies");
    drop(outer_lease);
    let outer_ended = (
        has_everyone_delete_child_deny(&protected, /*inherit_to_subdirs*/ true),
        has_everyone_delete_child_deny(&nested, /*inherit_to_subdirs*/ false),
        has_everyone_delete_child_deny(&inner, /*inherit_to_subdirs*/ true),
    );
    drop(nested_lease);
    let both_ended = (
        has_everyone_delete_child_deny(&nested, /*inherit_to_subdirs*/ false),
        has_everyone_delete_child_deny(&inner, /*inherit_to_subdirs*/ true),
    );

    assert_eq!(
        (outer_ended, both_ended),
        ((false, true, true), (false, false))
    );
}

#[test]
fn legacy_capture_cancellation_terminates_descendants_without_timeout() {
    let Some(pwsh) = pwsh_path() else {
        eprintln!("skipping cancellation regression test: PowerShell 7 is not installed");
        return;
    };
    let _guard = legacy_process_test_guard();
    // #1906: a folder of its own, not the checkout: a legacy session's
    // grant on its workspace lasts.
    let workspace = user_owned_test_root();
    let cwd = workspace.path().to_path_buf();
    let codex_home = sandbox_home("legacy-capture-cancel");
    // What the sandboxed commands write goes in the folder they may write
    // (#1890): the home is not one.
    let scratch = session_temp(codex_home.path());
    let descendant_marker = scratch.join("descendant-survived");
    let ready_marker = scratch.join("descendant-started");
    let descendant_command = format!(
        "Set-Content -LiteralPath '{}' -Value $PID; Start-Sleep -Seconds 1; Set-Content -LiteralPath '{}' -Value survived",
        powershell_literal(&ready_marker),
        powershell_literal(&descendant_marker),
    );
    let parent_command = start_powershell_child(
        &pwsh,
        &scratch,
        &descendant_command,
        "Start-Sleep -Seconds 30",
    );
    let descendant_process = Arc::new(Mutex::new(None));
    let descendant_process_for_cancellation = Arc::clone(&descendant_process);
    let cancellation = WindowsSandboxCancellationToken::new(move || {
        let Ok(pid) = fs::read_to_string(&ready_marker).and_then(|pid| {
            pid.trim()
                .parse()
                .map_err(|err| std::io::Error::other(format!("invalid descendant pid: {err}")))
        }) else {
            return false;
        };
        let Ok(process) = open_process_for_wait(pid) else {
            return false;
        };
        *descendant_process_for_cancellation
            .lock()
            .expect("descendant process lock poisoned") = Some(process);
        true
    });

    let started_at = Instant::now();
    let permission_profile = PermissionProfile::workspace_write();
    let result = run_windows_sandbox_capture(
        &permission_profile,
        workspace_roots_for(cwd.as_path()).as_slice(),
        codex_home.path(),
        vec![
            pwsh.display().to_string(),
            "-NoProfile".to_string(),
            "-Command".to_string(),
            parent_command,
        ],
        cwd.as_path(),
        session_temp_env(codex_home.path()),
        Some(30_000),
        /*cancellation*/ Some(cancellation),
        /*use_private_desktop*/ true,
    )
    .expect("run legacy capture powershell with cancellation");

    assert!(
        started_at.elapsed() < Duration::from_secs(10),
        "cancellation should end capture before the timeout"
    );
    assert!(
        !result.timed_out,
        "cancellation should not be reported as a timeout"
    );
    assert_ne!(result.exit_code, 0);
    let descendant_process = descendant_process
        .lock()
        .expect("descendant process lock poisoned")
        .take()
        .expect("cancellation did not capture descendant process");
    wait_for_process_exit(&descendant_process, Duration::from_secs(10))
        .expect("sandbox descendant did not exit after cancellation");
    assert!(
        !descendant_marker.exists(),
        "sandbox descendant survived cancellation"
    );
}

#[derive(Clone, Copy, Debug)]
enum LegacyTtyDescendantLifecycle {
    Terminate,
    Preserve,
}

async fn assert_legacy_tty_descendant_lifecycle(
    pwsh: &Path,
    lifecycle: LegacyTtyDescendantLifecycle,
) {
    // #1906: a folder of its own, not the checkout: a legacy session's
    // grant on its workspace lasts.
    let workspace = user_owned_test_root();
    let cwd = workspace.path().to_path_buf();
    let codex_home = sandbox_home(match lifecycle {
        LegacyTtyDescendantLifecycle::Terminate => "legacy-tty-descendant-terminate",
        LegacyTtyDescendantLifecycle::Preserve => "legacy-tty-descendant-preserve",
    });
    // What the sandboxed commands write goes in the folder they may write
    // (#1890): the home is not one.
    let scratch = session_temp(codex_home.path());
    let ready_marker = scratch.join("descendant-started");
    let release_marker = scratch.join("release-descendant");
    let survival_marker = scratch.join("descendant-survived");
    let child_tail = match lifecycle {
        LegacyTtyDescendantLifecycle::Terminate => "Start-Sleep -Seconds 30".to_string(),
        LegacyTtyDescendantLifecycle::Preserve => format!(
            "$deadline=(Get-Date).AddSeconds(30); while (-not (Test-Path -LiteralPath '{}')) {{ if ((Get-Date) -ge $deadline) {{ exit 3 }}; Start-Sleep -Milliseconds 25 }}; Set-Content -LiteralPath '{}' -Value survived",
            powershell_literal(&release_marker),
            powershell_literal(&survival_marker),
        ),
    };
    let child_command = format!(
        "Set-Content -LiteralPath '{}' -Value $PID; {child_tail}",
        powershell_literal(&ready_marker),
    );
    let parent_tail = match lifecycle {
        LegacyTtyDescendantLifecycle::Terminate => "Start-Sleep -Seconds 30".to_string(),
        LegacyTtyDescendantLifecycle::Preserve => format!(
            "while (-not (Test-Path -LiteralPath '{}')) {{ Start-Sleep -Milliseconds 25 }}",
            powershell_literal(&ready_marker),
        ),
    };
    let parent_command = start_powershell_child(pwsh, &scratch, &child_command, &parent_tail);
    let permission_profile = PermissionProfile::workspace_write();
    let spawned = spawn_windows_sandbox_session_legacy(
        &permission_profile,
        workspace_roots_for(cwd.as_path()).as_slice(),
        codex_home.path(),
        vec![
            pwsh.display().to_string(),
            "-NoProfile".to_string(),
            "-Command".to_string(),
            parent_command,
        ],
        cwd.as_path(),
        session_temp_env(codex_home.path()),
        Some(30_000),
        &[],
        &[],
        /*tty*/ true,
        /*stdin_open*/ false,
        /*use_private_desktop*/ true,
    )
    .await
    .expect("spawn legacy sandbox ConPTY lifecycle test");
    assert!(
        wait_for_path(&ready_marker, Duration::from_secs(10)),
        "{lifecycle:?} descendant did not start"
    );
    // The file appears before its contents are written.
    let deadline = Instant::now() + Duration::from_secs(10);
    let descendant_pid: u32 = loop {
        let contents = fs::read_to_string(&ready_marker).unwrap_or_default();
        if let Ok(pid) = contents.trim().parse() {
            break pid;
        }
        assert!(
            Instant::now() < deadline,
            "descendant pid not written: {contents:?}"
        );
        std::thread::sleep(Duration::from_millis(25));
    };
    let descendant_process = open_process_for_wait(descendant_pid);

    if matches!(lifecycle, LegacyTtyDescendantLifecycle::Terminate) {
        spawned.session.request_terminate();
    }
    let (_, exit_code) =
        collect_stdout_and_exit(spawned, codex_home.path(), Duration::from_secs(15)).await;
    if matches!(lifecycle, LegacyTtyDescendantLifecycle::Preserve) {
        fs::write(&release_marker, "release").expect("release preserved descendant");
    }
    let descendant_process = descendant_process.expect("open sandbox ConPTY descendant");

    match lifecycle {
        LegacyTtyDescendantLifecycle::Terminate => assert_ne!(exit_code, 0),
        LegacyTtyDescendantLifecycle::Preserve => {
            assert_eq!(exit_code, 0);
            assert!(
                wait_for_path(&survival_marker, Duration::from_secs(10)),
                "sandbox ConPTY descendant did not survive normal exit"
            );
        }
    }
    wait_for_process_exit(&descendant_process, Duration::from_secs(10))
        .expect("sandbox ConPTY descendant did not exit");
}

#[test]
fn legacy_tty_job_terminates_and_preserves_descendants() {
    let Some(pwsh) = pwsh_path() else {
        eprintln!("skipping sandbox ConPTY lifecycle test: PowerShell 7 is not installed");
        return;
    };
    let _guard = legacy_process_test_guard();
    current_thread_runtime().block_on(async move {
        assert_legacy_tty_descendant_lifecycle(&pwsh, LegacyTtyDescendantLifecycle::Terminate)
            .await;
        assert_legacy_tty_descendant_lifecycle(&pwsh, LegacyTtyDescendantLifecycle::Preserve).await;
    });
}

#[test]
fn legacy_tty_powershell_emits_output_and_accepts_input() {
    let Some(pwsh) = pwsh_path() else {
        return;
    };
    let _guard = legacy_process_test_guard();
    let runtime = current_thread_runtime();
    runtime.block_on(async move {
        // #1906: a folder of its own, not the checkout: a legacy session's
        // grant on its workspace lasts.
        let workspace = user_owned_test_root();
        let cwd = workspace.path().to_path_buf();
        let codex_home = sandbox_home("legacy-tty-pwsh");
        println!("tty pwsh codex_home={}", codex_home.path().display());
        let permission_profile = PermissionProfile::workspace_write();
        let spawned = spawn_windows_sandbox_session_legacy(
            &permission_profile,
            workspace_roots_for(cwd.as_path()).as_slice(),
            codex_home.path(),
            vec![
                pwsh.display().to_string(),
                "-NoLogo".to_string(),
                "-NoProfile".to_string(),
                "-NoExit".to_string(),
                "-Command".to_string(),
                "$PID; Write-Output ready".to_string(),
            ],
            cwd.as_path(),
            session_temp_env(codex_home.path()),
            Some(10_000),
            &[],
            &[],
            /*tty*/ true,
            /*stdin_open*/ true,
            /*use_private_desktop*/ true,
        )
        .await
        .expect("spawn legacy tty powershell session");
        println!("tty pwsh spawn returned");

        let writer = spawned.session.writer_sender();
        writer
            .send(b"Write-Output second\n".to_vec())
            .await
            .expect("send second command");
        writer
            .send(b"exit\n".to_vec())
            .await
            .expect("send exit command");
        spawned.session.close_stdin();

        let (stdout, exit_code) =
            collect_stdout_and_exit(spawned, codex_home.path(), Duration::from_secs(15)).await;
        let stdout = String::from_utf8_lossy(&stdout);
        assert_eq!(exit_code, 0, "stdout={stdout:?}");
        assert!(stdout.contains("ready"), "stdout={stdout:?}");
        assert!(stdout.contains("second"), "stdout={stdout:?}");
    });
}

#[test]
#[ignore = "TODO: legacy ConPTY cmd.exe exits with STATUS_DLL_INIT_FAILED in CI"]
fn legacy_tty_cmd_emits_output_and_accepts_input() {
    let runtime = current_thread_runtime();
    runtime.block_on(async move {
        // #1906: a folder of its own, not the checkout: a legacy session's
        // grant on its workspace lasts.
        let workspace = user_owned_test_root();
        let cwd = workspace.path().to_path_buf();
        let codex_home = sandbox_home("legacy-tty-cmd");
        println!("tty cmd codex_home={}", codex_home.path().display());
        let permission_profile = PermissionProfile::workspace_write();
        let spawned = spawn_windows_sandbox_session_legacy(
            &permission_profile,
            workspace_roots_for(cwd.as_path()).as_slice(),
            codex_home.path(),
            vec![
                "C:\\Windows\\System32\\cmd.exe".to_string(),
                "/K".to_string(),
                "echo ready".to_string(),
            ],
            cwd.as_path(),
            session_temp_env(codex_home.path()),
            Some(10_000),
            &[],
            &[],
            /*tty*/ true,
            /*stdin_open*/ true,
            /*use_private_desktop*/ true,
        )
        .await
        .expect("spawn legacy tty cmd session");
        println!("tty cmd spawn returned");

        let writer = spawned.session.writer_sender();
        writer
            .send(b"echo second\n".to_vec())
            .await
            .expect("send second command");
        writer
            .send(b"exit\n".to_vec())
            .await
            .expect("send exit command");
        spawned.session.close_stdin();

        let (stdout, exit_code) =
            collect_stdout_and_exit(spawned, codex_home.path(), Duration::from_secs(15)).await;
        let stdout = String::from_utf8_lossy(&stdout);
        assert_eq!(exit_code, 0, "stdout={stdout:?}");
        assert!(stdout.contains("ready"), "stdout={stdout:?}");
        assert!(stdout.contains("second"), "stdout={stdout:?}");
    });
}

#[test]
#[ignore = "TODO: legacy ConPTY cmd.exe exits with STATUS_DLL_INIT_FAILED in CI"]
fn legacy_tty_cmd_default_desktop_emits_output_and_accepts_input() {
    let runtime = current_thread_runtime();
    runtime.block_on(async move {
        // #1906: a folder of its own, not the checkout: a legacy session's
        // grant on its workspace lasts.
        let workspace = user_owned_test_root();
        let cwd = workspace.path().to_path_buf();
        let codex_home = sandbox_home("legacy-tty-cmd-default-desktop");
        println!(
            "tty cmd default desktop codex_home={}",
            codex_home.path().display()
        );
        let permission_profile = PermissionProfile::workspace_write();
        let spawned = spawn_windows_sandbox_session_legacy(
            &permission_profile,
            workspace_roots_for(cwd.as_path()).as_slice(),
            codex_home.path(),
            vec![
                "C:\\Windows\\System32\\cmd.exe".to_string(),
                "/K".to_string(),
                "echo ready".to_string(),
            ],
            cwd.as_path(),
            session_temp_env(codex_home.path()),
            Some(10_000),
            &[],
            &[],
            /*tty*/ true,
            /*stdin_open*/ true,
            /*use_private_desktop*/ false,
        )
        .await
        .expect("spawn legacy tty cmd session");
        println!("tty cmd default desktop spawn returned");

        let writer = spawned.session.writer_sender();
        writer
            .send(b"echo second\n".to_vec())
            .await
            .expect("send second command");
        writer
            .send(b"exit\n".to_vec())
            .await
            .expect("send exit command");
        spawned.session.close_stdin();

        let (stdout, exit_code) =
            collect_stdout_and_exit(spawned, codex_home.path(), Duration::from_secs(15)).await;
        let stdout = String::from_utf8_lossy(&stdout);
        assert_eq!(exit_code, 0, "stdout={stdout:?}");
        assert!(stdout.contains("ready"), "stdout={stdout:?}");
        assert!(stdout.contains("second"), "stdout={stdout:?}");
    });
}

/// Sentinel credentials for #398, typed into a sandboxed command line the
/// way a model or user would: a bearer header, an API-key header, a
/// password option and an API-key query parameter.
const CREDENTIAL_SENTINELS: [&str; 4] = [
    "fake-398-bearer-1a2b3c4d5e",
    "fake-398-xapikey-6f7a8b9c",
    "fake-398-password-0d1e2f3a",
    "fake-398-query-4b5c6d7e",
];

fn credential_command_line() -> String {
    let [bearer, api_key, password, query] = CREDENTIAL_SENTINELS;
    format!(
        "echo SBX-398-RAN & rem curl -H \"Authorization: Bearer {bearer}\" -H \"X-Api-Key: {api_key}\" --password={password} https://api.example/v1?api_key={query}"
    )
}

fn assert_sandbox_log_redacted(codex_home: &Path, stdout: &str) {
    assert!(stdout.contains("SBX-398-RAN"), "stdout={stdout:?}");
    let log = sandbox_log(codex_home);
    assert!(
        log.lines()
            .any(|line| line.contains("START: ") && line.contains("Authorization: REDACTED")),
        "the command should be logged with the credential redacted:\n{log}"
    );
    for secret in CREDENTIAL_SENTINELS {
        assert!(
            !log.contains(secret),
            "{secret} reached the sandbox log:\n{log}"
        );
    }
}

/// #398: a sandboxed command's credentials never reach the sandbox command
/// log on the legacy (restricted token) path.
#[test]
fn legacy_command_log_redacts_credentials() {
    let _guard = legacy_process_test_guard();
    let runtime = current_thread_runtime();
    runtime.block_on(async move {
        // #1906: a folder of its own, not the checkout: a legacy session's
        // grant on its workspace lasts.
        let workspace = user_owned_test_root();
        let cwd = workspace.path().to_path_buf();
        let codex_home = sandbox_home("legacy-398-redact");
        let spawned = spawn_windows_sandbox_session_legacy(
            &PermissionProfile::workspace_write(),
            workspace_roots_for(cwd.as_path()).as_slice(),
            codex_home.path(),
            vec![
                system32_exe("cmd.exe").display().to_string(),
                "/d".to_string(),
                "/c".to_string(),
                credential_command_line(),
            ],
            cwd.as_path(),
            session_temp_env(codex_home.path()),
            Some(5_000),
            &[],
            &[],
            /*tty*/ false,
            /*stdin_open*/ false,
            /*use_private_desktop*/ true,
        )
        .await
        .expect("spawn legacy credential command");
        let (stdout, exit_code) =
            collect_stdout_and_exit(spawned, codex_home.path(), Duration::from_secs(10)).await;
        let stdout = String::from_utf8_lossy(&stdout);
        assert_eq!(exit_code, 0, "stdout={stdout:?}");
        assert_sandbox_log_redacted(codex_home.path(), &stdout);
    });
}

/// #1964: killing a legacy command at its time limit writes
/// `timed out after N ms` to the sandbox log. A command that exits inside
/// the limit does not.
#[test]
fn legacy_timeout_kill_logs_the_limit_and_a_normal_exit_does_not() {
    let _guard = legacy_process_test_guard();
    let runtime = current_thread_runtime();
    runtime.block_on(async move {
        // #1906: a folder of its own, not the checkout: a legacy session's
        // grant on its workspace lasts.
        let workspace = user_owned_test_root();
        let cwd = workspace.path().to_path_buf();
        let permission_profile = PermissionProfile::workspace_write();
        let roots = workspace_roots_for(cwd.as_path());

        let timeout_ms = 1_500u64;
        let timed_out_home = sandbox_home("legacy-timeout-log");
        let spawned = spawn_windows_sandbox_session_legacy(
            &permission_profile,
            roots.as_slice(),
            timed_out_home.path(),
            vec![
                system32_exe("WindowsPowerShell")
                    .join("v1.0")
                    .join("powershell.exe")
                    .display()
                    .to_string(),
                "-NoProfile".to_string(),
                "-NonInteractive".to_string(),
                "-Command".to_string(),
                "Start-Sleep -Seconds 30".to_string(),
            ],
            cwd.as_path(),
            session_temp_env(timed_out_home.path()),
            Some(timeout_ms),
            &[],
            &[],
            /*tty*/ false,
            /*stdin_open*/ false,
            /*use_private_desktop*/ true,
        )
        .await
        .expect("spawn legacy command that exceeds its limit");
        let (_stdout, exit_code) =
            collect_stdout_and_exit(spawned, timed_out_home.path(), Duration::from_secs(30)).await;
        let log = sandbox_log(timed_out_home.path());
        assert_eq!(
            exit_code, 1,
            "the time limit kills the command with exit code 1:\n{log}"
        );
        let logged_limit_ms = u32::try_from(timeout_ms).expect("limit fits the wait");
        assert!(
            log.contains(&format!("timed out after {logged_limit_ms} ms")),
            "sandbox log should record the limit that killed the command:\n{log}"
        );

        let normal_timeout_ms = 60_000u64;
        let normal_home = sandbox_home("legacy-timeout-log-ok");
        let spawned = spawn_windows_sandbox_session_legacy(
            &permission_profile,
            roots.as_slice(),
            normal_home.path(),
            vec![
                system32_exe("cmd.exe").display().to_string(),
                "/d".to_string(),
                "/c".to_string(),
                "echo LEGACY-TIMEOUT-LOG-OK".to_string(),
            ],
            cwd.as_path(),
            session_temp_env(normal_home.path()),
            Some(normal_timeout_ms),
            &[],
            &[],
            /*tty*/ false,
            /*stdin_open*/ false,
            /*use_private_desktop*/ true,
        )
        .await
        .expect("spawn legacy command that exits inside its limit");
        let (stdout, exit_code) =
            collect_stdout_and_exit(spawned, normal_home.path(), Duration::from_secs(90)).await;
        let stdout = String::from_utf8_lossy(&stdout);
        let log = sandbox_log(normal_home.path());
        assert_eq!(exit_code, 0, "stdout={stdout:?}\n{log}");
        assert!(
            stdout.contains("LEGACY-TIMEOUT-LOG-OK"),
            "stdout={stdout:?}\n{log}"
        );
        assert!(
            !log.contains("timed out after"),
            "a normal exit must not record a timeout:\n{log}"
        );
    });
}

/// #398: the same on the elevated (sandbox user) path, whose command
/// runner writes the log too.
#[test]
fn elevated_command_log_redacts_credentials() {
    let _guard = legacy_process_test_guard();
    let runtime = current_thread_runtime();
    runtime.block_on(async move {
        // #1906: a folder of its own, not the checkout: a legacy session's
        // grant on its workspace lasts.
        let workspace = user_owned_test_root();
        let cwd = workspace.path().to_path_buf();
        let codex_home = sandbox_home("elevated-398-redact");
        let spawned = spawn_windows_sandbox_session_elevated_for_permission_profile(
            &PermissionProfile::workspace_write(),
            workspace_roots_for(cwd.as_path()).as_slice(),
            codex_home.path(),
            vec![
                system32_exe("cmd.exe").display().to_string(),
                "/d".to_string(),
                "/c".to_string(),
                credential_command_line(),
            ],
            cwd.as_path(),
            HashMap::new(),
            /*proxy_enforced*/ false,
            /*network_proxy_restricting_sid*/ None,
            Some(5_000),
            /*read_roots_override*/ None,
            /*read_roots_include_platform_defaults*/ true,
            /*write_roots_override*/ None,
            &crate::DenyReadTargets::default(),
            &[],
            /*tty*/ false,
            /*stdin_open*/ false,
            /*use_private_desktop*/ true,
        )
        .await
        .expect("spawn elevated credential command");
        let (stdout, exit_code) =
            collect_stdout_and_exit(spawned, codex_home.path(), Duration::from_secs(10)).await;
        let stdout = String::from_utf8_lossy(&stdout);
        assert_eq!(exit_code, 0, "stdout={stdout:?}");
        assert_sandbox_log_redacted(codex_home.path(), &stdout);
    });
}

/// #398: and on the one-shot capture path (`run_windows_sandbox_capture`),
/// which logs START and SUCCESS itself.
#[test]
fn legacy_capture_command_log_redacts_credentials() {
    let _guard = legacy_process_test_guard();
    // #1906: a folder of its own, not the checkout: a legacy session's
    // grant on its workspace lasts.
    let workspace = user_owned_test_root();
    let cwd = workspace.path().to_path_buf();
    let codex_home = sandbox_home("capture-398-redact");
    let result = run_windows_sandbox_capture(
        &PermissionProfile::workspace_write(),
        workspace_roots_for(cwd.as_path()).as_slice(),
        codex_home.path(),
        vec![
            system32_exe("cmd.exe").display().to_string(),
            "/d".to_string(),
            "/c".to_string(),
            credential_command_line(),
        ],
        cwd.as_path(),
        session_temp_env(codex_home.path()),
        Some(10_000),
        /*cancellation*/ None,
        /*use_private_desktop*/ true,
    )
    .expect("run legacy capture credential command");
    let stdout = String::from_utf8_lossy(&result.stdout);
    assert_eq!(result.exit_code, 0, "stdout={stdout:?}");
    assert_sandbox_log_redacted(codex_home.path(), &stdout);
    assert!(
        sandbox_log(codex_home.path())
            .lines()
            .any(|line| line.contains("SUCCESS: ") && line.contains("--password=REDACTED")),
        "{}",
        sandbox_log(codex_home.path())
    );
}

/// #1507: other process-creation routes out of a legacy session.
#[path = "launch_path_tests.rs"]
mod launch_paths;

/// D259 (#1507): each legacy command's own identity in its default DACL.
#[path = "command_identity_tests.rs"]
mod command_identity;

/// #1770: a command's own token keeps it out of folders any user may write to.
#[path = "token_boundary_tests.rs"]
mod token_boundary;

/// #1805: a command's environment holds each name once, in any case.
#[path = "env_case_tests.rs"]
mod env_case;

/// D285: what the per-command ACL work costs on a large workspace.
#[path = "acl_cost_tests.rs"]
mod acl_cost;

/// D283 (#1541): nested `.codex`/`.agents` folders.
#[path = "nested_metadata_tests.rs"]
mod nested_metadata;

/// D420/D421 (#1775): repositories below the top of the writable roots.
#[path = "repo_watch_tests.rs"]
mod repo_watch;

/// D293 (#1542 items 1 and 2): links above explicit read-only paths.
#[path = "carve_out_link_tests.rs"]
mod carve_out_link;

/// #1668: cloud-file placeholder folders at or above read-only paths.
#[path = "cloud_placeholder_tests.rs"]
pub(crate) mod cloud_placeholder;

/// #1780: a read-only path and its writable root spelled one through a link.
#[path = "real_path_read_only_tests.rs"]
mod real_path_read_only;
