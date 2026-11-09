//! D420/D421 (#1775): what git would run from a repository below the top of
//! a non-admin session's writable roots, so a change a sandboxed command
//! makes there can be told apart from git's own bookkeeping.
//!
//! Git reads a repository's config and runs its hooks with the person's
//! full rights whenever they (or an editor) run git there. [`fingerprint`]
//! records, by content, the small files git reads programs from (`config`,
//! `config.worktree`, `commondir`, `gitdir`, `hooks\*`, rebase and sequencer
//! steps, the legacy `remotes`/`branches` definitions), whether the files
//! git writes through (`COMMIT_EDITMSG`, `logs\*`, ...) are links or hard
//! links, and the same for the repositories in `modules\*` and
//! `worktrees\*` and for the folders a `gitdir:` line or `commondir` names
//! inside the watched roots. Rewriting a file with the same content is not a
//! change. [`is_plain`] (D421) tells a repository with exactly the shape
//! `git init` or `git clone` leave, which is recorded but not asked about.
//!
//! Plain `std::fs`, so it is tested on every platform; only the non-admin
//! Windows sandbox uses it.

use sha2::Digest as _;
use sha2::Sha256;
use std::collections::BTreeMap;
use std::io::Read as _;
use std::path::Path;
use std::path::PathBuf;

/// What git would run from a repository: a name relative to its git folder
/// (or `gitdir/`, `commondir/`, `modules/<name>/` ... for the folders it
/// names) and what is there.
pub(crate) type Prints = BTreeMap<String, String>;

/// Files git reads programs or other repositories from.
const PROGRAM_FILES: [&str; 4] = ["config", "config.worktree", "commondir", "gitdir"];
/// Folders whose files git runs or reads steps from.
const PROGRAM_FOLDERS: [&str; 6] = [
    "hooks",
    "rebase-merge",
    "rebase-apply",
    "sequencer",
    "remotes",
    "branches",
];
/// Nested repositories followed this deep (`modules` within `modules`, a
/// `commondir` within a `gitdir`, ...).
const MAX_DEPTH: usize = 6;
/// Entries looked at in one folder of a repository; past that the folder
/// counts as changed after every session.
const MAX_ENTRIES: usize = 20_000;

/// Whether `dir` is a git folder by git's own rule (`is_git_directory`): a
/// valid `HEAD`, and `objects` and `refs` either in `dir` or in the folder
/// its `commondir` names. A `HEAD` that can't be read counts as valid.
pub(crate) fn is_git_directory(dir: &Path) -> bool {
    let common = match std::fs::read(dir.join("commondir")) {
        Ok(bytes) => resolve(dir, &file_value(&bytes)),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => dir.to_path_buf(),
        // Unreadable: git may read it.
        Err(_) => return std::fs::symlink_metadata(dir.join("HEAD")).is_ok(),
    };
    // Never looked at from here (a network share could collect the
    // person's credentials): it counts as a repository.
    if is_remote_or_device(&common) {
        return std::fs::symlink_metadata(dir.join("HEAD")).is_ok();
    }
    exists(&common.join("objects")) && exists(&common.join("refs")) && valid_head(&dir.join("HEAD"))
}

fn exists(path: &Path) -> bool {
    std::fs::metadata(path).is_ok()
}

/// Git's `validate_headref`, leniently: any link (git reads through it),
/// `ref: refs/...`, or a value that starts with 40 or 64 hex digits
/// (git ignores what follows).
fn valid_head(head: &Path) -> bool {
    let Ok(metadata) = std::fs::symlink_metadata(head) else {
        return false;
    };
    if metadata.file_type().is_symlink() {
        return true;
    }
    // Read without following (#1909): one swapped in since is a link.
    let Some(bytes) = read_unfollowed(head) else {
        return true;
    };
    if let Some(rest) = bytes.strip_prefix(b"ref:") {
        return rest.trim_ascii_start().starts_with(b"refs/");
    }
    let hex = bytes.iter().take_while(|b| b.is_ascii_hexdigit()).count();
    hex >= 40
}

/// The path in a `commondir` file as git reads it: the whole file, with
/// trailing line ends removed.
fn file_value(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes)
        .trim_end_matches(['\n', '\r'])
        .to_string()
}

