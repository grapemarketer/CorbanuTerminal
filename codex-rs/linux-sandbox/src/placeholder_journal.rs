//! #348 (owner decision D55, 2026-10-20): a record, kept under the Corbanu
//! home, of the placeholders each run made, so one left by a run that ended
//! without cleaning up is removed later even when the temporary folder's
//! registry (see `linux_run_main`) was lost with it (a reboot, a sweep of the
//! temporary folder, another `TMPDIR`).
//!
//! - Each run writes its own record file (`<pid>-<start>-<nanos>.rec`) by
//!   writing a temporary file, syncing it, renaming it into place and
//!   syncing the folder, so a crash leaves the old record or the new one.
//! - A placeholder is recorded as pending before it exists. The run then
//!   makes it itself (`O_EXCL`, so never over a file of someone else's) and
//!   records what it made: device, inode, mode, owner, size, modification and
//!   change times. bubblewrap finds it in place and mounts over it.
//! - Each run also records the paths it uses: its placeholders, and the
//!   paths it mounts at that another run's record lists as made or about
//!   to be made (or paths in them). A sweep leaves a placeholder alone while
//!   a live run (in any `TMPDIR`) uses it.
//! - At the start of each run that uses bubblewrap, the records of runs
//!   that have ended (another boot, or a pid that is gone or was reused)
//!   are swept: a recorded placeholder is removed only when what is at its path, reached
//!   without following a link, is still exactly what was recorded and no
//!   live run uses it. Anything else (filled, replaced, changed, or a pending
//!   one whose identity was never recorded) is left in place and reported
//!   once. Nothing but a recorded path is ever removed.

use std::collections::HashMap;
use std::collections::HashSet;
use std::ffi::CString;
use std::fs::File;
use std::io;
use std::io::Read;
use std::io::Write;
use std::os::fd::AsRawFd;
use std::os::fd::FromRawFd;
use std::os::fd::OwnedFd;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::MetadataExt;
use std::path::Component;
use std::path::Path;
use std::path::PathBuf;
use std::sync::Mutex;
use std::sync::OnceLock;
use std::time::Duration;
use std::time::SystemTime;

const HEADER: &str = "corbanu-sandbox-placeholders 1";
const RECORD_EXTENSION: &str = "rec";
const TEMPORARY_EXTENSION: &str = "tmp";
const LOCK_NAME: &str = "lock";
const REPORT_NAME: &str = "kept.log";
/// The report log stops growing at this size.
const REPORT_LIMIT: u64 = 1 << 20;
/// A temporary record file older than this is a crashed write.
const TEMPORARY_STALE_AFTER: Duration = Duration::from_secs(10 * 60);
/// The largest record file read.
const RECORD_LIMIT: u64 = 16 << 20;
/// Leave large cleanup backlogs for later sweeps rather than removing them
/// all while holding the record folder's lock.
const SWEEP_REMOVAL_LIMIT: usize = 64;
/// How long a run waits for the record folder's lock (a backstop: the lock
/// is a write lock, which a command that sees the folder read-only cannot
/// take). Then the run goes on without the record, as before (nothing
/// recorded, made or removed).
const LOCK_WAIT: Duration = if cfg!(test) {
    Duration::from_millis(300)
} else {
    Duration::from_secs(10)
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Kind {
    File,
    Directory,
}

impl Kind {
    fn name(self) -> &'static str {
        match self {
            Self::File => "file",
            Self::Directory => "directory",
        }
    }

    fn parse(name: &str) -> Option<Self> {
        match name {
            "file" => Some(Self::File),
            "directory" => Some(Self::Directory),
            _ => None,
        }
    }
}

/// What a run made, as `fstat` saw it right after making it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Identity {
    dev: u64,
    ino: u64,
    mode: u32,
    uid: u32,
    size: i64,
    mtime: (i64, i64),
    ctime: (i64, i64),
}

impl Identity {
    fn of(status: &libc::stat) -> Self {
        Self {
            dev: status.st_dev,
            ino: status.st_ino,
            mode: status.st_mode,
            uid: status.st_uid,
            size: status.st_size,
            mtime: (status.st_mtime, status.st_mtime_nsec),
            ctime: (status.st_ctime, status.st_ctime_nsec),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum Entry {
    /// A path the run uses (makes or holds) while it runs.
    Uses { path: PathBuf, real: PathBuf },
    /// A placeholder the run is about to make (or bubblewrap makes, where
    /// the run could not): never removed on the strength of the record.
    Pending {
        kind: Kind,
        path: PathBuf,
        real: PathBuf,
    },
    /// A placeholder the run made, as it was right after.
    Made {
        kind: Kind,
        path: PathBuf,
        real: PathBuf,
        identity: Identity,
    },
}

impl Entry {
    fn paths(&self) -> (&Path, &Path) {
        match self {
            Self::Uses { path, real }
            | Self::Pending { path, real, .. }
            | Self::Made { path, real, .. } => (path, real),
        }
    }
}

/// The run a record belongs to.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Owner {
    /// The machine (`/etc/machine-id`, else the host name): a home shared
    /// between machines holds records of runs this one cannot see.
    host: Option<String>,
    /// `/proc/sys/kernel/random/boot_id`.
    boot: Option<String>,
    pid: libc::pid_t,
    /// The process's start time (`/proc/<pid>/stat`, field 22), so a reused
    /// pid is not taken for the run.
    start: Option<u64>,
    /// The PID namespace (`/proc/self/ns/pid` inode) the pid is in.
    pidns: Option<u64>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct Record {
    owner: Owner,
    entries: Vec<Entry>,
}

/// A path a run registers (see [`register`]).
pub(crate) struct Planned<'a> {
    pub(crate) kind: Kind,
    /// Its path as the policy spells it.
    pub(crate) path: &'a Path,
    /// Where it is removed (its folder resolved when it was registered).
    pub(crate) real: &'a Path,
    /// Whether this run makes the placeholder (not another run's, nor a
    /// pre-existing path it holds).
    pub(crate) makes: bool,
}

/// Why a recorded placeholder was left in place.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Kept {
    Changed,
    NotEmpty,
    Unrecorded,
}

impl Kept {
    fn reason(self) -> &'static str {
        match self {
            Self::Changed => {
                "it is not the empty placeholder a sandboxed run made there (it was replaced or changed since)"
            }
            Self::NotEmpty => {
                "the placeholder folder a sandboxed run made there is no longer empty"
            }
            Self::Unrecorded => {
                "a sandboxed run that ended without cleaning up may have made it, but did not record what it made"
            }
        }
    }
}

