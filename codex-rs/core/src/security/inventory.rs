//! PF-29-S01 protected-mode inventory (feature `protected_mode_preflight`).
//!
//! Lists, by safe finding ID, where raw secrets live on this machine and which
//! routes could carry them past a protected level: Corbanu's own credential
//! and history stores, a fixed list of credential files and shell profiles
//! under the home directory, project env files, the process environment,
//! every config layer (including values a higher layer shadows), MCP servers
//! and model providers, browser profiles, keychains and memories.
//!
//! Bounded on purpose: only the fixed locations below are read (no recursive
//! scan of user directories), nothing is written, and no discovered value is
//! kept, logged or shown. A finding names a location (path, variable or config
//! key) and a class. Detection is heuristic beyond the fixed names: secret-like
//! names, the PF-28 key shapes and Core's own secret-named variable values. A
//! secret under an innocent name in an unlisted file is not found ([`LIMITS`]).

use std::collections::BTreeSet;
use std::path::Path;
use std::path::PathBuf;

use codex_secret_broker::output_gate::OutputGate;
use codex_secret_broker::output_gate::OutputSink;
use codex_secret_broker::output_gate::SecretClass as GateClass;
use sha2::Digest as _;
use sha2::Sha256;

/// Files larger than this are listed but not read or hashed.
const MAX_READ_BYTES: u64 = 1024 * 1024;
/// Entries listed in one known credential directory (`~/.ssh`, memories).
const MAX_DIR_ENTRIES: usize = 512;
/// Core values shorter than this are too likely to match ordinary text.
const MIN_MANAGED_VALUE_BYTES: usize = 8;
const MAX_CONFIG_DEPTH: usize = 12;

/// Name fragments the Aggressive environment policy removes from agent
/// commands; a variable named like this is never passed to them.
const SECRET_NAME_FRAGMENTS: [&str; 7] = [
    "KEY",
    "SECRET",
    "TOKEN",
    "VAULT",
    "PASSWORD",
    "PASSPHRASE",
    "CREDENTIAL",
];

/// What the inventory cannot see. Shown with every preflight.
pub const LIMITS: &[&str] = &[
    "Only fixed locations are checked; secrets in other files, under innocent names, are not found.",
    "Values are matched by name, by common key shapes and against Corbanu's own secret variables; other encodings are missed.",
    "The vault is not opened; whether its key unlocks it is checked when a credential is used.",
    "Browser profiles and keychains are listed, not read.",
    "MCP servers, hooks and notify commands run outside the sandbox; only their configured secrets are checked.",
];

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum FindingKind {
    VaultStore,
    SignInFile,
    EnvFile,
    CredentialFile,
    SshPrivateKey,
    ShellProfileExport,
    EnvironmentVariable,
    ConfigLiteral,
    McpSecretLiteral,
    ProviderSecretLiteral,
    ExecProviderAuth,
    Integration,
    BrowserProfile,
    Keychain,
    Wallet,
    Transcript,
    Memory,
    ProtectedMount,
    Reference,
}

impl FindingKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::VaultStore => "vault-store",
            Self::SignInFile => "sign-in-file",
            Self::EnvFile => "env-file",
            Self::CredentialFile => "credential-file",
            Self::SshPrivateKey => "ssh-private-key",
            Self::ShellProfileExport => "shell-profile-export",
            Self::EnvironmentVariable => "environment-variable",
            Self::ConfigLiteral => "config-literal",
            Self::McpSecretLiteral => "mcp-secret",
            Self::ProviderSecretLiteral => "provider-secret",
            Self::ExecProviderAuth => "exec-provider-auth",
            Self::Integration => "integration",
            Self::BrowserProfile => "browser-profile",
            Self::Keychain => "keychain",
            Self::Wallet => "wallet",
            Self::Transcript => "transcript",
            Self::Memory => "memory",
            Self::ProtectedMount => "protected-mount",
            Self::Reference => "reference",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum SecretClass {
    /// Held by Corbanu (vault, sign-in) or a provider key Core brokers.
    ManagedSecret,
    /// A raw credential outside the vault.
    UnmanagedSecret,
    /// Wallet and signing keys: never migrated, never shown.
    CustodyKey,
    FinancialRecord,
    /// Transcripts, history, snapshots and logs that may hold earlier secrets.
    HistoricContent,
    /// A label or variable reference, not a value.
    PermittedDerived,
    OrdinaryEnv,
    /// A launch route (MCP server, hook) rather than a stored secret.
    Route,
}

impl SecretClass {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::ManagedSecret => "managed secret",
            Self::UnmanagedSecret => "unmanaged secret",
            Self::CustodyKey => "custody key",
            Self::FinancialRecord => "financial record",
            Self::HistoricContent => "historic content",
            Self::PermittedDerived => "reference",
            Self::OrdinaryEnv => "ordinary environment",
            Self::Route => "launch route",
        }
    }
}

/// What a protected level does with a finding.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Disposition {
    /// Already unreadable to (or removed from) agent commands.
    Denied,
    /// The protected level denies agent reads of [`Finding::paths`].
    Isolate,
    /// Blocks activation until migrated to the vault (PF-29-S02) or removed.
    Migrate,
    /// Reaches the model's context; blocks activation until removed.
    RemoveFromContext,
    /// Runs outside the sandbox; shown as not contained.
    NotContained,
    /// Classification only.
    Info,
}