/// `target` (a `gitdir:` or `commondir` value) relative to `base`.
fn resolve(base: &Path, target: &str) -> PathBuf {
    let target = Path::new(target);
    if target.is_absolute() {
        target.to_path_buf()
    } else {
        base.join(target)
    }
}

/// What git would run from the repository at `entry`: a `.git` folder or a
/// bare repository's folder (its git folder), or a `.git` file (its
/// `gitdir:` line, and the folder it names). Folders named by a `gitdir:`
/// line or `commondir` are only looked into when they lie in one of `roots`
/// (canonical; elsewhere commands can't write). A link `entry` isn't
/// followed (the caller counts it as changed every time).
pub(crate) fn fingerprint(entry: &Path, roots: &[PathBuf]) -> Prints {
    let mut prints = Prints::new();
    let mut walk = Walk::default();
    match std::fs::symlink_metadata(entry) {
        Ok(metadata) if metadata.is_dir() && !metadata.file_type().is_symlink() => {
            git_folder(
                entry,
                "",
                roots,
                MAX_DEPTH,
                /*follow_common*/ true,
                &mut walk,
                &mut prints,
            );
        }
        Ok(metadata) if metadata.is_file() => {
            prints.insert(".git".to_string(), file_print(entry));
            if let Some(target) = gitdir_target(entry)
                && in_roots(&target, roots)
            {
                git_folder(
                    &target,
                    "gitdir/",
                    roots,
                    MAX_DEPTH,
                    /*follow_common*/ true,
                    &mut walk,
                    &mut prints,
                );
            }
        }
        Ok(_) => {
            prints.insert(".git".to_string(), "link".to_string());
        }
        Err(_) => {
            prints.insert(".git".to_string(), "unreadable".to_string());
        }
    }
    prints
}

/// The git folders one fingerprint has looked at, and how many more it may.
struct Walk {
    /// Each folder with whether its `commondir` was followed.
    visited: std::collections::HashSet<(PathBuf, bool)>,
    budget: usize,
}

impl Default for Walk {
    fn default() -> Self {
        Self {
            visited: std::collections::HashSet::new(),
            budget: MAX_GIT_FOLDERS,
        }
    }
}

/// Git folders (modules, worktrees, `commondir`s ...) one fingerprint looks
/// at; past that it counts as changed after every session.
const MAX_GIT_FOLDERS: usize = 2_000;

/// The folder a `.git` file names, as git reads it (`read_gitfile`): the
/// whole file must start with `gitdir: `, and trailing line ends are
/// removed.
pub(crate) fn gitdir_target(file: &Path) -> Option<PathBuf> {
    let bytes = std::fs::read(file).ok()?;
    let text = String::from_utf8_lossy(&bytes);
    let target = text
        .strip_prefix("gitdir: ")?
        .trim_end_matches(['\n', '\r']);
    Some(resolve(file.parent()?, target))
}

fn in_roots(path: &Path, roots: &[PathBuf]) -> bool {
    // A path a command wrote (a `gitdir:` line, a `commondir`) is only
    // resolved on this machine's drives.
    if is_remote_or_device(path) {
        return false;
    }
    let path = dunce::canonicalize(path).unwrap_or_else(|_| normalize(path));
    roots.iter().any(|root| is_at_or_below(&path, root))
}

/// A network (`\\server\share`, `//server/share`, `\??\UNC\...`) or device
/// (`\\.\`, `\\?\`, `\??\`, or a component named `CON`, `NUL`, `COM1` ...)
/// path: one taken from a file a command could write is never opened, since
/// opening a share can send the person's credentials to it and opening a
/// device can block.
pub(crate) fn is_remote_or_device(path: &Path) -> bool {
    const DEVICES: [&str; 8] = [
        "CON", "PRN", "AUX", "NUL", "CONIN$", "CONOUT$", "CLOCK$", "CONFIG$",
    ];
    let text = path.as_os_str().to_string_lossy();
    let prefixed = ["\\\\", "//", "\\/", "/\\", "\\??\\", "/??/"]
        .iter()
        .any(|prefix| text.starts_with(prefix));
    // Split on both separators, as Windows does.
    prefixed
        || text.split(['\\', '/']).any(|name| {
            let stem = name
                .split('.')
                .next()
                .unwrap_or_default()
                .trim_end_matches([' ', ':'])
                .to_ascii_uppercase();
            DEVICES.contains(&stem.as_str())
                || ((stem.starts_with("COM") || stem.starts_with("LPT"))
                    && stem.len() == 4
                    && stem.as_bytes()[3].is_ascii_digit())
        })
}

