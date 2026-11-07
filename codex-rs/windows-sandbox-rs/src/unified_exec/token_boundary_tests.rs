//! #1770 (#1752 follow-up): a sandboxed command's own token, not the
//! folder's permissions, keeps it out of a folder outside its writable roots
//! that any signed-in account may write to. Both backends run commands with a
//! write-restricted token, so a write also needs the folder to grant one of
//! the token's restricting SIDs (the capability SIDs, the command's SID, the
//! logon SID, Everyone and, for the elevated backend, the sandbox account).
//! `Users` and `Authenticated Users` are none of them. These tests fail if a
//! change adds such a group (or the user) to the restricting SIDs, or stops
//! restricting the token.
//!
//! The target is a fresh folder under `CODEX_SEC_WIN_1770_SHARED_ROOT` (in
//! CI the runner's `D:\tmp`, which any signed-in account may write to), else
//! under `TEMP`. On top of what it inherits it grants `Users` and
//! `Authenticated Users` Modify, so any account may write there whatever the
//! root allows, and it is labelled Low: the legacy backend's Low integrity
//! (#158) alone refuses writes to an unlabelled folder and would hide a
//! weaker token. The command's `TEMP` is elsewhere, so the target is in none
//! of its writable roots.

use super::command_identity::elevated::elevated_home;
use super::command_identity::elevated::run_elevated;
use super::command_identity::run_legacy;
use super::*;

const SHARED_ROOT_ENV: &str = "CODEX_SEC_WIN_1770_SHARED_ROOT";
const EXISTING: &str = "existing.txt";
const BEFORE: &str = "before";

fn icacls(path: &Path, args: &[&str]) -> String {
    let output = std::process::Command::new(system32_exe("icacls.exe"))
        .arg(path)
        .args(args)
        .output()
        .expect("run icacls");
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(output.status.success(), "icacls {args:?} failed: {text}");
    text
}

/// A fresh folder under the shared root that any account may write to,
/// labelled Low, holding a file any account may modify. The user's own SID
/// is a restricting SID of neither backend's token.
fn shared_target() -> TempDir {
    let root = std::env::var_os(SHARED_ROOT_ENV)
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir);
    assert!(
        root.is_dir(),
        "{SHARED_ROOT_ENV}: {} is not a folder",
        root.display()
    );
    let target = tempfile::Builder::new()
        .prefix("sec-win-1770-")
        .tempdir_in(&root)
        .expect("create the shared target");
    // The user owns the folder, so it may change its DACL, but a normal
    // session may lack `WRITE_OWNER`, which the Low label needs, on a folder
    // in `D:\tmp` (#1689): Full control for the user grants it.
    let user = format!("*{}:(OI)(CI)F", current_user_sid());
    icacls(
        target.path(),
        &[
            "/grant",
            "*S-1-5-32-545:(OI)(CI)M",
            "*S-1-5-11:(OI)(CI)M",
            user.as_str(),
        ],
    );
    icacls(target.path(), &["/setintegritylevel", "(OI)(CI)L"]);
    fs::write(target.path().join(EXISTING), BEFORE).expect("write the existing file");
    println!(
        "sec-win-1770: target {}\n{}",
        target.path().display(),
        icacls(target.path(), &[])
    );
    target
}

/// A cmd script, in the workspace, that writes in the workspace and then
/// tries three writes in `target`.
fn probe_argv(root: &Path, target: &Path) -> Vec<String> {
    let (root, target) = (root.display(), target.display());
    let script = format!(
        "@echo off\r\n\
         echo inside>\"{root}\\inside.txt\"\r\n\
         (echo outside>\"{target}\\created.txt\") 2>&1\r\n\
         mkdir \"{target}\\created-dir\" 2>&1\r\n\
         (echo outside>>\"{target}\\{EXISTING}\") 2>&1\r\n\
         exit /b 0\r\n"
    );
    let script_path = format!("{root}\\sec-win-1770.cmd");
    fs::write(&script_path, script).expect("write the probe script");
    vec![
        system32_exe("cmd.exe").display().to_string(),
        "/d".to_string(),
        "/c".to_string(),
        script_path,
    ]
}

/// `TEMP` and `TMP` for the command: a folder of their own, not the target's
/// root.
fn command_temp(temp: &Path) -> [(&'static str, String); 2] {
    let temp = temp.display().to_string();
    [("TEMP", temp.clone()), ("TMP", temp)]
}

fn assert_refused(
    backend: &str,
    root: &Path,
    target: &Path,
    stdout: &str,
    exit_code: i32,
    codex_home: &Path,
) {
    assert!(
        exit_code == 0 && root.join("inside.txt").is_file(),
        "the {backend} command didn't run or couldn't write in its workspace: \
         exit={exit_code} stdout={stdout:?}\n{}",
        sandbox_log(codex_home)
    );
    let existing = fs::read_to_string(target.join(EXISTING)).expect("read the existing file");
    let wrote: Vec<&str> = [
        ("created a file", target.join("created.txt").exists()),
        ("created a folder", target.join("created-dir").exists()),
        ("appended to a file", existing != BEFORE),
    ]
    .into_iter()
    .filter_map(|(what, done)| done.then_some(what))
    .collect();
    assert!(
        wrote.is_empty(),
        "#1770: a {backend} command {wrote:?} in {}, outside its writable roots, \
         which only Users and Authenticated Users grant: its token is not restricted\n\
         stdout={stdout:?}",
        target.display()
    );
    // Refused for lack of access, not for a wrong path: cmd reports each.
    let denied = stdout.matches("Access is denied.").count();
    assert_eq!(
        denied,
        3,
        "the {backend} command's writes in {} failed, but not all for lack of access: \
         stdout={stdout:?}",
        target.display()
    );
    println!(
        "sec-win-1770 {backend}: creating a file, creating a folder and appending were refused in {}",
        target.display()
    );
}

#[test]
fn sec_win_1770_legacy_command_cannot_write_to_a_shared_folder() {
    let _guard = legacy_process_test_guard();
    let root = user_owned_test_root();
    let temp = user_owned_test_root();
    let target = shared_target();
    let codex_home = sandbox_home("sec-win-1770-legacy");
    let runtime = current_thread_runtime();
    let (stdout, exit_code) = runtime.block_on(run_legacy(
        root.path(),
        codex_home.path(),
        probe_argv(root.path(), target.path()),
        &command_temp(temp.path()),
    ));
    assert_refused(
        "legacy",
        root.path(),
        target.path(),
        &stdout,
        exit_code,
        codex_home.path(),
    );
}

#[test]
fn sec_win_1770_elevated_command_cannot_write_to_a_shared_folder() {
    let Some(codex_home) = elevated_home("sec-win-1770-elevated") else {
        return;
    };
    let _guard = legacy_process_test_guard();
    let root = user_owned_test_root();
    let temp = user_owned_test_root();
    let target = shared_target();
    let runtime = current_thread_runtime();
    let (stdout, exit_code) = runtime.block_on(run_elevated(
        root.path(),
        codex_home.path(),
        probe_argv(root.path(), target.path()),
        &command_temp(temp.path()),
        /*tty*/ false,
    ));
    assert_refused(
        "elevated",
        root.path(),
        target.path(),
        &stdout,
        exit_code,
        codex_home.path(),
    );
}
