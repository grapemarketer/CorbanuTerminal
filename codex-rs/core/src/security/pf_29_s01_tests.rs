//! PF-29-S01 inventory and preflight regressions. Synthetic secrets only.

use std::path::Path;
use std::path::PathBuf;

use pretty_assertions::assert_eq;
use tempfile::TempDir;

use super::preflight::ConfigLayerInput;
use super::preflight::Disposition;
use super::preflight::Drift;
#[cfg(unix)]
use super::preflight::EntryStatus;
use super::preflight::Finding;
use super::preflight::FindingKind;
use super::preflight::InventorySources;
use super::preflight::Preflight;
use super::preflight::ReadinessFlags;
use super::preflight::ReadinessState;
use super::preflight::SecretClass;

const FAKE_KEY: &str = "sk-pf29fakeAAAAAAAAAAAAAAAAAAAAAAAAAAAA";
const FAKE_CORE_VALUE: &str = "pf29-core-managed-value-0001";
const KEY: [u8; 32] = [7; 32];
const ALL_ON: ReadinessFlags = ReadinessFlags {
    secretless_launch: true,
    credential_broker: true,
    output_gate: true,
    untrusted_content: true,
};

struct Fixture {
    root: TempDir,
}

impl Fixture {
    fn new() -> Self {
        let root = TempDir::new().unwrap_or_else(|err| panic!("tempdir: {err}"));
        for dir in ["home", "corbanu", "work"] {
            std::fs::create_dir_all(root.path().join(dir))
                .unwrap_or_else(|err| panic!("mkdir: {err}"));
        }
        Self { root }
    }

    fn home(&self) -> PathBuf {
        self.root.path().join("home")
    }

    fn corbanu(&self) -> PathBuf {
        self.root.path().join("corbanu")
    }

    fn write(&self, path: &Path, contents: &[u8]) {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).unwrap_or_else(|err| panic!("mkdir: {err}"));
        }
        std::fs::write(path, contents).unwrap_or_else(|err| panic!("write: {err}"));
    }

    fn sources(&self) -> InventorySources {
        InventorySources {
            codex_home: self.corbanu(),
            home: Some(self.home()),
            cwd: self.root.path().join("work"),
            env: Vec::new(),
            config_layers: Vec::new(),
        }
    }

    fn run(&self, sources: &InventorySources) -> Preflight {
        Preflight::run_with_key(sources, ALL_ON, KEY)
    }
}

fn find(preflight: &Preflight, kind: FindingKind) -> Vec<&Finding> {
    preflight
        .inventory
        .findings
        .iter()
        .filter(|finding| finding.kind == kind)
        .collect()
}

/// The blocker every Windows machine has until PF-27-S06 brings protected
/// launch there (#691).
const WINDOWS_SANDBOX_BLOCKER: &str =
    "OS sandbox: protected launch is not available on this platform yet (PF-27-S06)";

/// [`Preflight::is_clean`] on macOS and Linux. Elsewhere (today, Windows)
/// the OS sandbox row is never ready yet, so it must be exactly
/// [`WINDOWS_SANDBOX_BLOCKER`] and the fixture is clean when nothing else
/// blocks.
pub(super) fn clean(preflight: &Preflight) -> bool {
    if cfg!(any(target_os = "macos", target_os = "linux")) {
        return preflight.is_clean();
    }
    let mut blockers = preflight.blockers();
    let Some(at) = blockers
        .iter()
        .position(|line| line == WINDOWS_SANDBOX_BLOCKER)
    else {
        panic!("no Windows OS sandbox blocker in {blockers:?}");
    };
    blockers.remove(at);
    blockers.is_empty()
}

fn layer(label: &str, path: Option<PathBuf>, toml: &str) -> ConfigLayerInput {
    ConfigLayerInput {
        label: label.to_string(),
        path,
        toml: toml::from_str(toml).unwrap_or_else(|err| panic!("toml: {err}")),
    }
}

#[test]
fn pf_29_s01_empty_machine_is_clean_only_when_controls_are_ready() {
    let fixture = Fixture::new();
    let sources = fixture.sources();
    assert!(clean(&fixture.run(&sources)));

    let off = Preflight::run_with_key(&sources, ReadinessFlags::default(), KEY);
    assert!(!clean(&off));
    let blockers = off.blockers();
    for key in [
        "secretless_agent_launch",
        "isolated_credential_broker",
        "secret_output_gate",
    ] {
        assert!(
            blockers.iter().any(|line| line.contains(key)),
            "{key} missing from {blockers:?}"
        );
    }
}

