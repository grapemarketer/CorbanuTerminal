//! What git reads to decide which hooks and config run, for the protected-path
//! rules (PF-23-S02): these paths become read-only.

use codex_utils_absolute_path::AbsolutePathBuf;
use std::path::Path;

/// Folders below `.git/modules` visited at most, and how deep. Past either
/// limit the whole `modules` folder becomes read-only instead (fail closed).
const MAX_MODULE_DIRS: usize = 512;
const MAX_MODULE_DEPTH: usize = 8;

/// For the repository `root` is in (the nearest `.git` at or above it, found
/// the way git finds it): hooks, config, config.worktree and commondir of its
/// git folder; for a `.git` file (worktree, submodule) the file itself and the
/// same entries in the folder it points to and its common folder; the folders
/// `core.hooksPath` names in its config, `~/.gitconfig` or
/// `~/.config/git/config`; the same entries of every folder below
/// `.git/modules` (submodule git folders, also nested ones); and those of
/// every linked worktree's folder, also the other worktrees' (#650).
///
/// On Linux a missing commondir, hooks folder or module entry is left out:
/// the sandbox would put an empty placeholder there, which breaks git.
/// Includes (`[include]`) are not
/// followed, and a new nested `.git` stays with the command-text net.
pub(super) fn git_persistence_paths(
    root: &AbsolutePathBuf,
    user_home: Option<&Path>,
) -> Vec<AbsolutePathBuf> {
    let home = user_home.and_then(|home| AbsolutePathBuf::from_absolute_path(home).ok());
    let (mut paths, work_tree) = match repository_paths(root, home.as_ref()) {
        Ok((paths, work_tree)) => {
            // Added to, never replaced: a `.git` a command creates below the
            // repository cannot make the earlier paths unprotected.
            let mut known = recall(&REPOSITORY_PATHS, root).unwrap_or_default();
            for path in paths {
                if !known.contains(&path) {
                    known.push(path);
                }
            }
            remember(&REPOSITORY_PATHS, root, Some(known.clone()));
            (known, work_tree)
        }
        // A folder made unreadable hides nothing: the `.git` paths found so
        // far, plus what this process found for `root` before.
        Err(closed) => {
            let mut paths = closed;
            paths.extend(recall(&REPOSITORY_PATHS, root).unwrap_or_default());
            (paths, root.clone())
        }
    };
    if let Some(home) = &home {
        for config in [home.join(".gitconfig"), home.join(".config/git/config")] {
            paths.extend(protected_hooks_path(&config, &work_tree, Some(home)));
        }
    }
    paths
}

/// The repository's own paths and its work tree, or (when a folder on the
/// way cannot be read) `Err` with the `.git` of that folder and of every
/// folder above it that may hold one, read-only whole.
fn repository_paths(
    root: &AbsolutePathBuf,
    home: Option<&AbsolutePathBuf>,
) -> Result<(Vec<AbsolutePathBuf>, AbsolutePathBuf), Vec<AbsolutePathBuf>> {
    let mut found = None;
    let ancestors: Vec<&Path> = root.as_path().ancestors().collect();
    for (at, folder) in ancestors.iter().enumerate() {
        let dot_git = folder.join(".git");
        match std::fs::symlink_metadata(&dot_git) {
            Ok(_) => {
                found = Some(dot_git);
                break;
            }
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
            Err(_) => {
                return Err(ancestors[at..]
                    .iter()
                    .map(|folder| folder.join(".git"))
                    .enumerate()
                    // Above the unreadable folder, a `.git` known to be
                    // missing is left out (no placeholder on Linux).
                    .filter(|(index, dot_git)| {
                        *index == 0
                            || !std::fs::symlink_metadata(dot_git)
                                .is_err_and(|err| err.kind() == std::io::ErrorKind::NotFound)
                    })
                    .filter_map(|(_, dot_git)| AbsolutePathBuf::from_absolute_path(dot_git).ok())
                    .collect());
            }
        }
    }
    let Some(Ok(dot_git)) = found.map(AbsolutePathBuf::from_absolute_path) else {
        return Ok((Vec::new(), root.clone()));
    };
    let Some(work_tree) = dot_git.parent() else {
        return Ok((Vec::new(), root.clone()));
    };
    let (mut paths, git_dirs) = if dot_git.as_path().is_dir() {
        (Vec::new(), vec![dot_git])
    } else {
        let mut git_dirs = Vec::new();
        if let Some(git_dir) = std::fs::read_to_string(dot_git.as_path())
            .ok()
            .and_then(|text| {
                text.lines()
                    .find_map(|line| line.strip_prefix("gitdir:"))
                    .map(|dir| dir.trim().to_string())
            })
        {
            let git_dir = AbsolutePathBuf::resolve_path_against_base(git_dir, work_tree.as_path());
            let common = match std::fs::read_to_string(git_dir.join("commondir").as_path()) {
                Ok(dir) => Some(AbsolutePathBuf::resolve_path_against_base(
                    dir.trim(),
                    git_dir.as_path(),
                )),
                Err(err) if err.kind() == std::io::ErrorKind::NotFound => None,
                // Unreadable (an agent can make it so): assume the usual
                // `<common>/worktrees/<name>` layout rather than lose it.
                Err(_) => git_dir
                    .parent()
                    .filter(|worktrees| worktrees.as_path().ends_with("worktrees"))
                    .and_then(|worktrees| worktrees.parent()),
            };
            git_dirs.push(git_dir);
            git_dirs.extend(common);
        }
        (vec![dot_git], git_dirs)
    };
    for dir in &git_dirs {
        paths.extend(entries(dir, /*below_modules*/ false));
        paths.extend(module_paths(dir));
        paths.extend(worktree_admin_paths(dir));
        for config in [dir.join("config"), dir.join("config.worktree")] {
            paths.extend(protected_hooks_path(&config, &work_tree, home));
        }
    }
    Ok((paths, work_tree))
}