impl Disposition {
    pub fn blocks(self) -> bool {
        matches!(self, Self::Migrate | Self::RemoveFromContext)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Finding {
    /// Stable across runs: kind plus a digest of the location, never a value.
    pub id: String,
    pub kind: FindingKind,
    pub class: SecretClass,
    /// Path (home shown as `~`), variable name or config key. Never a value.
    pub location: String,
    pub disposition: Disposition,
    /// What [`Disposition::Isolate`] denies: the path and, for a symlink, its target.
    pub paths: Vec<PathBuf>,
    /// For a shell-profile export: the 1-based line and variable name, which
    /// PF-29-S02 migration rewrites.
    pub source_line: Option<(usize, String)>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Scope {
    CorbanuHome,
    Home,
    Workspace,
    Config,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum EntryStatus {
    /// PF-29-S02 can move it into the vault.
    Supported,
    Unsupported(&'static str),
    Unreadable(String),
}

/// One dry-run migration source. The digest is keyed per inventory, so it
/// can detect a change without being a reusable hash of a secret.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ManifestEntry {
    pub finding_id: String,
    pub path: PathBuf,
    pub scope: Scope,
    /// The path itself is a symlink, a dangling one included.
    pub symlink: bool,
    /// The path itself is a regular file (not a folder, link, pipe, socket
    /// or device).
    pub regular: bool,
    /// Hard links to a file that is not a folder, on Unix and Windows.
    pub links: Option<u64>,
    /// Where the symlink at the path leads; for a dangling one, where a file
    /// created through it would land (#1441).
    pub symlink_target: Option<PathBuf>,
    /// `(device, inode)` on Unix.
    pub identity: Option<(u64, u64)>,
    pub len: Option<u64>,
    pub mode: Option<u32>,
    pub digest: Option<String>,
    pub status: EntryStatus,
}

#[derive(Clone)]
pub struct Manifest {
    key: [u8; 32],
    pub entries: Vec<ManifestEntry>,
}

impl std::fmt::Debug for Manifest {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Manifest")
            .field("entries", &self.entries)
            .finish_non_exhaustive()
    }
}

impl Manifest {
    /// Digest of every entry; any change in file type, link count, symlink
    /// target, identity, size, mode, content or status changes it.
    pub fn digest(&self) -> String {
        let mut hasher = Sha256::new();
        for entry in &self.entries {
            hasher.update(entry.finding_id.as_bytes());
            hasher.update(entry.path.to_string_lossy().as_bytes());
            hasher.update(format!(
                "|{:?}|{:?}|{:?}|{:?}|{:?}|{:?}|{:?}|{:?}|{:?}\n",
                entry.symlink,
                entry.regular,
                entry.links,
                entry.symlink_target,
                entry.identity,
                entry.len,
                entry.mode,
                entry.digest,
                entry.status
            ));
        }
        hex(&hasher.finalize())
    }

    fn keyed_digest(&self, bytes: &[u8]) -> String {
        let mut hasher = Sha256::new();
        hasher.update(self.key);
        hasher.update(bytes);
        hex(&hasher.finalize())
    }
}

/// Vault presence from its encrypted files only; the vault is never opened
/// (unlocking goes through the OS keyring when a credential is used).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum VaultState {
    Absent,
    Present,
    /// A store file cannot be read or is not an age file.
    Unusable,
}

#[derive(Clone, PartialEq)]
pub struct ConfigLayerInput {
    /// Shown to the human, for example `user config`.
    pub label: String,
    /// The file the layer came from; `None` for flags and managed layers.
    pub path: Option<PathBuf>,
    pub toml: toml::Value,
}

impl std::fmt::Debug for ConfigLayerInput {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ConfigLayerInput")
            .field("label", &self.label)
            .field("path", &self.path)
            .finish_non_exhaustive()
    }
}

#[derive(Clone, Default)]
pub struct InventorySources {
    pub codex_home: PathBuf,
    pub home: Option<PathBuf>,
    pub cwd: PathBuf,
    /// The process environment. Values are read for classification only.
    pub env: Vec<(String, String)>,
    pub config_layers: Vec<ConfigLayerInput>,
}

/// Variable names only: the values are secrets.
impl std::fmt::Debug for InventorySources {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("InventorySources")
            .field("codex_home", &self.codex_home)
            .field("home", &self.home)
            .field("cwd", &self.cwd)
            .field(
                "env",
                &self.env.iter().map(|(name, _)| name).collect::<Vec<_>>(),
            )
            .field(
                "config_layers",
                &self
                    .config_layers
                    .iter()
                    .map(|layer| &layer.label)
                    .collect::<Vec<_>>(),
            )
            .finish()
    }
}

#[derive(Clone, Debug)]
pub struct Inventory {
    pub findings: Vec<Finding>,
    pub manifest: Manifest,
    pub ordinary_env_count: usize,
    /// Providers whose auth runs a command; preflight never runs it.
    pub exec_provider_auth: Vec<String>,
    pub vault: VaultState,
    /// A PF-29-S02 migration journal exists: a migration was interrupted.
    pub migration_unfinished: bool,
}

impl Inventory {
    /// Every path the protected level must deny, sorted and unique.
    pub fn isolation_paths(&self) -> Vec<PathBuf> {
        self.findings
            .iter()
            .filter(|finding| finding.disposition == Disposition::Isolate)
            .flat_map(|finding| finding.paths.iter().cloned())
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect()
    }

    pub fn blocking(&self) -> impl Iterator<Item = &Finding> {
        self.findings
            .iter()
            .filter(|finding| finding.disposition.blocks())
    }

    pub fn finding_ids(&self) -> BTreeSet<String> {
        self.findings
            .iter()
            .map(|finding| finding.id.clone())
            .collect()
    }
}

pub(crate) fn take_with_key(sources: &InventorySources, key: [u8; 32]) -> Inventory {
    let mut collector = Collector::new(sources, key);
    collector.corbanu_home();
    collector.home();
    collector.workspace();
    collector.environment();
    for layer in &sources.config_layers {
        collector.config_layer(layer);
    }
    collector.finish()
}

