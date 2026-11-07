//! PF-29-S01 protected-mode activation preflight (feature
//! `protected_mode_preflight`).
//!
//! A protected level may be chosen only when the boundary it claims holds:
//! the secretless launch contract, isolated credential broker and output
//! gate are on, the OS sandbox is supported, the vault is usable, no provider
//! signs in through an unchecked command, and the [inventory](super::inventory)
//! finds no raw secret that would still reach an agent or the model. Paths the
//! level can deny are listed for isolation instead of blocking.
//!
//! The preflight only reads; it never runs provider commands, opens the vault
//! or changes a file. [`Preflight::recheck`] detects drift between the review
//! and the confirmation by comparing keyed manifests.

use std::collections::BTreeMap;
use std::collections::BTreeSet;

use codex_config::ConfigLayerSource;
use codex_features::Feature;

pub use super::inventory::ConfigLayerInput;
pub use super::inventory::Disposition;
pub use super::inventory::EntryStatus;
pub use super::inventory::Finding;
pub use super::inventory::FindingKind;
pub use super::inventory::Inventory;
pub use super::inventory::InventorySources;
pub use super::inventory::LIMITS;
pub use super::inventory::Manifest;
pub use super::inventory::ManifestEntry;
pub use super::inventory::Scope;
pub use super::inventory::SecretClass;
pub use super::inventory::VaultState;
use super::inventory::take_with_key;
/// PF-29-S02 migration of blocking findings.
pub use super::migration;
use crate::config::Config;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ReadinessFlags {
    pub secretless_launch: bool,
    pub credential_broker: bool,
    pub output_gate: bool,
    /// PF-24-S02: `source_envelopes`. Core's protected levels refuse model
    /// requests that carry unlabelled external content, so without it a
    /// session cannot work under them.
    pub untrusted_content: bool,
}

