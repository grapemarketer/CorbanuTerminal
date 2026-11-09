//! D283 (#1541): `.codex` and `.agents` folders below the top of a legacy
//! session's writable roots. Core loads them from every folder between a
//! project's root and its working directory, so a sandboxed command could
//! plant one deeper in a writable root, where D262 doesn't look.
//!
//! When a session starts, [`find`] lists the ones that exist; the session
//! protects them (and the folders above them) like the top-level ones and
//! records them with their file identity. When it ends, every one found then
//! that isn't in that list, unchanged, counts as planted ([`appeared`]), and
//! so does every folder the walk couldn't list. While it runs,
//! [`appeared_in_session`] tells Core the same about a single folder.
//!
//! #1896 item 3 (D425): an elevated session doesn't protect them, so it
//! also records what is inside each ([`with_contents`]); one whose contents
//! differ when the session ends counts as planted too.

use serde::Deserialize;
use serde::Serialize;
use std::os::windows::fs::MetadataExt as _;
use std::path::Path;
use std::path::PathBuf;

const PROJECT_METADATA_NAMES: [&str; 2] = [".codex", ".agents"];
const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x400;

/// A nested `.codex`/`.agents` entry and the file it was. `identity` is
/// `None` for a link (the sandbox may be able to point it elsewhere without
/// changing the link's identity) or when it can't be read; such an entry
/// never matches, so it counts as planted after every session.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct NestedMetadata {
    pub(crate) path: PathBuf,
    pub(crate) identity: Option<FileIdentity>,
    /// #1896 item 3: what was inside it ([`contents`]), for an elevated
    /// session (`None` otherwise: a legacy session keeps it unchanged).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) contents: Option<crate::git_watch::Prints>,
}

impl NestedMetadata {
    /// Whether `path`, the file `identity` now, is still this entry: the
    /// same file, with the same contents when they were recorded.
    fn unchanged(&self, path: &Path, identity: Option<FileIdentity>) -> bool {
        same_path(&self.path, path)
            && self.identity == identity
            && self
                .contents
                .as_ref()
                .is_none_or(|before| *before == contents(path))
    }
}

/// Volume serial number and 128-bit file ID.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct FileIdentity {
    volume: u64,
    id: [u8; 16],
}

/// What a walk of the writable roots found.
#[derive(Default)]
pub(crate) struct Found {
    pub(crate) entries: Vec<NestedMetadata>,
    /// Folders the walk couldn't list (or entries it couldn't inspect): what
    /// lies below them is unknown.
    pub(crate) unlistable: Vec<PathBuf>,
    /// D420 (#1775): the repositories below the top of the roots: each
    /// nested `.git` (folder, file or link) and each folder that is itself a
    /// git folder (a bare repository), by git's rule.
    pub(crate) repos: Vec<PathBuf>,
    /// D420: folder links the walk found (it doesn't follow them).
    pub(crate) links: Vec<PathBuf>,
    /// D420: folders that count as changed after every session, and why: a
    /// folder holding an entry whose name isn't valid Unicode (it can't be
    /// recorded, so it isn't looked at; one note per folder), or a link to a
    /// folder too large to look through.
    pub(crate) notes: Vec<(PathBuf, String)>,
    /// The walk stopped at its budget.
    exhausted: bool,
}

impl Found {
    fn is_empty(&self) -> bool {
        self.entries.is_empty()
            && self.unlistable.is_empty()
            && self.repos.is_empty()
            && self.links.is_empty()
            && self.notes.is_empty()
    }
}

