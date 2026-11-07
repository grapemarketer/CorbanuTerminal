//! #304: a deny-read path a later launch no longer lists loses exactly the
//! entry the sync added. #301: never while another process has the
//! secretless launch contract armed on the same `CODEX_HOME`.

use super::SECRETLESS_LAUNCH_LOCK_FILE;
use super::sync_persistent_deny_read_acls;
use crate::acl::add_allow_ace;
use crate::acl::add_deny_read_ace;
use crate::acl::add_deny_read_ace_for_new_files;
use crate::acl::ensure_explicit_deny_read_ace;
use crate::acl::has_explicit_deny_read_ace;
use crate::deny_read_sessions::DenyReadSessions;
use crate::deny_read_sessions::Registration;
use crate::deny_read_targets::DenyReadRule;
use crate::deny_read_targets::DenyReadTargets;
use crate::resolve_windows_deny_read_targets;
use crate::setup::sandbox_secrets_dir;
use crate::token::LocalSid;
use crate::winutil::to_wide;
use codex_protocol::permissions::FileSystemAccessMode;
use codex_protocol::permissions::FileSystemPath;
use codex_protocol::permissions::FileSystemSandboxEntry;
use codex_protocol::permissions::FileSystemSandboxPolicy;
use codex_utils_absolute_path::AbsolutePathBuf;
use pretty_assertions::assert_eq;
use std::ffi::c_void;
use std::io::BufRead as _;
use std::io::BufReader;
use std::io::Read as _;
use std::io::Write as _;
use std::os::windows::fs::OpenOptionsExt as _;
use std::path::Path;
use std::path::PathBuf;
use std::process::Child;
use std::process::Command;
use std::process::Stdio;
use windows_sys::Win32::Foundation::ERROR_SUCCESS;
use windows_sys::Win32::Foundation::HLOCAL;
use windows_sys::Win32::Foundation::LocalFree;
use windows_sys::Win32::Security::Authorization::ConvertSecurityDescriptorToStringSecurityDescriptorW;
use windows_sys::Win32::Security::Authorization::GetNamedSecurityInfoW;
use windows_sys::Win32::Security::Authorization::SDDL_REVISION_1;
use windows_sys::Win32::Security::Authorization::SE_FILE_OBJECT;
use windows_sys::Win32::Security::DACL_SECURITY_INFORMATION;

const FILE_FLAG_BACKUP_SEMANTICS: u32 = 0x0200_0000;
const SANDBOX_GROUP: &str = "S-1-5-21-2718281828-3141592653-1618033988-1001";
const OTHER_SID: &str = "S-1-5-21-2718281828-3141592653-1618033988-1002";
const ARMED_HOME_ENV: &str = "CODEX_SEC_WIN_301_ARMED_HOME";
const ARMED_ENTRY: &str = "deny_read_state::tests::sec_win_301_armed_session_entry";
const ARMED_LINE: &str = "sec-win-301: armed";

struct Home {
    _dir: tempfile::TempDir,
    /// The test's own session registry (#323), so no session elsewhere on
    /// the machine keeps its syncs from removing entries.
    _registry: Option<tempfile::TempDir>,
    sessions: DenyReadSessions,
    codex_home: PathBuf,
    /// A protected directory, and a directory in it.
    secret: PathBuf,
    nested: PathBuf,
}

fn home() -> Home {
    let registry = tempfile::tempdir().expect("session registry");
    let sessions = DenyReadSessions::in_test_registry(registry.path(), /*own*/ None);
    Home {
        _registry: Some(registry),
        ..home_in(sessions)
    }
}

/// A home whose syncs check `sessions`.
fn home_in(sessions: DenyReadSessions) -> Home {
    let dir = tempfile::tempdir().expect("codex home");
    let codex_home = dunce::canonicalize(dir.path()).expect("canonical codex home");
    let secret = codex_home.join("vault-secret");
    std::fs::create_dir(&secret).expect("secret dir");
    let nested = secret.join("nested");
    std::fs::create_dir(&nested).expect("nested dir");
    Home {
        _dir: dir,
        _registry: None,
        sessions,
        codex_home,
        secret,
        nested,
    }
}

