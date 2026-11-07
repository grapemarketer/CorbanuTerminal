//! PF-27-S02 secretless agent launch contract (feature `secretless_agent_launch`).
//!
//! Once armed, every agent command launch must:
//! - run under an OS sandbox on a platform whose containment is verified here
//!   (macOS Seatbelt, Linux bubblewrap/seccomp, and on Windows the elevated
//!   sandbox, which runs commands as a separate user; PF-27-S06);
//! - deny reads of the vault store, sign-in files, wallet and broker runtime
//!   directory, and deny writes to `CODEX_HOME` (the policy store);
//! - carry no raw managed secret in its environment, argv or stdin, and not
//!   start a login shell that would re-load profile secrets.
//!
//! The environment allowlist itself lives in
//! [`codex_protocol::secretless_launch`] so every launcher that builds an
//! environment from the process applies it. Brokered credentials are added
//! back afterwards as dummies by the network proxy (it sources the raw values
//! from Core, not from the stripped child environment).

use codex_protocol::models::PermissionProfile;
use codex_protocol::permissions::FileSystemAccessMode;
use codex_protocol::permissions::FileSystemPath;
use codex_protocol::permissions::FileSystemSandboxEntry;
use codex_protocol::permissions::FileSystemSandboxKind;
use codex_protocol::permissions::FileSystemSandboxPolicy;
use codex_protocol::permissions::NetworkSandboxPolicy;
use codex_protocol::permissions::ReadDenyMatcher;
use codex_protocol::secretless_launch;
use codex_sandboxing::SandboxCommand;
use codex_sandboxing::SandboxType;
use codex_sandboxing::policy_transforms::effective_permission_profile;
use codex_utils_absolute_path::AbsolutePathBuf;
use std::collections::HashMap;
use std::path::Path;
use std::sync::OnceLock;
use zeroize::Zeroizing;

/// The local group the elevated Windows sandbox's users belong to.
#[cfg(windows)]
const WINDOWS_SANDBOX_USERS_GROUP: &str = "CodexSandboxUsers";

/// Shorter values are too likely to collide with ordinary text.
const MIN_MANAGED_VALUE_BYTES: usize = 8;

/// Files and directories under `CODEX_HOME` that agent commands may not read:
/// credential stores, the broker runtime directory, and records that can hold
/// secrets from earlier sessions (shell snapshots, logs, transcripts, history).
const PROTECTED_CODEX_HOME_ENTRIES: &[&str] = &[
    "secrets",
    "auth.json",
    ".credentials.json",
    ".env",
    "provider_auth.json",
    "config.toml",
    "managed_config.toml",
    "wallet",
    "run",
    "shell_snapshots",
    "log",
    "sessions",
    "archived_sessions",
    "history.jsonl",
    // #651: the database init lock (#586); a command holding it would stall
    // every new launch on this home.
    super::preflight::DB_INIT_LOCK,
    // #1107: the rest of what Corbanu keeps beside the databases
    // (`preflight::DATABASE_FOLDER_ENTRIES`), as in a separate database
    // folder: the GPU controller's lock, pending accounting records and the
    // databases recovery set aside.
    "gpu-controller.lock",
    "accounting-pending",
    "db-backups",
];

/// The entries of [`PROTECTED_CODEX_HOME_ENTRIES`] that are directories. On
/// Windows they are denied even before they exist (#294); files created
/// directly in `CODEX_HOME` are covered by its new-files deny instead.
const PROTECTED_CODEX_HOME_DIRS: &[&str] = &[
    "secrets",
    "wallet",
    "run",
    "shell_snapshots",
    "log",
    "sessions",
    "archived_sessions",
    "accounting-pending",
    "db-backups",
];

/// Credential files under `$HOME` that tools read for the user: CLI tokens
/// for GitHub, AWS, Docker, npm and git, `.netrc`, and Claude's sign-in.
const PROTECTED_HOME_ENTRIES: &[&str] = &[
    ".netrc",
    ".git-credentials",
    ".config/gh/hosts.yml",
    ".aws/credentials",
    ".docker/config.json",
    ".npmrc",
    ".claude/.credentials.json",
];

/// Set in protected launches so zsh does not source `~/.zshenv`, which can
/// export credentials; `/var/empty` holds no start-up files.
pub(crate) const PROTECTED_ZDOTDIR: &str = "/var/empty";

static ACTIVE: OnceLock<LaunchContract> = OnceLock::new();

/// #1077: what a file the contract keeps unreadable at every level is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum KeptFile {
    /// A CLI token ([`PROTECTED_HOME_ENTRIES`] and their Windows places).
    CliToken,
    /// The Google Cloud CLI's folder on Windows (`%APPDATA%\gcloud`), which
    /// holds its credentials and config.
    GoogleCloudCliFolder,
    /// A file or folder inside it.
    InGoogleCloudCliFolder,
    /// The macOS keychains folder.
    Keychain,
    /// Corbanu's own files: its sign-in, vault, config, history, databases
    /// and credential broker.
    CorbanuOwn,
}

/// Why a protected launch was refused. The text is shown to the user and model.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum LaunchDenied {
    UnsupportedPlatform,
    /// PF-27-S06: the unelevated Windows sandbox restricts writes only.
    WindowsUnelevatedSandbox,
    /// PF-27-S06: new files in `CODEX_HOME` could not be denied to the
    /// elevated sandbox's users (it is not set up yet, or the ACL failed).
    #[cfg_attr(not(windows), allow(dead_code))]
    WindowsSandboxNotSetUp,
    /// PF-27-S07: this process does not hold the armed lock, so a process
    /// with the flag off could remove that deny during a launch.
    #[cfg_attr(not(windows), allow(dead_code))]
    CodexHomeLockUnavailable,
    /// #323: this process is not registered as a session relying on the
    /// sandbox's deny-read entries, so another `CODEX_HOME`'s launch could
    /// remove one of them.
    #[cfg_attr(not(windows), allow(dead_code))]
    DenyReadSessionUnavailable,
    ProcessHardening,
    Unsandboxed,
    RemoteEnvironment,
    UnprotectableFileSystem,
    ProtectedPathReadable(String),
    PolicyStoreWritable,
    LoginShell,
    RawSecretInArgv,
    RawSecretInStdin,
}

