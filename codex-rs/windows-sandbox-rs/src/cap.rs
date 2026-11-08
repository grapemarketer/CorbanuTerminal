use crate::deny_read_sessions::Descriptor;
use crate::deny_read_sessions::current_user_sid;
use crate::path_normalization::canonical_path_key;
use crate::path_normalization::canonicalize_path;
use anyhow::Context;
use anyhow::Result;
use rand::RngCore;
use rand::SeedableRng;
use rand::rngs::SmallRng;
use serde::Deserialize;
use serde::Serialize;
use std::collections::HashMap;
use std::fs;
use std::fs::TryLockError;
use std::path::Path;
use std::path::PathBuf;
use std::time::Duration;
use std::time::Instant;

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct CapSids {
    pub workspace: String,
    pub readonly: String,
    /// Per-workspace capability SIDs keyed by canonicalized CWD string.
    ///
    /// This is used to isolate workspaces from other workspace sandbox writes and to
    /// apply per-workspace denies (e.g. protect `CWD/.codex`)
    /// without permanently affecting other workspaces.
    #[serde(default)]
    pub workspace_by_cwd: HashMap<String, String>,
    /// Per-write-root capability SIDs keyed by canonicalized write-root path.
    ///
    /// These are included in a workspace-write token only when the root is
    /// currently allowed, so stale ACLs from earlier extra roots do not expand
    /// later workspace sandboxes.
    #[serde(default)]
    pub writable_root_by_path: HashMap<String, String>,
}

pub fn cap_sid_file(codex_home: &Path) -> PathBuf {
    codex_home.join("cap_sid")
}

fn make_random_cap_sid_string() -> String {
    let mut rng = SmallRng::from_entropy();
    let a = rng.next_u32();
    let b = rng.next_u32();
    let c = rng.next_u32();
    let d = rng.next_u32();
    format!("S-1-5-21-{a}-{b}-{c}-{d}")
}

/// D259 (#1507): a fresh SID that identifies one legacy command. Never
/// persisted or reused: no ACL names it, so it grants nothing but the
/// command's own objects (through its token's default DACL).
pub(crate) fn new_command_identity_sid_string() -> String {
    make_random_cap_sid_string()
}

fn persist_caps(path: &Path, caps: &CapSids) -> Result<()> {
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir).with_context(|| format!("create cap sid dir {}", dir.display()))?;
    }
    let json = serde_json::to_string(caps)?;
    // Whole or not at all: a reader never sees a half-written file.
    let mut staging = path.as_os_str().to_os_string();
    staging.push(format!(
        ".{}-{:08x}.tmp",
        std::process::id(),
        SmallRng::from_entropy().next_u32()
    ));
    let staging = PathBuf::from(staging);
    fs::write(&staging, json)
        .with_context(|| format!("write cap sid file {}", staging.display()))?;
    fs::rename(&staging, path).map_err(|err| {
        let _ = fs::remove_file(&staging);
        anyhow::Error::new(err).context(format!("replace cap sid file {}", path.display()))
    })?;
    Ok(())
}

/// The file whose lock serializes updates of [`cap_sid_file`].
fn cap_sid_lock_file(codex_home: &Path) -> PathBuf {
    codex_home.join("cap_sid.lock")
}

/// How long an update waits for another process's update to finish.
const LOCK_WAIT: Duration = Duration::from_secs(30);

fn new_caps(workspace: String) -> CapSids {
    CapSids {
        workspace,
        readonly: make_random_cap_sid_string(),
        workspace_by_cwd: HashMap::new(),
        writable_root_by_path: HashMap::new(),
    }
}

/// The SIDs recorded in `path`, and whether they must be written back (an
/// older plain-text file). `None` when there is none to use.
fn read_caps(path: &Path) -> Result<Option<(CapSids, bool)>> {
    let txt = match fs::read_to_string(path) {
        Ok(txt) => txt,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(err) => {
            return Err(err).with_context(|| format!("read cap sid file {}", path.display()));
        }
    };
    let t = txt.trim();
    if t.starts_with('{') && t.ends_with('}') {
        return Ok(serde_json::from_str::<CapSids>(t)
            .ok()
            .map(|caps| (caps, false)));
    }
    Ok((!t.is_empty()).then(|| (new_caps(t.to_string()), true)))
}

