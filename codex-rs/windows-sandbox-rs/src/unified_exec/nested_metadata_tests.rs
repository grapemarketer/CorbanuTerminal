//! D283 (#1541): `.codex`/`.agents` below the top of a legacy session's
//! writable roots. New ones (or replaced ones) count as planted, and the
//! existing ones are protected like the top-level ones.

use super::*;
use pretty_assertions::assert_eq;

fn canonical(path: &Path) -> PathBuf {
    dunce::canonicalize(path).expect("canonical")
}

/// What Core asks right before loading `folder`.
fn counts_now(folder: &Path) -> bool {
    crate::project_metadata_counts_as_planted(folder).expect("records")
}

fn planted(folder: &Path) -> bool {
    crate::planted_project_metadata()
        .expect("planted records")
        .contains(&folder.to_path_buf())
}

/// A nested folder that appears, or is replaced (here by moving its parent
/// away and recreating it), while a session runs counts as planted at once
/// and after the session; one left alone doesn't, and neither does one
/// inside `.git`. A confirmed one counts again once replaced.
#[test]
fn legacy_nested_project_metadata_is_watched() {
    let _guard = legacy_process_test_guard();
    let test_root = user_owned_test_root();
    let root = test_root.path().join("root");
    for dir in ["kept\\.agents", "replaced\\.codex", ".git\\x", "deep\\a"] {
        fs::create_dir_all(root.join(dir)).expect("create test directory");
    }
    let root = canonical(&root);
    let new = root.join("deep\\a\\.codex");
    let replaced = root.join("replaced\\.codex");
    let moved = root.join("replaced-old\\.codex");
    let kept = root.join("kept\\.agents");
    let in_git = root.join(".git\\x\\.codex");

    let lease =
        crate::integrity_leases::acquire(std::slice::from_ref(&root), /*log_dir*/ None)
            .expect("lease");
    fs::create_dir(&new).expect("plant");
    fs::rename(root.join("replaced"), root.join("replaced-old")).expect("move the parent");
    fs::create_dir_all(&replaced).expect("recreate");
    fs::create_dir(&in_git).expect("inside .git");
    let live = (
        counts_now(&new),
        counts_now(&replaced),
        counts_now(&kept),
        // It may appear right after the check.
        counts_now(&root.join("absent\\.codex")),
        crate::confirm_planted_project_metadata(&new).is_err(),
    );
    drop(lease);
    let ended = (
        planted(&new),
        planted(&replaced),
        planted(&moved),
        planted(&kept),
        planted(&in_git),
        counts_now(&new),
        counts_now(&kept),
        counts_now(&root.join("absent\\.codex")),
    );
    let confirmed = crate::confirm_planted_project_metadata(&new).expect("confirm");
    drop(
        crate::integrity_leases::acquire(std::slice::from_ref(&root), /*log_dir*/ None)
            .expect("unchanged session"),
    );
    let after_unchanged = planted(&new);
    let lease =
        crate::integrity_leases::acquire(std::slice::from_ref(&root), /*log_dir*/ None)
            .expect("replacing session");
    fs::remove_dir(&new).expect("remove");
    fs::create_dir(&new).expect("replace");
    drop(lease);
    let after_replacing = planted(&new);

    assert_eq!(
        (live, ended, confirmed, after_unchanged, after_replacing),
        (
            (true, true, false, true, true),
            (true, true, true, false, false, true, false, false),
            true,
            false,
            true,
        )
    );
}

/// A nested folder that appeared while a session ran stays planted after
/// that session crashes (the sweep records it).
#[test]
fn legacy_nested_project_metadata_survives_a_crash() {
    use std::io::BufRead as _;
    let _guard = legacy_process_test_guard();
    let test_root = user_owned_test_root();
    let root = test_root.path().join("root");
    fs::create_dir_all(root.join("sub")).expect("create root");
    let dot_agents = canonical(&root).join("sub\\.agents");

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
    fs::create_dir(&dot_agents).expect("plant .agents");
    let live = (counts_now(&dot_agents), planted(&dot_agents));
    child.kill().expect("kill the crash child");
    child.wait().expect("wait for the crash child");
    crate::repair_stale_integrity_labels().expect("repair");
    let crashed = (counts_now(&dot_agents), planted(&dot_agents));

    assert_eq!((ready, live, crashed), (true, (true, false), (true, true)));
}

