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
//! session records their contents and flags what changed or appeared, as for
//! the repositories themselves (D420). A global or system config (or a file
//! it includes) that lies in a writable root is a target itself. For Husky,
//! whose `_` stubs only run `<folder>\<hook>`, the hook-named files of the
//! folder above a `_` hooks folder count too, and so does the `_` folder
//! below any hooks folder (Husky v5 to v8 hooks source `_\husky.sh`).
//!
//! Plain `std::fs`, so it is tested on every platform; only the non-admin
//! Windows sandbox uses it.

use crate::git_watch::Prints;
use std::path::Path;
use std::path::PathBuf;

/// Include files followed this deep (git's own limit).
const MAX_INCLUDE_DEPTH: usize = 10;
/// Entries listed in a named folder; past that it counts as changed after
/// every session.
const MAX_ENTRIES: usize = 1024;

/// Keys whose value is a command git runs (`section.key`, or
/// `section.*.key` for any sub-section), lower-cased.
const PROGRAM_KEYS: [&str; 39] = [
    "core.fsmonitor",
    "core.pager",
    "core.editor",
    "core.sshcommand",
    "core.askpass",
    "core.gitproxy",
    "core.alternaterefscommand",
    "sequence.editor",
    "gpg.program",
    "gpg.*.program",
    "gpg.ssh.defaultkeycommand",
    "diff.external",
    "diff.*.textconv",
    "diff.*.command",
    "difftool.*.cmd",
    "difftool.*.path",
    "merge.*.driver",
    "mergetool.*.cmd",
    "mergetool.*.path",
    "credential.helper",
    "credential.*.helper",
    "filter.*.clean",
    "filter.*.smudge",
    "filter.*.process",
    "interactive.difffilter",
    "uploadpack.packobjectshook",
    "remote.*.uploadpack",
    "remote.*.receivepack",
    "remote.*.vcs",
    "trailer.*.cmd",
    "trailer.*.command",
    "sendemail.tocmd",
    "sendemail.cccmd",
    "sendemail.smtpserver",
    "browser.*.cmd",
    "browser.*.path",
    "man.*.cmd",
    "man.*.path",
    "hook.*.command",
];

