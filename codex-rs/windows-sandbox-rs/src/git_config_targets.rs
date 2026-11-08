//! D422 (#1775): files and folders a repository's config names that git
//! runs programs from, outside the repository's git folder.
//!
//! Many projects set `core.hooksPath` to a folder in the work tree (Husky's
//! `.husky` or `.husky\_`, a team `.githooks`). The repository's config may
//! be protected, but the folder it names is ordinary, writable project
//! content: a command writes a hook there and the next `git commit` runs it.
//! Config values that name a file in the work tree are the same case: an
//! `include.path` (more config) or a program key set to a script in the
//! project. [`targets`] lists them from every config git would read for a
//! repository (its own `config` and `config.worktree`, the global and system
//! configs, and the files their `include`/`includeIf` lines name); the
//! session records their contents ([`target_prints`]) and flags what changed,
//! as for the repositories themselves (D420). For Husky, whose `_` files only
//! run `.husky\<hook>`, the files in `.husky` named after git hooks count
//! too.
//!
//! Plain `std::fs`, so it is tested on every platform; only the non-admin
//! Windows sandbox uses it.

use crate::git_watch::Prints;
use std::path::Path;
use std::path::PathBuf;

/// Include files followed this deep.
const MAX_INCLUDE_DEPTH: usize = 5;
/// Entries listed in a named folder before the rest counts as changed.
const MAX_ENTRIES: usize = 1024;

/// Keys whose value is a command git runs (`section.key`, or
/// `section.*.key` for any sub-section), lower-cased.
const PROGRAM_KEYS: [&str; 20] = [
    "core.fsmonitor",
    "core.pager",
    "core.editor",
    "core.sshcommand",
    "core.askpass",
    "core.alternaterefscommand",
    "sequence.editor",
    "gpg.program",
    "gpg.*.program",
    "diff.external",
    "diff.*.textconv",
    "diff.*.command",
    "merge.*.driver",
    "credential.helper",
    "credential.*.helper",
    "filter.*.clean",
    "filter.*.smudge",
    "filter.*.process",
    "interactive.difffilter",
    "uploadpack.packobjectshook",
];

/// The names of git's hooks (`githooks(5)`).
const HOOK_NAMES: [&str; 28] = [
    "applypatch-msg",
    "pre-applypatch",
    "post-applypatch",
    "pre-commit",
    "pre-merge-commit",
    "prepare-commit-msg",
    "commit-msg",
    "post-commit",
    "pre-rebase",
    "post-checkout",
    "post-merge",
    "pre-push",
    "pre-receive",
    "update",
    "proc-receive",
    "post-receive",
    "post-update",
    "reference-transaction",
    "push-to-checkout",
    "pre-auto-gc",
    "post-rewrite",
    "sendemail-validate",
    "fsmonitor-watchman",
    "p4-changelist",
    "p4-prepare-changelist",
    "p4-post-changelist",
    "p4-pre-submit",
    "post-index-change",
];

/// A file or folder a config names.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Target {
    pub(crate) path: PathBuf,
    /// The key that names it (`core.hooksPath`, `include.path`, ...).
    pub(crate) why: String,
    pub(crate) kind: TargetKind,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum TargetKind {
    /// A hooks folder: every entry.
    Hooks,
    /// Husky's `.husky`: the entries named after git hooks.
    HuskyHooks,
    /// A single file.
    File,
}

/// One `section[.sub].key = value` of a config file.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Entry {
    /// `section.key` or `section.sub.key`; section and key lower-cased.
    pub(crate) name: String,
    pub(crate) value: String,
}