impl std::fmt::Display for LaunchDenied {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let reason = match self {
            Self::UnsupportedPlatform => {
                "secretless agent launch is not available on this platform yet (PF-27-S06)".to_string()
            }
            Self::WindowsUnelevatedSandbox => {
                "the unelevated Windows sandbox cannot deny reads of the vault and sign-in files; set `sandbox = \"elevated\"` under `[windows]`"
                    .to_string()
            }
            Self::WindowsSandboxNotSetUp => {
                "the elevated Windows sandbox is not set up yet, so new files in Corbanu's configuration directory cannot be denied to it; set it up first"
                    .to_string()
            }
            Self::CodexHomeLockUnavailable => {
                "Corbanu could not lock its configuration directory (another session may be changing its protection), so new files there might not stay protected; try again"
                    .to_string()
            }
            Self::DenyReadSessionUnavailable => {
                "Corbanu could not register this session in %ProgramData%\\CorbanuTerminalSandbox\\deny-read-sessions, so another Corbanu session could remove the sandbox's read protections while yours runs; run the Windows sandbox setup again (it creates and repairs that folder) and try again"
                    .to_string()
            }
            Self::ProcessHardening => {
                "Corbanu could not protect its own process memory from agent commands".to_string()
            }
            Self::Unsandboxed => {
                "this command would run outside the OS sandbox, which protected launch does not allow"
                    .to_string()
            }
            Self::RemoteEnvironment => {
                "commands in a remote environment build their environment on the remote host, which this contract cannot check yet"
                    .to_string()
            }
            Self::UnprotectableFileSystem => {
                "the current permissions give commands unrestricted file access, so the vault and sign-in files cannot be protected"
                    .to_string()
            }
            Self::ProtectedPathReadable(path) => {
                format!("the sandbox would let this command read {path}")
            }
            Self::PolicyStoreWritable => {
                "the sandbox would let this command write Corbanu's configuration directory".to_string()
            }
            Self::LoginShell => {
                "login shells load profile files that can export secrets".to_string()
            }
            Self::RawSecretInArgv => "the command line contains a managed secret".to_string(),
            Self::RawSecretInStdin => "the input contains a managed secret".to_string(),
        };
        write!(f, "{LAUNCH_DENIED_PREFIX}{reason}.")
    }
}

const LAUNCH_DENIED_PREFIX: &str = "Protected launch refused: ";

/// True when an error message is a [`LaunchDenied`] refusal.
pub(crate) fn is_launch_denial(message: &str) -> bool {
    message.starts_with(LAUNCH_DENIED_PREFIX)
}

impl std::error::Error for LaunchDenied {}

/// Process-wide contract state captured when the feature is first armed.
pub(crate) struct LaunchContract {
    codex_home: AbsolutePathBuf,
    protected_read_paths: Vec<AbsolutePathBuf>,
    /// #1077: the CLI tokens among [`Self::protected_read_paths`].
    cli_token_paths: Vec<AbsolutePathBuf>,
    /// #1077: the keychain folders among [`Self::protected_read_paths`].
    keychain_paths: Vec<AbsolutePathBuf>,
    /// The Google Cloud CLI's folder among [`Self::protected_read_paths`]
    /// (Windows only).
    google_cloud_cli_folders: Vec<AbsolutePathBuf>,
    managed_values: Vec<Zeroizing<String>>,
    hardened: bool,
    /// #651: database folders set apart from `CODEX_HOME` (`sqlite_home`)
    /// that this process's configs use, noted once each is loaded. Added
    /// to, never removed.
    database_folders: std::sync::Mutex<Vec<AbsolutePathBuf>>,
    /// PF-27-S07: the armed lock on `CODEX_HOME`, held while this contract
    /// exists; protected launches are refused without it.
    #[cfg(windows)]
    armed_lock: std::sync::Mutex<ArmedLock>,
}

impl std::fmt::Debug for LaunchContract {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let mut debug = f.debug_struct("LaunchContract");
        debug
            .field("codex_home", &self.codex_home)
            .field("protected_read_paths", &self.protected_read_paths)
            .field("managed_values", &self.managed_values.len())
            .field("hardened", &self.hardened);
        #[cfg(windows)]
        {
            let locked = self.armed_lock.lock().is_ok_and(|lock| lock.locked);
            debug.field("armed_lock", &locked);
        }
        debug.finish()
    }
}

/// Arms the contract for this process: the environment allowlist, Core's own
/// process hardening, and launch checks. Idempotent and one-way.
pub(crate) fn arm(codex_home: &AbsolutePathBuf) {
    secretless_launch::arm();
    ACTIVE.get_or_init(|| {
        let hardened = harden_current_process();
        LaunchContract::capture(codex_home, std::env::vars_os(), hardened)
    });
}

/// Brokered variables (for example `GITHUB_TOKEN`) that the user's shell
/// environment policy would pass to agent commands, before the launch
/// allowlist runs and without the model-provider keys Core strips for the
/// shell tool (built-in ones and `provider_env_keys`). Only these may be
/// sourced from Core's environment as dummies.
pub(crate) fn policy_permitted_brokered_env_keys(
    policy: &codex_protocol::config_types::ShellEnvironmentPolicy,
    provider_env_keys: &[String],
) -> Vec<String> {
    permitted_brokered_env_keys(
        std::env::vars_os().filter_map(|(name, value)| {
            Some((name.into_string().ok()?, value.into_string().ok()?))
        }),
        policy,
        provider_env_keys,
    )
}

