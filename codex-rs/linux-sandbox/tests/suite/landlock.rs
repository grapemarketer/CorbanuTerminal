#![cfg(target_os = "linux")]
#![allow(clippy::unwrap_used)]
use codex_core::exec::ExecCapturePolicy;
use codex_core::exec::ExecParams;
use codex_core::exec::process_exec_tool_call;
use codex_core::exec_env::create_env;
use codex_core::sandboxing::SandboxPermissions;
use codex_protocol::config_types::ShellEnvironmentPolicy;
use codex_protocol::config_types::WindowsSandboxLevel;
use codex_protocol::error::CodexErrorDetails;
use codex_protocol::error::Result;
use codex_protocol::error::SandboxErr;
use codex_protocol::models::PermissionProfile;
use codex_protocol::permissions::FileSystemAccessMode;
use codex_protocol::permissions::FileSystemPath;
use codex_protocol::permissions::FileSystemSandboxEntry;
use codex_protocol::permissions::FileSystemSandboxPolicy;
use codex_protocol::permissions::FileSystemSpecialPath;
use codex_protocol::permissions::NetworkSandboxPolicy;
use codex_utils_absolute_path::AbsolutePathBuf;
use pretty_assertions::assert_eq;
use std::collections::HashMap;
use std::path::Path;
use std::path::PathBuf;
use tempfile::NamedTempFile;

// At least on GitHub CI, the arm64 tests appear to need longer timeouts.

#[cfg(not(target_arch = "aarch64"))]
const SHORT_TIMEOUT_MS: u64 = 5_000;
#[cfg(target_arch = "aarch64")]
const SHORT_TIMEOUT_MS: u64 = 5_000;

#[cfg(not(target_arch = "aarch64"))]
const LONG_TIMEOUT_MS: u64 = 5_000;
#[cfg(target_arch = "aarch64")]
const LONG_TIMEOUT_MS: u64 = 5_000;

#[cfg(not(target_arch = "aarch64"))]
const NETWORK_TIMEOUT_MS: u64 = 10_000;
#[cfg(target_arch = "aarch64")]
const NETWORK_TIMEOUT_MS: u64 = 10_000;

const BWRAP_UNAVAILABLE_ERR: &str = "bubblewrap is unavailable:";

fn create_env_from_core_vars() -> HashMap<String, String> {
    let policy = ShellEnvironmentPolicy::default();
    create_env(&policy, /*thread_id*/ None)
}

fn codex_linux_sandbox_exe() -> PathBuf {
    let sandbox_program = PathBuf::from(env!("CARGO_BIN_EXE_codex-linux-sandbox"));
    match sandbox_program.canonicalize() {
        Ok(path) => path,
        Err(_) => sandbox_program,
    }
}

#[expect(clippy::print_stdout)]
async fn run_cmd(cmd: &[&str], writable_roots: &[PathBuf], timeout_ms: u64) {
    let output = run_cmd_output(cmd, writable_roots, timeout_ms).await;
    if output.exit_code != 0 {
        println!("stdout:\n{}", output.stdout.text);
        println!("stderr:\n{}", output.stderr.text);
        panic!("exit code: {}", output.exit_code);
    }
}

async fn run_cmd_output(
    cmd: &[&str],
    writable_roots: &[PathBuf],
    timeout_ms: u64,
) -> codex_protocol::exec_output::ExecToolCallOutput {
    run_cmd_result_with_writable_roots(
        cmd,
        writable_roots,
        timeout_ms,
        /*use_legacy_landlock*/ false,
        /*network_access*/ false,
    )
    .await
    .expect("sandboxed command should execute")
}

async fn run_cmd_result_with_writable_roots(
    cmd: &[&str],
    writable_roots: &[PathBuf],
    timeout_ms: u64,
    use_legacy_landlock: bool,
    network_access: bool,
) -> Result<codex_protocol::exec_output::ExecToolCallOutput> {
    let writable_roots = writable_roots
        .iter()
        .map(|path| AbsolutePathBuf::try_from(path.as_path()).unwrap())
        .collect::<Vec<_>>();
    let permission_profile = PermissionProfile::workspace_write_with(
        &writable_roots,
        if network_access {
            NetworkSandboxPolicy::Enabled
        } else {
            NetworkSandboxPolicy::Restricted
        },
        // Exclude tmp-related folders from writable roots because we need a
        // folder that is writable by tests but that we intentionally disallow
        // writing to in the sandbox.
        /*exclude_tmpdir_env_var*/
        true,
        /*exclude_slash_tmp*/ true,
    );
    run_cmd_result_with_permission_profile(cmd, permission_profile, timeout_ms, use_legacy_landlock)
        .await
}

async fn run_cmd_result_with_permission_profile(
    cmd: &[&str],
    permission_profile: PermissionProfile,
    timeout_ms: u64,
    use_legacy_landlock: bool,
) -> Result<codex_protocol::exec_output::ExecToolCallOutput> {
    let cwd = AbsolutePathBuf::current_dir().expect("cwd should exist");
    run_cmd_result_with_permission_profile_for_cwd(
        cmd,
        cwd,
        permission_profile,
        create_env_from_core_vars(),
        timeout_ms,
        use_legacy_landlock,
    )
    .await
}

async fn run_cmd_result_with_cwd_and_writable_roots(
    cmd: &[&str],
    cwd: &std::path::Path,
    writable_roots: &[PathBuf],
    timeout_ms: u64,
    use_legacy_landlock: bool,
    network_access: bool,
) -> Result<codex_protocol::exec_output::ExecToolCallOutput> {
    let writable_roots = writable_roots
        .iter()
        .map(|path| AbsolutePathBuf::try_from(path.as_path()).unwrap())
        .collect::<Vec<_>>();
    let permission_profile = PermissionProfile::workspace_write_with(
        &writable_roots,
        if network_access {
            NetworkSandboxPolicy::Enabled
        } else {
            NetworkSandboxPolicy::Restricted
        },
        /*exclude_tmpdir_env_var*/ true,
        /*exclude_slash_tmp*/ true,
    );
    let cwd = AbsolutePathBuf::try_from(cwd).expect("cwd should be absolute");
    run_cmd_result_with_permission_profile_for_cwd(
        cmd,
        cwd,
        permission_profile,
        create_env_from_core_vars(),
        timeout_ms,
        use_legacy_landlock,
    )
    .await
}

async fn run_cmd_result_with_permission_profile_for_cwd(
    cmd: &[&str],
    cwd: AbsolutePathBuf,
    permission_profile: PermissionProfile,
    env: HashMap<String, String>,
    timeout_ms: u64,
    use_legacy_landlock: bool,
) -> Result<codex_protocol::exec_output::ExecToolCallOutput> {
    let sandbox_cwd = cwd.clone();
    let params = ExecParams {
        command: cmd.iter().copied().map(str::to_owned).collect(),
        cwd,
        expiration: timeout_ms.into(),
        capture_policy: ExecCapturePolicy::ShellTool,
        env,
        network: None,
        network_environment_id: None,
        sandbox_permissions: SandboxPermissions::UseDefault,
        windows_sandbox_level: WindowsSandboxLevel::Disabled,
        windows_sandbox_private_desktop: false,
        justification: None,
        arg0: None,
    };
    let codex_linux_sandbox_exe = Some(codex_linux_sandbox_exe());

    process_exec_tool_call(
        params,
        &permission_profile,
        &sandbox_cwd,
        std::slice::from_ref(&sandbox_cwd),
        &codex_linux_sandbox_exe,
        use_legacy_landlock,
        /*stdout_stream*/ None,
    )
    .await
}

fn is_bwrap_unavailable_output(output: &codex_protocol::exec_output::ExecToolCallOutput) -> bool {
    output.stderr.text.contains(BWRAP_UNAVAILABLE_ERR)
        || (output
            .stderr
            .text
            .contains("Can't mount proc on /newroot/proc")
            && (output.stderr.text.contains("Operation not permitted")
                || output.stderr.text.contains("Permission denied")
                || output.stderr.text.contains("Invalid argument")))
}

async fn should_skip_bwrap_tests() -> bool {
    match run_cmd_result_with_writable_roots(
        &["bash", "-lc", "true"],
        &[],
        NETWORK_TIMEOUT_MS,
        /*use_legacy_landlock*/ false,
        /*network_access*/ true,
    )
    .await
    {
        Ok(output) => is_bwrap_unavailable_output(&output),
        Err(err) => match err.details() {
            CodexErrorDetails::Sandbox(SandboxErr::Denied { output, .. }) => {
                is_bwrap_unavailable_output(output)
            }
            // Probe timeouts are not actionable for the bwrap-specific assertions below;
            // skip rather than fail the whole suite.
            CodexErrorDetails::Sandbox(SandboxErr::Timeout { .. }) => true,
            details => panic!("bwrap availability probe failed unexpectedly: {details:?}"),
        },
    }
}

fn expect_denied(
    result: Result<codex_protocol::exec_output::ExecToolCallOutput>,
    context: &str,
) -> codex_protocol::exec_output::ExecToolCallOutput {
    match result {
        Ok(output) => {
            assert_ne!(output.exit_code, 0, "{context}: expected nonzero exit code");
            output
        }
        Err(err) => match err.details() {
            CodexErrorDetails::Sandbox(SandboxErr::Denied { output, .. }) => {
                output.as_ref().clone()
            }
            details => panic!("{context}: {details:?}"),
        },
    }
}

#[tokio::test]
async fn test_root_read() {
    run_cmd(&["ls", "-l", "/bin"], &[], SHORT_TIMEOUT_MS).await;
}

#[tokio::test]
#[should_panic]
async fn test_root_write() {
    let tmpfile = NamedTempFile::new().unwrap();
    let tmpfile_path = tmpfile.path().to_string_lossy();
    run_cmd(
        &["bash", "-lc", &format!("echo blah > {tmpfile_path}")],
        &[],
        SHORT_TIMEOUT_MS,
    )
    .await;
}

#[tokio::test]
async fn test_dev_null_write() {
    if should_skip_bwrap_tests().await {
        eprintln!("skipping bwrap test: bwrap sandbox prerequisites are unavailable");
        return;
    }

    let output = run_cmd_result_with_writable_roots(
        &["bash", "-lc", "echo blah > /dev/null"],
        &[],
        // We have seen timeouts when running this test in CI on GitHub,
        // so we are using a generous timeout until we can diagnose further.
        LONG_TIMEOUT_MS,
        /*use_legacy_landlock*/ false,
        /*network_access*/ true,
    )
    .await
    .expect("sandboxed command should execute");

    assert_eq!(output.exit_code, 0);
}

#[tokio::test]
async fn bwrap_populates_minimal_dev_nodes() {
    if should_skip_bwrap_tests().await {
        eprintln!("skipping bwrap test: bwrap sandbox prerequisites are unavailable");
        return;
    }

    let output = run_cmd_result_with_writable_roots(
        &[
            "bash",
            "-lc",
            "for node in null zero full random urandom tty; do [ -c \"/dev/$node\" ] || { echo \"missing /dev/$node\" >&2; exit 1; }; done",
        ],
        &[],
        LONG_TIMEOUT_MS,
        /*use_legacy_landlock*/ false,
        /*network_access*/ true,
    )
    .await
    .expect("sandboxed command should execute");

    assert_eq!(output.exit_code, 0);
}

#[tokio::test]
async fn bwrap_preserves_writable_dev_shm_bind_mount() {
    if should_skip_bwrap_tests().await {
        eprintln!("skipping bwrap test: bwrap sandbox prerequisites are unavailable");
        return;
    }
    if !std::path::Path::new("/dev/shm").exists() {
        eprintln!("skipping bwrap test: /dev/shm is unavailable in this environment");
        return;
    }

    let target_file = match NamedTempFile::new_in("/dev/shm") {
        Ok(file) => file,
        Err(err) => {
            eprintln!("skipping bwrap test: failed to create /dev/shm temp file: {err}");
            return;
        }
    };
    let target_path = target_file.path().to_path_buf();
    std::fs::write(&target_path, "host-before").expect("seed /dev/shm file");

    let output = run_cmd_result_with_writable_roots(
        &[
            "bash",
            "-lc",
            &format!("printf sandbox-after > {}", target_path.to_string_lossy()),
        ],
        &[PathBuf::from("/dev/shm")],
        LONG_TIMEOUT_MS,
        /*use_legacy_landlock*/ false,
        /*network_access*/ true,
    )
    .await
    .expect("sandboxed command should execute");

    assert_eq!(output.exit_code, 0);
    assert_eq!(
        std::fs::read_to_string(&target_path).expect("read /dev/shm file"),
        "sandbox-after"
    );
}

#[tokio::test]
async fn test_writable_root() {
    let tmpdir = tempfile::tempdir().unwrap();
    let file_path = tmpdir.path().join("test");
    run_cmd(
        &[
            "bash",
            "-lc",
            &format!("echo blah > {}", file_path.to_string_lossy()),
        ],
        &[tmpdir.path().to_path_buf()],
        // We have seen timeouts when running this test in CI on GitHub,
        // so we are using a generous timeout until we can diagnose further.
        LONG_TIMEOUT_MS,
    )
    .await;
}

#[tokio::test]
async fn sandbox_ignores_missing_writable_roots_under_bwrap() {
    if should_skip_bwrap_tests().await {
        eprintln!("skipping bwrap test: bwrap sandbox prerequisites are unavailable");
        return;
    }

    let tempdir = tempfile::tempdir().expect("tempdir");
    let existing_root = tempdir.path().join("existing");
    let missing_root = tempdir.path().join("missing");
    std::fs::create_dir(&existing_root).expect("create existing root");

    let output = run_cmd_result_with_writable_roots(
        &["bash", "-c", "printf sandbox-ok"],
        &[existing_root, missing_root],
        LONG_TIMEOUT_MS,
        /*use_legacy_landlock*/ false,
        /*network_access*/ true,
    )
    .await
    .expect("sandboxed command should execute");

    assert_eq!(output.exit_code, 0);
    assert_eq!(output.stdout.text, "sandbox-ok");
}

