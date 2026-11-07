//! #158, option (b) (security sign-off, Travis 2026-10-13): a writable root
//! carries the Low label only while a legacy session uses it.
//!
//! Legacy commands run at Low integrity ([`crate::integrity`]), so each
//! writable root needs an inheritable Low label. A label left in place lets
//! later Low-integrity programs delete there, such as a read-only session or
//! the PF-27-S08 credential broker in `%TEMP%`. So every session takes a
//! lease on its roots: the first lease on a root records the root's own
//! (explicit) label and labels it Low; when the last lease on it ends, the
//! recorded label is put back and propagates to the folder's children again.
//!
//! The registry is per Windows user, since sessions of different
//! `CODEX_HOME`s share roots such as `%TEMP%`:
//! `%LOCALAPPDATA%\CorbanuTerminalSandbox\integrity-labels`, labelled Medium
//! with no-write-up and no-read-up, so no Low-integrity program can write,
//! read or hold open anything in it. It holds:
//! - `labels.json`: each labelled root and the label to put back;
//! - one `session-*.json` per live session, listing its roots, held open
//!   without delete sharing for the session's lifetime. A file that can be
//!   deleted belongs to a session that ended without releasing its lease (a
//!   crash): its roots no longer count;
//! - `delete-child-denies.json`: each Everyone `FILE_DELETE_CHILD` deny a
//!   legacy session added on a protected path or its parent (#1506 item 1,
//!   D261), recorded before it is added;
//! - `planted-metadata.json`: each `.codex` or `.agents` folder at the top of
//!   a writable root that was absent when a session started and existed when
//!   it ended (#1506 item 2, D262). Core doesn't load these until the user
//!   confirms them ([`planted_project_metadata`],
//!   [`confirm_planted_project_metadata`]);
//! - `registry.lock`, held exclusively while the registry changes.
//!
//! A lease ends when the command's last process exits, so processes it
//! leaves running (a server, say) keep writing until they exit too.
//!
//! Every acquire, every release and the startup sweep
//! ([`repair_stale_integrity_labels`]) put back the label of each recorded
//! root that no live session holds, and remove each recorded deny that lies
//! in no live session's root. A deny that was there before and isn't
//! recorded is not ours: it is never removed.

use crate::acl::add_deny_delete_child_ace;
use crate::acl::has_deny_delete_child_ace;
use crate::acl::remove_deny_delete_child_ace;
use crate::integrity::LabelAce;
use crate::integrity::effective_label_is_low;
use crate::integrity::label_private_dir_medium;
use crate::integrity::label_writable_root_low;
use crate::integrity::read_explicit_labels;
use crate::integrity::set_explicit_labels;
use crate::logging::log_note;
use anyhow::Context;
use anyhow::Result;
use codex_utils_pty::JobObject;
use rand::Rng;
use rand::SeedableRng;
use rand::rngs::SmallRng;
use serde::Deserialize;
use serde::Serialize;
use std::collections::BTreeMap;
use std::collections::BTreeSet;
use std::fs::File;
use std::fs::OpenOptions;
use std::io::Write as _;
use std::os::windows::fs::MetadataExt as _;
use std::os::windows::fs::OpenOptionsExt as _;
use std::path::Path;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;
use std::time::Instant;

const REGISTRY_PARENT: &str = "CorbanuTerminalSandbox";
const REGISTRY_DIR: &str = "integrity-labels";
const LOCK_FILE: &str = "registry.lock";
const LABELS_FILE: &str = "labels.json";
const DENIES_FILE: &str = "delete-child-denies.json";
/// Read by `codex-config` too (`planted_project_metadata.rs`): keep in step.
const PLANTED_FILE: &str = "planted-metadata.json";
/// Project metadata folders Core loads from a project (#1506 item 2).
const PROJECT_METADATA_NAMES: [&str; 2] = [".codex", ".agents"];
const SESSION_PREFIX: &str = "session-";
const SESSION_SUFFIX: &str = ".json";
/// Labelling a large tree under the lock can take a while.
const LOCK_WAIT: Duration = Duration::from_secs(600);
const ERROR_SHARING_VIOLATION: i32 = 32;
const FILE_SHARE_READ: u32 = 0x1;
const FILE_FLAG_OPEN_REPARSE_POINT: u32 = 0x0020_0000;
const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x400;

/// Each labelled root and its own label before the first lease.
#[derive(Default, Serialize, Deserialize)]
struct Labels {
    roots: BTreeMap<PathBuf, Vec<LabelAce>>,
}