impl ReadinessFlags {
    pub fn from_config(config: &Config) -> Self {
        Self {
            secretless_launch: config.features.enabled(Feature::SecretlessAgentLaunch),
            credential_broker: config.features.enabled(Feature::IsolatedCredentialBroker),
            output_gate: config.features.enabled(Feature::SecretOutputGate),
            untrusted_content: config.features.enabled(Feature::SourceEnvelopes),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ReadinessState {
    Ready,
    /// A required control is off; the text says how to turn it on.
    Missing(String),
    /// Present but not usable as checked.
    Incomplete(String),
    /// A check that would run a command was skipped; it needs the human.
    ConsentRequired(String),
    Unsupported(String),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReadinessItem {
    pub id: String,
    pub label: String,
    pub state: ReadinessState,
}

impl ReadinessItem {
    pub fn is_ready(&self) -> bool {
        self.state == ReadinessState::Ready
    }
}

#[derive(Clone)]
pub struct Preflight {
    pub readiness: Vec<ReadinessItem>,
    pub inventory: Inventory,
    key: [u8; 32],
}

impl std::fmt::Debug for Preflight {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Preflight")
            .field("readiness", &self.readiness)
            .field("inventory", &self.inventory)
            .finish_non_exhaustive()
    }
}

/// What changed between two preflights, by finding ID.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Drift {
    pub added: Vec<String>,
    pub removed: Vec<String>,
    pub changed: Vec<String>,
    pub readiness_changed: bool,
}

impl Drift {
    pub fn is_empty(&self) -> bool {
        self.added.is_empty()
            && self.removed.is_empty()
            && self.changed.is_empty()
            && !self.readiness_changed
    }
}

impl Preflight {
    pub fn run(sources: &InventorySources, flags: ReadinessFlags) -> Self {
        Self::run_with_key(sources, flags, rand::random())
    }

    pub(crate) fn run_with_key(
        sources: &InventorySources,
        flags: ReadinessFlags,
        key: [u8; 32],
    ) -> Self {
        let inventory = take_with_key(sources, key);
        let readiness = readiness(&inventory, flags);
        Self {
            readiness,
            inventory,
            key,
        }
    }

    /// Clean only when every control is ready and no finding blocks.
    pub fn is_clean(&self) -> bool {
        self.readiness.iter().all(ReadinessItem::is_ready)
            && self.inventory.blocking().next().is_none()
    }

    /// One line per blocker. Locations and remedies only, never values.
    pub fn blockers(&self) -> Vec<String> {
        let mut lines = self
            .readiness
            .iter()
            .filter_map(|item| {
                let reason = match &item.state {
                    ReadinessState::Ready => return None,
                    ReadinessState::Missing(reason)
                    | ReadinessState::Incomplete(reason)
                    | ReadinessState::ConsentRequired(reason)
                    | ReadinessState::Unsupported(reason) => reason,
                };
                Some(format!("{}: {reason}", item.label))
            })
            .collect::<Vec<_>>();
        lines.extend(self.inventory.blocking().map(|finding| {
            let remedy = match finding.disposition {
                Disposition::RemoveFromContext => {
                    "the model reads memories; remove the secret from this file"
                }
                _ => "move it into the vault and use a reference, or remove it",
            };
            format!(
                "{}: {} ({}); {remedy} [{}]",
                finding.location,
                finding.class.as_str(),
                finding.kind.as_str().replace('-', " "),
                finding.id
            )
        }));
        lines
    }

    /// Takes the inventory again with the same key and reports any change.
    /// Corbanu's own history stores and databases do not count (#802).
    pub fn recheck(&self, sources: &InventorySources, flags: ReadinessFlags) -> (Self, Drift) {
        let next = Self::run_with_key(sources, flags, self.key);
        let drift = drift(self, &next, &sources.codex_home);
        (next, drift)
    }
}

fn readiness(inventory: &Inventory, flags: ReadinessFlags) -> Vec<ReadinessItem> {
    let item = |id: &str, label: &str, state: ReadinessState| ReadinessItem {
        id: id.to_string(),
        label: label.to_string(),
        state,
    };
    let required = |on: bool, key: &str| {
        if on {
            ReadinessState::Ready
        } else {
            ReadinessState::Missing(format!("off; turn on the `{key}` feature"))
        }
    };
    let sandbox = if cfg!(any(target_os = "macos", target_os = "linux")) {
        ReadinessState::Ready
    } else {
        ReadinessState::Unsupported(
            "protected launch is not available on this platform yet (PF-27-S06)".to_string(),
        )
    };
    let vault = match inventory.vault {
        VaultState::Absent | VaultState::Present => ReadinessState::Ready,
        VaultState::Unusable => ReadinessState::Incomplete(
            "a vault store file cannot be read or is damaged".to_string(),
        ),
    };
    let mut items = vec![
        item("os-sandbox", "OS sandbox", sandbox),
        item(
            "secretless-launch",
            "Secretless agent launch",
            required(flags.secretless_launch, "secretless_agent_launch"),
        ),
        item(
            "credential-broker",
            "Isolated credential broker",
            required(flags.credential_broker, "isolated_credential_broker"),
        ),
        item(
            "output-gate",
            "Secret output gate",
            required(flags.output_gate, "secret_output_gate"),
        ),
        item(
            "untrusted-content",
            "Untrusted-content labels",
            required(flags.untrusted_content, "source_envelopes"),
        ),
        item("vault", "Vault", vault),
    ];
    if inventory.migration_unfinished {
        items.push(item(
            "migration",
            "Credential migration",
            ReadinessState::Incomplete(
                "a credential migration did not finish; press r in this review to finish it"
                    .to_string(),
            ),
        ));
    }
    items.extend(inventory.exec_provider_auth.iter().map(|provider| {
        item(
            &format!("exec-provider-auth-{provider}"),
            &format!("Provider `{provider}` sign-in"),
            ReadinessState::ConsentRequired(format!(
                "signs in by running a command outside the protected boundary; preflight did not run it. Remove `model_providers.{provider}.auth` or use an environment key"
            )),
        )
    }));
    items
}

/// #802: the Corbanu home's history stores, which Corbanu itself writes
/// while it runs (and for some seconds after start-up).
const RUNTIME_STORES: [&str; 5] = [
    "sessions",
    "archived_sessions",
    "history.jsonl",
    "shell_snapshots",
    "log",
];

/// Whether `entry` is one of Corbanu's own history stores or state
/// database files (`-wal`, `-shm` and `-journal` included, #1428), not a
/// symlink. A protected launch denies these whatever they hold and whether
/// or not they exist (see [`CORBANU_HOME_STORES`] and [`database_glob`]),
/// so Corbanu's own writes to them leave the review true and are not drift.
/// A symlink there still counts, and so does a database file that is not a
/// regular file with a single link, and a folder named like a database (the
/// glob does not deny what it holds).
fn runtime_store(entry: &ManifestEntry, codex_home: &std::path::Path) -> bool {
    let plain_file = entry.regular && entry.links.is_none_or(|links| links == 1);
    entry.scope == Scope::CorbanuHome
        && !entry.symlink
        && ((plain_file && is_database_file(codex_home, &entry.path))
            || ((plain_file || entry.len.is_none())
                && entry.path.parent() == Some(codex_home)
                && entry
                    .path
                    .file_name()
                    .is_some_and(|name| RUNTIME_STORES.iter().any(|store| name == *store))))
}

fn drift(before: &Preflight, after: &Preflight, codex_home: &std::path::Path) -> Drift {
    let entries = |preflight: &Preflight| {
        preflight.inventory.manifest.entries.iter().fold(
            BTreeMap::<String, Vec<ManifestEntry>>::new(),
            |mut entries, entry| {
                entries
                    .entry(entry.finding_id.clone())
                    .or_default()
                    .push(entry.clone());
                entries
            },
        )
    };
    let (old_entries, new_entries) = (entries(before), entries(after));
    // A database finding holds one entry per file (#1428): it is ignored
    // only when every one of them is a runtime store.
    let counts = |entries: &BTreeMap<String, Vec<ManifestEntry>>, id: &String| {
        !entries
            .get(id)
            .is_some_and(|entries| entries.iter().all(|entry| runtime_store(entry, codex_home)))
    };
    let ids = |preflight: &Preflight| preflight.inventory.finding_ids();
    let (old, new) = (ids(before), ids(after));
    Drift {
        added: new
            .difference(&old)
            .filter(|id| counts(&new_entries, id))
            .cloned()
            .collect(),
        removed: old
            .difference(&new)
            .filter(|id| counts(&old_entries, id))
            .cloned()
            .collect(),
        changed: old
            .intersection(&new)
            .filter(|id| {
                old_entries.get(*id) != new_entries.get(*id)
                    && (counts(&old_entries, id) || counts(&new_entries, id))
            })
            .cloned()
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect(),
        readiness_changed: before.readiness != after.readiness,
    }
}

/// Corbanu's own stores that a protected launch denies even before they
/// exist: Corbanu creates them during startup, after launch takes its
/// inventory.
pub const CORBANU_HOME_STORES: [&str; 9] = [
    ".credentials.json",
    "provider_auth.json",
    ".env",
    "wallet",
    "sessions",
    "archived_sessions",
    "history.jsonl",
    "shell_snapshots",
    "log",
];

/// The database init lock (#549), next to the databases.
pub const DB_INIT_LOCK: &str = "pfterminal_db_init.lock";

/// #611: what Corbanu keeps in its database folder besides the databases:
/// the init lock, the GPU controller's lock, pending accounting records
/// (`codex_state::PENDING_DIR`) and databases set aside by recovery.
pub const DATABASE_FOLDER_ENTRIES: &[&str] = &[
    DB_INIT_LOCK,
    "gpu-controller.lock",
    "accounting-pending",
    "db-backups",
];

/// Deny-read glob for Corbanu's state databases and their `-wal`/`-shm`
/// files in `folder` (the Corbanu home or a separate database folder), which
/// come and go while Corbanu runs. The folder is escaped (#651).
pub fn database_glob(folder: &std::path::Path) -> std::path::PathBuf {
    std::path::PathBuf::from(glob_escape_path(folder)).join("*.sqlite*")
}

/// #651: `path` as a glob pattern that matches only itself, each glob
/// metacharacter in a one-character class (`[*]`, `[?]`, `[[]`, `[{]`,
/// `[}]`). A `]` outside a class is already literal to every matcher
/// (Seatbelt's translation, ripgrep and globset).
pub fn glob_escape_path(path: &std::path::Path) -> String {
    path.to_string_lossy()
        .chars()
        .map(|ch| match ch {
            '*' | '?' | '[' | '{' | '}' => format!("[{ch}]"),
            other => other.to_string(),
        })
        .collect()
}

/// Whether `path` contains glob syntax, so it cannot prefix a glob pattern.
pub fn has_glob_chars(path: &std::path::Path) -> bool {
    path.to_string_lossy()
        .contains(['*', '?', '[', ']', '{', '}'])
}

/// The `-wal`, `-shm` and `-journal` files that come and go beside a
/// database, whether or not they exist now.
pub fn database_siblings(path: &std::path::Path) -> Vec<std::path::PathBuf> {
    let name = path.to_string_lossy();
    let base = name
        .split_once(".sqlite")
        .map_or(name.as_ref(), |(base, _)| base);
    ["", "-wal", "-shm", "-journal"]
        .iter()
        .map(|suffix| std::path::PathBuf::from(format!("{base}.sqlite{suffix}")))
        .collect()
}

/// Whether `path` is one of the files [`database_glob`] denies.
pub fn is_database_file(codex_home: &std::path::Path, path: &std::path::Path) -> bool {
    path.parent() == Some(codex_home)
        && path
            .file_name()
            .is_some_and(|name| name.to_string_lossy().contains(".sqlite"))
}

/// The home directory the inventory checks (`$HOME` on Unix).
pub fn home_dir() -> Option<std::path::PathBuf> {
    dirs::home_dir()
}

/// Sources for this machine and the loaded config: the process environment,
/// the home directory and every enabled config layer (shadowed values too).
pub fn sources_from_config(config: &Config) -> InventorySources {
    let config_layers = config
        .config_layer_stack
        .layers_high_to_low()
        .into_iter()
        .filter(|layer| !layer.is_disabled())
        .map(|layer| {
            let (label, path) = match &layer.name {
                ConfigLayerSource::User { file, profile } => (
                    match profile {
                        Some(profile) => format!("user config (profile {profile})"),
                        None => "user config".to_string(),
                    },
                    Some(file.to_path_buf()),
                ),
                ConfigLayerSource::Project { dot_codex_folder } => (
                    "project config".to_string(),
                    Some(dot_codex_folder.join("config.toml").to_path_buf()),
                ),
                ConfigLayerSource::System { file }
                | ConfigLayerSource::LegacyManagedConfigTomlFromFile { file } => {
                    ("managed config".to_string(), Some(file.to_path_buf()))
                }
                ConfigLayerSource::SessionFlags => ("launch flags".to_string(), None),
                ConfigLayerSource::Mdm { .. }
                | ConfigLayerSource::EnterpriseManaged { .. }
                | ConfigLayerSource::LegacyManagedConfigTomlFromMdm => {
                    ("managed config".to_string(), None)
                }
            };
            ConfigLayerInput {
                label,
                path,
                toml: layer.config.clone(),
            }
        })
        .collect();
    InventorySources {
        codex_home: config.codex_home.to_path_buf(),
        home: dirs::home_dir(),
        cwd: config.cwd.to_path_buf(),
        env: std::env::vars().collect(),
        config_layers,
    }
}

/// Sources available before any config is loaded (launch-time isolation):
/// files, plus the user and project `config.toml` read directly; no
/// environment.
pub fn file_sources(
    codex_home: &std::path::Path,
    home: Option<&std::path::Path>,
    cwd: Option<&std::path::Path>,
) -> InventorySources {
    let mut files = vec![("user config", codex_home.join("config.toml"))];
    if let Some(cwd) = cwd {
        files.push(("project config", cwd.join(".codex").join("config.toml")));
    }
    let config_layers = files
        .into_iter()
        .filter_map(|(label, path)| {
            let toml = toml::from_str(&std::fs::read_to_string(&path).ok()?).ok()?;
            Some(ConfigLayerInput {
                label: label.to_string(),
                path: Some(path),
                toml,
            })
        })
        .collect();
    InventorySources {
        codex_home: codex_home.to_path_buf(),
        home: home.map(std::path::Path::to_path_buf),
        cwd: cwd.map(std::path::Path::to_path_buf).unwrap_or_default(),
        env: Vec::new(),
        config_layers,
    }
}
