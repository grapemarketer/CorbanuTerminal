//! D293 (#1542 items 1 and 2): links above an explicit read-only path
//! nested in a writable root, and such a path spelled through a linked root.

use super::*;
use codex_protocol::permissions::FileSystemAccessMode;
use codex_protocol::permissions::FileSystemPath;
use codex_protocol::permissions::FileSystemSandboxEntry;
use codex_protocol::permissions::FileSystemSandboxPolicy;
use codex_protocol::permissions::FileSystemSpecialPath;
use codex_protocol::permissions::NetworkSandboxPolicy;
use pretty_assertions::assert_eq;

fn junction(link: &Path, target: &Path) {
    let output = std::process::Command::new(system32_exe("cmd.exe"))
        .args(["/d", "/c", "mklink", "/J"])
        .arg(link)
        .arg(target)
        .output()
        .expect("mklink");
    assert!(
        output.status.success(),
        "mklink /J failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

/// Removes a junction (not its target) when dropped.
struct Unlink(PathBuf);

impl Drop for Unlink {
    fn drop(&mut self) {
        let _ = fs::remove_dir(&self.0);
    }
}

/// Runs `script` (cmd lines) as a legacy workspace-write command on
/// `workspace` with `profile` and the extra read-only `carve_outs`; returns
/// the start error, or the exit code and the sandbox log.
fn run_script(
    profile: &PermissionProfile,
    workspace: &Path,
    carve_outs: &[AbsolutePathBuf],
    env_map: HashMap<String, String>,
    script: &str,
) -> Result<i32, String> {
    let runtime = current_thread_runtime();
    runtime.block_on(async {
        let codex_home = sandbox_home("d293");
        let script_path = codex_home.path().join("d293.cmd");
        fs::write(&script_path, format!("@echo off\r\n{script}exit /b 0\r\n"))
            .expect("write script");
        let spawned = spawn_windows_sandbox_session_legacy(
            profile,
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
            /*timeout_ms*/ Some(10_000),
            &[],
            carve_outs,
            /*tty*/ false,
            /*stdin_open*/ false,
            /*use_private_desktop*/ true,
        )
        .await
        .map_err(|err| format!("{err:#}"))?;
        let (_stdout, exit_code) =
            collect_stdout_and_exit(spawned, codex_home.path(), Duration::from_secs(/*secs*/ 20))
                .await;
        Ok(exit_code)
    })
}

/// Item 1: a junction between the writable root and an explicit read-only
/// path stops the command. Before, D263's denies landed on the junction's
/// target, and the command could remove the junction and recreate the
/// read-only path behind it.
#[test]
fn legacy_link_above_read_only_path_stops_the_command() {
    let _guard = legacy_process_test_guard();
    let test_root = user_owned_test_root();
    let workspace = test_root.path().join("workspace");
    let real = workspace.join("real");
    fs::create_dir_all(real.join("b\\c")).expect("create carve-out");
    fs::write(real.join("b\\c\\keep.txt"), "original").expect("seed carve-out file");
    let link = workspace.join("a");
    junction(&link, &real);
    let carve_out = link.join("b\\c");
    let carve_outs = [AbsolutePathBuf::from_absolute_path(&carve_out).expect("absolute")];
    let started = run_script(
        &PermissionProfile::workspace_write(),
        &workspace,
        &carve_outs,
        HashMap::from([("A".to_string(), link.to_string_lossy().into_owned())]),
        concat!(
            "rmdir \"%A%\"\r\n",
            "mkdir \"%A%\\b\\c\"\r\n",
            "echo planted> \"%A%\\b\\c\\keep.txt\"\r\n",
        ),
    );

    assert_eq!(
        (
            started
                .as_ref()
                .err()
                .is_some_and(|err| err.contains("is a link")),
            crate::workspace_acl::is_reparse_point(&link),
            fs::read_to_string(carve_out.join("keep.txt")).ok(),
        ),
        (true, true, Some("original".to_string())),
        "{started:?}"
    );
}

/// Item 2: an explicit read-only path configured through a writable root
/// that is itself a junction at the top of a drive (so it is listed against
/// the root's resolved path) keeps D263's protection: the command can't move the folder above
/// it away and recreate it.
#[test]
fn legacy_read_only_path_through_a_linked_root_keeps_its_ancestors() {
    let _guard = legacy_process_test_guard();
    let test_root = user_owned_test_root();
    let workspace = test_root.path().join("workspace");
    fs::create_dir_all(workspace.join("a\\b\\c")).expect("create carve-out");
    let kept = workspace.join("a\\b\\c\\keep.txt");
    fs::write(&kept, "original").expect("seed carve-out file");
    // At the top of a drive: a link deeper down keeps the root's logical
    // spelling (`canonicalize_preserving_symlinks`), so only this one gets the
    // carve-out listed against the resolved root.
    let drive = std::env::var("SystemDrive").unwrap_or_else(|_| "C:".to_string());
    let linked = PathBuf::from(format!("{drive}\\")).join(format!(
        "d293-{}-{}",
        std::process::id(),
        TEST_HOME_COUNTER.fetch_add(1, Ordering::Relaxed)
    ));
    junction(&linked, &workspace);
    let _unlink = Unlink(linked.clone());
    let absolute = |path: PathBuf| AbsolutePathBuf::from_absolute_path(path).expect("absolute");
    let profile = PermissionProfile::from_runtime_permissions(
        &FileSystemSandboxPolicy::restricted(vec![
            FileSystemSandboxEntry::new(
                FileSystemPath::Special {
                    value: FileSystemSpecialPath::Root,
                },
                FileSystemAccessMode::Read,
            ),
            FileSystemSandboxEntry::new(
                FileSystemPath::Path {
                    path: absolute(linked.clone()),
                },
                FileSystemAccessMode::Write,
            ),
            FileSystemSandboxEntry::new(
                FileSystemPath::Path {
                    path: absolute(linked.join("a\\b\\c")),
                },
                FileSystemAccessMode::Read,
            ),
        ]),
        NetworkSandboxPolicy::Restricted,
    );
    let started = run_script(
        &profile,
        &linked,
        &[],
        HashMap::from([("W".to_string(), linked.to_string_lossy().into_owned())]),
        concat!(
            "echo after> \"%W%\\a\\other.txt\"\r\n",
            "ren \"%W%\\a\" a-renamed\r\n",
            "move \"%W%\\a\" \"%W%\\a-moved\"\r\n",
            "mkdir \"%W%\\a\\b\\c\"\r\n",
            "echo planted> \"%W%\\a\\b\\c\\keep.txt\"\r\n",
        ),
    );

    assert_eq!(
        (
            started,
            fs::read_to_string(&kept).ok(),
            fs::read_to_string(workspace.join("a\\other.txt")).ok(),
            ["a-renamed", "a-moved"]
                .iter()
                .filter(|name| workspace.join(name).exists())
                .copied()
                .collect::<Vec<_>>(),
        ),
        (
            Ok(0),
            Some("original".to_string()),
            Some("after\r\n".to_string()),
            Vec::<&str>::new(),
        )
    );
}

/// Item 1: which folders count. The root may be spelled through a link; a
/// folder below it that links back to the root, or anywhere else, counts.
#[test]
fn link_above_carve_out_finds_links_below_the_root() {
    let test_root = user_owned_test_root();
    let root = dunce::canonicalize(test_root.path())
        .expect("canonical")
        .join("root");
    fs::create_dir_all(root.join("real\\b\\c")).expect("create");
    fs::create_dir_all(root.join("plain\\b\\c")).expect("create");
    junction(&root.join("a"), &root.join("real"));
    junction(&root.join("alias"), &root);
    let linked_root = root.with_file_name("linked-root");
    junction(&linked_root, &root);
    let find = |path: &Path| crate::workspace_acl::link_above_carve_out(path, &root);

    assert_eq!(
        (
            find(&root.join("plain\\b\\c")),
            find(&root.join("a\\b\\c")),
            find(&root.join("alias\\plain\\b\\c")),
            find(&linked_root.join("plain\\b\\c")),
            find(&linked_root.join("a\\b\\c")),
        ),
        (
            None,
            Some(root.join("a")),
            Some(root.join("alias")),
            None,
            Some(linked_root.join("a")),
        )
    );
}
