use codex_protocol::ThreadId;
use std::collections::HashMap;
use std::collections::HashSet;
use std::collections::VecDeque;
use std::collections::hash_map;
use std::ffi::OsStr;
use std::ffi::OsString;
use std::io::Read;
use std::os::unix::fs::DirEntryExt;
use std::os::unix::fs::MetadataExt;
use std::os::unix::fs::OpenOptionsExt;
use std::path::Path;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::LazyLock;
use std::sync::Mutex;
use std::sync::PoisonError;
use std::time::SystemTime;
use std::time::UNIX_EPOCH;

/// Entries looked at per repository at most; past it the look is
/// incomplete (and holds, see `held_for`).
pub(super) const MAX_ENTRIES: usize = 50_000;
const MAX_DEPTH: usize = 16;
pub(super) const MAX_REPOS: usize = 8;
const MAX_REPOS_PER_THREAD: usize = 16;
const MAX_THREADS: usize = 64;
/// New, empty config files and hooks folders remembered (`Global::empty_new`).
const MAX_EMPTY_NEW: usize = 1024;

#[cfg(test)]
thread_local! {
    /// A test's own `MAX_EMPTY_NEW` (its thread only), so that the limit can
    /// be reached without filling the process-wide set other tests use.
    pub(super) static TEST_MAX_EMPTY_NEW: std::cell::Cell<Option<usize>> =
        const { std::cell::Cell::new(None) };
}

fn max_empty_new() -> usize {
    #[cfg(test)]
    if let Some(max) = TEST_MAX_EMPTY_NEW.with(std::cell::Cell::get) {
        return max;
    }
    MAX_EMPTY_NEW
}
/// `.git` folders reported as swaps remembered (`Global::swapped`).
pub(super) const MAX_SWAPPED: usize = 1024;
const SHOWN_FINDINGS: usize = 20;
/// Links followed per chain, like the kernel's limit.
const MAX_LINK_HOPS: usize = 40;
const MAX_TODO_BYTES: u64 = 4 << 20;
/// A `.git` file or `commondir` longer than this is not read.
const SMALL_FILE_BYTES: u64 = 64 << 10;
/// A folder or todo whose status changed this recently is read again next
/// time: a change in the same clock tick would not move its change time.
const RACY_NANOS: i128 = 2_000_000_000;
/// Stores below a git folder. Only their own entries are looked at (and
/// `objects/info`, `objects/pack`): git reads the objects below and puts new
/// ones in place by renaming or linking, it does not write through them.
const STORES: [&str; 3] = ["objects", "lfs", "annex"];
const OBJECT_FOLDERS_LOOKED_AT: [&str; 2] = ["info", "pack"];
/// What a later `git rebase --continue` (or `cherry-pick`/`revert
/// --continue`) runs, in each git folder: the steps, and the merge strategy
/// (git runs `git merge-<strategy>`, as a path where the name holds a `/`).
/// #943 item 5: git's other rebase and sequencer state names no program:
/// `gpg_sign_opt` and `options.gpg-sign` only pick the signing key (the
/// program comes from config, which is protected), `strategy_opts` and
/// `options.strategy-option` are options to the merge strategy, and
/// `author-script` is parsed, not run, by the C sequencer (git 2.34 and
/// later; older scripted backends sourced it).
const STATE_FILES: [&str; 5] = [
    "rebase-merge/git-rebase-todo",
    "rebase-apply/git-rebase-todo",
    "sequencer/todo",
    "rebase-merge/strategy",
    "sequencer/opts",
];
const BUILTIN_STRATEGIES: [&str; 6] = ["ort", "recursive", "resolve", "octopus", "ours", "subtree"];
/// Entries of a repository's own git folder that git reads for config or
/// hooks: a new one is reported (on Linux a missing one isn't held
/// read-only) once it isn't empty, as is anything new below `hooks`.
const CONFIG_NAMES: [&str; 3] = ["config", "config.worktree", "commondir"];
/// Legacy remote definitions git still reads for a remote with no URL in
/// its config.
const LEGACY_REMOTES: [&str; 2] = ["remotes", "branches"];
const SHOWN_EXEC_LINES: usize = 3;
const SHOWN_CHARS: usize = 160;

/// A project's repository: the `.git` git finds for it and its git folders.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(crate) struct Repo {
    /// As found (a folder, a `gitdir:` file or a link).
    dot_git: PathBuf,
    /// Real paths: the folder, or a `gitdir:` file's folder and its common
    /// folder.
    git_dirs: Vec<PathBuf>,
}

impl Repo {
    /// Paths that, where an agent command may write them, mean it may write
    /// this repository's git folder.
    pub(crate) fn probes(&self) -> Vec<PathBuf> {
        let mut probes = vec![self.dot_git.join("COMMIT_EDITMSG")];
        for dir in &self.git_dirs {
            for name in [
                "COMMIT_EDITMSG",
                "logs/HEAD",
                STATE_FILES[0],
                STATE_FILES[2],
            ] {
                probes.push(dir.join(name));
            }
        }
        probes
    }
}

/// [`repos`] of `folders`, and beyond the cap the repository of `also`
/// (#943 item 11: the folder a command runs in, its `workdir`).
pub(crate) fn repos_and(folders: &[&Path], also: Option<&Path>) -> Vec<Repo> {
    let mut found = repos(folders);
    for repo in also.map_or_else(Vec::new, |also| repos(&[also])) {
        if !found.iter().any(|known| known.dot_git == repo.dot_git) {
            found.push(repo);
        }
    }
    found
}

/// The repositories `folders` are in (the nearest `.git` at or above each,
/// as git finds it).
pub(crate) fn repos(folders: &[&Path]) -> Vec<Repo> {
    let mut repos: Vec<Repo> = Vec::new();
    for folder in folders {
        let Some(dot_git) = folder
            .ancestors()
            .map(|folder| folder.join(".git"))
            .find(|dot_git| std::fs::symlink_metadata(dot_git).is_ok())
        else {
            continue;
        };
        if repos.iter().any(|repo| repo.dot_git == dot_git) {
            continue;
        }
        if let Some(repo) = repo_at(dot_git) {
            repos.push(repo);
        }
        if repos.len() >= MAX_REPOS {
            break;
        }
    }
    repos
}

fn repo_at(dot_git: PathBuf) -> Option<Repo> {
    let meta = std::fs::metadata(&dot_git).ok()?;
    let mut git_dirs = Vec::new();
    if meta.is_dir() {
        git_dirs.push(std::fs::canonicalize(&dot_git).ok()?);
    } else {
        let text = read_small(&dot_git)?;
        let work_tree = dot_git.parent()?;
        let dir = text.lines().find_map(|line| line.strip_prefix("gitdir:"))?;
        let git_dir = std::fs::canonicalize(work_tree.join(dir.trim())).ok()?;
        if let Some(common) = read_small(&git_dir.join("commondir"))
            .and_then(|common| std::fs::canonicalize(git_dir.join(common.trim())).ok())
        {
            git_dirs.push(common);
        }
        git_dirs.push(git_dir);
    }
    git_dirs.dedup();
    Some(Repo { dot_git, git_dirs })
}

/// Reads a small regular file without blocking on a FIFO or device.
fn read_small(path: &Path) -> Option<String> {
    let mut bytes = Vec::new();
    let complete = read_regular(path, SMALL_FILE_BYTES, &mut bytes)?;
    complete.then(|| String::from_utf8_lossy(&bytes).into_owned())
}

/// Reads up to `max` bytes of the regular file at `path` (followed) into
/// `bytes`; whether that is all of it. `None` if it is not a readable
/// regular file.
fn read_regular(path: &Path, max: u64, bytes: &mut Vec<u8>) -> Option<bool> {
    let file = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NONBLOCK | libc::O_NOCTTY)
        .open(path)
        .ok()?;
    let meta = file.metadata().ok()?;
    if !meta.is_file() {
        return None;
    }
    file.take(max).read_to_end(bytes).ok()?;
    Some(meta.len() <= max)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Kind {
    Dir,
    File,
    Link,
    Other,
}

impl Kind {
    fn of(file_type: std::fs::FileType) -> Self {
        if file_type.is_symlink() {
            Kind::Link
        } else if file_type.is_dir() {
            Kind::Dir
        } else if file_type.is_file() {
            Kind::File
        } else {
            Kind::Other
        }
    }
}

/// Modification and status-change times. A command can set the first back,
/// not the second.
fn times(meta: &std::fs::Metadata) -> (i128, i128) {
    (
        i128::from(meta.mtime()) * 1_000_000_000 + i128::from(meta.mtime_nsec()),
        i128::from(meta.ctime()) * 1_000_000_000 + i128::from(meta.ctime_nsec()),
    )
}

