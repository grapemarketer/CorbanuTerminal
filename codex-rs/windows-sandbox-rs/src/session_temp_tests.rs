use super::*;
use codex_protocol::models::PermissionProfile;
use pretty_assertions::assert_eq;
use std::os::windows::fs::FileTimesExt as _;
use std::os::windows::fs::OpenOptionsExt as _;
use std::time::SystemTime;
use tempfile::TempDir;

const FILE_WRITE_ATTRIBUTES: u32 = 0x100;

fn permissions(profile: &PermissionProfile) -> ResolvedWindowsSandboxPermissions {
    ResolvedWindowsSandboxPermissions::try_from_permission_profile_for_workspace_roots(profile, &[])
        .expect("permissions")
}

fn env_for(base: &Path) -> HashMap<String, String> {
    let base = base.to_string_lossy().into_owned();
    HashMap::from([
        ("TEMP".to_string(), base.clone()),
        ("TMP".to_string(), base),
    ])
}

fn dacl_is_protected(path: &Path) -> bool {
    use windows_sys::Win32::Foundation::LocalFree;
    use windows_sys::Win32::Security::Authorization::GetNamedSecurityInfoW;
    use windows_sys::Win32::Security::DACL_SECURITY_INFORMATION;
    use windows_sys::Win32::Security::GetSecurityDescriptorControl;
    use windows_sys::Win32::Security::SE_DACL_PROTECTED;
    let mut descriptor: *mut std::ffi::c_void = std::ptr::null_mut();
    let mut control: u16 = 0;
    let mut revision: u32 = 0;
    // SAFETY: reads `path`'s DACL; `descriptor` is freed below.
    unsafe {
        let status = GetNamedSecurityInfoW(
            crate::winutil::to_wide(path).as_ptr(),
            1,
            DACL_SECURITY_INFORMATION,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            &mut descriptor,
        );
        assert_eq!(status, 0, "read the DACL of {}", path.display());
        GetSecurityDescriptorControl(descriptor, &mut control, &mut revision);
        LocalFree(descriptor as _);
    }
    control & SE_DACL_PROTECTED != 0
}

fn set_created(path: &Path, age: Duration) {
    std::fs::OpenOptions::new()
        .access_mode(FILE_WRITE_ATTRIBUTES)
        .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE)
        .custom_flags(FILE_FLAG_BACKUP_SEMANTICS)
        .open(path)
        .expect("open for times")
        .set_times(std::fs::FileTimes::new().set_created(SystemTime::now() - age))
        .expect("set creation time");
}

/// Whether `sid` has a write allow entry on `path` (explicit or inherited).
fn has_write_allow(path: &Path, sid: &str) -> bool {
    // SAFETY: the SID and descriptor are freed below.
    unsafe {
        let psid = crate::token::convert_string_sid_to_sid(sid).expect("sid");
        let (dacl, descriptor) = crate::acl::fetch_dacl_handle(path).expect("dacl");
        let has = crate::acl::dacl_has_write_allow_for_sid(dacl, psid);
        windows_sys::Win32::Foundation::LocalFree(descriptor as _);
        windows_sys::Win32::Foundation::LocalFree(psid as _);
        has
    }
}

#[test]
fn temp_points_at_a_private_folder_of_the_session() {
    let base = TempDir::new().expect("base");
    let home = TempDir::new().expect("home");
    let workspace_write = permissions(&PermissionProfile::workspace_write());
    let mut env = env_for(base.path());
    use_session_temp(&workspace_write, &mut env, home.path()).expect("session temp");

    let dir = PathBuf::from(&env["TEMP"]);
    assert_eq!(env["TMP"], env["TEMP"]);
    assert_eq!(
        dir.parent().map(canonical_path_key),
        Some(canonical_path_key(base.path()))
    );
    let name = dir
        .file_name()
        .expect("name")
        .to_string_lossy()
        .into_owned();
    assert!(name.starts_with(SESSION_TEMP_PREFIX), "{name}");
    assert!(dir.is_dir());
    assert!(dacl_is_protected(&dir), "inherits from the temp folder");
    // Held: not removed as left behind, nor deletable meanwhile.
    assert!(!is_left_behind(&dir, current_user_sid().ok().as_deref()));
    assert!(std::fs::remove_dir(&dir).is_err());

    // One folder per temp folder and session, however the environment names it.
    let mut again = env_for(base.path());
    use_session_temp(&workspace_write, &mut again, home.path()).expect("again");
    assert_eq!(again, env);
    let mut rewritten = env.clone();
    use_session_temp(&workspace_write, &mut rewritten, home.path()).expect("rewritten");
    assert_eq!(rewritten, env);

    end_session_temp_in(base.path());
    assert!(!dir.exists());
}

