//! D377 (#304): the temp folder of an elevated command.
//!
//! Workspace-write makes the temp folder (`TEMP`/`TMP`) writable. The
//! elevated setup's write grants are for the sandbox's users group and are
//! never taken back, so granting the user's `%TEMP%` gave every sandbox
//! account lasting Modify on all of it: other programs' temp files and other
//! sessions'. Instead each Core process (session) gets a folder of its own in
//! that temp folder, and its elevated commands get it as `TEMP` and `TMP`
//! (and `TMPDIR` where that names the same folder). Only that folder is
//! granted.
//!
//! The folder is made by the user with a protected DACL (the user, SYSTEM and
//! Administrators), so it inherits nothing from the temp folder, and the
//! session holds it open without delete sharing while it lives, so no one can
//! remove or rename it meanwhile. It also holds a file in it (the marker)
//! open without delete or write sharing: a sandboxed command can neither
//! delete it nor so empty the folder, and Windows makes only an empty folder
//! a junction or mount point. So the folder setup grants stays the folder
//! the session made, which [`use_session_temp`] also checks each time before
//! a command gets it (a sandboxed command that turned it into a link would
//! otherwise have the next setup grant the sandbox the link's target).
//! It is removed when the session ends
//! ([`end_session_temp_folders`]); one a session left behind (it crashed or
//! was killed) is removed by the next session that uses the same temp folder.
//!
//! Upgrade: the first time a session uses a temp folder, it takes back, as
//! the user, the grant older versions put on that folder itself (the sandbox
//! group's entry and this `CODEX_HOME`'s capability entry).

use crate::cap::existing_writable_root_cap_sid;
use crate::cap::forget_writable_root_cap_sids;
use crate::deny_read_sessions::Descriptor;
use crate::deny_read_sessions::current_user_sid;
use crate::deny_read_sessions::is_link;
use crate::deny_read_sessions::owner_sid;
use crate::logging::log_note;
use crate::path_normalization::canonical_path_key;
use crate::resolved_permissions::ResolvedWindowsSandboxPermissions;
use anyhow::Context;
use anyhow::Result;
use rand::Rng;
use rand::SeedableRng;
use rand::rngs::SmallRng;
use std::collections::BTreeSet;
use std::collections::HashMap;
use std::fs::File;
use std::os::windows::fs::OpenOptionsExt as _;
use std::path::Path;
use std::path::PathBuf;
use std::sync::LazyLock;
use std::sync::Mutex;
use std::time::Duration;

/// The name every session temp folder starts with.
pub(crate) const SESSION_TEMP_PREFIX: &str = "corbanu-sandbox-tmp-";
/// A folder younger than this is never removed as left behind: its session
/// may be between making it and opening it.
const STALE_AFTER: Duration = Duration::from_secs(120);
/// The file each session folder holds open while its session lives.
const MARKER: &str = ".corbanu-session";
const TEMP_KEYS: [&str; 2] = ["TMP", "TEMP"];
const DELETE: u32 = 0x1_0000;
const READ_CONTROL: u32 = 0x2_0000;
const FILE_READ_ATTRIBUTES: u32 = 0x80;
const FILE_READ_DATA: u32 = 0x1;
const FILE_LIST_DIRECTORY: u32 = 0x1;
const FILE_SHARE_READ: u32 = 0x1;
const FILE_SHARE_WRITE: u32 = 0x2;
const FILE_SHARE_DELETE: u32 = 0x4;
const FILE_FLAG_BACKUP_SEMANTICS: u32 = 0x0200_0000;
const FILE_FLAG_OPEN_REPARSE_POINT: u32 = 0x0020_0000;

/// What keeps a session folder the session's while it lives.
pub(crate) struct Hold {
    /// The folder, open without delete sharing.
    dir: File,
    /// The marker in it, open without delete or write sharing.
    _marker: File,
}

struct SessionTemp {
    dir: PathBuf,
    hold: Hold,
    /// The `CODEX_HOME`s that recorded a capability SID for `dir`.
    homes: BTreeSet<PathBuf>,
}

