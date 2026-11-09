//! D425 (#1846): the elevated sandbox keeps the non-admin sandbox's records
//! of what its commands may have planted (D262, D283, D332, D420, D422), written
//! when the command ends, and labels nothing to do so.

use super::*;

fn canonical(path: &Path) -> PathBuf {
    dunce::canonicalize(path).expect("canonical")
}

/// A repository the person made, as `git init` leaves it, with a
/// `core.hooksPath` folder in its work tree (D422).
fn user_repo(dir: &Path) {
    for folder in [
        "hooks",
        "info",
        "objects\\info",
        "objects\\pack",
        "refs\\heads",
    ] {
        fs::create_dir_all(dir.join(".git").join(folder)).expect("create folder");
    }
    fs::write(dir.join(".git\\HEAD"), "ref: refs/heads/main\n").expect("HEAD");
    fs::write(
        dir.join(".git\\config"),
        "[core]\n\trepositoryformatversion = 0\n\thooksPath = .githooks\n",
    )
    .expect("config");
    fs::create_dir_all(dir.join(".githooks")).expect("hooks folder");
}

/// What the records say about `workspace` after a command that planted
/// `.codex`, `deep\.agents` and `.git` there, changed `userrepo`'s config and
/// added a hook to the folder its `core.hooksPath` names, and what the post-command notice was told (records since `mark`).
fn recorded(workspace: &Path, mark: u64) -> (Vec<PathBuf>, Vec<PathBuf>, Vec<PathBuf>) {
    let mut planted: Vec<PathBuf> = crate::planted_project_metadata()
        .expect("planted records")
        .into_iter()
        .filter(|path| path.starts_with(workspace))
        .collect();
    let mut changed: Vec<PathBuf> = crate::planted_git_changes()
        .expect("git records")
        .into_iter()
        .map(|(path, _)| path)
        .filter(|path| path.starts_with(workspace))
        .collect();
    planted.sort();
    changed.sort();
    let mut told: Vec<PathBuf> = crate::planted_records_since(mark)
        .into_iter()
        .map(|record| record.path)
        .filter(|path| path.starts_with(workspace))
        .collect();
    told.sort();
    told.dedup();
    (planted, changed, told)
}

const PLANT: &str = "(mkdir .codex) & (mkdir deep\\.agents) & (mkdir .git) & (echo [user]>> userrepo\\.git\\config) & (echo name = x>> userrepo\\.git\\config) & (echo exit 0> userrepo\\.githooks\\pre-commit) & (echo ok> done.txt)";

fn expected(workspace: &Path) -> (Vec<PathBuf>, Vec<PathBuf>, Vec<PathBuf>) {
    let mut planted = vec![
        workspace.join(".codex"),
        workspace.join(".git"),
        workspace.join("deep\\.agents"),
    ];
    planted.sort();
    let changed = vec![
        workspace.join("userrepo\\.git"),
        workspace.join("userrepo\\.githooks"),
    ];
    let mut told = planted.clone();
    told.extend(changed.clone());
    told.sort();
    (planted, changed, told)
}

fn check(name: &str, workspace: &Path, codex_home: &Path, mark: u64, exit_code: i32) {
    let labelled = crate::integrity_leases::recorded_roots()
        .expect("label records")
        .iter()
        .any(|root| root.starts_with(workspace));
    let got = recorded(workspace, mark);
    println!("\nsec-win-1846 {name}: exit={exit_code} labelled={labelled} records={got:?}");
    pretty_assertions::assert_eq!(
        (
            exit_code,
            workspace.join("done.txt").exists(),
            labelled,
            got
        ),
        (0, true, false, expected(workspace)),
        "{}",
        sandbox_log(codex_home)
    );
}

/// An elevated session (unified exec) records what its command planted
/// before it reports the exit.
#[test]
fn sec_win_1846_elevated_session_records_what_its_command_plants() {
    let Some(codex_home) = elevated_home("sec-1846-session") else {
        return;
    };
    let _guard = legacy_process_test_guard();
    let root = user_owned_test_root();
    let workspace = canonical(root.path());
    user_repo(&workspace.join("userrepo"));
    let mark = crate::planted_records_mark();
    let exit_code = run_elevated_session(&workspace, codex_home.path(), PLANT);
    record_setup(codex_home.path());
    check("session", &workspace, codex_home.path(), mark, exit_code);
}