/// One launch's deny-read sync for the sandbox group, each path its own
/// exact rule (the rule is gone when a later launch does not list it).
fn sync(home: &Home, paths: &[PathBuf], group: &LocalSid) {
    let targets = DenyReadTargets::from_exact_paths(
        paths
            .iter()
            .map(|path| AbsolutePathBuf::from_absolute_path(path).expect("absolute path")),
    );
    sync_targets(home, &targets, group);
}

/// One launch's deny-read sync with the given rules.
fn sync_targets(home: &Home, targets: &DenyReadTargets, group: &LocalSid) {
    // SAFETY: `group` is a valid SID for the call.
    unsafe {
        sync_persistent_deny_read_acls(
            &home.codex_home,
            SANDBOX_GROUP,
            Some(targets),
            group.as_ptr(),
            Some(&home.sessions),
        )
    }
    .expect("sync deny-read ACLs");
}

/// The sync's record of the entries it added.
fn state(home: &Home) -> String {
    std::fs::read_to_string(sandbox_secrets_dir(&home.codex_home).join("deny_read_acl_rules.json"))
        .unwrap_or_default()
}

/// Whether the sync state still lists the protected directory.
fn recorded(home: &Home) -> bool {
    state(home).contains("vault-secret")
}

#[test]
fn sec_win_304_sync_removes_exactly_the_deny_it_added() {
    let home = home();
    let group = LocalSid::from_string(SANDBOX_GROUP).expect("group SID");
    let other = LocalSid::from_string(OTHER_SID).expect("other SID");
    // Entries the removal must leave: an allow for the same SID (which
    // `REVOKE_ACCESS` would take), another SID's deny, and another kind of
    // deny for the same SID.
    // SAFETY: valid SIDs and existing paths.
    unsafe {
        assert!(add_allow_ace(&home.secret, group.as_ptr()).expect("allow"));
        assert!(add_deny_read_ace(&home.secret, other.as_ptr()).expect("other deny"));
        assert!(
            add_deny_read_ace_for_new_files(&home.secret, group.as_ptr()).expect("new-file deny")
        );
    }
    let dir_before = dacl_sddl(&home.secret);
    let nested_before = dacl_sddl(&home.nested);

    sync(&home, std::slice::from_ref(&home.secret), &group);
    assert_ne!(dacl_sddl(&home.secret), dir_before);
    // Subdirectories inherit the deny (the new-file deny does not reach them).
    assert_ne!(dacl_sddl(&home.nested), nested_before);
    assert!(recorded(&home));

    // The rule is gone: the next launch no longer lists the path.
    sync(&home, &[], &group);
    assert_eq!(dacl_sddl(&home.secret), dir_before);
    assert_eq!(dacl_sddl(&home.nested), nested_before);
    assert!(!recorded(&home));
}

/// A read deny the sync found already in place (here, the explicit one the
/// launch contract gives its lock file) is not the sync's to remove.
#[test]
fn sec_win_304_sync_keeps_a_deny_it_did_not_add() {
    let home = home();
    let group = LocalSid::from_string(SANDBOX_GROUP).expect("group SID");
    let file = home.secret.join("lock");
    std::fs::write(&file, "").expect("file");
    // SAFETY: a valid SID and an existing file.
    unsafe {
        assert!(ensure_explicit_deny_read_ace(&file, group.as_ptr()).expect("deny"));
    }
    let denied = dacl_sddl(&file);
    sync(&home, std::slice::from_ref(&file), &group);
    assert_eq!(dacl_sddl(&file), denied);
    sync(&home, &[], &group);
    assert_eq!(dacl_sddl(&file), denied);
    assert!(!recorded(&home));
}

/// Most protected paths are files (`auth.json`, `.env`).
#[test]
fn sec_win_304_sync_removes_the_deny_it_added_to_a_file() {
    let home = home();
    let group = LocalSid::from_string(SANDBOX_GROUP).expect("group SID");
    let file = home.secret.join("auth.json");
    std::fs::write(&file, "{}").expect("file");
    let before = dacl_sddl(&file);
    sync(&home, std::slice::from_ref(&file), &group);
    assert!(explicit_deny(&file, &group), "{}", dacl_sddl(&file));
    sync(&home, &[], &group);
    assert_eq!(dacl_sddl(&file), before);
    assert!(!recorded(&home));
}

