//! Path resolution and protected-path classification for the post-taint
//! classifier. Lexical first; bounded, cached filesystem lookups then follow
//! symlinks and other non-canonical spellings of a home. Lookups never touch
//! locations that could reach the network or raise an OS privacy prompt.

use super::ProtectedActionKind;
use super::windows_paths;
use super::windows_reads;
use super::work;
use std::cell::Cell;
use std::cell::RefCell;
use std::collections::HashMap;
use std::collections::HashSet;
use std::path::Path;
use std::path::PathBuf;

/// Folders whose contents are credentials wherever they are.
const CREDENTIAL_SEGMENTS: &[&str] = &[".ssh", ".aws", ".gnupg", ".kube"];
/// Credential folders only in the user's home (projects often have their own).
const USER_CREDENTIAL_SEGMENTS: &[&str] = &[".docker", ".azure", ".config/gh", ".config/gcloud"];
const CREDENTIAL_FILES: &[&str] = &[
    ".credentials.json",
    ".netrc",
    ".git-credentials",
    ".npmrc",
    ".pypirc",
    // Network tool settings: may hold credentials or upload directives.
    ".curlrc",
    ".wgetrc",
    "id_rsa",
    "id_ecdsa",
    "id_ed25519",
    "id_dsa",
];
/// Files in the user's home that run code or set policy in later sessions,
/// outside any sandbox (PF-23-S02). The sandbox makes them read-only.
pub(crate) const USER_PERSISTENCE: &[&str] = &[
    ".zshrc",
    ".zshenv",
    ".zprofile",
    ".zlogin",
    ".zlogout",
    ".bashrc",
    ".bash_profile",
    ".bash_login",
    ".bash_logout",
    ".profile",
    ".kshrc",
    ".cshrc",
    ".tcshrc",
    ".config/fish",
    ".gitconfig",
    ".config/git",
    ".claude",
    ".config/autostart",
    ".config/systemd",
    ".config/environment.d",
    ".local/bin",
    "Library/LaunchAgents",
];
/// #1261 (D168): under Windows path rules, the user's places that run code
/// at sign-in: the Startup folder and the PowerShell folders (profiles, and
/// modules PowerShell loads by name), also under a OneDrive `Documents`.
/// A `*` in an entry matches within one name.
const WINDOWS_USER_PERSISTENCE: &[&str] = &[
    "appdata/roaming/microsoft/windows/start menu/programs/startup",
    "documents/windowspowershell",
    "documents/powershell",
    "onedrive*/documents/windowspowershell",
    "onedrive*/documents/powershell",
];
/// #1261 (D168): the same for every user, below any drive's root: the
/// common Startup folder, scheduled task files and the all-users PowerShell
/// profiles.
const WINDOWS_MACHINE_PERSISTENCE: &[&str] = &[
    "programdata/microsoft/windows/start menu/programs/startup",
    "windows/system32/tasks",
    "windows/syswow64/tasks",
    "windows/tasks",
    "windows/system32/windowspowershell/v1.0/*profile.ps1",
    "windows/syswow64/windowspowershell/v1.0/*profile.ps1",
    "program files/powershell/7/*profile.ps1",
];
/// The same inside a workspace: git runs hooks and reads config outside the
/// sandbox; project `.codex` and `.agents` set agent policy.
pub(crate) const WORKSPACE_PERSISTENCE: &[&str] =
    &[".git/hooks", ".git/config", ".codex", ".agents"];
/// The project's agent settings among them: read-only under Aggressive even
/// with a grant (#442, Travis 2026-10-11).
pub(crate) const WORKSPACE_AGENT_SETTINGS: &[&str] = &[".codex", ".agents"];
/// Corbanu home folder names (besides the configured `CODEX_HOME`).
const HOME_SEGMENTS: &[&str] = &[".corbanu", ".codex", ".pfterminal"];
/// Uncached filesystem lookups one classification may make. Past it the
/// classification fails closed.
const FS_LOOKUP_BUDGET: usize = 2048;
/// Never looked up: automounts and network or removable volumes (a lookup can
/// send traffic or block), unless inside an allowed root. On macOS `/home`
/// is an automount too.
const NO_LOOKUP_PREFIXES: &[&str] = &["/net", "/network", "/volumes", "/mnt", "/media"];
/// User folders behind macOS privacy prompts.
const PRIVACY_FOLDERS: &[&str] = &[
    "desktop",
    "documents",
    "downloads",
    "library",
    "movies",
    "music",
    "pictures",
];