/// The `.codex`/`.agents` entries below the top of `roots` (canonical), of
/// any kind (a link counts), recorded under the name Core opens. Links are
/// never followed; other reparse points, such as ProjFS folders or cloud
/// placeholders, are walked like plain folders (#1672). The walk doesn't descend into `.git` folders or into
/// the entries it reports: the sandbox can't write inside a protected one,
/// and Core doesn't run from inside `.git`. Names match the way NTFS
/// compares them (each character upper-cased), so `.agentſ` counts; such a
/// name never matches a snapshot.
///
/// #1877: the folders down to [`SPLIT_DEPTH`] are listed on several
/// threads, each deeper subtree by one thread as [`find_bounded`] walks it. Each part is put back in its place, so the result is the same,
/// in the same order, as one walk.
pub(crate) fn find(roots: &[PathBuf]) -> Found {
    // A folder's place: its root, then for each level its position counted
    // from the last folder listed (the walk takes the last one first).
    let parts = std::sync::Mutex::new(Vec::<(Vec<usize>, Found)>::new());
    let seed = roots
        .iter()
        .enumerate()
        .map(|(n, root)| (root.clone(), vec![n]))
        .collect();
    let walked = crate::acl::walk::par_walk(
        seed,
        crate::acl::walk::check_threads(),
        |(dir, place): (PathBuf, Vec<usize>), more| {
            let mut part = Found::default();
            let mut budget = usize::MAX;
            if place.len() <= SPLIT_DEPTH {
                let mut children = Vec::new();
                list(dir, place.len() == 1, &mut part, &mut budget, &mut children);
                let count = children.len();
                more.extend(children.into_iter().enumerate().map(|(n, child)| {
                    let mut place = place.clone();
                    place.push(count - 1 - n);
                    (child, place)
                }));
            } else {
                let mut stack = vec![dir];
                while let Some(dir) = stack.pop() {
                    list(dir, /*top*/ false, &mut part, &mut budget, &mut stack);
                }
            }
            if !part.is_empty() {
                parts
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .push((place, part));
            }
            Ok(())
        },
    );
    if walked.is_err() {
        // A panic: walk again on this thread, as before #1877.
        let mut budget = usize::MAX;
        return find_bounded(roots, &mut budget);
    }
    let mut parts = parts
        .into_inner()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    // A folder's own part comes before those below it.
    parts.sort_by(|a, b| a.0.cmp(&b.0));
    let mut found = Found::default();
    for (_, part) in parts {
        found.entries.extend(part.entries);
        found.unlistable.extend(part.unlistable);
        found.repos.extend(part.repos);
        found.links.extend(part.links);
        found.notes.extend(part.notes);
    }
    found
}

/// [`find`] lists each folder down to this depth (a root is 1) as its own
/// piece of work; each deeper subtree is walked whole by one thread.
const SPLIT_DEPTH: usize = 4;

/// [`find`], looking at no more than `budget` entries (it counts down).
fn find_bounded(roots: &[PathBuf], budget: &mut usize) -> Found {
    let mut found = Found::default();
    for root in roots {
        let mut stack = vec![root.clone()];
        while let Some(dir) = stack.pop() {
            if *budget == 0 {
                found.exhausted = true;
                return found;
            }
            let top = dir == *root;
            list(dir, top, &mut found, budget, &mut stack);
        }
    }
    found
}

