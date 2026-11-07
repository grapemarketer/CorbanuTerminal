//! PF-23-S01 slice 3: after untrusted content, under Moderate or Aggressive,
//! agent commands run in a sandbox that cannot read credential files or the
//! Corbanu home. This is the OS-level net under the PF-30-S03 command-text
//! classifier: run-time strings, build tools, unknown wrappers and hard links
//! reach the same denial whatever the command text says.
//!
//! The Corbanu home is denied by default: every existing entry except what
//! agent commands need to run (`tmp` holds the arg0 helpers, `shell_snapshots`
//! is sourced by every command) or read as instructions (skills, plugins).
//!
//! PF-23-S02 adds writes: files that run code or set policy later (shell
//! start-up files, login items, git hooks and config, project `.codex`, the
//! Corbanu home entries that stay readable) become read-only wherever the
//! sandbox would let the command write them. Under Aggressive all of this
//! applies from the start of the session, not only after untrusted content.
//!
//! Issue #239 extends the same to Moderate: the read (and read-only) rules
//! apply from the start under a protected level (Moderate or Aggressive), so
//! a workspace-write sandbox cannot read credential files from `$HOME` before
//! any taint. Permissive and flag-off sessions are unchanged.

use crate::security::preflight::DATABASE_FOLDER_ENTRIES;
use crate::security::preflight::DB_INIT_LOCK;
use crate::security::tainted_action::USER_PERSISTENCE;
use crate::security::tainted_action::WORKSPACE_AGENT_SETTINGS;
use crate::security::tainted_action::WORKSPACE_PERSISTENCE;
use codex_protocol::models::PermissionProfile;
use codex_protocol::permissions::EffectiveWrites;
use codex_protocol::permissions::FileSystemAccessMode;
use codex_protocol::permissions::FileSystemPath;
use codex_protocol::permissions::FileSystemSandboxEntry;
use codex_protocol::permissions::FileSystemSandboxKind;
use codex_protocol::permissions::FileSystemSandboxPolicy;
use codex_protocol::permissions::FileSystemSpecialPath;
use codex_protocol::permissions::ReadDenyMatcher;
use codex_utils_absolute_path::AbsolutePathBuf;
use std::path::Path;
use std::path::PathBuf;

/// Corbanu home entries agent commands keep reading after untrusted content.
const RUNTIME_READABLE: &[&str] = &[
    "tmp",
    ".tmp",
    "shell_snapshots",
    "skills",
    "plugins",
    "packages",
    "worktrees",
    "AGENTS.md",
];

/// Denied even before they exist, so a store created later is covered too.
const CORBANU_STORES: &[&str] = &[
    "secrets",
    "auth.json",
    ".credentials.json",
    ".env",
    "provider_auth.json",
    "config.toml",
    "managed_config.toml",
    "source-origin.key",
    "security_level.toml",
    "security_state.json",
    "security_state.lock",
    // #586: the database init lock (#549); a command holding it would
    // stall every new launch on this home.
    DB_INIT_LOCK,
    // #562: read as instructions ahead of AGENTS.md; a command may not
    // create it either.
    "AGENTS.override.md",
    "wallet",
    "run",
    "log",
    "sessions",
    "archived_sessions",
    "history.jsonl",
    "memories",
    // #348: the Linux sandbox's record of the placeholders runs made; a
    // command that could write it could have a later run remove an empty
    // file of the person's.
    codex_sandboxing::landlock::PLACEHOLDER_RECORD_DIR_NAME,
];

/// #838 (Travis, 2026-10-15): Corbanu's security state and keys. Under
/// Moderate they stay denied (read and write) even to a command a person
/// approved, as the secretless launch contract keeps config, history and the
/// vault: a person's approval lifts every other rule for that command
/// (#540). Each is among [`CORBANU_STORES`] or the names Corbanu reads later.
const CORBANU_SECURITY_FILES: &[&str] = &[
    "source-origin.key",
    "security_level.toml",
    "security_state.json",
    "security_state.lock",
    "security_preflight.toml",
    "security_migration.toml",
    "security_confirm.lock",
    "security_level.lock",
];

/// #838 (Linux): the lock files among them, made empty and private before an
/// approved run where it could create them, so they stay denied without a
/// placeholder (see [`CORBANU_READ_LATER_LOCKS`]).
const CORBANU_SECURITY_LOCKS: &[&str] = &[
    "security_state.lock",
    "security_confirm.lock",
    "security_level.lock",
];

/// Corbanu home entries that stay readable but never writable: they run, or
/// are read as instructions, in later commands and sessions.
const CORBANU_READ_ONLY: &[&str] = &[
    "tmp",
    ".tmp",
    "shell_snapshots",
    "skills",
    "plugins",
    "packages",
    "AGENTS.md",
];

/// #583: the Corbanu home folder commands keep writing in, also when the
/// home itself is read-only (Corbanu's own git worktrees).
const HOME_WRITABLE: &str = "worktrees";

/// #603 (owner decision 16): the other Corbanu home folders Corbanu reads at
/// session start or later (exec-policy and `/security` rules, agent roles,
/// themes, memory extensions, panes, sockets it connects to, the proxy's CA,
/// caches and lock folders). `prompts` is not read today; upstream custom
/// prompts lived there. Stores and runtime entries are listed above.
const CORBANU_READ_LATER_FOLDERS: &[&str] = &[
    "rules",
    "agents",
    "themes",
    "prompts",
    "memories_extensions",
    "panes",
    "whips",
    "tasknode",
    "telegram",
    "ipc",
    "app-server-control",
    "app-server-daemon",
    "proxy",
    "cache",
    "models-cache",
    "attachments",
    "visualizations",
    "pets",
    "avatars",
    "gpu-transport",
    "mcp-oauth-locks",
    "thread-writer-locks",
    // #651: in the database folder, the home by default (pending accounting
    // records are imported later; recovery sets databases aside there).
    "accounting-pending",
    "db-backups",
];

/// #603: the Corbanu home files Corbanu reads at session start or later
/// (hooks, exec-server environments, `/security` receipts and locks, caches
/// and markers). Windows-only names are left out (see `protect_read_later`).
const CORBANU_READ_LATER_FILES: &[&str] = &[
    "hooks.json",
    "environments.toml",
    "security_preflight.toml",
    "security_migration.toml",
    "security_confirm.lock",
    "security_level.lock",
    "cloud-config-bundle-cache.json",
    "provider-eligibility.json",
    ".claude-auth-selection-present",
    ".claude-auth-selection-revision",
    ".sandbox_migration",
    "models_cache.json",
    "version.json",
    "installation_id",
    "session_index.jsonl",
    "external_agent_session_imports.json",
    // In the database folder, the home by default.
    "gpu-controller.lock",
];

/// #603 (Linux): the lock files among them, made empty and private before
/// the run like the folders: bubblewrap's placeholder for a missing one
/// would be removed after the run while another process may hold it.
const CORBANU_READ_LATER_LOCKS: &[&str] = &[
    "security_confirm.lock",
    "security_level.lock",
    "gpu-controller.lock",
];

/// The one runtime entry that is a file (#562).
const DEFAULT_AGENTS_MD: &str = "AGENTS.md";

/// Other Corbanu home folders in the user's home: denied whole.
const OTHER_CORBANU_HOMES: &[&str] = &[".corbanu", ".codex", ".pfterminal"];

/// Credential folders and files in the user's home.
const USER_CREDENTIALS: &[&str] = &[
    ".ssh",
    ".aws",
    ".gnupg",
    ".kube",
    ".docker",
    ".azure",
    ".config/gh",
    ".config/gcloud",
    ".config/solana",
    ".netrc",
    ".git-credentials",
    ".npmrc",
    ".pypirc",
    ".curlrc",
    ".wgetrc",
    ".credentials.json",
    ".claude/.credentials.json",
    "Library/Keychains",
];