pub(super) struct Homes {
    /// #1240: Windows path rules (see `windows_paths`) instead of POSIX ones.
    /// #1261: on Windows both judgments use them.
    windows: bool,
    /// #1240: the Windows view (cmd and PowerShell readings; it opens no
    /// file).
    view: bool,
    /// #1261: folders (lowercase, as resolved) reached through a spelling
    /// the first judgment may not resolve as the shell does: a network share
    /// or device path (the Windows rules spell `\\host\c$` and `\\?\C:` as
    /// `/c:`), a name Windows changes (`sub.`, `sub `, `sub:x`), the drive's
    /// root (`/x`), a variable or `~`. No file below one is opened.
    unread_folders: RefCell<HashSet<String>>,
    /// Original spellings, used to expand `$CODEX_HOME`, `$HOME` and `~`.
    codex_home_raw: String,
    user_home_raw: Option<String>,
    /// Lowercase spellings to match against: as configured and canonical.
    codex_homes: Vec<String>,
    user_homes: Vec<String>,
    /// Lowercase folders where lookups are always allowed (the action's
    /// working folder, the homes, temp folders).
    lookup_roots: RefCell<Vec<String>>,
    cache: RefCell<HashMap<String, Option<String>>>,
    lookups: Cell<usize>,
    exhausted: Cell<bool>,
}

impl Homes {
    /// The homes for the first judgment: under POSIX path rules, or (#1261)
    /// on Windows under Windows path rules.
    pub(super) fn new(codex_home: &Path, user_home: Option<&Path>) -> Self {
        Self::with_rules(
            codex_home,
            user_home,
            /*windows*/ cfg!(windows),
            /*view*/ false,
        )
    }

    /// #1261: the first judgment's homes as on Windows, on any platform
    /// (files are opened only on Windows).
    #[cfg(test)]
    pub(super) fn windows_first(codex_home: &Path, user_home: Option<&Path>) -> Self {
        Self::with_rules(
            codex_home, user_home, /*windows*/ true, /*view*/ false,
        )
    }

    /// #1240: the homes under Windows path rules, for the classifier's
    /// Windows view.
    pub(super) fn windows(codex_home: &Path, user_home: Option<&Path>) -> Self {
        Self::with_rules(
            codex_home, user_home, /*windows*/ true, /*view*/ true,
        )
    }

    fn with_rules(codex_home: &Path, user_home: Option<&Path>, windows: bool, view: bool) -> Self {
        let separators: &[char] = if windows { &['/', '\\'] } else { &['/'] };
        let raw = |path: &Path| {
            path.to_string_lossy()
                .trim_end_matches(separators)
                .to_string()
        };
        let codex_home_raw = raw(codex_home);
        let user_home_raw = user_home.map(raw);
        let normalize = |path: &str| normalize_with(windows, path);
        let mut lookup_roots = vec![
            "/tmp".to_string(),
            "/private/tmp".to_string(),
            "/var/folders".to_string(),
            "/private/var/folders".to_string(),
        ];
        lookup_roots.extend(
            std::env::temp_dir()
                .to_str()
                .map(|temp| normalize(&temp.to_lowercase())),
        );
        if !codex_home_raw.is_empty() {
            lookup_roots.push(normalize(&codex_home_raw.to_lowercase()));
        }
        let homes = Self {
            windows,
            view,
            unread_folders: RefCell::new(HashSet::new()),
            codex_home_raw,
            user_home_raw,
            codex_homes: Vec::new(),
            user_homes: Vec::new(),
            lookup_roots: RefCell::new(lookup_roots),
            cache: RefCell::new(HashMap::new()),
            lookups: Cell::new(0),
            exhausted: Cell::new(false),
        };
        let spellings = |raw: &str| {
            let mut spellings = vec![normalize(&raw.to_lowercase())];
            if let Some(canonical) = homes.canonical(raw)
                && !spellings.contains(&canonical)
            {
                spellings.push(canonical);
            }
            spellings
        };
        let codex_homes = if homes.codex_home_raw.is_empty() {
            Vec::new()
        } else {
            spellings(&homes.codex_home_raw)
        };
        let user_homes = homes
            .user_home_raw
            .as_deref()
            .map(spellings)
            .unwrap_or_default();
        homes
            .lookup_roots
            .borrow_mut()
            .extend(codex_homes.iter().cloned());
        Self {
            codex_homes,
            user_homes,
            ..homes
        }
    }