pub(crate) fn is_secret_name(name: &str) -> bool {
    let upper = name.to_ascii_uppercase();
    SECRET_NAME_FRAGMENTS
        .iter()
        .any(|fragment| upper.contains(fragment))
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

/// Matches the PF-28 key shapes and Core's own secret-named variable values.
struct Detector {
    patterns: OutputGate,
    managed: OutputGate,
}

impl Detector {
    fn new(env: &[(String, String)]) -> Self {
        let managed = OutputGate::new();
        let values = env
            .iter()
            .filter(|(name, value)| is_secret_name(name) && value.len() >= MIN_MANAGED_VALUE_BYTES)
            .map(|(_, value)| ("inventory", GateClass::Operational, value.as_str()))
            .collect::<Vec<_>>();
        // A full registry only weakens detection; the key shapes still apply.
        let _ = managed.register_all(&values);
        Self {
            patterns: OutputGate::new(),
            managed,
        }
    }

    fn key_shape(&self, text: &str) -> bool {
        self.patterns
            .scrub(OutputSink::Diagnostic, text)
            .is_some_and(|(_, provenance)| provenance.changed())
            || has_url_password(text)
    }

    fn secret(&self, text: &str) -> bool {
        self.key_shape(text)
            || self
                .managed
                .scrub(OutputSink::ToolResult, text)
                .is_some_and(|(_, provenance)| provenance.changed())
    }
}

/// `scheme://user:password@host`.
fn has_url_password(text: &str) -> bool {
    text.split("://").skip(1).any(|rest| {
        let authority = rest.split(['/', '?', '#']).next().unwrap_or_default();
        authority.rsplit_once('@').is_some_and(|(userinfo, _)| {
            userinfo
                .split_once(':')
                .is_some_and(|(_, pw)| !pw.is_empty())
        })
    })
}

/// File facts gathered without following a symlink silently.
struct Probe {
    is_symlink: bool,
    is_regular: bool,
    links: Option<u64>,
    symlink_target: Option<PathBuf>,
    identity: Option<(u64, u64)>,
    len: Option<u64>,
    mode: Option<u32>,
    is_dir: bool,
    error: Option<String>,
}

fn probe(path: &Path) -> Option<Probe> {
    let link = std::fs::symlink_metadata(path).ok()?;
    let is_symlink = link.file_type().is_symlink();
    let is_regular = link.file_type().is_file();
    let (symlink_target, metadata) = if is_symlink {
        match (std::fs::canonicalize(path), std::fs::metadata(path)) {
            (Ok(target), Ok(metadata)) => (Some(target), Ok(metadata)),
            (_, Err(err)) | (Err(err), _) => (would_be_target(path), Err(err)),
        }
    } else {
        (None, Ok(link))
    };
    let mut probe = Probe {
        is_symlink,
        is_regular,
        links: None,
        symlink_target,
        identity: None,
        len: None,
        mode: None,
        is_dir: false,
        error: None,
    };
    match metadata {
        Ok(metadata) => {
            probe.is_dir = metadata.is_dir();
            probe.len = (!probe.is_dir).then_some(metadata.len());
            #[cfg(unix)]
            {
                use std::os::unix::fs::MetadataExt as _;
                probe.identity = Some((metadata.dev(), metadata.ino()));
                probe.mode = Some(metadata.mode() & 0o7777);
                probe.links = (!probe.is_dir).then_some(metadata.nlink());
            }
            #[cfg(windows)]
            if !probe.is_dir {
                probe.links = windows_link_count(path);
            }
        }
        Err(err) => probe.error = Some(format!("dangling or looping symlink ({})", err.kind())),
    }
    Some(probe)
}

/// Most symlinks followed in one chain (Linux's `MAXSYMLINKS`).
const MAX_SYMLINK_HOPS: usize = 40;

/// #1441: where a file created through the dangling symlink at `path` would
/// land (SQLite creates `-wal` and `-journal` files through one): the end of
/// its chain, with the part that exists canonical. `None` for a loop.
///
/// Always `None` on Windows: its sandbox creates a folder at every missing
/// path it denies, so a planted link would choose where Corbanu creates one.
fn would_be_target(path: &Path) -> Option<PathBuf> {
    if cfg!(windows) {
        return None;
    }
    let mut link = path.to_path_buf();
    for _ in 0..MAX_SYMLINK_HOPS {
        let target = std::fs::read_link(&link).ok()?;
        let next = match link.parent() {
            Some(parent) => parent.join(target),
            None => target,
        };
        if !std::fs::symlink_metadata(&next).is_ok_and(|meta| meta.file_type().is_symlink()) {
            return Some(canonical_prefix(&next));
        }
        link = next;
    }
    None
}

/// `path` with its longest existing ancestor canonical.
fn canonical_prefix(path: &Path) -> PathBuf {
    path.ancestors()
        .find_map(|ancestor| {
            let rest = path.strip_prefix(ancestor).ok()?;
            let canonical = std::fs::canonicalize(ancestor).ok()?;
            // `join("")` would add a trailing separator.
            Some(if rest.as_os_str().is_empty() {
                canonical
            } else {
                canonical.join(rest)
            })
        })
        .unwrap_or_else(|| path.to_path_buf())
}

/// #1441: hard links to the file at `path` (followed), read through a handle
/// opened for its attributes only.
#[cfg(windows)]
fn windows_link_count(path: &Path) -> Option<u64> {
    use std::os::windows::fs::OpenOptionsExt as _;
    const FILE_READ_ATTRIBUTES: u32 = 0x80;
    const SHARE_ALL: u32 = 0x7;
    let file = std::fs::OpenOptions::new()
        .access_mode(FILE_READ_ATTRIBUTES)
        .share_mode(SHARE_ALL)
        .open(path)
        .ok()?;
    codex_windows_sandbox::file_link_count(&file)
        .ok()
        .map(u64::from)
}

/// Opens regular files only, without blocking: a device (`/dev/zero`) would
/// never end and a named pipe would block, and a project can ship either.
fn open_regular(path: &Path) -> Result<std::fs::File, String> {
    let not_regular = || "not a regular file".to_string();
    if !std::fs::metadata(path)
        .map_err(|err| err.kind().to_string())?
        .is_file()
    {
        return Err(not_regular());
    }
    let mut options = std::fs::OpenOptions::new();
    options.read(true);
    // Windows has no FIFO that passes the `is_file` check above.
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.custom_flags(libc::O_NONBLOCK);
    }
    let file = options.open(path).map_err(|err| err.kind().to_string())?;
    // The path may have been swapped between the check and the open.
    if !file
        .metadata()
        .map_err(|err| err.kind().to_string())?
        .is_file()
    {
        return Err(not_regular());
    }
    Ok(file)
}