/// This process's folders, by the canonical key of the temp folder they are in.
static SESSIONS: LazyLock<Mutex<HashMap<String, SessionTemp>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

/// Points `env_map`'s temp folder at this session's folder in it (making that
/// folder on first use) when the profile makes the temp folder writable.
/// Unchanged otherwise, or when there is no temp folder to use.
pub(crate) fn use_session_temp(
    permissions: &ResolvedWindowsSandboxPermissions,
    env_map: &mut HashMap<String, String>,
    codex_home: &Path,
) -> Result<()> {
    if !permissions.has_writable_tmpdir_entry() {
        return Ok(());
    }
    let configured = configured_temp_folders(env_map);
    let Some(base) = configured.first().cloned() else {
        return Ok(());
    };
    let dir = session_temp_for(&base, &configured, codex_home)?;
    let value = dir.to_string_lossy().into_owned();
    let tmpdir_is_temp = env_value(env_map, "TMPDIR").is_some_and(|tmpdir| {
        configured
            .iter()
            .any(|folder| same_path(Path::new(&tmpdir), folder))
    });
    let mut keys = TEMP_KEYS.to_vec();
    if tmpdir_is_temp {
        keys.push("TMPDIR");
    }
    for key in keys {
        env_map.retain(|name, _| !name.eq_ignore_ascii_case(key));
        env_map.insert(key.to_string(), value.clone());
    }
    Ok(())
}

/// Removes this session's temp folders. Commands started afterwards get new
/// ones. Called once the session's threads have shut down.
pub fn end_session_temp_folders() {
    let sessions = std::mem::take(&mut *lock_sessions());
    for session in sessions.into_values() {
        remove_session_temp(session);
    }
}

/// Removes this session's folder in `base` only (tests share one process).
#[cfg(test)]
pub(crate) fn end_session_temp_in(base: &Path) {
    let session = lock_sessions().remove(&canonical_path_key(base));
    if let Some(session) = session {
        remove_session_temp(session);
    }
}

fn lock_sessions() -> std::sync::MutexGuard<'static, HashMap<String, SessionTemp>> {
    SESSIONS
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

fn remove_session_temp(session: SessionTemp) {
    let intact = is_intact(&session.dir, &session.hold);
    forget_session_cap_sids(&session);
    let SessionTemp { dir, hold, .. } = session;
    drop(hold);
    if intact {
        let _ = std::fs::remove_dir_all(&dir);
    }
}

fn forget_session_cap_sids(session: &SessionTemp) {
    let key = canonical_path_key(&session.dir);
    for home in &session.homes {
        let _ = forget_writable_root_cap_sids(home, |recorded| recorded == key);
    }
}

/// Whether `dir` still is the folder `hold` holds: a folder, not a link or
/// other reparse point, with the same file ID.
pub(crate) fn is_intact(dir: &Path, hold: &Hold) -> bool {
    use std::os::windows::io::AsRawHandle as _;
    // Attribute rights only: not stopped by the hold's sharing.
    let Ok(now) = std::fs::OpenOptions::new()
        .access_mode(FILE_READ_ATTRIBUTES)
        .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE)
        .custom_flags(FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT)
        .open(dir)
    else {
        return false;
    };
    // SAFETY: both handles are open.
    let same = unsafe {
        let held = crate::acl::file_identity(hold.dir.as_raw_handle() as _);
        let found = crate::acl::file_identity(now.as_raw_handle() as _);
        matches!((held, found), (Ok(held), Ok(found)) if held == found)
    };
    same && now.metadata().is_ok_and(|meta| meta.is_dir()) && is_link(&now).is_ok_and(|link| !link)
}

fn env_value(env_map: &HashMap<String, String>, key: &str) -> Option<String> {
    env_map
        .iter()
        .find(|(name, _)| name.eq_ignore_ascii_case(key))
        .map(|(_, value)| value.clone())
}

fn same_path(a: &Path, b: &Path) -> bool {
    canonical_path_key(a) == canonical_path_key(b)
}