/// A launch that denies a directory inside the previous launch's denied
/// directory: the child gets its own entry before the parent's goes.
#[test]
fn sec_win_304_child_stays_denied_when_its_parent_is_dropped() {
    let home = home();
    let group = LocalSid::from_string(SANDBOX_GROUP).expect("group SID");
    let before = dacl_sddl(&home.secret);
    sync(&home, std::slice::from_ref(&home.secret), &group);
    sync(&home, std::slice::from_ref(&home.nested), &group);
    assert!(
        explicit_deny(&home.nested, &group),
        "{}",
        dacl_sddl(&home.nested)
    );
    assert_eq!(dacl_sddl(&home.secret), before);
}

/// A sandboxed command can replace a stale deny-read path inside a writable
/// root, or a directory above it, with a junction to a path that is still
/// denied, or a stale file with a hard link to a file that is. The removal
/// must not reach them.
#[test]
fn sec_win_304_removal_does_not_follow_a_link() {
    let home = home();
    let group = LocalSid::from_string(SANDBOX_GROUP).expect("group SID");
    let at_path = home.codex_home.join("workspace-env");
    let parent = home.codex_home.join("workspace");
    let under_parent = parent.join("vault-secret");
    let stale_file = home.codex_home.join("workspace.env");
    let denied_file = home.secret.join("auth.json");
    std::fs::create_dir_all(&at_path).expect("stale dir");
    std::fs::create_dir_all(&under_parent).expect("stale dir");
    std::fs::write(&stale_file, "").expect("stale file");
    std::fs::write(&denied_file, "{}").expect("denied file");
    sync(
        &home,
        &[
            at_path.clone(),
            under_parent.clone(),
            stale_file.clone(),
            home.secret.clone(),
            denied_file.clone(),
        ],
        &group,
    );
    // The paths are swapped for links to the still-denied objects
    // (`workspace\vault-secret` then names `vault-secret`).
    std::fs::remove_dir(&at_path).expect("remove stale dir");
    std::fs::remove_dir(&under_parent).expect("remove stale dir");
    std::fs::remove_dir(&parent).expect("remove stale parent");
    std::fs::remove_file(&stale_file).expect("remove stale file");
    junction(&at_path, &home.secret);
    junction(&parent, &home.codex_home);
    std::fs::hard_link(&denied_file, &stale_file).expect("hard link");

    sync(&home, &[home.secret.clone(), denied_file.clone()], &group);
    assert!(
        explicit_deny(&home.secret, &group),
        "{}",
        dacl_sddl(&home.secret)
    );
    assert!(
        explicit_deny(&denied_file, &group),
        "{}",
        dacl_sddl(&denied_file)
    );
    // Kept, so the entries are removed if the paths come back.
    let state = state(&home);
    assert!(state.contains("workspace-env"), "{state}");
    assert!(state.contains("workspace\\\\vault-secret"), "{state}");
    assert!(state.contains("workspace.env"), "{state}");
}