/// The paths one protected command may not read, and those it may not write.
#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct ReadDenials {
    /// Existing paths: enforced as they are.
    existing: Vec<AbsolutePathBuf>,
    /// Fixed locations that may not exist yet.
    fixed: Vec<AbsolutePathBuf>,
    globs: Vec<FileSystemPath>,
    /// Readable but never writable (PF-23-S02), even before they exist.
    read_only: Vec<AbsolutePathBuf>,
    /// The same for the Corbanu home's runtime entries (`tmp` holds the
    /// sandbox launchers): never writable, not even with a grant (#442).
    runtime_read_only: Vec<AbsolutePathBuf>,
    /// The same for the project's `.codex` and `.agents`: never lifted by a
    /// grant either (#442, Travis 2026-10-11); also the read-only Corbanu
    /// home (#583) or the names Corbanu reads later in it (#603).
    never_lifted: Vec<AbsolutePathBuf>,
    /// #583: `worktrees` inside the read-only Corbanu home (each spelling),
    /// which stays writable where the profile can write it.
    home_writable: Vec<AbsolutePathBuf>,
    /// #603: the real location `worktrees` had when it was checked to be a
    /// real folder in the home ([`Self::protect_home`]).
    home_writable_real: Option<AbsolutePathBuf>,
    /// Where the profile's relative entries resolve (the turn's folder).
    cwd: Option<AbsolutePathBuf>,
    /// The Corbanu home, unless it is a folder the command keeps.
    home: Option<AbsolutePathBuf>,
    /// #506: whether listing [`Self::home`] is denied too (Aggressive).
    home_listing_denied: bool,
    /// #758: whether agent commands are kept away from the macOS keychain
    /// services (Aggressive), whatever the network rules allow.
    keychain_services_denied: bool,
    /// #594/#598: each spelling of the Corbanu home (as given, its real
    /// location, and a link naming it): it and the folders above it may not
    /// be moved aside, not even by a command a person approved (#540).
    unmovable: Vec<AbsolutePathBuf>,
    /// #766: under Moderate, the marker in the Corbanu home that is denied
    /// to the command, so a `corbanu exec` it starts knows Moderate is in
    /// force on this home.
    moderate_marker: Vec<AbsolutePathBuf>,
    /// #766: the registry of homes Moderate runs on, read-only to the command
    /// so it cannot drop this home from it.
    moderate_registry: Vec<AbsolutePathBuf>,
    /// #838: Corbanu's security state and keys ([`CORBANU_SECURITY_FILES`])
    /// in this and the other Corbanu homes, denied in [`Self::runtime_only`]
    /// as well.
    security_files: Vec<AbsolutePathBuf>,
    /// Denials dropped because they hold the command's folder or a writable
    /// root (denying them would deny the workspace itself).
    pub(crate) skipped: Vec<AbsolutePathBuf>,
}

impl ReadDenials {
    /// `keep` are folders the command must keep (its working folder and
    /// workspace roots): a denial equal to or above one of them is skipped.
    pub(crate) fn collect(
        codex_home: &Path,
        user_home: Option<&Path>,
        claude_config_dir: Option<&Path>,
        keep: &[AbsolutePathBuf],
    ) -> Self {
        let mut denials = Self {
            cwd: keep.first().cloned(),
            ..Self::default()
        };
        let Ok(codex_home) = AbsolutePathBuf::from_absolute_path(codex_home) else {
            return denials;
        };
        // Listing a folder the command works in stays allowed (by any
        // spelling: the sandboxes compare real locations).
        let real_home = std::fs::canonicalize(codex_home.as_path()).ok();
        if !keep.iter().any(|kept| {
            *kept == codex_home
                || real_home.is_some() && std::fs::canonicalize(kept.as_path()).ok() == real_home
        }) {
            denials.home = Some(codex_home.clone());
        }
        for entry in CORBANU_READ_ONLY {
            denials.push_runtime_read_only(codex_home.join(entry), keep);
        }
        denials.runtime_read_only = std::mem::take(&mut denials.read_only);
        denials.add_home_spellings(
            &codex_home,
            user_home
                .into_iter()
                .flat_map(|home| OTHER_CORBANU_HOMES.iter().map(|name| home.join(name))),
        );
        // #442 (Travis, 2026-10-11): the project's agent settings stay
        // read-only even with a grant.
        for root in keep {
            for entry in WORKSPACE_AGENT_SETTINGS {
                denials.push_read_only(root.join(entry), keep);
            }
        }
        denials.never_lifted = std::mem::take(&mut denials.read_only);
        if works_in_home(&codex_home, keep) {
            denials.protect_read_later(&codex_home, keep);
        } else if denials.home.is_some() {
            denials.protect_home(&codex_home, keep);
        }
        for root in keep {
            for entry in WORKSPACE_PERSISTENCE {
                if !WORKSPACE_AGENT_SETTINGS.contains(entry) {
                    denials.push_read_only(root.join(entry), keep);
                }
            }
            // The repository the root is in, found the way git finds it; a
            // worktree or submodule keeps hooks and config in the git folder
            // its `.git` file points to (hooks in the common one).
            for path in super::git_paths::git_persistence_paths(root, user_home) {
                denials.push_read_only(path, keep);
            }
        }
        if let Ok(entries) = std::fs::read_dir(codex_home.as_path()) {
            for entry in entries.flatten() {
                let name = entry.file_name();
                if RUNTIME_READABLE.iter().any(|keep| name == *keep) {
                    continue;
                }
                denials.push_existing(codex_home.join(name), keep);
            }
        }
        for store in CORBANU_STORES {
            denials.push_fixed(codex_home.join(store), keep);
        }
        denials.add_security_files(&codex_home, keep);
        // #651: the home escaped, should its path hold glob syntax.
        denials.globs.push(FileSystemPath::GlobPattern {
            pattern: crate::security::preflight::database_glob(codex_home.as_path())
                .to_string_lossy()
                .into_owned(),
            scan: None,
        });
        if let Some(home) =
            user_home.and_then(|home| AbsolutePathBuf::from_absolute_path(home).ok())
        {
            for other in OTHER_CORBANU_HOMES {
                let other = home.join(other);
                // The active home is handled entry by entry above, also
                // when another home name is a link to it. A name whose real
                // folder only holds the active home stays denied whole.
                if codex_home.as_path().starts_with(other.as_path())
                    || real_location(&codex_home)
                        .zip(real_location(&other))
                        .is_some_and(|(home, other)| home == other)
                {
                    continue;
                }
                // #838: what stays denied in it after an approval.
                if std::fs::symlink_metadata(other.as_path()).is_ok() {
                    denials.add_security_files(&other, keep);
                }
                denials.push_fixed(other, keep);
            }
            for credential in USER_CREDENTIALS {
                denials.push_fixed(home.join(credential), keep);
            }
            for file in USER_PERSISTENCE {
                denials.push_home_read_only(home.join(file), keep);
            }
        }
        if let Some(dir) =
            claude_config_dir.and_then(|dir| AbsolutePathBuf::from_absolute_path(dir).ok())
        {
            denials.push_fixed(dir.join(".credentials.json"), keep);
        }
        denials
    }

    /// The denials for this host's user: what stays readable is the turn's
    /// working folder, its workspace roots and the profile's writable roots
    /// resolved against that folder. `sqlite_home` is the database folder
    /// (`SqliteConfig::home`), the Corbanu home unless set apart (#611).
    pub(crate) fn for_turn(
        codex_home: &Path,
        sqlite_home: &Path,
        turn_cwd: &AbsolutePathBuf,
        workspace_roots: &[AbsolutePathBuf],
        profile: &PermissionProfile,
    ) -> Self {
        // #619: also through a root spelled by an alias of the home's
        // location (`/tmp` for `/private/tmp`), as the sandboxes match it.
        let can_write_home = can_write(
            &profile.file_system_sandbox_policy(),
            codex_home,
            turn_cwd.as_path(),
        );
        let mut keep = vec![turn_cwd.clone()];
        keep.extend(workspace_roots.iter().cloned());
        keep.extend(
            profile
                .file_system_sandbox_policy()
                .get_writable_roots_with_cwd(turn_cwd.as_path())
                .into_iter()
                .map(|root| root.root),
        );
        // #562: only where the command could otherwise create them.
        if cfg!(target_os = "linux") && can_write_home {
            create_missing_runtime_folders(codex_home);
            // #838: so the security locks stay denied after an approval
            // without bubblewrap holding them with a placeholder that is
            // removed while Corbanu may hold the lock.
            create_missing_security_locks(codex_home);
            // #603: the same for the folders Corbanu reads later, where the
            // home stays writable because the command works in it.
            if AbsolutePathBuf::from_absolute_path(codex_home)
                .is_ok_and(|home| works_in_home(&home, &keep))
            {
                create_missing_read_later_folders(codex_home);
            }
        }
        // #611 (Linux): a separate database folder's missing entries, made
        // as Corbanu would where the command could make them, so bubblewrap
        // puts no placeholder file where Corbanu expects a folder or a lock.
        // #651: the folder itself too, should it be missing (state start-up
        // failed): it is then denied whole, as when it exists, instead of
        // bubblewrap holding its path with a placeholder file on the host.
        if cfg!(target_os = "linux")
            && let Some(folder) = separate_database_folder(codex_home, sqlite_home)
            && profile
                .file_system_sandbox_policy()
                .can_write_path_with_cwd(folder.as_path(), turn_cwd.as_path())
        {
            create_missing_database_folder(folder.as_path());
        }
        // #583: the read-only home keeps `worktrees` writable only as a real
        // folder, so it is made (empty, as Corbanu would) where the command
        // could have made it before.
        if !cfg!(windows) && can_write_home {
            let _ = std::fs::create_dir(codex_home.join(HOME_WRITABLE));
        }
        let mut denials = Self::collect(
            codex_home,
            dirs::home_dir().as_deref(),
            std::env::var_os("CLAUDE_CONFIG_DIR")
                .as_deref()
                .map(Path::new),
            &keep,
        );
        // #838 (Linux): a missing security file where the command could
        // create it (its home writable once an approval lifts the rules)
        // cannot be denied without bubblewrap putting an empty placeholder
        // there for the run, which Corbanu would read as a broken security
        // state; such a file is left out (see `docs/security-moderate.md`).
        if cfg!(target_os = "linux") {
            let policy = profile.file_system_sandbox_policy();
            let writes = policy.effective_writes_with_cwd(turn_cwd.as_path());
            denials.security_files.retain(|path| {
                std::fs::symlink_metadata(path.as_path()).is_ok()
                    || !path.parent().is_some_and(|parent| {
                        can_write_with(&policy, parent.as_path(), turn_cwd.as_path(), &writes)
                    })
            });
        }
        denials.protect_database_folder(
            codex_home,
            sqlite_home,
            &keep,
            dirs::home_dir().as_deref(),
            &std::env::temp_dir(),
        );
        denials.protect_database_backups(sqlite_home, &keep);
        // #594: the home variable's own spelling (a link the home was found
        // through; the home itself is its real location).
        if let Ok(codex_home) = AbsolutePathBuf::from_absolute_path(codex_home) {
            denials.add_home_spellings(
                &codex_home,
                ["CORBANU_HOME", "PFTERMINAL_HOME", "CODEX_HOME"]
                    .into_iter()
                    .filter_map(std::env::var_os)
                    .map(PathBuf::from),
            );
        }
        denials
    }