pub(crate) fn permitted_brokered_env_keys<I>(
    vars: I,
    policy: &codex_protocol::config_types::ShellEnvironmentPolicy,
    provider_env_keys: &[String],
) -> Vec<String>
where
    I: IntoIterator<Item = (String, String)>,
{
    let mut env = codex_protocol::shell_environment::create_env_from_vars(
        vars, policy, /*thread_id*/ None,
    );
    crate::exec_env::remove_provider_auth_env_vars(
        &mut env,
        provider_env_keys.iter().map(String::as_str),
    );
    codex_network_proxy::credential_broker_env_var_names()
        .into_iter()
        .filter(|name| env.contains_key(*name))
        .map(str::to_string)
        .collect()
}

/// The armed contract, if any.
pub(crate) fn active() -> Option<&'static LaunchContract> {
    ACTIVE.get()
}

/// Issue #218: applies the armed contract to an external agent CLI that
/// Corbanu launches itself (contained Claude panes): the platform, process
/// hardening and sandbox checks, the argv and environment checks, and
/// `profile` with the protected reads denied and `CODEX_HOME` read-only.
/// Fails when the contract is not armed (`secretless_agent_launch` off).
pub fn protect_external_agent_launch(
    sandbox: SandboxType,
    argv: &[String],
    env: &mut HashMap<String, String>,
    profile: &PermissionProfile,
    cwd: &Path,
) -> Result<PermissionProfile, String> {
    let contract = active().ok_or_else(|| {
        "contained external agents need the secretless agent launch contract (feature `secretless_agent_launch`)"
            .to_string()
    })?;
    contract
        .protect_external_launch(sandbox, argv, env, profile, cwd)
        .map_err(|err| err.to_string())
}

/// #651: the armed contract also protects the database folder `sqlite_home`
/// when it is set apart from `CODEX_HOME`. Called once a config is loaded.
pub(crate) fn protect_database_folder(sqlite_home: &Path) {
    if let Some(contract) = active() {
        contract.note_database_folder(sqlite_home);
    }
}

/// True once the secretless launch contract is armed in this process.
pub fn external_agent_contract_armed() -> bool {
    active().is_some()
}

impl LaunchContract {
    pub(crate) fn capture<I>(codex_home: &AbsolutePathBuf, vars: I, hardened: bool) -> Self
    where
        I: IntoIterator<Item = (std::ffi::OsString, std::ffi::OsString)>,
    {
        let mut managed_values: Vec<Zeroizing<String>> = Vec::new();
        for (name, value) in vars {
            let (Some(name), Some(value)) = (name.to_str(), value.to_str()) else {
                continue;
            };
            let value = value.trim();
            if secretless_launch::is_secret_looking_name(name)
                && value.len() >= MIN_MANAGED_VALUE_BYTES
                && !managed_values.iter().any(|known| known.as_str() == value)
            {
                managed_values.push(Zeroizing::new(value.to_string()));
            }
        }
        let mut protected_read_paths = PROTECTED_CODEX_HOME_ENTRIES
            .iter()
            .map(|entry| codex_home.join(entry))
            .collect::<Vec<_>>();
        // #1077: kept apart too, so the approval prompt can say what a kept
        // file is.
        let mut cli_token_paths = Vec::new();
        let mut keychain_paths = Vec::new();
        let mut google_cloud_cli_folders = Vec::new();
        // Windows has no HOME by default; its profile directory is USERPROFILE.
        let home = std::env::var_os("HOME").or_else(|| {
            cfg!(windows)
                .then(|| std::env::var_os("USERPROFILE"))
                .flatten()
        });
        if let Some(home) = home
            && let Ok(home) = AbsolutePathBuf::from_absolute_path(home)
        {
            cli_token_paths.extend(PROTECTED_HOME_ENTRIES.iter().map(|entry| home.join(entry)));
            if cfg!(target_os = "macos") {
                keychain_paths.push(home.join("Library/Keychains"));
            }
            if cfg!(windows) {
                cli_token_paths.push(home.join("_netrc"));
            }
        }
        if cfg!(windows)
            && let Some(app_data) = std::env::var_os("APPDATA")
            && let Ok(app_data) = AbsolutePathBuf::from_absolute_path(app_data)
        {
            cli_token_paths.push(app_data.join("GitHub CLI").join("hosts.yml"));
            google_cloud_cli_folders.push(app_data.join("gcloud"));
        }
        if cfg!(windows)
            && let Some(cargo_home) = std::env::var_os("CARGO_HOME").or_else(|| {
                std::env::var_os("USERPROFILE").map(|home| {
                    std::path::PathBuf::from(home)
                        .join(".cargo")
                        .into_os_string()
                })
            })
            && let Ok(cargo_home) = AbsolutePathBuf::from_absolute_path(cargo_home)
        {
            cli_token_paths.push(cargo_home.join("credentials.toml"));
            cli_token_paths.push(cargo_home.join("credentials"));
        }
        if let Some(dir) = std::env::var_os("CLAUDE_CONFIG_DIR")
            && let Ok(dir) = AbsolutePathBuf::from_absolute_path(dir)
        {
            cli_token_paths.push(dir.join(".credentials.json"));
        }
        protected_read_paths.extend(cli_token_paths.iter().cloned());
        protected_read_paths.extend(keychain_paths.iter().cloned());
        protected_read_paths.extend(google_cloud_cli_folders.iter().cloned());
        if let Some(dir) = codex_network_proxy::credential_broker_user_runtime_dir()
            && let Ok(dir) = AbsolutePathBuf::from_absolute_path(dir)
        {
            protected_read_paths.push(dir);
        }
        Self {
            codex_home: codex_home.clone(),
            protected_read_paths,
            cli_token_paths,
            keychain_paths,
            google_cloud_cli_folders,
            managed_values,
            hardened,
            database_folders: std::sync::Mutex::new(Vec::new()),
            #[cfg(windows)]
            armed_lock: std::sync::Mutex::new(ArmedLock::open(codex_home.as_path())),
        }
    }