/// The config files git reads besides a repository's own: the system and
/// global ones (`GIT_CONFIG_SYSTEM`/`GIT_CONFIG_GLOBAL` when set). `home` is
/// the user's home folder.
pub(crate) fn outside_configs(home: Option<&Path>) -> Vec<PathBuf> {
    let mut configs = Vec::new();
    match std::env::var_os("GIT_CONFIG_SYSTEM") {
        Some(path) => configs.push(PathBuf::from(path)),
        None => {
            if let Some(data) = std::env::var_os("PROGRAMDATA") {
                configs.push(Path::new(&data).join("Git").join("config"));
            }
            for var in ["ProgramFiles", "ProgramFiles(x86)"] {
                if let Some(dir) = std::env::var_os(var) {
                    let git = Path::new(&dir).join("Git");
                    configs.push(git.join("etc").join("gitconfig"));
                    configs.push(git.join("mingw64").join("etc").join("gitconfig"));
                }
            }
            configs.push(PathBuf::from("/etc/gitconfig"));
        }
    }
    match std::env::var_os("XDG_CONFIG_HOME") {
        Some(xdg) => configs.push(Path::new(&xdg).join("git").join("config")),
        None => {
            if let Some(home) = home {
                configs.push(home.join(".config").join("git").join("config"));
            }
        }
    }
    match std::env::var_os("GIT_CONFIG_GLOBAL") {
        Some(path) => configs.push(PathBuf::from(path)),
        None => {
            if let Some(home) = home {
                configs.push(home.join(".gitconfig"));
            }
        }
    }
    configs
}

/// The targets the configs of the repository with git folder `git_dir`
/// (and work tree `work_tree`, `None` for a bare one) name, that lie in one
/// of `roots` (canonical) and not inside a `.git`. `outside` are the system
/// and global configs ([`outside_configs`]); `home` expands `~/`.
pub(crate) fn targets(
    git_dir: &Path,
    work_tree: Option<&Path>,
    outside: &[PathBuf],
    home: Option<&Path>,
    roots: &[PathBuf],
) -> Vec<Target> {
    let mut files: Vec<PathBuf> = outside.to_vec();
    let common = std::fs::read(git_dir.join("commondir"))
        .ok()
        .map(|bytes| resolve(git_dir, String::from_utf8_lossy(&bytes).trim(), home));
    files.push(common.as_deref().unwrap_or(git_dir).join("config"));
    files.push(git_dir.join("config.worktree"));
    // Hooks and program paths are relative to the work tree (the git folder
    // for a bare repository).
    let base = work_tree.unwrap_or(git_dir);
    let mut found = Vec::new();
    for file in files {
        read_config(&file, home, MAX_INCLUDE_DEPTH, &mut |entry, from| {
            target_of(&entry, from, base, home, &mut found);
        });
    }
    for target in &mut found {
        target.path = crate::git_watch::normalize(&target.path);
    }
    found.retain(|target: &Target| {
        let path = dunce::canonicalize(&target.path).unwrap_or_else(|_| target.path.clone());
        roots
            .iter()
            .any(|root| crate::git_watch::is_at_or_below(&path, root))
            && !path
                .components()
                .any(|part| part.as_os_str().eq_ignore_ascii_case(".git"))
    });
    let mut unique: Vec<Target> = Vec::new();
    for target in found {
        if !unique.contains(&target) {
            unique.push(target);
        }
    }
    unique
}

/// Calls `each` with every entry of the config file at `path` and the file
/// it came from, following `include.path` and `includeIf.*.path` (whatever
/// the condition) and reporting those as entries too.
fn read_config(path: &Path, home: Option<&Path>, depth: usize, each: &mut dyn FnMut(Entry, &Path)) {
    let Ok(bytes) = std::fs::read(path) else {
        return;
    };
    for entry in parse(&String::from_utf8_lossy(&bytes)) {
        let include = entry.name == "include.path"
            || (entry.name.starts_with("includeif.") && entry.name.ends_with(".path"));
        let included =
            include.then(|| resolve(path.parent().unwrap_or(Path::new("")), &entry.value, home));
        each(entry, path);
        if let Some(included) = included
            && depth > 0
        {
            read_config(&included, home, depth - 1, each);
        }
    }
}