/// `path` with `.` and `..` resolved by name (as Windows resolves them), so
/// `root\a\..\..\x` doesn't count as inside `root`.
pub(crate) fn normalize(path: &Path) -> PathBuf {
    let mut normal = PathBuf::new();
    for part in path.components() {
        match part {
            std::path::Component::CurDir => {}
            std::path::Component::ParentDir => {
                if !normal.pop() {
                    normal.push(part);
                }
            }
            other => normal.push(other),
        }
    }
    normal
}

/// `path` is `base` or below it, ignoring ASCII case (as Windows compares
/// these names).
pub(crate) fn is_at_or_below(path: &Path, base: &Path) -> bool {
    let mut parts = path.components();
    base.components().all(|part| {
        parts.next().is_some_and(|other| {
            other
                .as_os_str()
                .to_string_lossy()
                .eq_ignore_ascii_case(&part.as_os_str().to_string_lossy())
        })
    })
}

/// A marker that differs every time: a folder too big to look through
/// counts as changed after every session.
pub(crate) fn truncated() -> String {
    static COUNT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    format!(
        "truncated at {:?} #{}",
        std::time::SystemTime::now(),
        COUNT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    )
}

fn git_folder(
    dir: &Path,
    prefix: &str,
    roots: &[PathBuf],
    depth: usize,
    follow_common: bool,
    walk: &mut Walk,
    prints: &mut Prints,
) {
    if depth == 0 || walk.budget == 0 {
        prints.insert(format!("{prefix}*"), truncated());
        return;
    }
    walk.budget -= 1;
    // Each folder once (a `commondir` or module can lead back to one).
    let key = dunce::canonicalize(dir).unwrap_or_else(|_| normalize(dir));
    if !walk.visited.insert((key, follow_common)) {
        prints.insert(format!("{prefix}(seen)"), "seen".to_string());
        return;
    }
    for name in PROGRAM_FILES {
        let path = dir.join(name);
        if std::fs::symlink_metadata(&path).is_ok() {
            prints.insert(format!("{prefix}{name}"), file_print(&path));
        }
    }
    for name in PROGRAM_FOLDERS {
        folder_prints(&dir.join(name), &format!("{prefix}{name}/"), prints);
    }
    // Every other file at the top (`COMMIT_EDITMSG`, `FETCH_HEAD`, ...),
    // which git writes through, and every folder there that is a link
    // (`objects`, `refs`, `logs`, `rr-cache` ...), as D6 does.
    top_level_links(dir, prefix, prints);
    logs_prints(&dir.join("logs"), &format!("{prefix}logs/"), prints);
    // Other repositories git reads from here: each module is a git folder
    // (`modules\a\b` holds the module `a/b`), and so is each worktree's own
    // folder (its `commondir` is this one).
    for (folder, follow) in [("modules", true), ("worktrees", false)] {
        container(
            &dir.join(folder),
            &format!("{prefix}{folder}/"),
            roots,
            depth - 1,
            follow,
            walk,
            prints,
        );
    }
    // The folder `commondir` names holds the config and hooks git reads.
    if follow_common && let Ok(bytes) = std::fs::read(dir.join("commondir")) {
        let common = resolve(dir, &file_value(&bytes));
        if in_roots(&common, roots) {
            git_folder(
                &common,
                &format!("{prefix}commondir/"),
                roots,
                depth - 1,
                /*follow_common*/ true,
                walk,
                prints,
            );
        }
    }
}