#[test]
fn pf_29_s01_credential_files_and_ssh_keys_are_isolated_not_blocking() {
    let fixture = Fixture::new();
    let home = fixture.home();
    fixture.write(
        &home.join(".ssh/id_ed25519"),
        b"-----BEGIN OPENSSH PRIVATE KEY-----\nfake\n",
    );
    fixture.write(&home.join(".ssh/id_ed25519.pub"), b"ssh-ed25519 AAAA fake");
    fixture.write(
        &home.join(".ssh/deploy"),
        b"-----BEGIN RSA PRIVATE KEY-----\nfake\n",
    );
    fixture.write(&home.join(".ssh/config"), b"Host *\n");
    fixture.write(&home.join(".kube/config"), b"token: fake");
    fixture.write(&home.join(".netrc"), b"machine x login y password fake");
    let preflight = fixture.run(&fixture.sources());

    let isolated = preflight.inventory.isolation_paths();
    // Whole credential folders, so later or oddly named keys are covered.
    for path in [".ssh", ".ssh/id_ed25519", ".ssh/deploy", ".kube", ".netrc"] {
        assert!(isolated.contains(&home.join(path)), "{path} not isolated");
    }
    for path in [".ssh/id_ed25519.pub", ".ssh/config", ".kube/config"] {
        assert!(
            !isolated.contains(&home.join(path)),
            "{path} listed separately"
        );
    }
    assert_eq!(find(&preflight, FindingKind::SshPrivateKey).len(), 2);
    assert!(clean(&preflight), "{:?}", preflight.blockers());
}

#[test]
fn pf_29_s01_shell_profile_exports_block_and_never_expose_values() {
    let fixture = Fixture::new();
    let profile = format!(
        "# comment {FAKE_KEY}\nexport OPENAI_API_KEY=\"{FAKE_KEY}\"\nexport SAFE_URL=https://example.com\nexport GH_TOKEN=\"$(corbanu vault auth-helper gh)\"\nPLAIN={FAKE_KEY}\n"
    );
    fixture.write(&fixture.home().join(".zshrc"), profile.as_bytes());
    fixture.write(
        &fixture.home().join(".config/fish/config.fish"),
        format!("set -gx ANTHROPIC_API_KEY {FAKE_KEY}\n").as_bytes(),
    );
    let preflight = fixture.run(&fixture.sources());

    let exports = find(&preflight, FindingKind::ShellProfileExport);
    let locations = exports
        .iter()
        .map(|finding| finding.location.clone())
        .collect::<Vec<_>>();
    assert_eq!(
        locations,
        vec![
            "~/.zshrc:2 OPENAI_API_KEY".to_string(),
            "~/.zshrc:5 PLAIN".to_string(),
            "~/.config/fish/config.fish:1 ANTHROPIC_API_KEY".to_string(),
        ]
    );
    assert!(
        exports
            .iter()
            .all(|finding| finding.disposition == Disposition::Migrate)
    );
    let references = find(&preflight, FindingKind::Reference);
    assert_eq!(references.len(), 1);
    assert_eq!(references[0].class, SecretClass::PermittedDerived);
    assert!(!clean(&preflight));

    let rendered = format!("{preflight:?}{:?}", preflight.blockers());
    assert!(!rendered.contains(FAKE_KEY), "value leaked");
    assert!(!rendered.contains("pf29fake"), "partial value leaked");
}

#[test]
fn pf_29_s01_environment_is_classified_without_values() {
    let fixture = Fixture::new();
    let mut sources = fixture.sources();
    sources.env = vec![
        ("OPENAI_API_KEY".to_string(), FAKE_KEY.to_string()),
        ("MY_SERVICE_TOKEN".to_string(), FAKE_CORE_VALUE.to_string()),
        (
            "DATABASE_URL".to_string(),
            "postgres://app:hunter22@db/x".to_string(),
        ),
        ("INNOCENT".to_string(), FAKE_KEY.to_string()),
        ("PATH".to_string(), "/usr/bin".to_string()),
        ("EMPTY_TOKEN".to_string(), String::new()),
    ];
    let preflight = fixture.run(&sources);
    let env = find(&preflight, FindingKind::EnvironmentVariable)
        .into_iter()
        .map(|finding| (finding.location.clone(), finding.class, finding.disposition))
        .collect::<Vec<_>>();
    assert_eq!(
        env,
        vec![
            (
                "environment DATABASE_URL".to_string(),
                SecretClass::UnmanagedSecret,
                Disposition::Migrate
            ),
            (
                "environment INNOCENT".to_string(),
                SecretClass::UnmanagedSecret,
                Disposition::Migrate
            ),
            (
                "environment MY_SERVICE_TOKEN".to_string(),
                SecretClass::UnmanagedSecret,
                Disposition::Denied
            ),
            (
                "environment OPENAI_API_KEY".to_string(),
                SecretClass::ManagedSecret,
                Disposition::Denied
            ),
        ]
    );
    assert_eq!(preflight.inventory.ordinary_env_count, 1);
    assert!(!format!("{preflight:?}").contains("hunter22"));
}