    /// #611: a database folder set apart from the Corbanu home (`sqlite_home`
    /// or `CODEX_SQLITE_HOME`) is denied like the home's stores: whole, as
    /// Corbanu creates it before any command runs (the databases, the init
    /// lock, `gpu-controller.lock`, pending accounting records and database
    /// backups). Where it holds the command's own folder or the Corbanu home
    /// (whose runtime entries stay readable), what Corbanu keeps there is
    /// denied one by one instead: each existing database, the database
    /// pattern (reads of those created later) and [`DATABASE_FOLDER_ENTRIES`]
    /// also before they exist (the init lock, #586). A command a person
    /// approved under Moderate (#540) drops them with the other stores.
    ///
    /// #651: a folder missing at launch is denied whole too, before it
    /// exists, so a command cannot create it and fill it. On Linux `for_turn`
    /// first makes it where the command could (bubblewrap would otherwise
    /// hold the missing path with a placeholder file on the host).
    ///
    /// #651 item 6 (Travis, 2026-10-13): on macOS a well-known shared folder
    /// that exists (see [`is_well_known_shared_folder`]; `user_home` and
    /// `temp_dir` are the user's home and temporary folders) is denied one
    /// entry at a time too, so ordinary commands keep using it. On macOS each
    /// runtime database's name and its `-wal`, `-shm` and `-journal` files
    /// are also denied before they exist: the pattern alone stops reading a
    /// database created later, not creating one (or a link) under its name.
    /// Bubblewrap can't deny a name before it exists without putting a
    /// placeholder file on the host, where SQLite would meet it, so on Linux
    /// a shared folder stays denied whole (never weaker, Travis 2026-10-13).
    fn protect_database_folder(
        &mut self,
        codex_home: &Path,
        sqlite_home: &Path,
        keep: &[AbsolutePathBuf],
        user_home: Option<&Path>,
        temp_dir: &Path,
    ) {
        let Some(folder) = separate_database_folder(codex_home, sqlite_home) else {
            return;
        };
        let real_folder = std::fs::canonicalize(folder.as_path()).ok();
        let holds_home = codex_home.starts_with(folder.as_path())
            || real_folder.as_ref().is_some_and(|real| {
                std::fs::canonicalize(codex_home).is_ok_and(|home| home.starts_with(real))
            });
        let missing = std::fs::symlink_metadata(folder.as_path()).is_err();
        let shared = cfg!(target_os = "macos")
            && !missing
            && is_well_known_shared_folder(folder.as_path(), user_home, temp_dir);
        if (missing || folder.as_path().is_dir())
            && !holds_home
            && !shared
            && !Self::holds_kept(&folder, keep)
        {
            if missing {
                self.push_fixed(folder, keep);
            } else {
                self.push_existing(folder, keep);
            }
            return;
        }
        if let Ok(entries) = std::fs::read_dir(folder.as_path()) {
            for entry in entries.flatten() {
                if entry.file_name().to_string_lossy().contains(".sqlite") {
                    self.push_existing(folder.join(entry.file_name()), keep);
                }
            }
        }
        for name in DATABASE_FOLDER_ENTRIES {
            self.push_fixed(folder.join(name), keep);
        }
        if cfg!(target_os = "macos") {
            for database in codex_state::runtime_database_filenames() {
                for suffix in ["", "-wal", "-shm", "-journal"] {
                    self.push_fixed(folder.join(format!("{database}{suffix}")), keep);
                }
            }
        }
        // #732: only these direct database entries are generated here. Keep
        // their provenance through the permission profile so Linux never
        // recurses through unrelated descendants when expanding this rule.
        // #732 review: the top-level scan does not reach the backups folder's
        // databases; where the command works inside it (so it is not denied
        // whole), they keep the recursive scan they had.
        let backups = folder.join("db-backups");
        if Self::holds_kept(&backups, keep) {
            let backups = FileSystemPath::GlobPattern {
                pattern: format!(
                    "{}/**/*.sqlite*",
                    crate::security::preflight::glob_escape_path(backups.as_path())
                ),
                scan: None,
            };
            if !self.globs.contains(&backups) {
                self.globs.push(backups);
            }
        }
        let databases = FileSystemPath::database_entries(folder);
        if !self.globs.contains(&databases) {
            self.globs.push(databases);
        }
    }

    /// #651: databases recovery set aside beside the database folder (the
    /// Corbanu home by default) when something else stood in its place
    /// (`<folder>.db-backups`, `codex_state` recovery): denied when present.
    /// Not before it exists: on Linux bubblewrap would put a placeholder
    /// beside the folder (in the user's home for the default home, #348).
    fn protect_database_backups(&mut self, sqlite_home: &Path, keep: &[AbsolutePathBuf]) {
        let Some(name) = sqlite_home.file_name() else {
            return;
        };
        let mut backups = name.to_os_string();
        backups.push(".db-backups");
        if let Ok(backups) =
            AbsolutePathBuf::from_absolute_path(sqlite_home.with_file_name(backups))
            && std::fs::symlink_metadata(backups.as_path()).is_ok()
        {
            self.push_existing(backups, keep);
        }
    }

    /// #583: the Corbanu home itself is read-only, so a protected command
    /// cannot create an entry Corbanu reads later where it is missing:
    /// exec-policy `rules/` (which would approve commands in later
    /// sessions), `hooks.json`, `agents/` or any other name; every existing
    /// entry is already denied or read-only. Like the project's `.codex`, no
    /// grant lifts it; a person's approval of one command under Moderate
    /// does (#540), as it does the other rules. `worktrees` stays writable
    /// where the profile can write it, also when the command works in it.
    ///
    /// Not for a command working in the home itself, or in a folder of it
    /// outside `worktrees` (a writable carve-out there would become a sandbox
    /// root with its own default protections, such as a read-only git
    /// folder): see [`Self::protect_read_later`]. Nor on Windows, whose
    /// sandbox drops these entries anyway. On Linux a
    /// missing store below the home makes bubblewrap mount it as a read-only
    /// snapshot of its entries (#473), so nothing created in it after launch
    /// is visible either.
    fn protect_home(&mut self, codex_home: &AbsolutePathBuf, keep: &[AbsolutePathBuf]) {
        if cfg!(windows) {
            return;
        }
        let Some(real_home) = real_location(codex_home) else {
            return;
        };
        let homes = spellings(codex_home);
        let worktrees = spellings(&codex_home.join(HOME_WRITABLE));
        let inside_home = |folder: &AbsolutePathBuf| {
            homes.iter().any(|home| {
                folder.as_path() != home.as_path() && folder.as_path().starts_with(home.as_path())
            })
        };
        if works_in_home(codex_home, keep) {
            return;
        }
        for home in &homes {
            if !self.never_lifted.contains(home) {
                self.never_lifted.push(home.clone());
            }
        }
        // Only a real folder: a link (`worktrees -> tmp`, or `-> .`, which a
        // command a person approved could plant) would make what it points
        // to writable instead.
        let real_folder = std::fs::symlink_metadata(codex_home.join(HOME_WRITABLE).as_path())
            .is_ok_and(|metadata| metadata.is_dir());
        for folder in worktrees {
            // A spelling through a link below the top-level folders (a home
            // reached through `~/.corbanu -> /Volumes/x/home`) is left out:
            // Seatbelt resolves a writable root, so it would open the link's
            // target, which the profile's own rules never matched. The real
            // location is kept, where the profile can write it.
            if has_nested_link(folder.as_path()) {
                continue;
            }
            let holds_runtime = self
                .runtime_read_only
                .iter()
                .any(|runtime| runtime.as_path().starts_with(folder.as_path()));
            // #603: `worktrees` itself in a spelling of the home (a link
            // swapped in before `real_folder` was checked, and a folder put
            // back after it, would otherwise name the link's target).
            let names_worktrees = homes.iter().any(|home| folder == home.join(HOME_WRITABLE));
            if real_folder
                && names_worktrees
                && inside_home(&folder)
                && !holds_runtime
                && !self.home_writable.contains(&folder)
            {
                self.home_writable.push(folder);
                self.home_writable_real = Some(real_home.join(HOME_WRITABLE));
            }
        }
    }