fn read_bounded(path: &Path) -> Result<Vec<u8>, String> {
    use std::io::Read as _;
    let mut bytes = Vec::new();
    open_regular(path)?
        .take(MAX_READ_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|err| err.kind().to_string())?;
    if bytes.len() as u64 > MAX_READ_BYTES {
        return Err(format!("larger than {MAX_READ_BYTES} bytes"));
    }
    Ok(bytes)
}

struct Collector<'a> {
    sources: &'a InventorySources,
    detector: Detector,
    findings: Vec<Finding>,
    manifest: Manifest,
    seen: BTreeSet<String>,
    ordinary_env_count: usize,
    exec_provider_auth: BTreeSet<String>,
}

struct PathFinding<'p> {
    kind: FindingKind,
    class: SecretClass,
    disposition: Disposition,
    scope: Scope,
    path: &'p Path,
    /// Shown after the path, for example `:12 OPENAI_API_KEY`.
    detail: Option<String>,
    unsupported: Option<&'static str>,
}

impl<'a> Collector<'a> {
    fn new(sources: &'a InventorySources, key: [u8; 32]) -> Self {
        Self {
            sources,
            detector: Detector::new(&sources.env),
            findings: Vec::new(),
            manifest: Manifest {
                key,
                entries: Vec::new(),
            },
            seen: BTreeSet::new(),
            ordinary_env_count: 0,
            exec_provider_auth: BTreeSet::new(),
        }
    }

    fn display(&self, path: &Path) -> String {
        if let Some(home) = &self.sources.home
            && let Ok(rest) = path.strip_prefix(home)
        {
            return if rest.as_os_str().is_empty() {
                "~".to_string()
            } else {
                format!("~/{}", rest.display())
            };
        }
        path.display().to_string()
    }

    fn push(
        &mut self,
        kind: FindingKind,
        class: SecretClass,
        disposition: Disposition,
        location: String,
        paths: Vec<PathBuf>,
    ) -> Option<String> {
        let mut hasher = Sha256::new();
        hasher.update(kind.as_str().as_bytes());
        hasher.update([0]);
        hasher.update(location.as_bytes());
        let digest = hex(&hasher.finalize());
        let id = format!("{}-{}", kind.as_str(), &digest[..12]);
        if !self.seen.insert(id.clone()) {
            return None;
        }
        self.findings.push(Finding {
            id: id.clone(),
            kind,
            class,
            location,
            disposition,
            paths,
            source_line: None,
        });
        Some(id)
    }

    /// Records a finding for an existing path plus its manifest entry.
    /// Returns `false` when the path does not exist.
    fn path_finding(&mut self, spec: PathFinding<'_>) -> bool {
        let Some(probe) = probe(spec.path) else {
            return false;
        };
        let mut location = self.display(spec.path);
        if let Some(detail) = &spec.detail {
            location.push_str(detail);
        }
        let mut paths = vec![spec.path.to_path_buf()];
        paths.extend(probe.symlink_target.clone());
        let Some(id) = self.push(spec.kind, spec.class, spec.disposition, location, paths) else {
            return true;
        };
        self.manifest_entry(id, spec.path, spec.scope, spec.unsupported, probe);
        true
    }

    /// Records the manifest entry for probed `path` under finding `id`.
    fn manifest_entry(
        &mut self,
        id: String,
        path: &Path,
        scope: Scope,
        unsupported: Option<&'static str>,
        probe: Probe,
    ) {
        let (digest, status) = match (&probe.error, unsupported, probe.is_dir) {
            (Some(error), _, _) => (None, EntryStatus::Unreadable(error.clone())),
            (None, Some(reason), true) => (self.dir_digest(path), EntryStatus::Unsupported(reason)),
            (None, None, true) => (self.dir_digest(path), EntryStatus::Unsupported("directory")),
            (None, unsupported, false) => match read_bounded(path) {
                Ok(bytes) => (
                    Some(self.manifest.keyed_digest(&bytes)),
                    unsupported.map_or(EntryStatus::Supported, EntryStatus::Unsupported),
                ),
                Err(reason) => (None, EntryStatus::Unreadable(reason)),
            },
        };
        self.manifest.entries.push(ManifestEntry {
            finding_id: id,
            path: path.to_path_buf(),
            scope,
            symlink: probe.is_symlink,
            regular: probe.is_regular,
            links: probe.links,
            symlink_target: probe.symlink_target,
            identity: probe.identity,
            len: probe.len,
            mode: probe.mode,
            digest,
            status,
        });
    }

    /// One level: names, sizes and modification times.
    fn dir_digest(&self, path: &Path) -> Option<String> {
        let mut rows = std::fs::read_dir(path)
            .ok()?
            .take(MAX_DIR_ENTRIES)
            .filter_map(Result::ok)
            .map(|entry| {
                let metadata = entry.metadata().ok();
                format!(
                    "{}|{:?}|{:?}",
                    entry.file_name().to_string_lossy(),
                    metadata.as_ref().map(std::fs::Metadata::len),
                    metadata.and_then(|metadata| metadata.modified().ok())
                )
            })
            .collect::<Vec<_>>();
        rows.sort();
        Some(self.manifest.keyed_digest(rows.join("\n").as_bytes()))
    }