/// Records what `dir` (`top`: one of the roots) holds in `found`, and adds
/// the folders to walk next to `stack`, in the order listed.
fn list(dir: PathBuf, top: bool, found: &mut Found, budget: &mut usize, stack: &mut Vec<PathBuf>) {
    let entries = match std::fs::read_dir(&dir) {
        Ok(entries) => entries,
        // Gone. A folder that is still there but says "not found"
        // (a ProjFS or Cloud Files provider can) is unlistable.
        Err(err)
            if err.kind() == std::io::ErrorKind::NotFound
                && std::fs::symlink_metadata(&dir)
                    .is_err_and(|err| err.kind() == std::io::ErrorKind::NotFound) =>
        {
            return;
        }
        Err(_) => {
            found.unlistable.push(dir);
            return;
        }
    };
    // D420: a `HEAD` or `commondir` makes this folder a candidate
    // bare repository.
    let mut git_candidate = false;
    // Whether this folder already has its note, and its unlistable
    // record, for a name that isn't valid Unicode.
    let (mut noted, mut unlisted) = (false, false);
    for entry in entries {
        let Ok(entry) = entry else {
            found.unlistable.push(dir.clone());
            break;
        };
        *budget = budget.saturating_sub(1);
        // D420: a name that isn't valid Unicode can't be recorded
        // (the records are JSON), so what is below it isn't walked
        // and its folder counts as changed every time.
        if entry.file_name().to_str().is_none() {
            if !noted {
                noted = true;
                found.notes.push((
                    dir.clone(),
                    "holds an entry whose name isn't valid Unicode, so it isn't checked"
                        .to_string(),
                ));
            }
            // D283: nested `.codex`/`.agents` folders below it
            // aren't seen either, so its folder (which can be
            // recorded) counts as unlistable. A plain file hides
            // nothing.
            if !unlisted && !entry.file_type().is_ok_and(|kind| kind.is_file()) {
                unlisted = true;
                found.unlistable.push(dir.clone());
            }
            continue;
        }
        let name = entry.file_name();
        let name = name.to_string_lossy();
        git_candidate |=
            name.eq_ignore_ascii_case("HEAD") || name.eq_ignore_ascii_case("commondir");
        // D420: a nested `.git` of any kind (the top-level one is
        // D332's).
        // Names fold as NTFS compares them (`.gıt` opens as `.git`).
        if !top && (name.eq_ignore_ascii_case(".git") || folds_to(&name, ".git")) {
            found.repos.push(dir.join(".git"));
        }
        // Not followed if it is a link. A listing that shows a
        // reparse point may not carry its tag, which tells links
        // apart: then the entry itself is asked (#1672).
        let metadata = match entry.metadata() {
            Ok(metadata) if metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0 => {
                std::fs::symlink_metadata(entry.path())
            }
            other => other,
        };
        if let Some((metadata_name, exact)) = metadata_name(&name) {
            // The top-level ones are D262's.
            if top && exact {
                continue;
            }
            if !top {
                let path = dir.join(metadata_name);
                let identity = (exact && !is_link(metadata.as_ref()))
                    .then(|| identity(&entry.path()))
                    .flatten();
                found.entries.push(NestedMetadata {
                    path,
                    identity,
                    contents: None,
                });
                if exact {
                    continue;
                }
            }
        }
        // D420: a folder link isn't walked, but git follows it: a
        // repository behind one (moved to `%TEMP%`, say) counts.
        if !name.eq_ignore_ascii_case(".git")
            && !folds_to(&name, ".git")
            && is_link(metadata.as_ref())
        {
            let link = entry.path();
            if std::fs::metadata(link.join(".git")).is_ok() {
                found.repos.push(link.join(".git"));
            } else if crate::git_watch::is_git_directory(&link) {
                found.repos.push(link.clone());
            }
            if std::fs::metadata(&link).is_ok_and(|metadata| metadata.is_dir()) {
                found.links.push(link);
            }
        }
        let Ok(metadata) = metadata else {
            found.unlistable.push(entry.path());
            continue;
        };
        // `is_dir` is false for links.
        if metadata.is_dir()
            // ASCII only: skipping a folded name (`.gıt`) would
            // leave an ordinary folder unwatched.
            && !name.eq_ignore_ascii_case(".git")
        {
            stack.push(entry.path());
        }
    }
    if !top && git_candidate && crate::git_watch::is_git_directory(&dir) {
        found.repos.push(dir);
    }
}

/// Which of `.codex`/`.agents` `name` is, and whether it is that name up to
/// ASCII case.
fn metadata_name(name: &str) -> Option<(&'static str, bool)> {
    PROJECT_METADATA_NAMES.into_iter().find_map(|metadata| {
        if name.eq_ignore_ascii_case(metadata) {
            Some((metadata, true))
        } else if folds_to(name, metadata) {
            Some((metadata, false))
        } else {
            None
        }
    })
}

/// Whether `name` upper-cases, character by character as NTFS compares
/// names, to `target` (ASCII).
fn folds_to(name: &str, target: &str) -> bool {
    let mut folded = name.chars().map(|c| {
        let mut upper = c.to_uppercase();
        match (upper.next(), upper.next()) {
            (Some(single), None) => single,
            _ => c,
        }
    });
    target
        .chars()
        .all(|t| folded.next() == Some(t.to_ascii_uppercase()))
        && folded.next().is_none()
}

/// The entries of `found` the session protects like the top-level ones:
/// not links (their targets are the sandbox's choice), and identified (the
/// others count as planted anyway).
pub(crate) fn to_protect(found: &[NestedMetadata]) -> Vec<NestedMetadata> {
    found
        .iter()
        .filter(|entry| entry.identity.is_some())
        .cloned()
        .collect()
}

/// The entries of `now` that aren't in `before` as the same file.
pub(crate) fn appeared(before: &[NestedMetadata], now: Vec<NestedMetadata>) -> Vec<PathBuf> {
    now.into_iter()
        .filter(|entry| {
            entry.identity.is_none()
                || !before
                    .iter()
                    .any(|old| old.unchanged(&entry.path, entry.identity))
        })
        .map(|entry| entry.path)
        .collect()
}