/// This run's record and the folder it is in.
struct Journal {
    dir: OwnedFd,
    /// The folder's real location.
    path: PathBuf,
    name: String,
    record: Record,
    /// Whether the record file exists.
    written: bool,
    /// The folder's lock file, open for writing only: its write lock is
    /// this record's lock. Made `0200`, so a command that sees the folder
    /// (read-only) cannot open it at all, and so cannot take any lock on it.
    lock_file: File,
    /// Whether the lock is held across steps ([`hold`]).
    held: bool,
    /// Set when the lock could not be taken in time: from then on this run
    /// records nothing and removes nothing.
    unavailable: bool,
    /// Reads the mount points of this user's processes ([`mount_points`];
    /// replaced in tests).
    mount_points: Box<dyn Fn() -> Option<HashSet<Vec<u8>>> + Send>,
}

static JOURNAL: Mutex<Option<Journal>> = Mutex::new(None);

/// Opens (making it if needed, but never the Corbanu home above it) the
/// record folder `dir`. Without a usable folder (read-only, as in a nested
/// sandbox, or not this user's) nothing is recorded, as before.
pub(crate) fn open(dir: &Path) {
    let journal = open_journal(dir).ok();
    if let Ok(mut slot) = JOURNAL.lock() {
        *slot = journal;
    }
}

fn open_journal(dir: &Path) -> io::Result<Journal> {
    let (Some(home), Some(name)) = (dir.parent(), dir.file_name()) else {
        return Err(io::Error::from(io::ErrorKind::InvalidInput));
    };
    if !dir.is_absolute() {
        return Err(io::Error::from(io::ErrorKind::InvalidInput));
    }
    let home = open_raw(
        libc::AT_FDCWD,
        home.as_os_str().as_bytes(),
        libc::O_PATH | libc::O_DIRECTORY | libc::O_CLOEXEC,
        /*mode*/ 0,
    )?;
    let name = CString::new(name.as_bytes()).map_err(io::Error::other)?;
    // SAFETY: the folder descriptor is open and the name a C string.
    if unsafe { libc::mkdirat(home.as_raw_fd(), name.as_ptr(), 0o700) } < 0 {
        let err = io::Error::last_os_error();
        if err.kind() != io::ErrorKind::AlreadyExists {
            return Err(err);
        }
    }
    let dir = open_raw(
        home.as_raw_fd(),
        name.as_bytes(),
        libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
        /*mode*/ 0,
    )?;
    let status = fstat(&dir)?;
    // SAFETY: `geteuid` has no preconditions.
    let euid = unsafe { libc::geteuid() };
    if status.st_uid != euid {
        return Err(io::Error::from(io::ErrorKind::PermissionDenied));
    }
    // SAFETY: changes the mode of a folder this user owns.
    if status.st_mode & 0o077 != 0 && unsafe { libc::fchmod(dir.as_raw_fd(), 0o700) } < 0 {
        return Err(io::Error::last_os_error());
    }
    let path = std::fs::read_link(format!("/proc/self/fd/{}", dir.as_raw_fd()))?;
    // Without a lock file this run can write (a read-only home, as in a
    // nested sandbox), there is no record: as before.
    let lock_file = File::from(open_raw(
        dir.as_raw_fd(),
        LOCK_NAME.as_bytes(),
        libc::O_WRONLY | libc::O_CREAT | libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC,
        /*mode*/ 0o200,
    )?);
    let lock_status = lock_file.metadata()?;
    if !lock_status.is_file() || lock_status.uid() != euid {
        return Err(io::Error::from(io::ErrorKind::PermissionDenied));
    }
    if lock_status.mode() & 0o7777 != 0o200 {
        lock_file.set_permissions(std::os::unix::fs::PermissionsExt::from_mode(0o200))?;
    }
    let owner = own_owner();
    let nanos = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let name = format!("{}-{}-{nanos}", owner.pid, owner.start.unwrap_or(0));
    Ok(Journal {
        dir,
        path,
        name,
        record: Record {
            owner,
            entries: Vec::new(),
        },
        written: false,
        lock_file,
        held: false,
        unavailable: false,
        mount_points: Box::new(mount_points),
    })
}

/// Where the writable root bound at `mount_root` exposes the record folder,
/// when records are kept: the folder's path below it (found by device and
/// inode, so also through another path to the same folder), or the root
/// itself when it is inside the folder. bubblewrap binds that read-only, so
/// the command cannot forge a record a later run would act on.
pub(crate) fn record_dir_mount(mount_root: &Path) -> Option<PathBuf> {
    let mut dir = None;
    with_journal(|journal| dir = Some(journal.path.clone()));
    exposed_below(&dir?, mount_root)
}

/// Where `mount_root` exposes the folder `dir` (see [`record_dir_mount`]).
fn exposed_below(dir: &Path, mount_root: &Path) -> Option<PathBuf> {
    let identity = |path: &Path| {
        std::fs::metadata(path)
            .ok()
            .map(|metadata| (metadata.dev(), metadata.ino()))
    };
    let root = identity(mount_root)?;
    for above in dir.ancestors() {
        if identity(above) == Some(root) {
            return Some(mount_root.join(dir.strip_prefix(above).ok()?));
        }
    }
    let dir = identity(dir)?;
    mount_root
        .ancestors()
        .any(|above| identity(above) == Some(dir))
        .then(|| mount_root.to_path_buf())
}