/// The temp folders a command would get (`TMP` first, as `GetTempPath`
/// reads it), from `env_map` or this process; absolute ones only, no repeats.
fn configured_temp_folders(env_map: &HashMap<String, String>) -> Vec<PathBuf> {
    let mut folders: Vec<PathBuf> = Vec::new();
    for key in TEMP_KEYS {
        let Some(value) = env_value(env_map, key)
            .map(PathBuf::from)
            .or_else(|| std::env::var_os(key).map(PathBuf::from))
        else {
            continue;
        };
        if value.is_absolute() && !folders.iter().any(|known| same_path(known, &value)) {
            folders.push(value);
        }
    }
    folders
}

/// This session's folder in `base`. When `base` already is one (the
/// environment was pointed at it before), that folder.
fn session_temp_for(base: &Path, configured: &[PathBuf], codex_home: &Path) -> Result<PathBuf> {
    let mut sessions = lock_sessions();
    if let Some(session) = sessions
        .values_mut()
        .find(|session| same_path(&session.dir, base))
    {
        if !is_intact(&session.dir, &session.hold) {
            anyhow::bail!(
                "the sandbox's temp folder {} is no longer the folder this session made",
                session.dir.display()
            );
        }
        session.homes.insert(codex_home.to_path_buf());
        return Ok(session.dir.clone());
    }
    let key = canonical_path_key(base);
    if let Some(session) = sessions.get_mut(&key) {
        if is_intact(&session.dir, &session.hold) {
            session.homes.insert(codex_home.to_path_buf());
            return Ok(session.dir.clone());
        }
        // Not ours any more: forget it (without removing what is there now)
        // and make a new one.
        log_note(
            &format!(
                "D377: the sandbox's temp folder {} is no longer the folder this session made; making a new one",
                session.dir.display()
            ),
            Some(&codex_home.join(".sandbox")),
        );
        if let Some(session) = sessions.remove(&key) {
            forget_session_cap_sids(&session);
        }
    }
    for folder in configured {
        take_back_old_grant(folder, codex_home);
    }
    remove_left_behind(base, codex_home);
    let (dir, hold) = make_session_temp(base)?;
    let session = SessionTemp {
        dir: dir.clone(),
        hold,
        homes: BTreeSet::from([codex_home.to_path_buf()]),
    };
    sessions.insert(key, session);
    Ok(dir)
}

/// Makes a new folder in `base` that only the user, SYSTEM and Administrators
/// may use, with the marker in it, and holds both.
fn make_session_temp(base: &Path) -> Result<(PathBuf, Hold)> {
    let user = current_user_sid()?;
    let descriptor = Descriptor::parse(&format!(
        "O:{user}D:P(A;OICI;FA;;;SY)(A;OICI;FA;;;BA)(A;OICI;FA;;;{user})"
    ))?;
    let mut rng = SmallRng::from_entropy();
    let dir = base.join(format!(
        "{SESSION_TEMP_PREFIX}{}-{:016x}",
        std::process::id(),
        rng.r#gen::<u64>()
    ));
    descriptor
        .create_dir(&dir)
        .with_context(|| format!("make the sandbox's temp folder in {}", base.display()))?;
    // `FILE_LIST_DIRECTORY`: a handle with attribute rights only is exempt
    // from share checks, so it would not keep the folder from being removed.
    let hold = std::fs::OpenOptions::new()
        .access_mode(FILE_LIST_DIRECTORY | FILE_READ_ATTRIBUTES)
        .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE)
        .custom_flags(FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT)
        .open(&dir)
        .with_context(|| format!("open {}", dir.display()))?;
    // Protected and the user's only, so no grant setup puts on the folder
    // reaches it; the sharing keeps anyone from deleting, renaming or
    // writing it while the session lives.
    let marker = match Descriptor::parse(&format!(
        "O:{user}D:P(A;;FA;;;SY)(A;;FA;;;BA)(A;;FA;;;{user})"
    ))
    .and_then(|marker| marker.create_new_file(&dir.join(MARKER), FILE_READ_DATA, FILE_SHARE_READ))
    {
        Ok(marker) => marker,
        Err(err) => {
            drop(hold);
            let _ = std::fs::remove_dir_all(&dir);
            return Err(err);
        }
    };
    Ok((
        dir,
        Hold {
            dir: hold,
            _marker: marker,
        },
    ))
}