#[test]
fn pf_29_s01_config_layers_including_shadowed_values() {
    let fixture = Fixture::new();
    let user_path = fixture.corbanu().join("config.toml");
    let user = layer(
        "user config",
        Some(user_path.clone()),
        &format!(
            r#"
notify = ["say"]
[shell_environment_policy.set]
DEPLOY_TOKEN = "{FAKE_CORE_VALUE}"
[mcp_servers.docs]
command = "docs-mcp"
env = {{ DOCS_API_KEY = "{FAKE_KEY}", LOG_LEVEL = "debug" }}
[mcp_servers.remote]
url = "https://mcp.example"
http_headers = {{ Authorization = "Bearer abc", Accept = "json" }}
bearer_token_env_var = "REMOTE_TOKEN"
[model_providers.corp]
experimental_bearer_token = "corp-literal-token"
env_key = "CORP_KEY"
[model_providers.exec]
auth = {{ command = "print-token" }}
[hooks]
"#
        ),
    );
    // A higher layer replaces the server; the user file still holds the value.
    let project = layer(
        "project config",
        Some(fixture.root.path().join("work/.codex/config.toml")),
        r#"
[mcp_servers.docs]
command = "docs-mcp"
"#,
    );
    let flags = layer(
        "launch flags",
        /*path*/ None,
        r#"permissions.p.filesystem."/x/secrets" = "deny""#,
    );
    let mut sources = fixture.sources();
    sources.config_layers = vec![flags, project, user];
    let preflight = fixture.run(&sources);

    let locations = |kind| {
        find(&preflight, kind)
            .into_iter()
            .map(|finding| finding.location.clone())
            .collect::<Vec<_>>()
    };
    assert_eq!(
        locations(FindingKind::McpSecretLiteral),
        vec![
            "user config mcp_servers.docs.env.DOCS_API_KEY".to_string(),
            "user config mcp_servers.remote.http_headers.Authorization".to_string(),
        ]
    );
    assert_eq!(
        locations(FindingKind::ProviderSecretLiteral),
        vec!["user config model_providers.corp.experimental_bearer_token".to_string()]
    );
    let set = find(&preflight, FindingKind::ConfigLiteral);
    assert_eq!(set.len(), 1);
    assert_eq!(
        set[0].location,
        "user config shell_environment_policy.set.DEPLOY_TOKEN"
    );
    assert_eq!(set[0].disposition, Disposition::Isolate);
    assert_eq!(set[0].paths, vec![user_path]);
    let routes = locations(FindingKind::Integration);
    for route in [
        "user config notify",
        "user config hooks",
        "user config mcp_servers.docs",
        "project config mcp_servers.docs",
        "user config mcp_servers.remote",
    ] {
        assert!(
            routes.contains(&route.to_string()),
            "{route} not in {routes:?}"
        );
    }
    assert!(
        find(&preflight, FindingKind::Integration)
            .iter()
            .all(|finding| finding.disposition == Disposition::NotContained)
    );
    assert!(preflight.readiness.iter().any(|item| matches!(
        item.state,
        ReadinessState::ConsentRequired(_)
    ) && item.label.contains("exec")));
    assert!(!format!("{preflight:?}").contains("corp-literal-token"));
}

#[test]
fn pf_29_s01_old_memory_content_blocks_until_removed() {
    let fixture = Fixture::new();
    let memories = fixture.corbanu().join("memories");
    fixture.write(&memories.join("clean.md"), b"prefers short answers");
    fixture.write(
        &memories.join("rollout_summaries/old.md"),
        format!("user pasted {FAKE_CORE_VALUE} once").as_bytes(),
    );
    fixture.write(&memories.join("raw.bin"), &[0xff, 0xfe, 0x00, 0x80]);
    let mut sources = fixture.sources();
    sources.env = vec![("SERVICE_SECRET".to_string(), FAKE_CORE_VALUE.to_string())];
    let preflight = fixture.run(&sources);

    let memory = find(&preflight, FindingKind::Memory);
    assert_eq!(memory.len(), 1);
    assert_eq!(
        memory[0].location,
        memories
            .join("rollout_summaries")
            .join("old.md")
            .display()
            .to_string()
    );
    assert_eq!(memory[0].disposition, Disposition::RemoveFromContext);
    assert!(!clean(&preflight));
}