    /// Lookups are allowed under `folder` (the action's working folder).
    pub(super) fn allow_lookups_under(&self, folder: &str) {
        let folder = self.normalize(&folder.to_lowercase());
        let mut roots = self.lookup_roots.borrow_mut();
        if !roots.contains(&folder) {
            roots.push(folder);
        }
    }

    /// A lookup budget ran out, so some path was not followed.
    pub(super) fn exhausted(&self) -> bool {
        self.exhausted.get()
    }

    pub(super) fn codex_home(&self) -> &str {
        &self.codex_home_raw
    }

    pub(super) fn user_home(&self) -> Option<&str> {
        self.user_home_raw.as_deref()
    }

    /// Whether these are the Windows path rules.
    pub(super) fn windows_paths(&self) -> bool {
        self.windows
    }

    /// Whether this is the Windows view (#1240).
    pub(super) fn is_windows_view(&self) -> bool {
        self.view
    }

    /// Count one filesystem lookup. False, and the classification marked
    /// incomplete, once the budget is spent.
    pub(super) fn take_lookup(&self) -> bool {
        if self.lookups.get() >= FS_LOOKUP_BUDGET {
            self.exhausted.set(true);
            return false;
        }
        self.lookups.set(self.lookups.get() + 1);
        true
    }

    /// #1261: the plain local path (see `windows_reads`) that `word` names
    /// run in `cwd` under Windows rules, as a drive letter and the names
    /// below its root: `None` when it is not one, is spelled with a variable
    /// or `~`, or the folder is one of `unread_folders`.
    pub(super) fn windows_read_path(&self, word: &str, cwd: &str) -> Option<(char, Vec<String>)> {
        if windows_reads::expanded_spelling(word) || self.unread_folder(cwd) {
            return None;
        }
        windows_reads::plain_path(&self.expand_windows(word, cwd, /*places*/ false), cwd)
    }

    /// `normalize_path`, or its Windows counterpart.
    fn normalize(&self, path: &str) -> String {
        normalize_with(self.windows, path)
    }

    /// Expand `~`, `~user`, `$HOME` and `$CODEX_HOME` (or `$CORBANU_HOME`,
    /// `$PFTERMINAL_HOME`) spellings and make a
    /// relative word absolute against `cwd`. Keeps the word's case.
    pub(super) fn resolve(&self, word: &str, cwd: &str) -> String {
        if self.windows {
            return self.resolve_windows(word, cwd, /*places*/ false);
        }
        let word = self.expand_homes(word);
        if word.starts_with('/') || cwd.is_empty() {
            normalize_path(&word)
        } else {
            normalize_path(&format!("{cwd}/{word}"))
        }
    }

    /// #1436: [`Self::resolve`]'s home spellings expanded, the word kept as
    /// written otherwise: its path cleanup would drop `$X` from `$X/..`.
    /// Under Windows rules, [`Self::resolve`] without a folder.
    pub(super) fn expand_homes(&self, word: &str) -> String {
        if self.windows {
            return self.resolve_windows(word, "", /*places*/ false);
        }
        let mut word = word.to_string();
        for (name, value) in [
            ("CODEX_HOME", Some(&self.codex_home_raw)),
            ("CORBANU_HOME", Some(&self.codex_home_raw)),
            ("PFTERMINAL_HOME", Some(&self.codex_home_raw)),
            ("HOME", self.user_home_raw.as_ref()),
        ] {
            work::step();
            if let Some(value) = value {
                word = replace_variable(&word, name, value);
            }
        }
        if let Some(home) = &self.user_home_raw
            && let Some(rest) = word.strip_prefix('~')
        {
            let (user, tail) = rest.split_at(rest.find('/').unwrap_or(rest.len()));
            if user.is_empty() {
                word = format!("{home}{tail}");
            } else if let Some(parent) = Path::new(home).parent() {
                // `~name` is a sibling of the user's home.
                word = format!("{}/{user}{tail}", parent.to_string_lossy());
            }
        }
        word
    }