/// #650: what git reads from every linked worktree's folder
/// (`<git_dir>/worktrees/<name>`), also the other worktrees': with one's
/// `commondir` or `config.worktree` rewritten, the person's later git runs
/// in that worktree would read a planted config (which can run code); and
/// its `gitdir` (where git finds the worktree). #675: also the other
/// worktree's own `.git` file that `gitdir` names, in its work tree:
/// rewritten to `gitdir: <planted>`, git runs there would read a planted
/// config too. On Linux a missing `commondir` or `gitdir` is left out, and
/// a `.git` file only while it exists (as a regular file). Past the limit,
/// or when the folder cannot be read, `worktrees` as a whole (fail closed),
/// with the `.git` files found until then. A `.git` file whose admin
/// folder cannot be read is not known; this process keeps the ones it found
/// before (`REPOSITORY_PATHS`), but a later start does not.
fn worktree_admin_paths(git_dir: &AbsolutePathBuf) -> Vec<AbsolutePathBuf> {
    let worktrees = git_dir.join("worktrees");
    let mut dot_gits = Vec::new();
    let closed = |dot_gits: Vec<AbsolutePathBuf>| {
        let mut paths = vec![worktrees.clone()];
        paths.extend(dot_gits);
        paths
    };
    let children = match std::fs::read_dir(worktrees.as_path()) {
        Ok(children) => children,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Vec::new(),
        Err(_) => return closed(dot_gits),
    };
    let mut paths = Vec::new();
    let mut visited = 0;
    for child in children {
        let Ok(child) = child else {
            return closed(dot_gits);
        };
        let Ok(kind) = child.file_type() else {
            return closed(dot_gits);
        };
        if !kind.is_dir() {
            continue;
        }
        visited += 1;
        if visited > MAX_MODULE_DIRS {
            return closed(dot_gits);
        }
        let admin = worktrees.join(child.file_name());
        for entry in ["commondir", "config.worktree", "gitdir"] {
            let path = admin.join(entry);
            // Like a git folder's own `config.worktree`, also when missing
            // (an empty placeholder is an empty config).
            if !cfg!(target_os = "linux") || entry == "config.worktree" || !missing(&path) {
                paths.push(path);
            }
        }
        dot_gits.extend(linked_dot_git(&admin));
    }
    paths.extend(dot_gits);
    paths
}

/// Longest `gitdir` file read (git writes one path and a newline).
const MAX_GITDIR_BYTES: u64 = 4096;