#[cfg(unix)]
#[test]
fn pf_29_s01_symlinks_isolate_targets_and_survive_loops() {
    let fixture = Fixture::new();
    let home = fixture.home();
    let elsewhere = fixture.root.path().join("elsewhere/creds");
    fixture.write(&elsewhere, b"machine x password fake");
    std::os::unix::fs::symlink(&elsewhere, home.join(".netrc"))
        .unwrap_or_else(|err| panic!("{err}"));
    std::os::unix::fs::symlink(home.join("missing"), home.join(".git-credentials"))
        .unwrap_or_else(|err| panic!("{err}"));
    std::os::unix::fs::symlink(home.join(".pgpass"), home.join(".pgpass"))
        .unwrap_or_else(|err| panic!("{err}"));
    let preflight = fixture.run(&fixture.sources());

    let isolated = preflight.inventory.isolation_paths();
    let target = std::fs::canonicalize(&elsewhere).unwrap_or_else(|err| panic!("{err}"));
    assert!(isolated.contains(&home.join(".netrc")));
    assert!(isolated.contains(&target));
    let entry = |name: &str| {
        preflight
            .inventory
            .manifest
            .entries
            .iter()
            .find(|entry| entry.path == home.join(name))
            .cloned()
            .unwrap_or_else(|| panic!("{name} missing from manifest"))
    };
    assert_eq!(entry(".netrc").symlink_target, Some(target));
    assert_eq!(entry(".netrc").status, EntryStatus::Supported);
    for name in [".git-credentials", ".pgpass"] {
        assert!(
            matches!(entry(name).status, EntryStatus::Unreadable(_)),
            "{name}: {:?}",
            entry(name).status
        );
    }
}

#[cfg(unix)]
#[test]
fn pf_29_s01_denied_reads_are_listed_not_hidden() {
    use std::os::unix::fs::PermissionsExt as _;
    let fixture = Fixture::new();
    let netrc = fixture.home().join(".netrc");
    let profile = fixture.home().join(".bashrc");
    fixture.write(&netrc, b"machine x password fake");
    fixture.write(&profile, format!("export API_KEY={FAKE_KEY}\n").as_bytes());
    for path in [&netrc, &profile] {
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o000))
            .unwrap_or_else(|err| panic!("{err}"));
    }
    let preflight = fixture.run(&fixture.sources());
    for path in [&netrc, &profile] {
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
            .unwrap_or_else(|err| panic!("{err}"));
    }
    // Root reads everything; the denial cannot be observed then.
    if nix_is_root() {
        return;
    }
    let entry = preflight
        .inventory
        .manifest
        .entries
        .iter()
        .find(|entry| entry.path == netrc)
        .unwrap_or_else(|| panic!("netrc missing"));
    assert!(matches!(entry.status, EntryStatus::Unreadable(_)));
    assert_eq!(entry.mode, Some(0o000));
    let profile_finding = find(&preflight, FindingKind::ShellProfileExport);
    assert_eq!(profile_finding.len(), 1);
    assert_eq!(profile_finding[0].location, "~/.bashrc (not readable)");
}

#[cfg(unix)]
fn nix_is_root() -> bool {
    std::env::var("USER").is_ok_and(|user| user == "root")
}

/// The vault unlocks through the OS keyring only when a credential is used,
/// so a locked vault shows up as a store that cannot be read; a damaged
/// store is the same. An empty `secrets/` (lock file only) is no vault.
#[test]
fn pf_29_s01_locked_or_damaged_vault_is_incomplete_readiness() {
    let fixture = Fixture::new();
    let store = fixture.corbanu().join("secrets/local.age");
    fixture.write(&fixture.corbanu().join("secrets/.vault.lock"), b"");
    assert!(clean(&fixture.run(&fixture.sources())));

    let vault_incomplete = |preflight: &Preflight| {
        preflight
            .readiness
            .iter()
            .any(|item| item.id == "vault" && matches!(item.state, ReadinessState::Incomplete(_)))
    };
    fixture.write(&store, b"not an age file");
    let damaged = fixture.run(&fixture.sources());
    assert!(vault_incomplete(&damaged));
    assert!(!clean(&damaged));

    fixture.write(&store, b"age-encryption.org/v1\n-> scrypt salt 16\n---");
    let present = fixture.run(&fixture.sources());
    assert!(clean(&present), "{:?}", present.blockers());
    assert_eq!(
        find(&present, FindingKind::VaultStore)[0].disposition,
        Disposition::Denied
    );

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(&store, std::fs::Permissions::from_mode(0o000))
            .unwrap_or_else(|err| panic!("{err}"));
        let locked = fixture.run(&fixture.sources());
        std::fs::set_permissions(&store, std::fs::Permissions::from_mode(0o600))
            .unwrap_or_else(|err| panic!("{err}"));
        if !nix_is_root() {
            assert!(vault_incomplete(&locked));
        }
    }
}

#[test]
fn pf_29_s01_corrupt_snapshots_and_histories_are_isolated() {
    let fixture = Fixture::new();
    let home = fixture.corbanu();
    fixture.write(
        &home.join("shell_snapshots/s.sh"),
        &[0x00, 0xff, 0x13, 0x37],
    );
    fixture.write(&home.join("sessions/2026/x.jsonl"), b"{not json");
    fixture.write(&home.join("history.jsonl"), b"\xff\xfe");
    fixture.write(&home.join("state_5.sqlite"), b"garbage");
    fixture.write(&home.join("state_5.sqlite-wal"), b"garbage");
    fixture.write(&home.join("wallet/keystore"), b"fake");
    let preflight = fixture.run(&fixture.sources());
    let isolated = preflight.inventory.isolation_paths();
    for name in [
        "shell_snapshots",
        "sessions",
        "history.jsonl",
        "state_5.sqlite",
        "wallet",
    ] {
        assert!(isolated.contains(&home.join(name)), "{name} not isolated");
    }
    assert!(isolated.contains(&home.join("state_5.sqlite-wal")));
    assert_eq!(
        find(&preflight, FindingKind::Transcript)
            .iter()
            .filter(|finding| finding.location.contains("state_5"))
            .count(),
        1
    );
    assert_eq!(
        find(&preflight, FindingKind::Wallet)[0].class,
        SecretClass::CustodyKey
    );
    assert!(clean(&preflight), "{:?}", preflight.blockers());
}

