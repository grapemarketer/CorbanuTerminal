//! D377 (#304): an elevated workspace-write command writes to a temp folder
//! of its session's own, not to the user's whole temp folder, and the grant
//! older setups put on the temp folder itself is taken back.

use super::*;
use pretty_assertions::assert_eq;

fn unique_id() -> String {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("clock")
        .as_nanos();
    format!("{:x}-{}", nanos as u64, std::process::id())
}

/// A script that prints the command's temp folders and tries to write
/// `existing` (appending), make `new` and write in its own temp folder.
fn write_probe_script(dir: &Path, existing: &Path, new: &Path) -> Vec<String> {
    let script = dir.join("d377.cmd");
    fs::write(
        &script,
        format!(
            "@echo off\r\n\
             echo TEMP=%TEMP%\r\n\
             echo TMP=%TMP%\r\n\
             (echo x>>\"{}\") 2>nul && echo EXISTING-WRITTEN\r\n\
             (echo x>\"{}\") 2>nul && echo NEW-WRITTEN\r\n\
             (echo x>\"%TEMP%\\own.txt\") 2>nul && echo OWN-WRITTEN\r\n",
            existing.display(),
            new.display()
        ),
    )
    .expect("write script");
    vec![
        system32_exe("cmd.exe").display().to_string(),
        "/d".to_string(),
        "/c".to_string(),
        script.display().to_string(),
    ]
}

fn printed(stdout: &str, key: &str) -> PathBuf {
    let line = stdout
        .lines()
        .find_map(|line| line.trim().strip_prefix(key))
        .unwrap_or_else(|| panic!("no {key} in {stdout:?}"));
    PathBuf::from(line)
}

/// The command's temp folder (`TEMP` and `TMP`): a session folder in `base`.
fn assert_session_temp_in(stdout: &str, base: &Path) {
    let temp = printed(stdout, "TEMP=");
    assert_eq!(printed(stdout, "TMP="), temp, "{stdout}");
    let key = |path: &Path| {
        dunce::canonicalize(path)
            .unwrap_or_else(|_| path.to_path_buf())
            .to_string_lossy()
            .to_ascii_lowercase()
    };
    assert_eq!(
        temp.parent().map(key),
        Some(key(base)),
        "the command's TEMP is not a folder in {}: {stdout}",
        base.display()
    );
    let name = temp
        .file_name()
        .expect("name")
        .to_string_lossy()
        .into_owned();
    assert!(
        name.starts_with(crate::session_temp::SESSION_TEMP_PREFIX),
        "{stdout}"
    );
}

#[test]
fn sec_win_d377_elevated_command_gets_its_own_temp_folder() {
    let _guard = legacy_process_test_guard();
    let Some(home) = elevated_home("d377") else {
        return;
    };
    let root = user_owned_test_root();
    let temp = dunce::canonicalize(std::env::var_os("TEMP").expect("TEMP")).expect("TEMP");
    let id = unique_id();
    // Another program's temp file, and one a command would make.
    let existing = temp.join(format!("d377-other-program-{id}.txt"));
    fs::write(&existing, "other").expect("other program's file");
    let new = temp.join(format!("d377-new-{id}.txt"));
    let argv = write_probe_script(root.path(), &existing, &new);
    let (stdout, code) = current_thread_runtime().block_on(run_elevated(
        root.path(),
        home.path(),
        argv,
        &[],
        /*tty*/ false,
    ));
    println!("d377 own temp: exit={code} stdout={stdout:?}");
    assert_session_temp_in(&stdout, &temp);
    assert!(stdout.contains("OWN-WRITTEN"), "{stdout}");
    assert!(!stdout.contains("EXISTING-WRITTEN"), "{stdout}");
    assert!(!stdout.contains("NEW-WRITTEN"), "{stdout}");
    assert_eq!(fs::read_to_string(&existing).expect("read"), "other");
    assert!(!new.exists());
    let _ = fs::remove_file(&existing);
    crate::session_temp::end_session_temp_in(&temp);
    record_setup(home.path());
}