#[tokio::test]
async fn test_no_new_privs_is_enabled() {
    let output = run_cmd_output(
        &["bash", "-lc", "grep '^NoNewPrivs:' /proc/self/status"],
        &[],
        // We have seen timeouts when running this test in CI on GitHub,
        // so we are using a generous timeout until we can diagnose further.
        LONG_TIMEOUT_MS,
    )
    .await;
    let line = output
        .stdout
        .text
        .lines()
        .find(|line| line.starts_with("NoNewPrivs:"))
        .unwrap_or("");
    assert_eq!(line.trim(), "NoNewPrivs:\t1");
}

#[tokio::test]
#[should_panic(expected = "Sandbox(Timeout")]
async fn test_timeout() {
    run_cmd(&["sleep", "2"], &[], /*timeout_ms*/ 50).await;
}

/// Helper that runs `cmd` under the Linux sandbox and asserts that the command
/// does NOT succeed (i.e. returns a non‑zero exit code) **unless** the binary
/// is missing in which case we silently treat it as an accepted skip so the
/// suite remains green on leaner CI images.
async fn assert_network_blocked(cmd: &[&str]) {
    let cwd = AbsolutePathBuf::current_dir().expect("cwd should exist");
    let sandbox_cwd = cwd.clone();
    let params = ExecParams {
        command: cmd.iter().copied().map(str::to_owned).collect(),
        cwd,
        // Give the tool a generous 2-second timeout so even slow DNS timeouts
        // do not stall the suite.
        expiration: NETWORK_TIMEOUT_MS.into(),
        capture_policy: ExecCapturePolicy::ShellTool,
        env: create_env_from_core_vars(),
        network: None,
        network_environment_id: None,
        sandbox_permissions: SandboxPermissions::UseDefault,
        windows_sandbox_level: WindowsSandboxLevel::Disabled,
        windows_sandbox_private_desktop: false,
        justification: None,
        arg0: None,
    };

    let codex_linux_sandbox_exe: Option<PathBuf> = Some(codex_linux_sandbox_exe());
    let permission_profile = PermissionProfile::read_only();
    let result = process_exec_tool_call(
        params,
        &permission_profile,
        &sandbox_cwd,
        std::slice::from_ref(&sandbox_cwd),
        &codex_linux_sandbox_exe,
        /*use_legacy_landlock*/ false,
        /*stdout_stream*/ None,
    )
    .await;

    let output = match result {
        Ok(output) => output,
        Err(err) => match err.details() {
            CodexErrorDetails::Sandbox(SandboxErr::Denied { output, .. }) => {
                output.as_ref().clone()
            }
            details => panic!("expected sandbox denied error, got: {details:?}"),
        },
    };

    dbg!(&output.stderr.text);
    dbg!(&output.stdout.text);
    dbg!(&output.exit_code);

    // A completely missing binary exits with 127.  Anything else should also
    // be non‑zero (EPERM from seccomp will usually bubble up as 1, 2, 13…)
    // If—*and only if*—the command exits 0 we consider the sandbox breached.

    if output.exit_code == 0 {
        panic!(
            "Network sandbox FAILED - {cmd:?} exited 0\nstdout:\n{}\nstderr:\n{}",
            output.stdout.text, output.stderr.text
        );
    }
}

#[tokio::test]
async fn sandbox_blocks_curl() {
    assert_network_blocked(&["curl", "-I", "http://openai.com"]).await;
}

#[tokio::test]
async fn sandbox_blocks_wget() {
    assert_network_blocked(&["wget", "-qO-", "http://openai.com"]).await;
}

#[tokio::test]
async fn sandbox_blocks_ping() {
    // ICMP requires raw socket – should be denied quickly with EPERM.
    assert_network_blocked(&["ping", "-c", "1", "8.8.8.8"]).await;
}

#[tokio::test]
async fn sandbox_blocks_nc() {
    // Zero‑length connection attempt to localhost.
    assert_network_blocked(&["nc", "-z", "127.0.0.1", "80"]).await;
}

#[tokio::test]
async fn sandbox_blocks_git_and_codex_writes_inside_writable_root() {
    if should_skip_bwrap_tests().await {
        eprintln!("skipping bwrap test: bwrap sandbox prerequisites are unavailable");
        return;
    }

    let tmpdir = tempfile::tempdir().expect("tempdir");
    let dot_git = tmpdir.path().join(".git");
    let dot_codex = tmpdir.path().join(".codex");
    std::fs::create_dir_all(&dot_git).expect("create .git");
    std::fs::create_dir_all(&dot_codex).expect("create .codex");

    let git_target = dot_git.join("config");
    let codex_target = dot_codex.join("config.toml");

    let git_output = expect_denied(
        run_cmd_result_with_writable_roots(
            &[
                "bash",
                "-lc",
                &format!("echo denied > {}", git_target.to_string_lossy()),
            ],
            &[tmpdir.path().to_path_buf()],
            LONG_TIMEOUT_MS,
            /*use_legacy_landlock*/ false,
            /*network_access*/ true,
        )
        .await,
        ".git write should be denied under bubblewrap",
    );

    let codex_output = expect_denied(
        run_cmd_result_with_writable_roots(
            &[
                "bash",
                "-lc",
                &format!("echo denied > {}", codex_target.to_string_lossy()),
            ],
            &[tmpdir.path().to_path_buf()],
            LONG_TIMEOUT_MS,
            /*use_legacy_landlock*/ false,
            /*network_access*/ true,
        )
        .await,
        ".codex write should be denied under bubblewrap",
    );
    assert_ne!(git_output.exit_code, 0);
    assert_ne!(codex_output.exit_code, 0);
}

#[tokio::test]
async fn sandbox_blocks_codex_symlink_replacement_attack() {
    if should_skip_bwrap_tests().await {
        eprintln!("skipping bwrap test: bwrap sandbox prerequisites are unavailable");
        return;
    }

    use std::os::unix::fs::symlink;

    let tmpdir = tempfile::tempdir().expect("tempdir");
    let decoy = tmpdir.path().join("decoy-codex");
    std::fs::create_dir_all(&decoy).expect("create decoy dir");

    let dot_codex = tmpdir.path().join(".codex");
    symlink(&decoy, &dot_codex).expect("create .codex symlink");

    let codex_target = dot_codex.join("config.toml");

    let codex_output = expect_denied(
        run_cmd_result_with_writable_roots(
            &[
                "bash",
                "-lc",
                &format!("echo denied > {}", codex_target.to_string_lossy()),
            ],
            &[tmpdir.path().to_path_buf()],
            LONG_TIMEOUT_MS,
            /*use_legacy_landlock*/ false,
            /*network_access*/ true,
        )
        .await,
        ".codex symlink replacement should be denied",
    );
    assert_ne!(codex_output.exit_code, 0);
}

#[tokio::test]
async fn sandbox_reports_codex_symlink_build_failure_without_panicking() {
    if should_skip_bwrap_tests().await {
        eprintln!("skipping bwrap test: bwrap sandbox prerequisites are unavailable");
        return;
    }

    use std::os::unix::fs::symlink;

    let tmpdir = tempfile::tempdir().expect("tempdir");
    let decoy = tmpdir.path().join("decoy-codex");
    std::fs::create_dir_all(&decoy).expect("create decoy dir");

    let dot_codex = tmpdir.path().join(".codex");
    symlink(&decoy, &dot_codex).expect("create .codex symlink");

    let output = match run_cmd_result_with_writable_roots(
        &["bash", "-lc", "true"],
        &[tmpdir.path().to_path_buf()],
        LONG_TIMEOUT_MS,
        /*use_legacy_landlock*/ false,
        /*network_access*/ true,
    )
    .await
    {
        Err(err) => match err.details() {
            CodexErrorDetails::Sandbox(SandboxErr::Denied { output, .. }) => {
                output.as_ref().clone()
            }
            details => panic!(".codex symlink build failure should deny: {details:?}"),
        },
        Ok(output) => panic!(".codex symlink build failure should deny: {output:?}"),
    };

    assert_eq!(output.exit_code, 1);
    assert!(
        output
            .stderr
            .text
            .contains("error building bubblewrap command:"),
        "stderr: {}",
        output.stderr.text
    );
    assert!(
        output
            .stderr
            .text
            .contains("cannot enforce sandbox read-only path"),
        "stderr: {}",
        output.stderr.text
    );
    assert!(
        !output.stderr.text.contains("panicked at"),
        "stderr: {}",
        output.stderr.text
    );
}

#[tokio::test]
async fn sandbox_rejects_symlinked_synthetic_mount_registry() {
    if should_skip_bwrap_tests().await {
        eprintln!("skipping bwrap test: bwrap sandbox prerequisites are unavailable");
        return;
    }

    let temp = tempfile::tempdir().expect("tempdir");
    let workspace = temp.path().join("workspace");
    let registry_target = workspace.join("registry");
    std::fs::create_dir_all(&registry_target).expect("create registry target");
    let effective_uid = unsafe { libc::geteuid() };
    let registry = temp.path().join(format!(
        "codex-bwrap-synthetic-mount-targets-{effective_uid}"
    ));
    std::os::unix::fs::symlink(&registry_target, &registry).expect("symlink registry");

    let cwd = AbsolutePathBuf::try_from(workspace).expect("absolute workspace");
    let permission_profile = PermissionProfile::workspace_write_with(
        std::slice::from_ref(&cwd),
        NetworkSandboxPolicy::Enabled,
        /*exclude_tmpdir_env_var*/ true,
        /*exclude_slash_tmp*/ true,
    );
    let mut env = create_env_from_core_vars();
    env.insert("TMPDIR".to_string(), temp.path().display().to_string());
    let output = expect_denied(
        run_cmd_result_with_permission_profile_for_cwd(
            &["sh", "-c", "touch registry/forged-marker"],
            cwd,
            permission_profile,
            env,
            LONG_TIMEOUT_MS,
            /*use_legacy_landlock*/ false,
        )
        .await,
        "a symlinked registry must not expose writable bookkeeping",
    );
    assert!(
        output
            .stderr
            .text
            .contains("synthetic mount registry must not be a symlink"),
        "stderr: {}",
        output.stderr.text
    );
    assert_eq!(
        std::fs::read_dir(registry_target)
            .expect("read registry target")
            .count(),
        0,
        "the registry symlink must be rejected before registration or command execution"
    );
}

#[tokio::test]
async fn sandbox_keeps_parent_repo_discovery_while_blocking_child_metadata() {
    if should_skip_bwrap_tests().await {
        eprintln!("skipping bwrap test: bwrap sandbox prerequisites are unavailable");
        return;
    }

    let git_available = std::process::Command::new("git")
        .arg("--version")
        .status()
        .is_ok_and(|status| status.success());
    let python_available = std::process::Command::new("python3")
        .arg("--version")
        .status()
        .is_ok_and(|status| status.success());
    if !git_available || !python_available {
        eprintln!("skipping bwrap test: git or python3 is unavailable");
        return;
    }

    let tmpdir = tempfile::tempdir().expect("tempdir");
    let repo = tmpdir.path().join("repo");
    let subdir = repo.join("sub");
    let real_tmp = tmpdir.path().join("real-tmp");
    let redirected_tmp = tmpdir.path().join("redirected-tmp");
    let tmp_alias = tmpdir.path().join("tmp-alias");
    std::fs::create_dir(&real_tmp).expect("create real temp directory");
    std::fs::create_dir(&redirected_tmp).expect("create redirected temp directory");
    std::os::unix::fs::symlink(&real_tmp, &tmp_alias).expect("create temp directory alias");
    std::fs::create_dir_all(&subdir).expect("create nested workspace");
    assert!(
        std::process::Command::new("git")
            .arg("init")
            .arg("-q")
            .arg(&repo)
            .status()
            .expect("git init should run")
            .success(),
        "git init should create parent repo"
    );

    let repo = repo.to_string_lossy();
    let redirected_tmp = redirected_tmp.to_string_lossy();
    let script = format!(
        r#"set -e
test "$(git rev-parse --show-toplevel)" = '{repo}'
touch "$TMPDIR/writable-sibling"
registry="${{TMPDIR:-/tmp}}/codex-bwrap-synthetic-mount-targets-$(id -u)"
if touch "$registry/forged-marker" 2>/dev/null; then
  exit 22
fi
redirected_registry='{redirected_tmp}'/codex-bwrap-synthetic-mount-targets-$(id -u)
for marker_dir in "$registry"/*; do
  [ -d "$marker_dir" ] || continue
  mkdir -p "$redirected_registry/${{marker_dir##*/}}"
  touch "$redirected_registry/${{marker_dir##*/}}/1"
done
ln -sfn '{redirected_tmp}' "$TMPDIR"
git status --short > status.before
if grep -E '(^|[[:space:]])\.(git|codex|agents)(/|$)' status.before; then
  cat status.before
  exit 21