/// When it was made, where the file system records it (#943 item 12: a
/// freed inode number can come back for the next file or folder, on Linux
/// at once; the creation time tells them apart). Not on macOS, which doesn't
/// hand out a freed one at once and moves a file's creation time back with
/// its modification time (`touch -t`, `rsync -t`). Not on an overlay file
/// system either (#943 follow-up 19): git's first write in a container
/// copies the folder up from the image's layer, with a new creation time
/// and the same inode number.
pub(super) fn born(path: &Path, meta: &std::fs::Metadata) -> Option<i128> {
    if cfg!(target_os = "macos") || on_overlay(path, meta) {
        return None;
    }
    meta.created()
        .ok()
        .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
        .map(|since| since.as_nanos() as i128)
}

/// Devices seen not to be overlay file systems, checked once each. Only
/// that answer is remembered: a device number freed by an unmount can come
/// back for another file system, and a stale "not overlay" only compares
/// creation times where they would have been ignored.
#[cfg(target_os = "linux")]
static NOT_OVERLAY: LazyLock<Mutex<HashSet<u64>>> = LazyLock::new(Mutex::default);
#[cfg(target_os = "linux")]
const MAX_DEVICES: usize = 256;

/// Whether `path`, still the file or folder `meta` describes, is on an
/// overlay file system. Anything that can't be told counts as not.
#[cfg(target_os = "linux")]
fn on_overlay(path: &Path, meta: &std::fs::Metadata) -> bool {
    use std::os::fd::AsRawFd;

    let lock = || NOT_OVERLAY.lock().unwrap_or_else(PoisonError::into_inner);
    if lock().contains(&meta.dev()) {
        return false;
    }
    // Opened without following a link or reading it, and checked to be the
    // same one, so a swap since `meta` can't borrow another's file system.
    let Ok(file) = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_PATH | libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(path)
    else {
        return false;
    };
    let same = file
        .metadata()
        .is_ok_and(|now| now.dev() == meta.dev() && now.ino() == meta.ino());
    if !same {
        return false;
    }
    let mut stat = std::mem::MaybeUninit::<libc::statfs>::uninit();
    // SAFETY: `file` is an open descriptor and `stat` is written by the call.
    if unsafe { libc::fstatfs(file.as_raw_fd(), stat.as_mut_ptr()) } != 0 {
        return false;
    }
    // SAFETY: `fstatfs` succeeded, so it filled `stat`.
    let stat = unsafe { stat.assume_init() };
    // `f_type`'s and the constant's types differ between C libraries; the
    // value fits either.
    #[allow(clippy::unnecessary_cast)]
    let overlay = stat.f_type as i64 == libc::OVERLAYFS_SUPER_MAGIC as i64;
    if !overlay {
        let mut not_overlay = lock();
        if not_overlay.len() < MAX_DEVICES {
            not_overlay.insert(meta.dev());
        }
    }
    overlay
}

#[cfg(not(target_os = "linux"))]
fn on_overlay(_path: &Path, _meta: &std::fs::Metadata) -> bool {
    false
}

fn now_nanos() -> i128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(/*default*/ 0, |since| since.as_nanos() as i128)
}

/// Whether a status changed at `ctime` is old enough to trust.
fn settled(ctime: i128) -> bool {
    now_nanos() - ctime > RACY_NANOS
}

/// Where a link inside a git folder leads, followed hop by hop.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Link {
    raw: PathBuf,
    resolved: Option<PathBuf>,
    /// Ends anywhere but a ref (as `core.preferSymlinkRefs` makes): outside
    /// the repository's git folders, at its own config or hooks, at a
    /// folder, at a hard-linked file, or somewhere unknown. Git would write
    /// there through it.
    escapes: bool,
}

impl Link {
    fn read(path: &Path, git_dirs: &[PathBuf]) -> Self {
        let raw = std::fs::read_link(path).unwrap_or_default();
        let (resolved, escapes) = match follow(path) {
            Some((end, meta)) => {
                let not_ref = !git_dirs.iter().any(|dir| end.starts_with(dir.join("refs")));
                let odd = meta.is_some_and(|meta| meta.is_dir() || meta.nlink() > 1);
                (Some(end), not_ref || odd)
            }
            None => (None, true),
        };
        Link {
            raw,
            resolved,
            escapes,
        }
    }
}

/// The path a chain of links starting at `path` ends at (folders on the way
/// resolved), and its status if it exists. A missing end is where git would
/// create the file. `None` if the chain can't be followed.
fn follow(path: &Path) -> Option<(PathBuf, Option<std::fs::Metadata>)> {
    let mut current = path.to_path_buf();
    for _ in 0..MAX_LINK_HOPS {
        let raw = std::fs::read_link(&current).ok()?;
        let full = match current.parent() {
            Some(parent) if raw.is_relative() => parent.join(&raw),
            _ => raw,
        };
        let name = full.file_name()?.to_os_string();
        if name == OsStr::new("..") || name == OsStr::new(".") {
            return None;
        }
        current = std::fs::canonicalize(full.parent()?).ok()?.join(name);
        match std::fs::symlink_metadata(&current) {
            Ok(meta) if meta.file_type().is_symlink() => continue,
            Ok(meta) => return Some((current, Some(meta))),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Some((current, None)),
            Err(_) => return None,
        }
    }
    None
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct Entry {
    kind: Kind,
    dev: u64,
    ino: u64,
    nlink: u64,
    /// Length and times, kept current for legacy remote files.
    stamp: (u64, i128, i128),
    link: Option<Link>,
}

impl Entry {
    fn stat(path: &Path, kind: Kind, ino: u64) -> Self {
        let meta = std::fs::symlink_metadata(path).ok();
        Entry {
            kind,
            dev: meta.as_ref().map_or(/*default*/ 0, MetadataExt::dev),
            ino: meta.as_ref().map_or(ino, MetadataExt::ino),
            nlink: meta.as_ref().map_or(/*default*/ 1, MetadataExt::nlink),
            stamp: meta.as_ref().map_or(/*default*/ (0, 0, 0), |meta| {
                let (mtime, ctime) = times(meta);
                (meta.len(), mtime, ctime)
            }),
            link: None,
        }
    }
}

/// What a folder is to git, from where it sits (never from what it holds:
/// a command could make any folder look like a git folder).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Role {
    /// A git folder: a repository's, a linked worktree's or a submodule's.
    GitDir,
    /// `objects`, `lfs` or `annex` of a git folder.
    Store {
        objects: bool,
    },
    /// `objects/info` and `objects/pack` (git links packs into place).
    StorePart,
    /// `remotes` or `branches` of a git folder, and folders below them.
    Legacy,
    /// `hooks` of a git folder, and folders below it.
    Hooks,
    /// `worktrees` of a git folder: its folders are git folders.
    Worktrees,
    /// Below `modules` of a git folder: a git folder where it has `HEAD` and
    /// `objects` or `commondir` (submodule names may hold slashes). A fake
    /// one only hides its own objects, which git never writes through.
    Modules,
    Plain,
}