/// A module (or worktree) folder at `path`: fingerprinted as a git folder,
/// and its other subfolders likewise, since a module `a/b` lives in
/// `modules\a\b` whether or not `a` is one. Even an empty folder is
/// recorded, so one added is a change.
fn module_tree(
    path: &Path,
    prefix: &str,
    roots: &[PathBuf],
    depth: usize,
    follow_common: bool,
    walk: &mut Walk,
    prints: &mut Prints,
) {
    prints.insert(prefix.to_string(), file_print(path));
    git_folder(path, prefix, roots, depth, follow_common, walk, prints);
    let Ok(children) = std::fs::read_dir(path) else {
        return;
    };
    // A git folder's own subfolders are its, not modules; in any other
    // folder every subfolder may be one (a module `vendor/refs`).
    let is_git = is_git_directory(path);
    for child in children.flatten() {
        let name = child.file_name().to_string_lossy().into_owned();
        let known = is_git
            && GIT_SUBFOLDERS
                .iter()
                .any(|known| known.eq_ignore_ascii_case(&name));
        if known || !std::fs::metadata(child.path()).is_ok_and(|metadata| metadata.is_dir()) {
            continue;
        }
        if depth <= 1 || walk.budget == 0 {
            prints.insert(format!("{prefix}(more)"), truncated());
            return;
        }
        module_tree(
            &child.path(),
            &format!("{prefix}{name}/"),
            roots,
            depth - 1,
            follow_common,
            walk,
            prints,
        );
    }
}

/// The folders git reads from inside a git folder's subfolders.
const GIT_SUBFOLDERS: [&str; 15] = [
    "hooks",
    "objects",
    "refs",
    "logs",
    "info",
    "rebase-merge",
    "rebase-apply",
    "sequencer",
    "remotes",
    "branches",
    "modules",
    "worktrees",
    "lfs",
    "rr-cache",
    "annex",
];

/// `modules` or `worktrees` (or a folder below `modules` on the way to a
/// module named with slashes): each subfolder is fingerprinted as a git
/// folder, and its other subfolders are looked into the same way, since a
/// module `a/b` lives in `modules\a\b`. A link here is recorded, and
/// followed.
fn container(
    dir: &Path,
    prefix: &str,
    roots: &[PathBuf],
    depth: usize,
    follow_common: bool,
    walk: &mut Walk,
    prints: &mut Prints,
) {
    match std::fs::symlink_metadata(dir) {
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return,
        Ok(metadata) if metadata.file_type().is_symlink() => {
            prints.insert(format!("{prefix}(link)"), file_print(dir));
        }
        _ => {}
    }
    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(_) => {
            prints.insert(prefix.to_string(), "unreadable".to_string());
            return;
        }
    };
    for (count, entry) in entries.enumerate() {
        if count >= MAX_ENTRIES || walk.budget == 0 {
            prints.insert(prefix.to_string(), truncated());
            break;
        }
        let Ok(entry) = entry else {
            prints.insert(prefix.to_string(), "unreadable".to_string());
            break;
        };
        let name = entry.file_name().to_string_lossy().into_owned();
        let child = format!("{prefix}{name}/");
        let path = entry.path();
        // A link to a folder counts as a folder here (git follows it).
        if !std::fs::metadata(&path).is_ok_and(|metadata| metadata.is_dir()) {
            prints.insert(child, file_print(&path));
            continue;
        }
        module_tree(&path, &child, roots, depth, follow_common, walk, prints);
    }
}

/// Links and hard links among the entries at the top of the git folder
/// `dir` that aren't recorded by content already.
fn top_level_links(dir: &Path, prefix: &str, prints: &mut Prints) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for (count, entry) in entries.enumerate() {
        if count >= MAX_ENTRIES {
            prints.insert(format!("{prefix}./"), truncated());
            return;
        }
        let Ok(entry) = entry else {
            prints.insert(format!("{prefix}./"), "unreadable".to_string());
            return;
        };
        let name = entry.file_name().to_string_lossy().into_owned();
        let known = PROGRAM_FILES
            .iter()
            .chain(PROGRAM_FOLDERS.iter())
            .chain(["modules", "worktrees"].iter())
            .any(|known| known.eq_ignore_ascii_case(&name));
        if known {
            continue;
        }
        if let Some(mark) = link_mark(&entry.path()) {
            prints.insert(format!("{prefix}{name}"), mark);
        }
    }
}