fi
"#,
    );

    let cwd = AbsolutePathBuf::try_from(subdir.as_path()).expect("cwd should be absolute");
    let permission_profile = PermissionProfile::workspace_write_with(
        std::slice::from_ref(&cwd),
        NetworkSandboxPolicy::Enabled,
        /*exclude_tmpdir_env_var*/ false,
        /*exclude_slash_tmp*/ false,
    );
    let mut env = create_env_from_core_vars();
    env.insert("TMPDIR".to_string(), tmp_alias.display().to_string());
    let output = run_cmd_result_with_permission_profile_for_cwd(
        &["bash", "-lc", &script],
        cwd,
        permission_profile,
        env,
        LONG_TIMEOUT_MS,
        /*use_legacy_landlock*/ false,
    )
    .await
    .expect("sandboxed command should execute");

    assert_eq!(
        output.exit_code, 0,
        "stdout:\n{}\nstderr:\n{}",
        output.stdout.text, output.stderr.text
    );
    assert!(!subdir.join(".git").exists());

    let git_init_output = expect_denied(
        run_cmd_result_with_cwd_and_writable_roots(
            &["git", "init", "-q"],
            &subdir,
            std::slice::from_ref(&subdir),
            LONG_TIMEOUT_MS,
            /*use_legacy_landlock*/ false,
            /*network_access*/ true,
        )
        .await,
        "child git init should be denied",
    );
    assert_ne!(git_init_output.exit_code, 0);
    assert!(!subdir.join(".git").exists());

    let mkdir_codex_output = expect_denied(
        run_cmd_result_with_cwd_and_writable_roots(
            &["mkdir", ".codex"],
            &subdir,
            std::slice::from_ref(&subdir),
            LONG_TIMEOUT_MS,
            /*use_legacy_landlock*/ false,
            /*network_access*/ true,
        )
        .await,
        "child .codex directory creation should be denied",
    );
    assert_ne!(mkdir_codex_output.exit_code, 0);
    assert!(!subdir.join(".codex").exists());

    let script = format!(
        r#"set -e
test "$(git rev-parse --show-toplevel)" = '{repo}'
printf '%s\n' 'import json, sys' 'for line in sys.stdin:' '    obj = json.loads(line)' '    print(obj.get("message", obj))' > jsonl_viewer.py
printf '%s\n' '{{"message":"ok"}}' | python3 jsonl_viewer.py | grep -q ok
"#,
    );
    let output = run_cmd_result_with_cwd_and_writable_roots(
        &["bash", "-lc", &script],
        &subdir,
        std::slice::from_ref(&subdir),
        LONG_TIMEOUT_MS,
        /*use_legacy_landlock*/ false,
        /*network_access*/ true,
    )
    .await
    .expect("sandboxed command should execute");
    assert_eq!(
        output.exit_code, 0,
        "stdout:\n{}\nstderr:\n{}",
        output.stdout.text, output.stderr.text
    );

    assert!(subdir.join("jsonl_viewer.py").is_file());
    assert!(!subdir.join(".git").exists());
    assert!(!subdir.join(".codex").exists());
    assert!(!subdir.join(".agents").exists());
}

#[tokio::test]
async fn sandbox_blocks_explicit_split_policy_carveouts_under_bwrap() {
    if should_skip_bwrap_tests().await {
        eprintln!("skipping bwrap test: bwrap sandbox prerequisites are unavailable");
        return;
    }

    let tmpdir = tempfile::tempdir().expect("tempdir");
    let blocked = tmpdir.path().join("blocked");
    std::fs::create_dir_all(&blocked).expect("create blocked dir");
    let blocked_target = blocked.join("secret.txt");
    // These tests bypass the usual legacy-policy bridge, so explicitly keep
    // the sandbox helper binary and minimal runtime paths readable.
    let sandbox_helper_dir = codex_linux_sandbox_exe()
        .parent()
        .expect("sandbox helper should have a parent")
        .to_path_buf();

    let file_system_sandbox_policy = FileSystemSandboxPolicy::restricted(vec![
        FileSystemSandboxEntry {
            path: FileSystemPath::Special {
                value: FileSystemSpecialPath::Minimal,
            },
            access: FileSystemAccessMode::Read,
            missing_path_behavior: None,
        },
        FileSystemSandboxEntry {
            path: FileSystemPath::Path {
                path: AbsolutePathBuf::try_from(sandbox_helper_dir.as_path())
                    .expect("absolute helper dir"),
            },
            access: FileSystemAccessMode::Read,
            missing_path_behavior: None,
        },
        FileSystemSandboxEntry {
            path: FileSystemPath::Path {
                path: AbsolutePathBuf::try_from(tmpdir.path()).expect("absolute tempdir"),
            },
            access: FileSystemAccessMode::Write,
            missing_path_behavior: None,
        },
        FileSystemSandboxEntry {
            path: FileSystemPath::Path {
                path: AbsolutePathBuf::try_from(blocked.as_path()).expect("absolute blocked dir"),
            },
            access: FileSystemAccessMode::Deny,
            missing_path_behavior: None,
        },
    ]);
    let permission_profile = PermissionProfile::from_runtime_permissions(
        &file_system_sandbox_policy,
        NetworkSandboxPolicy::Enabled,
    );
    let output = expect_denied(
        run_cmd_result_with_permission_profile(
            &[
                "bash",
                "-lc",
                &format!("echo denied > {}", blocked_target.to_string_lossy()),
            ],
            permission_profile,
            LONG_TIMEOUT_MS,
            /*use_legacy_landlock*/ false,
        )
        .await,
        "explicit split-policy carveout should be denied under bubblewrap",
    );

    assert_ne!(output.exit_code, 0);
}

#[tokio::test]
async fn sandbox_starts_with_denied_tmp_without_exposing_registry() {
    if should_skip_bwrap_tests().await {
        eprintln!("skipping bwrap test: bwrap sandbox prerequisites are unavailable");
        return;
    }

    let temp = tempfile::tempdir().expect("tempdir");
    std::fs::write(temp.path().join("AGENTS.md"), "project instructions\n")
        .expect("write instructions");
    let cwd = AbsolutePathBuf::try_from(temp.path()).expect("absolute workspace");
    let sandbox_helper = codex_linux_sandbox_exe();
    let helper_dir = AbsolutePathBuf::try_from(sandbox_helper.parent().expect("helper parent"))
        .expect("absolute helper directory");

    for tmp_root in [PathBuf::from("/tmp"), temp.path().join("denied-tmp")] {
        std::fs::create_dir_all(&tmp_root).expect("create temp root");
        let secret = NamedTempFile::new_in(&tmp_root).expect("denied file");
        std::fs::write(secret.path(), "private").expect("write denied file");
        for read_root in [FileSystemSpecialPath::Root, FileSystemSpecialPath::Minimal] {
            let denied_path = if tmp_root == std::path::Path::new("/tmp") {
                FileSystemPath::Special {
                    value: FileSystemSpecialPath::SlashTmp,
                }
            } else {
                FileSystemPath::Path {
                    path: AbsolutePathBuf::try_from(tmp_root.as_path())
                        .expect("absolute temp root"),
                }
            };
            let policy = FileSystemSandboxPolicy::restricted(vec![
                FileSystemSandboxEntry::new(
                    FileSystemPath::Special { value: read_root },
                    FileSystemAccessMode::Read,
                ),
                FileSystemSandboxEntry::new(
                    FileSystemPath::Path {
                        path: helper_dir.clone(),
                    },
                    FileSystemAccessMode::Read,
                ),
                FileSystemSandboxEntry::new(
                    FileSystemPath::Path { path: cwd.clone() },
                    FileSystemAccessMode::Write,
                ),
                FileSystemSandboxEntry::new(denied_path, FileSystemAccessMode::Deny),
            ]);
            let mut env = create_env_from_core_vars();
            env.insert("TMPDIR".to_string(), tmp_root.display().to_string());
            env.insert(
                "DENIED_SECRET".to_string(),
                secret.path().display().to_string(),
            );
            let output = run_cmd_result_with_permission_profile_for_cwd(
                &[
                    "sh",
                    "-c",
                    r#"set -e
cat AGENTS.md
test ! -r "$DENIED_SECRET"
if printf modified > "$DENIED_SECRET" 2>/dev/null; then exit 1; fi
registry="$TMPDIR/codex-bwrap-synthetic-mount-targets-$(id -u)"
test ! -e "$registry"
"#,
                ],
                cwd.clone(),
                PermissionProfile::from_runtime_permissions(&policy, NetworkSandboxPolicy::Enabled),
                env,
                LONG_TIMEOUT_MS,
                /*use_legacy_landlock*/ false,
            )
            .await
            .expect("sandbox should start with denied temp directory");
            assert_eq!(
                (output.exit_code, output.stdout.text.as_str()),
                (0, "project instructions\n"),
                "stderr: {}",
                output.stderr.text
            );
            assert_eq!(std::fs::read_to_string(secret.path()).unwrap(), "private");
            assert!(!temp.path().join(".git").exists());
        }
    }
}

#[tokio::test]
async fn sandbox_reenables_writable_subpaths_under_unreadable_parents() {
    if should_skip_bwrap_tests().await {
        eprintln!("skipping bwrap test: bwrap sandbox prerequisites are unavailable");
        return;
    }

    let tmpdir = tempfile::tempdir().expect("tempdir");
    let blocked = tmpdir.path().join("blocked");
    let allowed = blocked.join("allowed");
    std::fs::create_dir_all(&allowed).expect("create blocked/allowed dir");
    let allowed_target = allowed.join("note.txt");
    // These tests bypass the usual legacy-policy bridge, so explicitly keep
    // the sandbox helper binary and minimal runtime paths readable.
    let sandbox_helper_dir = codex_linux_sandbox_exe()
        .parent()
        .expect("sandbox helper should have a parent")
        .to_path_buf();

    let file_system_sandbox_policy = FileSystemSandboxPolicy::restricted(vec![
        FileSystemSandboxEntry {
            path: FileSystemPath::Special {
                value: FileSystemSpecialPath::Minimal,
            },
            access: FileSystemAccessMode::Read,
            missing_path_behavior: None,
        },
        FileSystemSandboxEntry {
            path: FileSystemPath::Path {
                path: AbsolutePathBuf::try_from(sandbox_helper_dir.as_path())
                    .expect("absolute helper dir"),
            },
            access: FileSystemAccessMode::Read,
            missing_path_behavior: None,
        },
        FileSystemSandboxEntry {
            path: FileSystemPath::Path {
                path: AbsolutePathBuf::try_from(tmpdir.path()).expect("absolute tempdir"),
            },
            access: FileSystemAccessMode::Write,
            missing_path_behavior: None,
        },
        FileSystemSandboxEntry {
            path: FileSystemPath::Path {
                path: AbsolutePathBuf::try_from(blocked.as_path()).expect("absolute blocked dir"),
            },
            access: FileSystemAccessMode::Deny,
            missing_path_behavior: None,
        },
        FileSystemSandboxEntry {
            path: FileSystemPath::Path {
                path: AbsolutePathBuf::try_from(allowed.as_path()).expect("absolute allowed dir"),
            },
            access: FileSystemAccessMode::Write,
            missing_path_behavior: None,
        },
    ]);
    let permission_profile = PermissionProfile::from_runtime_permissions(
        &file_system_sandbox_policy,
        NetworkSandboxPolicy::Enabled,
    );
    let output = run_cmd_result_with_permission_profile(
        &[
            "bash",
            "-lc",
            &format!(
                "printf allowed > {} && cat {}",
                allowed_target.to_string_lossy(),
                allowed_target.to_string_lossy()
            ),
        ],
        permission_profile,
        LONG_TIMEOUT_MS,
        /*use_legacy_landlock*/ false,
    )
    .await
    .expect("nested writable carveout should execute under bubblewrap");

    assert_eq!(output.exit_code, 0);
    assert_eq!(output.stdout.text.trim(), "allowed");
}

#[tokio::test]
async fn sandbox_blocks_root_read_carveouts_under_bwrap() {
    if should_skip_bwrap_tests().await {
        eprintln!("skipping bwrap test: bwrap sandbox prerequisites are unavailable");
        return;
    }

    let tmpdir = tempfile::tempdir().expect("tempdir");
    let blocked = tmpdir.path().join("blocked");
    std::fs::create_dir_all(&blocked).expect("create blocked dir");
    let blocked_target = blocked.join("secret.txt");
    std::fs::write(&blocked_target, "secret").expect("seed blocked file");

    let file_system_sandbox_policy = FileSystemSandboxPolicy::restricted(vec![
        FileSystemSandboxEntry {
            path: FileSystemPath::Special {
                value: FileSystemSpecialPath::Root,
            },
            access: FileSystemAccessMode::Read,
            missing_path_behavior: None,
        },
        FileSystemSandboxEntry {
            path: FileSystemPath::Path {
                path: AbsolutePathBuf::try_from(blocked.as_path()).expect("absolute blocked dir"),
            },
            access: FileSystemAccessMode::Deny,
            missing_path_behavior: None,
        },
    ]);
    let permission_profile = PermissionProfile::from_runtime_permissions(
        &file_system_sandbox_policy,
        NetworkSandboxPolicy::Enabled,
    );
    let output = expect_denied(
        run_cmd_result_with_permission_profile(
            &[
                "bash",
                "-lc",
                &format!("cat {}", blocked_target.to_string_lossy()),
            ],
            permission_profile,
            LONG_TIMEOUT_MS,
            /*use_legacy_landlock*/ false,
        )
        .await,
        "root-read carveout should be denied under bubblewrap",
    );

    assert_ne!(output.exit_code, 0);
}

#[tokio::test]
async fn sandbox_blocks_ssh() {
    // Force ssh to attempt a real TCP connection but fail quickly.  `BatchMode`
    // avoids password prompts, and `ConnectTimeout` keeps the hang time low.
    assert_network_blocked(&[
        "ssh",
        "-o",
        "BatchMode=yes",
        "-o",
        "ConnectTimeout=1",
        "github.com",
    ])
    .await;
}