/// A sandboxed command can rename a denied object (the entry does not deny
/// DELETE) onto a recorded stale path, or try to get its own folder recorded
/// by leaving a junction at a denied path. The removal must leave that
/// object's entry: it was added to another object.
#[test]
fn sec_win_304_removal_skips_an_object_renamed_onto_a_stale_path() {
    let home = home();
    let group = LocalSid::from_string(SANDBOX_GROUP).expect("group SID");
    let stash = home.codex_home.join("stash");
    let plain = home.codex_home.join("plain");
    sync(&home, std::slice::from_ref(&home.secret), &group);

    // Command 1: move the secret away and leave a junction to a plain folder.
    std::fs::rename(&home.secret, &stash).expect("rename secret");
    std::fs::create_dir(&plain).expect("plain dir");
    junction(&home.secret, &plain);
    if junction_followable(&home.secret) {
        // Launch 2 adds the entry through the junction, to `plain`, which is
        // therefore not recorded.
        sync(&home, std::slice::from_ref(&home.secret), &group);
        assert!(!state(&home).contains("plain"), "{}", state(&home));

        // Command 2: put the secret where `plain` was.
        std::fs::remove_dir(&home.secret).expect("remove junction");
        std::fs::remove_dir(&plain).expect("remove plain");
        std::fs::rename(&stash, &plain).expect("rename secret onto plain");
        // Launch 3: `plain` is stale, but it is now the secret.
        sync(&home, std::slice::from_ref(&home.secret), &group);
        assert!(explicit_deny(&plain, &group), "{}", dacl_sddl(&plain));
        assert!(!state(&home).contains("plain"), "{}", state(&home));
    }

    // The same with a file renamed onto a stale file path.
    let stale = home.codex_home.join("old.env");
    let wallet = home.codex_home.join("wallet.json");
    std::fs::write(&stale, "").expect("stale file");
    std::fs::write(&wallet, "{}").expect("wallet");
    sync(&home, &[stale.clone(), wallet.clone()], &group);
    std::fs::remove_file(&stale).expect("remove stale file");
    std::fs::rename(&wallet, &stale).expect("rename wallet onto stale");
    sync(&home, &[], &group);
    assert!(explicit_deny(&stale, &group), "{}", dacl_sddl(&stale));
}

/// An entry added through a link lands on the target, which is not the
/// configured path: it is not recorded, so it stays (a sandboxed command
/// could have pointed the link at a folder it filled with moved secrets).
#[test]
fn sec_win_304_entry_added_through_a_link_stays() {
    let home = home();
    let group = LocalSid::from_string(SANDBOX_GROUP).expect("group SID");
    let link = home.codex_home.join("linked-secret");
    junction(&link, &home.secret);
    if !junction_followable(&link) {
        return;
    }
    sync(&home, std::slice::from_ref(&link), &group);
    assert!(
        explicit_deny(&home.secret, &group),
        "{}",
        dacl_sddl(&home.secret)
    );
    assert!(!recorded(&home), "{}", state(&home));
    sync(&home, &[], &group);
    assert!(
        explicit_deny(&home.secret, &group),
        "{}",
        dacl_sddl(&home.secret)
    );
}

/// The sandbox's group is machine-wide, so another `CODEX_HOME`'s sessions
/// can rely on the same entry; a sync removes only entries it added.
#[test]
fn sec_win_304_entry_another_codex_home_added_is_kept() {
    let home_a = home();
    let home_b = home();
    let group = LocalSid::from_string(SANDBOX_GROUP).expect("group SID");
    let shared = home_a.secret.clone();
    sync(&home_a, std::slice::from_ref(&shared), &group);
    sync(&home_b, std::slice::from_ref(&shared), &group);
    sync(&home_b, &[], &group);
    assert!(
        explicit_deny(&shared, &group),
        "removed another home's entry"
    );
    sync(&home_a, &[], &group);
    assert!(!explicit_deny(&shared, &group));
}