/// Opens the update lock, making it if missing open only to the user,
/// SYSTEM and Administrators: a sandboxed command that could open it could
/// hold the lock and stall every update.
fn open_lock(path: &Path) -> Result<fs::File> {
    const GENERIC_READ: u32 = 0x8000_0000;
    const GENERIC_WRITE: u32 = 0x4000_0000;
    const FILE_SHARE_ALL: u32 = 0x7;
    let open = || fs::OpenOptions::new().read(true).write(true).open(path);
    match open() {
        Ok(file) => return Ok(file),
        Err(err) if err.kind() != std::io::ErrorKind::NotFound => {
            return Err(err).with_context(|| format!("open {}", path.display()));
        }
        Err(_) => {}
    }
    let user = current_user_sid()?;
    let made = Descriptor::parse(&format!(
        "O:{user}D:P(A;;FA;;;SY)(A;;FA;;;BA)(A;;FA;;;{user})"
    ))
    .and_then(|descriptor| {
        descriptor.create_new_file(path, GENERIC_READ | GENERIC_WRITE, FILE_SHARE_ALL)
    });
    match made {
        Ok(file) => Ok(file),
        // Another process made it meanwhile.
        Err(_) => open().with_context(|| format!("open {}", path.display())),
    }
}

/// #1871: runs `update` on the recorded SIDs (made if there are none) with
/// the update lock held from the read to the write, so concurrent updates,
/// in this process or another, never lose one another's SIDs. `update`
/// returns its result and whether it changed the SIDs.
fn update_caps<T>(codex_home: &Path, update: impl FnOnce(&mut CapSids) -> (T, bool)) -> Result<T> {
    fs::create_dir_all(codex_home)
        .with_context(|| format!("create cap sid dir {}", codex_home.display()))?;
    let lock_path = cap_sid_lock_file(codex_home);
    let lock = open_lock(&lock_path)?;
    let deadline = Instant::now() + LOCK_WAIT;
    loop {
        match lock.try_lock() {
            Ok(()) => break,
            Err(TryLockError::WouldBlock) if Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(5));
            }
            Err(TryLockError::WouldBlock) => {
                anyhow::bail!("{} stayed locked for {LOCK_WAIT:?}", lock_path.display());
            }
            Err(TryLockError::Error(err)) => {
                return Err(err).with_context(|| format!("lock {}", lock_path.display()));
            }
        }
    }
    let path = cap_sid_file(codex_home);
    let (mut caps, mut changed) =
        read_caps(&path)?.unwrap_or_else(|| (new_caps(make_random_cap_sid_string()), true));
    let (value, updated) = update(&mut caps);
    changed |= updated;
    if changed {
        persist_caps(&path, &caps)?;
    }
    // Dropping `lock` releases it.
    Ok(value)
}

pub fn load_or_create_cap_sids(codex_home: &Path) -> Result<CapSids> {
    if let Some((caps, false)) = read_caps(&cap_sid_file(codex_home))? {
        return Ok(caps);
    }
    update_caps(codex_home, |caps| (caps.clone(), false))
}

/// The SID `map(caps)` records for `key`, recording a new one if it has none.
fn recorded_or_new_sid(
    codex_home: &Path,
    key: String,
    map: fn(&mut CapSids) -> &mut HashMap<String, String>,
) -> Result<String> {
    if let Some((mut caps, false)) = read_caps(&cap_sid_file(codex_home))?
        && let Some(sid) = map(&mut caps).get(&key)
    {
        return Ok(sid.clone());
    }
    update_caps(codex_home, |caps| {
        let sids = map(caps);
        if let Some(sid) = sids.get(&key) {
            return (sid.clone(), false);
        }
        let sid = make_random_cap_sid_string();
        sids.insert(key, sid.clone());
        (sid, true)
    })
}

/// Returns the workspace-specific capability SID for `cwd`, creating and persisting it if missing.
pub fn workspace_cap_sid_for_cwd(codex_home: &Path, cwd: &Path) -> Result<String> {
    recorded_or_new_sid(codex_home, canonical_path_key(cwd), |caps| {
        &mut caps.workspace_by_cwd
    })
}