#[tokio::test]
async fn sandbox_blocks_getent() {
    assert_network_blocked(&["getent", "ahosts", "openai.com"]).await;
}

#[tokio::test]
async fn sandbox_blocks_dev_tcp_redirection() {
    // This syntax is only supported by bash and zsh. We try bash first.
    // Fallback generic socket attempt using /bin/sh with bash‑style /dev/tcp.  Not
    // all images ship bash, so we guard against 127 as well.
    assert_network_blocked(&["bash", "-c", "echo hi > /dev/tcp/127.0.0.1/80"]).await;
}

/// PF-23-S02 on Linux, full-write profiles: what the protected-path rules
/// hold (a read-only file stays read-only, also through a symlinked dotfile;
/// since #587 its folder cannot be renamed either, as on macOS:
/// `seatbelt_tests::renaming_a_folder_above_a_protected_path_is_denied`)
/// and the limits the gate records (the link itself can be removed, and a
/// new file created next to the protected one).
#[tokio::test]
async fn pf_23_s02_full_write_profile_keeps_protected_files_read_only() {
    if should_skip_bwrap_tests().await {
        eprintln!("skipping bwrap test: bwrap sandbox prerequisites are unavailable");
        return;
    }
    let dir = tempfile::tempdir().expect("tempdir");
    let base = dir.path().canonicalize().expect("canonical");
    let config = base.join("config");
    std::fs::create_dir_all(config.join("fish")).expect("fish");
    std::fs::write(config.join("fish/config.fish"), "ok\n").expect("config");
    std::fs::create_dir_all(base.join("dotfiles")).expect("dotfiles");
    std::fs::write(base.join("dotfiles/zshrc"), "ok\n").expect("zshrc");
    std::os::unix::fs::symlink(base.join("dotfiles/zshrc"), base.join(".zshrc")).expect("link");
    let entry = |path: &std::path::Path, access| FileSystemSandboxEntry {
        path: FileSystemPath::Path {
            path: AbsolutePathBuf::from_absolute_path(path).expect("absolute"),
        },
        access,
        missing_path_behavior: None,
    };
    // As `ReadDenials` builds it on Linux: a full-write root, a denied
    // store, and read-only entries at real locations (a link's target).
    let policy = FileSystemSandboxPolicy::restricted(vec![
        FileSystemSandboxEntry {
            path: FileSystemPath::Special {
                value: FileSystemSpecialPath::Root,
            },
            access: FileSystemAccessMode::Write,
            missing_path_behavior: None,
        },
        entry(&base.join("secret"), FileSystemAccessMode::Deny),
        entry(&config.join("fish"), FileSystemAccessMode::Read),
        entry(&base.join("dotfiles/zshrc"), FileSystemAccessMode::Read),
    ]);
    std::fs::create_dir_all(base.join("secret")).expect("secret");
    let script = "echo x >> .zshrc; echo link-write=$?; \
                  echo x > config/fish/config.fish; echo write=$?; \
                  echo new > config/other.txt; echo other=$?; \
                  mv config moved; echo mv=$?; \
                  echo x > moved/fish/config.fish; echo moved-write=$?; \
                  rm .zshrc; echo rm-link=$?";
    let output = run_cmd_result_with_permission_profile_for_cwd(
        &["bash", "-c", script],
        AbsolutePathBuf::from_absolute_path(&base).expect("absolute base"),
        PermissionProfile::from_runtime_permissions(&policy, NetworkSandboxPolicy::Restricted),
        create_env_from_core_vars(),
        LONG_TIMEOUT_MS,
        /*use_legacy_landlock*/ false,
    )
    .await
    .expect("full-write profile runs under bubblewrap");
    let stdout = output.stdout.text;
    for held in ["link-write=1", "write=1", "mv=1", "moved-write=1"] {
        assert!(stdout.contains(held), "{held}: {stdout}");
    }
    // Recorded Linux limits: the link removal and a new file go through.
    for limit in ["other=0", "rm-link=0"] {
        assert!(stdout.contains(limit), "{limit}: {stdout}");
    }
    assert_eq!(
        std::fs::read_to_string(base.join("dotfiles/zshrc")).expect("zshrc"),
        "ok\n"
    );
    assert_eq!(
        std::fs::read_to_string(config.join("fish/config.fish")).expect("config"),
        "ok\n"
    );
    assert!(!base.join("moved").exists());
}

/// #587: under a profile that can write the Corbanu home's parent (full
/// access, or a workspace holding it) with protected paths, a command
/// cannot move the home (or a folder above it) aside and plant a new one:
/// the new folder would have none of the rules, and a later command could
/// read the moved copy's denied stores under the moved name.
#[tokio::test]
async fn issue_587_protected_home_and_its_folders_cannot_be_moved_aside() {
    if should_skip_bwrap_tests().await {
        eprintln!("skipping bwrap test: bwrap sandbox prerequisites are unavailable");
        return;
    }
    for full_access in [true, false] {
        let dir = tempfile::tempdir().expect("tempdir");
        let base = dir.path().canonicalize().expect("canonical");
        let home = base.join("outer/home");
        std::fs::create_dir_all(&home).expect("home");
        std::fs::write(home.join("auth.json"), "secret-587\n").expect("store");
        std::fs::write(home.join("AGENTS.md"), "original\n").expect("AGENTS.md");
        let entry = |path: std::path::PathBuf, access| {
            FileSystemSandboxEntry::skip_missing_path(
                FileSystemPath::Path {
                    path: AbsolutePathBuf::from_absolute_path(path).expect("absolute"),
                },
                access,
            )
        };
        let writable = if full_access {
            FileSystemSandboxEntry {
                path: FileSystemPath::Special {
                    value: FileSystemSpecialPath::Root,
                },
                access: FileSystemAccessMode::Write,
                missing_path_behavior: None,
            }
        } else {
            entry(base.clone(), FileSystemAccessMode::Write)
        };
        // As `ReadDenials` builds it: denied stores (one not created yet)
        // and read-only runtime entries (one missing).
        let policy = FileSystemSandboxPolicy::restricted(vec![
            FileSystemSandboxEntry {
                path: FileSystemPath::Special {
                    value: FileSystemSpecialPath::Root,
                },
                access: FileSystemAccessMode::Read,
                missing_path_behavior: None,
            },
            writable,
            entry(home.join("auth.json"), FileSystemAccessMode::Deny),
            entry(home.join("secrets"), FileSystemAccessMode::Deny),
            entry(home.join("AGENTS.md"), FileSystemAccessMode::Read),
            entry(home.join("skills"), FileSystemAccessMode::Read),
        ]);
        let run = |script: &'static str| {
            let cwd = AbsolutePathBuf::from_absolute_path(&base).expect("absolute base");
            let profile = PermissionProfile::from_runtime_permissions(
                &policy,
                NetworkSandboxPolicy::Restricted,
            );
            async move {
                run_cmd_result_with_permission_profile_for_cwd(
                    &["bash", "-c", script],
                    cwd,
                    profile,
                    create_env_from_core_vars(),
                    LONG_TIMEOUT_MS,
                    /*use_legacy_landlock*/ false,
                )
                .await
            }
        };
        let output = run("mv outer/home outer/home.old; echo mv-home=$?; \
                          mv outer outer.old; echo mv-outer=$?; \
                          mkdir -p outer/home; echo planted > outer/home/AGENTS.md; echo plant=$?")
        .await
        .expect("the profile runs under bubblewrap");
        let stdout = output.stdout.text;
        let stderr = output.stderr.text;
        for held in ["mv-home=1", "mv-outer=1", "plant=1"] {
            assert!(
                stdout.contains(held),
                "full access {full_access}: {held}: {stdout}\nstderr: {stderr}"
            );
        }
        assert!(!base.join("outer/home.old").exists());
        assert!(!base.join("outer.old").exists());
        assert_eq!(
            std::fs::read_to_string(home.join("AGENTS.md")).expect("AGENTS.md"),
            "original\n"
        );
        // A later command with the same rules: on main the moved store was
        // readable under its new name.
        let output = run("cat outer/home.old/auth.json outer.old/home/auth.json; true")
            .await
            .expect("the profile runs under bubblewrap");
        assert!(
            !output.stdout.text.contains("secret-587"),
            "full access {full_access}: {}",
            output.stdout.text
        );
    }
}

/// #473: under a full-write profile `~/.claude` is read-only (it runs hooks)
/// and `~/.claude/.credentials.json` is denied. Without that file, and with
/// no `~/.config` for the denied `~/.config/gh` and `~/.config/gcloud`, every
/// command still runs; the folder stays readable; and a credential file the
/// host writes while the command runs stays hidden from it.
#[tokio::test]
async fn missing_denied_paths_below_read_only_or_missing_folders_run_and_stay_hidden() {
    if should_skip_bwrap_tests().await {
        eprintln!("skipping bwrap test: bwrap sandbox prerequisites are unavailable");
        return;
    }
    let dir = tempfile::tempdir().expect("tempdir");
    let base = dir.path().canonicalize().expect("canonical");
    let claude = base.join(".claude");
    std::fs::create_dir_all(&claude).expect(".claude");
    std::fs::write(claude.join("settings.json"), "settings-ok\n").expect("settings");
    let credentials = claude.join(".credentials.json");
    let secret = claude.join("secret.json");
    std::fs::write(&secret, "secret-existing-473\n").expect("secret");
    let entry = |path: &std::path::Path, access| {
        FileSystemSandboxEntry::skip_missing_path(
            FileSystemPath::Path {
                path: AbsolutePathBuf::from_absolute_path(path).expect("absolute"),
            },
            access,
        )
    };
    // As `ReadDenials` builds it on Linux for a full-write profile.
    let policy = FileSystemSandboxPolicy::restricted(vec![
        FileSystemSandboxEntry {
            path: FileSystemPath::Special {
                value: FileSystemSpecialPath::Root,
            },
            access: FileSystemAccessMode::Write,
            missing_path_behavior: None,
        },
        entry(&claude, FileSystemAccessMode::Read),
        entry(&credentials, FileSystemAccessMode::Deny),
        entry(&secret, FileSystemAccessMode::Deny),
        entry(&base.join(".config/gh"), FileSystemAccessMode::Deny),
        entry(&base.join(".config/gcloud"), FileSystemAccessMode::Deny),
    ]);
    let script = "cat .claude/settings.json; echo settings=$?; \
                  cat .claude/secret.json; echo secret=$?; \
                  touch started; i=0; \
                  while [ ! -e go ] && [ $i -lt 100 ]; do sleep 0.05; i=$((i+1)); done; \
                  cat .claude/.credentials.json; echo read=$?; \
                  echo x > .claude/.credentials.json; echo write=$?; true";
    let command = ["bash", "-c", script];
    let run = run_cmd_result_with_permission_profile_for_cwd(
        &command,
        AbsolutePathBuf::from_absolute_path(&base).expect("absolute base"),
        PermissionProfile::from_runtime_permissions(&policy, NetworkSandboxPolicy::Restricted),
        create_env_from_core_vars(),
        NETWORK_TIMEOUT_MS,
        /*use_legacy_landlock*/ false,
    );
    // The host signs in while the command runs.
    let host = async {
        for _ in 0..200 {
            if base.join("started").exists() {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(25)).await;
        }
        std::fs::write(&credentials, "secret-473\n").expect("host writes credentials");
        std::fs::write(base.join("go"), "").expect("go");
    };
    let (output, ()) = tokio::join!(run, host);
    let output = output.expect("full-write profile runs under bubblewrap");
    let stdout = output.stdout.text;
    let stderr = output.stderr.text;
    assert_eq!(output.exit_code, 0, "stdout: {stdout}\nstderr: {stderr}");
    for held in ["settings-ok", "settings=0", "secret=1", "read=1", "write=1"] {
        assert!(stdout.contains(held), "{held}: {stdout}\nstderr: {stderr}");
    }
    assert!(!stdout.contains("secret-473"), "{stdout}");
    assert!(!stdout.contains("secret-existing-473"), "{stdout}");
    assert_eq!(
        std::fs::read_to_string(&credentials).expect("credentials"),
        "secret-473\n"
    );
    assert!(!base.join(".config").exists(), "placeholder removed");
}