/// S1: a lingering sandboxed process holds a folder open without sharing, so
/// the glob scan cannot list it and misses the secret inside. The rule is
/// still configured, so the secret keeps its deny; only removing the rule
/// from the configuration removes it.
#[test]
fn sec_win_304_s1_a_secret_hidden_from_the_scan_keeps_its_deny() {
    let home = home();
    let group = LocalSid::from_string(SANDBOX_GROUP).expect("group SID");
    let workspace = home.codex_home.join("ws");
    let folder = workspace.join("b");
    std::fs::create_dir_all(&folder).expect("folder");
    let secret = folder.join("x.env");
    std::fs::write(&secret, "secret").expect("secret");
    let secret_path = AbsolutePathBuf::from_absolute_path(&secret).expect("absolute secret");
    let before = dacl_sddl(&secret);
    let cwd = AbsolutePathBuf::from_absolute_path(&workspace).expect("absolute workspace");
    let policy = FileSystemSandboxPolicy::restricted(vec![FileSystemSandboxEntry {
        path: FileSystemPath::GlobPattern {
            scan: None,
            pattern: "**/*.env".to_string(),
        },
        access: FileSystemAccessMode::Deny,
        missing_path_behavior: None,
    }]);
    let scan = || resolve_windows_deny_read_targets(&policy, &cwd).expect("resolve");

    let first = scan();
    assert!(first.contains_path(&secret_path), "{first:?}");
    sync_targets(&home, &first, &group);
    assert!(explicit_deny(&secret, &group), "{}", dacl_sddl(&secret));

    // Held open with no sharing: the next launch's scan cannot list it.
    let holder = std::fs::OpenOptions::new()
        .read(true)
        .share_mode(0)
        .custom_flags(FILE_FLAG_BACKUP_SEMANTICS)
        .open(&folder)
        .expect("hold the folder open");
    let hidden = scan();
    assert!(
        !hidden.contains_path(&secret_path),
        "the scan still saw the secret: {hidden:?}"
    );
    assert_eq!(hidden.rules().len(), 1, "the rule is still configured");
    sync_targets(&home, &hidden, &group);
    drop(holder);
    assert!(
        explicit_deny(&secret, &group),
        "a match the scan missed lost its deny: {}",
        dacl_sddl(&secret)
    );
    assert!(state(&home).contains("x.env"), "{}", state(&home));

    // A launch whose scan sees it again keeps it recorded.
    sync_targets(&home, &scan(), &group);
    assert!(explicit_deny(&secret, &group));

    // The rule is removed from the configuration: now the entry goes.
    sync_targets(&home, &DenyReadTargets::default(), &group);
    assert_eq!(dacl_sddl(&secret), before);
    assert!(!state(&home).contains("x.env"), "{}", state(&home));
}

/// A glob rule's entries go when the rule is replaced by another one, while
/// the new rule's own matches get theirs; a refresh that carries no rules
/// (`None`: a read-root refresh, the first setup) changes nothing.
#[test]
fn sec_win_304_entries_follow_their_rules() {
    let home = home();
    let group = LocalSid::from_string(SANDBOX_GROUP).expect("group SID");
    let env = home.secret.join("a.env");
    let key = home.secret.join("a.key");
    std::fs::write(&env, "env").expect("env");
    std::fs::write(&key, "key").expect("key");
    let env_before = dacl_sddl(&env);
    let rule = |pattern: &str, matched: &Path| {
        let mut targets = DenyReadTargets::default();
        targets.add(
            DenyReadRule::Glob(home.secret.join(pattern).to_string_lossy().into_owned()),
            vec![AbsolutePathBuf::from_absolute_path(matched).expect("absolute")],
        );
        targets
    };

    sync_targets(&home, &rule("*.env", &env), &group);
    assert!(explicit_deny(&env, &group));
    // SAFETY: a valid SID for the call.
    unsafe {
        sync_persistent_deny_read_acls(
            &home.codex_home,
            SANDBOX_GROUP,
            None,
            group.as_ptr(),
            Some(&home.sessions),
        )
    }
    .expect("refresh without rules");
    assert!(explicit_deny(&env, &group), "a refresh removed an entry");

    // The same rule spelled differently (case, separators) is the same rule.
    let mut respelled = DenyReadTargets::default();
    respelled.add(
        DenyReadRule::Glob(
            home.secret
                .join("*.ENV")
                .to_string_lossy()
                .replace('\\', "/"),
        ),
        Vec::new(),
    );
    sync_targets(&home, &respelled, &group);
    assert!(explicit_deny(&env, &group), "{}", state(&home));

    sync_targets(&home, &rule("*.key", &key), &group);
    assert!(explicit_deny(&key, &group));
    assert_eq!(dacl_sddl(&env), env_before);
}

/// Whether `path` has its own explicit read deny for `sid`.
fn explicit_deny(path: &Path, sid: &LocalSid) -> bool {
    // SAFETY: a valid SID and an existing path.
    unsafe { has_explicit_deny_read_ace(path, sid.as_ptr()) }.expect("read DACL")
}