    /// Applies the whole contract to one launch before its sandbox transform:
    /// platform and sandbox checks, argv and environment checks, and the
    /// returned profile with protected paths denied (verified again after the
    /// command's own additional permissions are merged).
    /// `windows_elevated`: the command will run under the elevated Windows
    /// sandbox backend (ignored elsewhere).
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn protect_launch(
        &self,
        sandbox: SandboxType,
        sandbox_requested: bool,
        exec_server: bool,
        windows_elevated: bool,
        command: &mut SandboxCommand,
        permissions: &PermissionProfile,
        cwd: &Path,
    ) -> Result<PermissionProfile, LaunchDenied> {
        let mut protect = || -> Result<PermissionProfile, LaunchDenied> {
            self.check_sandbox(sandbox, sandbox_requested, exec_server, windows_elevated)?;
            let mut argv = vec![command.program.to_string_lossy().into_owned()];
            argv.extend(command.args.iter().cloned());
            self.check_command(&argv, &mut command.env)?;
            let protected = self.protect_permissions(permissions, cwd)?;
            let merged =
                effective_permission_profile(&protected, command.additional_permissions.as_ref());
            self.verify_permissions(&merged, cwd)?;
            Ok(protected)
        };
        protect().inspect_err(super::inspection::record_launch_denial)
    }

    /// [`protect_external_agent_launch`] with this contract.
    pub(crate) fn protect_external_launch(
        &self,
        sandbox: SandboxType,
        argv: &[String],
        env: &mut HashMap<String, String>,
        profile: &PermissionProfile,
        cwd: &Path,
    ) -> Result<PermissionProfile, LaunchDenied> {
        let mut protect = || -> Result<PermissionProfile, LaunchDenied> {
            // External agents are not launched through the Windows sandbox.
            self.check_sandbox(
                sandbox, /*sandbox_requested*/ true, /*exec_server*/ false,
                /*windows_elevated*/ false,
            )?;
            self.check_command(argv, env)?;
            self.protect_permissions(profile, cwd)
        };
        protect().inspect_err(super::inspection::record_launch_denial)
    }

    /// PF-27-S06: a protected file replaced by rename (a token refresh) is a
    /// new file and would not carry its per-file deny, so every file created
    /// directly in `CODEX_HOME` inherits a read deny for the elevated
    /// sandbox's users (subdirectories such as `skills` are unaffected). The
    /// entry stays on the directory until a process starts with the flag off
    /// ([`release_codex_home_when_unarmed`]). Needs the sandbox's users group, which
    /// its setup creates; until then protected launches are refused. Checked
    /// on every launch (a no-op once present), so a setup rerun that resets
    /// the directory's ACL is repaired. A file moved in from another
    /// directory keeps its own ACL: protected writers create their temporary
    /// files inside `CODEX_HOME` (as `auth.json` storage does).
    #[cfg(windows)]
    pub(crate) fn protect_new_codex_home_files(&self) -> Result<(), LaunchDenied> {
        let Ok(mut armed_lock) = self.armed_lock.lock() else {
            return Err(LaunchDenied::CodexHomeLockUnavailable);
        };
        if !armed_lock.ensure_locked() {
            return Err(LaunchDenied::CodexHomeLockUnavailable);
        }
        let mut group = codex_windows_sandbox::resolve_sid(WINDOWS_SANDBOX_USERS_GROUP)
            .map_err(|_| LaunchDenied::WindowsSandboxNotSetUp)?;
        // #323. A setup from before the registry existed: this launch's
        // elevated setup creates it and then registers this process.
        if !armed_lock.ensure_registered()
            && codex_windows_sandbox::sandbox_setup_is_complete(self.codex_home.as_path())
        {
            return Err(LaunchDenied::DenyReadSessionUnavailable);
        }
        drop(armed_lock);
        // The group may not have existed when the lock file was opened.
        deny_armed_lock(
            &self.codex_home.as_path().join(ARMED_LOCK_FILE),
            group.as_mut_ptr().cast(),
        )
        .map_err(|_| LaunchDenied::CodexHomeLockUnavailable)?;
        // SAFETY: `group` holds a valid SID for the duration of the call.
        let present = unsafe {
            codex_windows_sandbox::add_deny_read_ace_for_new_files(
                self.codex_home.as_path(),
                group.as_mut_ptr().cast(),
            )
        };
        if !matches!(present, Ok(true)) {
            return Err(LaunchDenied::WindowsSandboxNotSetUp);
        }
        Ok(())
    }

    /// Whether Core's own process hardening succeeded when it was armed.
    pub(crate) fn hardened(&self) -> bool {
        self.hardened
    }

    /// Platform, process-hardening and sandbox checks for one launch attempt.
    /// `sandbox_requested` is the policy decision; `sandbox` is the concrete
    /// wrapper (`None` for exec-server launches, which apply it remotely).
    /// On Windows only the elevated backend (`windows_elevated`) can deny
    /// reads, so the unelevated one is refused (PF-27-S06). Exec-server
    /// launches are refused on every platform before that.
    pub(crate) fn check_sandbox(
        &self,
        sandbox: SandboxType,
        sandbox_requested: bool,
        exec_server: bool,
        windows_elevated: bool,
    ) -> Result<(), LaunchDenied> {
        if !cfg!(any(target_os = "macos", target_os = "linux", windows)) {
            return Err(LaunchDenied::UnsupportedPlatform);
        }
        if !self.hardened {
            return Err(LaunchDenied::ProcessHardening);
        }
        if exec_server {
            return Err(LaunchDenied::RemoteEnvironment);
        }
        if !sandbox_requested || sandbox == SandboxType::None {
            return Err(LaunchDenied::Unsandboxed);
        }
        if cfg!(windows) && !windows_elevated {
            return Err(LaunchDenied::WindowsUnelevatedSandbox);
        }
        Ok(())
    }