/// A legacy command can't change, delete, move or add to a `.codex` folder
/// deeper in its workspace, while it can write next to it; a `.agents`
/// folder it creates deeper in the workspace counts as planted.
#[test]
fn legacy_workspace_write_cannot_change_nested_project_metadata() {
    let _guard = legacy_process_test_guard();
    let test_root = user_owned_test_root();
    let workspace = test_root.path().join("workspace");
    let sub = workspace.join("sub");
    let dot_codex = sub.join(".codex");
    fs::create_dir_all(&dot_codex).expect("create nested .codex");
    let config = dot_codex.join("config.toml");
    fs::write(&config, "original").expect("seed config");
    let other = sub.join("other.txt");
    fs::write(&other, "before").expect("seed other file");
    let script = concat!(
        "echo after> \"%SUB%\\other.txt\"\r\n",
        "echo planted>> \"%SUB%\\.codex\\config.toml\"\r\n",
        "echo planted> \"%SUB%\\.codex\\hooks.json\"\r\n",
        "mkdir \"%SUB%\\.codex\\skills\"\r\n",
        "del /f /q \"%SUB%\\.codex\\config.toml\"\r\n",
        "ren \"%SUB%\\.codex\" codex-renamed\r\n",
        "move \"%SUB%\\.codex\" \"%SUB%\\codex-moved\"\r\n",
        "rd /s /q \"%SUB%\\.codex\"\r\n",
        "ren \"%SUB%\" sub-renamed\r\n",
        "move \"%SUB%\" \"%WORKSPACE%\\sub-moved\"\r\n",
        "mkdir \"%WORKSPACE%\\new\\.agents\"\r\n",
    );
    let runtime = current_thread_runtime();
    let exit_code = runtime.block_on(async {
        let codex_home = sandbox_home("legacy-nested-metadata");
        let script_path = codex_home.path().join("nested-metadata.cmd");
        fs::write(&script_path, format!("@echo off\r\n{script}exit /b 0\r\n"))
            .expect("write script");
        let env_map = HashMap::from([
            ("SUB".to_string(), sub.to_string_lossy().into_owned()),
            (
                "WORKSPACE".to_string(),
                workspace.to_string_lossy().into_owned(),
            ),
        ]);
        let spawned = spawn_windows_sandbox_session_legacy(
            &PermissionProfile::workspace_write(),
            workspace_roots_for(&workspace).as_slice(),
            codex_home.path(),
            vec![
                "C:\\Windows\\System32\\cmd.exe".to_string(),
                "/d".to_string(),
                "/c".to_string(),
                script_path.display().to_string(),
            ],
            &workspace,
            env_map,
            /*timeout_ms*/ Some(10_000),
            &[],
            &[],
            /*tty*/ false,
            /*stdin_open*/ false,
            /*use_private_desktop*/ true,
        )
        .await
        .expect("spawn legacy session");
        let (_stdout, exit_code) =
            collect_stdout_and_exit(spawned, codex_home.path(), Duration::from_secs(/*secs*/ 20))
                .await;
        exit_code
    });
    let new_agents = canonical(&workspace).join("new\\.agents");

    assert_eq!(
        (
            exit_code,
            fs::read_to_string(&other)
                .expect("other")
                .trim()
                .to_string(),
            fs::read_to_string(&config).ok(),
            dot_codex.join("hooks.json").exists(),
            dot_codex.join("skills").exists(),
            sub.join("codex-renamed").exists()
                || sub.join("codex-moved").exists()
                || workspace.join("sub-renamed").exists()
                || workspace.join("sub-moved").exists(),
            new_agents.is_dir(),
            planted(&new_agents),
            planted(&canonical(&sub).join(".codex")),
        ),
        (
            0,
            "after".to_string(),
            Some("original".to_string()),
            false,
            false,
            false,
            true,
            true,
            false,
        )
    );
}