/// Whether this process may follow the junction at `link`. Windows refuses a
/// junction a non-administrator created to a process at medium integrity on
/// some machines (the QA machine); the elevated sandbox's setup, which runs
/// the sync, is an administrator.
fn junction_followable(link: &Path) -> bool {
    let followable = std::fs::metadata(link).is_ok();
    if !followable {
        eprintln!("skipped: this process cannot follow {}", link.display());
    }
    followable
}

/// Creates a directory junction at `link` to `target` (no privilege needed).
fn junction(link: &Path, target: &Path) {
    let status = Command::new("cmd")
        .arg("/C")
        .arg("mklink")
        .arg("/J")
        .arg(link)
        .arg(target)
        .stdout(Stdio::null())
        .status()
        .expect("mklink");
    assert!(status.success(), "mklink /J failed: {status}");
    assert!(
        link.symlink_metadata().is_ok(),
        "no junction at {}",
        link.display()
    );
}

/// The armed session of [`sec_win_301_flag_off_session_keeps_denies_while_another_is_armed`]:
/// holds the lock as the launch contract does, until its stdin closes.
#[test]
fn sec_win_301_armed_session_entry() {
    let Some(codex_home) = std::env::var_os(ARMED_HOME_ENV) else {
        return;
    };
    const FILE_SHARE_READ: u32 = 0x1;
    const FILE_SHARE_WRITE: u32 = 0x2;
    let lock = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE)
        .open(Path::new(&codex_home).join(SECRETLESS_LAUNCH_LOCK_FILE))
        .expect("lock file");
    lock.try_lock_shared().expect("shared lock");
    let mut stdout = std::io::stdout();
    writeln!(stdout, "{ARMED_LINE}").expect("report");
    stdout.flush().expect("report");
    let _ = std::io::stdin().read_to_end(&mut Vec::new());
}

/// Starts another process that has the contract armed on `codex_home`.
fn armed_session(codex_home: &Path) -> Child {
    let mut child = Command::new(std::env::current_exe().expect("test binary"))
        .args([ARMED_ENTRY, "--exact", "--nocapture", "--test-threads=1"])
        .env(ARMED_HOME_ENV, codex_home)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .expect("start the armed session");
    let mut stdout = BufReader::new(child.stdout.take().expect("stdout"));
    let mut line = String::new();
    // libtest prints the test's name without a newline before the line.
    while !line.trim_end().ends_with(ARMED_LINE) {
        line.clear();
        let read = stdout.read_line(&mut line).expect("armed session output");
        assert_ne!(read, 0, "the armed session did not take the lock");
    }
    // Keep reading, so its remaining output does not hit a closed pipe.
    std::thread::spawn(move || std::io::copy(&mut stdout, &mut std::io::sink()));
    child
}

#[test]
fn sec_win_301_flag_off_session_keeps_denies_while_another_is_armed() {
    let home = home();
    let group = LocalSid::from_string(SANDBOX_GROUP).expect("group SID");
    let unprotected = dacl_sddl(&home.secret);

    // Session A arms the contract and launches with its deny.
    let mut session_a = armed_session(&home.codex_home);
    sync(&home, std::slice::from_ref(&home.secret), &group);
    let protected = dacl_sddl(&home.secret);
    assert_ne!(protected, unprotected);

    // Session B, flag off, launches without that path while A runs.
    sync(&home, &[], &group);
    assert_eq!(
        dacl_sddl(&home.secret),
        protected,
        "removed under an armed session"
    );
    assert!(recorded(&home), "forgot a deny it did not remove");

    // A exits; B's next launch removes it.
    drop(session_a.stdin.take());
    assert!(session_a.wait().expect("armed session").success());
    sync(&home, &[], &group);
    assert_eq!(dacl_sddl(&home.secret), unprotected);
    assert!(!recorded(&home));
}

const SESSION_REGISTRY_ENV: &str = "CODEX_SEC_WIN_323_REGISTRY";
const SESSION_HOME_ENV: &str = "CODEX_SEC_WIN_323_ARMED_HOME";
const SESSION_ENTRY: &str = "deny_read_state::tests::sec_win_323_live_session_entry";
const SESSION_LINE: &str = "sec-win-323: live";