    /// [`Self::resolve`] under Windows rules: the same spellings, the Windows
    /// ones too (`%USERPROFILE%`, `$env:CODEX_HOME`, `~\`), and Windows roots.
    /// #1261: `places`: also expand `windows_paths::PLACE_VARIABLES`.
    fn resolve_windows(&self, word: &str, cwd: &str, places: bool) -> String {
        let expanded = self.expand_windows(word, cwd, places);
        let resolved = windows_paths::join(cwd, &expanded);
        // #1261: see `unread_folders`; everything below one is one too.
        let unread = windows_reads::expanded_spelling(word)
            || !windows_reads::plain_spelling(&expanded)
            || windows_reads::starts_at_a_share(cwd)
            || self.unread_folder(cwd);
        if unread {
            self.unread_folders
                .borrow_mut()
                .insert(resolved.to_lowercase());
        }
        resolved
    }

    /// #1261: whether `folder` (as resolved) is one of `unread_folders`.
    fn unread_folder(&self, folder: &str) -> bool {
        let folders = self.unread_folders.borrow();
        !folders.is_empty() && folders.contains(&folder.to_lowercase())
    }

    /// The home spellings of `word` expanded under Windows rules (`places`:
    /// see [`Self::resolve_windows`]).
    fn expand_windows(&self, word: &str, cwd: &str, places: bool) -> String {
        let mut word = word.to_string();
        for (name, value) in [
            ("CODEX_HOME", Some(&self.codex_home_raw)),
            ("CORBANU_HOME", Some(&self.codex_home_raw)),
            ("PFTERMINAL_HOME", Some(&self.codex_home_raw)),
            ("HOME", self.user_home_raw.as_ref()),
        ] {
            work::step();
            if let Some(value) = value {
                word = replace_variable(&word, name, value);
            }
        }
        word = windows_paths::expand(
            &word,
            &windows_paths::Values {
                codex_home: &self.codex_home_raw,
                user_home: self.user_home_raw.as_deref(),
                cwd,
                places,
            },
        );
        if let Some(home) = &self.user_home_raw
            && let Some(rest) = word.strip_prefix('~')
        {
            let (user, tail) = rest.split_at(rest.find(['/', '\\']).unwrap_or(rest.len()));
            if user.is_empty() {
                word = format!("{home}{tail}");
            } else {
                // `~name` is a sibling of the user's home.
                word = format!("{}/{user}{tail}", windows_paths::parent(home));
            }
        }
        word
    }

    /// #1261 (D168): [`Self::resolve`], with the Windows rules' variables for
    /// start-up places (`%ProgramData%`, `$PROFILE`, ...) expanded to their
    /// usual values: an extra reading, besides the one keeping them unknown.
    pub(super) fn resolve_places(&self, word: &str, cwd: &str) -> String {
        if self.windows {
            self.resolve_windows(word, cwd, /*places*/ true)
        } else {
            self.resolve(word, cwd)
        }
    }

    /// Classify a resolved path, also through its canonical form (symlinks,
    /// `/var` vs `/private/var`, letter case on case-insensitive volumes).
    pub(super) fn classify_resolved(&self, path: &str) -> Option<ProtectedActionKind> {
        self.classify_path(path).or_else(|| {
            self.canonical(path)
                .and_then(|canonical| self.classify_path(&canonical))
        })
    }