/// Records the paths a run uses (`planned`, and `mounted`: each path as
/// spelled with its real location) and the placeholders it makes, then
/// makes those (where nothing is) and records what it made. The caller
/// holds the temporary folder's registry lock, so no run of that registry
/// removes them in between, and this folder's lock ([`hold`]), which keeps
/// out the sweeps of runs in other temporary folders.
pub(crate) fn register(planned: &[Planned<'_>], mounted: &[(PathBuf, PathBuf)]) {
    with_journal(|journal| journal.register(planned, mounted));
}

/// Takes this folder's lock until [`release`], from before a run looks at
/// the paths it mounts until it has recorded them. Whether it was taken.
pub(crate) fn hold() -> bool {
    let mut held = false;
    with_journal(|journal| {
        if !journal.held && journal.acquire().is_ok() {
            journal.held = true;
            held = true;
        }
    });
    held
}

pub(crate) fn release() {
    with_journal(|journal| {
        if journal.held {
            journal.held = false;
            let _ = set_lock(journal.lock_file.as_raw_fd(), libc::F_UNLCK);
        }
    });
}

/// The record's lock while taken for one step; released when dropped.
struct LockGuard(libc::c_int);

impl Drop for LockGuard {
    fn drop(&mut self) {
        let _ = set_lock(self.0, libc::F_UNLCK);
    }
}

/// Sets (`F_WRLCK`) or clears (`F_UNLCK`) the open file description's lock
/// on the whole of `fd`, without waiting. Whether it was done.
fn set_lock(fd: libc::c_int, kind: libc::c_int) -> io::Result<()> {
    // SAFETY: `flock` is plain data; the fields set describe the file.
    let mut whole: libc::flock = unsafe { std::mem::zeroed() };
    whole.l_type = kind as libc::c_short;
    whole.l_whence = libc::SEEK_SET as libc::c_short;
    // SAFETY: locks a descriptor the journal owns.
    if unsafe { libc::fcntl(fd, libc::F_OFD_SETLK, &whole) } < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

/// Once the command has ended, before this run's cleanup: it uses nothing
/// any more, so its paths no longer keep another run's cleanup from
/// removing a placeholder; only what it made stays recorded.
pub(crate) fn leave() {
    with_journal(Journal::leave);
}

/// After this run's cleanup: keeps the placeholders it made that are still
/// there (a live run holds them, or they changed since: #745), for a later
/// sweep, and removes the rest of its record.
pub(crate) fn finish() {
    with_journal(Journal::finish);
}

/// Removes the placeholders recorded by runs that have ended, where nothing
/// uses them: no live run's record, and no live run of this `TMPDIR`'s
/// registry (`used_by_registry`, given the path as spelled and its real
/// location).
pub(crate) fn sweep(used_by_registry: &dyn Fn(&Path, &Path) -> bool) {
    with_journal(|journal| journal.sweep(used_by_registry));
}

/// Whether another live run's record uses `path` (as spelled, or `real`,
/// its real location) or a path below it: a run of another temporary folder
/// mounts over it. The temporary folder's registry then leaves the
/// placeholder in place (this run's record keeps it, for a later sweep)
/// instead of removing it from under that run.
pub(crate) fn used_by_live_run(path: &Path, real: &Path) -> bool {
    let mut used = false;
    with_journal(|journal| used = journal.used_by_live_run(path, real));
    used
}

/// #745: whether another run's record names `path` (as spelled, or where it
/// is) as a placeholder it made or was about to make. A run holds such a
/// folder read-only instead of making placeholders of its own in it, which
/// would change it, so that its maker could no longer remove it.
pub(crate) fn recorded_by_another_run(path: &Path) -> bool {
    let real = crate::placeholder_registry::real_placeholder_path(path);
    let mut recorded = false;
    with_journal(|journal| recorded = journal.recorded_by_another_run(path, real.as_deref()));
    recorded
}

/// #607: whether a record (this run's or another's) names a placeholder made
/// at `real` (or `path`) and what is there now (`status`) is none of those:
/// the person replaced it, or (another run's, which must still be exactly
/// what was made) changed it. It is then not removed. `false` when no record
/// names one made there (or there is no record).
pub(crate) fn recorded_otherwise(path: &Path, real: &Path, status: &libc::stat) -> bool {
    let mut otherwise = false;
    with_journal(|journal| otherwise = journal.recorded_otherwise(path, real, status));
    otherwise
}

/// Whether `identity` is `found`: exactly (`exact`), or the same file or
/// folder (device, inode and type).
fn same_made(identity: &Identity, found: &libc::stat, exact: bool) -> bool {
    if exact {
        return *identity == Identity::of(found);
    }
    (identity.dev, identity.ino, identity.mode & libc::S_IFMT)
        == (found.st_dev, found.st_ino, found.st_mode & libc::S_IFMT)
}

/// The identity `record` gives the folder placeholder it made at `real`.
fn record_identity(record: &Record, real: &Path) -> Option<Identity> {
    record.entries.iter().find_map(|entry| match entry {
        Entry::Made {
            kind: Kind::Directory,
            real: made,
            identity,
            ..
        } if made == real => Some(*identity),
        _ => None,
    })
}

/// Whether `record` uses `path` (or `real`) or a path below it. Only its
/// uses count: a run that has left ([`leave`]) keeps what it made recorded,
/// but uses nothing.
fn uses(record: &Record, path: &Path, real: &Path) -> bool {
    record.entries.iter().any(|entry| {
        let Entry::Uses {
            path: used,
            real: used_real,
        } = entry
        else {
            return false;
        };
        [used, used_real]
            .iter()
            .any(|used| used.starts_with(path) || used.starts_with(real))
    })
}

fn with_journal(f: impl FnOnce(&mut Journal)) {
    if let Ok(mut slot) = JOURNAL.lock()
        && let Some(journal) = slot.as_mut()
    {
        f(journal);
    }
}

#[derive(Debug, PartialEq, Eq)]
enum Removal {
    /// With the folder it was in as it was just before and just after.
    Removed {
        folder_before: Option<Identity>,
        folder_after: Option<Identity>,
    },
    Missing,
    Kept(Kept),
    /// Not removed for another reason (no permission, or a process mounts
    /// over it): tried again later.
    Failed,
}

/// Removes the placeholder at `real` (reached without following a link)
/// when it is still exactly what was made and no process mounts over it
/// (by the mount points `read_mounts` reads now).
fn remove_if_made(
    path: &Path,
    real: &Path,
    kind: Kind,
    identity: &Identity,
    read_mounts: &dyn Fn() -> Option<HashSet<Vec<u8>>>,
) -> Removal {
    let still_made = || match crate::placeholder_registry::find_without_links(real) {
        Ok(Some(found)) if matches_made(&found, kind, identity) => Ok(found),
        Ok(Some(_)) => Err(Removal::Kept(Kept::Changed)),
        Ok(None) => Err(Removal::Missing),
        Err(_) => Err(Removal::Failed),
    };
    if let Err(removal) = still_made() {
        return removal;
    }
    // A live run that left no record (an older helper, a record it could
    // not write, another Corbanu home) may mount over it: read now.
    if mounted_anywhere(&[path, real], read_mounts()) {
        return Removal::Failed;
    }
    // Checked again last, right before the removal.
    let found = match still_made() {
        Ok(found) => found,
        Err(removal) => return removal,
    };
    let folder_before = found
        .folder_status()
        .ok()
        .map(|status| Identity::of(&status));
    match found.remove() {
        Ok(()) => Removal::Removed {
            folder_before,
            folder_after: found
                .folder_status()
                .ok()
                .map(|status| Identity::of(&status)),
        },
        Err(err) if err.kind() == io::ErrorKind::NotFound => Removal::Missing,
        Err(err) if matches!(err.raw_os_error(), Some(libc::ENOTEMPTY | libc::EEXIST)) => {
            Removal::Kept(Kept::NotEmpty)
        }
        Err(_) => Removal::Failed,
    }
}

/// Whether any of `paths` is a mount point in the mount namespace of any of
/// this user's processes (a sandbox's mounts are at the paths they have on
/// the host), by `points` as read now. When that cannot be told, as if it
/// were.
fn mounted_anywhere(paths: &[&Path], points: Option<HashSet<Vec<u8>>>) -> bool {
    points.as_ref().is_none_or(|points| {
        paths
            .iter()
            .any(|path| points.contains(path.as_os_str().as_bytes()))
    })
}

/// The mount points in the mount namespaces of this user's processes, or
/// `None` when they cannot all be read.
fn mount_points() -> Option<HashSet<Vec<u8>>> {
    mount_points_for_processes(
        std::fs::read_dir("/proc")
            .ok()?
            .flatten()
            .map(|process| process.path()),
    )
}

fn mount_points_for_processes(
    processes: impl Iterator<Item = PathBuf>,
) -> Option<HashSet<Vec<u8>>> {
    // SAFETY: `geteuid` has no preconditions.
    let euid = unsafe { libc::geteuid() };
    let mut seen = HashSet::new();
    let mut points = HashSet::new();
    for dir in processes {
        if !dir
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| name.bytes().all(|byte| byte.is_ascii_digit()))
        {
            continue;
        }
        if !std::fs::metadata(&dir).is_ok_and(|metadata| metadata.uid() == euid) {
            continue;
        }
        // mountinfo paths are relative to the process's root. Processes in
        // one mount namespace can have different roots (for example, chroot).
        // When either identity is unavailable, read the view without deduping.
        let view = view_identity(&dir);
        if view.as_ref().is_some_and(|view| seen.contains(view)) {
            continue;
        }
        let mounts = match std::fs::read(dir.join("mountinfo")) {
            Ok(mounts) => mounts,
            // The process has ended, or is ending (no mount namespace any
            // more: `EINVAL`, as for a zombie).
            Err(err)
                if err.kind() == io::ErrorKind::NotFound
                    || matches!(err.raw_os_error(), Some(libc::ESRCH | libc::EINVAL)) =>
            {
                continue;
            }
            Err(_) => return None,
        };
        // A view counts as seen only once it has been read, and only if the
        // process still has it (its pid was not reused by a process with
        // another view while it was read): a process that ends before its
        // view is read must not hide the same view of the others.
        if let Some(view) = view
            && view_identity(&dir).is_some_and(|now| now == view)
        {
            seen.insert(view);
        }
        points.extend(
            mounts
                .split(|byte| *byte == b'\n')
                .filter_map(|line| line.split(|byte| *byte == b' ').nth(4))
                .map(unescape_mount_point),
        );
    }
    Some(points)
}

/// The mount namespace and root of the process at `dir`, which together
/// decide what its `mountinfo` lists, or `None` when either is unavailable.
fn view_identity(dir: &Path) -> Option<(PathBuf, u64, u64)> {
    let namespace = std::fs::read_link(dir.join("ns/mnt")).ok()?;
    let root = std::fs::metadata(dir.join("root")).ok()?;
    Some((namespace, root.dev(), root.ino()))
}

/// A mount point as `mountinfo` writes it (space, tab, newline and
/// backslash as three octal digits after a backslash).
fn unescape_mount_point(point: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(point.len());
    let mut index = 0;
    while index < point.len() {
        if point[index] == b'\\'
            && let Some(code) = point
                .get(index + 1..index + 4)
                .and_then(|digits| std::str::from_utf8(digits).ok())
                .and_then(|digits| u8::from_str_radix(digits, 8).ok())
        {
            out.push(code);
            index += 4;
        } else {
            out.push(point[index]);
            index += 1;
        }
    }
    out
}

fn matches_made(
    found: &crate::placeholder_registry::Found,
    kind: Kind,
    identity: &Identity,
) -> bool {
    let kind_matches = match kind {
        Kind::File => found.is_empty_file(),
        Kind::Directory => found.is_dir(),
    };
    kind_matches && Identity::of(&found.status) == *identity
}

/// Makes the placeholder at `real` (its folder reached without following a
/// link), only where nothing is: `O_EXCL` for a file, `mkdirat` for a
/// folder, with bubblewrap's modes (`0444`, `0755`, less the umask).
fn make_placeholder(real: &Path, kind: Kind) -> io::Result<Identity> {
    let (folder, name) = crate::placeholder_registry::open_parent_without_links(real)?;
    match kind {
        Kind::File => {
            let file = open_raw(
                folder.as_raw_fd(),
                name.as_bytes(),
                libc::O_RDONLY | libc::O_CREAT | libc::O_EXCL | libc::O_NOFOLLOW | libc::O_CLOEXEC,
                /*mode*/ 0o444,
            )?;
            Ok(Identity::of(&fstat(&file)?))
        }
        Kind::Directory => {
            // SAFETY: the folder descriptor is open and the name a C string.
            if unsafe { libc::mkdirat(folder.as_raw_fd(), name.as_ptr(), 0o755) } < 0 {
                return Err(io::Error::last_os_error());
            }
            let made = open_raw(
                folder.as_raw_fd(),
                name.as_bytes(),
                libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
                /*mode*/ 0,
            )?;
            Ok(Identity::of(&fstat(&made)?))
        }
    }
}

/// Whether the run that wrote a record may still be running.
fn owner_is_live(owner: &Owner) -> bool {
    // A run on another machine (a home shared over the network), or one
    // whose machine is not known on either side, is taken as running: its
    // machine sweeps its records.
    match (&owner.host, own_host()) {
        (Some(recorded), Some(now)) if recorded == now => {}
        _ => return true,
    }
    if let (Some(recorded), Some(now)) = (&owner.boot, own_boot())
        && recorded != now
    {
        return false;
    }
    // A run in another PID namespace (a container sharing the home) is
    // taken as running: its pid names another process here, or none. So is
    // one whose namespace is not known on either side.
    match (
        owner.pidns,
        crate::placeholder_registry::own_pid_namespace(),
    ) {
        (Some(recorded), Some(now)) if recorded == now => {}
        _ => return true,
    }
    match process_start(owner.pid) {
        Ok(Some(start)) => owner.start.is_none_or(|recorded| recorded == start),
        Ok(None) => false,
        Err(_) => crate::placeholder_registry::process_is_active(owner.pid),
    }
}

/// Whether the run a record file is named for (`<pid>-<start>-<nanos>`)
/// may be running. A file named otherwise is no run's record: ignored.
fn named_run_is_live(stem: &str) -> bool {
    let parts: Vec<&str> = stem.split('-').collect();
    let [pid, start, nanos] = parts.as_slice() else {
        return false;
    };
    let (Ok(pid), Ok(start), Ok(_)) = (
        pid.parse::<libc::pid_t>(),
        start.parse::<u64>(),
        nanos.parse::<u128>(),
    ) else {
        return false;
    };
    match process_start(pid) {
        Ok(Some(now)) => start == 0 || now == start,
        Ok(None) => false,
        Err(_) => crate::placeholder_registry::process_is_active(pid),
    }
}

fn own_owner() -> Owner {
    let pid = std::process::id() as libc::pid_t;
    Owner {
        host: own_host().cloned(),
        boot: own_boot().cloned(),
        pid,
        start: process_start(pid).ok().flatten(),
        pidns: crate::placeholder_registry::own_pid_namespace(),
    }
}

fn own_host() -> Option<&'static String> {
    static HOST: OnceLock<Option<String>> = OnceLock::new();
    HOST.get_or_init(|| {
        ["/etc/machine-id", "/proc/sys/kernel/hostname"]
            .iter()
            .filter_map(|path| std::fs::read_to_string(path).ok())
            .map(|host| host.trim().to_string())
            .find(|host| is_word(host))
    })
    .as_ref()
}