/// An Everyone `FILE_DELETE_CHILD` deny on `path` (canonical), as
/// [`crate::acl::add_deny_delete_child_ace`] adds it.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub(crate) struct DeleteChildDeny {
    pub(crate) path: PathBuf,
    pub(crate) inherit_to_subdirs: bool,
}

/// The denies legacy sessions added and a sweep removes again.
#[derive(Default, Serialize, Deserialize)]
struct DeleteChildDenies {
    entries: BTreeSet<DeleteChildDeny>,
}

#[derive(Serialize, Deserialize)]
struct SessionRecord {
    roots: Vec<PathBuf>,
    /// D262: the roots' `.codex`/`.agents` folders absent at the start.
    #[serde(default)]
    absent_metadata: Vec<PathBuf>,
}

/// D262: project metadata folders that appeared while a session ran, and
/// those the user confirmed (until they are absent at a session's start).
#[derive(Default, Serialize, Deserialize)]
struct PlantedMetadata {
    entries: BTreeSet<PathBuf>,
    #[serde(default)]
    confirmed: BTreeSet<PathBuf>,
}

/// A live session's hold on its roots' Low label; released on drop.
pub(crate) struct IntegrityLease {
    registry: PathBuf,
    session: Option<(File, PathBuf)>,
    log_dir: Option<PathBuf>,
    absent_metadata: Vec<PathBuf>,
}

/// `%LOCALAPPDATA%\CorbanuTerminalSandbox`, which a session whose writable
/// root holds it protects like `.git`.
pub(crate) fn registry_parent_dir() -> Option<PathBuf> {
    local_app_data().map(|dir| dir.join(REGISTRY_PARENT))
}

fn registry_dir() -> Option<PathBuf> {
    registry_parent_dir().map(|dir| dir.join(REGISTRY_DIR))
}

fn local_app_data() -> Option<PathBuf> {
    use windows_sys::Win32::System::Com::CoTaskMemFree;
    use windows_sys::Win32::UI::Shell::FOLDERID_LocalAppData;
    use windows_sys::Win32::UI::Shell::SHGetKnownFolderPath;
    let mut path: *mut u16 = std::ptr::null_mut();
    // SAFETY: `path` receives a string the call allocates, freed below.
    let status = unsafe { SHGetKnownFolderPath(&FOLDERID_LocalAppData, 0, 0, &mut path) };
    let result = (status == 0 && !path.is_null()).then(|| {
        // SAFETY: a NUL-terminated string from the call.
        let len = (0..).take_while(|&i| unsafe { *path.add(i) } != 0).count();
        // SAFETY: as above.
        PathBuf::from(String::from_utf16_lossy(unsafe {
            std::slice::from_raw_parts(path, len)
        }))
    });
    // SAFETY: allocated by SHGetKnownFolderPath (null is allowed).
    unsafe { CoTaskMemFree(path.cast()) };
    result
}

/// Labels `roots` (canonical paths) Low for a session, until the lease is
/// dropped. A root another live session already holds keeps its label.
pub(crate) fn acquire(roots: &[PathBuf], log_dir: Option<&Path>) -> Result<IntegrityLease> {
    let registry = ensure_registry()?;
    let lock = lock(&registry)?;
    let mut lease = IntegrityLease {
        registry,
        session: None,
        log_dir: log_dir.map(Path::to_path_buf),
        absent_metadata: absent_project_metadata(roots),
    };
    let wanted: BTreeSet<PathBuf> = roots.iter().cloned().collect();
    let result = acquire_locked(&mut lease, &wanted);
    drop(lock);
    // On failure the lease drops here and puts back what it labelled.
    result.map(|()| lease)
}

fn acquire_locked(lease: &mut IntegrityLease, wanted: &BTreeSet<PathBuf>) -> Result<()> {
    // Damaged records don't stop commands: Core then loads no project
    // metadata at all (see `planted_project_metadata`).
    if let Err(err) = prune_planted(&lease.registry, &lease.absent_metadata) {
        log_note(
            &format!("planted metadata: {err:#}"),
            lease.log_dir.as_deref(),
        );
    }
    // Registered first: whatever happens below, a sweep can undo it.
    lease.session = Some(create_session(
        &lease.registry,
        wanted,
        &lease.absent_metadata,
    )?);
    let mut labels = load_labels(&lease.registry)?;
    sweep(&lease.registry, &mut labels, lease.log_dir.as_deref())?;
    for root in wanted {
        // A root that can't be labelled stays as it is: commands can't write
        // there, which is safe, and the others still work.
        let leased = lease_root(&lease.registry, &mut labels, root);
        // #734: core tells the person about a root left read-only.
        crate::unlabelled_roots::record(root, leased.is_ok());
        if let Err(err) = leased {
            log_note(
                &format!(
                    "integrity labels: writable root {} can't be labelled Low, so sandboxed commands can't write there (labelling needs permission to change the folder's owner settings, WRITE_OWNER, and a file system that holds integrity labels): {err:#}",
                    root.display()
                ),
                lease.log_dir.as_deref(),
            );
        }
    }
    Ok(())
}