    /// Whether writing, moving or removing `path` changes what runs later
    /// outside the sandbox (PF-23-S02): a user persistence file or a folder
    /// above one below the home (moving `~/.config` moves the files out from
    /// under their rule), or a workspace's `.git` (hooks, config), `.codex` or
    /// `.agents`, also through its canonical form.
    pub(super) fn is_persistence(&self, path: &str) -> bool {
        let lexical = self.normalize(&path.to_lowercase());
        let canonical = self.canonical(path);
        let entry_segments =
            |entry: &str| -> Vec<String> { entry.split('/').map(str::to_lowercase).collect() };
        let prefix =
            |short: &[&str], long: &[String]| short.iter().zip(long).all(|(got, want)| got == want);
        [Some(lexical), canonical]
            .into_iter()
            .flatten()
            .any(|path| {
                let segments = segments(&path);
                let in_user_home = self.user_homes.iter().any(|home| {
                    below(&segments, home).is_some_and(|rest| {
                        USER_PERSISTENCE.iter().any(|entry| {
                            let entry = entry_segments(entry);
                            // The file or below it, or a folder above it.
                            prefix(&rest[..rest.len().min(entry.len())], &entry)
                        })
                    })
                });
                // `.git` itself, or anything in it but objects, refs and the
                // index (`commondir`, `config`, hooks choose what git runs).
                let in_git_dir = segments
                    .iter()
                    .position(|segment| *segment == ".git")
                    .is_some_and(|at| {
                        segments
                            .get(at + 1)
                            .is_none_or(|next| !matches!(*next, "objects" | "refs" | "index"))
                    });
                // #1261 (D168): Windows sign-in and scheduled places.
                let windows_entry = |rest: &[&str], entries: &[&str]| {
                    entries.iter().any(|entry| {
                        let entry: Vec<&str> = entry.split('/').collect();
                        let shared = rest.len().min(entry.len());
                        rest[..shared]
                            .iter()
                            .zip(&entry)
                            .all(|(got, want)| glob_any(want, got) || glob_any(got, want))
                    })
                };
                let in_windows_place = self.windows
                    && (self.user_homes.iter().any(|home| {
                        below(&segments, home)
                            .is_some_and(|rest| windows_entry(rest, WINDOWS_USER_PERSISTENCE))
                    }) || segments.split_first().is_some_and(|(root, rest)| {
                        root.ends_with(':')
                            && !rest.is_empty()
                            && windows_entry(rest, WINDOWS_MACHINE_PERSISTENCE)
                    }));
                in_user_home
                    || in_windows_place
                    || in_git_dir
                    || (0..segments.len()).any(|start| {
                        WORKSPACE_PERSISTENCE.iter().any(|entry| {
                            let entry = entry_segments(entry);
                            segments.len() - start >= entry.len()
                                && prefix(&segments[start..], &entry)
                        })
                    })
            })
    }

    /// Whether `path` (globs allowed) is the user's home, or a folder holding
    /// it or the Corbanu home: copying or archiving it takes credentials.
    pub(super) fn holds_a_home(&self, path: &str) -> bool {
        let lexical = self.normalize(&path.to_lowercase());
        let canonical = self.canonical(path);
        [Some(lexical), canonical]
            .into_iter()
            .flatten()
            .any(|path| {
                let pattern = segments(&path);
                let holds = |home: &String| {
                    let home = segments(home);
                    pattern.len() <= home.len()
                        && pattern
                            .iter()
                            .zip(&home)
                            .all(|(pattern, segment)| glob_any(pattern, segment))
                };
                self.user_homes.iter().any(holds) || self.codex_homes.iter().any(holds)
            })
    }

    /// Whether filesystem lookups may touch `path`.
    pub(super) fn lookups_allowed(&self, path: &str) -> bool {
        // #1240: Windows rules never look up (a lookup can follow a link or
        // a mapped drive to a network share); #1261: script and sed program
        // files are opened only through `windows_reads`.
        if self.windows {
            return false;
        }
        let lower = self.normalize(&path.to_lowercase());
        let under = |root: &str| lower == root || lower.starts_with(&format!("{root}/"));
        if self.lookup_roots.borrow().iter().any(|root| under(root)) {
            return true;
        }
        if NO_LOOKUP_PREFIXES.iter().any(|prefix| under(prefix))
            || (cfg!(target_os = "macos") && under("/home"))
        {
            return false;
        }
        !self.user_homes.iter().any(|home| {
            PRIVACY_FOLDERS
                .iter()
                .any(|folder| under(&format!("{home}/{folder}")))
        })
    }