/// The target `entry` names, if any.
fn target_of(
    entry: &Entry,
    from: &Path,
    base: &Path,
    home: Option<&Path>,
    found: &mut Vec<Target>,
) {
    let name = entry.name.as_str();
    if name == "core.hookspath" {
        let path = resolve(base, &entry.value, home);
        let why = || "core.hooksPath".to_string();
        // Husky v9: `.husky\_\<hook>` runs `.husky\<hook>`.
        let husky_v9 = path.file_name().is_some_and(|name| name == "_")
            && path
                .parent()
                .and_then(Path::file_name)
                .is_some_and(|name| name.eq_ignore_ascii_case(".husky"));
        if husky_v9 && let Some(parent) = path.parent() {
            found.push(Target {
                path: parent.to_path_buf(),
                why: why(),
                kind: TargetKind::HuskyHooks,
            });
        }
        // Husky v5 to v8: `.husky\<hook>` sources `.husky\_\husky.sh`.
        if path
            .file_name()
            .is_some_and(|name| name.eq_ignore_ascii_case(".husky"))
        {
            found.push(Target {
                path: path.join("_"),
                why: why(),
                kind: TargetKind::Hooks,
            });
        }
        found.push(Target {
            path,
            why: why(),
            kind: TargetKind::Hooks,
        });
        return;
    }
    if name == "include.path" || (name.starts_with("includeif.") && name.ends_with(".path")) {
        found.push(Target {
            path: resolve(from.parent().unwrap_or(Path::new("")), &entry.value, home),
            why: "include.path".to_string(),
            kind: TargetKind::File,
        });
        return;
    }
    let program = PROGRAM_KEYS.iter().any(|key| key_matches(key, name))
        || (name.starts_with("alias.") && entry.value.trim_start().starts_with('!'));
    if !program {
        return;
    }
    let command = entry
        .value
        .trim_start()
        .trim_start_matches('!')
        .trim_start();
    let Some(first) = first_word(command) else {
        return;
    };
    // Only a path names a file; a bare program name is looked up on PATH.
    if first.contains('/') || first.contains('\\') || first.starts_with('.') {
        found.push(Target {
            path: resolve(base, &first, home),
            why: entry.name.clone(),
            kind: TargetKind::File,
        });
    }
}

/// `pattern` (`a.b` or `a.*.b`) matches the entry name `name`.
fn key_matches(pattern: &str, name: &str) -> bool {
    match pattern.split_once(".*.") {
        Some((section, key)) => name
            .strip_prefix(section)
            .and_then(|rest| rest.strip_prefix('.'))
            .and_then(|rest| rest.rsplit_once('.'))
            .is_some_and(|(sub, last)| !sub.is_empty() && last == key),
        None => name == pattern,
    }
}

/// The first shell word of `command`, unquoted.
fn first_word(command: &str) -> Option<String> {
    let mut word = String::new();
    let mut quote: Option<char> = None;
    for c in command.chars() {
        match (quote, c) {
            (None, '"' | '\'') => quote = Some(c),
            (Some(q), c) if c == q => quote = None,
            (None, c) if c.is_whitespace() => break,
            (_, c) => word.push(c),
        }
    }
    (!word.is_empty()).then_some(word)
}

/// `value` (a path in a config) relative to `base`; `~/` is the home folder.
fn resolve(base: &Path, value: &str, home: Option<&Path>) -> PathBuf {
    let value = value.trim();
    if let (Some(rest), Some(home)) = (
        value
            .strip_prefix("~/")
            .or_else(|| value.strip_prefix("~\\")),
        home,
    ) {
        return home.join(rest);
    }
    let path = Path::new(value);
    if path.is_absolute() || value.starts_with('/') || value.starts_with('\\') {
        path.to_path_buf()
    } else {
        base.join(path)
    }
}