/// Another Core process for the #323 tests: registered as a live session in
/// the registry given, and with the contract armed on the home given (its
/// lock held shared, as the launch contract holds it), until its stdin
/// closes.
#[test]
fn sec_win_323_live_session_entry() {
    let Some(registry) = std::env::var_os(SESSION_REGISTRY_ENV) else {
        return;
    };
    let _armed = std::env::var_os(SESSION_HOME_ENV).map(|codex_home| {
        const FILE_SHARE_READ: u32 = 0x1;
        const FILE_SHARE_WRITE: u32 = 0x2;
        let lock = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE)
            .open(Path::new(&codex_home).join(SECRETLESS_LAUNCH_LOCK_FILE))
            .expect("lock file");
        lock.try_lock_shared().expect("shared lock");
        lock
    });
    let _session = register_session(Path::new(&registry));
    let mut stdout = std::io::stdout();
    writeln!(stdout, "{SESSION_LINE}").expect("report");
    stdout.flush().expect("report");
    let _ = std::io::stdin().read_to_end(&mut Vec::new());
}

/// This process's registration as a live session (#323).
fn register_session(registry: &Path) -> Registration {
    Registration::register(
        registry,
        /*trust_this_user*/ true,
        crate::deny_read_sessions::DENY_READ_SYNC_LOCK_WAIT,
    )
    .expect("register the session")
}

/// Starts another process: a live session in `registry`, with the contract
/// armed on `armed_home` if given.
fn live_session(registry: &Path, armed_home: Option<&Path>) -> Child {
    let mut command = Command::new(std::env::current_exe().expect("test binary"));
    command
        .args([SESSION_ENTRY, "--exact", "--nocapture", "--test-threads=1"])
        .env(SESSION_REGISTRY_ENV, registry)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped());
    if let Some(home) = armed_home {
        command.env(SESSION_HOME_ENV, home);
    }
    let mut child = command.spawn().expect("start the other session");
    let mut stdout = BufReader::new(child.stdout.take().expect("stdout"));
    let mut line = String::new();
    while !line.trim_end().ends_with(SESSION_LINE) {
        line.clear();
        let read = stdout.read_line(&mut line).expect("session output");
        assert_ne!(read, 0, "the other session did not start");
    }
    std::thread::spawn(move || std::io::copy(&mut stdout, &mut std::io::sink()));
    child
}

/// A protected path outside both homes (like `~/.netrc`).
fn shared_secret() -> (tempfile::TempDir, PathBuf) {
    let dir = tempfile::tempdir().expect("profile");
    let secret = dunce::canonicalize(dir.path())
        .expect("canonical profile")
        .join("netrc-secret");
    std::fs::create_dir(&secret).expect("secret dir");
    (dir, secret)
}

/// #323: home B's launch added the entry on a path outside both homes; a
/// contract armed on home A found it there and relies on it. B's next
/// launch, without that rule, used to remove it while A's commands ran (A's
/// lock is in A's home, not B's). Two homes, two processes.
#[test]
fn sec_win_323_another_homes_sync_keeps_an_armed_contracts_entry() {
    let registry = tempfile::tempdir().expect("session registry");
    let sessions = DenyReadSessions::in_test_registry(registry.path(), /*own*/ None);
    let home_a = home_in(sessions.clone());
    let home_b = home_in(sessions);
    let group = LocalSid::from_string(SANDBOX_GROUP).expect("group SID");
    let (_profile, secret) = shared_secret();
    let unprotected = dacl_sddl(&secret);

    // B launches first, with the rule: its entry.
    sync(&home_b, std::slice::from_ref(&secret), &group);
    assert!(explicit_deny(&secret, &group));
    // A arms the contract in another process and launches with the same rule.
    let mut session_a = live_session(registry.path(), Some(&home_a.codex_home));
    sync(&home_a, std::slice::from_ref(&secret), &group);
    let protected = dacl_sddl(&secret);

    // B launches again without the rule while A runs.
    sync(&home_b, &[], &group);
    let kept = dacl_sddl(&secret) == protected;
    let still_recorded = state(&home_b).contains("netrc-secret");

    // A exits; B's next launch removes its entry.
    drop(session_a.stdin.take());
    assert!(session_a.wait().expect("session A").success());
    sync(&home_b, &[], &group);
    let removed = dacl_sddl(&secret) == unprotected;
    eprintln!(
        "sec-win-323: kept while A ran: {kept}; still recorded: {still_recorded}; removed after: {removed}"
    );
    assert!(
        kept,
        "removed an entry the armed contract on another home relies on"
    );
    assert!(still_recorded, "forgot an entry it did not remove");
    assert!(removed, "never removed the entry");
    assert!(!state(&home_b).contains("netrc-secret"));
}