    fn corbanu_home(&mut self) {
        let home = self.sources.codex_home.clone();
        let entries: [(
            &str,
            FindingKind,
            SecretClass,
            Disposition,
            Option<&'static str>,
        ); 11] = [
            (
                "secrets",
                FindingKind::VaultStore,
                SecretClass::ManagedSecret,
                Disposition::Denied,
                Some("the vault itself"),
            ),
            (
                "auth.json",
                FindingKind::SignInFile,
                SecretClass::ManagedSecret,
                Disposition::Denied,
                Some("Corbanu sign-in"),
            ),
            (
                ".credentials.json",
                FindingKind::SignInFile,
                SecretClass::ManagedSecret,
                Disposition::Isolate,
                Some("Corbanu sign-in"),
            ),
            (
                "provider_auth.json",
                FindingKind::SignInFile,
                SecretClass::ManagedSecret,
                Disposition::Isolate,
                Some("Corbanu sign-in"),
            ),
            (
                ".env",
                FindingKind::EnvFile,
                SecretClass::UnmanagedSecret,
                Disposition::Isolate,
                None,
            ),
            (
                "wallet",
                FindingKind::Wallet,
                SecretClass::CustodyKey,
                Disposition::Isolate,
                Some("custody keys are never migrated"),
            ),
            (
                "sessions",
                FindingKind::Transcript,
                SecretClass::HistoricContent,
                Disposition::Isolate,
                Some("historic content"),
            ),
            (
                "archived_sessions",
                FindingKind::Transcript,
                SecretClass::HistoricContent,
                Disposition::Isolate,
                Some("historic content"),
            ),
            (
                "history.jsonl",
                FindingKind::Transcript,
                SecretClass::HistoricContent,
                Disposition::Isolate,
                Some("historic content"),
            ),
            (
                "shell_snapshots",
                FindingKind::Transcript,
                SecretClass::HistoricContent,
                Disposition::Isolate,
                Some("historic content"),
            ),
            (
                "log",
                FindingKind::Transcript,
                SecretClass::HistoricContent,
                Disposition::Isolate,
                Some("historic content"),
            ),
        ];
        // Config files are reported per secret key by `config_layer`.
        for (name, kind, class, disposition, unsupported) in entries {
            let path = home.join(name);
            // An empty directory holds nothing. On Windows the secretless
            // launch contract creates its protected directories to deny them
            // before they are used (#294).
            if std::fs::read_dir(&path).is_ok_and(|mut entries| entries.next().is_none()) {
                continue;
            }
            self.path_finding(PathFinding {
                kind,
                class,
                disposition,
                scope: Scope::CorbanuHome,
                path: &path,
                detail: None,
                unsupported,
            });
        }
        // One finding per database: `x.sqlite` plus its `-wal`, `-shm` and
        // `-journal` files, each one probed and recorded (#1428), so a
        // symlink there is drift and its target is isolated.
        let mut databases = std::collections::BTreeMap::<String, Vec<PathBuf>>::new();
        for name in std::fs::read_dir(&home)
            .into_iter()
            .flatten()
            .take(MAX_DIR_ENTRIES)
            .filter_map(Result::ok)
            .map(|entry| entry.file_name().to_string_lossy().into_owned())
        {
            if let Some((base, _)) = name.split_once(".sqlite") {
                databases
                    .entry(format!("{base}.sqlite"))
                    .or_default()
                    .push(home.join(&name));
            }
        }
        for (name, mut files) in databases {
            files.sort();
            let Some(main) = files.first().cloned() else {
                continue;
            };
            let financial = ["wallet", "ledger", "accounting"]
                .iter()
                .any(|fragment| name.contains(fragment));
            let before = self.findings.len();
            self.path_finding(PathFinding {
                kind: FindingKind::Transcript,
                class: if financial {
                    SecretClass::FinancialRecord
                } else {
                    SecretClass::HistoricContent
                },
                disposition: Disposition::Isolate,
                scope: Scope::CorbanuHome,
                path: &main,
                detail: None,
                unsupported: Some("state database"),
            });
            if self.findings.len() <= before {
                continue;
            }
            let id = self.findings[before].id.clone();
            for file in files.into_iter().skip(1) {
                let mut paths = vec![file.clone()];
                if let Some(probe) = probe(&file) {
                    paths.extend(probe.symlink_target.clone());
                    self.manifest_entry(
                        id.clone(),
                        &file,
                        Scope::CorbanuHome,
                        Some("state database"),
                        probe,
                    );
                }
                self.findings[before].paths.extend(paths);
            }
        }
        self.memories();
    }

    fn memories(&mut self) {
        let root = self.sources.codex_home.join("memories");
        let mut stack = vec![(root, 0usize)];
        let mut visited = 0usize;
        while let Some((dir, depth)) = stack.pop() {
            let Ok(entries) = std::fs::read_dir(&dir) else {
                continue;
            };
            let mut entries = entries.filter_map(Result::ok).collect::<Vec<_>>();
            entries.sort_by_key(std::fs::DirEntry::file_name);
            for entry in entries {
                visited += 1;
                if visited > MAX_DIR_ENTRIES {
                    return;
                }
                let path = entry.path();
                let Ok(file_type) = entry.file_type() else {
                    continue;
                };
                if file_type.is_dir() && depth < 2 {
                    stack.push((path, depth + 1));
                } else if file_type.is_file()
                    && let Ok(bytes) = read_bounded(&path)
                    && self.detector.secret(&String::from_utf8_lossy(&bytes))
                {
                    self.path_finding(PathFinding {
                        kind: FindingKind::Memory,
                        class: SecretClass::HistoricContent,
                        disposition: Disposition::RemoveFromContext,
                        scope: Scope::CorbanuHome,
                        path: &path,
                        detail: None,
                        unsupported: Some("memory content is removed, not migrated"),
                    });
                }
            }
        }
    }