/// Each entry of `dir` (not below it, but a subfolder counts as `dir`); a
/// `dir` that is itself a link is recorded as one, not followed.
fn folder_prints(dir: &Path, prefix: &str, prints: &mut Prints) {
    match std::fs::symlink_metadata(dir) {
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return,
        // Recorded as a link, and its contents through it (git follows it).
        Ok(metadata) if metadata.file_type().is_symlink() => {
            prints.insert(prefix.to_string(), file_print(dir));
        }
        _ => {}
    }
    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return,
        Err(_) => {
            prints.insert(prefix.to_string(), "unreadable".to_string());
            return;
        }
    };
    for (count, entry) in entries.enumerate() {
        if count >= MAX_ENTRIES {
            prints.insert(prefix.to_string(), truncated());
            return;
        }
        let Ok(entry) = entry else {
            prints.insert(prefix.to_string(), "unreadable".to_string());
            return;
        };
        let name = entry.file_name().to_string_lossy().into_owned();
        // Git never runs a `*.sample` hook, so its name is enough (one
        // renamed to a hook's name is a new entry).
        let print = if name.to_ascii_lowercase().ends_with(".sample")
            && entry.file_type().is_ok_and(|kind| kind.is_file())
        {
            "sample".to_string()
        } else {
            file_print(&entry.path())
        };
        prints.insert(format!("{prefix}{name}"), print);
    }
}

/// Links below `logs`, which git appends to. (A hard link needs write
/// access to the file it names, which a command could write directly, so
/// only the top-level files are checked for those.)
fn logs_prints(dir: &Path, prefix: &str, prints: &mut Prints) {
    let mut stack = vec![(dir.to_path_buf(), prefix.to_string())];
    let mut seen = 0;
    while let Some((dir, prefix)) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            seen += 1;
            if seen > MAX_ENTRIES {
                prints.insert(prefix, truncated());
                return;
            }
            let name = entry.file_name().to_string_lossy().into_owned();
            match entry.file_type() {
                Ok(kind) if kind.is_symlink() => {
                    prints.insert(format!("{prefix}{name}"), file_print(&entry.path()));
                }
                Ok(kind) if kind.is_dir() => {
                    stack.push((entry.path(), format!("{prefix}{name}/")));
                }
                _ => {}
            }
        }
    }
}

/// `Some` for a link, or a file with more than one name (a hard link).
fn link_mark(path: &Path) -> Option<String> {
    let metadata = std::fs::symlink_metadata(path).ok()?;
    if metadata.file_type().is_symlink() {
        return Some(file_print(path));
    }
    (metadata.is_file() && link_count(path).is_none_or(|count| count > 1))
        .then(|| "hard link".to_string())
}

#[cfg(unix)]
fn link_count(path: &Path) -> Option<u64> {
    use std::os::unix::fs::MetadataExt as _;
    std::fs::symlink_metadata(path)
        .ok()
        .map(|metadata| metadata.nlink())
}

#[cfg(windows)]
fn link_count(path: &Path) -> Option<u64> {
    crate::acl::link_count(path).map(u64::from)
}

#[cfg(not(any(unix, windows)))]
fn link_count(_path: &Path) -> Option<u64> {
    Some(1)
}

/// A file's content hash (with `hard link` when it has more than one
/// name), `dir`, `link to <target>` or `unreadable`.
pub(crate) fn file_print(path: &Path) -> String {
    let Ok(metadata) = std::fs::symlink_metadata(path) else {
        return "unreadable".to_string();
    };
    if metadata.file_type().is_symlink() {
        // The link, and what git reads through it.
        let target =
            std::fs::read_link(path).map_or_else(|_| "?".to_string(), |t| t.display().to_string());
        let through = match std::fs::metadata(path) {
            Ok(target) if target.is_file() => content_print(path),
            Ok(_) => "dir".to_string(),
            Err(_) => "missing".to_string(),
        };
        return format!("link to {target}: {through}");
    }
    if metadata.is_dir() {
        return "dir".to_string();
    }
    unfollowed_print(path)
}

/// #1909: [`file_print`], but a link is recorded by its target only and
/// never read through: `link to <target>`.
pub(crate) fn unfollowed_file_print(path: &Path) -> String {
    let Ok(metadata) = std::fs::symlink_metadata(path) else {
        return "unreadable".to_string();
    };
    if metadata.file_type().is_symlink() {
        let target =
            std::fs::read_link(path).map_or_else(|_| "?".to_string(), |t| t.display().to_string());
        return format!("link to {target}");
    }
    if metadata.is_dir() {
        return "dir".to_string();
    }
    unfollowed_print(path)
}

/// The content hash of the file at `path` itself (with ` hard link` when
/// it has more than one name), or `unreadable`, also when it has become a
/// link or a folder since the caller looked (#1909): a process a command
/// left running can't steer the read through a link it swaps in.
fn unfollowed_print(path: &Path) -> String {
    #[cfg(test)]
    tests::before_read(path);
    let Some((file, links)) = open_unfollowed(path) else {
        return "unreadable".to_string();
    };
    let mut text = hash(file);
    if links.is_none_or(|count| count > 1) {
        text.push_str(" hard link");
    }
    text
}