/// Returns the capability SID for an additional writable root, creating and persisting it if missing.
pub fn writable_root_cap_sid_for_path(codex_home: &Path, root: &Path) -> Result<String> {
    recorded_or_new_sid(codex_home, canonical_path_key(root), |caps| {
        &mut caps.writable_root_by_path
    })
}

/// The capability SID recorded for the writable root `root`, if any; never
/// makes one.
pub(crate) fn existing_writable_root_cap_sid(codex_home: &Path, root: &Path) -> Option<String> {
    if !cap_sid_file(codex_home).exists() {
        return None;
    }
    load_or_create_cap_sids(codex_home)
        .ok()?
        .writable_root_by_path
        .get(&canonical_path_key(root))
        .cloned()
}

/// Forgets the writable-root capability SIDs whose key `forget` picks (D377:
/// session temp folders that are gone).
pub(crate) fn forget_writable_root_cap_sids(
    codex_home: &Path,
    forget: impl Fn(&str) -> bool,
) -> Result<()> {
    if !cap_sid_file(codex_home).exists() {
        return Ok(());
    }
    update_caps(codex_home, |caps| {
        let before = caps.writable_root_by_path.len();
        caps.writable_root_by_path.retain(|key, _| !forget(key));
        ((), caps.writable_root_by_path.len() != before)
    })
}

pub fn workspace_write_cap_sid_for_root(
    codex_home: &Path,
    cwd: &Path,
    root: &Path,
) -> Result<String> {
    if canonical_path_key(root) == canonical_path_key(cwd) {
        workspace_cap_sid_for_cwd(codex_home, cwd)
    } else {
        writable_root_cap_sid_for_path(codex_home, root)
    }
}

pub fn workspace_write_root_contains_path(root: &Path, path: &Path) -> bool {
    canonicalize_path(path).starts_with(canonicalize_path(root))
}

pub fn workspace_write_root_overlaps_path(root: &Path, path: &Path) -> bool {
    workspace_write_root_contains_path(root, path) || workspace_write_root_contains_path(path, root)
}

pub fn workspace_write_root_specificity(root: &Path) -> usize {
    canonicalize_path(root).components().count()
}

#[cfg(test)]
mod tests {
    use super::load_or_create_cap_sids;
    use super::workspace_cap_sid_for_cwd;
    use super::workspace_write_cap_sid_for_root;
    use super::writable_root_cap_sid_for_path;
    use crate::path_normalization::canonical_path_key;
    use pretty_assertions::assert_eq;
    use std::path::PathBuf;
    use std::sync::Arc;
    use std::sync::Barrier;

    #[test]
    fn equivalent_cwd_spellings_share_workspace_sid_key() {
        let temp = tempfile::tempdir().expect("tempdir");
        let codex_home = temp.path().join("codex-home");
        std::fs::create_dir_all(&codex_home).expect("create codex home");

        let workspace = temp.path().join("WorkspaceRoot");
        std::fs::create_dir_all(&workspace).expect("create workspace root");

        let canonical = dunce::canonicalize(&workspace).expect("canonical workspace root");
        let alt_spelling = PathBuf::from(
            canonical
                .to_string_lossy()
                .replace('\\', "/")
                .to_ascii_uppercase(),
        );

        let first_sid =
            workspace_cap_sid_for_cwd(&codex_home, canonical.as_path()).expect("first sid");
        let second_sid =
            workspace_cap_sid_for_cwd(&codex_home, alt_spelling.as_path()).expect("second sid");

        assert_eq!(first_sid, second_sid);

        let caps = load_or_create_cap_sids(&codex_home).expect("load caps");
        assert_eq!(caps.workspace_by_cwd.len(), 1);
    }