    /// #603 (owner decision 16, Travis 2026-10-12): where the home stays
    /// writable because the command works in it (its working folder, a
    /// workspace root or a writable root is the home, or a folder of it
    /// outside `worktrees`), each name Corbanu reads later is read-only even
    /// before it exists, so the command cannot create `rules/x.rules`,
    /// `hooks.json`, `agents/` and the rest. Existing entries are denied
    /// already; other new names stay creatable. Like the read-only home, no
    /// grant lifts it and a person's approval of one command under Moderate
    /// does (#540, owner decision 15).
    ///
    /// On Linux bubblewrap holds a missing name with an empty placeholder
    /// for the command's run, so `for_turn` first creates the missing
    /// folders where the command could create them (as for the runtime
    /// folders, #562) and lock files; another file keeps its placeholder
    /// while the command runs (an empty `hooks.json` adds no hooks; another
    /// session meanwhile sees an empty `.claude-auth-selection-present`,
    /// `.sandbox_migration` or `security_preflight.toml`, and an empty
    /// marker it writes then may go with the placeholder's cleanup, #582:
    /// an accepted limit). A name that holds the command's own folder (it works
    /// in `rules`) is left out. Windows is not covered: its sandbox drops
    /// read-only entries for missing paths, as for every other one.
    fn protect_read_later(&mut self, codex_home: &AbsolutePathBuf, keep: &[AbsolutePathBuf]) {
        if cfg!(windows) {
            return;
        }
        let lifted = std::mem::take(&mut self.read_only);
        for name in CORBANU_READ_LATER_FOLDERS
            .iter()
            .chain(CORBANU_READ_LATER_FILES)
        {
            self.push_read_only(codex_home.join(name), keep);
        }
        for path in std::mem::replace(&mut self.read_only, lifted) {
            if !self.never_lifted.contains(&path) {
                self.never_lifted.push(path);
            }
        }
    }

    fn holds_kept(path: &AbsolutePathBuf, keep: &[AbsolutePathBuf]) -> bool {
        if keep
            .iter()
            .any(|kept| kept.as_path().starts_with(path.as_path()))
        {
            return true;
        }
        // #603: also by real location, as the sandboxes compare them (a home
        // spelled through `/tmp` with the working folder below it spelled by
        // its real location). A kept folder is resolved only through
        // top-level aliases: one that is or sits below a link (`build ->
        // ~/.ssh`) must not skip the denial of what it points to.
        let Ok(real) = std::fs::canonicalize(path.as_path()) else {
            return false;
        };
        keep.iter()
            .filter_map(|kept| kept_real_location(kept.as_path()))
            .any(|kept| kept_within(&kept, &real))
    }

    fn push_existing(&mut self, path: AbsolutePathBuf, keep: &[AbsolutePathBuf]) {
        if Self::holds_kept(&path, keep) {
            self.skipped.push(path);
        } else if !self.existing.contains(&path) {
            self.existing.push(path);
        }
    }

    fn push_fixed(&mut self, path: AbsolutePathBuf, keep: &[AbsolutePathBuf]) {
        if Self::holds_kept(&path, keep) {
            self.skipped.push(path);
        } else if !self.existing.contains(&path) && !self.fixed.contains(&path) {
            self.fixed.push(path);
        }
    }

    /// A read-only path in a home folder. On Linux a missing one is left out:
    /// the sandbox would put an empty placeholder at it in the real home for
    /// as long as the command runs (an empty `~/.bash_profile` hides
    /// `~/.profile` from the user's own login shells). The command-text net
    /// still asks before such a file is written.
    fn push_home_read_only(&mut self, path: AbsolutePathBuf, keep: &[AbsolutePathBuf]) {
        // `metadata` follows links: a link to a missing target is left out too.
        if cfg!(target_os = "linux") && std::fs::metadata(path.as_path()).is_err() {
            return;
        }
        self.push_read_only(path, keep);
    }

    /// #562: a runtime entry of the Corbanu home, also when it is missing,
    /// so a protected command cannot create it (`AGENTS.md`, `skills`). On
    /// Linux bubblewrap holds a missing path with an empty read-only
    /// placeholder for the command's run, as it does for the Corbanu stores;
    /// `for_turn` first creates the missing runtime folders where the
    /// profile could write them ([`create_missing_runtime_folders`]).
    fn push_runtime_read_only(&mut self, path: AbsolutePathBuf, keep: &[AbsolutePathBuf]) {
        self.push_read_only(path, keep);
    }

    /// A read-only path, by its real location: below a symlinked folder the
    /// Linux sandbox cannot bind the link name. A path below a regular file
    /// (`.git` in a worktree) cannot exist and is left out. On macOS the link
    /// name is kept too, so the sandbox also refuses to remove the link.
    fn push_read_only(&mut self, path: AbsolutePathBuf, keep: &[AbsolutePathBuf]) {
        let Some(real) = real_location(&path) else {
            return;
        };
        let mut spellings = vec![real];
        if cfg!(target_os = "macos") && !spellings.contains(&path) {
            spellings.push(path);
        }
        for path in spellings {
            if Self::holds_kept(&path, keep) {
                self.skipped.push(path);
            } else if !self.read_only.contains(&path) {
                self.read_only.push(path);
            }
        }
    }

    /// #442: an Aggressive grant lifts the write protections of files that
    /// run later (start-up files, git hooks and config) only. Every read
    /// denial stays, and so do the Corbanu home's read-only entries and the
    /// project's `.codex` and `.agents` (Travis, 2026-10-11).
    pub(crate) fn lift_write_rules(&mut self) {
        self.read_only.clear();
    }

    /// #594/#598: records the Corbanu home and each of `candidates` that
    /// names the same folder (a link to it, such as `~/.corbanu` or the
    /// value of `CORBANU_HOME`), so none of them can be moved aside.
    pub(crate) fn add_home_spellings(
        &mut self,
        codex_home: &AbsolutePathBuf,
        candidates: impl IntoIterator<Item = PathBuf>,
    ) {
        let Ok(real) = std::fs::canonicalize(codex_home.as_path()) else {
            return;
        };
        let names_home = |path: &Path| std::fs::canonicalize(path).is_ok_and(|path| path == real);
        let spellings = [codex_home.to_path_buf(), real.clone()].into_iter().chain(
            candidates
                .into_iter()
                .filter(|path| path.is_absolute() && names_home(path)),
        );
        for spelling in spellings {
            if let Ok(spelling) = AbsolutePathBuf::from_absolute_path(spelling)
                && !self.unmovable.contains(&spelling)
            {
                self.unmovable.push(spelling);
            }
        }
    }

    /// #594/#598: the Corbanu home's spellings that may not be moved.
    #[cfg(all(test, unix))]
    pub(crate) fn unmovable_paths(&self) -> impl Iterator<Item = &AbsolutePathBuf> {
        self.unmovable.iter()
    }

    /// Issue #540: only the rule that keeps the Corbanu home's runtime
    /// entries read-only, for a command whose other rules a person's
    /// approval lifted (Moderate, #239). The home and the folders above it
    /// stay in place too (#598), and Corbanu's security state and keys stay
    /// denied (#838).
    pub(crate) fn runtime_only(self) -> Self {
        // Existing ones as they are (the Windows sandbox drops entries that
        // may be missing), missing ones before they exist.
        let (existing, fixed) = self
            .security_files
            .into_iter()
            .partition(|path| std::fs::symlink_metadata(path.as_path()).is_ok());
        Self {
            existing,
            fixed,
            runtime_read_only: self.runtime_read_only,
            unmovable: self.unmovable,
            cwd: self.cwd,
            moderate_marker: self.moderate_marker,
            moderate_registry: self.moderate_registry,
            skipped: self.skipped,
            ..Self::default()
        }
    }

    /// #838: Corbanu's security state and keys in `home` (the active Corbanu
    /// home or another one), also before they exist, each by its real
    /// location (bubblewrap refuses a deny spelled through a writable link,
    /// as a home reached through one would be, #766) and on macOS also as
    /// given. One that holds a kept folder is skipped, as other denials are.
    fn add_security_files(&mut self, home: &AbsolutePathBuf, keep: &[AbsolutePathBuf]) {
        for file in CORBANU_SECURITY_FILES {
            let path = home.join(file);
            if Self::holds_kept(&path, keep) {
                if !self.skipped.contains(&path) {
                    self.skipped.push(path);
                }
                continue;
            }
            for spelling in moderate_spellings(path.as_path()) {
                if !self.security_files.contains(&spelling) {
                    self.security_files.push(spelling);
                }
            }
        }
    }

    /// #838: whether `path` (by any spelling) is one of Corbanu's security
    /// state and key files in `codex_home` or another Corbanu home in
    /// `user_home`, which a person's approval under Moderate does not lift.
    /// Explanation only.
    pub(crate) fn is_security_file(
        codex_home: &Path,
        user_home: Option<&Path>,
        path: &Path,
    ) -> bool {
        let real = |path: &Path| std::fs::canonicalize(path).ok();
        let real_path = real(path).or_else(|| {
            let name = path.file_name()?;
            real(path.parent()?).map(|parent| parent.join(name))
        });
        let homes = [Some(codex_home.to_path_buf()), real(codex_home)];
        // Also another Corbanu home's (`~/.codex` and the like).
        let in_other_home = |path: &Path| {
            path.parent().is_some_and(|parent| {
                user_home.is_some_and(|home| {
                    OTHER_CORBANU_HOMES
                        .iter()
                        .any(|other| parent == home.join(other))
                })
            }) && path
                .file_name()
                .is_some_and(|name| CORBANU_SECURITY_FILES.iter().any(|file| name == *file))
        };
        in_other_home(path)
            || real_path.as_deref().is_some_and(in_other_home)
            || CORBANU_SECURITY_FILES.iter().any(|file| {
                homes.iter().flatten().any(|home| {
                    let candidate = home.join(file);
                    candidate == path || real_path.as_ref() == Some(&candidate)
                })
            })
    }