/// #294: the secretless launch contract creates its protected directories
/// empty on Windows; an empty directory is not a finding.
#[test]
fn pf_29_s01_empty_protected_directories_are_not_findings() {
    let fixture = Fixture::new();
    let home = fixture.corbanu();
    for name in ["wallet", "secrets", "sessions", "log"] {
        std::fs::create_dir_all(home.join(name)).expect("empty dir");
    }
    let preflight = fixture.run(&fixture.sources());
    assert!(find(&preflight, FindingKind::Wallet).is_empty());
    assert!(find(&preflight, FindingKind::VaultStore).is_empty());
    assert!(find(&preflight, FindingKind::Transcript).is_empty());
}

#[test]
fn pf_29_s01_drift_between_preflight_and_activation() {
    let fixture = Fixture::new();
    let netrc = fixture.home().join(".netrc");
    fixture.write(&netrc, b"one");
    let sources = fixture.sources();
    let first = fixture.run(&sources);

    let (_, unchanged) = first.recheck(&sources, ALL_ON);
    assert!(unchanged.is_empty(), "{unchanged:?}");

    fixture.write(&netrc, b"two");
    fixture.write(
        &fixture.home().join(".zshenv"),
        format!("export X_TOKEN={FAKE_KEY}\n").as_bytes(),
    );
    let (second, drift) = first.recheck(&sources, ALL_ON);
    let netrc_id = find(&first, FindingKind::CredentialFile)[0].id.clone();
    assert_eq!(drift.changed, vec![netrc_id]);
    assert_eq!(drift.added.len(), 1);
    assert!(!clean(&second));

    let (_, flags) = first.recheck(&sources, ReadinessFlags::default());
    assert!(flags.readiness_changed);
}

/// #802: Corbanu's own writes to its history stores and state databases
/// (start-up settles them for some seconds) are not drift; a change to any
/// other reviewed item, or a symlink at a store, still is.
#[test]
fn pf_29_s01_corbanu_own_store_writes_are_not_drift() {
    let fixture = Fixture::new();
    let corbanu = fixture.corbanu();
    fixture.write(&corbanu.join("pfterminal_logs_2.sqlite"), b"page one");
    fixture.write(&corbanu.join("sessions/2026/10/27/rollout-a.jsonl"), b"{}");
    fixture.write(&corbanu.join("history.jsonl"), b"one\n");
    fixture.write(&corbanu.join(".env"), b"PLAIN=1\n");
    let sources = fixture.sources();
    let first = fixture.run(&sources);

    // What start-up and a running session write: a checkpoint, a new
    // database, a new store and appended history, with a rollout added.
    fixture.write(&corbanu.join("pfterminal_logs_2.sqlite"), b"page one, two");
    fixture.write(&corbanu.join("pfterminal_state_5.sqlite"), b"state");
    fixture.write(&corbanu.join("shell_snapshots/s.sh"), b"snapshot");
    fixture.write(&corbanu.join("history.jsonl"), b"one\ntwo\n");
    fixture.write(&corbanu.join("sessions/2026/rollout-b.jsonl"), b"{}");
    let (second, drift) = first.recheck(&sources, ALL_ON);
    assert!(drift.is_empty(), "{drift:?}");

    // History cleared is not drift either.
    std::fs::remove_file(corbanu.join("history.jsonl")).expect("remove history");
    let (_, drift) = second.recheck(&sources, ALL_ON);
    assert!(drift.is_empty(), "{drift:?}");

    // Another file in the Corbanu home still counts, and so does a folder
    // named like a database.
    fixture.write(&corbanu.join(".env"), b"PLAIN=2\n");
    fixture.write(&corbanu.join("odd.sqlite/inside"), b"x");
    let (third, drift) = second.recheck(&sources, ALL_ON);
    let id = |location: &str| {
        third
            .inventory
            .findings
            .iter()
            .find(|finding| finding.location.ends_with(location))
            .unwrap_or_else(|| panic!("no finding for {location}"))
            .id
            .clone()
    };
    assert_eq!(
        drift,
        Drift {
            added: vec![id("odd.sqlite")],
            changed: vec![id(".env")],
            ..Drift::default()
        }
    );
}