    fn home(&mut self) {
        let Some(home) = self.sources.home.clone() else {
            return;
        };
        // Whole folders where the tool keeps only credentials, so a file
        // added later or under another name is covered too.
        for name in [".ssh", ".aws", ".kube", ".config/gcloud", ".azure"] {
            self.path_finding(PathFinding {
                kind: FindingKind::CredentialFile,
                class: SecretClass::UnmanagedSecret,
                disposition: Disposition::Isolate,
                scope: Scope::Home,
                path: &home.join(name),
                detail: None,
                unsupported: Some("credential folder"),
            });
        }
        const CREDENTIAL_FILES: [&str; 15] = [
            ".netrc",
            ".git-credentials",
            ".config/gh/hosts.yml",
            ".config/hub",
            ".docker/config.json",
            ".npmrc",
            ".yarnrc.yml",
            ".pypirc",
            ".cargo/credentials",
            ".cargo/credentials.toml",
            ".gem/credentials",
            ".vault-token",
            ".terraform.d/credentials.tfrc.json",
            ".claude/.credentials.json",
            ".pgpass",
        ];
        for name in CREDENTIAL_FILES {
            self.path_finding(PathFinding {
                kind: FindingKind::CredentialFile,
                class: SecretClass::UnmanagedSecret,
                disposition: Disposition::Isolate,
                scope: Scope::Home,
                path: &home.join(name),
                detail: None,
                unsupported: None,
            });
        }
        self.path_finding(PathFinding {
            kind: FindingKind::CredentialFile,
            class: SecretClass::CustodyKey,
            disposition: Disposition::Isolate,
            scope: Scope::Home,
            path: &home.join(".gnupg/private-keys-v1.d"),
            detail: None,
            unsupported: Some("signing keys stay with gpg"),
        });
        self.ssh_keys(&home);
        self.shell_profiles(&home);
        const BROWSER_PROFILES: [&str; 9] = [
            "Library/Application Support/Google/Chrome",
            "Library/Application Support/BraveSoftware/Brave-Browser",
            "Library/Application Support/Microsoft Edge",
            "Library/Application Support/Firefox/Profiles",
            "Library/Cookies",
            ".config/google-chrome",
            ".config/chromium",
            ".config/BraveSoftware",
            ".mozilla/firefox",
        ];
        for name in BROWSER_PROFILES {
            self.path_finding(PathFinding {
                kind: FindingKind::BrowserProfile,
                class: SecretClass::UnmanagedSecret,
                disposition: Disposition::Isolate,
                scope: Scope::Home,
                path: &home.join(name),
                detail: None,
                unsupported: Some("browser sign-ins cannot be migrated"),
            });
        }
        self.path_finding(PathFinding {
            kind: FindingKind::Keychain,
            class: SecretClass::ManagedSecret,
            disposition: Disposition::Isolate,
            scope: Scope::Home,
            path: &home.join("Library/Keychains"),
            detail: None,
            unsupported: Some("the OS keychain is not migrated"),
        });
    }

    /// Private keys in `~/.ssh` (one level): `id_*` without `.pub`, or any
    /// small file that starts like a PEM/OpenSSH private key.
    fn ssh_keys(&mut self, home: &Path) {
        let Ok(entries) = std::fs::read_dir(home.join(".ssh")) else {
            return;
        };
        let mut paths = entries
            .take(MAX_DIR_ENTRIES)
            .filter_map(Result::ok)
            .map(|entry| entry.path())
            .collect::<Vec<_>>();
        paths.sort();
        for path in paths {
            let name = path
                .file_name()
                .map(|name| name.to_string_lossy().into_owned())
                .unwrap_or_default();
            if name.ends_with(".pub") || !path.is_file() {
                continue;
            }
            let looks_private = name.starts_with("id_")
                || read_bounded(&path).is_ok_and(|bytes| {
                    String::from_utf8_lossy(&bytes[..bytes.len().min(256)])
                        .contains("PRIVATE KEY-----")
                });
            if looks_private {
                self.path_finding(PathFinding {
                    kind: FindingKind::SshPrivateKey,
                    class: SecretClass::UnmanagedSecret,
                    disposition: Disposition::Isolate,
                    scope: Scope::Home,
                    path: &path,
                    detail: None,
                    unsupported: None,
                });
            }
        }
    }

    fn shell_profiles(&mut self, home: &Path) {
        const PROFILES: [&str; 9] = [
            ".bashrc",
            ".bash_profile",
            ".bash_login",
            ".profile",
            ".zshrc",
            ".zshenv",
            ".zprofile",
            ".zlogin",
            ".config/fish/config.fish",
        ];
        for name in PROFILES {
            let path = home.join(name);
            let Ok(bytes) = read_bounded(&path) else {
                if path.exists() || std::fs::symlink_metadata(&path).is_ok() {
                    // Unreadable or too large: listed so the human can check it.
                    self.path_finding(PathFinding {
                        kind: FindingKind::ShellProfileExport,
                        class: SecretClass::UnmanagedSecret,
                        disposition: Disposition::Info,
                        scope: Scope::Home,
                        path: &path,
                        detail: Some(" (not readable)".to_string()),
                        unsupported: Some("not readable"),
                    });
                }
                continue;
            };
            let text = String::from_utf8_lossy(&bytes).into_owned();
            for (index, line) in text.lines().enumerate() {
                let Some((name, value)) = parse_assignment(line) else {
                    continue;
                };
                let value = unquote(value);
                if value.is_empty() {
                    continue;
                }
                let detail = format!(":{} {name}", index + 1);
                if is_reference(value) {
                    if is_secret_name(name) {
                        let location = format!("{}{detail}", self.display(&path));
                        self.push(
                            FindingKind::Reference,
                            SecretClass::PermittedDerived,
                            Disposition::Info,
                            location,
                            Vec::new(),
                        );
                    }
                    continue;
                }
                if is_secret_name(name) || self.detector.secret(value) {
                    let before = self.findings.len();
                    self.path_finding(PathFinding {
                        kind: FindingKind::ShellProfileExport,
                        class: SecretClass::UnmanagedSecret,
                        disposition: Disposition::Migrate,
                        scope: Scope::Home,
                        path: &path,
                        detail: Some(detail),
                        unsupported: None,
                    });
                    if self.findings.len() > before
                        && let Some(finding) = self.findings.last_mut()
                    {
                        finding.source_line = Some((index + 1, name.to_string()));
                    }
                }
            }
        }
    }

    fn workspace(&mut self) {
        let cwd = self.sources.cwd.clone();
        if cwd.as_os_str().is_empty() {
            return;
        }
        for name in [
            ".env",
            ".env.local",
            ".env.development",
            ".env.production",
            ".envrc",
        ] {
            self.path_finding(PathFinding {
                kind: FindingKind::EnvFile,
                class: SecretClass::UnmanagedSecret,
                disposition: Disposition::Isolate,
                scope: Scope::Workspace,
                path: &cwd.join(name),
                detail: None,
                unsupported: None,
            });
        }
    }