    /// #766 (Travis, 2026-10-14): under Moderate, deny the command the
    /// Corbanu home's Moderate marker (made here when it is missing) and keep
    /// the registry of Moderate homes (with this home in it) read-only, so a
    /// `corbanu exec` or `review` the command starts runs with Moderate
    /// enforced, also when pointed at another home. The marker sits in the
    /// home's runtime folder, which these rules keep read-only at every
    /// level. No approval lifts either. Only on macOS and Linux, whose
    /// sandboxes this is checked with; elsewhere such a launch is treated as
    /// before.
    pub(crate) fn mark_moderate(&mut self, codex_home: &Path) {
        if !cfg!(any(target_os = "macos", target_os = "linux")) {
            return;
        }
        let registry = codex_security_level::nested::account_home()
            .ok()
            .flatten()
            .map(|account| codex_security_level::nested::moderate_registry_dir(&account));
        self.mark_moderate_with(codex_home, registry.as_deref());
    }

    /// #766: where the protected-file rules apply but Moderate does not mark
    /// the home, a marker someone made unreadable is made readable again, so
    /// it never passes for Moderate's (on Linux an unreadable file is the
    /// evidence; see `nested::keep_moderate_marker_readable`). The file
    /// stays: a Moderate session on this home may hold it masked for a
    /// running command.
    pub(crate) fn keep_moderate_marker_readable(codex_home: &Path) {
        if let Err(err) = codex_security_level::nested::keep_moderate_marker_readable(codex_home) {
            tracing::warn!("could not make the Moderate marker readable: {err}");
        }
    }

    /// [`Self::mark_moderate`] with the registry folder given. Not when the
    /// home's stored level enforces Aggressive (also when it is unreadable),
    /// or an Aggressive session's rule file is still there: a nested launch
    /// then follows Aggressive's rules, as before, also where this session's
    /// Core level is only Moderate (a session not started by the TUI, which
    /// alone applies Aggressive's profile).
    pub(crate) fn mark_moderate_with(&mut self, codex_home: &Path, registry: Option<&Path>) {
        use codex_security_level::level;
        use codex_security_level::nested;
        let rules = std::fs::read_to_string(level::rules_path(codex_home))
            .is_ok_and(|contents| level::is_rules_contents(&contents));
        if rules || level::load(codex_home).enforced() == level::ChosenLevel::Aggressive {
            Self::keep_moderate_marker_readable(codex_home);
            return;
        }
        match nested::prepare_moderate_marker(codex_home) {
            Ok(marker) => self.moderate_marker = moderate_spellings(&marker),
            Err(err) => tracing::warn!("could not prepare the Moderate marker: {err}"),
        }
        let Some(registry) = registry else {
            return;
        };
        // Expected inside another command's sandbox (a nested run's own
        // home), where the registry is read-only.
        if let Err(err) = nested::register_moderate_home(registry, codex_home) {
            tracing::debug!("could not register the Moderate home: {err}");
        }
        // Read-only also when this home could not be added (a nested run's
        // own home, inside another command's sandbox).
        if registry.is_dir() {
            self.moderate_registry = moderate_spellings(registry);
        }
    }

    /// #506: under Aggressive, agent commands may not list the Corbanu home
    /// either. What is below it keeps these rules: its runtime entries stay
    /// readable, everything else stays denied.
    pub(crate) fn deny_home_listing(&mut self) {
        self.home_listing_denied = true;
    }

    /// #758: under Aggressive, agent commands may not reach the macOS
    /// keychain services either, not even through a managed network proxy
    /// or an allowed Unix socket (Travis, 2026-10-14). No grant lifts it.
    pub(crate) fn deny_keychain_services(&mut self) {
        self.keychain_services_denied = true;
    }

    /// The folder whose listing is denied, if any.
    pub(crate) fn unlistable_home(&self) -> Option<&AbsolutePathBuf> {
        self.home.as_ref().filter(|_| self.home_listing_denied)
    }

    /// Every path made read-only.
    pub(crate) fn read_only_paths(&self) -> impl Iterator<Item = &AbsolutePathBuf> {
        self.never_lifted_paths().chain(&self.read_only)
    }

    /// #584: the read-only paths an Aggressive grant lifts (start-up files,
    /// git hooks and config): every read-only path but those no grant lifts.
    pub(crate) fn liftable_read_only_paths(&self) -> impl Iterator<Item = &AbsolutePathBuf> {
        self.read_only.iter()
    }

    /// Issue #513: the Corbanu home's runtime entries (`tmp` holds the
    /// sandbox launchers), read-only wherever the rules apply.
    pub(crate) fn runtime_read_only_paths(&self) -> impl Iterator<Item = &AbsolutePathBuf> {
        self.runtime_read_only.iter()
    }

    /// #583: the folder inside the read-only Corbanu home that stays
    /// writable where the profile can write it (`worktrees`).
    pub(crate) fn home_writable_paths(&self) -> impl Iterator<Item = &AbsolutePathBuf> {
        self.home_writable.iter()
    }

    /// Issue #513: the read-only paths no grant lifts (#442): the runtime
    /// entries and the project's `.codex` and `.agents`.
    pub(crate) fn never_lifted_paths(&self) -> impl Iterator<Item = &AbsolutePathBuf> {
        self.runtime_read_only
            .iter()
            .chain(&self.never_lifted)
            .chain(&self.moderate_registry)
    }

    /// `path` with its nearest existing folder resolved (links followed),
    /// as read-only paths are recorded.
    pub(crate) fn real_location(path: &AbsolutePathBuf) -> Option<AbsolutePathBuf> {
        real_location(path)
    }

    /// Issue #245: these read denials alone, without the profile's own
    /// entries, as a matcher (subtrees and patterns): what a command's
    /// additional permissions may never open. A malformed pattern matches
    /// every path (fail closed).
    pub(crate) fn matcher(&self) -> Option<ReadDenyMatcher> {
        let entries: Vec<FileSystemSandboxEntry> =
            self.paths()
                .map(|path| {
                    FileSystemSandboxEntry::new(
                        FileSystemPath::Path { path: path.clone() },
                        FileSystemAccessMode::Deny,
                    )
                })
                .chain(self.globs.iter().map(|path| {
                    FileSystemSandboxEntry::new(path.clone(), FileSystemAccessMode::Deny)
                }))
                .collect();
        let cwd = self
            .cwd
            .as_ref()
            .map_or_else(|| Path::new("/"), AbsolutePathBuf::as_path);
        ReadDenyMatcher::new(&FileSystemSandboxPolicy::restricted(entries), cwd)
    }

    /// Every denied path, existing first.
    pub(crate) fn paths(&self) -> impl Iterator<Item = &AbsolutePathBuf> {
        self.existing.iter().chain(&self.fixed)
    }