/// #562: two overlapping runs that share placeholders for missing protected
/// paths (a read-only `AGENTS.md`, a denied `AGENTS.override.md`, and a
/// denied path below a missing folder). The second run starts while the
/// first holds them. When the first ends, its cleanup must not remove them
/// from under the second (unlinking a mount point detaches it in every
/// mount namespace), and the second must still run where a placeholder file
/// stands in for a missing folder. Nothing is created, and nothing is left.
#[tokio::test]
async fn overlapping_runs_keep_shared_placeholders_until_the_last_ends() {
    if should_skip_bwrap_tests().await {
        eprintln!("skipping bwrap test: bwrap sandbox prerequisites are unavailable");
        return;
    }
    let dir = tempfile::tempdir().expect("tempdir");
    let base = dir.path().canonicalize().expect("canonical");
    let home = base.join("home");
    std::fs::create_dir_all(&home).expect("home");
    let entry = |path: &std::path::Path, access| {
        FileSystemSandboxEntry::skip_missing_path(
            FileSystemPath::Path {
                path: AbsolutePathBuf::from_absolute_path(path).expect("absolute"),
            },
            access,
        )
    };
    // As `ReadDenials` builds it on Linux for a full-write profile.
    let policy = FileSystemSandboxPolicy::restricted(vec![
        FileSystemSandboxEntry {
            path: FileSystemPath::Special {
                value: FileSystemSpecialPath::Root,
            },
            access: FileSystemAccessMode::Write,
            missing_path_behavior: None,
        },
        entry(&home.join("AGENTS.md"), FileSystemAccessMode::Read),
        entry(&home.join("AGENTS.override.md"), FileSystemAccessMode::Deny),
        entry(&home.join("Library/Keychains"), FileSystemAccessMode::Deny),
    ]);
    let run = |script: String| {
        let base = base.clone();
        let policy = policy.clone();
        async move {
            run_cmd_result_with_permission_profile_for_cwd(
                &["bash", "-c", &script],
                AbsolutePathBuf::from_absolute_path(&base).expect("absolute base"),
                PermissionProfile::from_runtime_permissions(
                    &policy,
                    NetworkSandboxPolicy::Restricted,
                ),
                create_env_from_core_vars(),
                NETWORK_TIMEOUT_MS,
                /*use_legacy_landlock*/ false,
            )
            .await
        }
    };
    let wait_for = |name: &str| {
        format!("i=0; while [ ! -e {name} ] && [ $i -lt 120 ]; do sleep 0.05; i=$((i+1)); done")
    };
    // The first run waits until the second has started, then ends.
    let first_script = format!("touch first-started; {}; true", wait_for("second-started"));
    // The second starts once the first holds the placeholders, waits until
    // the first has ended, then tries to create every protected path.
    let second_script = format!(
        "touch second-started; {}; \
         printf x > home/AGENTS.md; echo agents=$?; \
         printf x > home/AGENTS.override.md; echo override=$?; \
         mkdir -p home/Library/Keychains; echo keychains=$?; true",
        wait_for("first-ended")
    );
    let first = async {
        let output = run(first_script).await;
        std::fs::write(base.join("first-ended"), "").expect("first-ended");
        output
    };
    let second = async {
        for _ in 0..200 {
            if base.join("first-started").exists() {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(25)).await;
        }
        assert!(
            home.join("AGENTS.md").exists(),
            "the first run's placeholder"
        );
        run(second_script).await
    };
    let (first, second) = tokio::join!(first, second);
    let first = first.expect("first run");
    assert_eq!(first.exit_code, 0, "first: {}", first.stderr.text);
    let second = second.expect("second run");
    let stdout = second.stdout.text;
    let stderr = second.stderr.text;
    assert_eq!(second.exit_code, 0, "stdout: {stdout}\nstderr: {stderr}");
    for held in ["agents=1", "override=1", "keychains=1"] {
        assert!(stdout.contains(held), "{held}: {stdout}\nstderr: {stderr}");
    }
    for name in ["AGENTS.md", "AGENTS.override.md", "Library"] {
        assert!(!home.join(name).exists(), "{name} left in the home");
    }
}

/// #562: a file left where a denied path's folder would be (here a stale
/// placeholder for `home/Library` from a run that was killed) used to fail
/// every run (`Can't mkdir parents … Not a directory`). The run now works,
/// and the file stays in place: the command can neither remove it nor create
/// the denied path below it.
#[tokio::test]
async fn a_file_where_a_denied_paths_folder_would_be_stays_in_place() {
    if should_skip_bwrap_tests().await {
        eprintln!("skipping bwrap test: bwrap sandbox prerequisites are unavailable");
        return;
    }
    let dir = tempfile::tempdir().expect("tempdir");
    let base = dir.path().canonicalize().expect("canonical");
    let home = base.join("home");
    std::fs::create_dir_all(&home).expect("home");
    std::fs::write(home.join("Library"), "").expect("stale placeholder");
    let policy = FileSystemSandboxPolicy::restricted(vec![
        FileSystemSandboxEntry {
            path: FileSystemPath::Special {
                value: FileSystemSpecialPath::Root,
            },
            access: FileSystemAccessMode::Write,
            missing_path_behavior: None,
        },
        FileSystemSandboxEntry::skip_missing_path(
            FileSystemPath::Path {
                path: AbsolutePathBuf::from_absolute_path(home.join("Library/Keychains"))
                    .expect("absolute"),
            },
            FileSystemAccessMode::Deny,
        ),
    ]);
    let output = run_cmd_result_with_permission_profile_for_cwd(
        &[
            "bash",
            "-c",
            "rm -f home/Library; echo rm=$?; mv home/Library home/Moved; echo mv=$?; \
             mkdir -p home/Library/Keychains; echo mkdir=$?; true",
        ],
        AbsolutePathBuf::from_absolute_path(&base).expect("absolute base"),
        PermissionProfile::from_runtime_permissions(&policy, NetworkSandboxPolicy::Restricted),
        create_env_from_core_vars(),
        NETWORK_TIMEOUT_MS,
        /*use_legacy_landlock*/ false,
    )
    .await
    .expect("the run starts");
    let stdout = output.stdout.text;
    let stderr = output.stderr.text;
    assert_eq!(output.exit_code, 0, "stdout: {stdout}\nstderr: {stderr}");
    for held in ["rm=1", "mv=1", "mkdir=1"] {
        assert!(stdout.contains(held), "{held}: {stdout}\nstderr: {stderr}");
    }
    assert!(home.join("Library").is_file(), "the file stays");
    assert!(!home.join("Moved").exists());
}

/// #582: a placeholder whose run was killed (SIGKILL on a timeout) before
/// it could clean up is removed by the next protected run, instead of
/// staying for good: an empty read-only `~/Library` once broke every
/// protected run on a machine (#579 then held it in place).
#[tokio::test]
async fn issue_582_a_placeholder_left_by_a_killed_run_is_removed_by_the_next() {
    if should_skip_bwrap_tests().await {
        eprintln!("skipping bwrap test: bwrap sandbox prerequisites are unavailable");
        return;
    }
    let dir = tempfile::tempdir().expect("tempdir");
    let base = dir.path().canonicalize().expect("canonical");
    let home = base.join("home");
    std::fs::create_dir_all(&home).expect("home");
    let policy = FileSystemSandboxPolicy::restricted(vec![
        FileSystemSandboxEntry {
            path: FileSystemPath::Special {
                value: FileSystemSpecialPath::Root,
            },
            access: FileSystemAccessMode::Write,
            missing_path_behavior: None,
        },
        FileSystemSandboxEntry::skip_missing_path(
            FileSystemPath::Path {
                path: AbsolutePathBuf::from_absolute_path(home.join("Library/Keychains"))
                    .expect("absolute"),
            },
            FileSystemAccessMode::Deny,
        ),
    ]);
    // A registry of its own, so other tests' runs do not sweep this one's
    // placeholder first.
    let tmp = base.join("tmp");
    std::fs::create_dir(&tmp).expect("tmp");
    let run = |script: &'static str, timeout_ms: u64| {
        let cwd = AbsolutePathBuf::from_absolute_path(&base).expect("absolute base");
        let profile =
            PermissionProfile::from_runtime_permissions(&policy, NetworkSandboxPolicy::Restricted);
        let mut env = create_env_from_core_vars();
        env.insert("TMPDIR".to_string(), tmp.to_string_lossy().into_owned());
        async move {
            run_cmd_result_with_permission_profile_for_cwd(
                &["bash", "-c", script],
                cwd,
                profile,
                env,
                timeout_ms,
                /*use_legacy_landlock*/ false,
            )
            .await
        }
    };
    // Killed on its timeout while its placeholder is in place.
    let killed = run("sleep 30", 2_000).await;
    assert!(
        matches!(
            killed
                .as_ref()
                .err()
                .map(codex_protocol::error::CodexErr::details),
            Some(CodexErrorDetails::Sandbox(SandboxErr::Timeout { .. }))
        ),
        "{killed:?}"
    );
    let library = home.join("Library");
    let stale = std::fs::symlink_metadata(&library).expect("the placeholder stays");
    assert!(stale.is_file() && stale.len() == 0, "{stale:?}");
    let output = run("echo ran-582", NETWORK_TIMEOUT_MS)
        .await
        .expect("the next run starts");
    assert!(output.stdout.text.contains("ran-582"), "{output:?}");
    assert!(
        std::fs::symlink_metadata(&library).is_err(),
        "the stale placeholder is removed, and the next run cleans up its own"
    );
}

/// #348: as #582, but the killed run's temporary folder (with the registry
/// that recorded its placeholder) is lost before the next run, as on a
/// reboot. The record kept in the Corbanu home removes it at the next start;
/// before, it stayed for good. Needs an existing Corbanu home (where the
/// sandbox keeps the record); skipped without one.
#[tokio::test]
async fn issue_348_a_placeholder_left_by_a_killed_run_is_removed_after_its_registry_is_lost() {
    if should_skip_bwrap_tests().await {
        eprintln!("skipping bwrap test: bwrap sandbox prerequisites are unavailable");
        return;
    }
    if !codex_core::config::find_codex_home().is_ok_and(|home| home.as_path().is_dir()) {
        eprintln!("skipping: no Corbanu home to keep the placeholder record in");
        return;
    }
    let dir = tempfile::tempdir().expect("tempdir");
    let base = dir.path().canonicalize().expect("canonical");
    let home = base.join("home");
    std::fs::create_dir_all(&home).expect("home");
    let policy = FileSystemSandboxPolicy::restricted(vec![
        FileSystemSandboxEntry {
            path: FileSystemPath::Special {
                value: FileSystemSpecialPath::Root,
            },
            access: FileSystemAccessMode::Write,
            missing_path_behavior: None,
        },
        FileSystemSandboxEntry::skip_missing_path(
            FileSystemPath::Path {
                path: AbsolutePathBuf::from_absolute_path(home.join("Library/Keychains"))
                    .expect("absolute"),
            },
            FileSystemAccessMode::Deny,
        ),
    ]);
    let run = |script: &'static str, timeout_ms: u64, tmp: PathBuf| {
        let cwd = AbsolutePathBuf::from_absolute_path(&base).expect("absolute base");
        let profile =
            PermissionProfile::from_runtime_permissions(&policy, NetworkSandboxPolicy::Restricted);
        let mut env = create_env_from_core_vars();
        std::fs::create_dir_all(&tmp).expect("tmp");
        env.insert("TMPDIR".to_string(), tmp.to_string_lossy().into_owned());
        async move {
            run_cmd_result_with_permission_profile_for_cwd(
                &["bash", "-c", script],
                cwd,
                profile,
                env,
                timeout_ms,
                /*use_legacy_landlock*/ false,
            )
            .await
        }
    };
    let killed_tmp = base.join("tmp-killed");
    let killed = run("sleep 30", 2_000, killed_tmp.clone()).await;
    assert!(
        matches!(
            killed
                .as_ref()
                .err()
                .map(codex_protocol::error::CodexErr::details),
            Some(CodexErrorDetails::Sandbox(SandboxErr::Timeout { .. }))
        ),
        "{killed:?}"
    );
    let library = home.join("Library");
    let stale = std::fs::symlink_metadata(&library).expect("the placeholder stays");
    assert!(stale.is_file() && stale.len() == 0, "{stale:?}");
    // The reboot.
    std::fs::remove_dir_all(&killed_tmp).expect("lose the registry");
    let output = run("echo ran-348", NETWORK_TIMEOUT_MS, base.join("tmp-next"))
        .await
        .expect("the next run starts");
    assert!(output.stdout.text.contains("ran-348"), "{output:?}");
    assert!(
        std::fs::symlink_metadata(&library).is_err(),
        "the stale placeholder is removed, and the next run cleans up its own"
    );
}

/// #493: under a full-write profile with a denial, the writable `/` bind
/// must not cover the minimal `/dev` (bubblewrap binds `nodev`), so
/// `/dev/null` stays usable while the denied file stays unreadable.
#[tokio::test]
async fn full_write_profile_with_denial_keeps_dev_null_usable() {
    if should_skip_bwrap_tests().await {
        eprintln!("skipping bwrap test: bwrap sandbox prerequisites are unavailable");
        return;
    }
    let dir = tempfile::tempdir().expect("tempdir");
    let base = dir.path().canonicalize().expect("canonical");
    let secret = base.join("secret.txt");
    std::fs::write(&secret, "secret-493\n").expect("secret");
    let policy = FileSystemSandboxPolicy::restricted(vec![
        FileSystemSandboxEntry {
            path: FileSystemPath::Special {
                value: FileSystemSpecialPath::Root,
            },
            access: FileSystemAccessMode::Write,
            missing_path_behavior: None,
        },
        FileSystemSandboxEntry {
            path: FileSystemPath::Path {
                path: AbsolutePathBuf::from_absolute_path(&secret).expect("absolute secret"),
            },
            access: FileSystemAccessMode::Deny,
            missing_path_behavior: None,
        },
        // A denied device node stays denied on the fresh `/dev`.
        FileSystemSandboxEntry {
            path: FileSystemPath::Path {
                path: AbsolutePathBuf::from_absolute_path("/dev/full").expect("absolute /dev/full"),
            },
            access: FileSystemAccessMode::Deny,
            missing_path_behavior: None,
        },
    ]);
    let script = "echo hi > /dev/null; echo null=$?; \
                  head -c 1 /dev/full > /dev/null; echo full=$?; \
                  head -c 1 /dev/urandom > /dev/null; echo urandom=$?; \
                  cat secret.txt; echo cat=$?; \
                  echo x > secret.txt; echo write=$?; \
                  echo ok > other.txt; echo other=$?";
    let output = run_cmd_result_with_permission_profile_for_cwd(
        &["bash", "-c", script],
        AbsolutePathBuf::from_absolute_path(&base).expect("absolute base"),
        PermissionProfile::from_runtime_permissions(&policy, NetworkSandboxPolicy::Restricted),
        create_env_from_core_vars(),
        LONG_TIMEOUT_MS,
        /*use_legacy_landlock*/ false,
    )
    .await
    .expect("full-write profile runs under bubblewrap");
    let stdout = output.stdout.text;
    let stderr = output.stderr.text;
    for expected in [
        "null=0",
        "urandom=0",
        "full=1",
        "cat=1",
        "write=1",
        "other=0",
    ] {
        assert!(
            stdout.contains(expected),
            "{expected}: {stdout}\nstderr: {stderr}"
        );
    }
    assert!(!stdout.contains("secret-493"), "{stdout}");
    assert_eq!(
        std::fs::read_to_string(&secret).expect("secret"),
        "secret-493\n"
    );
    assert_eq!(
        std::fs::read_to_string(base.join("other.txt")).expect("other"),
        "ok\n"
    );
}