    /// Returns `profile` with read denials for the protected paths added, then
    /// verifies the result denies those reads and every write to `CODEX_HOME`.
    pub(crate) fn protect_permissions(
        &self,
        profile: &PermissionProfile,
        cwd: &Path,
    ) -> Result<PermissionProfile, LaunchDenied> {
        let PermissionProfile::Managed { .. } = profile else {
            return Err(LaunchDenied::Unsandboxed);
        };
        let (mut file_system, network) = profile.to_runtime_permissions();
        if file_system.kind != FileSystemSandboxKind::Restricted {
            return Err(LaunchDenied::UnprotectableFileSystem);
        }
        self.keep_codex_home_read_only(&mut file_system, cwd);
        // #651 (Linux): a database folder (and what Corbanu keeps in it)
        // missing where the command could make it is made as Corbanu would,
        // so bubblewrap holds no missing path there with a placeholder file
        // on the host; its entries are then denied as when it exists.
        if cfg!(target_os = "linux") {
            for folder in self.database_folders() {
                if file_system.can_write_path_with_cwd(folder.as_path(), cwd) {
                    super::protected_surface::create_missing_database_folder(folder.as_path());
                }
            }
        }
        for entry in self.protected_entries() {
            if !file_system.entries.contains(&entry) {
                file_system.entries.push(entry);
            }
        }
        let protected = PermissionProfile::from_runtime_permissions(&file_system, network);
        self.verify_permissions(&protected, cwd)?;
        Ok(protected)
    }

    /// A writable root above CODEX_HOME (for example `$TMPDIR`) becomes
    /// read-only there; this only narrows access. A workspace inside
    /// CODEX_HOME stays writable and is refused by `verify_permissions`.
    /// #619: also a root spelled by an alias of its location (`/tmp` for
    /// `/private/tmp`). #652: also through a root reached through a nested
    /// link, spelled through that root too, which is how the sandboxes honor
    /// it.
    fn keep_codex_home_read_only(&self, file_system: &mut FileSystemSandboxPolicy, cwd: &Path) {
        if file_system.can_write_path_with_cwd(self.codex_home.as_path(), cwd)
            || file_system.can_write_effective_path_with_cwd(self.codex_home.as_path(), cwd)
        {
            let spellings =
                file_system.writable_root_spellings_with_cwd(self.codex_home.as_path(), cwd);
            for path in std::iter::once(self.codex_home.clone()).chain(spellings) {
                file_system.entries.push(FileSystemSandboxEntry::new(
                    FileSystemPath::Path { path },
                    FileSystemAccessMode::Read,
                ));
            }
        }
    }

    /// #748: whether this contract alone, at every security level, keeps
    /// agent commands from reading `path`. Explanation only.
    pub(crate) fn keeps_unreadable(&self, path: &Path, cwd: &Path) -> bool {
        let policy = FileSystemSandboxPolicy::restricted(self.protected_entries());
        ReadDenyMatcher::try_new(&policy, cwd)
            .ok()
            .flatten()
            .is_some_and(|matcher| matcher.is_read_denied(path))
    }

    /// #1077: what `path` is when this contract keeps it unreadable at every
    /// level, so the approval prompt can name it. Explanation only.
    pub(crate) fn kept_file(&self, path: &Path, cwd: &Path) -> Option<KeptFile> {
        if !self.keeps_unreadable(path, cwd) {
            return None;
        }
        let among = |paths: &[AbsolutePathBuf]| {
            let entries = paths
                .iter()
                .map(|path| {
                    FileSystemSandboxEntry::skip_missing_path(
                        FileSystemPath::Path { path: path.clone() },
                        FileSystemAccessMode::Deny,
                    )
                })
                .collect();
            ReadDenyMatcher::try_new(&FileSystemSandboxPolicy::restricted(entries), cwd)
                .ok()
                .flatten()
                .is_some_and(|matcher| matcher.is_read_denied(path))
        };
        Some(if among(&self.cli_token_paths) {
            KeptFile::CliToken
        } else if among(&self.keychain_paths) {
            KeptFile::Keychain
        } else if among(&self.google_cloud_cli_folders) {
            if self
                .google_cloud_cli_folders
                .iter()
                .any(|folder| folder.as_path() == path)
            {
                KeptFile::GoogleCloudCliFolder
            } else {
                KeptFile::InGoogleCloudCliFolder
            }
        } else {
            KeptFile::CorbanuOwn
        })
    }

    /// #748: whether this contract alone, at every security level, keeps
    /// agent commands from writing `path` where `base` lets them (it keeps
    /// `CODEX_HOME` read-only and its protected paths unreadable).
    /// Explanation only.
    pub(crate) fn keeps_read_only(
        &self,
        base: &FileSystemSandboxPolicy,
        path: &Path,
        cwd: &Path,
    ) -> bool {
        let mut file_system = base.clone();
        self.keep_codex_home_read_only(&mut file_system, cwd);
        self.keeps_unreadable(path, cwd) || !file_system.can_write_path_with_cwd(path, cwd)
    }

    /// The deny entries [`Self::protect_permissions`] adds: every protected
    /// path and Core's databases.
    fn protected_entries(&self) -> Vec<FileSystemSandboxEntry> {
        // #651: each folder escaped, should its path hold glob syntax.
        let globs = std::iter::once(self.codex_home.clone())
            .chain(self.database_folders())
            .map(|folder| {
                FileSystemSandboxEntry::new(
                    FileSystemPath::GlobPattern {
                        scan: None,
                        pattern: crate::security::preflight::database_glob(folder.as_path())
                            .to_string_lossy()
                            .into_owned(),
                    },
                    FileSystemAccessMode::Deny,
                )
            })
            .collect::<Vec<_>>();
        self.protected_paths()
            .into_iter()
            .map(|path| {
                let path_entry = FileSystemPath::Path { path: path.clone() };
                let path = &path;
                // PF-27-S06: the Windows sandbox drops every skip-if-missing
                // entry, so a path that exists is denied outright there. So
                // is a protected CODEX_HOME directory that does not exist yet
                // (#294): the sandbox's ACL setup creates it before applying
                // the deny, so a vault created later is covered too.
                if cfg!(windows)
                    && (path.as_path().exists() || self.is_protected_codex_home_dir(path))
                {
                    FileSystemSandboxEntry::new(path_entry, FileSystemAccessMode::Deny)
                } else {
                    FileSystemSandboxEntry::skip_missing_path(
                        path_entry,
                        FileSystemAccessMode::Deny,
                    )
                }
            })
            .chain(globs)
            .collect()
    }

