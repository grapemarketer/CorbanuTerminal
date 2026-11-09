//! #1863 on a real Windows host: `command/exec` (`build_exec_request` +
//! `execute_env`) with a `\\localhost\...` network (UNC) working folder used
//! to panic inside the `url` crate before any sandbox ran. It now fails with
//! an error, and the command doesn't run, under both Windows sandboxes.
//!
//! The working folder is the workspace through the administrative share
//! `\\localhost\<drive>$` (an elevated session), or through the share
//! `\\localhost\<CODEX_TEST_UNC_SHARE>` over `CODEX_TEST_UNC_SHARE_DIR` (a
//! normal session, where administrative shares are refused).

// The probe's output is the evidence of the measured runs (`--nocapture`).
#![allow(clippy::print_stderr, clippy::expect_used)]

use super::ExecCapturePolicy;
use super::ExecExpiration;
use super::ExecParams;
use super::build_exec_request;
use crate::sandboxing::SandboxPermissions;
use codex_protocol::config_types::WindowsSandboxLevel;
use codex_protocol::models::PermissionProfile;
use codex_utils_absolute_path::AbsolutePathBuf;
use futures::FutureExt;
use std::collections::HashMap;
use std::ffi::OsString;
use std::panic::AssertUnwindSafe;
use std::path::Path;
use std::path::PathBuf;

struct EnvGuard {
    key: &'static str,
    original: Option<OsString>,
}

impl EnvGuard {
    fn set(key: &'static str, value: &Path) -> Self {
        let original = std::env::var_os(key);
        // SAFETY: serialized with the other CODEX_HOME tests; CI runs it
        // filtered with `--test-threads=1`.
        unsafe { std::env::set_var(key, value) };
        Self { key, original }
    }
}

impl Drop for EnvGuard {
    fn drop(&mut self) {
        // SAFETY: as in `set`.
        unsafe {
            match &self.original {
                Some(value) => std::env::set_var(self.key, value),
                None => std::env::remove_var(self.key),
            }
        }
    }
}

/// `real` through `\\<host>`: the share from the environment, else the
/// administrative share of its drive.
fn unc(host: &str, real: &Path) -> PathBuf {
    let real_text = real.display().to_string();
    match (
        std::env::var("CODEX_TEST_UNC_SHARE"),
        std::env::var("CODEX_TEST_UNC_SHARE_DIR"),
    ) {
        (Ok(share), Ok(dir)) => {
            let dir = dunce::canonicalize(&dir).expect("canonical share folder");
            let rest = real.strip_prefix(&dir).expect("workspace under the share");
            PathBuf::from(format!("\\\\{host}\\{share}\\{}", rest.display()))
        }
        _ => PathBuf::from(format!(
            "\\\\{host}\\{}$\\{}",
            &real_text[..1],
            &real_text[3..]
        )),
    }
}

/// Copies the elevated sandbox's helpers next to the test binary, where the
/// sandbox looks for them (as `launch_contract_windows_tests` does).
fn stage_windows_sandbox_helpers() {
    let exe = std::env::current_exe().expect("test binary");
    let resources = exe.parent().expect("test dir").join("codex-resources");
    std::fs::create_dir_all(&resources).expect("resources dir");
    for helper in ["codex-windows-sandbox-setup", "codex-command-runner"] {
        let source = codex_utils_cargo_bin::cargo_bin(helper).expect("sandbox helper built");
        let destination = resources.join(format!("{helper}.exe"));
        if let Err(error) = std::fs::copy(&source, &destination)
            && !destination.exists()
        {
            panic!("stage {helper}: {error}");
        }
    }
}