/// The entries of a config file's text: `[section]`, `[section "sub"]` and
/// the deprecated `[section.sub]` headers, `key = value` lines (a key alone
/// is `true`), quotes, escapes, comments and continuation lines.
pub(crate) fn parse(text: &str) -> Vec<Entry> {
    let mut entries = Vec::new();
    let mut section = String::new();
    let mut chars = text.chars().peekable();
    loop {
        // Skip blanks and comments up to the next header or key.
        while let Some(&c) = chars.peek() {
            if c.is_whitespace() {
                chars.next();
            } else if c == '#' || c == ';' {
                for c in chars.by_ref() {
                    if c == '\n' {
                        break;
                    }
                }
            } else {
                break;
            }
        }
        let Some(&c) = chars.peek() else {
            break;
        };
        if c == '[' {
            chars.next();
            let mut header = String::new();
            let mut in_quotes = false;
            while let Some(c) = chars.next() {
                match c {
                    '"' => {
                        in_quotes = !in_quotes;
                        header.push('\u{1}');
                    }
                    '\\' if in_quotes => {
                        if let Some(next) = chars.next() {
                            header.push(next);
                        }
                    }
                    ']' if !in_quotes => break,
                    c => header.push(c),
                }
            }
            section = match header.split_once('\u{1}') {
                Some((name, sub)) => format!(
                    "{}.{}",
                    name.trim().to_ascii_lowercase(),
                    sub.trim_end_matches('\u{1}')
                ),
                None => match header.trim().split_once('.') {
                    Some((name, sub)) => {
                        format!("{}.{}", name.to_ascii_lowercase(), sub.to_ascii_lowercase())
                    }
                    None => header.trim().to_ascii_lowercase(),
                },
            };
            continue;
        }
        let mut key = String::new();
        while let Some(&c) = chars.peek() {
            if c.is_ascii_alphanumeric() || c == '-' {
                key.push(c);
                chars.next();
            } else {
                break;
            }
        }
        if key.is_empty() {
            // Not a key: skip the line.
            for c in chars.by_ref() {
                if c == '\n' {
                    break;
                }
            }
            continue;
        }
        while chars.peek().is_some_and(|c| *c == ' ' || *c == '\t') {
            chars.next();
        }
        let value = if chars.peek() == Some(&'=') {
            chars.next();
            read_value(&mut chars)
        } else {
            for c in chars.by_ref() {
                if c == '\n' {
                    break;
                }
            }
            "true".to_string()
        };
        entries.push(Entry {
            name: format!("{section}.{}", key.to_ascii_lowercase()),
            value,
        });
    }
    entries
}

fn read_value(chars: &mut std::iter::Peekable<std::str::Chars<'_>>) -> String {
    let mut value = String::new();
    let mut in_quotes = false;
    // Trailing unquoted blanks are dropped.
    let mut pending_space = String::new();
    while let Some(c) = chars.next() {
        match c {
            '\n' => break,
            '"' => {
                value.push_str(&pending_space);
                pending_space.clear();
                in_quotes = !in_quotes;
            }
            '\\' => match chars.next() {
                Some('\n') => {}
                Some('\r') if chars.peek() == Some(&'\n') => {
                    chars.next();
                }
                Some('n') => value.push('\n'),
                Some('t') => value.push('\t'),
                Some('b') => {
                    value.pop();
                }
                Some(other) => {
                    value.push_str(&pending_space);
                    pending_space.clear();
                    value.push(other);
                }
                None => break,
            },
            '#' | ';' if !in_quotes => {
                for c in chars.by_ref() {
                    if c == '\n' {
                        break;
                    }
                }
                break;
            }
            '\r' if !in_quotes && chars.peek() == Some(&'\n') => {}
            c if c.is_whitespace() && !in_quotes => {
                if !value.is_empty() {
                    pending_space.push(c);
                }
            }
            c => {
                value.push_str(&pending_space);
                pending_space.clear();
                value.push(c);
            }
        }
    }
    value
}

/// The git folder and work tree (`None` for a bare repository) of each of
/// `entries`: a `.git` folder, a `.git` file (the folder its `gitdir:` line
/// names) or a bare repository's folder. A `.git` link isn't followed.
pub(crate) fn repo_dirs(entries: &[PathBuf]) -> Vec<(PathBuf, Option<PathBuf>)> {
    entries
        .iter()
        .filter_map(|entry| {
            if !entry
                .file_name()
                .is_some_and(|name| name.eq_ignore_ascii_case(".git"))
            {
                return Some((entry.clone(), None));
            }
            let work_tree = entry.parent()?.to_path_buf();
            let metadata = std::fs::symlink_metadata(entry).ok()?;
            if metadata.is_dir() {
                Some((entry.clone(), Some(work_tree)))
            } else if metadata.is_file() {
                let text = String::from_utf8_lossy(&std::fs::read(entry).ok()?).into_owned();
                let target = text
                    .lines()
                    .next()?
                    .strip_prefix("gitdir:")?
                    .trim()
                    .to_string();
                Some((resolve(&work_tree, &target, None), Some(work_tree)))
            } else {
                None
            }
        })
        .collect()
}