/// #675: the `.git` file of the linked worktree whose admin folder is
/// `admin`, as its `gitdir` names it (relative to `admin`'s real location
/// when relative, as git writes it with `worktree.useRelativePaths`),
/// when that is an existing regular file named `.git` (or, outside Linux,
/// may be one: it cannot be checked). Nothing when `gitdir` cannot be read
/// as a small regular file, or names something else (a folder, such as a
/// repository's own `.git`, stays writable).
fn linked_dot_git(admin: &AbsolutePathBuf) -> Option<AbsolutePathBuf> {
    let text = read_small_regular(&admin.join("gitdir"))?;
    let named = text.lines().next()?.trim();
    if named.is_empty() {
        return None;
    }
    let base = std::fs::canonicalize(admin.as_path()).unwrap_or_else(|_| admin.to_path_buf());
    let dot_git = AbsolutePathBuf::resolve_path_against_base(named, base);
    // #1468: `.GIT` names the same file on a case-insensitive volume.
    if !dot_git
        .as_path()
        .file_name()
        .is_some_and(|name| name.eq_ignore_ascii_case(".git"))
    {
        return None;
    }
    match std::fs::symlink_metadata(dot_git.as_path()) {
        Ok(meta) if meta.is_file() => Some(dot_git),
        Ok(_) => None,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => None,
        // Unreadable (it may be one): kept, except where Linux would put a
        // placeholder over whatever it is.
        Err(_) => (!cfg!(target_os = "linux")).then_some(dot_git),
    }
}

/// The text of the small regular file at `path`, read without blocking on
/// a FIFO or device swapped in; `None` when it is anything else or longer
/// than [`MAX_GITDIR_BYTES`].
fn read_small_regular(path: &AbsolutePathBuf) -> Option<String> {
    use std::io::Read;
    let mut options = std::fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::custom_flags(
        &mut options,
        libc::O_NONBLOCK | libc::O_NOCTTY | libc::O_CLOEXEC,
    );
    #[cfg(not(unix))]
    if !std::fs::symlink_metadata(path.as_path()).is_ok_and(|meta| meta.is_file()) {
        return None;
    }
    let file = options.open(path.as_path()).ok()?;
    let meta = file.metadata().ok()?;
    if !meta.is_file() || meta.len() > MAX_GITDIR_BYTES {
        return None;
    }
    let mut bytes = Vec::new();
    file.take(MAX_GITDIR_BYTES).read_to_end(&mut bytes).ok()?;
    String::from_utf8(bytes).ok()
}

/// The hooks folder `config` names, when it should be protected (on Linux
/// only when it exists or cannot be checked).
fn protected_hooks_path(
    config: &AbsolutePathBuf,
    work_tree: &AbsolutePathBuf,
    home: Option<&AbsolutePathBuf>,
) -> Option<AbsolutePathBuf> {
    hooks_path(config, work_tree, home)
        .filter(|hooks| !cfg!(target_os = "linux") || !missing(hooks))
}

/// The hooks, config, config.worktree and commondir of one git folder. On
/// Linux a missing commondir is left out; below `.git/modules` (where most
/// folders are not git folders) every missing entry is.
fn entries(git_dir: &AbsolutePathBuf, below_modules: bool) -> Vec<AbsolutePathBuf> {
    ["hooks", "config", "config.worktree", "commondir"]
        .into_iter()
        .map(|entry| git_dir.join(entry))
        .filter(|path| {
            let may_be_missing = below_modules || path.as_path().ends_with("commondir");
            !cfg!(target_os = "linux") || !may_be_missing || !missing(path)
        })
        .collect()
}

/// Only "not found" counts as missing: a path made unreadable keeps its
/// protection.
fn missing(path: &AbsolutePathBuf) -> bool {
    std::fs::symlink_metadata(path.as_path())
        .is_err_and(|err| err.kind() == std::io::ErrorKind::NotFound)
}

/// Large folders of git's own below `modules` that hold no hooks or config;
/// every other folder is walked (a submodule `libs/b` lives at
/// `modules/libs/b`, a submodule's worktrees at `<module>/worktrees/<name>`).
/// Directly inside `modules` nothing is skipped: those are submodule names.
const GIT_BULK: &[&str] = &["objects", "refs", "logs", "lfs", "rr-cache"];