#[test]
fn sec_win_d377_old_temp_folder_grant_is_taken_back() {
    let _guard = legacy_process_test_guard();
    let Some(home) = elevated_home("d377-old") else {
        return;
    };
    let root = user_owned_test_root();
    let runtime = current_thread_runtime();
    // The sandbox's group exists once its setup ran.
    let (stdout, code) = runtime.block_on(run_elevated(
        root.path(),
        home.path(),
        vec![
            system32_exe("cmd.exe").display().to_string(),
            "/d".to_string(),
            "/c".to_string(),
            "echo d377-ready".to_string(),
        ],
        &[],
        /*tty*/ false,
    ));
    assert!(stdout.contains("d377-ready"), "exit={code} {stdout:?}");
    // A temp folder secured like a user's, with the grant older setups left
    // on it: the sandbox group may write it and what it holds.
    let temp = user_owned_test_root();
    let group = crate::winutil::string_from_sid_bytes(
        &crate::winutil::resolve_sid(crate::token::SANDBOX_USERS_GROUP).expect("group"),
    )
    .expect("group sid");
    // SAFETY: the SID is freed below; the folder exists.
    let has_group_write = |path: &Path| unsafe {
        let psid = crate::token::convert_string_sid_to_sid(&group).expect("psid");
        let (dacl, descriptor) = crate::acl::fetch_dacl_handle(path).expect("dacl");
        let has = crate::acl::dacl_has_write_allow_for_sid(dacl, psid);
        windows_sys::Win32::Foundation::LocalFree(descriptor as _);
        windows_sys::Win32::Foundation::LocalFree(psid as _);
        has
    };
    // SAFETY: as above.
    unsafe {
        let psid = crate::token::convert_string_sid_to_sid(&group).expect("psid");
        crate::acl::ensure_allow_write_aces(temp.path(), &[psid]).expect("old grant");
        windows_sys::Win32::Foundation::LocalFree(psid as _);
    }
    let existing = temp.path().join("other-program.txt");
    fs::write(&existing, "other").expect("other program's file");
    assert!(has_group_write(temp.path()) && has_group_write(&existing));
    let new = temp.path().join("new.txt");
    let argv = write_probe_script(root.path(), &existing, &new);
    let value = temp.path().display().to_string();
    let (stdout, code) = runtime.block_on(run_elevated(
        root.path(),
        home.path(),
        argv,
        &[("TEMP", value.clone()), ("TMP", value)],
        /*tty*/ false,
    ));
    println!("d377 old grant: exit={code} stdout={stdout:?}");
    assert!(
        !has_group_write(temp.path()),
        "the old grant is still there"
    );
    assert!(!has_group_write(&existing));
    assert_session_temp_in(&stdout, temp.path());
    assert!(stdout.contains("OWN-WRITTEN"), "{stdout}");
    assert!(!stdout.contains("EXISTING-WRITTEN"), "{stdout}");
    assert!(!stdout.contains("NEW-WRITTEN"), "{stdout}");
    assert!(!new.exists());
    crate::session_temp::end_session_temp_in(temp.path());
    record_setup(home.path());
}