/// #323 (the case from its discussion): two sessions on one home, neither
/// armed, with different rules. Session 2's launch used to remove the entry
/// session 1 added while session 1's commands may still run.
#[test]
fn sec_win_323_unarmed_sessions_keep_each_others_entries() {
    let registry = tempfile::tempdir().expect("session registry");
    let home = home_in(DenyReadSessions::in_test_registry(
        registry.path(),
        /*own*/ None,
    ));
    let group = LocalSid::from_string(SANDBOX_GROUP).expect("group SID");
    let (_profile, secret) = shared_secret();
    let unprotected = dacl_sddl(&secret);

    // Session 1 (another process) launches with the rule: the home's entry.
    let mut session_1 = live_session(registry.path(), None);
    sync(&home, std::slice::from_ref(&secret), &group);
    let protected = dacl_sddl(&secret);
    assert_ne!(protected, unprotected);

    // Session 2, configured without it, launches while session 1 runs.
    sync(&home, &[], &group);
    let kept = dacl_sddl(&secret) == protected;

    drop(session_1.stdin.take());
    assert!(session_1.wait().expect("session 1").success());
    sync(&home, &[], &group);
    let removed = dacl_sddl(&secret) == unprotected;
    eprintln!("sec-win-323: kept while session 1 ran: {kept}; removed after: {removed}");
    assert!(kept, "removed an entry another live session relies on");
    assert!(removed, "never removed the entry");
}

/// The entries of `path`'s DACL, in SDDL. The DACL's control flags are left
/// out: rewriting a DACL sets "auto-inherited" (`AI`) on one that lacked it
/// (as on GitHub's Windows runners), which changes no entry.
fn dacl_sddl(path: &Path) -> String {
    let mut sd: *mut c_void = std::ptr::null_mut();
    // SAFETY: valid path; `sd` is freed below.
    let code = unsafe {
        GetNamedSecurityInfoW(
            to_wide(path).as_ptr(),
            SE_FILE_OBJECT,
            DACL_SECURITY_INFORMATION,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            &mut sd,
        )
    };
    assert_eq!(code, ERROR_SUCCESS, "GetNamedSecurityInfoW");
    let mut text: *mut u16 = std::ptr::null_mut();
    // SAFETY: `sd` is a valid descriptor; `text` is freed below.
    let ok = unsafe {
        ConvertSecurityDescriptorToStringSecurityDescriptorW(
            sd,
            SDDL_REVISION_1,
            DACL_SECURITY_INFORMATION,
            &mut text,
            std::ptr::null_mut(),
        )
    };
    assert_ne!(
        ok, 0,
        "ConvertSecurityDescriptorToStringSecurityDescriptorW"
    );
    // SAFETY: `text` is a NUL-terminated string allocated by the call.
    let sddl = unsafe {
        let len = (0..).take_while(|&i| *text.add(i) != 0).count();
        String::from_utf16_lossy(std::slice::from_raw_parts(text, len))
    };
    // SAFETY: both were allocated by the calls above.
    unsafe {
        LocalFree(text as HLOCAL);
        LocalFree(sd as HLOCAL);
    }
    match sddl.find('(') {
        Some(entries) => sddl[entries..].to_string(),
        None => String::new(),
    }
}