#[cfg(unix)]
#[test]
fn pf_29_s01_symlink_at_a_corbanu_store_is_drift() {
    let fixture = Fixture::new();
    let corbanu = fixture.corbanu();
    fixture.write(&corbanu.join("log/codex-tui.log"), b"line");
    let sources = fixture.sources();
    let first = fixture.run(&sources);

    std::fs::rename(corbanu.join("log"), fixture.home().join("elsewhere")).expect("move log");
    std::os::unix::fs::symlink(fixture.home().join("elsewhere"), corbanu.join("log"))
        .expect("symlink");
    let (second, drift) = first.recheck(&sources, ALL_ON);
    assert_eq!(drift.changed.len(), 1, "{drift:?}");

    // A symlink that appears where no store was also counts, a database
    // included, and so does a symlinked store going away.
    std::os::unix::fs::symlink(fixture.home().join("missing"), corbanu.join("sessions"))
        .expect("dangling symlink");
    std::os::unix::fs::symlink(
        fixture.home().join("elsewhere/codex-tui.log"),
        corbanu.join("x.sqlite"),
    )
    .expect("database symlink");
    let (third, drift) = second.recheck(&sources, ALL_ON);
    assert_eq!(drift.added.len(), 2, "{drift:?}");

    std::fs::remove_file(corbanu.join("log")).expect("remove log symlink");
    let (_, drift) = third.recheck(&sources, ALL_ON);
    assert_eq!(drift.removed.len(), 1, "{drift:?}");
}

/// #1428: a database's `-wal`, `-shm` and `-journal` files count like the
/// database: Corbanu's own writes to them are not drift.
#[test]
fn pf_29_s01_database_side_file_writes_are_not_drift() {
    let fixture = Fixture::new();
    let corbanu = fixture.corbanu();
    fixture.write(&corbanu.join("state_5.sqlite"), b"pages");
    fixture.write(&corbanu.join("state_5.sqlite-wal"), b"frames");
    fixture.write(&corbanu.join("state_5.sqlite-shm"), b"index");
    let sources = fixture.sources();
    let first = fixture.run(&sources);
    let entries = first
        .inventory
        .manifest
        .entries
        .iter()
        .filter(|entry| entry.path.to_string_lossy().contains("state_5"))
        .count();
    assert_eq!(entries, 3, "one manifest entry per database file");

    fixture.write(&corbanu.join("state_5.sqlite-wal"), b"more frames");
    std::fs::remove_file(corbanu.join("state_5.sqlite-shm")).expect("remove shm");
    fixture.write(&corbanu.join("state_5.sqlite-journal"), b"rollback");
    fixture.write(&corbanu.join("logs_2.sqlite-wal"), b"frames");
    let (_, drift) = first.recheck(&sources, ALL_ON);
    assert!(drift.is_empty(), "{drift:?}");
}

/// #1428: a symlink at a database's side file is drift, and its target is
/// isolated like a symlinked database's.
#[cfg(unix)]
#[test]
fn pf_29_s01_symlink_at_a_database_side_file_is_drift() {
    for suffix in ["-wal", "-shm", "-journal"] {
        let fixture = Fixture::new();
        let corbanu = fixture.corbanu();
        let secret = fixture.home().join("secret.txt");
        fixture.write(&secret, b"TOKEN=1");
        fixture.write(&corbanu.join("state_5.sqlite"), b"pages");
        let sources = fixture.sources();
        let first = fixture.run(&sources);

        let side = corbanu.join(format!("state_5.sqlite{suffix}"));
        std::os::unix::fs::symlink(&secret, &side).expect("symlink");
        let (second, drift) = first.recheck(&sources, ALL_ON);
        assert_eq!(drift.changed.len(), 1, "{suffix}: {drift:?}");
        let real = std::fs::canonicalize(&secret).expect("canonical secret");
        assert!(
            second.inventory.isolation_paths().contains(&real),
            "{suffix}: target not isolated"
        );

        // A dangling one counts too, and so does its removal.
        std::fs::remove_file(&side).expect("remove symlink");
        std::os::unix::fs::symlink(fixture.home().join("missing"), &side).expect("dangling");
        let (third, drift) = second.recheck(&sources, ALL_ON);
        assert_eq!(drift.changed.len(), 1, "{suffix}: {drift:?}");
        std::fs::remove_file(&side).expect("remove dangling symlink");
        let (_, drift) = third.recheck(&sources, ALL_ON);
        assert_eq!(drift.changed.len(), 1, "{suffix}: {drift:?}");
    }
}