/// Every folder below `git_dir/modules`, without following links: its four
/// entries, whether or not it looks like a git folder (removing `HEAD` or
/// adding `objects` must not unprotect one). Git's bulk folders are not
/// entered. #1468: a folder in a `worktrees` folder (a submodule's linked
/// worktree, `modules/m/worktrees/n`) also gets its `gitdir` and the `.git`
/// file that names, as [`worktree_admin_paths`] gives them. Past the limits,
/// or when a folder cannot be read, `modules` as a whole (fail closed), with
/// the `.git` files found until then.
fn module_paths(git_dir: &AbsolutePathBuf) -> Vec<AbsolutePathBuf> {
    let modules = git_dir.join("modules");
    let mut dot_gits = Vec::new();
    let closed = |dot_gits: Vec<AbsolutePathBuf>| {
        let mut paths = vec![modules.clone()];
        paths.extend(dot_gits);
        paths
    };
    let mut paths = Vec::new();
    let mut pending = vec![(modules.clone(), 0)];
    let mut visited = 0;
    while let Some((folder, depth)) = pending.pop() {
        let children = match std::fs::read_dir(folder.as_path()) {
            Ok(children) => children,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => continue,
            Err(_) => return closed(dot_gits),
        };
        // Also below a submodule named `worktrees`: its folders then get
        // a `gitdir` that is not there (left out on Linux) and no `.git`,
        // which is harmless.
        let in_worktrees = folder.as_path().ends_with("worktrees");
        for child in children {
            let Ok(child) = child else {
                return closed(dot_gits);
            };
            let Ok(kind) = child.file_type() else {
                return closed(dot_gits);
            };
            let name = child.file_name();
            if !kind.is_dir() || (depth > 0 && GIT_BULK.iter().any(|bulk| name == *bulk)) {
                continue;
            }
            visited += 1;
            if visited > MAX_MODULE_DIRS || depth >= MAX_MODULE_DEPTH {
                return closed(dot_gits);
            }
            let module = folder.join(name);
            paths.extend(entries(&module, /*below_modules*/ true));
            if in_worktrees {
                let gitdir = module.join("gitdir");
                if !cfg!(target_os = "linux") || !missing(&gitdir) {
                    paths.push(gitdir);
                }
                dot_gits.extend(linked_dot_git(&module));
            }
            pending.push((module, depth + 1));
        }
    }
    paths.extend(dot_gits);
    paths
}

/// The `core.hooksPath` a git config file sets (the last one wins), resolved
/// like git: `~/` against the user's home, a relative path against the work
/// tree. Comments and quotes are handled; escapes and includes are not.
fn hooks_path(
    config: &AbsolutePathBuf,
    work_tree: &AbsolutePathBuf,
    home: Option<&AbsolutePathBuf>,
) -> Option<AbsolutePathBuf> {
    let text = match std::fs::read_to_string(config.as_path()) {
        Ok(text) => text,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            remember(&HOOKS_PATHS, config, /*value*/ None);
            return None;
        }
        // Made unreadable: what this session read from it before still holds.
        Err(_) => return recall(&HOOKS_PATHS, config),
    };
    let mut in_core = false;
    let mut found = None;
    for line in text.lines() {
        let mut line = line.trim();
        if let Some(rest) = line.strip_prefix('[') {
            let Some((section, after)) = rest.split_once(']') else {
                in_core = false;
                continue;
            };
            in_core = section.trim().eq_ignore_ascii_case("core");
            // `[core] hooksPath = x` on one line.
            line = after.trim();
        }
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        if in_core && key.trim().eq_ignore_ascii_case("hookspath") {
            found = Some(config_value(value));
        }
    }
    let resolved =
        found
            .filter(|value| !value.is_empty())
            .and_then(|value| match value.strip_prefix("~/") {
                Some(rest) => home.map(|home| home.join(rest)),
                None if value.starts_with('~') => None,
                None => Some(AbsolutePathBuf::resolve_path_against_base(
                    value,
                    work_tree.as_path(),
                )),
            });
    remember(&HOOKS_PATHS, config, resolved.clone());
    resolved
}

type Remembered<T> =
    std::sync::LazyLock<std::sync::Mutex<std::collections::HashMap<AbsolutePathBuf, T>>>;

/// The hooks folder each config file named when it was last readable in
/// this process, and the repository paths found for each root. Lost at
/// restart: a config or folder still unreadable then is not covered.
static HOOKS_PATHS: Remembered<AbsolutePathBuf> = std::sync::LazyLock::new(Default::default);
static REPOSITORY_PATHS: Remembered<Vec<AbsolutePathBuf>> =
    std::sync::LazyLock::new(Default::default);

fn remember<T>(cache: &Remembered<T>, key: &AbsolutePathBuf, value: Option<T>) {
    let mut cache = cache
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    match value {
        Some(value) => {
            cache.insert(key.clone(), value);
        }
        None => {
            cache.remove(key);
        }
    }
}

fn recall<T: Clone>(cache: &Remembered<T>, key: &AbsolutePathBuf) -> Option<T> {
    cache
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .get(key)
        .cloned()
}

/// A git config value without its comment (`#` or `;` outside quotes) and
/// quotes.
fn config_value(raw: &str) -> String {
    let mut value = String::new();
    let mut quoted = false;
    for character in raw.trim().chars() {
        match character {
            '"' => quoted = !quoted,
            '#' | ';' if !quoted => break,
            other => value.push(other),
        }
    }
    value.trim().to_string()
}

#[cfg(test)]
#[path = "git_paths_tests.rs"]
mod tests;