fn own_boot() -> Option<&'static String> {
    static BOOT: OnceLock<Option<String>> = OnceLock::new();
    BOOT.get_or_init(|| {
        std::fs::read_to_string("/proc/sys/kernel/random/boot_id")
            .ok()
            .map(|boot| boot.trim().to_string())
            .filter(|boot| is_word(boot))
    })
    .as_ref()
}

/// The start time of process `pid` (`Ok(None)`: there is none). One that
/// has exited and not been reaped yet still counts, as for the temporary
/// folder's registry (`kill(pid, 0)`).
fn process_start(pid: libc::pid_t) -> io::Result<Option<u64>> {
    let stat = match std::fs::read_to_string(format!("/proc/{pid}/stat")) {
        Ok(stat) => stat,
        Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(err) => return Err(err),
    };
    // After the command name (in parentheses, which it may hold too), the
    // fields from the third on; the start time is the 22nd.
    let rest = stat
        .rfind(')')
        .map(|end| &stat[end + 1..])
        .ok_or_else(|| io::Error::from(io::ErrorKind::InvalidData))?;
    rest.split_whitespace()
        .nth(19)
        .and_then(|start| start.parse().ok())
        .map(Some)
        .ok_or_else(|| io::Error::from(io::ErrorKind::InvalidData))
}