/// Sections all of whose keys name commands (`pager.log`, `alias.x` with
/// `!`).
const PROGRAM_SECTIONS: [&str; 1] = ["pager"];

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

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub(crate) enum TargetKind {
    /// A hooks folder: every entry.
    Hooks,
    /// The folder above Husky's `_`: the entries named after git hooks.
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
/// global ones, the union of where git may look (`GIT_CONFIG_SYSTEM`,
/// `GIT_CONFIG_GLOBAL`, Git for Windows' and a user-scope install's
/// `etc\gitconfig`, `%PROGRAMDATA%\Git\config`, `/etc/gitconfig`, the XDG
/// config, and `.gitconfig` in each of `homes`), since the git the person
/// runs later may see another environment than this process.
pub(crate) fn outside_configs(homes: &[PathBuf]) -> Vec<PathBuf> {
    let mut configs = Vec::new();
    for var in ["GIT_CONFIG_SYSTEM", "GIT_CONFIG_GLOBAL"] {
        if let Some(path) = std::env::var_os(var) {
            configs.push(PathBuf::from(path));
        }
    }
    if let Some(data) = std::env::var_os("PROGRAMDATA") {
        configs.push(Path::new(&data).join("Git").join("config"));
    }
    let mut installs: Vec<PathBuf> = ["ProgramFiles", "ProgramFiles(x86)"]
        .iter()
        .filter_map(std::env::var_os)
        .map(|dir| Path::new(&dir).join("Git"))
        .collect();
    if let Some(local) = std::env::var_os("LOCALAPPDATA") {
        installs.push(Path::new(&local).join("Programs").join("Git"));
    }
    for git in installs {
        configs.push(git.join("etc").join("gitconfig"));
        configs.push(git.join("mingw64").join("etc").join("gitconfig"));
    }
    configs.push(PathBuf::from("/etc/gitconfig"));
    if let Some(xdg) = std::env::var_os("XDG_CONFIG_HOME") {
        configs.push(Path::new(&xdg).join("git").join("config"));
    }
    for home in homes {
        configs.push(home.join(".config").join("git").join("config"));
        configs.push(home.join(".gitconfig"));
    }
    configs.dedup();
    configs
}

/// The home folders git may use: `HOME` (Git for Windows prefers it) and
/// the user profile.
fn homes() -> Vec<PathBuf> {
    let mut homes: Vec<PathBuf> = std::env::var_os("HOME")
        .map(PathBuf::from)
        .into_iter()
        .collect();
    if let Some(profile) = dirs_next::home_dir()
        && !homes.contains(&profile)
    {
        homes.push(profile);
    }
    homes
}

/// The entries of the configs at `paths` (and the files they include), each
/// with the file it came from; and the files read (includes too).
pub(crate) fn read_configs(
    paths: &[PathBuf],
    homes: &[PathBuf],
) -> (Vec<(Entry, PathBuf)>, Vec<PathBuf>) {
    let mut entries = Vec::new();
    let mut files = Vec::new();
    for path in paths {
        read_config(path, homes, MAX_INCLUDE_DEPTH, &mut |entry, from| {
            if !files.iter().any(|file: &PathBuf| file == from) {
                files.push(from.to_path_buf());
            }
            entries.push((entry, from.to_path_buf()));
        });
    }
    (entries, files)
}

/// The targets the configs of the repository with git folder `git_dir`
/// (and work tree `work_tree`, `None` for a bare one) name: its own configs
/// and `outside` (the system and global entries, from [`read_configs`]),
/// kept when they lie in one of `roots` (canonical) by name or after links,
/// and not inside a root's own (protected) `.git`. `homes` expand `~/`.
pub(crate) fn targets(
    git_dir: &Path,
    work_tree: Option<&Path>,
    outside: &[(Entry, PathBuf)],
    homes: &[PathBuf],
    roots: &[PathBuf],
) -> Vec<Target> {
    let common = std::fs::read(git_dir.join("commondir")).ok().map(|bytes| {
        // As git reads it: only trailing line ends go.
        let value = String::from_utf8_lossy(&bytes);
        let value = Path::new(value.trim_end_matches(['\n', '\r']));
        if value.is_absolute() {
            value.to_path_buf()
        } else {
            git_dir.join(value)
        }
    });
    let own = [
        common.as_deref().unwrap_or(git_dir).join("config"),
        git_dir.join("config.worktree"),
    ];
    let (own_entries, _) = read_configs(&own, homes);
    // Hooks and program paths are relative to the work tree (the git folder
    // for a bare repository), or to `core.worktree` when set.
    let mut bases: Vec<PathBuf> = vec![work_tree.unwrap_or(git_dir).to_path_buf()];
    for (entry, _) in outside.iter().chain(&own_entries) {
        if entry.name == "core.worktree" {
            bases.push(resolve(git_dir, &entry.value, homes));
        }
    }
    let mut found = Vec::new();
    for (entry, from) in outside.iter().chain(&own_entries) {
        for base in &bases {
            target_of(entry, from, base, homes, &mut found);
        }
    }
    let mut unique: Vec<Target> = Vec::new();
    for mut target in found {
        target.path = crate::git_watch::normalize(&target.path);
        if in_roots(&target.path, roots) && !unique.contains(&target) {
            unique.push(target);
        }
    }
    unique
}

/// `path` lies in one of `roots`, by name or after links, and not inside a
/// root's own `.git` (protected, and D332's).
fn in_roots(path: &Path, roots: &[PathBuf]) -> bool {
    let canonical = dunce::canonicalize(path).ok();
    [Some(path), canonical.as_deref()]
        .into_iter()
        .flatten()
        .any(|path| {
            roots.iter().any(|root| {
                crate::git_watch::is_at_or_below(path, root)
                    && !crate::git_watch::is_at_or_below(path, &root.join(".git"))
            })
        })
}

/// Calls `each` with every entry of the config file at `path` and the file
/// it came from, following `include.path` and `includeIf.*.path` (whatever
/// the condition).
fn read_config(path: &Path, homes: &[PathBuf], depth: usize, each: &mut dyn FnMut(Entry, &Path)) {
    let Ok(bytes) = std::fs::read(path) else {
        return;
    };
    for entry in parse(&String::from_utf8_lossy(&bytes)) {
        let included = is_include(&entry.name)
            .then(|| resolve(path.parent().unwrap_or(Path::new("")), &entry.value, homes));
        each(entry, path);
        if let Some(included) = included
            && depth > 0
        {
            read_config(&included, homes, depth - 1, each);
        }
    }
}

fn is_include(name: &str) -> bool {
    name == "include.path" || (name.starts_with("includeif.") && name.ends_with(".path"))
}

/// The targets `entry` (from the config file `from`) names, relative to
/// `base`.
fn target_of(entry: &Entry, from: &Path, base: &Path, homes: &[PathBuf], found: &mut Vec<Target>) {
    let name = entry.name.as_str();
    if name == "core.hookspath" {
        let path = resolve(base, &entry.value, homes);
        let why = || "core.hooksPath".to_string();
        // Husky v9 (`husky <folder>` sets `<folder>\_`): its stubs run
        // `<folder>\<hook>`.
        if path.file_name().is_some_and(|name| name == "_")
            && let Some(parent) = path.parent()
        {
            found.push(Target {
                path: parent.to_path_buf(),
                why: why(),
                kind: TargetKind::HuskyHooks,
            });
        }
        // Husky v5 to v8: each hook sources `<folder>\_\husky.sh`.
        found.push(Target {
            path: path.join("_"),
            why: why(),
            kind: TargetKind::Hooks,
        });
        found.push(Target {
            path,
            why: why(),
            kind: TargetKind::Hooks,
        });
        return;
    }
    if is_include(name) {
        found.push(Target {
            path: resolve(from.parent().unwrap_or(Path::new("")), &entry.value, homes),
            why: "include.path".to_string(),
            kind: TargetKind::File,
        });
        return;
    }
    if name == "init.templatedir" {
        found.push(Target {
            path: resolve(base, &entry.value, homes).join("hooks"),
            why: entry.name.clone(),
            kind: TargetKind::Hooks,
        });
        return;
    }
    let section = name.split('.').next().unwrap_or_default();
    let program = PROGRAM_KEYS.iter().any(|key| key_matches(key, name))
        || PROGRAM_SECTIONS.contains(&section)
        || (section == "alias" && entry.value.trim_ascii_start().starts_with('!'));
    if !program {
        return;
    }
    // Every word that looks like a path names a file (`!sh ./x.sh`,
    // `node scripts/cred.js`, `!f() { ./x.sh; }; f`); a bare program name is
    // looked up on PATH.
    for word in words(&entry.value) {
        let word = word.trim_start_matches('!');
        let path_like = (word.contains('/') || word.contains('\\') || word.starts_with('.'))
            && !word.contains("://")
            && !word.starts_with('-')
            && !word.starts_with('$');
        if path_like {
            found.push(Target {
                path: resolve(base, word, homes),
                why: entry.name.clone(),
                kind: TargetKind::File,
            });
        }
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

/// The shell words of `command`, unquoted, split also at `;`, `&`, `|`,
/// `(`, `)`, `{` and `}`.
fn words(command: &str) -> Vec<String> {
    let mut words = Vec::new();
    let mut word = String::new();
    let mut quote: Option<char> = None;
    for c in command.chars() {
        match (quote, c) {
            (None, '"' | '\'') => quote = Some(c),
            (Some(q), c) if c == q => quote = None,
            (None, c) if c.is_ascii_whitespace() || ";&|(){}".contains(c) => {
                if !word.is_empty() {
                    words.push(std::mem::take(&mut word));
                }
            }
            (_, c) => word.push(c),
        }
    }
    if !word.is_empty() {
        words.push(word);
    }
    words
}

/// `value` (a path in a config) relative to `base`; `~/` is each home
/// folder's (the first).
fn resolve(base: &Path, value: &str, homes: &[PathBuf]) -> PathBuf {
    let value = value.trim_ascii();
    if let (Some(rest), Some(home)) = (
        value
            .strip_prefix("~/")
            .or_else(|| value.strip_prefix("~\\")),
        homes.first(),
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
    // Git skips a UTF-8 byte-order mark.
    let text = text.strip_prefix('\u{FEFF}').unwrap_or(text);
    let mut chars = text.chars().peekable();
    loop {
        // Skip blanks and comments up to the next header or key.
        while let Some(&c) = chars.peek() {
            if c.is_ascii_whitespace() {
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
                    name.trim_ascii().to_ascii_lowercase(),
                    sub.trim_end_matches('\u{1}')
                ),
                None => match header.trim_ascii().split_once('.') {
                    Some((name, sub)) => {
                        format!("{}.{}", name.to_ascii_lowercase(), sub.to_ascii_lowercase())
                    }
                    None => header.trim_ascii().to_ascii_lowercase(),
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
                Some('n') => {
                    value.push_str(&pending_space);
                    pending_space.clear();
                    value.push('\n');
                }
                Some('t') => {
                    value.push_str(&pending_space);
                    pending_space.clear();
                    value.push('\t');
                }
                Some('b') => {
                    value.push_str(&pending_space);
                    pending_space.clear();
                    value.push('\u{8}');
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
            // Git keeps an unquoted run of blanks inside a value as spaces.
            c if c.is_ascii_whitespace() && !in_quotes => {
                if !value.is_empty() {
                    pending_space.push(' ');
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
                // As git reads it.
                Some((crate::git_watch::gitdir_target(entry)?, Some(work_tree)))
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
    pub(crate) kind: TargetKind,
    pub(crate) prints: Prints,
}

impl TargetState {
    /// What is in it now.
    pub(crate) fn now(&self) -> Prints {
        target_prints(&Target {
            path: self.path.clone(),
            why: self.why.clone(),
            kind: self.kind,
        })
    }
}

/// The targets the configs of `repos` (each a git folder and its work tree,
/// `None` for a bare repository) and the system and global configs name
/// inside `roots`, with what is in each; and those configs themselves (and
/// the files they include) when they lie in `roots`.
pub(crate) fn target_states(
    repos: &[(PathBuf, Option<PathBuf>)],
    roots: &[PathBuf],
) -> Vec<TargetState> {
    let homes = homes();
    target_states_with(repos, roots, &outside_configs(&homes), &homes)
}

/// [`target_states`] with the system and global configs and the home
/// folders passed in.
pub(crate) fn target_states_with(
    repos: &[(PathBuf, Option<PathBuf>)],
    roots: &[PathBuf],
    outside_paths: &[PathBuf],
    homes: &[PathBuf],
) -> Vec<TargetState> {
    // Read once for all repositories.
    let (outside, outside_files) = read_configs(outside_paths, homes);
    let mut found: Vec<Target> = outside_files
        .into_iter()
        .map(|path| Target {
            path: crate::git_watch::normalize(&path),
            why: "git config".to_string(),
            kind: TargetKind::File,
        })
        .filter(|target| in_roots(&target.path, roots))
        .collect();
    for (git_dir, work_tree) in repos {
        found.extend(targets(
            git_dir,
            work_tree.as_deref(),
            &outside,
            homes,
            roots,
        ));
    }
    let mut states: Vec<TargetState> = Vec::new();
    for target in found {
        if states.iter().any(|state| state.path == target.path) {
            continue;
        }
        states.push(TargetState {
            prints: target_prints(&target),
            path: target.path,
            why: target.why,
            kind: target.kind,
        });
    }
    states
}

/// What changed in each target of `now` since `before` (taken when the
/// session started), for the person: `core.hooksPath: pre-commit added`
/// ... A target only `now` has counts with what is in it (a new repository
/// with a hooks folder a global config names, a link replaced by a folder).
pub(crate) fn target_changes(
    before: &[TargetState],
    now: Vec<TargetState>,
) -> Vec<(PathBuf, Vec<String>)> {
    now.into_iter()
        .filter_map(|state| {
            let old = before.iter().find(|old| {
                crate::git_watch::is_at_or_below(&old.path, &state.path)
                    && crate::git_watch::is_at_or_below(&state.path, &old.path)
            });
            let empty = Prints::new();
            let changes: Vec<String> =
                crate::git_watch::changes(old.map_or(&empty, |old| &old.prints), &state.prints)
                    .into_iter()
                    .map(|change| format!("{}: {}", state.why, change.trim_start()))
                    .collect();
            (!changes.is_empty()).then_some((state.path, changes))
        })
        .collect()
}

/// What is in `target` now: every entry of a hooks folder (a link to one is
/// followed, and recorded), the hook-named entries of Husky's folder, or a
/// file's content.
pub(crate) fn target_prints(target: &Target) -> Prints {
    let mut prints = Prints::new();
    match target.kind {
        TargetKind::File => {
            if std::fs::symlink_metadata(&target.path).is_ok() {
                prints.insert(String::new(), crate::git_watch::file_print(&target.path));
            }
        }
        TargetKind::Hooks | TargetKind::HuskyHooks => {
            if std::fs::symlink_metadata(&target.path)
                .is_ok_and(|metadata| metadata.file_type().is_symlink())
            {
                prints.insert(
                    "(link)".to_string(),
                    crate::git_watch::file_print(&target.path),
                );
            }
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
                    prints.insert(String::new(), crate::git_watch::truncated());
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