    /// #651: notes `sqlite_home` when it is not `CODEX_HOME` (by any
    /// spelling).
    pub(crate) fn note_database_folder(&self, sqlite_home: &Path) {
        let Ok(folder) = AbsolutePathBuf::from_absolute_path(sqlite_home) else {
            return;
        };
        let same_folder = folder == self.codex_home
            || std::fs::canonicalize(sqlite_home).is_ok_and(|real| {
                std::fs::canonicalize(self.codex_home.as_path()).is_ok_and(|home| home == real)
            });
        let mut folders = self
            .database_folders
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if !same_folder && !folders.contains(&folder) {
            folders.push(folder);
        }
    }

    fn database_folders(&self) -> Vec<AbsolutePathBuf> {
        self.database_folders
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }

    /// Every path agent commands may not read: [`Self::protected_read_paths`],
    /// what Corbanu keeps in each separate database folder besides the
    /// databases, and the backups beside each folder (#651).
    fn protected_paths(&self) -> Vec<AbsolutePathBuf> {
        let mut paths = self.protected_read_paths.clone();
        for folder in self.database_folders() {
            paths.extend(
                super::preflight::DATABASE_FOLDER_ENTRIES
                    .iter()
                    .map(|entry| folder.join(entry)),
            );
        }
        // Databases recovery set aside beside each folder (the home's too).
        // On Linux only when present: bubblewrap would hold a missing one
        // with a placeholder file beside the folder (in the user's home for
        // the default home, #348), as `ReadDenials` does.
        for folder in std::iter::once(self.codex_home.clone()).chain(self.database_folders()) {
            if let Some(name) = folder.as_path().file_name() {
                let mut backups = name.to_os_string();
                backups.push(".db-backups");
                if let Ok(backups) =
                    AbsolutePathBuf::from_absolute_path(folder.as_path().with_file_name(backups))
                    && (!cfg!(target_os = "linux")
                        || std::fs::symlink_metadata(backups.as_path()).is_ok())
                {
                    paths.push(backups);
                }
            }
        }
        paths
    }

    /// #294: the Windows deny-read ACL targets for the protected paths. Every
    /// elevated sandbox launch in an armed process carries them, protected or
    /// not: each launch's ACL sync revokes the sandbox-group denies it does
    /// not list, so an unprotected launch (a TUI workspace probe) would
    /// otherwise reopen the vault to commands that are already running.
    #[cfg(windows)]
    pub(crate) fn windows_deny_read_targets(
        &self,
        cwd: &AbsolutePathBuf,
    ) -> Result<codex_windows_sandbox::DenyReadTargets, String> {
        let mut policy = FileSystemSandboxPolicy::restricted(self.protected_entries());
        policy.remove_skip_missing_path_entries();
        codex_windows_sandbox::resolve_windows_deny_read_targets(&policy, cwd)
    }

    /// Verifies that `profile` denies reading every protected path and writing
    /// `CODEX_HOME`. Run again after per-command permissions are merged.
    pub(crate) fn verify_permissions(
        &self,
        profile: &PermissionProfile,
        cwd: &Path,
    ) -> Result<(), LaunchDenied> {
        let PermissionProfile::Managed { .. } = profile else {
            return Err(LaunchDenied::Unsandboxed);
        };
        let file_system = profile.file_system_sandbox_policy();
        if file_system.kind != FileSystemSandboxKind::Restricted {
            return Err(LaunchDenied::UnprotectableFileSystem);
        }
        for path in &self.protected_paths() {
            if file_system.can_read_path_with_cwd(path.as_path(), cwd) {
                return Err(LaunchDenied::ProtectedPathReadable(
                    path.display().to_string(),
                ));
            }
        }
        let canonical_home = std::fs::canonicalize(self.codex_home.as_path()).ok();
        let writable_inside_home =
            file_system
                .get_writable_roots_with_cwd(cwd)
                .iter()
                .any(|root| {
                    let root = root.root.as_path();
                    let canonical_root = std::fs::canonicalize(root).ok();
                    [Some(root), canonical_root.as_deref()]
                        .into_iter()
                        .flatten()
                        .any(|root| {
                            root.starts_with(self.codex_home.as_path())
                                || canonical_home
                                    .as_deref()
                                    .is_some_and(|home| root.starts_with(home))
                        })
                });
        if writable_inside_home
            || file_system.can_write_path_with_cwd(self.codex_home.as_path(), cwd)
            || file_system.can_write_path_with_cwd(&self.codex_home.join("config.toml"), cwd)
            || file_system.can_write_effective_path_with_cwd(self.codex_home.as_path(), cwd)
            || file_system
                .can_write_effective_path_with_cwd(&self.codex_home.join("config.toml"), cwd)
        {
            return Err(LaunchDenied::PolicyStoreWritable);
        }
        Ok(())
    }

    /// Permissions for in-process file tools (apply_patch, structured edits,
    /// image viewing): the protected profile, or a deny-everything profile
    /// when the launch could not be protected, so these tools fail closed.
    pub(crate) fn file_tool_permissions(
        &self,
        profile: &PermissionProfile,
        sandboxed: bool,
        cwd: &Path,
    ) -> PermissionProfile {
        let protected = sandboxed
            .then(|| self.protect_permissions(profile, cwd).ok())
            .flatten();
        protected.unwrap_or_else(|| {
            PermissionProfile::from_runtime_permissions(
                &FileSystemSandboxPolicy::restricted(Vec::new()),
                NetworkSandboxPolicy::Restricted,
            )
        })
    }