    /// Canonical lowercase spelling of `path`, following symlinks. A path
    /// that does not exist yet keeps its missing tail on top of its nearest
    /// existing folder. `None` where lookups are not allowed or the budget
    /// is spent (which marks the classification as incomplete).
    pub(super) fn canonical(&self, path: &str) -> Option<String> {
        let path = normalize_path(path);
        if !self.lookups_allowed(&path) {
            return None;
        }
        let mut existing = PathBuf::from(&path);
        let mut tail: Vec<String> = Vec::new();
        loop {
            work::step();
            let key = existing.to_string_lossy().into_owned();
            let cached = self.cache.borrow().get(&key).cloned();
            let found = match cached {
                Some(found) => found,
                None => {
                    if !self.take_lookup() {
                        return None;
                    }
                    let found = std::fs::canonicalize(&existing)
                        .ok()
                        .map(|canonical| canonical.to_string_lossy().into_owned());
                    self.cache.borrow_mut().insert(key, found.clone());
                    found
                }
            };
            if let Some(mut canonical) = found {
                for segment in tail.iter().rev() {
                    work::step();
                    canonical = format!("{}/{segment}", canonical.trim_end_matches('/'));
                }
                return Some(normalize_path(&canonical.to_lowercase()));
            }
            let segment = existing.file_name()?.to_string_lossy().into_owned();
            tail.push(segment);
            if !existing.pop() {
                return None;
            }
        }
    }

    /// The protected kind of one absolute path. Glob segments count as any
    /// protected folder they could match.
    pub(super) fn classify_path(&self, path: &str) -> Option<ProtectedActionKind> {
        let path = self.normalize(&path.to_lowercase());
        let segments = segments(&path);
        let name = segments.last().copied().unwrap_or_default();
        if segments
            .iter()
            .any(|segment| matches_any(segment, CREDENTIAL_SEGMENTS))
            || USER_CREDENTIAL_SEGMENTS.iter().any(|folder| {
                self.user_homes.iter().any(|home| {
                    below(&segments, home).is_some_and(|rest| {
                        let expected: Vec<&str> = folder.split('/').collect();
                        rest.len() >= expected.len()
                            && expected.iter().zip(rest).all(|(expected, got)| {
                                glob_matches(got, expected)
                                    // `*` matches non-dot folder names.
                                    || (!expected.starts_with('.') && glob_any(got, expected))
                            })
                    })
                })
            })
            || CREDENTIAL_FILES.iter().any(|file| glob_matches(name, file))
            || (glob_matches(name, "hosts.yml")
                && segments.iter().any(|segment| glob_any(segment, "gh")))
            || name.starts_with("id_rsa")
            || name.starts_with("id_ed25519")
        {
            return Some(ProtectedActionKind::Credentials);
        }
        let below_home = self.below_home(&segments)?;
        let first = below_home.first().copied().unwrap_or("");
        // Agent worktrees kept under the home are ordinary workspaces.
        if first == "worktrees" {
            return None;
        }
        if name.contains("vault") || first.contains("vault") {
            return Some(ProtectedActionKind::Vault);
        }
        if name.starts_with("auth") || name.contains("credential") || name.ends_with(".key") {
            return Some(ProtectedActionKind::Credentials);
        }
        Some(ProtectedActionKind::SecurityPolicy)
    }

    /// The segments below a Corbanu home, if the path is inside one: the
    /// configured home as a run of segments anywhere in the path (globs in
    /// the path count), or a default home folder name as a segment.
    fn below_home<'a>(&self, segments: &[&'a str]) -> Option<Vec<&'a str>> {
        for home in &self.codex_homes {
            work::step();
            let home = self::segments(home);
            if home.is_empty() || home.len() > segments.len() {
                continue;
            }
            let found = (0..=segments.len() - home.len()).find(|start| {
                segments[*start..start + home.len()]
                    .iter()
                    .zip(&home)
                    .all(|(pattern, segment)| glob_any(pattern, segment))
            });
            if let Some(start) = found {
                return Some(segments[start + home.len()..].to_vec());
            }
        }
        let index = segments
            .iter()
            .position(|segment| matches_any(segment, HOME_SEGMENTS))?;
        Some(segments[index + 1..].to_vec())
    }
}

fn segments(path: &str) -> Vec<&str> {
    path.split('/')
        .filter(|segment| !segment.is_empty())
        .collect()
}

/// The segments of `path` below `home` (path segments may be globs).
fn below<'a, 'b>(path: &'b [&'a str], home: &str) -> Option<&'b [&'a str]> {
    let home = segments(home);
    (path.len() > home.len()
        && path
            .iter()
            .zip(&home)
            .all(|(pattern, segment)| glob_any(pattern, segment)))
    .then(|| &path[home.len()..])
}