    /// `profile` with these reads denied. A profile without a sandbox gets
    /// one that keeps its full write and network access; an external
    /// sandbox cannot take extra rules and is returned as `None`.
    pub(crate) fn apply(&self, profile: &PermissionProfile) -> Option<PermissionProfile> {
        if matches!(profile, PermissionProfile::External { .. }) {
            return None;
        }
        // #603: `worktrees` is writable only as the real folder checked in
        // `protect_home`. Its location is recorded for the sandboxes, which
        // grant writes where a root was first seen (#536): a link swapped in
        // before the command's sandbox is set up (by a command a person
        // approved, or another session) then refuses the command instead of
        // opening the link's target (Linux: bound with `--bind-fd` there).
        if let Some(real) = &self.home_writable_real {
            for path in &self.home_writable {
                codex_sandboxing::writable_root_ledger::record_checked_root(
                    path.as_path(),
                    real.as_path(),
                );
            }
        }
        let (mut file_system, network) = profile.to_runtime_permissions();
        let entries: Vec<FileSystemSandboxEntry> =
            self.existing
                .iter()
                .map(|path| {
                    FileSystemSandboxEntry::new(
                        FileSystemPath::Path { path: path.clone() },
                        FileSystemAccessMode::Deny,
                    )
                })
                .chain(self.fixed.iter().map(|path| {
                    FileSystemSandboxEntry::skip_missing_path(
                        FileSystemPath::Path { path: path.clone() },
                        FileSystemAccessMode::Deny,
                    )
                }))
                .chain(self.globs.iter().map(|path| {
                    FileSystemSandboxEntry::new(path.clone(), FileSystemAccessMode::Deny)
                }))
                .chain(
                    self.unlistable_home()
                        .map(FileSystemSandboxEntry::deny_directory_listing),
                )
                .chain(
                    self.unmovable
                        .iter()
                        .map(FileSystemSandboxEntry::deny_moving),
                )
                .chain(
                    self.keychain_services_denied
                        .then(FileSystemSandboxEntry::deny_keychain_services),
                )
                .chain(self.moderate_marker.iter().map(|path| {
                    FileSystemSandboxEntry::new(
                        FileSystemPath::Path { path: path.clone() },
                        FileSystemAccessMode::Deny,
                    )
                }))
                .collect();
        if entries.is_empty()
            && self.read_only.is_empty()
            && self.runtime_read_only.is_empty()
            && self.never_lifted.is_empty()
            && self.moderate_registry.is_empty()
        {
            return Some(profile.clone());
        }
        // Turns an unrestricted policy into full write access plus the
        // denials, and appends them to a restricted one. Read-only paths
        // alone (#540) turn it into full write access too.
        if file_system.kind == FileSystemSandboxKind::Unrestricted && entries.is_empty() {
            file_system = FileSystemSandboxPolicy::restricted(vec![FileSystemSandboxEntry::new(
                FileSystemPath::Special {
                    value: FileSystemSpecialPath::Root,
                },
                FileSystemAccessMode::Write,
            )]);
        }
        file_system
            .preserve_deny_read_restrictions_from(&FileSystemSandboxPolicy::restricted(entries));
        if file_system.kind != FileSystemSandboxKind::Restricted {
            return None;
        }
        // Only narrows: a path becomes read-only where the profile would let
        // the command write it, never readable where it was not.
        let cwd = self
            .cwd
            .as_ref()
            .map_or_else(|| Path::new("/"), AbsolutePathBuf::as_path);
        let readable = Readability::new(&file_system, cwd);
        // #652: the profile's writable roots reached through a nested link,
        // and (#907) its entries normalized, computed once for every path
        // below.
        let writes = file_system.effective_writes_with_cwd(cwd);
        // #583: what stays writable inside the read-only Corbanu home, where
        // the profile could write it (more specific than the home's entry).
        let home_writable: Vec<FileSystemSandboxEntry> = self
            .home_writable
            .iter()
            // #652: under every reading of the profile (a folder it names
            // in two spellings with different access stays read-only).
            .filter(|path| {
                can_write_with(&file_system, path.as_path(), cwd, &writes)
                    && writes.can_grant_write_effective_path(path.as_path())
            })
            .map(|path| {
                FileSystemSandboxEntry::skip_missing_path(
                    FileSystemPath::Path { path: path.clone() },
                    FileSystemAccessMode::Write,
                )
            })
            .collect();
        let read_only: Vec<FileSystemSandboxEntry> = self
            .read_only_paths()
            .filter(|path| may_write(&file_system, path.as_path(), cwd, &writes))
            .flat_map(|path| {
                // #518: a `Read` entry more specific than a denial would
                // reopen it. A read-only path that is denied in any spelling
                // (its real location under a denial spelled through a linked
                // home, or a link to a denied file) is denied here too.
                let access = if readable.allows(path.as_path()) {
                    FileSystemAccessMode::Read
                } else {
                    FileSystemAccessMode::Deny
                };
                // #652: also spelled through each writable root reached
                // through a nested link, the spelling the sandboxes honor
                // there.
                std::iter::once(path.clone())
                    .chain(writes.links().spellings(path.as_path(), cwd))
                    .map(move |path| {
                        FileSystemSandboxEntry::skip_missing_path(
                            FileSystemPath::Path { path },
                            access,
                        )
                    })
                    .collect::<Vec<_>>()
            })
            .collect();
        file_system.entries.extend(read_only);
        file_system.entries.extend(home_writable);
        Some(PermissionProfile::from_runtime_permissions(
            &file_system,
            network,
        ))
    }
}

/// Whether `policy` lets a command write `path`, as written or through a
/// writable root spelled by an alias of its location (#619: Seatbelt matches
/// a root spelled `/tmp` or `/var/...` at `/private/...`, where the Corbanu
/// home and these paths are recorded) or reached through a nested link
/// (#652).
fn can_write(policy: &FileSystemSandboxPolicy, path: &Path, cwd: &Path) -> bool {
    can_write_with(policy, path, cwd, &policy.effective_writes_with_cwd(cwd))
}

/// [`can_write`] with `policy`'s entries normalized and its roots reached
/// through a nested link computed once (`writes`, #907), for many paths.
fn can_write_with(
    policy: &FileSystemSandboxPolicy,
    path: &Path,
    cwd: &Path,
    writes: &EffectiveWrites<'_>,
) -> bool {
    policy.can_write_path_with_cwd(path, cwd) || writes.can_write_effective_path(path)
}

/// [`can_write`], or (#605) through the macOS platform defaults' scratch
/// folders, which `:minimal` reads make writable outside the policy's own
/// roots: where a path becomes read-only. Never used to grant a write.
fn may_write(
    policy: &FileSystemSandboxPolicy,
    path: &Path,
    cwd: &Path,
    writes: &EffectiveWrites<'_>,
) -> bool {
    can_write_with(policy, path, cwd, writes) || platform_tmp_writable(policy, path)
}

/// #605: whether `path` is in a scratch folder the macOS platform defaults
/// (`:minimal` reads) let commands write, by its real location.
fn platform_tmp_writable(policy: &FileSystemSandboxPolicy, path: &Path) -> bool {
    if !cfg!(target_os = "macos") || !policy.include_platform_defaults() {
        return false;
    }
    AbsolutePathBuf::from_absolute_path(path)
        .ok()
        .and_then(|path| real_location(&path))
        .is_some_and(|real| {
            ["/private/tmp", "/private/var/tmp"]
                .iter()
                .any(|folder| real.as_path().starts_with(folder))
        })
}

/// #562 (Linux): creates the Corbanu home's missing runtime folders, empty,
/// as Corbanu would. They are then held read-only as they are, so bubblewrap
/// never puts a placeholder file where Corbanu expects a folder (a skill or
/// plugin install while a command runs, or one left behind). A runtime
/// entry that is a link to a missing folder is left as it is (its target
/// gets the placeholder). `AGENTS.md`, a file, keeps its placeholder: an
/// empty one adds no instructions.
pub(crate) fn create_missing_runtime_folders(codex_home: &Path) {
    for entry in CORBANU_READ_ONLY {
        if *entry != DEFAULT_AGENTS_MD {
            // Fails harmlessly where it exists (also as a link) or the home
            // is missing; the entry is protected either way.
            let _ = std::fs::create_dir(codex_home.join(entry));
        }
    }
}

/// #611: the database folder, when it is not the Corbanu home (by any
/// spelling).
fn separate_database_folder(codex_home: &Path, sqlite_home: &Path) -> Option<AbsolutePathBuf> {
    let folder = AbsolutePathBuf::from_absolute_path(sqlite_home).ok()?;
    let same_folder = sqlite_home == codex_home
        || std::fs::canonicalize(sqlite_home)
            .is_ok_and(|real| std::fs::canonicalize(codex_home).is_ok_and(|home| home == real));
    (!same_folder).then_some(folder)
}

/// #651 item 6: the folders many programs share, which a database folder in
/// them would otherwise deny whole: `/tmp`, `/var/tmp` (and their macOS
/// locations below `/private`), the temporary folder (`TMPDIR`) and the
/// user's home folder itself. By spelling or by real location; a folder
/// inside one of them is not shared.
fn is_well_known_shared_folder(folder: &Path, home: Option<&Path>, temp: &Path) -> bool {
    let real = |path: &Path| std::fs::canonicalize(path).ok();
    let folder_real = real(folder);
    ["/tmp", "/var/tmp", "/private/tmp", "/private/var/tmp"]
        .into_iter()
        .map(Path::new)
        .chain(std::iter::once(temp))
        .chain(home)
        // A relative `TMPDIR` would resolve against Corbanu's own folder;
        // `/` is never shared (its pattern isn't usable).
        .filter(|shared| shared.is_absolute() && shared.parent().is_some())
        .any(|shared| {
            folder.components().eq(shared.components())
                || folder_real
                    .as_ref()
                    .is_some_and(|folder| real(shared).is_some_and(|shared| shared == *folder))
        })
}

/// #611 (Linux): creates a separate database folder's missing folders and
/// GPU controller lock, empty and private, as Corbanu would. The init lock
/// keeps #608's placeholder handling. #651: the folder itself too, where it
/// is missing (private; the folders above it as state start-up makes them),
/// unless the nearest existing folder above it is reached through a link: a
/// command could have planted one there, and the folder would then be made
/// wherever it points. It is denied either way.
pub(crate) fn create_missing_database_folder(folder: &Path) {
    #[cfg(unix)]
    let builder = {
        let mut builder = std::fs::DirBuilder::new();
        std::os::unix::fs::DirBuilderExt::mode(&mut builder, 0o700);
        builder
    };
    #[cfg(not(unix))]
    let builder = std::fs::DirBuilder::new();
    if std::fs::symlink_metadata(folder).is_err() && !reached_through_link(folder) {
        // Fail harmlessly where they cannot be made.
        if let Some(parent) = folder.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let _ = builder.create(folder);
    }
    for name in ["accounting-pending", "db-backups"] {
        // Fails harmlessly where it exists (also as a link).
        let _ = builder.create(folder.join(name));
    }
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
    let _ = options.open(folder.join("gpu-controller.lock"));
}