    fn environment(&mut self) {
        let mut env = self.sources.env.clone();
        env.sort();
        for (name, value) in env {
            if value.is_empty() {
                continue;
            }
            let location = format!("environment {name}");
            if is_secret_name(&name) {
                let class = if crate::exec_env::is_provider_auth_env_var(&name) {
                    SecretClass::ManagedSecret
                } else {
                    SecretClass::UnmanagedSecret
                };
                self.push(
                    FindingKind::EnvironmentVariable,
                    class,
                    Disposition::Denied,
                    location,
                    Vec::new(),
                );
            } else if self.detector.key_shape(&value) {
                // Passed to agent commands: the name does not look secret.
                self.push(
                    FindingKind::EnvironmentVariable,
                    SecretClass::UnmanagedSecret,
                    Disposition::Migrate,
                    location,
                    Vec::new(),
                );
            } else {
                self.ordinary_env_count += 1;
            }
        }
    }

    fn config_layer(&mut self, layer: &ConfigLayerInput) {
        let location = |key: &str| format!("{} {key}", layer.label);
        let mut stack = vec![(String::new(), &layer.toml, 0usize)];
        while let Some((key, value, depth)) = stack.pop() {
            if depth > MAX_CONFIG_DEPTH {
                continue;
            }
            match value {
                toml::Value::Table(table) => {
                    let last = key.rsplit('.').next().unwrap_or_default();
                    if last == "mcp_servers" {
                        for (server, entry) in table {
                            self.mcp_server(layer, &format!("{key}.{server}"), entry);
                        }
                        continue;
                    }
                    if last == "model_providers" {
                        for (provider, entry) in table {
                            self.model_provider(
                                layer,
                                &format!("{key}.{provider}"),
                                provider,
                                entry,
                            );
                        }
                        continue;
                    }
                    for (child, child_value) in table {
                        let child_key = if key.is_empty() {
                            child.clone()
                        } else {
                            format!("{key}.{child}")
                        };
                        if (child == "hooks" && depth == 0) || child == "notify" {
                            self.push(
                                FindingKind::Integration,
                                SecretClass::Route,
                                Disposition::NotContained,
                                location(&child_key),
                                Vec::new(),
                            );
                        }
                        if child == "writable_roots" {
                            self.mounts(layer, &child_key, child_value);
                            continue;
                        }
                        stack.push((child_key, child_value, depth + 1));
                    }
                }
                toml::Value::Array(items) => {
                    for item in items {
                        stack.push((key.clone(), item, depth + 1));
                    }
                }
                toml::Value::String(text) => {
                    let leaf = key.rsplit('.').next().unwrap_or_default();
                    let secret_key = is_secret_config_key(leaf);
                    if text.is_empty() || is_reference(text) || is_non_secret_setting(&key, text) {
                        continue;
                    }
                    if (secret_key && !looks_like_name_or_path(text)) || self.detector.secret(text)
                    {
                        let (disposition, paths) = match &layer.path {
                            Some(path) => (Disposition::Isolate, vec![path.clone()]),
                            None => (Disposition::Migrate, Vec::new()),
                        };
                        self.push(
                            FindingKind::ConfigLiteral,
                            SecretClass::UnmanagedSecret,
                            disposition,
                            location(&key),
                            paths,
                        );
                    }
                }
                _ => {}
            }
        }
    }

    fn literal(&self, value: &toml::Value) -> bool {
        value
            .as_str()
            .is_some_and(|text| !text.is_empty() && !is_reference(text))
    }

    fn mcp_server(&mut self, layer: &ConfigLayerInput, key: &str, entry: &toml::Value) {
        let Some(table) = entry.as_table() else {
            return;
        };
        let location = |suffix: &str| format!("{} {key}{suffix}", layer.label);
        let mut literals = Vec::new();
        if let Some(env) = table.get("env").and_then(toml::Value::as_table) {
            for (name, value) in env {
                if self.literal(value)
                    && (is_secret_name(name)
                        || value
                            .as_str()
                            .is_some_and(|text| self.detector.secret(text)))
                {
                    literals.push(format!(".env.{name}"));
                }
            }
        }
        if let Some(headers) = table.get("http_headers").and_then(toml::Value::as_table) {
            for (name, value) in headers {
                if self.literal(value) && is_auth_header(name) {
                    literals.push(format!(".http_headers.{name}"));
                }
            }
        }
        if table
            .get("bearer_token")
            .is_some_and(|value| self.literal(value))
        {
            literals.push(".bearer_token".to_string());
        }
        for suffix in literals {
            self.push(
                FindingKind::McpSecretLiteral,
                SecretClass::UnmanagedSecret,
                Disposition::Migrate,
                location(&suffix),
                Vec::new(),
            );
        }
        for reference in ["bearer_token_env_var", "env_http_headers", "env_vars"] {
            if table.contains_key(reference) {
                self.push(
                    FindingKind::Reference,
                    SecretClass::PermittedDerived,
                    Disposition::Info,
                    location(&format!(".{reference}")),
                    Vec::new(),
                );
            }
        }
        let enabled = table
            .get("enabled")
            .and_then(toml::Value::as_bool)
            .unwrap_or(true);
        if enabled {
            self.push(
                FindingKind::Integration,
                SecretClass::Route,
                Disposition::NotContained,
                location(""),
                Vec::new(),
            );
        }
    }

    fn model_provider(
        &mut self,
        layer: &ConfigLayerInput,
        key: &str,
        provider: &str,
        entry: &toml::Value,
    ) {
        let Some(table) = entry.as_table() else {
            return;
        };
        let location = |suffix: &str| format!("{} {key}{suffix}", layer.label);
        if table
            .get("experimental_bearer_token")
            .is_some_and(|value| self.literal(value))
        {
            self.push(
                FindingKind::ProviderSecretLiteral,
                SecretClass::UnmanagedSecret,
                Disposition::Migrate,
                location(".experimental_bearer_token"),
                Vec::new(),
            );
        }
        if let Some(headers) = table.get("http_headers").and_then(toml::Value::as_table) {
            for (name, value) in headers {
                if self.literal(value) && is_auth_header(name) {
                    self.push(
                        FindingKind::ProviderSecretLiteral,
                        SecretClass::UnmanagedSecret,
                        Disposition::Migrate,
                        location(&format!(".http_headers.{name}")),
                        Vec::new(),
                    );
                }
            }
        }
        if table.get("auth").is_some_and(toml::Value::is_table) {
            self.exec_provider_auth.insert(provider.to_string());
            self.push(
                FindingKind::ExecProviderAuth,
                SecretClass::Route,
                Disposition::Info,
                location(".auth"),
                Vec::new(),
            );
        }
        for reference in ["env_key", "env_http_headers"] {
            if table.contains_key(reference) {
                self.push(
                    FindingKind::Reference,
                    SecretClass::PermittedDerived,
                    Disposition::Info,
                    location(&format!(".{reference}")),
                    Vec::new(),
                );
            }
        }
    }