/// Round-1 review: a command must not be able to turn its temp folder into
/// a link, which the next setup would follow and grant the sandbox the
/// link's target.
#[test]
fn sec_win_d377_command_cannot_turn_its_temp_folder_into_a_link() {
    let _guard = legacy_process_test_guard();
    let Some(home) = elevated_home("d377-link") else {
        return;
    };
    let root = user_owned_test_root();
    let runtime = current_thread_runtime();
    let temp = user_owned_test_root();
    let target = user_owned_test_root();
    let script = root.path().join("d377-link.cmd");
    // Empty, rename or remove the folder, and (had that worked) put a
    // junction in its place. Making the folder itself a mount point needs it
    // empty: `session_temp_tests` shows that is refused while it is held.
    fs::write(
        &script,
        format!(
            "@echo off\r\n\
             echo TEMP=%TEMP%\r\n\
             echo TMP=%TMP%\r\n\
             del /f /q \"%TEMP%\\{marker}\" >nul 2>nul\r\n\
             if exist \"%TEMP%\\{marker}\" echo MARKER-KEPT\r\n\
             ren \"%TEMP%\" d377-moved >nul 2>nul && echo RENAMED\r\n\
             rd /s /q \"%TEMP%\" >nul 2>nul\r\n\
             if exist \"%TEMP%\\\" echo FOLDER-KEPT\r\n\
             mklink /J \"%TEMP%\" \"{target}\" >nul 2>nul && echo LINKED\r\n",
            marker = ".corbanu-session",
            target = target.path().display()
        ),
    )
    .expect("write script");
    let argv = vec![
        system32_exe("cmd.exe").display().to_string(),
        "/d".to_string(),
        "/c".to_string(),
        script.display().to_string(),
    ];
    let value = temp.path().display().to_string();
    let env = [("TEMP", value.clone()), ("TMP", value)];
    let (stdout, code) = runtime.block_on(run_elevated(
        root.path(),
        home.path(),
        argv.clone(),
        &env,
        /*tty*/ false,
    ));
    println!("d377 link attempt: exit={code} stdout={stdout:?}");
    assert_session_temp_in(&stdout, temp.path());
    for kept in ["MARKER-KEPT", "FOLDER-KEPT"] {
        assert!(stdout.contains(kept), "{kept}: {stdout}");
    }
    for done in ["RENAMED", "LINKED"] {
        assert!(!stdout.contains(done), "{done}: {stdout}");
    }
    let session = printed(&stdout, "TEMP=");
    let is_reparse_point = |path: &Path| {
        use std::os::windows::fs::MetadataExt as _;
        fs::symlink_metadata(path)
            .expect("metadata")
            .file_attributes()
            & 0x400
            != 0
    };
    assert!(session.is_dir() && !is_reparse_point(&session));
    // The next command (and its setup) gets the same, intact folder; the
    // would-be target gains no grant.
    let (again, code) = runtime.block_on(run_elevated(
        root.path(),
        home.path(),
        argv,
        &env,
        /*tty*/ false,
    ));
    println!("d377 link attempt again: exit={code} stdout={again:?}");
    assert_eq!(printed(&again, "TEMP="), session, "{again}");
    assert!(!again.contains("LINKED"), "{again}");
    let group = crate::winutil::string_from_sid_bytes(
        &crate::winutil::resolve_sid(crate::token::SANDBOX_USERS_GROUP).expect("group"),
    )
    .expect("group sid");
    // SAFETY: the SID and descriptor are freed below; the folder exists.
    let target_granted = unsafe {
        let psid = crate::token::convert_string_sid_to_sid(&group).expect("psid");
        let (dacl, descriptor) = crate::acl::fetch_dacl_handle(target.path()).expect("dacl");
        let has = crate::acl::dacl_has_write_allow_for_sid(dacl, psid);
        windows_sys::Win32::Foundation::LocalFree(descriptor as _);
        windows_sys::Win32::Foundation::LocalFree(psid as _);
        has
    };
    assert!(!target_granted, "the link's target was granted");
    crate::session_temp::end_session_temp_in(temp.path());
    assert!(!session.exists());
    record_setup(home.path());
}