/// `command/exec` of a command writing `ran.txt` in the UNC working folder,
/// by its full UNC path (cmd doesn't use a UNC working folder: it falls back
/// to the Windows folder): the error it returned, or what it printed. The
/// non-admin sandbox gets `temp` as its `TEMP` and `TMP`: it grants its temp
/// folder to this test's new capability SID for good, so the real one would
/// gain an entry on every run (#1890).
async fn command_exec(
    level: WindowsSandboxLevel,
    cwd: &AbsolutePathBuf,
    profile: &PermissionProfile,
    temp: &Path,
) -> Result<String, String> {
    let env = if level == WindowsSandboxLevel::RestrictedToken {
        ["TEMP", "TMP"]
            .map(|key| (key.to_string(), temp.display().to_string()))
            .into()
    } else {
        HashMap::new()
    };
    let request = build_exec_request(
        ExecParams {
            command: vec![
                "cmd.exe".to_string(),
                "/D".to_string(),
                "/C".to_string(),
                format!("echo ran> \"{}\"", cwd.join("ran.txt").display()),
            ],
            cwd: cwd.clone(),
            expiration: ExecExpiration::from(120_000),
            capture_policy: ExecCapturePolicy::ShellTool,
            env,
            network: None,
            network_environment_id: None,
            sandbox_permissions: SandboxPermissions::UseDefault,
            windows_sandbox_level: level,
            windows_sandbox_private_desktop: false,
            justification: None,
            arg0: None,
        },
        profile,
        cwd,
        std::slice::from_ref(cwd),
        &None,
        /*use_legacy_landlock*/ false,
    )
    .map_err(|err| format!("refused: {err}"))?;
    let output = crate::sandboxing::execute_env(request, /*stdout_stream*/ None)
        .await
        .map_err(|err| format!("failed: {err}"))?;
    Ok(format!("{}{}", output.stdout.text, output.stderr.text))
}

#[tokio::test]
#[serial_test::serial(codex_home)]
async fn sec_win_1863_command_exec_localhost_unc_cwd_fails_closed() {
    let elevated = std::env::var_os("CODEX_TEST_1863_ELEVATED").is_some();
    // Inside the user profile, like the default `~/.codex`: the elevated
    // sandbox's setup grants its user read access to profile folders.
    let profile_dir = std::env::var_os("USERPROFILE").expect("USERPROFILE");
    let codex_home_dir = tempfile::Builder::new()
        .prefix("sec-1863-codex-home-")
        .tempdir_in(profile_dir)
        .expect("codex home");
    let _codex_home = EnvGuard::set("CODEX_HOME", codex_home_dir.path());
    let mut levels = vec![("non-admin", WindowsSandboxLevel::RestrictedToken)];
    if elevated {
        stage_windows_sandbox_helpers();
        levels.push(("elevated", WindowsSandboxLevel::Elevated));
    }

    let temp = tempfile::tempdir().expect("workspace parent");
    let session_temp = tempfile::tempdir().expect("session temp");
    let real = dunce::canonicalize(temp.path())
        .expect("canonical workspace parent")
        .join("Workspace Folder");
    std::fs::create_dir_all(&real).expect("workspace");
    // `\\localhost` is the case #1863 found. `\\127.0.0.1` is the same
    // folder through another server name, for comparison.
    let mut outcomes = Vec::new();
    for host in ["localhost", "127.0.0.1"] {
        let unc = unc(host, &real);
        assert!(unc.is_dir(), "{} can't be reached here", unc.display());
        let cwd = AbsolutePathBuf::from_absolute_path(&unc).expect("absolute UNC cwd");
        let profile = PermissionProfile::workspace_write()
            .materialize_project_roots_with_workspace_roots(std::slice::from_ref(&cwd));
        for (sandbox, level) in &levels {
            let _ = std::fs::remove_file(real.join("ran.txt"));
            // Record a panic as an outcome rather than end the probe.
            let outcome =
                match AssertUnwindSafe(command_exec(*level, &cwd, &profile, session_temp.path()))
                    .catch_unwind()
                    .await
                {
                    Ok(Ok(output)) => format!("ran: {}", output.trim()),
                    Ok(Err(err)) => err,
                    Err(_) => "panicked".to_string(),
                };
            let written = real.join("ran.txt").exists();
            eprintln!(
                "sec-win-1863 {sandbox}, cwd {}: {outcome} (ran.txt written: {written})",
                unc.display()
            );
            outcomes.push((host, *sandbox, outcome, written));
        }
    }
    for (host, sandbox, outcome, written) in outcomes {
        if host == "localhost" {
            // Refused before any sandbox runs, as release builds already did
            // (with an unclear error), instead of a panic in debug builds.
            assert!(
                outcome.starts_with("refused: ") && outcome.contains("network (UNC) path"),
                "{sandbox}: {outcome}"
            );
            assert!(!written, "{sandbox}: the command ran");
        } else {
            // Unchanged: the command starts sandboxed. Whether it could
            // write through the share is measured only (printed above).
            assert!(outcome.starts_with("ran: "), "{host}, {sandbox}: {outcome}");
        }
    }
}