/// A target and what was in it, when a session started or ended.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub(crate) struct TargetState {
    pub(crate) path: PathBuf,
    pub(crate) why: String,
    pub(crate) prints: Prints,
}

/// The targets the configs of `repos` (each a git folder and its work tree,
/// `None` for a bare repository) name inside `roots`, with what is in each.
pub(crate) fn target_states(
    repos: &[(PathBuf, Option<PathBuf>)],
    roots: &[PathBuf],
) -> Vec<TargetState> {
    let home = dirs_next::home_dir();
    let outside = outside_configs(home.as_deref());
    let mut states: Vec<TargetState> = Vec::new();
    for (git_dir, work_tree) in repos {
        for target in targets(
            git_dir,
            work_tree.as_deref(),
            &outside,
            home.as_deref(),
            roots,
        ) {
            if states.iter().any(|state| state.path == target.path) {
                continue;
            }
            states.push(TargetState {
                prints: target_prints(&target),
                path: target.path,
                why: target.why,
            });
        }
    }
    states
}

/// What changed in each target of `now` that `before` (taken when the
/// session started) also has, for the person: `core.hooksPath:
/// pre-commit added` ... A target only `now` has comes from a config that
/// changed, which D420 reports.
pub(crate) fn target_changes(
    before: &[TargetState],
    now: Vec<TargetState>,
) -> Vec<(PathBuf, Vec<String>)> {
    now.into_iter()
        .filter_map(|state| {
            let old = before.iter().find(|old| {
                crate::git_watch::is_at_or_below(&old.path, &state.path)
                    && crate::git_watch::is_at_or_below(&state.path, &old.path)
            })?;
            let changes: Vec<String> = crate::git_watch::changes(&old.prints, &state.prints)
                .into_iter()
                .map(|change| format!("{}: {}", state.why, change.trim_start()))
                .collect();
            (!changes.is_empty()).then_some((state.path, changes))
        })
        .collect()
}

/// What is in `target` now: every entry of a hooks folder, the entries of
/// Husky's `.husky` named after git hooks, or a file's content.
pub(crate) fn target_prints(target: &Target) -> Prints {
    let mut prints = Prints::new();
    match target.kind {
        TargetKind::File => {
            if std::fs::symlink_metadata(&target.path).is_ok() {
                prints.insert(String::new(), crate::git_watch::file_print(&target.path));
            }
        }
        TargetKind::Hooks | TargetKind::HuskyHooks => {
            let entries = match std::fs::read_dir(&target.path) {
                Ok(entries) => entries,
                Err(err) if err.kind() == std::io::ErrorKind::NotFound => return prints,
                Err(_) => {
                    prints.insert(String::new(), "unreadable".to_string());
                    return prints;
                }
            };
            for (count, entry) in entries.enumerate() {
                if count >= MAX_ENTRIES {
                    prints.insert(String::new(), "truncated".to_string());
                    break;
                }
                let Ok(entry) = entry else {
                    prints.insert(String::new(), "unreadable".to_string());
                    break;
                };
                let name = entry.file_name().to_string_lossy().into_owned();
                if target.kind == TargetKind::HuskyHooks
                    && !HOOK_NAMES
                        .iter()
                        .any(|hook| hook.eq_ignore_ascii_case(&name))
                {
                    continue;
                }
                prints.insert(name, crate::git_watch::file_print(&entry.path()));
            }
        }
    }
    prints
}

#[cfg(test)]
#[path = "git_config_targets_tests.rs"]
mod tests;
