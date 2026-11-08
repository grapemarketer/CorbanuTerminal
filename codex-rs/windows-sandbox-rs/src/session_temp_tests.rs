use super::*;
use codex_protocol::models::PermissionProfile;
use pretty_assertions::assert_eq;
use std::os::windows::fs::FileTimesExt as _;
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
    // Its marker can be neither deleted nor written, so the folder cannot be
    // emptied (and made a link).
    let marker = dir.join(MARKER);
    assert!(dacl_is_protected(&marker));
    assert!(std::fs::remove_file(&marker).is_err());
    assert!(
        std::fs::OpenOptions::new()
            .write(true)
            .open(&marker)
            .is_err()
    );
    assert!(marker.is_file());

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

#[test]
fn a_folder_that_is_no_longer_the_one_made_is_not_intact() {
    let base = TempDir::new().expect("base");
    let (dir, hold) = make_session_temp(base.path()).expect("session folder");
    let (other, other_hold) = make_session_temp(base.path()).expect("other folder");
    assert!(is_intact(&dir, &hold));
    assert!(!is_intact(&dir, &other_hold));
    assert!(!is_intact(&base.path().join("missing"), &hold));
    // A junction to the folder resolves to it, but is not it.
    let link = base.path().join("link");
    let status = std::process::Command::new("cmd")
        .args(["/d", "/c", "mklink", "/J"])
        .arg(&link)
        .arg(&dir)
        .status()
        .expect("mklink");
    assert!(status.success(), "mklink /J failed");
    assert!(!is_intact(&link, &hold));
    let _ = std::fs::remove_dir(&link);
    drop((hold, other_hold));
    let _ = std::fs::remove_dir_all(&dir);
    let _ = std::fs::remove_dir_all(&other);
}

/// Tries to make `dir` a mount point (a junction) to `target` the way a
/// sandboxed command with write access to it could: through a handle with
/// attribute rights only, which no sharing stops.
fn try_make_mount_point(dir: &Path, target: &Path) -> std::io::Result<()> {
    use std::os::windows::io::AsRawHandle as _;
    use windows_sys::Win32::System::IO::DeviceIoControl;
    const FSCTL_SET_REPARSE_POINT: u32 = 0x0009_00A4;
    const IO_REPARSE_TAG_MOUNT_POINT: u32 = 0xA000_0003;
    let substitute: Vec<u16> = format!(r"\??\{}", target.display())
        .encode_utf16()
        .collect();
    let print: Vec<u16> = target.display().to_string().encode_utf16().collect();
    let mut names: Vec<u16> = Vec::new();
    names.extend(&substitute);
    names.push(0);
    names.extend(&print);
    names.push(0);
    let header = [
        0_u16,
        (substitute.len() * 2) as u16,
        ((substitute.len() + 1) * 2) as u16,
        (print.len() * 2) as u16,
    ];
    let data_len = (header.len() + names.len()) * 2;
    let mut buffer: Vec<u8> = Vec::new();
    buffer.extend(IO_REPARSE_TAG_MOUNT_POINT.to_le_bytes());
    buffer.extend((data_len as u16).to_le_bytes());
    buffer.extend(0_u16.to_le_bytes());
    for word in header.iter().chain(names.iter()) {
        buffer.extend(word.to_le_bytes());
    }
    let handle = std::fs::OpenOptions::new()
        .access_mode(FILE_WRITE_ATTRIBUTES)
        .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE)
        .custom_flags(FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT)
        .open(dir)?;
    let mut returned = 0_u32;
    // SAFETY: a valid handle and buffer.
    let ok = unsafe {
        DeviceIoControl(
            handle.as_raw_handle() as _,
            FSCTL_SET_REPARSE_POINT,
            buffer.as_ptr().cast(),
            buffer.len() as u32,
            std::ptr::null_mut(),
            0,
            &mut returned,
            std::ptr::null_mut(),
        )
    };
    if ok == 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(())
}

#[test]
fn a_held_session_folder_cannot_be_made_a_mount_point() {
    let base = TempDir::new().expect("base");
    let target = TempDir::new().expect("target");
    let (dir, hold) = make_session_temp(base.path()).expect("session folder");
    // Held, with its marker in it: refused even to its owner.
    let err = try_make_mount_point(&dir, target.path()).expect_err("made a mount point");
    // ERROR_DIR_NOT_EMPTY
    assert_eq!(err.raw_os_error(), Some(145), "{err}");
    assert!(is_intact(&dir, &hold));

    // Control: without the marker, holding the folder alone does not stop it.
    use std::os::windows::fs::MetadataExt as _;
    let Hold {
        dir: dir_hold,
        _marker: marker,
    } = hold;
    drop(marker);
    std::fs::remove_file(dir.join(MARKER)).expect("remove the marker");
    try_make_mount_point(&dir, target.path()).expect("an empty folder becomes a mount point");
    let attributes = std::fs::symlink_metadata(&dir).expect("metadata");
    assert_ne!(attributes.file_attributes() & 0x400, 0);
    drop(dir_hold);
    let _ = std::fs::remove_dir(&dir);
}