/// The file at `path` itself, not followed if it is a link, with how many
/// names it has.
#[cfg(windows)]
fn open_unfollowed(path: &Path) -> Option<(std::fs::File, Option<u64>)> {
    crate::acl::open_unfollowed(path).map(|(file, links)| (file, Some(u64::from(links))))
}

/// Elsewhere (tests only) a link is refused as it is found; the open
/// itself follows.
#[cfg(not(windows))]
fn open_unfollowed(path: &Path) -> Option<(std::fs::File, Option<u64>)> {
    let metadata = std::fs::symlink_metadata(path).ok()?;
    if !metadata.is_file() {
        return None;
    }
    Some((std::fs::File::open(path).ok()?, link_count(path)))
}

/// The bytes of the file at `path` itself, `None` when it is a link or a
/// folder or can't be read (#1909).
fn read_unfollowed(path: &Path) -> Option<Vec<u8>> {
    let (mut file, _) = open_unfollowed(path)?;
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes).ok()?;
    Some(bytes)
}

/// The SHA-256 of the file at `path` (followed), or `unreadable`.
fn content_print(path: &Path) -> String {
    match std::fs::File::open(path) {
        Ok(file) => hash(file),
        Err(_) => "unreadable".to_string(),
    }
}

/// The SHA-256 of what `file` holds, or `unreadable`.
fn hash(mut file: std::fs::File) -> String {
    let mut hasher = Sha256::new();
    let mut buffer = [0_u8; 8192];
    loop {
        match file.read(&mut buffer) {
            Ok(0) => break,
            Ok(read) => hasher.update(&buffer[..read]),
            Err(_) => return "unreadable".to_string(),
        }
    }
    let digest = hasher.finalize();
    let mut text = String::with_capacity(64);
    for byte in digest {
        text.push_str(&format!("{byte:02x}"));
    }
    text
}

/// What changed between two fingerprints, for the person: `config changed`,
/// `hooks/post-checkout added`, `commondir removed` ...
pub(crate) fn changes(before: &Prints, after: &Prints) -> Vec<String> {
    let mut changes = Vec::new();
    for (name, print) in after {
        match before.get(name) {
            None => changes.push(format!("{name} added")),
            Some(old) if old != print => changes.push(format!("{name} changed")),
            Some(_) => {}
        }
    }
    for name in before.keys() {
        if !after.contains_key(name) {
            changes.push(format!("{name} removed"));
        }
    }
    changes
}

/// D421 (#1775): whether the new repository at `entry` (a `.git` folder or
/// a bare repository's folder) has exactly the shape `git init` or `git
/// clone` leave: a plain folder (not a link or `gitdir:` file), no
/// `commondir`, `config.worktree`, other repositories (`modules`,
/// `worktrees`) or legacy remote definitions, no links anywhere inside, no
/// rebase or sequencer state, hooks that are only `*.sample` files, and a
/// config holding only the keys those commands write, with no `ext::` URL.
/// Anything unreadable makes it not plain.
pub(crate) fn is_plain(entry: &Path) -> bool {
    let Ok(metadata) = std::fs::symlink_metadata(entry) else {
        return false;
    };
    if !metadata.is_dir() || metadata.file_type().is_symlink() {
        return false;
    }
    for name in [
        "commondir",
        "config.worktree",
        "gitdir",
        "rebase-merge",
        "rebase-apply",
        "sequencer",
        "modules",
        "worktrees",
        "remotes",
    ] {
        if !matches!(std::fs::symlink_metadata(entry.join(name)), Err(err) if err.kind() == std::io::ErrorKind::NotFound)
        {
            return false;
        }
    }
    // `git init` leaves `branches` empty on older versions.
    if !empty_or_absent(&entry.join("branches")) {
        return false;
    }
    let Ok(hooks) = list(&entry.join("hooks")) else {
        return false;
    };
    let only_samples = hooks
        .iter()
        .all(|(name, is_file)| *is_file && name.to_ascii_lowercase().ends_with(".sample"));
    if !only_samples {
        return false;
    }
    let Some(config) = read_unfollowed(&entry.join("config")) else {
        return false;
    };
    plain_config(&String::from_utf8_lossy(&config)) && no_links_inside(entry)
}