/// #488: a workspace opened through a nested link keeps a denied file
/// unreadable whichever spelling denies or reads it. Bubblewrap binds the
/// workspace at its real path, so a denial spelled by the real path must be
/// masked after that bind.
#[tokio::test]
async fn sandbox_denies_reads_through_a_nested_link_workspace() {
    if should_skip_bwrap_tests().await {
        eprintln!("skipping bwrap test: bwrap sandbox prerequisites are unavailable");
        return;
    }

    let tmpdir = tempfile::tempdir().expect("tempdir");
    let real = tmpdir.path().join("real");
    let link = tmpdir.path().join("link");
    std::fs::create_dir(&real).expect("real");
    std::fs::write(real.join(".env"), "API_KEY=x\n").expect(".env");
    std::fs::write(real.join("other"), "ok\n").expect("other");
    std::os::unix::fs::symlink(&real, &link).expect("link");
    let script = format!(
        "cat .env; echo dot=$?; cat {real}/.env; echo real=$?; cat {link}/.env; echo link=$?; \
         cat other; echo other=$?",
        real = real.display(),
        link = link.display(),
    );
    for (denied, cwd) in [(&real, &link), (&link, &real)] {
        let file_system_sandbox_policy = FileSystemSandboxPolicy::restricted(vec![
            FileSystemSandboxEntry {
                path: FileSystemPath::Special {
                    value: FileSystemSpecialPath::Root,
                },
                access: FileSystemAccessMode::Read,
                missing_path_behavior: None,
            },
            FileSystemSandboxEntry {
                path: FileSystemPath::Special {
                    value: FileSystemSpecialPath::project_roots(/*subpath*/ None),
                },
                access: FileSystemAccessMode::Write,
                missing_path_behavior: None,
            },
            FileSystemSandboxEntry {
                path: FileSystemPath::Path {
                    path: AbsolutePathBuf::try_from(denied.join(".env").as_path())
                        .expect("absolute .env"),
                },
                access: FileSystemAccessMode::Deny,
                missing_path_behavior: None,
            },
        ]);
        let permission_profile = PermissionProfile::from_runtime_permissions(
            &file_system_sandbox_policy,
            NetworkSandboxPolicy::Restricted,
        );
        let output = run_cmd_result_with_permission_profile_for_cwd(
            &["bash", "-c", &script],
            AbsolutePathBuf::try_from(cwd.as_path()).expect("absolute cwd"),
            permission_profile,
            create_env_from_core_vars(),
            LONG_TIMEOUT_MS,
            /*use_legacy_landlock*/ false,
        )
        .await
        .expect("sandboxed command should execute");
        let stdout = &output.stdout.text;
        let report = format!(
            "{denied:?} denied: exit {}, stdout {stdout}, stderr {}",
            output.exit_code, output.stderr.text
        );
        // #502: a denial spelled through the absolute link runs too (it
        // used to fail every command) and stays denied in every spelling.
        assert!(!stdout.contains("API_KEY"), "{report}");
        for line in ["dot=1", "real=1", "link=1", "other=0"] {
            assert!(stdout.contains(line), "{line} missing: {report}");
        }
    }
}

/// #501: under restricted reads, a read-only root reached through a link is
/// mounted at the link's path. A denial spelled by the real path (or through
/// the link) stays unreadable through that mount, and the rest stays
/// readable.
#[tokio::test]
async fn restricted_reads_deny_through_a_read_only_linked_root() {
    if should_skip_bwrap_tests().await {
        eprintln!("skipping bwrap test: bwrap sandbox prerequisites are unavailable");
        return;
    }
    let tmpdir = tempfile::tempdir().expect("tempdir");
    let base = tmpdir.path().canonicalize().expect("canonical");
    let real = base.join("real");
    let link = base.join("link");
    let work = base.join("work");
    std::fs::create_dir_all(&real).expect("real");
    std::fs::create_dir_all(&work).expect("work");
    std::fs::write(real.join("secret"), "API_KEY=x\n").expect("secret");
    std::fs::write(real.join("other"), "ok\n").expect("other");
    std::os::unix::fs::symlink(&real, &link).expect("link");
    let sandbox_helper_dir = codex_linux_sandbox_exe()
        .parent()
        .expect("sandbox helper should have a parent")
        .to_path_buf();
    let entry = |path: &Path, access| FileSystemSandboxEntry {
        path: FileSystemPath::Path {
            path: AbsolutePathBuf::try_from(path).expect("absolute path"),
        },
        access,
        missing_path_behavior: None,
    };
    let script = format!(
        "cat {link}/secret; echo link=$?; cat {link}/other; echo other=$?",
        link = link.display(),
    );
    // The last denial holds the linked root's whole target.
    for (denied, other) in [
        (real.join("secret"), "other=0"),
        (link.join("secret"), "other=0"),
        (real.clone(), "other=1"),
    ] {
        let file_system_sandbox_policy = FileSystemSandboxPolicy::restricted(vec![
            FileSystemSandboxEntry {
                path: FileSystemPath::Special {
                    value: FileSystemSpecialPath::Minimal,
                },
                access: FileSystemAccessMode::Read,
                missing_path_behavior: None,
            },
            entry(&sandbox_helper_dir, FileSystemAccessMode::Read),
            entry(&link, FileSystemAccessMode::Read),
            entry(&work, FileSystemAccessMode::Write),
            entry(&denied, FileSystemAccessMode::Deny),
        ]);
        let permission_profile = PermissionProfile::from_runtime_permissions(
            &file_system_sandbox_policy,
            NetworkSandboxPolicy::Restricted,
        );
        let output = run_cmd_result_with_permission_profile_for_cwd(
            &["bash", "-c", &script],
            AbsolutePathBuf::try_from(work.as_path()).expect("absolute cwd"),
            permission_profile,
            create_env_from_core_vars(),
            LONG_TIMEOUT_MS,
            /*use_legacy_landlock*/ false,
        )
        .await
        .expect("sandboxed command should execute");
        let stdout = &output.stdout.text;
        let report = format!(
            "{denied:?} denied: exit {}, stdout {stdout}, stderr {}",
            output.exit_code, output.stderr.text
        );
        assert!(!stdout.contains("API_KEY"), "{report}");
        for line in ["link=1", other] {
            assert!(stdout.contains(line), "{line} missing: {report}");
        }
    }
}

/// #503: in a full-write profile (`:root = write` plus a denial), an extra
/// writable root that is a symlink to `/` is treated as `/`: it is bound before
/// shallower writable roots, so their protections (a read-only `.git`, a
/// denied file) are not covered by a later `/` bind; read-only paths outside
/// every workspace stay read-only; and it asks for no mount points at the
/// filesystem root.
#[tokio::test]
async fn writable_symlink_to_root_keeps_shallower_root_protections() {
    if should_skip_bwrap_tests().await {
        eprintln!("skipping bwrap test: bwrap sandbox prerequisites are unavailable");
        return;
    }
    let dir = tempfile::tempdir().expect("tempdir");
    let base = dir.path().canonicalize().expect("canonical");
    let workspace = base.join("ws");
    std::fs::create_dir_all(workspace.join(".git")).expect("workspace .git");
    let secret = workspace.join("secret.txt");
    std::fs::write(&secret, "secret-503\n").expect("secret");
    let read_only = base.join("ro");
    std::fs::create_dir(&read_only).expect("read-only dir");
    let link = base.join("a").join("b").join("c").join("root-link");
    std::fs::create_dir_all(link.parent().expect("link parent")).expect("link parent");
    std::os::unix::fs::symlink("/", &link).expect("symlink to /");
    let entry = |path: &std::path::Path, access| FileSystemSandboxEntry {
        path: FileSystemPath::Path {
            path: AbsolutePathBuf::from_absolute_path(path).expect("absolute path"),
        },
        access,
        missing_path_behavior: None,
    };
    let policy = FileSystemSandboxPolicy::restricted(vec![
        FileSystemSandboxEntry {
            path: FileSystemPath::Special {
                value: FileSystemSpecialPath::Root,
            },
            access: FileSystemAccessMode::Write,
            missing_path_behavior: None,
        },
        entry(&workspace, FileSystemAccessMode::Write),
        entry(&link, FileSystemAccessMode::Write),
        entry(&secret, FileSystemAccessMode::Deny),
        entry(&read_only, FileSystemAccessMode::Read),
    ]);
    let script = "echo hi > /dev/null; echo null=$?; \
                  touch ../ro/probe; echo ro=$?; \
                  touch .git/probe; echo git=$?; \
                  cat secret.txt; echo cat=$?; \
                  echo ok > other.txt; echo other=$?; \
                  echo ok > ../outside.txt; echo outside=$?";
    let output = run_cmd_result_with_permission_profile_for_cwd(
        &["bash", "-c", script],
        AbsolutePathBuf::from_absolute_path(&workspace).expect("absolute workspace"),
        PermissionProfile::from_runtime_permissions(&policy, NetworkSandboxPolicy::Restricted),
        create_env_from_core_vars(),
        LONG_TIMEOUT_MS,
        /*use_legacy_landlock*/ false,
    )
    .await
    .expect("symlinked root profile runs under bubblewrap");
    let stdout = output.stdout.text;
    let stderr = output.stderr.text;
    for expected in ["null=0", "ro=1", "git=1", "cat=1", "other=0", "outside=0"] {
        assert!(
            stdout.contains(expected),
            "{expected}: {stdout}\nstderr: {stderr}"
        );
    }
    assert!(!stdout.contains("secret-503"), "{stdout}");
    assert!(!workspace.join(".git").join("probe").exists());
    assert!(!read_only.join("probe").exists());
    assert_eq!(
        std::fs::read_to_string(base.join("outside.txt")).expect("outside"),
        "ok\n"
    );
}

/// #506: under real bubblewrap, a directory the policy may not list cannot
/// be listed or have its mode changed, under a read-only and a full-write
/// base; its entries stay usable and a denied entry stays denied.
#[tokio::test]
async fn issue_506_unlistable_directory_under_bwrap() {
    if should_skip_bwrap_tests().await {
        eprintln!("skipping bwrap test: bwrap sandbox prerequisites are unavailable");
        return;
    }
    let tmpdir = tempfile::tempdir().expect("tempdir");
    let base = tmpdir.path().canonicalize().expect("canonical");
    let home = base.join("home");
    std::fs::create_dir_all(home.join("tmp")).expect("tmp");
    std::fs::write(home.join("tmp/helper.txt"), "helper-506").expect("helper");
    std::fs::write(home.join("listing-canary-506"), "x").expect("canary");
    std::fs::write(home.join("secret.txt"), "secret-506").expect("secret");
    std::os::unix::fs::symlink("tmp", home.join("link")).expect("link");
    let home_path = AbsolutePathBuf::try_from(home.as_path()).expect("absolute home");
    let home_str = home.to_string_lossy().into_owned();
    let script = format!(
        "ls {home_str}; echo ls=$?; ls {home_str}/tmp/..; echo ls-dotdot=$?; \
         chmod 755 {home_str}; echo chmod=$?; ls {home_str}; echo ls-after-chmod=$?; \
         touch {home_str}/new 2>/dev/null; echo touch=$?; \
         cat {home_str}/tmp/helper.txt; echo; ls {home_str}/tmp; echo ls-tmp=$?; \
         cat {home_str}/link/helper.txt; echo; \
         cat {home_str}/secret.txt; echo cat-secret=$?"
    );
    for root_access in [FileSystemAccessMode::Read, FileSystemAccessMode::Write] {
        let policy = FileSystemSandboxPolicy::restricted(vec![
            FileSystemSandboxEntry {
                path: FileSystemPath::Special {
                    value: FileSystemSpecialPath::Root,
                },
                access: root_access,
                missing_path_behavior: None,
            },
            FileSystemSandboxEntry {
                path: FileSystemPath::Path {
                    path: home_path.join("secret.txt"),
                },
                access: FileSystemAccessMode::Deny,
                missing_path_behavior: None,
            },
            FileSystemSandboxEntry::deny_directory_listing(&home_path),
        ]);
        let permission_profile =
            PermissionProfile::from_runtime_permissions(&policy, NetworkSandboxPolicy::Enabled);
        let output = match run_cmd_result_with_permission_profile(
            &["bash", "-c", &script],
            permission_profile,
            LONG_TIMEOUT_MS,
            /*use_legacy_landlock*/ false,
        )
        .await
        {
            Ok(output) => output,
            Err(err) => match err.details() {
                CodexErrorDetails::Sandbox(SandboxErr::Denied { output, .. }) => {
                    output.as_ref().clone()
                }
                details => panic!("{root_access:?}: {details:?}"),
            },
        };
        let stdout = &output.stdout.text;
        let context = format!("{root_access:?}: {stdout} / {}", output.stderr.text);
        for expected in [
            "ls=2",
            "ls-dotdot=2",
            "chmod=1",
            "ls-after-chmod=2",
            "touch=1",
            "ls-tmp=0",
            "cat-secret=1",
        ] {
            assert!(stdout.contains(expected), "{expected} {context}");
        }
        assert!(!stdout.contains("listing-canary-506"), "{context}");
        assert_eq!(stdout.matches("helper-506").count(), 2, "{context}");
        assert!(stdout.contains("helper.txt"), "{context}");
        assert!(!stdout.contains("secret-506"), "{context}");
        assert!(!home.join("new").exists(), "{context}");
    }
}