/// #1871: what an older version's commands made in the temp folder stays
/// their accounts' (an owner may change an object's permissions), so taking
/// back the folder's grant alone leaves a command able to grant itself
/// access again. The upgrade step makes the user their owner first.
#[test]
fn sec_win_d377_old_leftovers_in_temp_are_taken_for_the_user() {
    let _guard = legacy_process_test_guard();
    let Some(home) = elevated_home("d377-left") else {
        return;
    };
    let root = user_owned_test_root();
    let runtime = current_thread_runtime();
    let cmd = |script: &Path| {
        vec![
            system32_exe("cmd.exe").display().to_string(),
            "/d".to_string(),
            "/c".to_string(),
            script.display().to_string(),
        ]
    };
    // A temp folder secured like a user's, with the grant older setups left
    // on it (the sandbox's group exists once its setup ran), and a `CREATOR
    // OWNER` entry as shared temp folders have (`C:\Temp`): what an account
    // makes there inherits an entry naming that account.
    let temp = user_owned_test_root();
    let creator_owner = std::process::Command::new(system32_exe("icacls.exe"))
        .arg(temp.path())
        .args(["/grant", "*S-1-3-0:(OI)(CI)(IO)F"])
        .output()
        .expect("icacls");
    assert!(creator_owner.status.success(), "{creator_owner:?}");
    let (stdout, code) = runtime.block_on(run_elevated(
        root.path(),
        home.path(),
        cmd(&system32_exe("hostname.exe")),
        &[],
        /*tty*/ false,
    ));
    println!("d377 leftovers setup: exit={code} stdout={stdout:?}");
    let group = crate::winutil::string_from_sid_bytes(
        &crate::winutil::resolve_sid(crate::token::SANDBOX_USERS_GROUP).expect("group"),
    )
    .expect("group sid");
    // SAFETY: the SID is freed below; the folder exists.
    unsafe {
        let psid = crate::token::convert_string_sid_to_sid(&group).expect("psid");
        crate::acl::ensure_allow_write_aces(temp.path(), &[psid]).expect("old grant");
        windows_sys::Win32::Foundation::LocalFree(psid as _);
    }
    let file = temp.path().join("left.txt");
    let dir = temp.path().join("left-dir");
    let users = temp.path().join("other-program.txt");
    fs::write(&users, "other").expect("other program's file");
    // An older version's command, writing there as when the temp folder was
    // one of its writable roots.
    let make = temp.path().join("d377-make.cmd");
    fs::write(
        &make,
        format!(
            "@echo off\r\n\
             (echo made>\"{file}\") 2>nul && echo FILE-MADE\r\n\
             mkdir \"{dir}\" 2>nul && (echo made>\"{dir}\\inner.txt\") 2>nul && echo DIR-MADE\r\n",
            file = file.display(),
            dir = dir.display()
        ),
    )
    .expect("write script");
    let (stdout, code) = runtime.block_on(run_elevated(
        temp.path(),
        home.path(),
        cmd(&make),
        &[],
        /*tty*/ false,
    ));
    println!("d377 leftovers made: exit={code} stdout={stdout:?}");
    assert!(
        stdout.contains("FILE-MADE") && stdout.contains("DIR-MADE"),
        "{stdout}"
    );
    let owner = |path: &Path| {
        use std::os::windows::fs::OpenOptionsExt as _;
        let file = std::fs::OpenOptions::new()
            .access_mode(0x2_0000) // READ_CONTROL
            .custom_flags(0x0200_0000) // FILE_FLAG_BACKUP_SEMANTICS
            .open(path)
            .expect("open");
        crate::deny_read_sessions::owner_sid(&file).expect("owner")
    };
    let user = crate::deny_read_sessions::current_user_sid().expect("user");
    assert_ne!(
        owner(&file),
        user,
        "the sandbox's file is the user's already"
    );
    // The upgrade step: the first command with this temp folder.
    let value = temp.path().display().to_string();
    let (stdout, code) = runtime.block_on(run_elevated(
        root.path(),
        home.path(),
        cmd(&system32_exe("hostname.exe")),
        &[("TEMP", value.clone()), ("TMP", value)],
        /*tty*/ false,
    ));
    println!("d377 leftovers upgrade: exit={code} stdout={stdout:?}");
    // A command grants itself access to what its account made, and writes.
    let tamper = root.path().join("d377-tamper.cmd");
    fs::write(
        &tamper,
        format!(
            "@echo off\r\n\
             icacls \"{file}\" /grant \"%USERNAME%:(M)\" >nul 2>nul\r\n\
             (echo x>>\"{file}\") 2>nul && echo FILE-WRITTEN\r\n\
             icacls \"{dir}\" /grant \"%USERNAME%:(OI)(CI)(M)\" >nul 2>nul\r\n\
             (echo x>\"{dir}\\new.txt\") 2>nul && echo DIR-WRITTEN\r\n\
             (echo x>>\"{dir}\\inner.txt\") 2>nul && echo INNER-WRITTEN\r\n",
            file = file.display(),
            dir = dir.display()
        ),
    )
    .expect("write script");
    let (stdout, code) = runtime.block_on(run_elevated(
        root.path(),
        home.path(),
        cmd(&tamper),
        &[],
        /*tty*/ false,
    ));
    println!("d377 leftovers tamper: exit={code} stdout={stdout:?}");
    for written in ["FILE-WRITTEN", "DIR-WRITTEN", "INNER-WRITTEN"] {
        assert!(!stdout.contains(written), "{written}: {stdout}");
    }
    // Taken, not deleted.
    for path in [&file, &dir, &dir.join("inner.txt"), &users] {
        assert_eq!(owner(path), user, "{}", path.display());
    }
    assert_eq!(fs::read_to_string(&file).expect("read").trim(), "made");
    assert_eq!(fs::read_to_string(&users).expect("read"), "other");
    crate::session_temp::end_session_temp_in(temp.path());
    crate::session_temp::end_session_temp_in(
        &dunce::canonicalize(std::env::var_os("TEMP").expect("TEMP")).expect("TEMP"),
    );
    record_setup(home.path());
}