    /// Refuses login shells and managed secrets in argv, then removes any
    /// remaining environment value that embeds a managed secret or a URL
    /// password. Broker dummies differ from the raw values and are kept.
    pub(crate) fn check_command(
        &self,
        argv: &[String],
        env: &mut HashMap<String, String>,
    ) -> Result<(), LaunchDenied> {
        if is_login_shell(argv) {
            return Err(LaunchDenied::LoginShell);
        }
        if argv.iter().any(|arg| self.contains_managed_value(arg)) {
            return Err(LaunchDenied::RawSecretInArgv);
        }
        env.retain(|_, value| {
            !self.contains_managed_value(value) && !secretless_launch::value_has_url_password(value)
        });
        if cfg!(unix) {
            env.insert("ZDOTDIR".to_string(), PROTECTED_ZDOTDIR.to_string());
        }
        Ok(())
    }

    /// Refuses input that carries a managed secret to a running process.
    pub(crate) fn check_stdin(&self, input: &[u8]) -> Result<(), LaunchDenied> {
        let input = String::from_utf8_lossy(input);
        if self.contains_managed_value(&input) {
            super::inspection::record_launch_denial(&LaunchDenied::RawSecretInStdin);
            return Err(LaunchDenied::RawSecretInStdin);
        }
        Ok(())
    }

    fn is_protected_codex_home_dir(&self, path: &AbsolutePathBuf) -> bool {
        PROTECTED_CODEX_HOME_DIRS
            .iter()
            .any(|dir| self.codex_home.join(dir) == *path)
    }

    fn contains_managed_value(&self, text: &str) -> bool {
        self.managed_values
            .iter()
            .any(|value| text.contains(value.as_str()))
    }
}

/// PF-27-S07: a lock file in `CODEX_HOME` that every armed process holds
/// shared for its lifetime, so a process with the flag off never removes the
/// new-file deny, or (#301) the sandbox's deny-read entries, under one that
/// relies on them.
#[cfg(windows)]
const ARMED_LOCK_FILE: &str = codex_windows_sandbox::SECRETLESS_LAUNCH_LOCK_FILE;

/// PF-27-S07: a contract's hold on the armed lock file. #323: and this
/// process's registration as a session relying on the sandbox's deny-read
/// entries (held until it exits), so other `CODEX_HOME`s' launches don't
/// remove one of them either.
#[cfg(windows)]
#[derive(Default)]
struct ArmedLock {
    file: Option<std::fs::File>,
    locked: bool,
    registered: bool,
}

#[cfg(windows)]
impl ArmedLock {
    /// Opens the lock file and tries once to take it shared (no waiting:
    /// this runs during config load).
    fn open(codex_home: &Path) -> Self {
        let group = codex_windows_sandbox::resolve_sid(WINDOWS_SANDBOX_USERS_GROUP).ok();
        match open_armed_lock(codex_home, group) {
            Ok(file) => {
                let locked = file.try_lock_shared().is_ok();
                Self {
                    file: Some(file),
                    locked,
                    // Config load: hardly any wait.
                    registered: register_deny_read_session(REGISTER_WAIT_AT_CONFIG_LOAD),
                }
            }
            Err(err) => {
                tracing::warn!(
                    "secretless_agent_launch: could not open the CODEX_HOME lock, so protected launches are refused: {err}"
                );
                Self::default()
            }
        }
    }

    /// Whether the lock is held, trying once more (without waiting: this
    /// runs on the launch path) if it was not.
    fn ensure_locked(&mut self) -> bool {
        if !self.locked
            && let Some(file) = self.file.as_ref()
        {
            self.locked = file.try_lock_shared().is_ok();
        }
        self.locked
    }

    /// Whether this process is registered, trying once more if not.
    fn ensure_registered(&mut self) -> bool {
        if !self.registered {
            self.registered =
                register_deny_read_session(codex_windows_sandbox::DENY_READ_SYNC_LOCK_WAIT);
        }
        self.registered
    }
}

#[cfg(windows)]
const REGISTER_WAIT_AT_CONFIG_LOAD: std::time::Duration = std::time::Duration::from_millis(100);

#[cfg(windows)]
fn register_deny_read_session(wait: std::time::Duration) -> bool {
    match codex_windows_sandbox::register_deny_read_session(wait) {
        Ok(_) => true,
        Err(err) => {
            tracing::warn!(
                "secretless_agent_launch: could not register this session for the sandbox's deny-read entries, so protected launches are refused: {err:#}"
            );
            false
        }
    }
}

/// Opens (creating it if needed) the armed lock file: a regular file, never
/// through a link, not deletable or renamable while open, and with its own
/// read deny for the sandbox's users when their group exists (otherwise a
/// sandboxed command could open it and hold the lock).
#[cfg(windows)]
pub(crate) fn open_armed_lock(
    codex_home: &Path,
    mut group: Option<Vec<u8>>,
) -> std::io::Result<std::fs::File> {
    use std::os::windows::fs::OpenOptionsExt as _;
    const FILE_SHARE_READ: u32 = 0x1;
    const FILE_SHARE_WRITE: u32 = 0x2;
    const FILE_FLAG_OPEN_REPARSE_POINT: u32 = 0x0020_0000;
    let path = codex_home.join(ARMED_LOCK_FILE);
    let file = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE)
        .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT)
        .open(&path)?;
    let metadata = file.metadata()?;
    if !metadata.is_file() || metadata.file_type().is_symlink() {
        return Err(std::io::Error::other(format!(
            "{} is not a regular file",
            path.display()
        )));
    }
    // A hard link would lock (and deny) another file, such as `auth.json`.
    if codex_windows_sandbox::file_link_count(&file)? != 1 {
        return Err(std::io::Error::other(format!(
            "{} has other hard links; delete it",
            path.display()
        )));
    }
    if let Some(group) = group.as_mut() {
        deny_armed_lock(&path, group.as_mut_ptr().cast())?;
    }
    Ok(file)
}