/// #651: whether the nearest existing folder above `path` is not its real
/// location (a link on the way). Unknown counts as reached through a link.
fn reached_through_link(path: &Path) -> bool {
    let Some(existing) = path.ancestors().skip(1).find(|ancestor| ancestor.exists()) else {
        return true;
    };
    !std::fs::canonicalize(existing).is_ok_and(|real| real == existing)
}

/// #603 (Linux): creates the missing folders Corbanu reads later, empty and
/// private (as the socket folders need), and its lock files, for a command
/// working in the home: held read-only as they are, so bubblewrap puts no
/// placeholder there.
fn create_missing_read_later_folders(codex_home: &Path) {
    #[cfg(unix)]
    let builder = {
        let mut builder = std::fs::DirBuilder::new();
        std::os::unix::fs::DirBuilderExt::mode(&mut builder, 0o700);
        builder
    };
    #[cfg(not(unix))]
    let builder = std::fs::DirBuilder::new();
    for folder in CORBANU_READ_LATER_FOLDERS {
        // Fails harmlessly where it exists (also as a link).
        let _ = builder.create(codex_home.join(folder));
    }
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
    for lock in CORBANU_READ_LATER_LOCKS {
        let _ = options.open(codex_home.join(lock));
    }
}

/// #838 (Linux): creates the Corbanu home's missing security locks, empty
/// and private, as Corbanu would.
fn create_missing_security_locks(codex_home: &Path) {
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
    for lock in CORBANU_SECURITY_LOCKS {
        // Fails harmlessly where it exists (also as a link).
        let _ = options.open(codex_home.join(lock));
    }
}

/// Each spelling of `path` the sandboxes compare, as read-only paths are
/// recorded: its real location, and on macOS the link name too.
fn spellings(path: &AbsolutePathBuf) -> Vec<AbsolutePathBuf> {
    let mut spellings: Vec<AbsolutePathBuf> = real_location(path).into_iter().collect();
    if cfg!(target_os = "macos") && !spellings.contains(path) {
        spellings.push(path.clone());
    }
    spellings
}

/// #583/#603: whether a kept folder (by any spelling) is the Corbanu home or
/// a folder of it outside `worktrees`: then the home stays writable and only
/// the names Corbanu reads later are protected in it. Only a real
/// `worktrees` folder is left out: a link there (to the home, or above it)
/// would otherwise switch the protection off (#603 review).
fn works_in_home(codex_home: &AbsolutePathBuf, keep: &[AbsolutePathBuf]) -> bool {
    let homes = spellings(codex_home);
    let worktrees_path = codex_home.join(HOME_WRITABLE);
    let real_folder = std::fs::symlink_metadata(worktrees_path.as_path())
        .is_ok_and(|metadata| metadata.file_type().is_dir());
    // Only spellings strictly inside the home: a link swapped in after the
    // check above resolves to the home or outside it.
    let worktrees: Vec<AbsolutePathBuf> = if real_folder {
        spellings(&worktrees_path)
            .into_iter()
            .filter(|worktrees| {
                homes.iter().any(|home| {
                    worktrees.as_path() != home.as_path()
                        && worktrees.as_path().starts_with(home.as_path())
                })
            })
            .collect()
    } else {
        Vec::new()
    };
    keep.iter().flat_map(spellings).any(|kept| {
        homes
            .iter()
            .any(|home| kept.as_path().starts_with(home.as_path()))
            && !worktrees
                .iter()
                .any(|worktrees| kept.as_path().starts_with(worktrees.as_path()))
    })
}

/// Whether a folder of `path` below the top-level ones is a link (top-level
/// aliases such as `/tmp` on macOS are not counted), as Seatbelt tells them.
fn has_nested_link(path: &Path) -> bool {
    path.ancestors()
        .filter(|ancestor| ancestor.parent().and_then(Path::parent).is_some())
        .any(|ancestor| {
            std::fs::symlink_metadata(ancestor)
                .is_ok_and(|metadata| metadata.file_type().is_symlink())
        })
}

/// #603: where a kept folder really is, resolved only through its top-level
/// folder (an alias such as `/tmp` on macOS): `None` for one that is or sits
/// below a link. No link below the top-level folder is followed, so one
/// swapped in after [`has_nested_link`] looked (review round 4) cannot
/// redirect the folder to what it points to (`build -> ~/.ssh`).
fn kept_real_location(kept: &Path) -> Option<PathBuf> {
    if has_nested_link(kept) {
        return None;
    }
    let mut components = kept.components();
    let mut real = PathBuf::from(components.next()?.as_os_str());
    if let Some(top) = components.next() {
        real = std::fs::canonicalize(real.join(top)).ok()?;
        let rest = components.as_path();
        if !rest.as_os_str().is_empty() {
            real.push(rest);
        }
    }
    // As `canonicalize` did: only a folder that exists.
    std::fs::symlink_metadata(&real).ok()?;
    Some(real)
}

/// Whether kept folder `kept` (resolved by [`kept_real_location`]) is
/// `real` or inside it; on macOS in any case, as `canonicalize` reported
/// the stored case before (a working folder typed in another case).
fn kept_within(kept: &Path, real: &Path) -> bool {
    #[cfg(unix)]
    {
        codex_protocol::permissions::location_within(kept, real)
    }
    #[cfg(not(unix))]
    {
        kept.starts_with(real)
    }
}

/// #766: a marker or registry path by its real location, as the sandboxes
/// match it (bubblewrap refuses a deny that crosses a writable link, as a
/// home spelled through a link would), and on macOS also as given.
fn moderate_spellings(path: &Path) -> Vec<AbsolutePathBuf> {
    let Ok(path) = AbsolutePathBuf::from_absolute_path(path) else {
        return Vec::new();
    };
    let mut spellings: Vec<AbsolutePathBuf> = real_location(&path).into_iter().collect();
    if cfg!(target_os = "macos") && !spellings.contains(&path) {
        spellings.push(path);
    }
    spellings
}

/// `path` with its nearest existing folder resolved, or `None` when that
/// folder is not a folder (nothing can be created below a file).
fn real_location(path: &AbsolutePathBuf) -> Option<AbsolutePathBuf> {
    real_location_within(path, /*hops*/ 8)
}

fn real_location_within(path: &AbsolutePathBuf, hops: usize) -> Option<AbsolutePathBuf> {
    let mut existing = path.as_path();
    let mut tail = Vec::new();
    while std::fs::symlink_metadata(existing).is_err() {
        tail.push(existing.file_name()?);
        existing = existing.parent()?;
    }
    // A link (the path or a folder above it) whose target does not exist
    // yet: protect the path below the target.
    if std::fs::metadata(existing).is_err()
        && let Ok(target) = std::fs::read_link(existing)
    {
        let mut next = AbsolutePathBuf::resolve_path_against_base(target, existing.parent()?);
        for name in tail.iter().rev() {
            next = next.join(name);
        }
        return real_location_within(&next, hops.checked_sub(1)?);
    }
    if !tail.is_empty() && !existing.is_dir() {
        return None;
    }
    let mut real = std::fs::canonicalize(existing).ok()?;
    for name in tail.iter().rev() {
        real.push(name);
    }
    AbsolutePathBuf::from_absolute_path(real).ok()
}

/// In-process file tools (the patch pre-check, structured edits, image
/// viewing, extension tools) read files with the same denials after untrusted
/// content, and from the start under a protected level (Moderate/Aggressive).
/// No approval lifts them here.
pub(crate) fn protect_file_tool_context(
    session: &crate::session::session::Session,
    turn: &crate::session::turn_context::TurnContext,
    mut context: codex_file_system::FileSystemSandboxContext,
) -> codex_file_system::FileSystemSandboxContext {
    let Some(state) = session
        .services
        .model_client()
        .post_taint_state()
        .filter(crate::security::tainted_action::PostTaintState::protected_paths_apply)
    else {
        return context;
    };
    let aggressive = state.level == codex_security_policy::SecurityLevel::Aggressive;
    let Ok(profile) = PermissionProfile::try_from(context.permissions.clone()) else {
        // Unreadable permissions: no file access at all (fail closed).
        context.permissions = PermissionProfile::from_runtime_permissions(
            &FileSystemSandboxPolicy::restricted(Vec::new()),
            codex_protocol::permissions::NetworkSandboxPolicy::Restricted,
        )
        .into();
        return context;
    };
    #[allow(deprecated)]
    let turn_cwd = turn.cwd.clone();
    let workspace_roots: Vec<AbsolutePathBuf> = context
        .workspace_roots
        .iter()
        .filter_map(|root| root.to_abs_path().ok())
        .collect();
    // Keep roots come from the turn's own profile: extra permissions granted
    // to this call never make a denied folder readable.
    let mut denials = ReadDenials::for_turn(
        turn.config.codex_home.as_path(),
        turn.config.sqlite.home(),
        &turn_cwd,
        &workspace_roots,
        turn.config.permissions.permission_profile(),
    );
    // #506: file tools list folders inside the sandbox, so the same denial.
    if aggressive {
        denials.deny_home_listing();
    }
    if let Some(protected) = denials.apply(&profile) {
        context.permissions = protected.into();
    }
    context
}