    #[test]
    fn write_roots_get_path_scoped_sids() {
        let temp = tempfile::tempdir().expect("tempdir");
        let codex_home = temp.path().join("codex-home");
        std::fs::create_dir_all(&codex_home).expect("create codex home");

        let workspace = temp.path().join("workspace");
        let extra_root = temp.path().join("extra-root");
        std::fs::create_dir_all(&workspace).expect("create workspace");
        std::fs::create_dir_all(&extra_root).expect("create extra root");

        let workspace_sid = workspace_write_cap_sid_for_root(&codex_home, &workspace, &workspace)
            .expect("workspace sid");
        let extra_sid = workspace_write_cap_sid_for_root(&codex_home, &workspace, &extra_root)
            .expect("extra root sid");

        assert_ne!(workspace_sid, extra_sid);
        assert_eq!(
            extra_sid,
            writable_root_cap_sid_for_path(&codex_home, &extra_root).expect("extra root sid again")
        );

        let caps = load_or_create_cap_sids(&codex_home).expect("load caps");
        assert_eq!(caps.workspace_by_cwd.len(), 1);
        assert_eq!(caps.writable_root_by_path.len(), 1);
    }

    /// #1871: the update lock is the user's only, even in a home others
    /// may read (a sandboxed command that could open it could hold it).
    #[test]
    fn update_lock_is_the_users_only() {
        let temp = tempfile::tempdir().expect("tempdir");
        let codex_home = temp.path().join("codex-home");
        std::fs::create_dir_all(&codex_home).expect("create codex home");
        let granted = std::process::Command::new("icacls")
            .arg(&codex_home)
            .args(["/grant", "*S-1-1-0:(OI)(CI)R"])
            .output()
            .expect("icacls");
        assert!(granted.status.success(), "{granted:?}");
        writable_root_cap_sid_for_path(&codex_home, &temp.path().join("root")).expect("sid");
        let lock = codex_home.join("cap_sid.lock");
        let user = crate::deny_read_sessions::current_user_sid().expect("user");
        let reads = |sid: &str| {
            let sid = crate::token::LocalSid::from_string(sid).expect("sid");
            crate::acl::path_mask_allows(&lock, &[sid.as_ptr()], 0x1, true).expect("dacl")
        };
        assert!(!reads("S-1-1-0"), "Everyone may read the lock");
        assert!(reads(&user), "the user may not read the lock");
    }

    /// #1871: updates made at the same time keep every SID they record, and
    /// first uses agree on one set of SIDs.
    #[test]
    fn concurrent_updates_lose_no_sid() {
        const THREADS: usize = 8;
        const ROOTS: usize = 25;
        for round in 0..5 {
            let temp = tempfile::tempdir().expect("tempdir");
            let codex_home = temp.path().join("codex-home");
            let barrier = Arc::new(Barrier::new(THREADS));
            let mut handles = Vec::new();
            for thread in 0..THREADS {
                let codex_home = codex_home.clone();
                let barrier = Arc::clone(&barrier);
                handles.push(std::thread::spawn(move || {
                    barrier.wait();
                    let workspace = load_or_create_cap_sids(&codex_home)
                        .map(|caps| caps.workspace)
                        .map_err(|err| format!("{err:#}"));
                    let roots: Vec<_> = (0..ROOTS)
                        .map(|i| {
                            let root = PathBuf::from(format!(r"C:\r1871\{thread}\{i}"));
                            let sid = writable_root_cap_sid_for_path(&codex_home, &root)
                                .map_err(|err| format!("{err:#}"));
                            (root, sid)
                        })
                        .collect();
                    (workspace, roots)
                }));
            }
            let results: Vec<_> = handles
                .into_iter()
                .map(|handle| handle.join().expect("thread"))
                .collect();
            let caps = load_or_create_cap_sids(&codex_home).expect("load caps");
            for (workspace, roots) in results {
                assert_eq!(workspace, Ok(caps.workspace.clone()), "round {round}");
                for (root, sid) in roots {
                    let recorded = caps
                        .writable_root_by_path
                        .get(&canonical_path_key(&root))
                        .cloned();
                    assert_eq!(
                        sid.map(Some),
                        Ok(recorded),
                        "round {round}: {}",
                        root.display()
                    );
                }
            }
            assert_eq!(
                caps.writable_root_by_path.len(),
                THREADS * ROOTS,
                "round {round}"
            );
        }
    }
}