#[test]
fn read_only_profile_keeps_the_temp_folder() {
    let base = TempDir::new().expect("base");
    let home = TempDir::new().expect("home");
    let mut env = env_for(base.path());
    use_session_temp(
        &permissions(&PermissionProfile::read_only()),
        &mut env,
        home.path(),
    )
    .expect("session temp");
    assert_eq!(env, env_for(base.path()));
    assert!(
        lock_sessions()
            .get(&canonical_path_key(base.path()))
            .is_none()
    );
}

#[test]
fn tmpdir_follows_only_when_it_names_the_temp_folder() {
    let base = TempDir::new().expect("base");
    let other = TempDir::new().expect("other");
    let home = TempDir::new().expect("home");
    let workspace_write = permissions(&PermissionProfile::workspace_write());

    let mut env = env_for(base.path());
    env.insert(
        "TMPDIR".to_string(),
        base.path().to_string_lossy().into_owned(),
    );
    use_session_temp(&workspace_write, &mut env, home.path()).expect("session temp");
    assert_eq!(env["TMPDIR"], env["TEMP"]);

    let mut env = env_for(base.path());
    let other_value = other.path().to_string_lossy().into_owned();
    env.insert("TMPDIR".to_string(), other_value.clone());
    use_session_temp(&workspace_write, &mut env, home.path()).expect("session temp");
    assert_eq!(env["TMPDIR"], other_value);
    end_session_temp_in(base.path());
}

#[test]
fn left_behind_folders_are_removed_but_live_and_new_ones_kept() {
    let base = TempDir::new().expect("base");
    let home = TempDir::new().expect("home");
    let (stale, stale_hold) = make_session_temp(base.path()).expect("stale");
    drop(stale_hold);
    std::fs::write(stale.join("left.txt"), "x").expect("left file");
    set_created(&stale, Duration::from_secs(600));
    let (live, _live_hold) = make_session_temp(base.path()).expect("live");
    set_created(&live, Duration::from_secs(600));
    let (young, young_hold) = make_session_temp(base.path()).expect("young");
    drop(young_hold);
    let stale_sid =
        crate::cap::writable_root_cap_sid_for_path(home.path(), &stale).expect("stale sid");
    let live_sid =
        crate::cap::writable_root_cap_sid_for_path(home.path(), &live).expect("live sid");

    remove_left_behind(base.path(), home.path());

    assert!(!stale.exists());
    assert!(live.is_dir());
    assert!(young.is_dir());
    assert_eq!(existing_writable_root_cap_sid(home.path(), &stale), None);
    assert_eq!(
        existing_writable_root_cap_sid(home.path(), &live),
        Some(live_sid)
    );
    assert_ne!(stale_sid, String::new());
}

#[test]
fn the_old_grant_on_the_temp_folder_is_taken_back() {
    let base = TempDir::new().expect("base");
    let home = TempDir::new().expect("home");
    // What older setups left: this home's capability entry for the temp
    // folder, inherited by what is in it.
    let sid = crate::cap::writable_root_cap_sid_for_path(home.path(), base.path()).expect("sid");
    // SAFETY: the SID is freed below; the folder exists.
    unsafe {
        let psid = crate::token::convert_string_sid_to_sid(&sid).expect("psid");
        crate::acl::ensure_allow_write_aces(base.path(), &[psid]).expect("grant");
        windows_sys::Win32::Foundation::LocalFree(psid as _);
    }
    let child = base.path().join("other-program.tmp");
    std::fs::write(&child, "x").expect("child");
    assert!(has_write_allow(base.path(), &sid));
    assert!(has_write_allow(&child, &sid));

    take_back_old_grant(base.path(), home.path());

    assert!(!has_write_allow(base.path(), &sid));
    assert!(!has_write_allow(&child, &sid));
}