/// #1428: a database file that is not a regular file with one link (a
/// pipe, or a hard link to a file elsewhere) is drift, the database itself
/// included.
#[cfg(unix)]
#[test]
fn pf_29_s01_irregular_database_files_are_drift() {
    let fixture = Fixture::new();
    let corbanu = fixture.corbanu();
    let secret = fixture.home().join("secret.txt");
    fixture.write(&secret, b"TOKEN=1");
    fixture.write(&corbanu.join("state_5.sqlite"), b"pages");
    let sources = fixture.sources();
    let first = fixture.run(&sources);

    std::fs::hard_link(&secret, corbanu.join("state_5.sqlite-shm")).expect("hard link");
    let (second, drift) = first.recheck(&sources, ALL_ON);
    assert_eq!(drift.changed.len(), 1, "{drift:?}");

    std::fs::remove_file(corbanu.join("state_5.sqlite-shm")).expect("remove hard link");
    let (third, drift) = second.recheck(&sources, ALL_ON);
    assert_eq!(drift.changed.len(), 1, "{drift:?}");

    let fifo = |path: &Path| {
        let path = std::ffi::CString::new(path.as_os_str().as_encoded_bytes()).expect("path");
        // SAFETY: `path` is a valid NUL-terminated string.
        assert_eq!(unsafe { libc::mkfifo(path.as_ptr(), 0o600) }, 0, "mkfifo");
    };
    std::fs::remove_file(corbanu.join("state_5.sqlite")).expect("remove database");
    fifo(&corbanu.join("state_5.sqlite"));
    let (_, drift) = third.recheck(&sources, ALL_ON);
    assert_eq!(drift.changed.len(), 1, "{drift:?}");
}

/// #1441: a hard-linked database file is drift on every platform (Windows
/// reads the link count through the file's handle); a plain one written by
/// Corbanu is not.
#[test]
fn pf_29_s01_hard_linked_database_files_are_drift_everywhere() {
    let fixture = Fixture::new();
    let corbanu = fixture.corbanu();
    let secret = fixture.home().join("secret.txt");
    fixture.write(&secret, b"TOKEN=1");
    fixture.write(&corbanu.join("state_5.sqlite"), b"pages");
    let sources = fixture.sources();
    let first = fixture.run(&sources);

    fixture.write(&corbanu.join("state_5.sqlite-wal"), b"frames");
    let (second, drift) = first.recheck(&sources, ALL_ON);
    assert!(drift.is_empty(), "{drift:?}");

    let shm = corbanu.join("state_5.sqlite-shm");
    std::fs::hard_link(&secret, &shm).expect("hard link");
    let (third, drift) = second.recheck(&sources, ALL_ON);
    assert_eq!(drift.changed.len(), 1, "{drift:?}");
    let links = |path: &Path| {
        third
            .inventory
            .manifest
            .entries
            .iter()
            .find(|entry| entry.path == path)
            .unwrap_or_else(|| panic!("{} missing from manifest", path.display()))
            .links
    };
    assert_eq!(links(&shm), Some(2));
    assert_eq!(links(&corbanu.join("state_5.sqlite")), Some(1));
}

/// #1441: where a file created through a dangling symlink would land is
/// isolated (at the end of a chain, relative targets resolved against the
/// link's folder), and pointing the link elsewhere is drift.
#[cfg(unix)]
#[test]
fn pf_29_s01_dangling_symlink_targets_are_isolated() {
    let fixture = Fixture::new();
    let home = fixture.home();
    let corbanu = fixture.corbanu();
    let elsewhere = fixture.root.path().join("elsewhere");
    std::fs::create_dir_all(&elsewhere).expect("mkdir");
    let real = |path: &Path| std::fs::canonicalize(path).expect("canonical");
    std::os::unix::fs::symlink(home.join("missing"), home.join(".git-credentials"))
        .expect("absolute");
    std::os::unix::fs::symlink("../elsewhere/netrc", home.join(".netrc")).expect("relative");
    std::os::unix::fs::symlink(home.join("hop"), home.join(".pgpass")).expect("chain");
    std::os::unix::fs::symlink(elsewhere.join("pgpass"), home.join("hop")).expect("hop");
    fixture.write(&corbanu.join("state_5.sqlite"), b"pages");
    let wal = corbanu.join("state_5.sqlite-wal");
    std::os::unix::fs::symlink(elsewhere.join("wal"), &wal).expect("side file");
    let sources = fixture.sources();
    let first = fixture.run(&sources);

    let isolated = first.inventory.isolation_paths();
    for target in [
        real(&home).join("missing"),
        real(&elsewhere).join("netrc"),
        real(&elsewhere).join("pgpass"),
        real(&elsewhere).join("wal"),
    ] {
        assert!(
            isolated.contains(&target),
            "{} missing from {isolated:?}",
            target.display()
        );
    }

    std::fs::remove_file(&wal).expect("remove");
    std::os::unix::fs::symlink(elsewhere.join("other"), &wal).expect("retarget");
    let (_, drift) = first.recheck(&sources, ALL_ON);
    assert_eq!(drift.changed.len(), 1, "{drift:?}");
}