/// Whether `folder` (a `.codex`/`.agents` path, parent canonical) lies below
/// the top of one of `roots` and isn't in `before` as the file it is now
/// (one that doesn't exist counts: it may appear right after this check).
pub(crate) fn appeared_in_session(
    roots: &[PathBuf],
    before: &[NestedMetadata],
    folder: &Path,
) -> bool {
    let Some(parent) = folder.parent() else {
        return false;
    };
    let nested = roots
        .iter()
        .any(|root| !same_path(parent, root) && starts_with(parent, root));
    if !nested {
        return false;
    }
    let now = identity(folder);
    now.is_none() || !before.iter().any(|old| old.unchanged(folder, now))
}

/// Entries [`contents`] looks at in one folder, all levels together; past
/// that it differs every time.
const MAX_CONTENT_ENTRIES: usize = 20_000;
/// Bytes of file content [`contents`] reads in one folder; past that it
/// differs every time.
const MAX_CONTENT_BYTES: u64 = 64 * 1024 * 1024;

/// #1896 item 3 (D425): `entries` (from [`find`]) with what is inside each
/// identified one ([`contents`]), several at a time.
pub(crate) fn with_contents(entries: Vec<NestedMetadata>) -> Vec<NestedMetadata> {
    crate::par_map::par_map(&entries, |entry| NestedMetadata {
        contents: entry.identity.map(|_| contents(&entry.path)),
        ..entry.clone()
    })
}

/// What is inside the `.codex`/`.agents` folder `folder`: each entry at any
/// depth by its path from `folder`, a file by its content
/// ([`crate::git_watch::unfollowed_file_print`], which marks hard links and
/// reads only the file itself), a link by its target only (never followed),
/// a folder as `dir`. A `folder` that is a file is its content (read the
/// same way). A folder that can't be listed, a name that isn't valid
/// Unicode, or more than [`MAX_CONTENT_ENTRIES`]
/// entries or [`MAX_CONTENT_BYTES`] of files make it differ every time.
pub(crate) fn contents(folder: &Path) -> crate::git_watch::Prints {
    let mut prints = crate::git_watch::Prints::new();
    // A `.codex`/`.agents` that is a file: its content.
    if let Ok(metadata) = std::fs::symlink_metadata(folder)
        && !metadata.is_dir()
    {
        let print = if metadata.len() > MAX_CONTENT_BYTES {
            crate::git_watch::truncated()
        } else {
            crate::git_watch::unfollowed_file_print(folder)
        };
        prints.insert(String::new(), print);
        return prints;
    }
    let (mut seen, mut bytes) = (0_usize, 0_u64);
    let mut stack = vec![(folder.to_path_buf(), String::new())];
    while let Some((dir, prefix)) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            prints.insert(prefix, crate::git_watch::truncated());
            continue;
        };
        for entry in entries {
            seen += 1;
            if seen > MAX_CONTENT_ENTRIES {
                prints.insert(String::new(), crate::git_watch::truncated());
                return prints;
            }
            let Ok(entry) = entry else {
                prints.insert(prefix.clone(), crate::git_watch::truncated());
                break;
            };
            let Some(name) = entry.file_name().to_str().map(str::to_string) else {
                prints.insert(prefix.clone(), crate::git_watch::truncated());
                continue;
            };
            let path = entry.path();
            let child = format!("{prefix}{name}");
            match std::fs::symlink_metadata(&path) {
                Ok(metadata) if metadata.is_dir() && !metadata.file_type().is_symlink() => {
                    prints.insert(child.clone(), "dir".to_string());
                    stack.push((path, format!("{child}\\")));
                }
                // A link by its target, never read through: it may name a
                // network share (reading it would send the person's
                // credentials there) or a device.
                Ok(metadata) if metadata.file_type().is_symlink() => {
                    let target = std::fs::read_link(&path)
                        .map_or_else(|_| "?".to_string(), |target| target.display().to_string());
                    prints.insert(child, format!("link to {target}"));
                }
                Ok(metadata) => {
                    bytes = bytes.saturating_add(metadata.len());
                    let print = if bytes > MAX_CONTENT_BYTES {
                        crate::git_watch::truncated()
                    } else {
                        crate::git_watch::unfollowed_file_print(&path)
                    };
                    prints.insert(child, print);
                }
                Err(_) => {
                    prints.insert(child, crate::git_watch::truncated());
                }
            }
        }
    }
    prints
}