/// An explicit read deny for `group` on the lock file (an inherited copy of
/// the new-file deny does not count: it goes when that entry is removed).
#[cfg(windows)]
fn deny_armed_lock(path: &Path, group: *mut std::ffi::c_void) -> std::io::Result<()> {
    // SAFETY: `group` is a valid SID for the duration of the call.
    match unsafe { codex_windows_sandbox::ensure_explicit_deny_read_ace(path, group) } {
        Ok(true) => Ok(()),
        Ok(false) => Err(std::io::Error::other(format!(
            "could not deny {} to the sandbox's users",
            path.display()
        ))),
        Err(err) => Err(std::io::Error::other(err)),
    }
}

/// PF-27-S07: on Windows, once per process with `secretless_agent_launch`
/// off and the contract not armed here, removes exactly the `CODEX_HOME`
/// new-file deny an earlier protected run added (and the copies files in
/// `CODEX_HOME` inherited), leaving every other entry. Skipped while another
/// process has the contract armed. Turning the flag off in a process that
/// already armed it takes effect at its next start (arming is one-way).
#[cfg(windows)]
pub(crate) fn release_codex_home_when_unarmed(codex_home: &AbsolutePathBuf) {
    static ONCE: std::sync::Once = std::sync::Once::new();
    if active().is_some() {
        return;
    }
    ONCE.call_once(|| {
        // Cheap check first: most users never ran a protected launch, and
        // resolving the group name can reach a domain controller.
        // SAFETY: no SID is passed.
        let present = unsafe {
            codex_windows_sandbox::has_exact_deny_read_ace_for_new_files(
                codex_home.as_path(),
                /*psid*/ None,
            )
        };
        if !matches!(present, Ok(true)) {
            return;
        }
        // No sandbox group: setup never ran, so no entry names it.
        let Ok(mut group) = codex_windows_sandbox::resolve_sid(WINDOWS_SANDBOX_USERS_GROUP) else {
            return;
        };
        match release_new_file_deny(codex_home.as_path(), group.as_mut_ptr().cast()) {
            Ok(true) => tracing::info!(
                "secretless_agent_launch is off: removed the CODEX_HOME new-file deny"
            ),
            Ok(false) => {}
            Err(err) => tracing::warn!(
                "secretless_agent_launch is off; the CODEX_HOME new-file deny was not removed ({err})"
            ),
        }
    });
}

/// Removes the new-file deny for `group` from `codex_home` unless an armed
/// process holds the lock. Returns whether it was removed.
#[cfg(windows)]
pub(crate) fn release_new_file_deny(
    codex_home: &Path,
    sid: *mut std::ffi::c_void,
) -> std::io::Result<bool> {
    // SAFETY: `sid` is a valid SID for the duration of the call.
    let present = unsafe {
        codex_windows_sandbox::has_exact_deny_read_ace_for_new_files(codex_home, Some(sid))
    }
    .map_err(std::io::Error::other)?;
    if !present {
        return Ok(false);
    }
    let lock = open_armed_lock(codex_home, /*group*/ None)?;
    match lock.try_lock() {
        Ok(()) => {}
        Err(std::fs::TryLockError::WouldBlock) => return Ok(false),
        Err(std::fs::TryLockError::Error(err)) => return Err(err),
    }
    // Held exclusively until `lock` drops: no process can arm meanwhile.
    // The lock file's inherited copy goes with the entry; it gets its own
    // first, so it is never openable by the sandbox's users.
    deny_armed_lock(&codex_home.join(ARMED_LOCK_FILE), sid)
        .map_err(|err| std::io::Error::other(format!("protecting the lock file: {err}")))?;
    // SAFETY: as above.
    unsafe { codex_windows_sandbox::remove_deny_read_ace_for_new_files(codex_home, sid) }
        .map_err(|err| std::io::Error::other(format!("removing the entry: {err}")))
}

/// `sh -l`, `bash -lc`, `zsh --login`, and similar.
fn is_login_shell(argv: &[String]) -> bool {
    let Some(program) = argv.first() else {
        return false;
    };
    let name = Path::new(program)
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or_default();
    if name.starts_with('-') {
        return true;
    }
    if !matches!(name, "sh" | "bash" | "zsh" | "dash" | "ksh" | "fish") {
        return false;
    }
    argv.iter()
        .skip(1)
        .take_while(|arg| arg.starts_with('-'))
        .any(|arg| {
            arg == "--login"
                || (!arg.starts_with("--") && arg.trim_start_matches('-').contains('l'))
        })
}

/// Keeps same-user processes outside the sandbox from reading Core's memory:
/// non-dumpable on Linux (also hides `/proc/<pid>/environ` and `mem`), no
/// debugger attach on macOS, a process DACL that grants no memory or handle
/// access on Windows (PF-27-S06). Returns false when the platform call failed.
fn harden_current_process() -> bool {
    #[cfg(target_os = "linux")]
    {
        // SAFETY: prctl with these arguments only changes this process's flags.
        let set = unsafe { libc::prctl(libc::PR_SET_DUMPABLE, 0, 0, 0, 0) } == 0;
        // SAFETY: reading the flag has no side effects.
        set && unsafe { libc::prctl(libc::PR_GET_DUMPABLE, 0, 0, 0, 0) } == 0
    }
    #[cfg(target_os = "macos")]
    {
        // SAFETY: PT_DENY_ATTACH takes no address or data.
        unsafe { libc::ptrace(libc::PT_DENY_ATTACH, 0, std::ptr::null_mut(), 0) == 0 }
    }
    #[cfg(windows)]
    {
        codex_process_hardening::restrict_current_process_access().is_ok()
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
    {
        false
    }
}

#[cfg(test)]
#[path = "launch_contract_tests.rs"]
mod tests;

#[cfg(test)]
#[cfg(windows)]
#[path = "launch_contract_windows_tests.rs"]
mod windows_tests;

#[cfg(test)]
#[cfg(windows)]
#[path = "launch_contract_windows_workspace_write_tests.rs"]
mod windows_workspace_write_tests;