/// #506 review: a Corbanu home named `.codex` directly in a writable
/// workspace is read-only there and still cannot be listed under real
/// bubblewrap; its entries stay readable but not writable.
#[tokio::test]
async fn issue_506_unlistable_dot_codex_home_in_a_writable_workspace_under_bwrap() {
    if should_skip_bwrap_tests().await {
        eprintln!("skipping bwrap test: bwrap sandbox prerequisites are unavailable");
        return;
    }
    let tmpdir = tempfile::tempdir().expect("tempdir");
    let workspace = tmpdir.path().canonicalize().expect("canonical").join("ws");
    let home = workspace.join(".codex");
    std::fs::create_dir_all(home.join("tmp")).expect("tmp");
    std::fs::write(home.join("tmp/helper.txt"), "helper-506").expect("helper");
    std::fs::write(home.join("listing-canary-506"), "x").expect("canary");
    let home_path = AbsolutePathBuf::try_from(home.as_path()).expect("absolute home");
    let home_str = home.to_string_lossy().into_owned();
    let policy = FileSystemSandboxPolicy::restricted(vec![
        FileSystemSandboxEntry {
            path: FileSystemPath::Special {
                value: FileSystemSpecialPath::Root,
            },
            access: FileSystemAccessMode::Read,
            missing_path_behavior: None,
        },
        FileSystemSandboxEntry {
            path: FileSystemPath::Path {
                path: AbsolutePathBuf::try_from(workspace.as_path()).expect("absolute ws"),
            },
            access: FileSystemAccessMode::Write,
            missing_path_behavior: None,
        },
        FileSystemSandboxEntry {
            path: FileSystemPath::Path {
                path: home_path.join("wallet"),
            },
            access: FileSystemAccessMode::Deny,
            missing_path_behavior: Some(
                codex_protocol::permissions::FileSystemSandboxEntryMissingPathBehavior::Skip,
            ),
        },
        FileSystemSandboxEntry::deny_directory_listing(&home_path),
    ]);
    let script = format!(
        "ls {home_str}; echo ls=$?; cat {home_str}/tmp/helper.txt; echo; \
         touch {home_str}/tmp/new 2>/dev/null; echo touch=$?; \
         touch {}/ok && echo workspace-writable",
        workspace.display()
    );
    let output = run_cmd_result_with_permission_profile_for_cwd(
        &["bash", "-c", &script],
        AbsolutePathBuf::try_from(workspace.as_path()).expect("cwd"),
        PermissionProfile::from_runtime_permissions(&policy, NetworkSandboxPolicy::Enabled),
        create_env_from_core_vars(),
        LONG_TIMEOUT_MS,
        /*use_legacy_landlock*/ false,
    )
    .await;
    let output = match output {
        Ok(output) => output,
        Err(err) => match err.details() {
            CodexErrorDetails::Sandbox(SandboxErr::Denied { output, .. }) => {
                output.as_ref().clone()
            }
            details => panic!("{details:?}"),
        },
    };
    let stdout = &output.stdout.text;
    let context = format!("{stdout} / {}", output.stderr.text);
    assert!(stdout.contains("ls=2"), "{context}");
    assert!(!stdout.contains("listing-canary-506"), "{context}");
    assert!(stdout.contains("helper-506"), "{context}");
    assert!(stdout.contains("touch=1"), "{context}");
    assert!(stdout.contains("workspace-writable"), "{context}");
    assert!(!home.join("tmp/new").exists(), "{context}");
}

/// Aggressive's profile on Linux: write in `work`, network off, and every
/// credential file name below `work` denied (`tui::security::aggressive`).
fn credential_name_profile(work: &std::path::Path) -> PermissionProfile {
    let root = AbsolutePathBuf::try_from(work).unwrap();
    let (mut file_system, network) = PermissionProfile::workspace_write_with(
        std::slice::from_ref(&root),
        NetworkSandboxPolicy::Restricted,
        /*exclude_tmpdir_env_var*/ true,
        /*exclude_slash_tmp*/ true,
    )
    .to_runtime_permissions();
    file_system.entries.extend(
        codex_protocol::permissions::credential_file_globs(&work.to_string_lossy())
            .into_iter()
            .map(|pattern| {
                FileSystemSandboxEntry::new(
                    FileSystemPath::GlobPattern {
                        pattern,
                        scan: None,
                    },
                    FileSystemAccessMode::Deny,
                )
            }),
    );
    PermissionProfile::from_runtime_permissions(&file_system, network)
}

async fn run_in(
    work: &std::path::Path,
    script: &str,
) -> codex_protocol::exec_output::ExecToolCallOutput {
    let result = run_cmd_result_with_permission_profile_for_cwd(
        &["bash", "-c", script],
        AbsolutePathBuf::try_from(work).unwrap(),
        credential_name_profile(work),
        create_env_from_core_vars(),
        LONG_TIMEOUT_MS,
        /*use_legacy_landlock*/ false,
    )
    .await;
    match result {
        Ok(output) => output,
        Err(err) => match err.details() {
            CodexErrorDetails::Sandbox(SandboxErr::Denied { output, .. }) => {
                output.as_ref().clone()
            }
            details => panic!("{script}: {details:?}"),
        },
    }
}

/// A workspace for the #442 cases, or `None` when bwrap cannot run here.
async fn credential_workspace() -> Option<(tempfile::TempDir, std::path::PathBuf)> {
    if should_skip_bwrap_tests().await {
        eprintln!("skipping bwrap test: bwrap sandbox prerequisites are unavailable");
        return None;
    }
    let tmpdir = tempfile::tempdir().expect("tempdir");
    let work = tmpdir.path().canonicalize().unwrap();
    std::fs::create_dir_all(work.join("app")).unwrap();
    std::fs::write(work.join("notes.txt"), "plain-notes").unwrap();
    Some((tmpdir, work))
}

/// #442 (Travis, 2026-10-11), real bubblewrap: a sandboxed command that
/// creates a `.env` is stopped, and the next command cannot read the file.
#[tokio::test]
async fn issue_442_env_created_by_a_sandboxed_command_stops_it_and_stays_hidden() {
    let Some((_tmpdir, work)) = credential_workspace().await else {
        return;
    };
    let made = run_in(
        &work,
        "printf API_KEY=dummy-442-agent > .env.local; sleep 2; echo made=$?",
    )
    .await;
    assert_ne!(made.exit_code, 0, "{made:?}");
    assert!(!made.stdout.text.contains("made="), "{made:?}");
    assert!(
        made.stderr
            .text
            .contains("the sandbox stopped this command"),
        "{made:?}"
    );
    let read = run_in(
        &work,
        "cat .env.local; echo env=$?; cp .env.local copy; echo copy=$?; cat notes.txt",
    )
    .await;
    let text = format!("{}{}", read.stdout.text, read.stderr.text);
    assert!(!text.contains("dummy-442"), "{text}");
    for line in ["env=1", "copy=1", "plain-notes"] {
        assert!(read.stdout.text.contains(line), "{line} not in {text}");
    }
}

/// #442, real bubblewrap: files the person creates between commands, in any
/// letter case, are unreadable to the next command.
#[tokio::test]
async fn issue_442_env_created_between_commands_stays_hidden() {
    let Some((_tmpdir, work)) = credential_workspace().await else {
        return;
    };
    std::fs::write(work.join("app/.env"), "API_KEY=dummy-442-person").unwrap();
    std::fs::write(work.join(".ENV.Local"), "API_KEY=dummy-442-case").unwrap();
    std::fs::write(work.join(".envrc"), "export API_KEY=dummy-442-rc").unwrap();
    let read = run_in(
        &work,
        "for f in app/.env .ENV.Local .envrc; do cat \"$f\"; echo \"$f=$?\"; done; cat notes.txt",
    )
    .await;
    let text = format!("{}{}", read.stdout.text, read.stderr.text);
    assert!(!text.contains("dummy-442"), "{text}");
    for line in ["app/.env=1", ".ENV.Local=1", ".envrc=1", "plain-notes"] {
        assert!(read.stdout.text.contains(line), "{line} not in {text}");
    }
}

/// #442, real bubblewrap: a `.env` created while a command runs stops that
/// command before it reads it (the command waits, as a later read in a
/// long-lived shell would).
#[tokio::test]
async fn issue_442_env_created_while_a_command_runs_stops_it() {
    let Some((_tmpdir, work)) = credential_workspace().await else {
        return;
    };
    let running = {
        let work = work.clone();
        tokio::spawn(async move {
            run_in(
                &work,
                "touch started; sleep 4; cat .env.later; echo later=$?",
            )
            .await
        })
    };
    // Once the command is running inside the sandbox.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    while !work.join("started").exists() && std::time::Instant::now() < deadline {
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    assert!(work.join("started").exists(), "the command did not start");
    std::fs::write(work.join(".env.later"), "API_KEY=dummy-442-later").unwrap();
    let later = running.await.unwrap();
    let text = format!("{}{}", later.stdout.text, later.stderr.text);
    assert!(!text.contains("dummy-442"), "{text}");
    assert!(!text.contains("later="), "{text}");
    assert!(text.contains("the sandbox stopped this command"), "{text}");
}

/// #442, real bubblewrap: a link planted under a credential name stops the
/// command that plants it, and later commands are refused until it is
/// removed, so the person's `.env` can never be written through it to a
/// file the command can read.
#[tokio::test]
async fn issue_442_a_planted_env_link_refuses_later_commands() {
    let Some((_tmpdir, work)) = credential_workspace().await else {
        return;
    };
    let planted = run_in(&work, "ln -s notes-later .env; sleep 2; echo planted=$?").await;
    assert!(!planted.stdout.text.contains("planted="), "{planted:?}");
    assert!(
        planted
            .stderr
            .text
            .contains("the sandbox stopped this command"),
        "{planted:?}"
    );
    for state in ["dangling", "written through"] {
        if state == "written through" {
            // The person writes the key through the link.
            std::fs::write(work.join(".env"), "API_KEY=dummy-442-planted").unwrap();
        }
        let refused = run_in(&work, "cat notes-later; echo read=$?").await;
        let text = format!("{}{}", refused.stdout.text, refused.stderr.text);
        assert!(!text.contains("dummy-442"), "{state}: {text}");
        assert!(!text.contains("read="), "{state}: {text}");
        assert!(
            text.contains("the sandbox refuses to run this command"),
            "{state}: {text}"
        );
    }
    std::fs::remove_file(work.join(".env")).unwrap();
    let after = run_in(&work, "cat notes.txt; echo notes=$?").await;
    assert!(after.stdout.text.contains("notes=0"), "{after:?}");
}

/// #442 (Travis, 2026-10-12), real bubblewrap: `.env.example`,
/// `.env.sample` and `.env.template` (any case) are not credential files:
/// commands read, create and change them without being stopped. A template
/// name that is a link to the person's `.env` still reads nothing, and a
/// hard link from a template name to `.env` refuses later commands, as any
/// extra name of `.env` does.
#[tokio::test]
async fn issue_442_templates_stay_usable_and_never_reach_the_env() {
    let Some((_tmpdir, work)) = credential_workspace().await else {
        return;
    };
    std::fs::write(work.join(".env"), "API_KEY=dummy-442-person").unwrap();
    std::fs::write(work.join(".env.example"), "API_KEY=example-442").unwrap();
    std::fs::write(work.join("app/.ENV.SAMPLE"), "API_KEY=sample-442").unwrap();
    let used = run_in(
        &work,
        "cat .env.example; echo example=$?; cat app/.ENV.SAMPLE; echo sample=$?; \
         printf tpl > .env.template; sleep 1; cat .env.template; echo template=$?; \
         ln -s .env .env.sample; sleep 1; cat .env.sample; echo link=$?; cat .env; echo env=$?",
    )
    .await;
    let text = format!("{}{}", used.stdout.text, used.stderr.text);
    assert!(!text.contains("dummy-442"), "{text}");
    assert!(!text.contains("stopped this command"), "{text}");
    for line in [
        "API_KEY=example-442example=0",
        "API_KEY=sample-442sample=0",
        "tpltemplate=0",
        "link=1",
        "env=1",
    ] {
        assert!(used.stdout.text.contains(line), "{line} not in {text}");
    }

    std::fs::remove_file(work.join(".env.sample")).unwrap();
    std::fs::hard_link(work.join(".env"), work.join(".env.sample")).unwrap();
    let refused = run_in(&work, "cat .env.sample; echo read=$?").await;
    let text = format!("{}{}", refused.stdout.text, refused.stderr.text);
    assert!(!text.contains("dummy-442"), "{text}");
    assert!(!text.contains("read="), "{text}");
    assert!(text.contains("has other hard links"), "{text}");
    assert!(text.contains("delete the other names"), "{text}");
}