/// A folder the end-of-session walk can't list hides nothing: everything
/// below it counts as planted. (Here the host holds it open without sharing,
/// so listing it fails; a command could deny the user listing it instead,
/// which an elevated host with backup rights would not notice.)
#[test]
fn legacy_unlistable_folder_counts_as_planted() {
    use std::os::windows::fs::OpenOptionsExt as _;
    const FILE_FLAG_BACKUP_SEMANTICS: u32 = 0x0200_0000;
    let _guard = legacy_process_test_guard();
    let test_root = user_owned_test_root();
    let root = test_root.path().join("root");
    fs::create_dir_all(&root).expect("create root");
    let root = canonical(&root);
    let x = root.join("x");
    let lease =
        crate::integrity_leases::acquire(std::slice::from_ref(&root), /*log_dir*/ None)
            .expect("lease");
    fs::create_dir_all(x.join(".codex")).expect("plant");
    let held = fs::OpenOptions::new()
        .read(true)
        .share_mode(0)
        .custom_flags(FILE_FLAG_BACKUP_SEMANTICS)
        .open(&x)
        .expect("hold x");
    let listable = fs::read_dir(&x).is_ok();
    drop(lease);
    drop(held);

    assert_eq!(
        (
            listable,
            planted(&x),
            counts_now(&x.join(".codex")),
            counts_now(&x.join("y\\.agents")),
        ),
        (false, true, true, true)
    );
}

/// `%TEMP%` (a writable root of every workspace-write session) isn't
/// watched for nested folders: every program creates folders there. The
/// workspace still is.
#[test]
fn legacy_temp_root_is_not_watched_for_nested_metadata() {
    let _guard = legacy_process_test_guard();
    let test_root = user_owned_test_root();
    let workspace = test_root.path().join("workspace");
    let temp = test_root.path().join("temp");
    fs::create_dir_all(&workspace).expect("create workspace");
    fs::create_dir_all(&temp).expect("create temp");
    let runtime = current_thread_runtime();
    let exit_code = runtime.block_on(async {
        let codex_home = sandbox_home("legacy-nested-temp");
        let script_path = temp.join("nested-temp.cmd");
        fs::write(
            &script_path,
            "@echo off\r\nmkdir \"%TEMP%\\proj\\.codex\"\r\nmkdir \"%WORKSPACE%\\sub\\.codex\"\r\nexit /b 0\r\n",
        )
        .expect("write script");
        let env_map = HashMap::from([
            ("TEMP".to_string(), temp.to_string_lossy().into_owned()),
            ("TMP".to_string(), temp.to_string_lossy().into_owned()),
            (
                "WORKSPACE".to_string(),
                workspace.to_string_lossy().into_owned(),
            ),
        ]);
        let spawned = spawn_windows_sandbox_session_legacy(
            &PermissionProfile::workspace_write(),
            workspace_roots_for(&workspace).as_slice(),
            codex_home.path(),
            vec![
                "C:\\Windows\\System32\\cmd.exe".to_string(),
                "/d".to_string(),
                "/c".to_string(),
                script_path.display().to_string(),
            ],
            &workspace,
            env_map,
            /*timeout_ms*/ Some(10_000),
            &[],
            &[],
            /*tty*/ false,
            /*stdin_open*/ false,
            /*use_private_desktop*/ true,
        )
        .await
        .expect("spawn legacy session");
        collect_stdout_and_exit(spawned, codex_home.path(), Duration::from_secs(/*secs*/ 20))
            .await
            .1
    });
    let in_temp = canonical(&temp).join("proj\\.codex");
    let in_workspace = canonical(&workspace).join("sub\\.codex");

    assert_eq!(
        (
            exit_code,
            in_temp.is_dir(),
            planted(&in_temp),
            counts_now(&in_temp),
            planted(&in_workspace),
        ),
        (0, true, false, false, true)
    );
}
