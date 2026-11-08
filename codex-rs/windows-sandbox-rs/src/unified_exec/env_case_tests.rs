//! #1805: a command's environment holds each name once, whatever case the
//! caller spelled it in. Core passes the environment `std::env::vars()`
//! gives, where Windows spells the search path `Path`; both backends used to
//! add the parent's `PATH` beside it (the legacy one with its no-network
//! folder in front), and Windows PowerShell's `Start-Process` then failed
//! with "Item has already been added. Key in dictionary: 'Path' Key being
//! added: 'PATH'". The caller's `Path` must also be the one that counts: it
//! holds a folder the parent's lacks.

use super::command_identity::elevated::elevated_home;
use super::command_identity::elevated::run_elevated_with_env_map;
use super::command_identity::run_legacy_with_env_map;
use super::*;

const MARKER: &str = "win-1805-marker-found";
const STARTED: &str = "win-1805 start-process ran";
/// Printed when the process `Start-Process` started wrote its file.
const CHILD: &str = "child wrote: True";

/// The parent's environment with the search path spelled `Path` only, a
/// folder holding `marker1805.cmd` in front, and its own `TEMP`.
fn caller_env(tools: &Path, temp: &Path) -> HashMap<String, String> {
    let mut env_map: HashMap<String, String> = std::env::vars()
        .filter(|(key, _)| !key.eq_ignore_ascii_case("PATH"))
        .collect();
    let parent = std::env::var("PATH").expect("PATH");
    env_map.insert("Path".to_string(), format!("{};{parent}", tools.display()));
    for key in ["TEMP", "TMP"] {
        env_map.retain(|name, _| !name.eq_ignore_ascii_case(key));
        env_map.insert(key.to_string(), temp.display().to_string());
    }
    env_map
}

/// A cmd script that lists its variables whose names start with `PATH`, runs
/// the marker through the search path and starts a process with Windows
/// PowerShell's `Start-Process`.
fn probe_argv(root: &Path, tools: &Path) -> Vec<String> {
    fs::create_dir_all(tools).expect("create the tools folder");
    fs::write(tools.join("marker1805.cmd"), format!("@echo {MARKER}\r\n"))
        .expect("write the marker");
    let ps1 = root.join("start-process-1805.ps1");
    fs::write(
        &ps1,
        "$ErrorActionPreference = 'Stop'\r\n\
         try {\r\n\
           $p = Start-Process -FilePath \"$env:SystemRoot\\System32\\cmd.exe\" -ArgumentList '/c', 'echo ran>win-1805-child.txt' -Wait -PassThru -NoNewWindow\r\n\
           \"win-1805 start-process ran (exit=$($p.ExitCode), child wrote: $(Test-Path win-1805-child.txt))\"\r\n\
         } catch {\r\n\
           \"win-1805 start-process failed: $($_.Exception.Message)\"\r\n\
         }\r\n",
    )
    .expect("write the PowerShell script");
    let powershell = system32_exe(r"WindowsPowerShell\v1.0\powershell.exe");
    let script = format!(
        "@echo off\r\n\
         set PATH\r\n\
         call marker1805\r\n\
         \"{}\" -NoProfile -NonInteractive -ExecutionPolicy Bypass -File \"{}\"\r\n\
         exit /b 0\r\n",
        powershell.display(),
        ps1.display()
    );
    let script_path = root.join("env-case-1805.cmd");
    fs::write(&script_path, script).expect("write the probe script");
    vec![
        system32_exe("cmd.exe").display().to_string(),
        "/d".to_string(),
        "/c".to_string(),
        script_path.display().to_string(),
    ]
}

fn assert_one_path(backend: &str, stdout: &str, exit_code: i32, codex_home: &Path) {
    let names: Vec<&str> = stdout
        .lines()
        .filter_map(|line| line.split_once('=').map(|(name, _)| name))
        .filter(|name| name.eq_ignore_ascii_case("PATH"))
        .collect();
    assert!(
        exit_code == 0 && names.len() == 1,
        "#1805: the {backend} command's environment spells its search path as {names:?} \
         (exit={exit_code}), not once\nstdout={stdout:?}\n{}",
        sandbox_log(codex_home)
    );
    assert!(
        stdout.contains(MARKER),
        "#1805: the {backend} command's search path is not the caller's `Path`\n\
         stdout={stdout:?}"
    );
    // The exit code is printed but not checked: in the legacy backend under
    // an elevated runner, `-Wait -PassThru` once gave none (CI run
    // 42451153807), an issue apart from #1805.
    assert!(
        stdout.contains(STARTED) && stdout.contains(CHILD),
        "#1805: Windows PowerShell's Start-Process failed in the {backend} command\n\
         stdout={stdout:?}"
    );
    println!(
        "win-1805 {backend}: one PATH ({}), the caller's, and Start-Process ran",
        names[0]
    );
}

#[test]
fn win_1805_legacy_command_gets_one_path_and_start_process_runs() {
    let _guard = legacy_process_test_guard();
    let root = user_owned_test_root();
    let temp = user_owned_test_root();
    let codex_home = sandbox_home("win-1805-legacy");
    let tools = root.path().join("tools1805");
    let argv = probe_argv(root.path(), &tools);
    let (stdout, exit_code) = current_thread_runtime().block_on(run_legacy_with_env_map(
        root.path(),
        codex_home.path(),
        argv,
        caller_env(&tools, temp.path()),
        /*tty*/ false,
    ));
    assert_one_path("legacy", &stdout, exit_code, codex_home.path());
}

#[test]
fn win_1805_elevated_command_gets_one_path_and_start_process_runs() {
    let Some(codex_home) = elevated_home("win-1805-elevated") else {
        return;
    };
    let _guard = legacy_process_test_guard();
    let root = user_owned_test_root();
    let temp = user_owned_test_root();
    let tools = root.path().join("tools1805");
    let argv = probe_argv(root.path(), &tools);
    let (stdout, exit_code) = current_thread_runtime().block_on(run_elevated_with_env_map(
        root.path(),
        codex_home.path(),
        argv,
        caller_env(&tools, temp.path()),
    ));
    assert_one_path("elevated", &stdout, exit_code, codex_home.path());
}