/// A folder for test files outside every writable root (not a temporary
/// folder): Cargo's target scratch folder, or the crate where a build tool
/// does not set it. Cargo creates the scratch folder when it builds the
/// test; a run from a nextest archive on another machine (CI's test shards)
/// has to create it.
fn outside_temp_root() -> PathBuf {
    // Under Bazel the compile-time scratch path is read-only in the test
    // sandbox; its per-test `TEST_TMPDIR` is outside `/tmp` and `TMPDIR` too.
    let root = std::env::var_os("TEST_TMPDIR")
        .filter(|_| option_env!("BAZEL_PACKAGE").is_some())
        .map(PathBuf::from)
        .or_else(|| option_env!("CARGO_TARGET_TMPDIR").map(PathBuf::from))
        .unwrap_or_else(|| PathBuf::from(env!("CARGO_MANIFEST_DIR")));
    std::fs::create_dir_all(&root).unwrap();
    root
}

/// Policies shaped like each security level, for the #535/#536 tests:
/// Permissive (a workspace-write config with extra writable roots, no
/// denials), Moderate (the same with a denied path) and Aggressive (as
/// Moderate, with the temporary folders not writable).
fn level_policies(
    workspace: &Path,
    extra_writable: &[&Path],
    denied: &Path,
) -> Vec<(&'static str, FileSystemSandboxPolicy)> {
    let workspace_write = |exclude_tmp: bool| {
        FileSystemSandboxPolicy::from_legacy_sandbox_policy_for_cwd(
            &codex_protocol::protocol::SandboxPolicy::WorkspaceWrite {
                writable_roots: extra_writable
                    .iter()
                    .map(|path| AbsolutePathBuf::from_absolute_path(path).unwrap())
                    .collect(),
                network_access: false,
                exclude_tmpdir_env_var: exclude_tmp,
                exclude_slash_tmp: exclude_tmp,
            },
            workspace,
        )
    };
    let denial = FileSystemSandboxEntry {
        path: FileSystemPath::Path {
            path: AbsolutePathBuf::from_absolute_path(denied).unwrap(),
        },
        access: FileSystemAccessMode::Deny,
        missing_path_behavior: None,
    };
    let permissive = workspace_write(false);
    let mut moderate = permissive.entries.clone();
    moderate.push(denial.clone());
    let mut aggressive = workspace_write(true).entries;
    aggressive.push(denial);
    vec![
        ("permissive", permissive),
        ("moderate", FileSystemSandboxPolicy::restricted(moderate)),
        (
            "aggressive",
            FileSystemSandboxPolicy::restricted(aggressive),
        ),
    ]
}

async fn run_level(
    workspace: &Path,
    policy: &FileSystemSandboxPolicy,
    script: &str,
) -> codex_protocol::exec_output::ExecToolCallOutput {
    let output = run_cmd_result_with_permission_profile_for_cwd(
        &["bash", "-c", script],
        AbsolutePathBuf::from_absolute_path(workspace).unwrap(),
        PermissionProfile::from_runtime_permissions(policy, NetworkSandboxPolicy::Restricted),
        create_env_from_core_vars(),
        LONG_TIMEOUT_MS,
        /*use_legacy_landlock*/ false,
    )
    .await;
    match output {
        Ok(output) => output,
        Err(err) => match err.details() {
            CodexErrorDetails::Sandbox(SandboxErr::Denied { output, .. }) => (**output).clone(),
            _ => panic!("command failed to run: {err:?}"),
        },
    }
}

/// #535 (Travis, 2026-10-12: option B), real bubblewrap: a writable root
/// that is a link into another root's read-only path (a workspace's
/// `.git/hooks`, or its `.git` itself) is not a grant at any level: the hooks
/// and `.git` stay read-only through it, while the workspace stays writable.
#[tokio::test]
async fn writable_links_into_a_read_only_path_are_not_grants_at_any_level() {
    if should_skip_bwrap_tests().await {
        eprintln!("skipping bwrap test: bwrap sandbox prerequisites are unavailable");
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let base = dir.path().canonicalize().unwrap();
    let workspace = base.join("ws");
    let hooks = workspace.join(".git").join("hooks");
    std::fs::create_dir_all(&hooks).unwrap();
    std::fs::write(hooks.join("pre-commit"), "#!/bin/sh\n").unwrap();
    let hooks_link = base.join("hooks-link");
    std::os::unix::fs::symlink(&hooks, &hooks_link).unwrap();
    let git_link = base.join("git-link");
    std::os::unix::fs::symlink(workspace.join(".git"), &git_link).unwrap();
    let denied = base.join("denied");
    std::fs::write(&denied, "x").unwrap();
    for (level, policy) in level_policies(&workspace, &[&hooks_link, &git_link], &denied) {
        assert_eq!(
            policy.writable_links_into_read_only_paths(&workspace).len(),
            2,
            "{level}"
        );
        let script = format!(
            "echo x >> '{hooks_link}/pre-commit'; echo hook=$?; echo x > '{hooks_link}/new'; \
             echo hooknew=$?; echo x > '{git_link}/HEAD'; echo gitlink=$?; \
             echo x > '{hooks}/direct'; echo direct=$?; echo x > '{workspace}/file'; echo ws=$?",
            hooks_link = hooks_link.display(),
            git_link = git_link.display(),
            hooks = hooks.display(),
            workspace = workspace.display(),
        );
        let output = run_level(&workspace, &policy, &script).await;
        for line in ["hook=1", "hooknew=1", "gitlink=1", "direct=1", "ws=0"] {
            assert!(
                output.stdout.text.lines().any(|got| got == line),
                "{level}: {line} not in {}\nstderr: {}",
                output.stdout.text,
                output.stderr.text
            );
        }
        assert_eq!(
            std::fs::read_to_string(hooks.join("pre-commit")).unwrap(),
            "#!/bin/sh\n",
            "{level}"
        );
        assert!(!hooks.join("new").exists() && !workspace.join(".git/HEAD").exists());
        std::fs::remove_file(workspace.join("file")).unwrap();
    }
}

/// #535: what stays a grant at every level: an explicit root spelled through
/// the read-only path (the person names `.git/hooks` itself), a link that
/// leads elsewhere in `/tmp`, a workspace spelled through a link, and a git
/// worktree with the main checkout as an extra root.
#[tokio::test]
async fn ordinary_roots_stay_writable_at_every_level() {
    if should_skip_bwrap_tests().await {
        eprintln!("skipping bwrap test: bwrap sandbox prerequisites are unavailable");
        return;
    }
    let dir = tempfile::Builder::new()
        .prefix("roots-535-")
        .tempdir_in("/tmp")
        .unwrap();
    let real = dir.path().canonicalize().unwrap();
    // The whole tree is spelled through a link, like `/tmp` on macOS: one
    // outside the writable roots (Linux refuses a read-only path spelled
    // through a link a command could change).
    let aliases = tempfile::tempdir_in(outside_temp_root()).unwrap();
    let alias = aliases.path().join("alias");
    std::os::unix::fs::symlink(&real, &alias).unwrap();
    let base = alias.clone();
    let main = base.join("main");
    let gitdir = main.join(".git").join("worktrees").join("wt");
    std::fs::create_dir_all(&gitdir).unwrap();
    std::fs::create_dir_all(main.join(".git/hooks")).unwrap();
    let worktree = base.join("wt");
    std::fs::create_dir_all(&worktree).unwrap();
    std::fs::write(
        worktree.join(".git"),
        format!("gitdir: {}\n", gitdir.display()),
    )
    .unwrap();
    let elsewhere = base.join("elsewhere");
    std::fs::create_dir_all(&elsewhere).unwrap();
    let link = base.join("cache");
    std::os::unix::fs::symlink(&elsewhere, &link).unwrap();
    let hooks = main.join(".git/hooks");
    let denied = base.join("denied");
    std::fs::write(&denied, "x").unwrap();
    for (level, policy) in level_policies(&worktree, &[&main, &hooks, &link], &denied) {
        assert_eq!(
            policy.writable_links_into_read_only_paths(&worktree),
            Vec::new(),
            "{level}"
        );
        let script = format!(
            "echo x > file; echo wt=$?; echo x > '{main}/file'; echo main=$?; \
             echo x > '{hooks}/explicit'; echo hooks=$?; echo x > '{link}/file'; echo link=$?",
            main = main.display(),
            hooks = hooks.display(),
            link = link.display(),
        );
        let output = run_level(&worktree, &policy, &script).await;
        for line in ["wt=0", "main=0", "hooks=0", "link=0"] {
            assert!(
                output.stdout.text.lines().any(|got| got == line),
                "{level}: {line} not in {}\nstderr: {}",
                output.stdout.text,
                output.stderr.text
            );
        }
        for written in [
            worktree.join("file"),
            main.join("file"),
            hooks.join("explicit"),
            elsewhere.join("file"),
        ] {
            std::fs::remove_file(written).unwrap();
        }
    }
}

/// #536 (Travis, 2026-10-12), real bubblewrap: at every level, Permissive
/// included, a configured writable root that does not exist yet and is
/// created by a command as a link elsewhere does not move write access
/// there; the next command is refused and says why. Removing the link lets
/// commands run again.
#[tokio::test]
async fn a_root_created_as_a_link_during_the_session_is_refused_at_every_level() {
    if should_skip_bwrap_tests().await {
        eprintln!("skipping bwrap test: bwrap sandbox prerequisites are unavailable");
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let base = dir.path().canonicalize().unwrap();
    // Outside every writable root at every level (not in a temporary folder).
    let outside = tempfile::tempdir_in(outside_temp_root()).unwrap();
    let outside = outside.path().canonicalize().unwrap();
    let denied = base.join("denied");
    std::fs::write(&denied, "x").unwrap();
    for level in ["permissive", "moderate", "aggressive"] {
        let workspace = base.join(level).join("ws");
        std::fs::create_dir_all(&workspace).unwrap();
        let target = workspace.join("target");
        let (_, policy) = level_policies(&workspace, &[&target], &denied)
            .into_iter()
            .find(|(shape, _)| *shape == level)
            .unwrap();
        codex_sandboxing::writable_root_ledger::record_writable_roots(&policy, &workspace);
        let made = run_level(
            &workspace,
            &policy,
            &format!("ln -s '{}' target; echo link=$?", outside.display()),
        )
        .await;
        assert!(made.stdout.text.contains("link=0"), "{level}: {made:?}");
        let refused = run_cmd_result_with_permission_profile_for_cwd(
            &["bash", "-c", "echo pwned > target/planted; echo wrote=$?"],
            AbsolutePathBuf::from_absolute_path(&workspace).unwrap(),
            PermissionProfile::from_runtime_permissions(&policy, NetworkSandboxPolicy::Restricted),
            create_env_from_core_vars(),
            LONG_TIMEOUT_MS,
            /*use_legacy_landlock*/ false,
        )
        .await;
        let err = match refused {
            Err(err) => err.to_string(),
            Ok(output) => panic!("{level}: not refused: {output:?}"),
        };
        assert!(
            err.contains("Refused to run the command")
                && err.contains(&target.display().to_string()),
            "{level}: {err}"
        );
        assert!(!outside.join("planted").exists(), "{level}");
        std::fs::remove_file(&target).unwrap();
        let again = run_level(&workspace, &policy, "echo ok > file; echo ws=$?").await;
        assert!(again.stdout.text.contains("ws=0"), "{level}: {again:?}");
    }
}

/// #536, real bubblewrap: the helper binds a writable root from the folder
/// the session pinned. A root swapped for a link after the check (between
/// the sandbox manager and the helper) is refused, and nothing is written
/// where the link leads.
#[tokio::test]
async fn the_helper_refuses_a_root_swapped_after_the_check() {
    if should_skip_bwrap_tests().await {
        eprintln!("skipping bwrap test: bwrap sandbox prerequisites are unavailable");
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let base = dir.path().canonicalize().unwrap();
    let workspace = base.join("ws");
    let target = workspace.join("target");
    std::fs::create_dir_all(&target).unwrap();
    let outside = tempfile::tempdir_in(outside_temp_root()).unwrap();
    let outside = outside.path().canonicalize().unwrap();
    let policy = FileSystemSandboxPolicy::restricted(vec![
        FileSystemSandboxEntry {
            path: FileSystemPath::Special {
                value: FileSystemSpecialPath::Root,
            },
            access: FileSystemAccessMode::Read,
            missing_path_behavior: None,
        },
        FileSystemSandboxEntry {
            path: FileSystemPath::Path {
                path: AbsolutePathBuf::from_absolute_path(&target).unwrap(),
            },
            access: FileSystemAccessMode::Write,
            missing_path_behavior: None,
        },
    ]);
    let profile =
        PermissionProfile::from_runtime_permissions(&policy, NetworkSandboxPolicy::Restricted);
    codex_sandboxing::writable_root_ledger::record_writable_roots(&policy, &workspace);
    let pinned =
        codex_sandboxing::writable_root_ledger::check_writable_roots(&policy, &workspace).unwrap();
    let mut args =
        codex_sandboxing::landlock::create_linux_sandbox_command_args_for_permission_profile(
            vec![
                "bash".to_string(),
                "-c".to_string(),
                "echo pwned > target/planted; echo wrote=$?".to_string(),
            ],
            &workspace,
            &profile,
            &workspace,
            /*use_legacy_landlock*/ false,
            /*allow_network_for_proxy*/ false,
        );
    codex_sandboxing::landlock::insert_pinned_writable_roots_arg(&mut args, &pinned);
    // Swapped after the check, before the helper runs.
    std::fs::rename(&target, workspace.join("moved")).unwrap();
    std::os::unix::fs::symlink(&outside, &target).unwrap();
    let output = std::process::Command::new(codex_linux_sandbox_exe())
        .args(&args)
        .current_dir(&workspace)
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(!output.status.success(), "{stdout}\n{stderr}");
    assert!(stderr.contains("Refused to run the command"), "{stderr}");
    assert!(!outside.join("planted").exists(), "{stdout}\n{stderr}");
}