#[derive(Clone, Debug)]
struct Folder {
    dev: u64,
    ino: u64,
    born: Option<i128>,
    ctime: i128,
    /// Listed long enough after its last change to trust its change time.
    settled: bool,
    role: Role,
    entries: HashMap<OsString, Entry>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct StateRead {
    dev: u64,
    ino: u64,
    len: u64,
    times: (i128, i128),
    /// What in it runs commands.
    lines: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(super) enum DotGit {
    Missing,
    Folder {
        dev: u64,
        ino: u64,
        born: Option<i128>,
    },
    File {
        dev: u64,
        ino: u64,
        born: Option<i128>,
        text: Option<String>,
    },
    Link {
        target: Option<PathBuf>,
    },
    Other,
}

impl DotGit {
    fn read(path: &Path) -> Self {
        let Ok(meta) = std::fs::symlink_metadata(path) else {
            return DotGit::Missing;
        };
        match Kind::of(meta.file_type()) {
            Kind::Dir => DotGit::Folder {
                dev: meta.dev(),
                ino: meta.ino(),
                born: born(path, &meta),
            },
            Kind::File => DotGit::File {
                dev: meta.dev(),
                ino: meta.ino(),
                born: born(path, &meta),
                text: read_small(path),
            },
            Kind::Link => DotGit::Link {
                target: std::fs::read_link(path).ok(),
            },
            Kind::Other => DotGit::Other,
        }
    }
}

#[derive(Clone, Debug)]
struct Snapshot {
    folders: HashMap<PathBuf, Folder>,
    states: HashMap<PathBuf, StateRead>,
    dot_git: DotGit,
    /// Why the look is incomplete, if it is.
    incomplete: Option<String>,
    /// `.git` was missing at a later look (a worktree removed, a clone
    /// deleted).
    went_missing: bool,
    /// New config files and hooks folders this session found empty
    /// (`is_empty_stub`), still there and empty: reported once something
    /// is in them, whatever `Global::empty_new` holds meanwhile.
    empty_new: HashSet<PathBuf>,
    /// The git folders this look walked (#943 review r6); empty in a
    /// baseline.
    git_dirs: Vec<PathBuf>,
    /// A folder it reached again in another role than the one it listed it
    /// in (one git folder inside another's tree, #943 item 13 review r1):
    /// what it saw of that tree may be less than a look of that folder alone
    /// sees.
    overlapping: bool,
    /// Its `.git` is held as a reported swap (#943 r8 N1): reported by this
    /// look, or by another session or an earlier look and still the same
    /// one. Kept here, not only in `Global::held` (which a `held_for` of
    /// any session prunes once the `.git` is swapped again, before this
    /// session's next look), so that a later swap, also for another empty
    /// folder, is reported (#1497 review).
    swap_held: bool,
    /// Those of `empty_new` past `MAX_EMPTY_NEW` (not remembered) that this
    /// look reported, or found reported and held (#943 r8 N2).
    unremembered: HashSet<PathBuf>,
    /// #943 item 14: what the last look saw of git folders the repository
    /// names now that this look didn't walk, because it was a `finish` given
    /// the git folders found before its command (another look of this
    /// session saw the `.git` replaced meanwhile). Kept while later looks
    /// walk the same git folders and see the same `.git`; `told_again` reads
    /// it.
    beyond: HashMap<PathBuf, Folder>,
    beyond_states: HashMap<PathBuf, StateRead>,
    /// What of `beyond` this session's look that saw it reported.
    beyond_told: HashSet<HeldWhat>,
    /// What this look reported, for `beyond_told`.
    told: HashSet<HeldWhat>,
}

impl Snapshot {
    /// What a first look is compared with: nothing (the `.git` itself
    /// aside).
    fn empty(dot_git: &DotGit) -> Self {
        Snapshot {
            folders: HashMap::new(),
            states: HashMap::new(),
            dot_git: dot_git.clone(),
            incomplete: None,
            went_missing: false,
            empty_new: HashSet::new(),
            git_dirs: Vec::new(),
            overlapping: false,
            swap_held: false,
            unremembered: HashSet::new(),
            beyond: HashMap::new(),
            beyond_states: HashMap::new(),
            beyond_told: HashSet::new(),
            told: HashSet::new(),
        }
    }
}

fn walk(repo: &Repo, prev: Option<&Snapshot>) -> Snapshot {
    let mut snapshot = Snapshot::empty(&DotGit::read(&repo.dot_git));
    snapshot.git_dirs = repo.git_dirs.clone();
    let home = dirs::home_dir().and_then(|home| std::fs::canonicalize(home).ok());
    if repo
        .git_dirs
        .iter()
        .any(|dir| dir.parent().is_none() || Some(dir) == home.as_ref())
    {
        snapshot.incomplete =
            Some("its git folder is your home folder or the file system root".to_string());
        return snapshot;
    }
    let mut roots = repo.git_dirs.clone();
    let mut count = 0;
    // Breadth first: a git folder's own entries are looked at before a cap
    // can stop the look.
    let mut queue: VecDeque<(PathBuf, usize, Role)> = repo
        .git_dirs
        .iter()
        .map(|dir| (dir.clone(), 0, Role::GitDir))
        .collect();
    while let Some((dir, depth, role)) = queue.pop_front() {
        if let Some(seen) = snapshot.folders.get(&dir) {
            let has = |name: &str| seen.entries.contains_key(OsStr::new(name));
            let role = match role {
                Role::Modules if has("HEAD") && (has("objects") || has("commondir")) => {
                    Role::GitDir
                }
                role => role,
            };
            if role != seen.role {
                snapshot.overlapping = true;
            }
            continue;
        }
        let Ok(meta) = std::fs::symlink_metadata(&dir) else {
            continue;
        };
        if !meta.is_dir() {
            continue;
        }
        let old = prev.and_then(|prev| prev.folders.get(&dir));
        let (_, ctime) = times(&meta);
        let mut folder = match old.filter(|old| {
            old.settled
                && old.dev == meta.dev()
                && old.ino == meta.ino()
                && old.born == born(&dir, &meta)
                && old.ctime == ctime
        }) {
            Some(old) => old.clone(),
            None => match list(&dir, &meta, old) {
                Some(folder) => folder,
                // Gone meanwhile (a rebase ended): nothing there to follow.
                None if std::fs::symlink_metadata(&dir)
                    .is_err_and(|err| err.kind() == std::io::ErrorKind::NotFound) =>
                {
                    continue;
                }
                None => {
                    snapshot.incomplete = Some(format!("{} can't be read", shown_path(&dir).0));
                    continue;
                }
            },
        };
        let has = |name: &str| folder.entries.contains_key(OsStr::new(name));
        let role = match role {
            Role::Modules if has("HEAD") && (has("objects") || has("commondir")) => Role::GitDir,
            role => role,
        };
        folder.role = role;
        for (name, entry) in &mut folder.entries {
            let path = dir.join(name);
            if entry.kind == Kind::Link {
                entry.link = Some(Link::read(&path, &repo.git_dirs));
            }
            // Config files' lengths too: an empty one may be filled later
            // (`is_empty_stub`).
            if (role == Role::Legacy && entry.kind != Kind::Dir)
                || (role == Role::GitDir && entry.kind == Kind::File && is_config_name(name))
            {
                *entry = Entry {
                    link: entry.link.take(),
                    ..Entry::stat(&path, entry.kind, entry.ino)
                };
            }
        }
        count += folder.entries.len();
        if count > MAX_ENTRIES {
            snapshot.incomplete = Some(
                "it has too many entries (git pack-refs --all or git gc may help)".to_string(),
            );
            break;
        }
        if role == Role::GitDir && !roots.contains(&dir) {
            roots.push(dir.clone());
        }
        for (name, entry) in &folder.entries {
            if entry.kind != Kind::Dir {
                continue;
            }
            let is = |names: &[&str]| names.iter().any(|kept| name.as_os_str() == *kept);
            let child = match role {
                Role::GitDir if is(&STORES) => Role::Store {
                    objects: name.as_os_str() == "objects",
                },
                Role::GitDir if is(&LEGACY_REMOTES) => Role::Legacy,
                Role::GitDir if name.as_os_str() == "worktrees" => Role::Worktrees,
                Role::GitDir if name.as_os_str() == "modules" => Role::Modules,
                Role::GitDir if name.as_os_str() == "hooks" => Role::Hooks,
                Role::Hooks => Role::Hooks,
                Role::Worktrees => Role::GitDir,
                Role::Modules => Role::Modules,
                Role::Store { objects: true } if is(&OBJECT_FOLDERS_LOOKED_AT) => Role::StorePart,
                Role::Store { .. } => continue,
                // `remotes/a/b` defines the remote `a/b`.
                Role::Legacy => Role::Legacy,
                Role::GitDir | Role::StorePart | Role::Plain => Role::Plain,
            };
            if depth >= MAX_DEPTH {
                snapshot.incomplete = Some("its folders are nested too deep".to_string());
                continue;
            }
            queue.push_back((dir.join(name), depth + 1, child));
        }
        snapshot.folders.insert(dir, folder);
    }
    // Where the look stopped short, what the last look saw stays the
    // baseline (listed again next time).
    if snapshot.incomplete.is_some()
        && let Some(prev) = prev
    {
        for (dir, folder) in &prev.folders {
            if !snapshot.folders.contains_key(dir) {
                let mut folder = folder.clone();
                folder.settled = false;
                snapshot.folders.insert(dir.clone(), folder);
            }
        }
    }
    for root in roots {
        for name in STATE_FILES {
            let path = root.join(name);
            let Ok(meta) = std::fs::metadata(&path) else {
                continue;
            };
            let now = times(&meta);
            let old = prev
                .and_then(|prev| prev.states.get(&path))
                .filter(|old| {
                    old.dev == meta.dev()
                        && old.ino == meta.ino()
                        && old.len == meta.len()
                        && old.times == now
                        && settled(now.1)
                })
                .cloned();
            let read = old.unwrap_or_else(|| StateRead {
                dev: meta.dev(),
                ino: meta.ino(),
                len: meta.len(),
                times: now,
                lines: state_lines(&path),
            });
            snapshot.states.insert(path, read);
        }
    }
    snapshot
}

fn list(dir: &Path, meta: &std::fs::Metadata, old: Option<&Folder>) -> Option<Folder> {
    let mut entries = HashMap::new();
    // An unreadable folder fails the look (it can hide what git follows).
    for item in std::fs::read_dir(dir).ok()? {
        let item = item.ok()?;
        // Gone meanwhile (git renamed it into place): skipped.
        let file_type = match item.file_type() {
            Ok(file_type) => file_type,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => continue,
            Err(_) => return None,
        };
        let (name, kind, ino) = (item.file_name(), Kind::of(file_type), item.ino());
        let entry = old
            .and_then(|old| old.entries.get(&name))
            .filter(|old| old.kind == kind && old.ino == ino)
            .cloned()
            .unwrap_or_else(|| Entry::stat(&item.path(), kind, ino));
        entries.insert(name, entry);
    }
    let after = std::fs::symlink_metadata(dir).ok()?;
    let (_, ctime) = times(meta);
    Some(Folder {
        dev: meta.dev(),
        ino: meta.ino(),
        born: born(dir, meta),
        ctime,
        settled: times(&after).1 == ctime && settled(ctime),
        role: Role::Plain,
        entries,
    })
}

/// What in a rebase or sequencer state file runs commands: a todo's `exec`
/// lines, a merge strategy that isn't built in. Fails closed: a file that
/// can't be read or is too long counts as holding one.
fn state_lines(path: &Path) -> Vec<String> {
    let mut bytes = Vec::new();
    let Some(complete) = read_regular(path, MAX_TODO_BYTES, &mut bytes) else {
        return vec!["(the file can't be read)".to_string()];
    };
    let text = String::from_utf8_lossy(&bytes);
    let mut lines = match path.file_name().and_then(OsStr::to_str) {
        Some("strategy") => strategy_lines([text.trim()]),
        // Git config syntax: `strategy = <name>` in `[options]`.
        Some("opts") => strategy_lines(text.lines().filter_map(|line| {
            // Git takes `[options] strategy = x` on one line, any case.
            let line = match line.trim_start().strip_prefix('[') {
                Some(rest) => rest.split_once(']')?.1,
                None => line,
            };
            let (key, value) = line.split_once('=')?;
            key.trim()
                .eq_ignore_ascii_case("strategy")
                .then(|| value.trim().trim_matches('"'))
        })),
        _ => exec_lines(&text),
    };
    if !complete && lines.is_empty() {
        lines.push("(the file is too long to check)".to_string());
    }
    lines
}

fn strategy_lines<'a>(names: impl IntoIterator<Item = &'a str>) -> Vec<String> {
    names
        .into_iter()
        .filter(|name| !name.is_empty() && !BUILTIN_STRATEGIES.contains(name))
        .map(|name| format!("merge strategy {name}"))
        .collect()
}

/// Git's sequencer takes a command name followed by a space, tab, carriage
/// return or the line's end; `exec` runs the rest through the shell.
pub(super) fn exec_lines(text: &str) -> Vec<String> {
    text.split('\n')
        .map(|line| line.trim_start_matches([' ', '\t']))
        .filter(|line| {
            let command = line.split([' ', '\t', '\r']).next().unwrap_or_default();
            (command == "exec" || command == "x")
                && !line[command.len()..]
                    .trim_matches([' ', '\t', '\r'])
                    .is_empty()
        })
        .map(|line| line.trim_end().to_string())
        .collect()
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum Finding {
    Link {
        path: PathBuf,
        target: Option<PathBuf>,
    },
    HardLink {
        path: PathBuf,
    },
    /// A rebase or sequencer state file that runs commands.
    Exec {
        todo: PathBuf,
        lines: Vec<String>,
    },
    Remote {
        path: PathBuf,
    },
    /// New config or hooks of a repository's own git folder.
    Config {
        path: PathBuf,
    },
    Replaced {
        dot_git: PathBuf,
    },
    Incomplete {
        dot_git: PathBuf,
        why: String,
    },
}

/// What has changed from `prev` to `cur`. A first look (`prev` empty)
/// reports what git would follow out of the sandbox (links, commands in
/// state files) and hard links: an ordinary repository has legacy remotes,
/// config and hooks of its own, and hard-links only its objects.
fn compare(
    repo: &Repo,
    prev: &Snapshot,
    cur: &Snapshot,
    first: bool,
    remembered: &HashSet<PathBuf>,
    swap_held: bool,
) -> (Vec<Finding>, EmptyNew) {
    let mut findings = Vec::new();
    let mut empty = EmptyNew::default();
    // An empty `.git` folder replaced by another (the Linux sandbox's mount
    // point where there was none, removed and made again by other commands)
    // holds nothing git follows; what is put in it later is compared with
    // that. #943 (r8 N1): not while the last one is held as a reported swap
    // (`swap_held`): swapped again, the hold would lapse unreported.
    if prev.dot_git != cur.dot_git
        && (swap_held || !(empty_git_folder(repo, prev) && empty_git_folder(repo, cur)))
    {
        findings.push(Finding::Replaced {
            dot_git: repo.dot_git.clone(),
        });
    }
    let mut folders: Vec<_> = cur.folders.iter().collect();
    folders.sort_by(|a, b| a.0.cmp(b.0));
    for (dir, folder) in folders {
        // A folder the last look didn't have counts as empty then (also
        // where that look couldn't read it).
        let prev_folder = prev.folders.get(dir);
        let own = repo.git_dirs.contains(dir);
        let mut names: Vec<_> = folder.entries.iter().collect();
        names.sort_by(|a, b| a.0.cmp(b.0));
        for (name, entry) in names {
            let path = dir.join(name);
            let old = prev_folder.and_then(|folder| folder.entries.get(name));
            let new = old.is_none_or(|old| {
                old.kind != entry.kind || old.ino != entry.ino || old.dev != entry.dev
            });
            // Git runs hooks, it doesn't write through them; links in an
            // existing hooks folder are the person's (the folder is
            // protected), a new one is reported below.
            let hooks = folder.role == Role::Hooks
                || (folder.role == Role::GitDir && name.as_os_str() == "hooks");
            if let Some(link) = entry.link.as_ref().filter(|link| link.escapes)
                && !hooks
                && (new || old.and_then(|old| old.link.as_ref()) != Some(link))
            {
                findings.push(Finding::Link {
                    path: path.clone(),
                    target: link.resolved.clone().or_else(|| Some(link.raw.clone())),
                });
            }
            // Git links packs into place, then removes the temporary name.
            if new
                && entry.kind == Kind::File
                && entry.nlink > 1
                && !matches!(folder.role, Role::Store { .. } | Role::StorePart)
            {
                findings.push(Finding::HardLink { path: path.clone() });
            }
            // Config or hooks of the repository's own git folder.
            let config = own && (name.as_os_str() == "hooks" || is_config_name(name));
            // One that was new and empty (in any session's look, first
            // looks too): reported once something is in it.
            if config && (remembered.contains(&path) || prev.empty_new.contains(&path)) {
                if is_empty_stub(cur, &path, entry) {
                    empty.keep.push(path.clone());
                    empty.remember.push(path);
                } else {
                    findings.push(Finding::Config { path: path.clone() });
                    empty.forget.push(path);
                }
                continue;
            }
            if first {
                continue;
            }
            if folder.role == Role::Legacy
                && entry.kind != Kind::Dir
                && (new || old.is_some_and(|old| old.stamp != entry.stamp))
            {
                findings.push(Finding::Remote { path: path.clone() });
            }
            // What the protected-file rules can't hold: config or hooks of
            // the repository's own git folder that didn't exist (on Linux a
            // missing one isn't held read-only). Git rewrites existing ones
            // by renaming; that isn't reported. A new one that is empty
            // (`is_empty_stub`) is remembered instead.
            if config && prev_folder.is_some() && old.is_none() {
                if is_empty_stub(cur, &path, entry) {
                    empty.keep.push(path.clone());
                    empty.remember.push(path);
                } else {
                    findings.push(Finding::Config { path });
                }
            }
        }
    }
    let mut states: Vec<_> = cur.states.iter().collect();
    states.sort_by(|a, b| a.0.cmp(b.0));
    for (path, read) in states {
        // Reported when something runs that didn't before (a rebase with
        // `-x` drops a line at every step).
        let before = prev.states.get(path).map(|old| old.lines.as_slice());
        if read
            .lines
            .iter()
            .any(|line| !before.unwrap_or_default().contains(line))
        {
            findings.push(Finding::Exec {
                todo: path.clone(),
                lines: read.lines.clone(),
            });
        }
    }
    // Remembered ones that are gone from a git folder this look listed
    // (not after an incomplete look).
    empty.forget.extend(
        remembered
            .iter()
            .filter(|_| cur.incomplete.is_none())
            .filter(|path| {
                path.parent()
                    .and_then(|dir| cur.folders.get(dir))
                    .zip(path.file_name())
                    .is_some_and(|(folder, name)| !folder.entries.contains_key(name))
            })
            .cloned(),
    );
    (findings, empty)
}

/// New config files and hooks folders found empty (`is_empty_stub`): to
/// remember for every session, to keep in this session's look, and
/// remembered ones that are filled or gone, to forget.
#[derive(Default)]
struct EmptyNew {
    remember: Vec<PathBuf>,
    keep: Vec<PathBuf>,
    forget: Vec<PathBuf>,
}

/// A `.git` folder whose git folder holds only empty entries
/// (`is_empty_stub`), as the Linux sandbox's mount points leave it.
fn empty_git_folder(repo: &Repo, snapshot: &Snapshot) -> bool {
    matches!(snapshot.dot_git, DotGit::Folder { .. })
        && repo.git_dirs.iter().all(|dir| {
            snapshot.folders.get(dir).is_some_and(|folder| {
                folder
                    .entries
                    .iter()
                    .all(|(name, entry)| is_empty_stub(snapshot, &dir.join(name), entry))
            })
        })
}

fn is_config_name(name: &OsStr) -> bool {
    CONFIG_NAMES.iter().any(|kept| name == *kept)
}

/// An empty file, or a listed folder with nothing in it: git runs nothing
/// from it. The Linux sandbox leaves such entries in place while a command
/// runs (mount points for protected paths that didn't exist), where another
/// session's look can see them.
fn is_empty_stub(snapshot: &Snapshot, path: &Path, entry: &Entry) -> bool {
    match entry.kind {
        Kind::File => entry.stamp.0 == 0,
        Kind::Dir => snapshot
            .folders
            .get(path)
            .is_some_and(|folder| folder.entries.is_empty()),
        Kind::Link | Kind::Other => false,
    }
}

/// A finding that stands until the person removes it: while any does, a
/// person's approval doesn't lift the protected-file rules for commands in
/// that repository, in any session of this process (a sub-agent's findings
/// count for its parent too).
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
struct Held {
    /// The repository's git folders (real paths).
    git_dirs: Vec<PathBuf>,
    what: HeldWhat,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
enum HeldWhat {
    Link { path: PathBuf },
    HardLink { path: PathBuf },
    Exec { todo: PathBuf },
    Exists { path: PathBuf },
    Replaced { dot_git: PathBuf, now: DotGit },
}

impl Held {
    fn of(finding: &Finding, repo: &Repo, snapshot: &Snapshot) -> Option<Self> {
        let what = match finding {
            Finding::Link { path, .. } => HeldWhat::Link { path: path.clone() },
            Finding::HardLink { path } => HeldWhat::HardLink { path: path.clone() },
            Finding::Exec { todo, .. } => HeldWhat::Exec { todo: todo.clone() },
            Finding::Remote { path } | Finding::Config { path } => {
                HeldWhat::Exists { path: path.clone() }
            }
            Finding::Replaced { dot_git } => HeldWhat::Replaced {
                dot_git: dot_git.clone(),
                now: snapshot.dot_git.clone(),
            },
            Finding::Incomplete { .. } => return None,
        };
        Some(Held {
            git_dirs: repo.git_dirs.clone(),
            what,
        })
    }

    fn path(&self) -> &Path {
        self.what.path()
    }
}

impl HeldWhat {
    fn path(&self) -> &Path {
        match self {
            HeldWhat::Link { path } | HeldWhat::HardLink { path } | HeldWhat::Exists { path } => {
                path
            }
            HeldWhat::Exec { todo } => todo,
            HeldWhat::Replaced { dot_git, .. } => dot_git,
        }
    }
}

impl Held {
    /// Whether what was found is still there (looked at again, not by
    /// identity: a freed inode may be reused).
    fn stands(&self) -> bool {
        match &self.what {
            HeldWhat::Link { path } => {
                std::fs::symlink_metadata(path).is_ok_and(|meta| meta.file_type().is_symlink())
                    && Link::read(path, &self.git_dirs).escapes
            }
            HeldWhat::HardLink { path } => std::fs::symlink_metadata(path)
                .is_ok_and(|meta| meta.file_type().is_file() && meta.nlink() > 1),
            HeldWhat::Exec { todo } => {
                std::fs::metadata(todo).is_ok() && !state_lines(todo).is_empty()
            }
            HeldWhat::Exists { path } => std::fs::symlink_metadata(path).is_ok(),
            HeldWhat::Replaced { dot_git, now } => DotGit::read(dot_git) == *now,
        }
    }
}

/// A repository's last look, locked while one is taken. Lock order: a
/// repository's lock is never taken while holding `GLOBAL`; `GLOBAL` may be
/// taken briefly while holding a repository's lock.
type Slot = Arc<Mutex<Option<Snapshot>>>;

#[derive(Default)]
struct ThreadState {
    repos: HashMap<PathBuf, (Repo, Slot)>,
    /// When each repository was last used, for forgetting the oldest.
    used: HashMap<PathBuf, u64>,
    last_used: u64,
}

#[derive(Default)]
pub(super) struct Global {
    threads: HashMap<ThreadId, ThreadState>,
    held: HashSet<Held>,
    /// `.git` folders (by identity) any session reported as a swap: not
    /// pruned like `held`, so that a session whose last look saw one before
    /// that report (#943 r8 N1, #1502 review) still reports its next swap.
    swapped: HashSet<DotGit>,
    /// `swapped` was full when another was reported: from then on every
    /// `.git` folder counts as one (fails closed; dropping entries would
    /// reopen the gap they close). Never cleared while the process runs:
    /// clearing it would drop all of them at once.
    swapped_full: bool,
    /// New config files and hooks folders that were empty when a session
    /// looked (`is_empty_stub`): any session's later look reports one once
    /// something is in it. Kept when a session forgets the repository.
    empty_new: HashSet<PathBuf>,
    /// Git folders (by repository) a look couldn't see all of; those a
    /// later look saw in full are taken out (#943 item 13: also where that
    /// look walked other folders with them).
    incomplete: HashMap<Vec<PathBuf>, Unchecked>,
    uses: u64,
    /// Looks started, for `incomplete`: a look that started earlier doesn't
    /// clear what a later one recorded.
    looks: u64,
}

impl Global {
    /// `dot_git` (a folder; nothing else needs it) was reported as a swap.
    pub(super) fn note_swapped(&mut self, dot_git: &DotGit) {
        if !matches!(dot_git, DotGit::Folder { .. }) {
            return;
        }
        if self.swapped.len() < MAX_SWAPPED || self.swapped.contains(dot_git) {
            self.swapped.insert(dot_git.clone());
        } else {
            self.swapped_full = true;
        }
    }

    /// `dot_git` counts as reported as a swap (by any session).
    pub(super) fn is_swapped(&self, dot_git: &DotGit) -> bool {
        (self.swapped_full && matches!(dot_git, DotGit::Folder { .. }))
            || self.swapped.contains(dot_git)
    }
}

static GLOBAL: LazyLock<Mutex<Global>> = LazyLock::new(Default::default);

fn with_global<T>(f: impl FnOnce(&mut Global) -> T) -> T {
    f(&mut GLOBAL.lock().unwrap_or_else(PoisonError::into_inner))
}

/// `repo`'s slot in this session, counted as used now where `used` (a
/// command runs there), and the git folders stored for it before. A
/// forgotten repository (past the limits) gets a first look again, which
/// reports links, steps and hard links there.
fn slot_of(thread: ThreadId, repo: &Repo, used: bool) -> (Slot, Vec<PathBuf>) {
    with_global(|global| {
        global.uses += 1;
        let uses = global.uses;
        if !global.threads.contains_key(&thread)
            && global.threads.len() >= MAX_THREADS
            && let Some(oldest) = global
                .threads
                .iter()
                .min_by_key(|(_, state)| state.last_used)
                .map(|(thread, _)| *thread)
        {
            global.threads.remove(&oldest);
        }
        let state = global.threads.entry(thread).or_default();
        state.last_used = uses;
        if !state.repos.contains_key(&repo.dot_git)
            && state.repos.len() >= MAX_REPOS_PER_THREAD
            && let Some(oldest) = state
                .used
                .iter()
                .min_by_key(|(_, used)| **used)
                .map(|(path, _)| path.clone())
        {
            state.repos.remove(&oldest);
            state.used.remove(&oldest);
        }
        if used || !state.used.contains_key(&repo.dot_git) {
            state.used.insert(repo.dot_git.clone(), uses);
        }
        let (stored, slot) = state
            .repos
            .entry(repo.dot_git.clone())
            .or_insert_with(|| (repo.clone(), Slot::default()));
        let before = stored.git_dirs.clone();
        // #943 review r5: where a command runs there, its git folders as
        // found now (a `.git` made again may be another kind) are what later
        // looks (`recheck`) walk.
        if used && stored.git_dirs != repo.git_dirs {
            *stored = repo.clone();
        }
        (slot.clone(), before)
    })
}

/// Before a command that may write these repositories' git folders: the
/// first look, where this session has none yet, with what it found there
/// already for the person.
pub(crate) fn begin(thread: ThreadId, repos: &[Repo], level: &str) -> Option<String> {
    report(
        &look(thread, repos, /*first_only*/ true, /*used*/ true),
        level,
    )
}

/// After such a command: what is new since the last look, as a message for
/// the person (`None` if nothing).
pub(crate) fn finish(thread: ThreadId, repos: &[Repo], level: &str) -> Option<String> {
    report(
        &look(thread, repos, /*first_only*/ false, /*used*/ true),
        level,
    )
}

/// Before a command is decided, and at the end of a turn: the repositories
/// this session looked at before, looked at again. Returns the message for
/// the person and what holds the rules for a command in `folders` (found in
/// its repository by any session, a sub-agent's too).
pub(crate) fn recheck(
    thread: ThreadId,
    level: &str,
    folders: &[&Path],
) -> (Option<String>, Vec<String>) {
    let repos: Vec<Repo> = with_global(|global| {
        global.threads.get(&thread).map_or_else(Vec::new, |state| {
            state.repos.values().map(|(repo, _)| repo.clone()).collect()
        })
    });
    let message = report(
        &look(
            thread, &repos, /*first_only*/ false, /*used*/ false,
        ),
        level,
    );
    // Held for a command in the project it runs in. Each folder on its own,
    // so no cap leaves one out (#943 item 11: the command's `workdir` added).
    let watched: Vec<PathBuf> = folders
        .iter()
        .flat_map(|folder| self::repos(&[folder]))
        .flat_map(|repo| repo.git_dirs)
        .collect();
    (message, held_for(&watched))
}

#[cfg(test)]
pub(super) fn check(thread: ThreadId, repos: &[Repo]) -> Vec<Finding> {
    look(thread, repos, /*first_only*/ false, /*used*/ true)
}

/// Looks at `repos` (only those without a look yet, where `first_only`).
fn look(thread: ThreadId, repos: &[Repo], first_only: bool, used: bool) -> Vec<Finding> {
    let mut findings = Vec::new();
    let mut held = Vec::new();
    let mut walked = Vec::new();
    for repo in repos {
        let (slot, stored_dirs) = slot_of(thread, repo, used);
        let mut snapshot = slot.lock().unwrap_or_else(PoisonError::into_inner);
        if first_only && snapshot.is_some() {
            continue;
        }
        // Gone (a worktree removed, a scratch clone deleted): nothing to
        // follow. Should the same one come back, it is compared with the
        // last look; a new one there gets a first look.
        let now = DotGit::read(&repo.dot_git);
        if now == DotGit::Missing {
            if let Some(prev) = snapshot.as_mut() {
                prev.went_missing = true;
            }
            let started = with_global(|global| {
                global.looks += 1;
                global.looks
            });
            walked.push((Walked::Exact(repo.git_dirs.clone()), started));
            continue;
        }
        // Made again or replaced (#943 reviews r4, r6): it may be another
        // kind (a worktree's or submodule's `.git` file replaced by a folder,
        // or the reverse) or name other git folders, so they are worked out
        // again, also for later looks. #943 item 12: also where it is back
        // as the same one after a look saw it missing (a `commondir` that
        // names another folder now, or a file system that doesn't record
        // when a reused inode was made).
        let remade_repo = snapshot
            .as_ref()
            .filter(|prev| prev.dot_git != now || prev.went_missing)
            .and_then(|_| repo_at(repo.dot_git.clone()))
            .filter(|fresh| fresh.git_dirs != repo.git_dirs);
        if let Some(fresh) = &remade_repo {
            with_global(|global| {
                if let Some((stored, _)) = global
                    .threads
                    .get_mut(&thread)
                    .and_then(|state| state.repos.get_mut(&fresh.dot_git))
                {
                    *stored = fresh.clone();
                }
            });
        }
        // Replaced in place (no look saw it missing), or back as the same
        // one naming other git folders (#943 item 12 review r1): this look
        // also walks the git folders it named before, where something may
        // have been left meanwhile.
        let replaced_repo = remade_repo
            .as_ref()
            .filter(|_| {
                snapshot
                    .as_ref()
                    .is_some_and(|prev| !prev.went_missing || prev.dot_git == now)
            })
            .map(|fresh| {
                let mut both = fresh.clone();
                for dir in &repo.git_dirs {
                    if !both.git_dirs.contains(dir) {
                        both.git_dirs.push(dir.clone());
                    }
                }
                both
            });
        let repo = replaced_repo
            .as_ref()
            .or(remade_repo.as_ref())
            .unwrap_or(repo);
        // The last look stays until this one is done (a panic keeps it).
        let prev = snapshot.as_ref();
        let started = with_global(|global| {
            global.looks += 1;
            global.looks
        });
        // Made again: what the old one had isn't carried into this look.
        let mut cur = walk(
            repo,
            prev.filter(|prev| !(prev.went_missing && prev.dot_git != now)),
        );
        // Missing at the last look and back as a different one: a full
        // look, not a first look (#943). Likewise where the git folders
        // differ from those the last look walked (review r6: replaced as
        // another kind, or a `gitdir:` file pointed elsewhere), still
        // reported as replaced where it was.
        let remade = prev
            .filter(|prev| {
                (prev.went_missing && prev.dot_git != cur.dot_git)
                    || (!prev.git_dirs.is_empty() && prev.git_dirs != repo.git_dirs)
            })
            .map(|prev| {
                let mut baseline = remade_baseline(repo, prev, &cur);
                if !prev.went_missing {
                    baseline.dot_git = prev.dot_git.clone();
                    baseline.swap_held = prev.swap_held;
                }
                baseline
            });
        let prev = remade.as_ref().or(prev);
        let remembered: HashSet<PathBuf> = with_global(|global| {
            global
                .empty_new
                .iter()
                .filter(|path| {
                    path.parent()
                        .is_some_and(|dir| repo.git_dirs.iter().any(|git_dir| git_dir == dir))
                })
                .cloned()
                .collect()
        });
        // #943 (r8 N1): the `.git` the last look saw is held as a reported
        // swap (by this session's looks, or reported by any session).
        let swap_held = prev.is_some_and(|prev| {
            prev.swap_held
                || with_global(|global| {
                    global.is_swapped(&prev.dot_git)
                        || global.held.iter().any(|held| {
                            matches!(&held.what, HeldWhat::Replaced { dot_git, now }
                            if *dot_git == repo.dot_git && *now == prev.dot_git)
                        })
                })
        });
        let (mut found, empty) = match prev {
            Some(prev) => compare(
                repo,
                prev,
                &cur,
                /*first*/ false,
                &remembered,
                swap_held,
            ),
            None => compare(
                repo,
                &Snapshot::empty(&cur.dot_git),
                &cur,
                /*first*/ true,
                &remembered,
                /*swap_held*/ false,
            ),
        };
        // Reported as a swap now, or held as one and still the same `.git`.
        let swapped = found
            .iter()
            .any(|finding| matches!(finding, Finding::Replaced { .. }));
        if swapped {
            with_global(|global| global.note_swapped(&cur.dot_git));
        }
        cur.swap_held =
            swapped || (swap_held && prev.is_some_and(|prev| prev.dot_git == cur.dot_git));
        cur.empty_new = empty.keep.iter().cloned().collect();
        // Full: those gone meanwhile (a deleted scratch repository) go,
        // looked at without holding `GLOBAL`.
        let candidates = with_global(|global| {
            if global.empty_new.len() + empty.remember.len() > max_empty_new() {
                global.empty_new.iter().cloned().collect()
            } else {
                Vec::new()
            }
        });
        let gone: Vec<PathBuf> = candidates
            .into_iter()
            .filter(|path| std::fs::symlink_metadata(path).is_err())
            .collect();
        // Past the limit, one is reported instead of remembered.
        let unremembered = with_global(|global| {
            for path in empty.forget.iter().chain(&gone) {
                global.empty_new.remove(path);
            }
            let mut unremembered = Vec::new();
            for path in empty.remember {
                if global.empty_new.len() < max_empty_new() || global.empty_new.contains(&path) {
                    global.empty_new.insert(path);
                } else {
                    unremembered.push(path);
                }
            }
            unremembered
        });
        // #943 r8 N2: told once, not at every look: not again where this
        // session's last look reported it (still empty, past the limit) and
        // that report still holds it (held again below, as before). Filled,
        // it is reported (`compare`).
        let repeated: HashSet<PathBuf> = with_global(|global| {
            unremembered
                .iter()
                .filter(|path| prev.is_some_and(|prev| prev.unremembered.contains(*path)))
                .filter(|path| {
                    global.held.contains(&Held {
                        git_dirs: repo.git_dirs.clone(),
                        what: HeldWhat::Exists {
                            path: (*path).clone(),
                        },
                    })
                })
                .cloned()
                .collect()
        });
        cur.unremembered = unremembered.iter().cloned().collect();
        found.extend(
            unremembered
                .into_iter()
                .map(|path| Finding::Config { path }),
        );
        // What went again meanwhile (a pack git linked into place) isn't
        // reported.
        found.retain(|finding| Held::of(finding, repo, &cur).is_none_or(|held| held.stands()));
        held.extend(
            found
                .iter()
                .filter_map(|finding| Held::of(finding, repo, &cur)),
        );
        cur.told = found
            .iter()
            .filter_map(|finding| Held::of(finding, repo, &cur))
            .map(|held| held.what)
            .collect();
        // Held as before; only the message leaves out what was told already
        // (#943 r8 N2, item 14).
        let told = told_again(snapshot.as_ref(), repo, &cur, &found);
        found.retain(|finding| {
            !told.contains(finding)
                && !matches!(finding, Finding::Config { path } if repeated.contains(path))
        });
        // Told each time a look becomes incomplete; held while it is.
        if let Some(why) = &cur.incomplete
            && prev.is_none_or(|prev| prev.incomplete.as_ref() != Some(why))
        {
            found.push(Finding::Incomplete {
                dot_git: repo.dot_git.clone(),
                why: why.clone(),
            });
        }
        walked.push((
            match &cur.incomplete {
                Some(why) => {
                    Walked::Incomplete(repo.git_dirs.clone(), why.clone(), repo.dot_git.clone())
                }
                // Where git folders overlap in other roles, only its own
                // entry (as before #943 item 13).
                None if cur.overlapping => Walked::Exact(repo.git_dirs.clone()),
                None => Walked::Complete(repo.git_dirs.clone(), seen_in_full(&cur)),
            },
            started,
        ));
        findings.extend(found);
        carry_beyond(
            snapshot.as_ref(),
            &mut cur,
            (used && !first_only).then_some(stored_dirs.as_slice()),
        );
        *snapshot = Some(cur);
    }
    with_global(|global| {
        for (look, started) in walked {
            match look {
                Walked::Incomplete(git_dirs, why, dot_git) => {
                    let entry = global.incomplete.entry(git_dirs).or_insert(Unchecked {
                        why: why.clone(),
                        at: started,
                        by: HashSet::new(),
                    });
                    if entry.at <= started {
                        entry.why = why;
                        entry.at = started;
                    }
                    entry.by.insert((thread, dot_git));
                }
                // Its own entry goes, if recorded before it started. #943
                // item 13: so do the folders it saw in full from an entry
                // left behind, whose every recorder's repository names other
                // git folders now (its `.git` replaced or made again, or the
                // look that walked the old and new ones). Any other entry goes
                // only by a complete look of its own: looks of its git folders
                // one by one may each stay below `MAX_ENTRIES`.
                Walked::Complete(own, seen) => {
                    let threads = &global.threads;
                    let left_behind = |dirs: &Vec<PathBuf>, by: &HashSet<(ThreadId, PathBuf)>| {
                        by.iter().all(|(thread, dot_git)| {
                            threads
                                .get(thread)
                                .and_then(|state| state.repos.get(dot_git))
                                .is_some_and(|(repo, _)| repo.git_dirs != *dirs)
                        })
                    };
                    let mut kept: HashMap<Vec<PathBuf>, Unchecked> = HashMap::new();
                    for (mut dirs, entry) in std::mem::take(&mut global.incomplete) {
                        if entry.at < started {
                            if dirs == own {
                                continue;
                            }
                            if left_behind(&dirs, &entry.by) {
                                dirs.retain(|dir| !seen.contains(dir));
                            }
                        }
                        if dirs.is_empty() {
                            continue;
                        }
                        // Two entries left the same: the later one's reason
                        // and start, both recorders.
                        match kept.entry(dirs) {
                            hash_map::Entry::Occupied(mut kept) => {
                                let kept = kept.get_mut();
                                kept.by.extend(entry.by);
                                if kept.at < entry.at {
                                    kept.why = entry.why;
                                    kept.at = entry.at;
                                }
                            }
                            hash_map::Entry::Vacant(kept) => {
                                kept.insert(entry);
                            }
                        }
                    }
                    global.incomplete = kept;
                }
                Walked::Exact(git_dirs) => {
                    if global
                        .incomplete
                        .get(&git_dirs)
                        .is_some_and(|entry| entry.at < started)
                    {
                        global.incomplete.remove(&git_dirs);
                    }
                }
            }
        }
        global.held.extend(held);
    });
    findings
}

/// The git folders a complete look saw as a look of each alone would (#943
/// item 13 review r2): every folder it listed in the git folder role, but
/// one with a folder below it as deep as a look of it alone stops at (a
/// git folder nested in another's tree is walked from its own depth), and
/// a home folder or file system root (never looked at in full). Where it
/// reached a folder again in another role, none (`Snapshot::overlapping`).
fn seen_in_full(cur: &Snapshot) -> Vec<PathBuf> {
    let home = dirs::home_dir().and_then(|home| std::fs::canonicalize(home).ok());
    let git_dirs: HashSet<&Path> = cur
        .folders
        .iter()
        .filter(|(dir, folder)| {
            folder.role == Role::GitDir && dir.parent().is_some() && Some(*dir) != home.as_ref()
        })
        .map(|(dir, _)| dir.as_path())
        .collect();
    let too_deep: HashSet<&Path> = cur
        .folders
        .keys()
        .flat_map(|below| below.ancestors().skip(MAX_DEPTH))
        .filter(|dir| git_dirs.contains(dir))
        .collect();
    git_dirs
        .difference(&too_deep)
        .map(|dir| dir.to_path_buf())
        .collect()
}

/// An entry of `Global::incomplete`.
struct Unchecked {
    why: String,
    /// When the look that recorded it last started.
    at: u64,
    /// The sessions' repositories (by `.git`) whose looks recorded it.
    by: HashSet<(ThreadId, PathBuf)>,
}

/// How a look at a repository went, for `Global::incomplete`.
enum Walked {
    /// Its git folders, and those it saw in full (`seen_in_full`).
    Complete(Vec<PathBuf>, Vec<PathBuf>),
    /// Its git folders weren't, why, and its `.git`.
    Incomplete(Vec<PathBuf>, String, PathBuf),
    /// Only its own entry goes: its `.git` is gone (nothing to follow
    /// there), or it saw them in full but overlapping in other roles.
    Exact(Vec<PathBuf>),
}

/// #943 item 14: findings in folders `last.beyond` holds (a look after a
/// stale `finish`) that the look which saw them there reported, unchanged
/// since, and whose hold still stands: the person was told already.
fn told_again(
    last: Option<&Snapshot>,
    repo: &Repo,
    cur: &Snapshot,
    found: &[Finding],
) -> Vec<Finding> {
    let Some(last) = last.filter(|last| {
        !last.beyond.is_empty() && !last.went_missing && last.dot_git == cur.dot_git
    }) else {
        return Vec::new();
    };
    let unchanged = |finding: &Finding| {
        Held::of(finding, repo, cur).is_some_and(|held| last.beyond_told.contains(&held.what))
            && match finding {
                Finding::Link { path, .. }
                | Finding::HardLink { path }
                | Finding::Config { path }
                | Finding::Remote { path } => {
                    let entry = |folders: &HashMap<PathBuf, Folder>| {
                        let folder = folders.get(path.parent()?)?;
                        folder.entries.get(path.file_name()?).cloned()
                    };
                    entry(&last.beyond).is_some_and(|old| entry(&cur.folders) == Some(old))
                }
                Finding::Exec { todo, .. } => last
                    .beyond_states
                    .get(todo)
                    .is_some_and(|old| cur.states.get(todo) == Some(old)),
                Finding::Replaced { .. } | Finding::Incomplete { .. } => false,
            }
    };
    let candidates: Vec<&Finding> = found.iter().filter(|finding| unchanged(finding)).collect();
    if candidates.is_empty() {
        return Vec::new();
    }
    let holds: Vec<Held> = with_global(|global| global.held.iter().cloned().collect());
    candidates
        .into_iter()
        .filter(|finding| {
            Held::of(finding, repo, cur).is_some_and(|mine| {
                holds.iter().any(|held| {
                    held.what == mine.what
                        && held.git_dirs.iter().any(|dir| repo.git_dirs.contains(dir))
                        && held.stands()
                })
            })
        })
        .cloned()
        .collect()
}

/// #943 item 14: `cur.beyond` (and what of it was told). Where `stale` (a
/// `finish`, with the git folders stored for the repository before it),
/// what `last` saw of those of them `cur` didn't walk; and `last.beyond`
/// where `cur` walked the same git folders. Only while the `.git` is the same one (no look saw it
/// missing).
fn carry_beyond(last: Option<&Snapshot>, cur: &mut Snapshot, stale: Option<&[PathBuf]>) {
    let Some(last) = last.filter(|last| !last.went_missing && last.dot_git == cur.dot_git) else {
        return;
    };
    let stale: Vec<&PathBuf> = stale
        .unwrap_or_default()
        .iter()
        .filter(|dir| !cur.git_dirs.contains(dir))
        .collect();
    let same = last.git_dirs == cur.git_dirs;
    let folders = last
        .folders
        .iter()
        .filter(|(dir, _)| stale.iter().any(|stale| dir.starts_with(stale)))
        .chain(last.beyond.iter().filter(|_| same));
    for (dir, folder) in folders {
        if !cur.folders.contains_key(dir) && !cur.beyond.contains_key(dir) {
            cur.beyond.insert(dir.clone(), folder.clone());
        }
    }
    let told = last
        .told
        .iter()
        .filter(|what| stale.iter().any(|stale| what.path().starts_with(stale)))
        .chain(last.beyond_told.iter().filter(|_| same));
    for what in told {
        cur.beyond_told.insert(what.clone());
    }
    for (path, read) in last.states.iter().chain(&last.beyond_states) {
        if path
            .parent()
            .is_some_and(|dir| cur.beyond.contains_key(dir))
            && !cur.states.contains_key(path)
            && !cur.beyond_states.contains_key(path)
        {
            cur.beyond_states.insert(path.clone(), read.clone());
        }
    }
}

/// What a `.git` made again (missing at the last look, now a different one)
/// is compared with: the last look's folders that are still the same ones,
/// and empty ones for its own git folders that aren't, so that config,
/// hooks, remotes and steps there are reported as new (empty ones once
/// filled). A worktree's `commondir` naming its common folder, still the
/// same one, isn't new (`git worktree add` again).
fn remade_baseline(repo: &Repo, prev: &Snapshot, cur: &Snapshot) -> Snapshot {
    let mut baseline = Snapshot::empty(&cur.dot_git);
    baseline.empty_new = prev.empty_new.clone();
    for (dir, folder) in &cur.folders {
        if let Some(old) = prev
            .folders
            .get(dir)
            .filter(|old| old.dev == folder.dev && old.ino == folder.ino && old.born == folder.born)
        {
            baseline.folders.insert(dir.clone(), old.clone());
        }
    }
    baseline.states = prev
        .states
        .iter()
        .filter(|(path, _)| {
            path.parent()
                .is_some_and(|dir| baseline.folders.contains_key(dir))
        })
        .map(|(path, read)| (path.clone(), read.clone()))
        .collect();
    let kept: Vec<&PathBuf> = repo
        .git_dirs
        .iter()
        .filter(|dir| baseline.folders.contains_key(*dir))
        .collect();
    for dir in &repo.git_dirs {
        if baseline.folders.contains_key(dir) {
            continue;
        }
        let commondir = cur
            .folders
            .get(dir)
            .and_then(|folder| folder.entries.get(OsStr::new("commondir")))
            .filter(|entry| entry.kind == Kind::File && entry.nlink == 1)
            .filter(|_| {
                read_small(&dir.join("commondir"))
                    .and_then(|common| plain_commondir(dir, &common))
                    .is_some_and(|common| kept.contains(&&common))
            });
        let entries = commondir
            .map(|entry| (OsString::from("commondir"), entry.clone()))
            .into_iter()
            .collect();
        baseline.folders.insert(
            dir.clone(),
            Folder {
                dev: 0,
                ino: 0,
                born: None,
                ctime: 0,
                settled: false,
                role: Role::GitDir,
                entries,
            },
        );
    }
    baseline
}

/// The folder a `commondir` names, as `git worktree add` writes it (`../..`
/// or a real path), without following links: git follows its text each
/// time it runs, where a link could lead elsewhere later.
fn plain_commondir(dir: &Path, text: &str) -> Option<PathBuf> {
    let text = text.trim_end_matches(['\n', '\r']);
    if text == "../.." {
        return dir.parent()?.parent().map(Path::to_path_buf);
    }
    let path = Path::new(text);
    (path.is_absolute() && std::fs::canonicalize(path).ok()? == path).then(|| path.to_path_buf())
}

/// What any session (a sub-agent's too) found in the git folders `watched`
/// that is still there, and their incomplete looks: while there is any, a
/// person's approval doesn't lift the protected-file rules.
pub(super) fn held_for(watched: &[PathBuf]) -> Vec<String> {
    let shares = |git_dirs: &Vec<PathBuf>| git_dirs.iter().any(|dir| watched.contains(dir));
    let (held, incomplete) = with_global(|global| {
        (
            global
                .held
                .iter()
                .filter(|held| shares(&held.git_dirs))
                .cloned()
                .collect::<Vec<_>>(),
            global
                .incomplete
                .keys()
                .filter(|git_dirs| shares(git_dirs))
                .cloned()
                .collect::<Vec<_>>(),
        )
    });
    let (standing, gone): (Vec<Held>, Vec<Held>) = held.into_iter().partition(Held::stands);
    with_global(|global| {
        for held in &gone {
            global.held.remove(held);
        }
    });
    let mut out: Vec<String> = standing
        .iter()
        .map(|held| shown_path(held.path()).0)
        .collect();
    out.sort();
    out.extend(incomplete.iter().filter_map(|git_dirs| {
        git_dirs
            .first()
            .map(|dir| format!("{} (not checked fully)", shown_path(dir).0))
    }));
    out
}

/// Characters that could disguise shown text: controls and bidirectional
/// overrides.
fn disguising(c: char) -> bool {
    c.is_control()
        || matches!(
            c,
            '\u{200e}' | '\u{200f}' | '\u{061c}' | '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}'
        )
}

/// Shown text, and whether it is exactly the original.
fn shown(text: &str) -> (String, bool) {
    let mut exact = true;
    let mut out: String = text
        .chars()
        .map(|c| {
            if disguising(c) {
                exact = false;
                '?'
            } else {
                c
            }
        })
        .take(SHOWN_CHARS)
        .collect();
    if text.chars().count() > SHOWN_CHARS {
        exact = false;
        out.push('…');
    }
    (out, exact)
}

fn shown_path(path: &Path) -> (String, bool) {
    match path.to_str() {
        Some(text) => shown(text),
        None => (shown(&path.to_string_lossy()).0, false),
    }
}

/// How to remove `path` (`rm` plus `flags`), or what to do where its name
/// can't be shown exactly.
fn removal(flags: &str, path: &Path) -> String {
    match shown_path(path) {
        (text, true) => format!("rm {flags}'{}'", text.replace('\'', r"'\''")),
        (_, false) => "remove it by hand (its name has characters that aren't shown)".to_string(),
    }
}

pub(super) fn report(findings: &[Finding], level: &str) -> Option<String> {
    if findings.is_empty() {
        return None;
    }
    let mut lines = vec![format!(
        "Security level {level}: agent commands could write this project's git folder, and \
         Corbanu found things there that your own git would follow outside the sandbox. \
         Nothing was removed. Check them, and remove what you didn't make:"
    )];
    for finding in findings.iter().take(SHOWN_FINDINGS) {
        lines.push(match finding {
            Finding::Link { path, target } => format!(
                "- {} is a link to {}: git writes through it (for example on your next commit, \
                 fetch or pull). Remove the link (its target stays): {}",
                shown_path(path).0,
                target.as_deref().map_or_else(
                    || "a place that can't be resolved".to_string(),
                    |target| shown_path(target).0
                ),
                removal("", path)
            ),
            Finding::HardLink { path } => format!(
                "- {} is a hard link: the same file has another name, maybe outside the project, \
                 and git writes into it. Remove this name: {}",
                shown_path(path).0,
                removal("", path)
            ),
            Finding::Exec { todo, lines } => {
                let state = todo.parent().unwrap_or(todo);
                let continues = if state.ends_with("sequencer") {
                    "git cherry-pick --continue or git revert --continue"
                } else {
                    "git rebase --continue"
                };
                let mut shown_lines: Vec<String> = lines
                    .iter()
                    .take(SHOWN_EXEC_LINES)
                    .map(|line| format!("\"{}\"", shown(line).0))
                    .collect();
                if lines.len() > SHOWN_EXEC_LINES {
                    shown_lines.push(format!("and {} more", lines.len() - SHOWN_EXEC_LINES));
                }
                format!(
                    "- {} holds steps that run commands outside the sandbox if you continue \
                     ({continues}): {}. If you didn't start this yourself, remove it without \
                     continuing: {}",
                    shown_path(state).0,
                    shown_lines.join(", "),
                    removal("-r ", state)
                )
            }
            Finding::Remote { path } => format!(
                "- {} defines a git remote, which git fetch, pull and push use for a remote with \
                 no URL in your git config. Remove it: {}",
                shown_path(path).0,
                removal("", path)
            ),
            Finding::Config { path } => format!(
                "- {} is new: git reads it for config or hooks, which can run commands. If you \
                 didn't add it, remove it: {}",
                shown_path(path).0,
                removal(
                    if std::fs::symlink_metadata(path).is_ok_and(|meta| meta.is_dir()) {
                        "-r "
                    } else {
                        ""
                    },
                    path
                )
            ),
            Finding::Replaced { dot_git } => format!(
                "- {} was replaced. If you didn't do this, check its hooks folder and config \
                 before you run git in this project.",
                shown_path(dot_git).0
            ),
            Finding::Incomplete { dot_git, why } => format!(
                "- {} couldn't be checked fully: {}. Look for links in it before you run git in \
                 this project.",
                shown_path(dot_git).0,
                shown(why).0
            ),
        });
    }
    if findings.len() > SHOWN_FINDINGS {
        lines.push(format!("- and {} more.", findings.len() - SHOWN_FINDINGS));
    }
    lines.push(format!(
        "Until these are removed (a replaced git folder: until it changes again or Corbanu \
         restarts), approving a command in this project doesn't lift {level}'s protected-file \
         rules."
    ));
    Some(lines.join("\n"))
}