/// Removes the session temp folders in `base` that no live session holds,
/// are older than [`STALE_AFTER`] and belong to this user, and forgets their
/// capability SIDs in `codex_home`. Best effort.
fn remove_left_behind(base: &Path, codex_home: &Path) {
    let Ok(entries) = std::fs::read_dir(base) else {
        return;
    };
    let user = current_user_sid().ok();
    for entry in entries.flatten() {
        if !entry
            .file_name()
            .to_string_lossy()
            .to_ascii_lowercase()
            .starts_with(SESSION_TEMP_PREFIX)
        {
            continue;
        }
        let path = entry.path();
        if is_left_behind(&path, user.as_deref()) {
            let _ = std::fs::remove_dir_all(&path);
        }
    }
    let _ = forget_writable_root_cap_sids(codex_home, |key| {
        key.rsplit('/')
            .next()
            .is_some_and(|name| name.starts_with(SESSION_TEMP_PREFIX))
            && !Path::new(key).exists()
    });
}

fn is_left_behind(path: &Path, user: Option<&str>) -> bool {
    // First without `DELETE` (rights that share checks skip), so a new
    // session's folder, between being made and held, is never opened in a
    // way that would keep its session from holding it.
    let Ok(look) = std::fs::OpenOptions::new()
        .access_mode(READ_CONTROL | FILE_READ_ATTRIBUTES)
        .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE)
        .custom_flags(FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT)
        .open(path)
    else {
        return false;
    };
    let Ok(meta) = look.metadata() else {
        return false;
    };
    let old = meta
        .created()
        .ok()
        .and_then(|created| created.elapsed().ok())
        .is_some_and(|age| age >= STALE_AFTER);
    if !(meta.is_dir()
        && old
        && !is_link(&look).unwrap_or(true)
        && user.is_some_and(|user| owner_sid(&look).is_ok_and(|owner| owner == user)))
    {
        return false;
    }
    // A live session holds it open without delete sharing: this fails
    // (`ERROR_SHARING_VIOLATION`; anything else also keeps it).
    std::fs::OpenOptions::new()
        .access_mode(DELETE | FILE_READ_ATTRIBUTES)
        .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE)
        .custom_flags(FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT)
        .open(path)
        .is_ok()
}

/// Takes back the write grant older versions' setup put on the temp folder
/// itself: the sandbox group's allow entries and this `CODEX_HOME`'s
/// capability entry for it. Best effort; logged.
fn take_back_old_grant(folder: &Path, codex_home: &Path) {
    let mut sids: Vec<String> = Vec::new();
    if let Ok(group) = crate::winutil::resolve_sid(crate::token::SANDBOX_USERS_GROUP)
        && let Ok(group) = crate::winutil::string_from_sid_bytes(&group)
    {
        sids.push(group);
    }
    sids.extend(existing_writable_root_cap_sid(codex_home, folder));
    let psids: Vec<*mut std::ffi::c_void> = sids
        .iter()
        // SAFETY: freed below.
        .filter_map(|sid| unsafe { crate::token::convert_string_sid_to_sid(sid) })
        .collect();
    if !psids.is_empty() && folder.is_dir() {
        // SAFETY: valid SIDs; the folder exists.
        match unsafe { crate::acl::remove_explicit_allow_aces(folder, &psids) } {
            Ok(true) => log_note(
                &format!(
                    "D377: removed the sandbox's old write grant on {}",
                    folder.display()
                ),
                Some(&codex_home.join(".sandbox")),
            ),
            Ok(false) => {}
            Err(err) => log_note(
                &format!(
                    "D377: could not remove the sandbox's old write grant on {}: {err:#}",
                    folder.display()
                ),
                Some(&codex_home.join(".sandbox")),
            ),
        }
    }
    for psid in psids {
        // SAFETY: allocated by `convert_string_sid_to_sid`.
        unsafe {
            windows_sys::Win32::Foundation::LocalFree(
                psid as windows_sys::Win32::Foundation::HLOCAL,
            )
        };
    }
}

#[cfg(test)]
#[path = "session_temp_tests.rs"]
mod tests;