/// D332 (#1682): the top-level `.git` of each of `roots` (canonical) that
/// exists, of any kind, with the file it is. A link (Google's `repo` tool
/// makes one; a sandboxed command could point it elsewhere) or one that
/// can't be inspected has no identity, so it counts as changed after every
/// session. Git runs a `.git`'s hooks and config outside the sandbox, so one
/// that appears or changes while a session runs counts as planted, as new
/// `.codex`/`.agents` folders do; a missing one may still be made
/// (`git init` must work).
pub(crate) fn top_level_git(roots: &[PathBuf]) -> Vec<NestedMetadata> {
    roots
        .iter()
        .filter_map(|root| {
            let path = root.join(".git");
            let identity = match std::fs::symlink_metadata(&path) {
                Err(err) if err.kind() == std::io::ErrorKind::NotFound => return None,
                Ok(metadata) if !metadata.file_type().is_symlink() => identity(&path),
                _ => None,
            };
            Some(NestedMetadata {
                path,
                identity,
                contents: None,
            })
        })
        .collect()
}

/// D332: whether `folder` is the top-level `.git` of one of `roots` and
/// isn't in `before` (from [`top_level_git`]) as the file it is now (one
/// that doesn't exist counts: it may appear right after this check).
pub(crate) fn git_appeared_in_session(
    roots: &[PathBuf],
    before: &[NestedMetadata],
    folder: &Path,
) -> bool {
    let top_level = folder
        .file_name()
        .is_some_and(|name| name.eq_ignore_ascii_case(".git"))
        && folder
            .parent()
            .is_some_and(|parent| roots.iter().any(|root| same_path(parent, root)));
    if !top_level {
        return false;
    }
    let now = identity(folder).filter(|_| !crate::workspace_acl::is_link(folder));
    now.is_none()
        || !before
            .iter()
            .any(|old| same_path(&old.path, folder) && old.identity == now)
}

/// D420 (#1775): the repositories deeper behind `links` (folder links a walk
/// found) whose target lies in one of `unwalked` (writable roots the walk
/// doesn't cover, such as `%TEMP%`): a command could build one there and
/// link it in. Each such link is walked by its own path, its links in turn,
/// up to 20,000 entries in all; past that the link is a note (it counts as
/// changed after every session).
pub(crate) fn repos_behind_links(
    links: &[PathBuf],
    unwalked: &[PathBuf],
) -> (Vec<PathBuf>, Vec<(PathBuf, String)>) {
    let mut budget = 20_000_usize;
    let mut repos = Vec::new();
    let mut notes = Vec::new();
    let mut visited = std::collections::HashSet::new();
    let mut queue: Vec<PathBuf> = links.to_vec();
    while let Some(link) = queue.pop() {
        let Ok(target) = dunce::canonicalize(&link) else {
            continue;
        };
        if !unwalked.iter().any(|root| starts_with(&target, root)) || !visited.insert(target) {
            continue;
        }
        let found = find_bounded(std::slice::from_ref(&link), &mut budget);
        if found.exhausted {
            notes.push((
                link,
                "links to a folder too large to check for repositories".to_string(),
            ));
            break;
        }
        repos.extend(found.repos);
        notes.extend(found.notes);
        queue.extend(found.links);
    }
    (repos, notes)
}

/// D420 (#1775): a repository and what git would run from it, when a
/// session started or ended. `identity` is `None` for a link or an entry that
/// can't be inspected: such a repository counts as changed after every
/// session, as a top-level `.git` link does (D332).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct RepoState {
    pub(crate) path: PathBuf,
    pub(crate) identity: Option<FileIdentity>,
    #[serde(default)]
    pub(crate) link: bool,
    pub(crate) prints: crate::git_watch::Prints,
}

/// D420/gap 2 (#1775): those of `roots` (canonical) that are themselves git
/// folders (bare repositories), which git uses where it would use
/// `<root>\.git`.
pub(crate) fn bare_roots(roots: &[PathBuf]) -> Vec<PathBuf> {
    roots
        .iter()
        .filter(|root| crate::git_watch::is_git_directory(root))
        .cloned()
        .collect()
}