fn lease_root(registry: &Path, labels: &mut Labels, root: &Path) -> Result<()> {
    let new = !labels.roots.contains_key(root);
    if new {
        // SAFETY: reads the label of an existing canonical root.
        let original = unsafe { read_explicit_labels(root) }
            .with_context(|| format!("read the integrity label of {}", root.display()))?;
        labels.roots.insert(root.to_path_buf(), original);
        // Recorded before it changes, so a crash cannot lose it.
        save_labels(registry, labels)?;
    }
    // SAFETY: labels an existing canonical root.
    if let Err(err) = unsafe { label_writable_root_low(root) } {
        // Unchanged: nothing to put back later.
        // SAFETY: as above.
        if new && unsafe { read_explicit_labels(root) }.is_ok_and(|now| labels.roots[root] == now) {
            labels.roots.remove(root);
            save_labels(registry, labels)?;
        }
        return Err(err);
    }
    Ok(())
}

impl IntegrityLease {
    /// Ends the lease once no process of the command's `job` runs: right
    /// away when none does, so a caller that reports the exit afterwards
    /// finds the labels back; otherwise, when `descendants_preserved` (the
    /// command exited and left processes running on purpose), on a thread of
    /// its own when the last of them exits. Without it (the job timed out, or
    /// still kills its processes when its handle closes), what is left of the
    /// job is ended now, so nothing Low keeps running in the roots while
    /// their label is put back, and the release keeps no reference to the job.
    pub(crate) fn release_after_job(self, job: Arc<JobObject>, descendants_preserved: bool) {
        if !descendants_preserved {
            let _ = job.terminate();
        }
        if wait_for_empty_job(&job, Duration::from_secs(/*secs*/ 2)) || !descendants_preserved {
            drop(self);
            return;
        }
        std::thread::spawn(move || {
            while !wait_for_empty_job(&job, Duration::from_secs(/*secs*/ 1)) {}
            drop(self);
        });
    }

    fn release(&mut self) -> Result<()> {
        let Some((file, path)) = self.session.take() else {
            return Ok(());
        };
        // If the lock can't be had, the closed session file is stale and
        // the next sweep puts the labels back.
        let lock = lock(&self.registry)?;
        drop(file);
        // Closed now: a sweep that finds it left behind removes it. The
        // folders it saw appear are recorded first; if that fails, the file
        // stays for a later sweep to try again.
        if record_planted(
            &self.registry,
            &self.absent_metadata,
            self.log_dir.as_deref(),
        )
        .is_ok()
        {
            let _ = std::fs::remove_file(&path);
        }
        let mut labels = load_labels(&self.registry)?;
        sweep(&self.registry, &mut labels, self.log_dir.as_deref())?;
        drop(lock);
        Ok(())
    }
}

impl Drop for IntegrityLease {
    fn drop(&mut self) {
        if let Err(err) = self.release() {
            log_note(
                &format!(
                    "integrity labels: could not end this session's lease; the next Windows sandbox command or start puts the labels back: {err:#}"
                ),
                self.log_dir.as_deref(),
            );
        }
    }
}