impl Journal {
    fn register(&mut self, planned: &[Planned<'_>], mounted: &[(PathBuf, PathBuf)]) {
        if planned.is_empty() && mounted.is_empty() {
            return;
        }
        let Ok(_lock) = self.lock() else {
            return;
        };
        // Of the paths it mounts at, only another run's recorded placeholder
        // (or a path in one) is one a cleanup could remove from under it:
        // one it made, or was about to make (bubblewrap may have made it).
        let (records, unreadable_live) = self.other_records();
        let made: Vec<&Path> = records
            .iter()
            .flat_map(|(_, record)| record.entries.iter())
            .filter(|entry| matches!(entry, Entry::Made { .. } | Entry::Pending { .. }))
            .flat_map(|entry| {
                let (path, real) = entry.paths();
                [path, real]
            })
            .collect();
        let mounted: Vec<&(PathBuf, PathBuf)> = mounted
            .iter()
            .filter(|(path, real)| {
                unreadable_live
                    || made
                        .iter()
                        .any(|made| path.starts_with(made) || real.starts_with(made))
            })
            .collect();
        if planned.is_empty() && mounted.is_empty() {
            return;
        }
        for (path, real) in mounted {
            self.record.entries.push(Entry::Uses {
                path: path.clone(),
                real: real.clone(),
            });
        }
        for plan in planned {
            self.record.entries.push(Entry::Uses {
                path: plan.path.to_path_buf(),
                real: plan.real.to_path_buf(),
            });
            if plan.makes {
                self.record.entries.push(Entry::Pending {
                    kind: plan.kind,
                    path: plan.path.to_path_buf(),
                    real: plan.real.to_path_buf(),
                });
            }
        }
        // Recorded before anything is made: without the record nothing is made
        // here (bubblewrap makes it, as before).
        if self.write().is_err() {
            self.record
                .entries
                .retain(|entry| !matches!(entry, Entry::Pending { .. }));
            return;
        }
        let mut made_any = false;
        for entry in &mut self.record.entries {
            let Entry::Pending { kind, path, real } = entry else {
                continue;
            };
            if let Ok(identity) = make_placeholder(real, *kind) {
                *entry = Entry::Made {
                    kind: *kind,
                    path: std::mem::take(path),
                    real: std::mem::take(real),
                    identity,
                };
                made_any = true;
            }
        }
        if made_any {
            // A folder this run made changes when it makes a placeholder in
            // it too: recorded as it is once all are made.
            for entry in &mut self.record.entries {
                if let Entry::Made {
                    kind: Kind::Directory,
                    real,
                    identity,
                    ..
                } = entry
                    && let Ok(Some(found)) = crate::placeholder_registry::find_without_links(real)
                    && found.is_dir()
                    && found.identity() == (identity.dev, identity.ino)
                {
                    *identity = Identity::of(&found.status);
                }
            }
            // A failure here leaves them pending: reported, never removed, after
            // a crash; this run's own cleanup removes them as before.
            let _ = self.write();
        }
    }

    fn used_by_live_run(&mut self, path: &Path, real: &Path) -> bool {
        // Without the record, as if it were used: left for a later sweep.
        let Ok(_lock) = self.lock() else {
            return true;
        };
        let (records, unreadable_live) = self.other_records();
        unreadable_live
            || records
                .iter()
                .filter(|(_, record)| owner_is_live(&record.owner))
                .any(|(_, record)| uses(record, path, real))
    }

    fn recorded_by_another_run(&mut self, path: &Path, real: Option<&Path>) -> bool {
        let Ok(_lock) = self.lock() else {
            return false;
        };
        let (records, _) = self.other_records();
        records
            .iter()
            .flat_map(|(_, record)| record.entries.iter())
            .filter(|entry| matches!(entry, Entry::Made { .. } | Entry::Pending { .. }))
            .any(|entry| {
                let (made, made_real) = entry.paths();
                [made, made_real]
                    .iter()
                    .any(|made| *made == path || Some(*made) == real)
            })
    }

    fn recorded_otherwise(&mut self, path: &Path, real: &Path, status: &libc::stat) -> bool {
        let Ok(_lock) = self.lock() else {
            return false;
        };
        let (records, _) = self.other_records();
        // This run's own placeholder: the same file or folder; another
        // run's: exactly as made.
        let own = self.record.entries.iter().map(|entry| (entry, false));
        let others = records
            .iter()
            .flat_map(|(_, record)| record.entries.iter())
            .map(|entry| (entry, true));
        let mut named = false;
        for (entry, exact) in own.chain(others) {
            let Entry::Made {
                path: made,
                real: made_real,
                identity,
                ..
            } = entry
            else {
                continue;
            };
            if made_real != real && made != path {
                continue;
            }
            if same_made(identity, status, exact) {
                return false;
            }
            named = true;
        }
        named
    }

    /// Whether a record other than the one named `stem` made exactly what is
    /// at `real` now (a placeholder another run made there since).
    fn made_by_another_record(&self, stem: &str, real: &Path) -> bool {
        let Ok(Some(found)) = crate::placeholder_registry::find_without_links(real) else {
            return false;
        };
        let (records, _) = self.other_records();
        self.record
            .entries
            .iter()
            .chain(
                records
                    .iter()
                    .filter(|(name, _)| name != stem)
                    .flat_map(|(_, record)| record.entries.iter()),
            )
            .any(|entry| {
                matches!(entry, Entry::Made { real: made, identity, .. }
                    if made == real && same_made(identity, &found.status, /*exact*/ true))
            })
    }

    fn leave(&mut self) {
        if !self.written {
            self.record.entries.clear();
            return;
        }
        let Ok(_lock) = self.lock() else {
            return;
        };
        let before = self.record.entries.len();
        self.record
            .entries
            .retain(|entry| matches!(entry, Entry::Made { .. }));
        if self.record.entries.is_empty() {
            if self.remove_record().is_ok() {
                self.written = false;
            }
        } else if self.record.entries.len() != before {
            let _ = self.write();
        }
    }

    fn finish(&mut self) {
        if !self.written {
            self.record.entries.clear();
            return;
        }
        let Ok(_lock) = self.lock() else {
            return;
        };
        // #745: also one changed or replaced since (a folder that was not
        // empty when this run's cleanup came to it): a sweep reports it once
        // and leaves it, instead of it being forgotten here.
        self.record.entries.retain(|entry| match entry {
            // One that cannot be looked at now is kept too: dropping it
            // would leave only the temporary folder's looser rules for it.
            Entry::Made { real, .. } => !matches!(
                crate::placeholder_registry::find_without_links(real),
                Ok(None)
            ),
            Entry::Uses { .. } | Entry::Pending { .. } => false,
        });
        if self.record.entries.is_empty() {
            if self.remove_record().is_ok() {
                self.written = false;
            }
        } else {
            let _ = self.write();
        }
    }

    fn sweep(&mut self, used_by_registry: &dyn Fn(&Path, &Path) -> bool) {
        let Ok(_lock) = self.lock() else {
            return;
        };
        self.remove_stale_temporaries();
        let (records, unreadable_live) = self.other_records();
        // A live run's record this version cannot read (a newer version's)
        // may use any placeholder: nothing is removed while it runs.
        if unreadable_live {
            return;
        }
        let (live, ended): (Vec<_>, Vec<_>) = records
            .into_iter()
            .partition(|(_, record)| owner_is_live(&record.owner));
        // At it or below it (a live run's placeholder in a placeholder
        // folder).
        let in_use = |path: &Path, real: &Path| {
            live.iter().any(|(_, record)| uses(record, path, real)) || used_by_registry(path, real)
        };
        let mut removals_left = SWEEP_REMOVAL_LIMIT;
        // Once the mount points cannot be read, every later placeholder would
        // fail the same way, each after reading them all again while the lock
        // is held: the rest is left for a later sweep.
        let reader = &self.mount_points;
        let unreadable = std::cell::Cell::new(false);
        let read_mounts = || {
            let points = reader();
            if points.is_none() {
                unreadable.set(true);
            }
            points
        };
        for (stem, record) in ended {
            if removals_left == 0 || unreadable.get() {
                break;
            }
            let mut kept = Vec::new();
            // Deepest first, so a placeholder in a placeholder folder of the
            // same run goes before the folder; the folder is then expected
            // as that removal left it (`expected`), and as nothing else.
            let mut entries = record.entries.clone();
            entries.sort_by_key(|entry| std::cmp::Reverse(entry.paths().1.components().count()));
            let mut expected: HashMap<PathBuf, Identity> = HashMap::new();
            for mut entry in entries {
                if matches!(entry, Entry::Uses { .. }) {
                    continue;
                }
                if removals_left == 0 || unreadable.get() {
                    // A child removed in this sweep can change a deferred
                    // parent. Keep only the identity change already checked
                    // below, so the next sweep can still check it exactly.
                    if let Entry::Made { real, identity, .. } = &mut entry
                        && let Some(after) = expected.get(real)
                    {
                        *identity = *after;
                    }
                    kept.push(entry);
                    continue;
                }
                let (path, real) = entry.paths();
                if in_use(path, real) {
                    kept.push(entry);
                    continue;
                }
                match &entry {
                    Entry::Uses { .. } => {}
                    // Its identity was never recorded: never removed.
                    Entry::Pending { real, .. } => {
                        if matches!(
                            crate::placeholder_registry::find_without_links(real),
                            Ok(Some(_))
                        ) {
                            self.report(real, Kept::Unrecorded);
                        }
                    }
                    Entry::Made {
                        kind,
                        path,
                        real,
                        identity,
                    } => {
                        let identity = expected.get(real).unwrap_or(identity);
                        match remove_if_made(path, real, *kind, identity, &read_mounts) {
                            Removal::Removed {
                                folder_before,
                                folder_after,
                            } => {
                                removals_left -= 1;
                                // Only the change an unlink makes: times and
                                // size, never its mode or owner (a chmod or
                                // chown in between). Both readings come from
                                // the same open folder, so it is the same
                                // folder either way.
                                if let (Some(folder), Some(before), Some(after)) =
                                    (real.parent(), folder_before, folder_after)
                                    && let Some(recorded) = record_identity(&record, folder)
                                    && before == *expected.get(folder).unwrap_or(&recorded)
                                    && (after.dev, after.ino, after.mode, after.uid)
                                        == (before.dev, before.ino, before.mode, before.uid)
                                {
                                    expected.insert(folder.to_path_buf(), after);
                                }
                            }
                            Removal::Missing => {}
                            // #745: not when another run made what is there
                            // now (its own record has it).
                            Removal::Kept(Kept::Changed)
                                if self.made_by_another_record(&stem, real) => {}
                            Removal::Kept(why) => self.report(real, why),
                            // Kept as the identity checked now, as when
                            // deferred above.
                            Removal::Failed => kept.push(Entry::Made {
                                kind: *kind,
                                path: path.clone(),
                                real: real.clone(),
                                identity: *identity,
                            }),
                        }
                    }
                }
            }
            if kept.is_empty() {
                let _ = self.unlink(&format!("{stem}.{RECORD_EXTENSION}"));
            } else if kept.len() != record.entries.len() {
                let _ = self.write_named(
                    &stem,
                    &Record {
                        owner: record.owner,
                        entries: kept,
                    },
                );
            }
        }
    }

    /// The record folder's lock, held while records are read or written:
    /// `None` when it is held already ([`hold`]; releasing it here would
    /// release that too).
    fn lock(&mut self) -> io::Result<Option<LockGuard>> {
        if self.held {
            return Ok(None);
        }
        self.acquire()?;
        Ok(Some(LockGuard(self.lock_file.as_raw_fd())))
    }

    /// Takes the write lock, waiting at most [`LOCK_WAIT`]; after that, the
    /// record is unavailable for the rest of this run.
    fn acquire(&mut self) -> io::Result<()> {
        if self.unavailable {
            return Err(io::Error::from(io::ErrorKind::WouldBlock));
        }
        let deadline = std::time::Instant::now() + LOCK_WAIT;
        loop {
            let err = match set_lock(self.lock_file.as_raw_fd(), libc::F_WRLCK) {
                Ok(()) => return Ok(()),
                Err(err) => err,
            };
            if !matches!(
                err.raw_os_error(),
                Some(libc::EAGAIN | libc::EACCES | libc::EINTR)
            ) || std::time::Instant::now() >= deadline
            {
                self.unavailable = true;
                return Err(err);
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    /// The other runs' records (by name, without the extension) that this
    /// version wrote and no other user may have written, and whether there
    /// is one it cannot read whose run (named in the file name) is running.
    /// Without a listing, as if there were.
    fn other_records(&self) -> (Vec<(String, Record)>, bool) {
        let Ok(names) = self.list() else {
            return (Vec::new(), true);
        };
        let mut records = Vec::new();
        let mut unreadable_live = false;
        for name in &names {
            let Some(stem) = name.strip_suffix(&format!(".{RECORD_EXTENSION}")) else {
                continue;
            };
            if stem == self.name {
                continue;
            }
            match self.read(name) {
                Some(record) => records.push((stem.to_string(), record)),
                None => unreadable_live |= named_run_is_live(stem),
            }
        }
        (records, unreadable_live)
    }

    /// Removes the temporary record files left by crashed writes.
    fn remove_stale_temporaries(&self) {
        let Ok(names) = self.list() else {
            return;
        };
        for name in names {
            if let Some(stem) = name.strip_suffix(&format!(".{TEMPORARY_EXTENSION}"))
                && stem != self.name
            {
                self.remove_stale_temporary(&name);
            }
        }
    }

    fn list(&self) -> io::Result<Vec<String>> {
        let mut names = Vec::new();
        for entry in std::fs::read_dir(format!("/proc/self/fd/{}", self.dir.as_raw_fd()))? {
            if let Some(name) = entry?.file_name().to_str() {
                names.push(name.to_string());
            }
        }
        Ok(names)
    }

    fn write(&mut self) -> io::Result<()> {
        let name = self.name.clone();
        let record = self.record.clone();
        self.write_named(&name, &record)?;
        self.written = true;
        Ok(())
    }

    /// Writes a record crash-safely: a temporary file, synced, renamed into
    /// place, then the folder synced.
    fn write_named(&self, stem: &str, record: &Record) -> io::Result<()> {
        let temporary = format!("{stem}.{TEMPORARY_EXTENSION}");
        match self.unlink(&temporary) {
            Ok(()) => {}
            Err(err) if err.kind() == io::ErrorKind::NotFound => {}
            Err(err) => return Err(err),
        }
        let mut file = File::from(open_raw(
            self.dir.as_raw_fd(),
            temporary.as_bytes(),
            libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL | libc::O_NOFOLLOW | libc::O_CLOEXEC,
            /*mode*/ 0o600,
        )?);
        file.write_all(serialize(record).as_bytes())?;
        file.sync_all()?;
        drop(file);
        let from = CString::new(temporary).map_err(io::Error::other)?;
        let to = CString::new(format!("{stem}.{RECORD_EXTENSION}")).map_err(io::Error::other)?;
        // SAFETY: both names are C strings in this open folder.
        if unsafe {
            libc::renameat(
                self.dir.as_raw_fd(),
                from.as_ptr(),
                self.dir.as_raw_fd(),
                to.as_ptr(),
            )
        } < 0
        {
            return Err(io::Error::last_os_error());
        }
        self.sync()
    }

    fn remove_record(&self) -> io::Result<()> {
        match self.unlink(&format!("{}.{RECORD_EXTENSION}", self.name)) {
            Ok(()) => {}
            Err(err) if err.kind() == io::ErrorKind::NotFound => {}
            Err(err) => return Err(err),
        }
        self.sync()
    }

    fn sync(&self) -> io::Result<()> {
        // SAFETY: syncs a descriptor owned here.
        if unsafe { libc::fsync(self.dir.as_raw_fd()) } < 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }

    fn unlink(&self, name: &str) -> io::Result<()> {
        let name = CString::new(name).map_err(io::Error::other)?;
        // SAFETY: the name is a C string in this open folder.
        if unsafe { libc::unlinkat(self.dir.as_raw_fd(), name.as_ptr(), 0) } < 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }

    /// A record of this user's that no one else may write, or `None`.
    fn read(&self, name: &str) -> Option<Record> {
        let file = File::from(
            open_raw(
                self.dir.as_raw_fd(),
                name.as_bytes(),
                libc::O_RDONLY | libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC,
                /*mode*/ 0,
            )
            .ok()?,
        );
        let metadata = file.metadata().ok()?;
        // SAFETY: `geteuid` has no preconditions.
        let euid = unsafe { libc::geteuid() };
        if !metadata.is_file() || metadata.uid() != euid || metadata.mode() & 0o022 != 0 {
            return None;
        }
        let mut text = String::new();
        file.take(RECORD_LIMIT).read_to_string(&mut text).ok()?;
        parse(&text)
    }

    /// Removes a temporary record file left by a crashed write.
    fn remove_stale_temporary(&self, name: &str) {
        let Ok(file) = open_raw(
            self.dir.as_raw_fd(),
            name.as_bytes(),
            libc::O_PATH | libc::O_NOFOLLOW | libc::O_CLOEXEC,
            /*mode*/ 0,
        ) else {
            return;
        };
        let Ok(status) = fstat(&file) else {
            return;
        };
        let modified = SystemTime::UNIX_EPOCH
            + Duration::from_secs(u64::try_from(status.st_mtime).unwrap_or(0));
        if modified
            .elapsed()
            .is_ok_and(|elapsed| elapsed > TEMPORARY_STALE_AFTER)
        {
            let _ = self.unlink(name);
        }
    }

    /// Says once (on standard error, and in `kept.log` in the record folder)
    /// that a recorded placeholder was left in place.
    fn report(&self, path: &Path, why: Kept) {
        eprintln!(
            "corbanu sandbox: left {} in place: {}.",
            path.display(),
            why.reason()
        );
        let Ok(log) = open_raw(
            self.dir.as_raw_fd(),
            REPORT_NAME.as_bytes(),
            libc::O_WRONLY | libc::O_CREAT | libc::O_APPEND | libc::O_NOFOLLOW | libc::O_CLOEXEC,
            /*mode*/ 0o600,
        ) else {
            return;
        };
        let mut log = File::from(log);
        if log
            .metadata()
            .is_ok_and(|metadata| metadata.is_file() && metadata.len() < REPORT_LIMIT)
        {
            let now = SystemTime::now()
                .duration_since(SystemTime::UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs();
            let _ = writeln!(log, "{now} {}: {}", path.display(), why.reason());
        }
    }
}

fn open_raw(
    folder: libc::c_int,
    name: &[u8],
    flags: libc::c_int,
    mode: libc::mode_t,
) -> io::Result<OwnedFd> {
    let name = CString::new(name).map_err(io::Error::other)?;
    // SAFETY: `name` is a C string; the result is checked below.
    let fd = unsafe { libc::openat(folder, name.as_ptr(), flags, libc::c_uint::from(mode)) };
    if fd < 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: `fd` was just opened and is owned here.
    Ok(unsafe { OwnedFd::from_raw_fd(fd) })
}

fn fstat(fd: &OwnedFd) -> io::Result<libc::stat> {
    // SAFETY: `stat` is plain data, filled in by `fstat` below.
    let mut status: libc::stat = unsafe { std::mem::zeroed() };
    // SAFETY: the descriptor is open and `status` writable.
    if unsafe { libc::fstat(fd.as_raw_fd(), &mut status) } < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(status)
}

fn is_word(text: &str) -> bool {
    !text.is_empty() && !text.contains(char::is_whitespace)
}

fn hex(path: &Path) -> String {
    path.as_os_str()
        .as_bytes()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

fn unhex(text: &str) -> Option<PathBuf> {
    if !text.len().is_multiple_of(2) {
        return None;
    }
    let bytes = (0..text.len())
        .step_by(2)
        .map(|index| u8::from_str_radix(text.get(index..index + 2)?, 16).ok())
        .collect::<Option<Vec<u8>>>()?;
    let path = PathBuf::from(std::ffi::OsStr::from_bytes(&bytes));
    // Absolute and normal: no `.`, `..` or empty components.
    let normal = path.is_absolute()
        && path
            .components()
            .all(|component| matches!(component, Component::RootDir | Component::Normal(_)))
        && path.file_name().is_some();
    normal.then_some(path)
}

fn optional<T: std::fmt::Display>(value: Option<T>) -> String {
    value.map_or_else(|| "-".to_string(), |value| value.to_string())
}

fn serialize(record: &Record) -> String {
    let owner = &record.owner;
    let mut text = format!(
        "{HEADER}\nhost {}\nboot {}\npid {}\nstart {}\npidns {}\n",
        optional(owner.host.as_deref()),
        optional(owner.boot.as_deref()),
        owner.pid,
        optional(owner.start),
        optional(owner.pidns),
    );
    for entry in &record.entries {
        let line = match entry {
            Entry::Uses { path, real } => format!("uses {} {}", hex(path), hex(real)),
            Entry::Pending { kind, path, real } => {
                format!("pending {} {} {}", kind.name(), hex(path), hex(real))
            }
            Entry::Made {
                kind,
                path,
                real,
                identity,
            } => format!(
                "made {} {} {} {} {} {} {} {} {} {} {} {}",
                kind.name(),
                hex(path),
                hex(real),
                identity.dev,
                identity.ino,
                identity.mode,
                identity.uid,
                identity.size,
                identity.mtime.0,
                identity.mtime.1,
                identity.ctime.0,
                identity.ctime.1,
            ),
        };
        text.push_str(&line);
        text.push('\n');
    }
    text.push_str("end\n");
    text
}

/// A record, or `None` when any of it is not as this version writes it:
/// such a record is never acted on.
fn parse(text: &str) -> Option<Record> {
    let mut lines = text.lines();
    if lines.next()? != HEADER {
        return None;
    }
    let mut field = |name: &str| -> Option<Option<String>> {
        let (key, value) = lines.next()?.split_once(' ')?;
        (key == name && is_word(value)).then(|| (value != "-").then(|| value.to_string()))
    };
    let host = field("host")?;
    let boot = field("boot")?;
    let pid = field("pid")??.parse().ok()?;
    let start = match field("start")? {
        Some(start) => Some(start.parse().ok()?),
        None => None,
    };
    let pidns = match field("pidns")? {
        Some(pidns) => Some(pidns.parse().ok()?),
        None => None,
    };
    let mut entries = Vec::new();
    let mut ended = false;
    for line in lines {
        if ended {
            return None;
        }
        let words: Vec<&str> = line.split(' ').collect();
        let entry = match words.as_slice() {
            ["end"] => {
                ended = true;
                continue;
            }
            ["uses", path, real] => Entry::Uses {
                path: unhex(path)?,
                real: unhex(real)?,
            },
            ["pending", kind, path, real] => Entry::Pending {
                kind: Kind::parse(kind)?,
                path: unhex(path)?,
                real: unhex(real)?,
            },
            [
                "made",
                kind,
                path,
                real,
                dev,
                ino,
                mode,
                uid,
                size,
                mtime,
                mtime_nsec,
                ctime,
                ctime_nsec,
            ] => Entry::Made {
                kind: Kind::parse(kind)?,
                path: unhex(path)?,
                real: unhex(real)?,
                identity: Identity {
                    dev: dev.parse().ok()?,
                    ino: ino.parse().ok()?,
                    mode: mode.parse().ok()?,
                    uid: uid.parse().ok()?,
                    size: size.parse().ok()?,
                    mtime: (mtime.parse().ok()?, mtime_nsec.parse().ok()?),
                    ctime: (ctime.parse().ok()?, ctime_nsec.parse().ok()?),
                },
            },
            _ => return None,
        };
        let (path, real) = entry.paths();
        if path.file_name() != real.file_name() {
            return None;
        }
        entries.push(entry);
    }
    ended.then_some(Record {
        owner: Owner {
            host,
            boot,
            pid,
            start,
            pidns,
        },
        entries,
    })
}

#[cfg(test)]
#[path = "placeholder_journal_tests.rs"]
mod tests;
