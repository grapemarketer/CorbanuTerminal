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
/// Files git writes through: a link or hard link there makes git write
/// elsewhere.
const WRITTEN_FILES: [&str; 8] = [
    "COMMIT_EDITMSG",
    "MERGE_MSG",
    "SQUASH_MSG",
    "TAG_EDITMSG",
    "FETCH_HEAD",
    "ORIG_HEAD",
    "MERGE_HEAD",
    "packed-refs",
];
/// Nested repositories followed this deep (`modules` within `modules`, a
/// `commondir` within a `gitdir`, ...).
const MAX_DEPTH: usize = 6;
/// Entries listed in one folder of a repository before the rest counts as
/// changed (`truncated`).
const MAX_ENTRIES: usize = 4096;

/// Whether `dir` is a git folder by git's own rule (`is_git_directory`): a
/// valid `HEAD`, and `objects` and `refs` either in `dir` or in the folder
/// its `commondir` names. A `HEAD` that can't be read counts as valid.
pub(crate) fn is_git_directory(dir: &Path) -> bool {
    let common = match std::fs::read(dir.join("commondir")) {
        Ok(bytes) => resolve(dir, String::from_utf8_lossy(&bytes).trim()),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => dir.to_path_buf(),
        // Unreadable: git may read it.
        Err(_) => return std::fs::symlink_metadata(dir.join("HEAD")).is_ok(),
    };
    exists(&common.join("objects")) && exists(&common.join("refs")) && valid_head(&dir.join("HEAD"))
}

fn exists(path: &Path) -> bool {
    std::fs::metadata(path).is_ok()
}

fn valid_head(head: &Path) -> bool {
    let Ok(metadata) = std::fs::symlink_metadata(head) else {
        return false;
    };
    if metadata.file_type().is_symlink() {
        // Git accepts a link into `refs/`; anything it can't read counts.
        return std::fs::read_link(head).map_or(true, |target| target.starts_with("refs"));
    }
    let Ok(bytes) = std::fs::read(head) else {
        return true;
    };
    let text = String::from_utf8_lossy(&bytes);
    let text = text.trim_end();
    if let Some(rest) = text.strip_prefix("ref:") {
        return rest.trim_start().starts_with("refs/");
    }
    matches!(text.len(), 40 | 64) && text.bytes().all(|b| b.is_ascii_hexdigit())
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
    match std::fs::symlink_metadata(entry) {
        Ok(metadata) if metadata.is_dir() => {
            git_folder(entry, "", roots, MAX_DEPTH, &mut prints);
        }
        Ok(metadata) if metadata.is_file() => {
            prints.insert(".git".to_string(), file_print(entry));
            if let Some(target) = gitdir_target(entry)
                && in_roots(&target, roots)
            {
                git_folder(&target, "gitdir/", roots, MAX_DEPTH, &mut prints);
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

/// The folder a `.git` file's `gitdir:` line names.
fn gitdir_target(file: &Path) -> Option<PathBuf> {
    let bytes = std::fs::read(file).ok()?;
    let text = String::from_utf8_lossy(&bytes);
    let target = text.lines().next()?.strip_prefix("gitdir:")?.trim();
    Some(resolve(file.parent()?, target))
}

fn in_roots(path: &Path, roots: &[PathBuf]) -> bool {
    let path = dunce::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    roots.iter().any(|root| is_at_or_below(&path, root))
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

fn git_folder(dir: &Path, prefix: &str, roots: &[PathBuf], depth: usize, prints: &mut Prints) {
    if depth == 0 {
        prints.insert(format!("{prefix}*"), "too deep".to_string());
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
    for name in WRITTEN_FILES {
        if let Some(mark) = write_through_mark(&dir.join(name)) {
            prints.insert(format!("{prefix}{name}"), mark);
        }
    }
    logs_prints(&dir.join("logs"), &format!("{prefix}logs/"), prints);
    // Other repositories git reads from here.
    for (folder, own) in [("modules", true), ("worktrees", false)] {
        let Ok(entries) = std::fs::read_dir(dir.join(folder)) else {
            continue;
        };
        for entry in entries.take(MAX_ENTRIES).flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            let path = entry.path();
            let child = format!("{prefix}{folder}/{name}/");
            if entry.file_type().is_ok_and(|kind| kind.is_symlink()) {
                prints.insert(child, "link".to_string());
            } else if own {
                // `modules\a\b` holds the module `a/b` when `a` isn't one.
                git_folder(&path, &child, roots, depth - 1, prints);
            } else {
                for name in PROGRAM_FILES {
                    let file = path.join(name);
                    if std::fs::symlink_metadata(&file).is_ok() {
                        prints.insert(format!("{child}{name}"), file_print(&file));
                    }
                }
            }
        }
    }
    // The folder `commondir` names holds the config and hooks git reads.
    if let Ok(bytes) = std::fs::read(dir.join("commondir")) {
        let common = resolve(dir, String::from_utf8_lossy(&bytes).trim());
        if in_roots(&common, roots) {
            git_folder(
                &common,
                &format!("{prefix}commondir/"),
                roots,
                depth - 1,
                prints,
            );
        }
    }
}

/// Each entry of `dir` (not below it, but a subfolder counts as `dir`).
fn folder_prints(dir: &Path, prefix: &str, prints: &mut Prints) {
    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return,
        Err(_) => {
            prints.insert(prefix.to_string(), "unreadable".to_string());
            return;
        }
    };
    let mut count = 0;
    for entry in entries {
        count += 1;
        if count > MAX_ENTRIES {
            prints.insert(prefix.to_string(), "truncated".to_string());
            return;
        }
        let Ok(entry) = entry else {
            prints.insert(prefix.to_string(), "unreadable".to_string());
            return;
        };
        let name = entry.file_name().to_string_lossy().into_owned();
        prints.insert(format!("{prefix}{name}"), file_print(&entry.path()));
    }
}

/// Links and hard links below `logs`, which git appends to.
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
                prints.insert(prefix, "truncated".to_string());
                return;
            }
            let name = entry.file_name().to_string_lossy().into_owned();
            let path = entry.path();
            match entry.file_type() {
                Ok(kind) if kind.is_dir() => stack.push((path, format!("{prefix}{name}/"))),
                _ => {
                    if let Some(mark) = write_through_mark(&path) {
                        prints.insert(format!("{prefix}{name}"), mark);
                    }
                }
            }
        }
    }
}

/// `Some` for a file git writes through that is a link or hard link.
fn write_through_mark(path: &Path) -> Option<String> {
    let metadata = std::fs::symlink_metadata(path).ok()?;
    if metadata.file_type().is_symlink() {
        return Some(format!(
            "link to {}",
            std::fs::read_link(path).map_or_else(|_| "?".to_string(), |t| t.display().to_string())
        ));
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

/// A file's content hash, `dir`, `link to <target>` or `unreadable`.
fn file_print(path: &Path) -> String {
    let Ok(metadata) = std::fs::symlink_metadata(path) else {
        return "unreadable".to_string();
    };
    if metadata.file_type().is_symlink() {
        return format!(
            "link to {}",
            std::fs::read_link(path).map_or_else(|_| "?".to_string(), |t| t.display().to_string())
        );
    }
    if metadata.is_dir() {
        return "dir".to_string();
    }
    let Ok(mut file) = std::fs::File::open(path) else {
        return "unreadable".to_string();
    };
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
    let Ok(config) = std::fs::read(entry.join("config")) else {
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
        if line.ends_with('\\') || line.to_ascii_lowercase().contains("ext::") {
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
mod tests;