/// D420: the state of each repository in `paths`; folders a `gitdir:` line
/// or `commondir` names are looked into when they lie in `roots`. #1877:
/// several repositories at a time.
pub(crate) fn repo_states(paths: &[PathBuf], roots: &[PathBuf]) -> Vec<RepoState> {
    let folders = crate::git_watch::FolderPrints::default();
    crate::par_map::par_map(paths, |path| {
        let link =
            std::fs::symlink_metadata(path).is_ok_and(|metadata| metadata.file_type().is_symlink());
        RepoState {
            path: path.clone(),
            identity: if link { None } else { identity(path) },
            link,
            prints: crate::git_watch::fingerprint_with(path, roots, &folders),
        }
    })
}

/// D420/D421 (#1775): what changed in each repository of `now` since
/// `before`, for the person. A replaced one counts, and so does a new one
/// unless it has exactly the shape `git init` or `git clone` leave (D421); a
/// link, or one that can't be inspected, counts every time.
pub(crate) fn repo_changes(
    before: &[RepoState],
    now: Vec<RepoState>,
) -> Vec<(PathBuf, Vec<String>)> {
    // #1877: `before` by path as [`same_path`] compares them, in order,
    // looked up rather than compared with each.
    let mut by_path: std::collections::HashMap<String, Vec<&RepoState>> =
        std::collections::HashMap::new();
    for old in before {
        by_path.entry(path_text(&old.path)).or_default().push(old);
    }
    now.into_iter()
        .filter_map(|state| {
            let changes = if state.link {
                vec!["a link (checked after every command)".to_string()]
            } else if state.identity.is_none() {
                vec!["can't be inspected".to_string()]
            } else {
                let same = by_path
                    .get(&path_text(&state.path))
                    .map_or(&[][..], Vec::as_slice);
                match same.iter().find(|old| old.identity == state.identity) {
                    Some(old) => crate::git_watch::changes(&old.prints, &state.prints),
                    // Only a new one may pass as plain (D421), not one that
                    // replaced a repository.
                    None if !same.is_empty() => vec!["replaced".to_string()],
                    None if crate::git_watch::is_plain(&state.path) => Vec::new(),
                    None => vec!["new repository".to_string()],
                }
            };
            (!changes.is_empty()).then_some((state.path, changes))
        })
        .collect()
}

/// The file `path` is (not followed if it is a link).
pub(crate) fn identity(path: &Path) -> Option<FileIdentity> {
    use std::os::windows::fs::OpenOptionsExt as _;
    use std::os::windows::io::AsRawHandle as _;
    const FILE_FLAG_BACKUP_SEMANTICS: u32 = 0x0200_0000;
    const FILE_FLAG_OPEN_REPARSE_POINT: u32 = 0x0020_0000;
    const FILE_READ_ATTRIBUTES: u32 = 0x80;
    let file = std::fs::OpenOptions::new()
        .access_mode(FILE_READ_ATTRIBUTES)
        .share_mode(0x7)
        .custom_flags(FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT)
        .open(path)
        .ok()?;
    // SAFETY: the handle is open for the call.
    let (volume, id) = unsafe { crate::acl::file_identity(file.as_raw_handle() as _) }.ok()?;
    Some(FileIdentity { volume, id })
}

/// A link (a name-surrogate reparse point); unknown counts as one. Other
/// reparse points, such as ProjFS folders or cloud placeholders, are the
/// folder itself (#1672).
fn is_link(metadata: Result<&std::fs::Metadata, &std::io::Error>) -> bool {
    metadata.map_or(true, |metadata| metadata.file_type().is_symlink())
}

/// `path` as [`same_path`] compares it: two paths are the same exactly when
/// these are equal.
fn path_text(path: &Path) -> String {
    path.as_os_str().to_string_lossy().to_ascii_lowercase()
}

fn same_path(a: &Path, b: &Path) -> bool {
    a.as_os_str()
        .to_string_lossy()
        .eq_ignore_ascii_case(&b.as_os_str().to_string_lossy())
}

/// `path` is `base` or below it, ignoring case.
pub(crate) fn starts_with(path: &Path, base: &Path) -> bool {
    let mut path = path.components();
    base.components().all(|part| {
        path.next().is_some_and(|other| {
            other
                .as_os_str()
                .to_string_lossy()
                .eq_ignore_ascii_case(&part.as_os_str().to_string_lossy())
        })
    })
}

#[cfg(test)]
#[path = "nested_metadata_tests.rs"]
mod tests;