/// Whether `job` has no process left within `timeout` (or can't be asked).
fn wait_for_empty_job(job: &JobObject, timeout: Duration) -> bool {
    use std::os::windows::io::AsRawHandle as _;
    use windows_sys::Win32::Foundation::HANDLE;
    use windows_sys::Win32::System::JobObjects::JOBOBJECT_BASIC_ACCOUNTING_INFORMATION;
    use windows_sys::Win32::System::JobObjects::JobObjectBasicAccountingInformation;
    use windows_sys::Win32::System::JobObjects::QueryInformationJobObject;
    let deadline = Instant::now() + timeout;
    loop {
        // SAFETY: a zeroed plain-data struct the call fills in.
        let mut info: JOBOBJECT_BASIC_ACCOUNTING_INFORMATION = unsafe { std::mem::zeroed() };
        // SAFETY: `job` holds a valid job handle; the buffer and its size match.
        let ok = unsafe {
            QueryInformationJobObject(
                job.as_raw_handle() as HANDLE,
                JobObjectBasicAccountingInformation,
                &mut info as *mut JOBOBJECT_BASIC_ACCOUNTING_INFORMATION as *mut std::ffi::c_void,
                std::mem::size_of::<JOBOBJECT_BASIC_ACCOUNTING_INFORMATION>() as u32,
                std::ptr::null_mut(),
            )
        };
        if ok == 0 || info.ActiveProcesses == 0 {
            return true;
        }
        if Instant::now() >= deadline {
            return false;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

/// Puts back the label of every root that only sessions which ended without
/// releasing their lease (a crash) had labelled Low. Returns how many roots
/// it put back. Run at startup.
pub fn repair_stale_integrity_labels() -> Result<usize> {
    let Some(registry) = registry_dir() else {
        return Ok(0);
    };
    if std::fs::symlink_metadata(&registry).is_err() {
        return Ok(0);
    }
    let registry = ensure_registry()?;
    let lock = lock(&registry)?;
    let mut labels = load_labels(&registry)?;
    let restored = sweep(&registry, &mut labels, /*log_dir*/ None)?;
    drop(lock);
    Ok(restored)
}

/// Puts back the label of each recorded root no live session holds, and
/// stores the result. Returns how many it put back.
fn sweep(registry: &Path, labels: &mut Labels, log_dir: Option<&Path>) -> Result<usize> {
    let mut live = BTreeSet::new();
    for entry in
        std::fs::read_dir(registry).with_context(|| format!("list {}", registry.display()))?
    {
        let entry = entry?;
        let name = entry.file_name().to_string_lossy().into_owned();
        if !(name.starts_with(SESSION_PREFIX) && name.ends_with(SESSION_SUFFIX)) {
            continue;
        }
        let path = entry.path();
        let record = std::fs::read(&path)
            .ok()
            .and_then(|bytes| serde_json::from_slice::<SessionRecord>(&bytes).ok());
        // D262: record what the session saw appear before its file can go
        // (live or not). If that fails (a full disk, say), keep the file and
        // count the session as live: its folders that appeared still count as
        // planted, and a later sweep tries again.
        if let Some(record) = &record
            && record_planted(registry, &record.absent_metadata, log_dir).is_err()
        {
            live.extend(record.roots.iter().cloned());
            continue;
        }
        match std::fs::remove_file(&path) {
            // Its session ended without releasing the lease.
            Ok(()) => {}
            Err(err) => match record {
                Some(record) if err.raw_os_error() == Some(ERROR_SHARING_VIOLATION) => {
                    live.extend(record.roots);
                }
                // A session file that can't be read or probed may belong to
                // a live session: keep every label this time.
                _ => {
                    log_note(
                        &format!(
                            "integrity labels: cannot tell whether {} is live ({err}); no label is put back",
                            path.display()
                        ),
                        log_dir,
                    );
                    return Ok(0);
                }
            },
        }
    }
    let stale: Vec<PathBuf> = labels
        .roots
        .keys()
        .filter(|root| !live.contains(*root))
        .cloned()
        .collect();
    let mut restored = 0;
    for root in stale {
        let original = labels.roots[&root].clone();
        match restore(&root, &original) {
            Ok(()) => {
                labels.roots.remove(&root);
                restored += 1;
            }
            // Kept recorded; a later sweep tries again.
            Err(err) => log_note(
                &format!(
                    "integrity labels: could not put back the label of {}: {err:#}",
                    root.display()
                ),
                log_dir,
            ),
        }
    }
    save_labels(registry, labels)?;
    remove_unneeded_denies(registry, &live, log_dir);
    Ok(restored)
}

/// D262: the `.codex`/`.agents` folders at the top of `roots` that don't
/// exist (a link counts as existing).
fn absent_project_metadata(roots: &[PathBuf]) -> Vec<PathBuf> {
    roots
        .iter()
        .flat_map(|root| PROJECT_METADATA_NAMES.map(|name| root.join(name)))
        .filter(|path| {
            std::fs::symlink_metadata(path)
                .is_err_and(|err| err.kind() == std::io::ErrorKind::NotFound)
        })
        .collect()
}

/// D262: records each of `absent` (absent when a session started) that exists
/// now, whoever made it: a sandboxed command may have planted it. Called
/// with the registry locked.
fn record_planted(registry: &Path, absent: &[PathBuf], log_dir: Option<&Path>) -> Result<()> {
    let appeared: Vec<&PathBuf> = absent
        .iter()
        .filter(|path| std::fs::symlink_metadata(path).is_ok())
        .collect();
    if appeared.is_empty() {
        return Ok(());
    }
    let result = load_planted(registry).and_then(|mut planted| {
        let before = planted.entries.len();
        planted.entries.extend(
            appeared
                .iter()
                .filter(|path| !planted.confirmed.contains(**path))
                .map(|path| (*path).clone()),
        );
        if planted.entries.len() == before {
            return Ok(());
        }
        save_json(registry, PLANTED_FILE, &planted)
    });
    let paths = appeared
        .iter()
        .map(|path| path.display().to_string())
        .collect::<Vec<_>>()
        .join(", ");
    match &result {
        Ok(()) => log_note(
            &format!(
                "project metadata appeared while a sandboxed command ran; Core won't load it until confirmed: {paths}"
            ),
            log_dir,
        ),
        Err(err) => log_note(
            &format!("project metadata appeared ({paths}) but could not be recorded: {err:#}"),
            log_dir,
        ),
    }
    result
}

/// D262 (#1506 item 2): the `.codex`/`.agents` folders that appeared while a
/// legacy session ran and the user hasn't confirmed yet, including those a
/// session that is still running (a server it left behind, say) has seen
/// appear. Empty when there are no records.
pub fn planted_project_metadata() -> Result<Vec<PathBuf>> {
    let Some(registry) = registry_dir() else {
        return Ok(Vec::new());
    };
    match std::fs::symlink_metadata(&registry) {
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(err) => return Err(err).with_context(|| format!("inspect {}", registry.display())),
        Ok(_) => {}
    }
    // Session files first: a session that ends meanwhile records what it
    // saw before its file goes, so nothing falls between the two reads.
    let records = session_records(&registry)?;
    let PlantedMetadata {
        entries: mut planted,
        confirmed,
    } = load_planted(&registry)?;
    for record in records {
        planted.extend(
            record.absent_metadata.into_iter().filter(|path| {
                !confirmed.contains(path) && std::fs::symlink_metadata(path).is_ok()
            }),
        );
    }
    // A folder that is gone needs no confirmation.
    Ok(planted
        .into_iter()
        .filter(|path| std::fs::symlink_metadata(path).is_ok())
        .collect())
}

/// D262: the user checked `path` (one of [`planted_project_metadata`]) and
/// wants it loaded. Returns whether it was recorded.
///
/// Refused while a session that saw it appear still runs: that session's
/// processes could still change it after the user checked it.
pub fn confirm_planted_project_metadata(path: &Path) -> Result<bool> {
    let registry = ensure_registry()?;
    let lock = lock(&registry)?;
    // Records what ended sessions saw and drops their files, so only live
    // sessions' files remain.
    let mut labels = load_labels(&registry)?;
    sweep(&registry, &mut labels, /*log_dir*/ None)?;
    for record in session_records(&registry)? {
        if record.absent_metadata.iter().any(|absent| absent == path) {
            anyhow::bail!(
                "a non-admin sandbox command that may have created {} is still running; confirm it once that command has ended",
                path.display()
            );
        }
    }
    let mut planted = load_planted(&registry)?;
    let removed = planted.entries.remove(path);
    // Not recorded again by a sweep that finds a session file recording
    // failed for earlier.
    planted.confirmed.insert(path.to_path_buf());
    save_json(&registry, PLANTED_FILE, &planted)?;
    drop(lock);
    Ok(removed)
}

/// D262: drops records of folders that are gone, and forgets a confirmed
/// folder that is absent when a session starts, so it is reported if it
/// reappears.
fn prune_planted(registry: &Path, absent: &[PathBuf]) -> Result<()> {
    let mut planted = load_planted(registry)?;
    let before = (planted.entries.len(), planted.confirmed.len());
    planted
        .entries
        .retain(|path| std::fs::symlink_metadata(path).is_ok());
    planted
        .confirmed
        .retain(|path| !absent.contains(path) && std::fs::symlink_metadata(path).is_ok());
    if (planted.entries.len(), planted.confirmed.len()) != before {
        save_json(registry, PLANTED_FILE, &planted)?;
    }
    Ok(())
}

/// The records of the session files in `registry` that can be read (a live
/// session's file is shared for reading).
fn session_records(registry: &Path) -> Result<Vec<SessionRecord>> {
    let mut records = Vec::new();
    for entry in
        std::fs::read_dir(registry).with_context(|| format!("list {}", registry.display()))?
    {
        let name = entry?.file_name().to_string_lossy().into_owned();
        if !(name.starts_with(SESSION_PREFIX) && name.ends_with(SESSION_SUFFIX)) {
            continue;
        }
        if let Some(record) = std::fs::read(registry.join(&name))
            .ok()
            .and_then(|bytes| serde_json::from_slice::<SessionRecord>(&bytes).ok())
        {
            records.push(record);
        }
    }
    Ok(records)
}

fn load_planted(registry: &Path) -> Result<PlantedMetadata> {
    let path = registry.join(PLANTED_FILE);
    match std::fs::read(&path) {
        Ok(bytes) => serde_json::from_slice(&bytes)
            .with_context(|| format!("{} is damaged; remove it", path.display())),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(PlantedMetadata::default()),
        Err(err) => Err(err).with_context(|| format!("read {}", path.display())),
    }
}

/// #1506 item 1 (D261): removes each recorded deny that lies in no live
/// session's root. One that can't be removed stays recorded for a later
/// sweep; a path that is gone, or is a link now, is dropped untouched.
fn remove_unneeded_denies(registry: &Path, live: &BTreeSet<PathBuf>, log_dir: Option<&Path>) {
    let mut denies = match load_denies(registry) {
        Ok(denies) => denies,
        Err(err) => {
            log_note(&format!("delete-child denies: {err:#}"), log_dir);
            return;
        }
    };
    // SAFETY: builds the well-known Everyone SID.
    let Ok(mut everyone) = (unsafe { crate::token::world_sid() }) else {
        return;
    };
    let everyone = everyone.as_mut_ptr() as *mut std::ffi::c_void;
    let before = denies.entries.len();
    let live: Vec<String> = live.iter().map(|root| comparable_path(root)).collect();
    denies.entries.retain(|deny| {
        let path = comparable_path(&deny.path);
        if live.iter().any(|root| path_is_within(&path, root)) {
            return true;
        }
        match std::fs::symlink_metadata(&deny.path) {
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => return false,
            Ok(meta) if meta.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0 => {
                return false;
            }
            _ => {}
        }
        // Low: a Low-integrity sandbox may be using the folder right now, a
        // session of another Windows account (whose records these aren't),
        // say. Otherwise a Low-integrity command can't delete through it
        // anyway, so the deny isn't needed.
        // SAFETY: reads the label of an existing folder that is not a link.
        if !matches!(unsafe { effective_label_is_low(&deny.path) }, Ok(false)) {
            return true;
        }
        // SAFETY: `everyone` is a valid SID; the path is a folder, not a link.
        match unsafe { remove_deny_delete_child_ace(&deny.path, everyone, deny.inherit_to_subdirs) }
        {
            Ok(_) => false,
            Err(err) => {
                log_note(
                    &format!(
                        "delete-child denies: could not remove the Everyone deny on {}: {err:#}",
                        deny.path.display()
                    ),
                    log_dir,
                );
                true
            }
        }
    });
    if denies.entries.len() != before
        && let Err(err) = save_denies(registry, &denies)
    {
        log_note(&format!("delete-child denies: {err:#}"), log_dir);
    }
}

/// `path` as compared for liveness: without a `\\?\` prefix (which
/// `dunce` keeps on long paths) and, as Windows paths are, case-insensitive.
fn comparable_path(path: &Path) -> String {
    let path = path.to_string_lossy();
    let path = if let Some(rest) = path.strip_prefix(r"\\?\UNC\") {
        format!(r"\\{rest}")
    } else {
        path.strip_prefix(r"\\?\").unwrap_or(&path).to_string()
    };
    path.trim_end_matches('\\').to_ascii_lowercase()
}

/// Whether comparable `path` is `root` or lies below it.
fn path_is_within(path: &str, root: &str) -> bool {
    path == root
        || path
            .strip_prefix(root)
            .is_some_and(|rest| rest.starts_with('\\'))
}

/// #1506 item 1 (D261): adds each Everyone `FILE_DELETE_CHILD` deny in
/// `denies` that isn't there yet, recording it first, so a sweep removes it
/// once it lies in no live session's root, even after a crash. Links are
/// never followed. Without usable records the denies are added untracked,
/// as before, so protection never depends on the records.
///
/// With `required`, every deny must be in place afterwards (as an explicit
/// entry) or this fails, so the command doesn't start without it (D263's
/// denies above nested read-only paths); otherwise it is best effort, as
/// the `.git`/`.codex`/`.agents` denies always were.
pub(crate) fn add_tracked_delete_child_denies(
    denies: &[DeleteChildDeny],
    required: bool,
    log_dir: Option<&Path>,
) -> Result<()> {
    let mut usable = Vec::new();
    for deny in denies {
        let is_plain_dir = std::fs::symlink_metadata(&deny.path).is_ok_and(|meta| {
            meta.is_dir() && meta.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT == 0
        });
        match is_plain_dir
            .then(|| dunce::canonicalize(&deny.path).ok())
            .flatten()
        {
            Some(path) => usable.push(DeleteChildDeny {
                path,
                inherit_to_subdirs: deny.inherit_to_subdirs,
            }),
            None if required => anyhow::bail!(
                "the Windows sandbox could not protect {}: it is not a plain folder",
                deny.path.display()
            ),
            None => {}
        }
    }
    if usable.is_empty() {
        return Ok(());
    }
    // SAFETY: builds the well-known Everyone SID.
    let mut everyone = unsafe { crate::token::world_sid() }?;
    let everyone = everyone.as_mut_ptr() as *mut std::ffi::c_void;
    let tracked = ensure_registry().and_then(|registry| {
        let lock = lock(&registry)?;
        // SAFETY: `everyone` is a valid SID.
        let result = unsafe { add_tracked_locked(&registry, &usable, everyone) };
        drop(lock);
        result
    });
    if let Err(err) = tracked {
        log_note(
            &format!(
                "delete-child denies: some may not be recorded, and those stay after the session: {err:#}"
            ),
            log_dir,
        );
        for deny in &usable {
            // SAFETY: as above; the path is an existing folder.
            let _ =
                unsafe { add_deny_delete_child_ace(&deny.path, everyone, deny.inherit_to_subdirs) };
        }
    }
    if required {
        for deny in &usable {
            // SAFETY: as above.
            let present =
                unsafe { has_deny_delete_child_ace(&deny.path, everyone, deny.inherit_to_subdirs) };
            if !matches!(present, Ok(true)) {
                anyhow::bail!(
                    "the Windows sandbox could not add the Everyone delete-child deny on {}",
                    deny.path.display()
                );
            }
        }
    }
    Ok(())
}

/// # Safety
/// `everyone` must be a valid SID.
unsafe fn add_tracked_locked(
    registry: &Path,
    denies: &[DeleteChildDeny],
    everyone: *mut std::ffi::c_void,
) -> Result<()> {
    let mut records = load_denies(registry)?;
    let mut missing = Vec::new();
    for deny in denies {
        if !has_deny_delete_child_ace(&deny.path, everyone, deny.inherit_to_subdirs)? {
            missing.push(deny);
        }
    }
    if missing.is_empty() {
        return Ok(());
    }
    let before = records.entries.len();
    records
        .entries
        .extend(missing.iter().map(|deny| (*deny).clone()));
    // Recorded before they change, so a crash cannot lose them.
    if records.entries.len() != before {
        save_denies(registry, &records)?;
    }
    for deny in missing {
        if !add_deny_delete_child_ace(&deny.path, everyone, deny.inherit_to_subdirs)? {
            anyhow::bail!("could not add the Everyone deny on {}", deny.path.display());
        }
    }
    Ok(())
}

fn restore(root: &Path, original: &[LabelAce]) -> Result<()> {
    match std::fs::symlink_metadata(root) {
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(err) => Err(err).with_context(|| format!("inspect {}", root.display())),
        // A link now stands where the root was: never relabel what it points at.
        Ok(meta) if meta.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0 => Ok(()),
        // SAFETY: relabels an existing folder or file that is not a link.
        Ok(_) => unsafe { set_explicit_labels(root, original) },
    }
}

/// Creates the registry folders, or checks them: plain folders, not at Low
/// integrity (a Low-integrity program would have made them), with their
/// private Medium label.
fn ensure_registry() -> Result<PathBuf> {
    let parent = registry_parent_dir().context("no LocalAppData folder")?;
    let registry = parent.join(REGISTRY_DIR);
    for dir in [&parent, &registry] {
        match std::fs::symlink_metadata(dir) {
            Ok(meta) => {
                if !meta.is_dir() || meta.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
                    anyhow::bail!(
                        "{} is not a plain folder, so the Windows sandbox can't keep its integrity label records there; remove it",
                        dir.display()
                    );
                }
            }
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
                std::fs::create_dir(dir).with_context(|| format!("create {}", dir.display()))?;
            }
            Err(err) => return Err(err).with_context(|| format!("inspect {}", dir.display())),
        }
        // SAFETY: reads and sets the label of a plain folder of the host's.
        unsafe {
            if effective_label_is_low(dir)? {
                anyhow::bail!(
                    "{} has a Low integrity label, so a sandboxed command may have made it; remove it",
                    dir.display()
                );
            }
            label_private_dir_medium(dir)
                .with_context(|| format!("label {} Medium", dir.display()))?;
        }
    }
    Ok(registry)
}

fn lock(registry: &Path) -> Result<File> {
    let path = registry.join(LOCK_FILE);
    let deadline = Instant::now() + LOCK_WAIT;
    loop {
        match OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .share_mode(0)
            .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT)
            .open(&path)
        {
            Ok(file) => return Ok(file),
            Err(err)
                if err.raw_os_error() == Some(ERROR_SHARING_VIOLATION)
                    && Instant::now() < deadline =>
            {
                std::thread::sleep(Duration::from_millis(50));
            }
            Err(err) => return Err(err).with_context(|| format!("lock {}", path.display())),
        }
    }
}

fn create_session(
    registry: &Path,
    roots: &BTreeSet<PathBuf>,
    absent_metadata: &[PathBuf],
) -> Result<(File, PathBuf)> {
    let mut rng = SmallRng::from_entropy();
    let path = registry.join(format!(
        "{SESSION_PREFIX}{}-{:016x}{SESSION_SUFFIX}",
        std::process::id(),
        rng.r#gen::<u64>()
    ));
    // No delete sharing: while this handle is open, the file can't be
    // deleted, which is how sweeps tell the session is live.
    let mut file = OpenOptions::new()
        .read(true)
        .write(true)
        .create_new(true)
        .share_mode(FILE_SHARE_READ)
        .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT)
        .open(&path)
        .with_context(|| format!("create {}", path.display()))?;
    let record = SessionRecord {
        roots: roots.iter().cloned().collect(),
        absent_metadata: absent_metadata.to_vec(),
    };
    file.write_all(&serde_json::to_vec(&record)?)?;
    file.flush()?;
    Ok((file, path))
}