/// Runs `script` with `cmd /c` in an elevated capture (the shell tool) in
/// `workspace`; its exit code.
fn run_elevated_capture(workspace: &Path, codex_home: &Path, script: &str) -> i32 {
    let cmd = system32_exe("cmd.exe").display().to_string();
    let permission_profile = PermissionProfile::workspace_write();
    let workspace_roots = workspace_roots_for(workspace);
    let result = crate::run_windows_sandbox_capture_for_permission_profile_elevated(
        crate::ElevatedSandboxProfileCaptureRequest {
            permission_profile: &permission_profile,
            workspace_roots: workspace_roots.as_slice(),
            codex_home,
            command: vec![cmd, "/d".to_string(), "/c".to_string(), script.to_string()],
            cwd: workspace,
            env_map: std::env::vars().collect(),
            timeout_ms: Some(90_000),
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
    .unwrap_or_else(|err| panic!("run elevated capture: {err:#}\n{}", sandbox_log(codex_home)));
    result.exit_code
}

/// Runs `script` with `cmd /c` in an elevated session (unified exec) in
/// `workspace`; its exit code.
fn run_elevated_session(workspace: &Path, codex_home: &Path, script: &str) -> i32 {
    let cmd = system32_exe("cmd.exe").display().to_string();
    let (_, exit_code) = current_thread_runtime().block_on(run_elevated(
        workspace,
        codex_home,
        vec![cmd, "/d".to_string(), "/c".to_string(), script.to_string()],
        &[],
        /*tty*/ false,
    ));
    exit_code
}

/// So does an elevated capture (the shell tool) before it returns.
#[test]
fn sec_win_1846_elevated_capture_records_what_its_command_plants() {
    let Some(codex_home) = elevated_home("sec-1846-capture") else {
        return;
    };
    let _guard = legacy_process_test_guard();
    let root = user_owned_test_root();
    let workspace = canonical(root.path());
    user_repo(&workspace.join("userrepo"));
    let mark = crate::planted_records_mark();
    let exit_code = run_elevated_capture(&workspace, codex_home.path(), PLANT);
    record_setup(codex_home.path());
    check("capture", &workspace, codex_home.path(), mark, exit_code);
}

/// The elevated session's records-only watch labels nothing, and its live
/// session file doesn't keep a legacy session's label in place: the label
/// goes back when that session ends. What appeared is recorded when the
/// watch ends. (No elevation needed.)
#[test]
fn sec_win_1846_records_only_watch_keeps_no_legacy_label_in_place() {
    let _guard = legacy_process_test_guard();
    let test_root = user_owned_test_root();
    let root = canonical(test_root.path());
    let roots = std::slice::from_ref(&root);
    let watch = crate::integrity_leases::watch(roots, roots, /*log_dir*/ None).expect("watch");
    let labelled_by_watch = crate::integrity_leases::recorded_roots()
        .expect("records")
        .contains(&root);
    drop(crate::integrity_leases::acquire(roots, /*log_dir*/ None).expect("legacy lease"));
    let labelled_after_legacy = crate::integrity_leases::recorded_roots()
        .expect("records")
        .contains(&root);
    fs::create_dir_all(root.join("a\\.codex")).expect("plant");
    let counted_live =
        crate::project_metadata_counts_as_planted(&root.join("a\\.codex")).expect("records");
    drop(watch);
    let planted = crate::planted_project_metadata()
        .expect("records")
        .contains(&root.join("a\\.codex"));

    pretty_assertions::assert_eq!(
        (
            labelled_by_watch,
            labelled_after_legacy,
            counted_live,
            planted
        ),
        (false, false, true, true)
    );
}

/// #1896 item 3: nested `.codex`/`.agents` folders that exist when an
/// elevated command starts, with what the person put in them.
fn nested_metadata(workspace: &Path) {
    for (file, text) in [
        ("deep\\.codex\\config.toml", "model = \"a\"\n"),
        ("other\\.agents\\skills\\one\\SKILL.md", "one\n"),
        ("kept\\.codex\\config.toml", "model = \"b\"\n"),
    ] {
        let path = workspace.join(file);
        fs::create_dir_all(path.parent().expect("parent")).expect("create folder");
        fs::write(path, text).expect("write");
    }
}

/// The command changes a file in `deep\.codex`, adds a skill deep in
/// `other\.agents`, and leaves `kept\.codex` as it was.
const CHANGE_INSIDE: &str = "(echo hooks = true>> deep\\.codex\\config.toml) & (mkdir other\\.agents\\skills\\two) & (echo two> other\\.agents\\skills\\two\\SKILL.md) & (echo ok> done.txt)";

fn check_inside(name: &str, workspace: &Path, codex_home: &Path, mark: u64, exit_code: i32) {
    let (planted, changed, told) = recorded(workspace, mark);
    println!("\nsec-win-1896 {name}: exit={exit_code} planted={planted:?} told={told:?}");
    let expected = vec![
        workspace.join("deep\\.codex"),
        workspace.join("other\\.agents"),
    ];
    pretty_assertions::assert_eq!(
        (
            exit_code,
            workspace.join("done.txt").exists(),
            planted,
            changed,
            told
        ),
        (0, true, expected.clone(), Vec::new(), expected),
        "{}",
        sandbox_log(codex_home)
    );
}

/// #1896 item 3: an elevated session flags a change inside a nested
/// `.codex`/`.agents` folder that was there before (the non-admin sandbox
/// keeps those unchanged, D283), and nothing for one left as it was.
#[test]
fn sec_win_1896_elevated_session_flags_changes_inside_nested_metadata() {
    let Some(codex_home) = elevated_home("sec-1896-session") else {
        return;
    };
    let _guard = legacy_process_test_guard();
    let root = user_owned_test_root();
    let workspace = canonical(root.path());
    nested_metadata(&workspace);
    let mark = crate::planted_records_mark();
    let exit_code = run_elevated_session(&workspace, codex_home.path(), CHANGE_INSIDE);
    record_setup(codex_home.path());
    check_inside("session", &workspace, codex_home.path(), mark, exit_code);
}

/// #1896 item 3: so does an elevated capture.
#[test]
fn sec_win_1896_elevated_capture_flags_changes_inside_nested_metadata() {
    let Some(codex_home) = elevated_home("sec-1896-capture") else {
        return;
    };
    let _guard = legacy_process_test_guard();
    let root = user_owned_test_root();
    let workspace = canonical(root.path());
    nested_metadata(&workspace);
    let mark = crate::planted_records_mark();
    let exit_code = run_elevated_capture(&workspace, codex_home.path(), CHANGE_INSIDE);
    record_setup(codex_home.path());
    check_inside("capture", &workspace, codex_home.path(), mark, exit_code);
}

/// #1896 item 3: the records-only watch counts a nested `.codex`/`.agents`
/// folder whose contents changed as planted, live and once it ends; one
/// rewritten with the same contents, or left alone, doesn't count. (No
/// elevation needed.)
#[test]
fn sec_win_1896_records_only_watch_flags_changes_inside_nested_metadata() {
    let _guard = legacy_process_test_guard();
    let test_root = user_owned_test_root();
    let root = canonical(test_root.path());
    nested_metadata(&root);
    fs::create_dir_all(root.join("same\\.codex")).expect("create");
    fs::write(root.join("same\\.codex\\config.toml"), "x\n").expect("write");
    let roots = std::slice::from_ref(&root);
    let watch = crate::integrity_leases::watch(roots, roots, /*log_dir*/ None).expect("watch");
    fs::write(root.join("deep\\.codex\\config.toml"), "hooks = 1\n").expect("change");
    fs::create_dir_all(root.join("other\\.agents\\skills\\two")).expect("add");
    fs::write(root.join("same\\.codex\\config.toml"), "x\n").expect("rewrite");
    let live: Vec<bool> = [
        "deep\\.codex",
        "other\\.agents",
        "kept\\.codex",
        "same\\.codex",
    ]
    .iter()
    .map(|folder| crate::project_metadata_counts_as_planted(&root.join(folder)).expect("records"))
    .collect();
    drop(watch);
    let mut planted: Vec<PathBuf> = crate::planted_project_metadata()
        .expect("records")
        .into_iter()
        .filter(|path| path.starts_with(&root))
        .collect();
    planted.sort();

    pretty_assertions::assert_eq!(
        (live, planted),
        (
            vec![true, true, false, false],
            vec![root.join("deep\\.codex"), root.join("other\\.agents")]
        )
    );
}