fn empty_or_absent(dir: &Path) -> bool {
    match std::fs::read_dir(dir) {
        Ok(mut entries) => entries.next().is_none(),
        Err(err) => err.kind() == std::io::ErrorKind::NotFound,
    }
}

/// The entries of `dir` and whether each is a plain file; absent is empty.
fn list(dir: &Path) -> std::io::Result<Vec<(String, bool)>> {
    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(err) => return Err(err),
    };
    entries
        .map(|entry| {
            let entry = entry?;
            let kind = entry.file_type()?;
            Ok((
                entry.file_name().to_string_lossy().into_owned(),
                kind.is_file() && !kind.is_symlink(),
            ))
        })
        .collect()
}

/// No link anywhere below `dir` (bounded: a repository too big to look
/// through isn't plain).
fn no_links_inside(root: &Path) -> bool {
    const MAX_WALKED: usize = 50_000;
    let mut stack = vec![root.to_path_buf()];
    let mut walked = 0;
    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            return false;
        };
        for entry in entries {
            walked += 1;
            let Ok(entry) = entry else {
                return false;
            };
            let Ok(kind) = entry.file_type() else {
                return false;
            };
            if walked > MAX_WALKED || kind.is_symlink() {
                return false;
            }
            let path = entry.path();
            if kind.is_dir() {
                stack.push(path);
                continue;
            }
            // A hard link (`git clone --local` makes them for objects, which
            // git only reads and replaces by renaming) outside `objects`
            // isn't plain.
            let in_objects = path.strip_prefix(root).is_ok_and(|relative| {
                relative
                    .components()
                    .next()
                    .is_some_and(|first| first.as_os_str().eq_ignore_ascii_case("objects"))
            });
            if !in_objects && link_count(&path).is_none_or(|count| count > 1) {
                return false;
            }
        }
    }
    true
}

/// D421: a config holding only the keys `git init`/`git clone` write.
pub(crate) fn plain_config(text: &str) -> bool {
    let mut section = String::new();
    for raw in text.lines() {
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') || line.starts_with(';') {
            continue;
        }
        // Continuation lines, and `ext::` URLs anywhere, are never plain.
        // Quotes are dropped first: git joins `"e"xt::` into `ext::`.
        if line.ends_with('\\') || line.replace('"', "").to_ascii_lowercase().contains("ext::") {
            return false;
        }
        let rest = if let Some(header) = line.strip_prefix('[') {
            let Some((header, rest)) = header.split_once(']') else {
                return false;
            };
            let Some(name) = section_name(header) else {
                return false;
            };
            section = name;
            rest.trim()
        } else {
            line
        };
        if rest.is_empty() || rest.starts_with('#') || rest.starts_with(';') {
            continue;
        }
        let key = rest
            .split_once('=')
            .map_or(rest, |(key, _)| key)
            .trim()
            .to_ascii_lowercase();
        if !key.chars().all(|c| c.is_ascii_alphanumeric() || c == '-') {
            return false;
        }
        let allowed: &[&str] = match section.as_str() {
            "core" => &[
                "repositoryformatversion",
                "filemode",
                "bare",
                "logallrefupdates",
                "symlinks",
                "ignorecase",
            ],
            "remote" => &["url", "fetch"],
            "branch" => &["remote", "merge"],
            _ => &[],
        };
        if !allowed.contains(&key.as_str()) {
            return false;
        }
    }
    true
}

/// The section of a `[section]` or `[section "sub"]` header (sub-sections
/// only for `remote` and `branch`), lower-cased; `None` for anything else.
fn section_name(header: &str) -> Option<String> {
    let header = header.trim();
    let (name, sub) = match header.split_once(|c: char| c.is_whitespace() || c == '.') {
        Some((name, sub)) => (name, Some(sub.trim())),
        None => (header, None),
    };
    let name = name.to_ascii_lowercase();
    match (name.as_str(), sub) {
        ("core", None) => Some(name),
        ("remote" | "branch", Some(sub)) if !sub.is_empty() => Some(name),
        _ => None,
    }
}

#[cfg(test)]
#[path = "git_watch_tests.rs"]
pub(crate) mod tests;