/// The read policy for host code that reads a model-named file (Codex Apps
/// uploads) once the protected-path rules apply: `None` before that. A
/// profile that cannot take the rules reads nothing.
pub(crate) fn post_taint_read_policy(
    session: &crate::session::session::Session,
    turn: &crate::session::turn_context::TurnContext,
) -> Option<FileSystemSandboxPolicy> {
    session
        .services
        .model_client()
        .post_taint_state()
        .filter(crate::security::tainted_action::PostTaintState::protected_paths_apply)?;
    #[allow(deprecated)]
    let turn_cwd = turn.cwd.clone();
    let workspace_roots: Vec<AbsolutePathBuf> = turn
        .environments
        .primary()
        .map(|environment| {
            environment
                .workspace_roots()
                .iter()
                .filter_map(|root| root.to_abs_path().ok())
                .collect()
        })
        .unwrap_or_default();
    let profile = turn.config.permissions.permission_profile();
    let denials = ReadDenials::for_turn(
        turn.config.codex_home.as_path(),
        turn.config.sqlite.home(),
        &turn_cwd,
        &workspace_roots,
        profile,
    );
    Some(
        denials
            .apply(profile)
            .map(|protected| protected.file_system_sandbox_policy())
            .unwrap_or_else(|| FileSystemSandboxPolicy::restricted(Vec::new())),
    )
}

/// Whether `path` (and what it resolves to) may be read under `policy`.
///
/// What `path` resolves to is also checked against the policy's paths with
/// links resolved alike (#512): a denial spelled through a link (`/tmp` and
/// `/var` on macOS, a linked home) never names the resolved target, so a
/// link to a denied file, or the file under its real name, passed. Pattern
/// denials and denials relative to the working folder (resolved against its
/// real location too) apply as well (#519). This only adds checks: it never
/// makes a path readable.
pub(crate) fn readable_under(policy: &FileSystemSandboxPolicy, path: &Path, cwd: &Path) -> bool {
    Readability::new(policy, cwd).allows(path)
}

/// [`readable_under`] for one policy and folder, built once for many paths.
struct Readability<'a> {
    policy: &'a FileSystemSandboxPolicy,
    resolved: FileSystemSandboxPolicy,
    cwds: [std::path::PathBuf; 2],
    patterns: Option<ReadDenyMatcher>,
    resolved_patterns: [Option<ReadDenyMatcher>; 2],
}

impl<'a> Readability<'a> {
    fn new(policy: &'a FileSystemSandboxPolicy, cwd: &Path) -> Self {
        let resolved = with_real_locations(policy);
        let real_cwd = std::fs::canonicalize(cwd).unwrap_or_else(|_| cwd.to_path_buf());
        let cwds = [cwd.to_path_buf(), real_cwd];
        Self {
            patterns: pattern_matcher(policy, cwd),
            resolved_patterns: [
                pattern_matcher(&resolved, &cwds[0]),
                pattern_matcher(&resolved, &cwds[1]),
            ],
            policy,
            resolved,
            cwds,
        }
    }

    fn allows(&self, path: &Path) -> bool {
        let cwd = self.cwds[0].as_path();
        let path = AbsolutePathBuf::resolve_path_against_base(path, cwd);
        let path = path.as_path();
        if !self.policy.can_read_path_with_cwd(path, cwd)
            || self
                .patterns
                .as_ref()
                .is_some_and(|matcher| matcher.is_read_denied(path))
        {
            return false;
        }
        // A dangling link (or a path not created yet) is checked where it
        // would lead; when that cannot be told, it is not readable.
        let Some(real) = std::fs::canonicalize(path)
            .ok()
            .or_else(|| missing_target(path))
        else {
            return false;
        };
        self.cwds
            .iter()
            .zip(&self.resolved_patterns)
            .all(|(cwd, patterns)| {
                self.policy.can_read_path_with_cwd(&real, cwd)
                    && self.resolved.can_read_path_with_cwd(&real, cwd)
                    && !patterns
                        .as_ref()
                        .is_some_and(|matcher| matcher.is_read_denied(&real))
            })
    }
}

/// Where a missing `path` would lead once created: each dangling link on
/// the way resolved by the system one hop at a time (a target's `..` is
/// resolved through the folders it names, not lexically). `None` when that
/// cannot be told (too many links).
fn missing_target(path: &Path) -> Option<std::path::PathBuf> {
    let mut path = path.to_path_buf();
    for _ in 0..8 {
        let mut existing = path.as_path();
        let mut tail = Vec::new();
        while std::fs::symlink_metadata(existing).is_err() {
            tail.push(existing.file_name()?);
            existing = existing.parent()?;
        }
        let next = match std::fs::canonicalize(existing) {
            Ok(real) => {
                return Some(tail.iter().rev().fold(real, |path, name| path.join(name)));
            }
            Err(_) => existing.parent()?.join(std::fs::read_link(existing).ok()?),
        };
        path = tail.iter().rev().fold(next, |path, name| path.join(name));
    }
    None
}

/// The pattern (glob) denials of `policy`, or `None` without any. A
/// malformed pattern matches every path (fails closed).
fn pattern_matcher(policy: &FileSystemSandboxPolicy, cwd: &Path) -> Option<ReadDenyMatcher> {
    let patterns: Vec<FileSystemSandboxEntry> = policy
        .entries
        .iter()
        .filter(|entry| {
            entry.access == FileSystemAccessMode::Deny
                && matches!(entry.path, FileSystemPath::GlobPattern { .. })
        })
        .cloned()
        .collect();
    if patterns.is_empty() {
        return None;
    }
    ReadDenyMatcher::new(&FileSystemSandboxPolicy::restricted(patterns), cwd)
}

/// `policy` with each path entry also under its real location, with the
/// same access, so the most specific entry still wins once links resolve.
/// An absolute pattern gets a copy with its literal folder resolved too.
///
/// An allow is copied only where it does not land under a denial it was not
/// already inside (#518): a `Read` entry for a link (a workspace's `.agents`
/// pointing at `~/.ssh/id_ed25519`) must not reopen what the link targets.
/// A carve-out inside a denied folder (`vault/open.txt` in a denied `vault`)
/// keeps applying under the folder's real location.
fn with_real_locations(policy: &FileSystemSandboxPolicy) -> FileSystemSandboxPolicy {
    let mut resolved = policy.clone();
    let mut denied: Vec<(AbsolutePathBuf, AbsolutePathBuf)> = Vec::new();
    for entry in &policy.entries {
        if entry.access == FileSystemAccessMode::Deny
            && let FileSystemPath::Path { path } = &entry.path
        {
            let real = real_location(path).unwrap_or_else(|| path.clone());
            denied.push((path.clone(), real));
        }
    }
    for entry in &policy.entries {
        let real = match &entry.path {
            FileSystemPath::Path { path } => real_location(path)
                .filter(|real| real != path)
                .filter(|real| {
                    entry.access == FileSystemAccessMode::Deny
                        || denied
                            .iter()
                            .filter(|(spelled, real_denied)| {
                                real.as_path().starts_with(spelled.as_path())
                                    || real.as_path().starts_with(real_denied.as_path())
                            })
                            .all(|(spelled, real_denied)| {
                                path.as_path().starts_with(spelled.as_path())
                                    || path.as_path().starts_with(real_denied.as_path())
                            })
                })
                .map(|path| FileSystemPath::Path { path }),
            FileSystemPath::GlobPattern { pattern, scan } => {
                real_pattern(pattern).map(|pattern| FileSystemPath::GlobPattern {
                    pattern,
                    scan: scan.as_ref().map(|scan| match scan {
                        codex_protocol::permissions::FileSystemGlobScan::DatabaseEntries {
                            directory,
                        } => codex_protocol::permissions::FileSystemGlobScan::DatabaseEntries {
                            directory: real_location(directory)
                                .unwrap_or_else(|| directory.clone()),
                        },
                    }),
                })
            }
            FileSystemPath::Special { .. } => None,
        };
        if let Some(path) = real {
            resolved.entries.push(FileSystemSandboxEntry {
                path,
                ..entry.clone()
            });
        }
    }
    resolved
}

/// `pattern` with the folder before its first wildcard at its real
/// location, when that differs.
fn real_pattern(pattern: &str) -> Option<String> {
    let wildcard = pattern.find(['*', '?', '[', '{'])?;
    let split = pattern[..wildcard].rfind('/')?;
    let folder = AbsolutePathBuf::from_absolute_path(Path::new(&pattern[..split.max(1)])).ok()?;
    let real = real_location(&folder).filter(|real| *real != folder)?;
    // #651: escaped, should the real location hold glob syntax.
    let real = crate::security::preflight::glob_escape_path(real.as_path());
    Some(format!(
        "{}{}",
        real.trim_end_matches('/'),
        &pattern[split..]
    ))
}

#[cfg(test)]
#[path = "read_denials_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "read_denials_907_tests.rs"]
mod issue_907_tests;

#[cfg(test)]
#[path = "read_denials_732_tests.rs"]
mod issue_732_tests;