/// Replace `$NAME` when `NAME` is not followed by more identifier characters.
pub(super) fn replace_variable(word: &str, name: &str, value: &str) -> String {
    let marker = format!("${name}");
    let mut out = String::new();
    let mut rest = word;
    while let Some(at) = rest.find(&marker) {
        work::step();
        let after = &rest[at + marker.len()..];
        out.push_str(&rest[..at]);
        if after
            .chars()
            .next()
            .is_some_and(|ch| ch.is_ascii_alphanumeric() || ch == '_')
        {
            out.push_str(&marker);
        } else {
            out.push_str(value);
        }
        rest = after;
    }
    out.push_str(rest);
    out
}

/// [`normalize_path`] under POSIX or Windows rules.
fn normalize_with(windows: bool, path: &str) -> String {
    if windows {
        windows_paths::normalize(path)
    } else {
        normalize_path(path)
    }
}

/// Resolve `.`, `..` and repeated slashes without touching the filesystem.
pub(super) fn normalize_path(path: &str) -> String {
    let mut parts: Vec<&str> = Vec::new();
    for segment in path.split('/') {
        work::step();
        match segment {
            "" | "." => {}
            ".." => {
                parts.pop();
            }
            segment => parts.push(segment),
        }
    }
    format!("/{}", parts.join("/"))
}

fn matches_any(segment: &str, names: &[&str]) -> bool {
    names.iter().any(|name| glob_matches(segment, name))
}

/// Whether the shell glob `pattern` (`*`, `?`, `[...]`) could match `name`.
/// Without glob characters this is equality.
fn glob_matches(pattern: &str, name: &str) -> bool {
    // A leading wildcard never expands to a dot-name in the shell (no
    // `dotglob`), and treating `*` or `*.json` as protected would block
    // ordinary work: only an explicit prefix can name a protected file.
    // #1261 (D167, Travis 2026-10-22): cmd and PowerShell wildcards do match
    // dot-names (`type *rc` reads `.netrc`); this assumption is kept for them
    // too, knowingly, because matching dot-names would make `*.json` and the
    // like ask everywhere.
    if pattern.starts_with(['*', '?', '[']) {
        return pattern == name;
    }
    glob_any(pattern, name)
}

/// The glob match itself, leading wildcards included. Iterative with one
/// backtrack point, so it is linear-times-linear whatever the pattern.
pub(super) fn glob_any(pattern: &str, name: &str) -> bool {
    // The pattern's text before its first glob character must start the
    // name, and without glob characters the match below is equality. Checked
    // first, byte by byte, this skips the match's two allocations and stops
    // at the first difference: every word's path checks compare it with many
    // short names, and a long word otherwise is read in full for each
    // (#1275).
    let (pattern_bytes, name_bytes) = (pattern.as_bytes(), name.as_bytes());
    let mut at = 0;
    loop {
        work::step();
        match pattern_bytes.get(at) {
            None => return at == name_bytes.len(),
            Some(b'*' | b'?' | b'[') => break,
            Some(byte) if name_bytes.get(at) != Some(byte) => return false,
            Some(_) => at += 1,
        }
    }
    let pattern: Vec<char> = pattern.chars().collect();
    let name: Vec<char> = name.chars().collect();
    // Length of the single-character token at `at` (`?`, `[...]` or literal).
    let token = |at: usize| match pattern[at] {
        '[' => pattern[at..]
            .iter()
            .position(|ch| *ch == ']')
            .map_or(1, |close| close + 1),
        _ => 1,
    };
    let matches_one = |at: usize, ch: char| match pattern[at] {
        '?' => true,
        '[' => token(at) > 1 || ch == '[',
        literal => literal == ch,
    };
    let (mut p, mut n) = (0, 0);
    let mut star: Option<(usize, usize)> = None;
    while n < name.len() {
        work::step();
        if p < pattern.len() && pattern[p] == '*' {
            star = Some((p, n));
            p += 1;
        } else if p < pattern.len() && matches_one(p, name[n]) {
            p += token(p);
            n += 1;
        } else if let Some((star_p, star_n)) = star {
            p = star_p + 1;
            n = star_n + 1;
            star = Some((star_p, star_n + 1));
        } else {
            return false;
        }
    }
    while p < pattern.len() && pattern[p] == '*' {
        work::step();
        p += 1;
    }
    p == pattern.len()
}