#[test]
fn pf_29_s01_ids_are_stable_and_digests_are_keyed() {
    let fixture = Fixture::new();
    fixture.write(&fixture.home().join(".netrc"), b"same");
    let sources = fixture.sources();
    let one = Preflight::run_with_key(&sources, ALL_ON, [1; 32]);
    let two = Preflight::run_with_key(&sources, ALL_ON, [2; 32]);
    assert_eq!(one.inventory.finding_ids(), two.inventory.finding_ids());
    assert_ne!(
        one.inventory.manifest.digest(),
        two.inventory.manifest.digest()
    );
}

#[test]
fn pf_29_s01_workspace_env_files_found_without_recursion() {
    let fixture = Fixture::new();
    let work = fixture.root.path().join("work");
    fixture.write(&work.join(".env"), b"API_KEY=fake");
    fixture.write(&work.join("nested/.env"), b"API_KEY=fake");
    fixture.write(&fixture.home().join("projects/app/.env"), b"API_KEY=fake");
    let preflight = fixture.run(&fixture.sources());
    let env_files = find(&preflight, FindingKind::EnvFile);
    assert_eq!(env_files.len(), 1);
    assert_eq!(env_files[0].paths, vec![work.join(".env")]);
}

/// A project can ship `.env` as a link to a device or as a named pipe; the
/// preflight must neither read forever nor block.
#[cfg(unix)]
#[test]
fn pf_29_s01_devices_and_pipes_neither_hang_nor_exhaust_memory() {
    let fixture = Fixture::new();
    let work = fixture.root.path().join("work");
    std::os::unix::fs::symlink("/dev/zero", work.join(".env"))
        .unwrap_or_else(|err| panic!("{err}"));
    let fifo = work.join(".envrc");
    let status = std::process::Command::new("mkfifo")
        .arg(&fifo)
        .status()
        .unwrap_or_else(|err| panic!("mkfifo: {err}"));
    assert!(status.success());
    std::os::unix::fs::symlink(&fifo, fixture.home().join(".zshrc"))
        .unwrap_or_else(|err| panic!("{err}"));
    let mut big = vec![b'a'; 2 * 1024 * 1024];
    big.extend_from_slice(b"\nexport API_KEY=fake\n");
    fixture.write(&fixture.home().join(".bashrc"), &big);

    let started = std::time::Instant::now();
    let preflight = fixture.run(&fixture.sources());
    assert!(started.elapsed() < std::time::Duration::from_secs(10));
    let status = |path: PathBuf| {
        preflight
            .inventory
            .manifest
            .entries
            .iter()
            .find(|entry| entry.path == path)
            .map(|entry| entry.status.clone())
    };
    for name in [".env", ".envrc"] {
        assert!(
            matches!(status(work.join(name)), Some(EntryStatus::Unreadable(_))),
            "{name}"
        );
    }
    // Too large to read: listed for the human, not parsed.
    assert!(
        find(&preflight, FindingKind::ShellProfileExport)
            .iter()
            .any(|finding| finding.location == "~/.bashrc (not readable)")
    );
}

/// #438 (AGG-25): `-c cli_auth_credentials_store="file"` chooses where sign-in
/// is stored; it is not a secret, so it neither blocks Aggressive nor makes
/// its config file a "credential file".
#[test]
fn pf_29_s01_credential_store_modes_are_not_secrets() {
    let fixture = Fixture::new();
    let system_path = fixture.root.path().join("etc/codex/config.toml");
    let flags = layer(
        "launch flags",
        /*path*/ None,
        r#"cli_auth_credentials_store = "file""#,
    );
    let system = layer(
        "system config",
        Some(system_path),
        r#"
cli_auth_credentials_store = "keyring"
mcp_oauth_credentials_store = "auto"
model_auto_compact_token_limit_scope = "body_after_prefix"
"#,
    );
    let mut sources = fixture.sources();
    sources.config_layers = vec![flags, system];
    let preflight = fixture.run(&sources);
    assert_eq!(
        find(&preflight, FindingKind::ConfigLiteral),
        Vec::<&Finding>::new()
    );
    assert_eq!(preflight.inventory.isolation_paths(), Vec::<PathBuf>::new());
    assert!(clean(&preflight), "{:?}", preflight.blockers());

    // Only the top-level settings, and only their choices: the same name
    // under another table, or a value that is not a choice, still blocks.
    for flag in [
        r#"shell_environment_policy.set.CLI_AUTH_CREDENTIALS_STORE = "hunter2-plain-password""#,
        r#"shell_environment_policy.set.cli_auth_credentials_store = "hunter2-plain-password""#,
        r#"cli_auth_credentials_store = "hunter2-plain-password""#,
        &format!(r#"shell_environment_policy.set.DEPLOY_TOKEN = "{FAKE_CORE_VALUE}""#),
    ] {
        let mut sources = fixture.sources();
        sources.config_layers = vec![layer("launch flags", /*path*/ None, flag)];
        assert!(!clean(&fixture.run(&sources)), "{flag}");
    }
}