fn load_labels(registry: &Path) -> Result<Labels> {
    let path = registry.join(LABELS_FILE);
    match std::fs::read(&path) {
        Ok(bytes) => serde_json::from_slice(&bytes).with_context(|| {
            format!(
                "{} is damaged, so the Windows sandbox doesn't know which folders' integrity labels to put back; remove it, and give each writable root you used (%TEMP% included, and any nested root) a Medium label again (icacls <folder> /setintegritylevel (OI)(CI)M)",
                path.display()
            )
        }),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(Labels::default()),
        Err(err) => Err(err).with_context(|| format!("read {}", path.display())),
    }
}

fn save_labels(registry: &Path, labels: &Labels) -> Result<()> {
    let path = registry.join(LABELS_FILE);
    let tmp = registry.join(format!("{LABELS_FILE}.tmp"));
    // Flushed before it replaces the old records, so a power loss cannot
    // leave damaged records behind.
    let mut file = File::create(&tmp).with_context(|| format!("create {}", tmp.display()))?;
    file.write_all(&serde_json::to_vec_pretty(labels)?)
        .and_then(|()| file.sync_all())
        .with_context(|| format!("write {}", tmp.display()))?;
    drop(file);
    std::fs::rename(&tmp, &path).with_context(|| format!("replace {}", path.display()))
}