    /// Writable roots that contain a credential location. The protected
    /// level replaces writable roots, so these are recorded, not blocking.
    fn mounts(&mut self, layer: &ConfigLayerInput, key: &str, value: &toml::Value) {
        let credential_paths = self
            .findings
            .iter()
            .filter(|finding| finding.disposition == Disposition::Isolate)
            .flat_map(|finding| finding.paths.clone())
            .collect::<Vec<_>>();
        for root in value
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(toml::Value::as_str)
        {
            let root = PathBuf::from(root);
            if credential_paths.iter().any(|path| path.starts_with(&root)) {
                self.push(
                    FindingKind::ProtectedMount,
                    SecretClass::Route,
                    Disposition::Info,
                    format!("{} {key} {}", layer.label, self.display(&root)),
                    Vec::new(),
                );
            }
        }
    }

    fn finish(self) -> Inventory {
        let stores = std::fs::read_dir(self.sources.codex_home.join("secrets"))
            .into_iter()
            .flatten()
            .filter_map(Result::ok)
            .map(|entry| entry.path())
            .filter(|path| path.extension().is_some_and(|extension| extension == "age"))
            .collect::<Vec<_>>();
        let usable = |path: &PathBuf| {
            open_regular(path).is_ok_and(|mut file| {
                let mut header = [0u8; 21];
                std::io::Read::read_exact(&mut file, &mut header).is_ok()
                    && &header == b"age-encryption.org/v1"
            })
        };
        let vault = if stores.is_empty() {
            VaultState::Absent
        } else if stores.iter().all(usable) {
            VaultState::Present
        } else {
            VaultState::Unusable
        };
        Inventory {
            findings: self.findings,
            manifest: self.manifest,
            ordinary_env_count: self.ordinary_env_count,
            exec_provider_auth: self.exec_provider_auth.into_iter().collect(),
            vault,
            migration_unfinished: std::fs::symlink_metadata(super::migration::journal_path(
                &self.sources.codex_home,
            ))
            .is_ok(),
        }
    }
}

/// `export NAME=value`, `NAME=value`, `declare -x NAME=value` or fish
/// `set -gx NAME value`.
fn parse_assignment(line: &str) -> Option<(&str, &str)> {
    let line = line.trim();
    if line.starts_with('#') {
        return None;
    }
    if let Some(rest) = line.strip_prefix("set ") {
        // Walk the tokens: the name is the first one that is not a flag
        // (searching for it could match inside a flag such as `-gx`).
        let mut offset = 0;
        for token in rest.split(' ') {
            let start = offset;
            offset += token.len() + 1;
            if token.is_empty() || token.starts_with('-') {
                continue;
            }
            return Some((token, rest.get(start + token.len()..)?.trim()));
        }
        return None;
    }
    let line = line
        .strip_prefix("export ")
        .or_else(|| line.strip_prefix("declare -x "))
        .unwrap_or(line)
        .trim_start();
    let (name, value) = line.split_once('=')?;
    let valid = !name.is_empty()
        && name
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || ch == '_');
    valid.then_some((name, value.trim()))
}

fn unquote(value: &str) -> &str {
    let value = value.split(" #").next().unwrap_or(value).trim();
    value
        .strip_prefix('"')
        .and_then(|rest| rest.strip_suffix('"'))
        .or_else(|| {
            value
                .strip_prefix('\'')
                .and_then(|rest| rest.strip_suffix('\''))
        })
        .unwrap_or(value)
}

/// A variable or command substitution resolves the value elsewhere.
fn is_reference(value: &str) -> bool {
    value.starts_with('$') || value.contains("$(") || value.contains('`')
}

fn is_secret_config_key(key: &str) -> bool {
    let lower = key.to_ascii_lowercase();
    ["token", "secret", "password", "passphrase", "api_key", "apikey", "credential"]
        .iter()
        .any(|fragment| lower.contains(fragment))
        && !lower.ends_with("_env_var")
        && !lower.ends_with("_env")
        && !lower.ends_with("_path")
        && !lower.ends_with("_file")
        && lower != "env_key"
        // Path keys (permission profiles) are not secrets.
        && !lower.contains('/')
}

/// #438: top-level settings named like secrets whose values are enum
/// choices, never credentials, with every value they accept.
const NON_SECRET_SETTINGS: &[(&str, &[&str])] = &[
    (
        "cli_auth_credentials_store",
        &["file", "keyring", "auto", "ephemeral"],
    ),
    ("mcp_oauth_credentials_store", &["auto", "file", "keyring"]),
    (
        "model_auto_compact_token_limit_scope",
        &["total", "body_after_prefix"],
    ),
];

/// Whether the top-level `key` (the full dotted key, compared exactly) holds
/// one of its enum choices. Any other value, or the name under another table
/// (`shell_environment_policy.set.CLI_AUTH_CREDENTIALS_STORE`), is checked
/// as before.
fn is_non_secret_setting(key: &str, value: &str) -> bool {
    NON_SECRET_SETTINGS
        .iter()
        .any(|(name, values)| *name == key && values.contains(&value))
}

/// Variable names and paths stored under a secret-looking key are references.
fn looks_like_name_or_path(text: &str) -> bool {
    text.starts_with('/')
        || text.starts_with('~')
        || text.starts_with("./")
        || text
            .chars()
            .all(|ch| ch.is_ascii_uppercase() || ch.is_ascii_digit() || ch == '_')
}

fn is_auth_header(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    ["authorization", "token", "key", "secret", "cookie", "auth"]
        .iter()
        .any(|fragment| lower.contains(fragment))
}