fn load_denies(registry: &Path) -> Result<DeleteChildDenies> {
    let path = registry.join(DENIES_FILE);
    match std::fs::read(&path) {
        Ok(bytes) => serde_json::from_slice(&bytes).with_context(|| {
            format!(
                "{} is damaged, so the Windows sandbox can't tell which Everyone delete-child denies it added; remove it (the denies stay; icacls <dir> /remove:d *S-1-1-0 removes them)",
                path.display()
            )
        }),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(DeleteChildDenies::default()),
        Err(err) => Err(err).with_context(|| format!("read {}", path.display())),
    }
}

fn save_denies(registry: &Path, denies: &DeleteChildDenies) -> Result<()> {
    save_json(registry, DENIES_FILE, denies)
}

/// Replaces `registry/name` with `value`, flushed first.
fn save_json(registry: &Path, name: &str, value: &impl Serialize) -> Result<()> {
    let path = registry.join(name);
    let tmp = registry.join(format!("{name}.tmp"));
    let mut file = File::create(&tmp).with_context(|| format!("create {}", tmp.display()))?;
    file.write_all(&serde_json::to_vec_pretty(value)?)
        .and_then(|()| file.sync_all())
        .with_context(|| format!("write {}", tmp.display()))?;
    drop(file);
    std::fs::rename(&tmp, &path).with_context(|| format!("replace {}", path.display()))
}

/// Tests only: the recorded denies.
#[cfg(test)]
pub(crate) fn recorded_delete_child_denies() -> Result<Vec<DeleteChildDeny>> {
    let registry = ensure_registry()?;
    let lock = lock(&registry)?;
    let denies = load_denies(&registry)?;
    drop(lock);
    Ok(denies.entries.into_iter().collect())
}

/// Tests only: the recorded roots.
#[cfg(test)]
pub(crate) fn recorded_roots() -> Result<Vec<PathBuf>> {
    let registry = ensure_registry()?;
    let lock = lock(&registry)?;
    let labels = load_labels(&registry)?;
    drop(lock);
    Ok(labels.roots.into_keys().collect())
}
