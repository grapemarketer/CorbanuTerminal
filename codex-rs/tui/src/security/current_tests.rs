use std::collections::HashMap;

use codex_config::LoaderOverrides;
use codex_config::types::ShellEnvironmentPolicyToml;
use codex_protocol::config_types::EnvironmentVariablePattern;
use pretty_assertions::assert_eq;

use super::*;
use crate::legacy_core::config::ConfigBuilder;
use crate::legacy_core::config::ConfigOverrides;
use crate::legacy_core::security_level_change::SecurityLevel;
use crate::security::aggressive;

async fn build(home: &Path, cwd: &Path, user_config: &str, aggressive_level: bool) -> Config {
    std::fs::write(home.join("config.toml"), user_config).unwrap();
    let mut cli = Vec::new();
    let mut harness = ConfigOverrides {
        cwd: Some(cwd.to_path_buf()),
        ..Default::default()
    };
    if aggressive_level {
        cli = aggressive::base_overrides(home, home);
        cli.extend(aggressive::env_overrides(
            &ShellEnvironmentPolicyToml::default(),
        ));
        aggressive::apply_launch_overrides(&mut harness);
    }
    ConfigBuilder::default()
        .codex_home(home.to_path_buf())
        .cli_overrides(cli)
        .harness_overrides(harness)
        .loader_overrides(LoaderOverrides::without_managed_config_for_tests())
        .build()
        .await
        .unwrap()
}

/// #691: on Windows workspace-write needs the Windows sandbox; without it the
/// session falls back to read-only (Core's `config/permissions.rs`). These
/// configs then select the default (elevated) Windows sandbox, as on a machine
/// where it is set up. Unchanged elsewhere.
fn with_windows_sandbox(user_config: &str) -> String {
    if cfg!(windows) {
        format!("{user_config}\n[windows]\nsandbox = \"elevated\"\n")
    } else {
        user_config.to_string()
    }
}

/// A legacy workspace-write sandbox without temp folders, with an extra
/// writable root and the given network setting.
fn workspace_write(extra: &Path, network: bool) -> String {
    format!(
        r#"approval_policy = "on-request"
sandbox_mode = "workspace-write"
web_search = "live"

[sandbox_workspace_write]
writable_roots = [{extra:?}]
network_access = {network}
exclude_tmpdir_env_var = true
exclude_slash_tmp = true
"#,
        extra = extra.display().to_string()
    )
}

/// Code-blind case B-06: the review shows what the user would lose. The
/// legacy sandbox has no denied reads, so approved commands can leave it,
/// and its "off" network and vault rows say so.
#[tokio::test]
async fn workspace_write_rows_name_the_escape_route() {
    let home = tempfile::tempdir().unwrap();
    let cwd = tempfile::tempdir().unwrap();
    let extra = tempfile::tempdir().unwrap();
    // Writable roots are shown with top-level aliases resolved (#467: `/tmp`
    // on macOS).
    let extra_path = codex_utils_absolute_path::canonicalize_preserving_symlinks(extra.path())
        .unwrap()
        .display()
        .to_string();

    let open = build(
        home.path(),
        cwd.path(),
        &with_windows_sandbox(&workspace_write(extra.path(), /*network*/ true)),
        /*aggressive_level*/ false,
    )
    .await;
    let [sandbox, approvals, network, vault, _, _] =
        current_values(&open, SecurityLevel::Permissive);
    assert_eq!(
        [sandbox, approvals, network, vault],
        [
            format!(
                "commands can write to the current folder, {extra_path}; approved or allow-listed commands can run outside the sandbox; permission-request tools are off"
            ),
            "on-request (reviewer: you)".to_string(),
            "on for agent commands; web search live".to_string(),
            "the vault store and sign-in file are readable to agent commands; secret-like environment variables are passed through (KEY, SECRET, TOKEN, VAULT, PASSWORD, PASSPHRASE, CREDENTIAL); login profiles or shell snapshots are used".to_string(),
        ]
    );

    let closed = build(
        home.path(),
        cwd.path(),
        &with_windows_sandbox(&workspace_write(extra.path(), /*network*/ false)),
        /*aggressive_level*/ false,
    )
    .await;
    let [_, _, network, _, _, _] = current_values(&closed, SecurityLevel::Permissive);
    assert_eq!(
        network,
        "off inside the sandbox, on for commands that run outside it; web search live"
    );
}

/// Under Aggressive every row matches the Aggressive value.
#[tokio::test]
async fn aggressive_rows_claim_only_what_is_enforced() {
    let home = tempfile::tempdir().unwrap();
    let cwd = tempfile::tempdir().unwrap();
    let extra = tempfile::tempdir().unwrap();
    let config = build(
        home.path(),
        cwd.path(),
        &workspace_write(extra.path(), /*network*/ true),
        /*aggressive_level*/ true,
    )
    .await;
    // #391: the stored level the launch path reads turns the model key
    // broker on; these overrides alone do not (see the next test).
    let [sandbox, approvals, network, vault, _, children] =
        current_values(&config, SecurityLevel::Permissive);
    assert_eq!(
        [sandbox, approvals, network, vault, children],
        [
            format!(
                "profile {}: commands can write to the current folder; permission-request tools are off",
                aggressive::PROFILE_ID
            ),
            "untrusted (reviewer: you)".to_string(),
            "off for agent commands; web search disabled".to_string(),
            "the vault store and sign-in file are unreadable to agent commands; secret-like environment variables are removed; login profiles and shell snapshots are not used".to_string(),
            "spawned agents get this session's values".to_string(),
        ]
    );
}

/// #391: the model keys row reports whether the broker holds them.
#[tokio::test]
async fn model_keys_row_reports_the_broker_setting() {
    let home = tempfile::tempdir().unwrap();
    let cwd = tempfile::tempdir().unwrap();
    let mut config = build(home.path(), cwd.path(), "", /*aggressive_level*/ false).await;
    assert_eq!(
        current_values(&config, SecurityLevel::Permissive)[4],
        "Corbanu reads provider keys itself (broker_model_auth off)"
    );
    // Enabled after loading, so this test process is not marked brokered.
    config
        .features
        .enable(Feature::BrokerModelAuth)
        .expect("enable the broker");
    assert_eq!(
        current_values(&config, SecurityLevel::Permissive)[4],
        "held by the isolated credential broker (broker_model_auth on)"
    );
}

/// #440 (milestone AGG-15): with the secretless launch on, ordinary
/// variables outside its allowlist (`PLAIN_VAR`) are removed too, so the
/// review says the environment is allowlisted, not only that secret-like
/// variables are removed.
#[tokio::test]
async fn issue_440_vault_row_says_the_environment_is_allowlisted_under_secretless_launch() {
    let home = tempfile::tempdir().unwrap();
    let cwd = tempfile::tempdir().unwrap();
    let extra = tempfile::tempdir().unwrap();
    let mut config = build(
        home.path(),
        cwd.path(),
        &workspace_write(extra.path(), /*network*/ true),
        /*aggressive_level*/ true,
    )
    .await;
    // Enabled after loading, so this test process is not armed.
    config
        .features
        .enable(Feature::SecretlessAgentLaunch)
        .expect("enable the secretless launch");
    let vault = &current_values(&config, SecurityLevel::Permissive)[3];
    assert!(
        vault.contains("only allowlisted environment variables reach agent commands"),
        "{vault}"
    );
    // What the launch does: an ordinary name outside the allowlist goes.
    let mut env = std::collections::HashMap::from([
        ("PLAIN_VAR".to_string(), "visible".to_string()),
        ("PATH".to_string(), "/bin".to_string()),
    ]);
    codex_protocol::secretless_launch::retain_launch_env(&mut env, |_| false);
    assert_eq!(env.keys().collect::<Vec<_>>(), vec!["PATH"]);
    let vault_row = aggressive::ROWS
        .iter()
        .find(|(label, _)| *label == "Vault")
        .expect("vault row");
    assert!(
        vault_row
            .1
            .contains("so is every other variable outside an allowlist")
    );
}

/// #739 (option B): with protected launch on (secretless_agent_launch),
/// whose launch contract refuses every run outside the sandbox and every
/// read of the vault store and sign-in file, the Permissive rows no longer
/// claim that approved or allow-listed commands can run outside it, nor what
/// would follow from that.
#[tokio::test]
async fn issue_739_rows_under_protected_launch_claim_no_run_outside_the_sandbox() {
    let home = tempfile::tempdir().unwrap();
    let cwd = tempfile::tempdir().unwrap();
    let extra = tempfile::tempdir().unwrap();
    let mut config = build(
        home.path(),
        cwd.path(),
        &with_windows_sandbox(&workspace_write(extra.path(), /*network*/ false)),
        /*aggressive_level*/ false,
    )
    .await;
    let [sandbox, _, network, vault, _, _] = current_values(&config, SecurityLevel::Permissive);
    assert!(
        sandbox.contains("approved or allow-listed commands can run outside the sandbox"),
        "{sandbox}"
    );
    assert!(network.starts_with("off inside the sandbox, on for commands that run outside it"));
    assert!(vault.starts_with("the vault store and sign-in file are readable"));
    // Enabled after loading, so this test process is not armed.
    config
        .features
        .enable(Feature::SecretlessAgentLaunch)
        .expect("enable the secretless launch");
    let [sandbox, _, network, vault, _, _] = current_values(&config, SecurityLevel::Permissive);
    assert!(
        !sandbox.contains("can run outside the sandbox"),
        "{sandbox}"
    );
    assert!(
        sandbox.contains(
            "with protected launch (secretless_agent_launch) no command runs outside the sandbox, so none is offered"
        ),
        "{sandbox}"
    );
    assert_eq!(network, "off for agent commands; web search live");
    assert!(
        vault.starts_with(
            "the vault store and sign-in file are unreadable (secretless_agent_launch, also when you approve a command) to agent commands"
        ),
        "{vault}"
    );
}

/// Names the user lists explicitly are reported, not just the fixed probes.
#[test]
fn secret_kinds_include_named_variables_from_the_environment() {
    let env = || {
        [
            ("PATH", "/bin"),
            ("OPENAI_API_KEY", "fake"),
            ("STRIPE_SECRET", "fake"),
        ]
        .map(|(name, value)| (name.to_string(), value.to_string()))
    };
    let include_only = ShellEnvironmentPolicy {
        include_only: vec![EnvironmentVariablePattern::new_case_insensitive(
            "OPENAI_API_KEY",
        )],
        ..Default::default()
    };
    // Aggressive's excludes: the built-in KEY/SECRET/TOKEN ones plus these.
    let exclude = ["*VAULT*", "*PASSWORD*", "*PASSPHRASE*", "*CREDENTIAL*"]
        .map(EnvironmentVariablePattern::new_case_insensitive)
        .to_vec();
    let removed = ShellEnvironmentPolicy {
        ignore_default_excludes: false,
        exclude: exclude.clone(),
        ..Default::default()
    };
    let set = ShellEnvironmentPolicy {
        ignore_default_excludes: false,
        exclude,
        r#set: HashMap::from([("DB_PASSWORD".to_string(), "fake".to_string())]),
        ..Default::default()
    };
    assert_eq!(
        [
            kept_secret_kinds(&include_only, env()),
            kept_secret_kinds(&removed, env()),
            kept_secret_kinds(&set, env()),
        ],
        [vec!["KEY"], Vec::new(), vec!["PASSWORD"]]
    );
}

/// A role counts as changing its children's values unless it sets only
/// model and instruction keys.
#[test]
fn roles_that_set_permission_keys_change_children() {
    let dir = tempfile::tempdir().unwrap();
    let write = |name: &str, contents: &str| {
        let path = dir.path().join(name);
        std::fs::write(&path, contents).unwrap();
        path
    };
    assert_eq!(
        [
            role_changes_values(&write(
                "model.toml",
                "model = \"m\"\ndeveloper_instructions = \"x\"\n"
            )),
            role_changes_values(&write(
                "network.toml",
                "[sandbox_workspace_write]\nnetwork_access = true\n"
            )),
            role_changes_values(&write("broken.toml", "model = \n")),
            role_changes_values(&dir.path().join("missing.toml")),
        ],
        [false, true, true, true]
    );
}

/// Denied reads keep commands inside the sandbox, but an approved permission
/// request can still add network or reads, so those rows say so.
#[tokio::test]
async fn permission_requests_qualify_network_and_vault_rows() {
    let home = tempfile::tempdir().unwrap();
    let cwd = tempfile::tempdir().unwrap();
    let secrets = home.path().join("secrets").display().to_string();
    let auth = home.path().join("auth.json").display().to_string();
    let user_config = |request_tools: bool| {
        format!(
            r#"approval_policy = "on-request"
default_permissions = "locked"

[permissions.locked]
extends = ":workspace"

[permissions.locked.filesystem]
":tmpdir" = "read"
":slash_tmp" = "read"
{secrets:?} = "deny"
{auth:?} = "deny"

[permissions.locked.network]
enabled = false

[features]
request_permissions_tool = {request_tools}
"#
        )
    };
    let [_, _, network, vault, _, _] = current_values(
        &build(
            home.path(),
            cwd.path(),
            &user_config(true),
            /*aggressive_level*/ false,
        )
        .await,
        SecurityLevel::Permissive,
    );
    assert_eq!(
        [network, vault.split("; ").next().unwrap().to_string()],
        [
            "off unless an approved permission request turns it on; web search cached".to_string(),
            "the vault store and sign-in file are unreadable (unless an approved permission request grants access) to agent commands".to_string(),
        ]
    );
    let [sandbox, _, network, _, _, _] = current_values(
        &build(
            home.path(),
            cwd.path(),
            &user_config(false),
            /*aggressive_level*/ false,
        )
        .await,
        SecurityLevel::Permissive,
    );
    assert_eq!(
        [sandbox, network],
        [
            "profile locked: commands can write to the current folder; permission-request tools are off".to_string(),
            "off for agent commands; web search cached".to_string(),
        ]
    );
}

/// #437 (AGG-24/34, AGG-20 child pane): after a session's first turn the app
/// server sends its thread settings, which carry only the legacy sandbox
/// projection. Rebuilding the profile from it dropped the Aggressive denied
/// reads, so the rows claimed approved commands leave the sandbox and the
/// vault is readable. The known profile is kept while nothing changed.
#[tokio::test]
async fn sec_437_thread_settings_keep_the_aggressive_rows() {
    use crate::legacy_core::config::PermissionProfileSnapshot;

    let home = tempfile::tempdir().unwrap();
    let cwd = tempfile::tempdir().unwrap();
    let extra = tempfile::tempdir().unwrap();
    let mut config = build(
        home.path(),
        cwd.path(),
        &workspace_write(extra.path(), /*network*/ true),
        /*aggressive_level*/ true,
    )
    .await;
    let before = current_values(&config, SecurityLevel::Permissive);
    let profile = config.permissions.permission_profile().clone();
    let active = config.permissions.active_permission_profile();
    let cwd_abs = config.cwd.clone();
    let settings = codex_app_server_protocol::ThreadSettings {
        cwd: cwd_abs.clone(),
        approval_policy: codex_app_server_protocol::AskForApproval::UnlessTrusted,
        approvals_reviewer: codex_app_server_protocol::ApprovalsReviewer::User,
        sandbox_policy: codex_sandboxing::compatibility_sandbox_policy_for_permission_profile(
            &config.permissions.effective_permission_profile(),
            cwd_abs.as_path(),
        )
        .into(),
        active_permission_profile: active.clone().map(Into::into),
        model: "glm-5.3-flash".to_string(),
        model_provider: "zai".to_string(),
        service_tier: None,
        effort: None,
        summary: None,
        collaboration_mode: codex_protocol::config_types::CollaborationMode {
            mode: codex_protocol::config_types::ModeKind::Default,
            settings: codex_protocol::config_types::Settings {
                model: "glm-5.3-flash".to_string(),
                reasoning_effort: None,
                developer_instructions: None,
            },
        },
        multi_agent_mode: Default::default(),
        personality: None,
    };

    // The milestone's rows: rebuilt from the projection alone.
    let lossy = codex_protocol::models::PermissionProfile::from_legacy_sandbox_policy_for_cwd(
        &settings.sandbox_policy.to_core(),
        cwd_abs.as_path(),
    );
    let mut weakened = config.clone();
    weakened
        .permissions
        .replace_permission_profile_from_session_snapshot(
            PermissionProfileSnapshot::from_session_snapshot(lossy, active.clone()),
        )
        .unwrap();
    let rows = current_values(&weakened, SecurityLevel::Permissive);
    assert!(
        rows[0].contains("approved or allow-listed commands can run outside the sandbox"),
        "{rows:?}"
    );

    let kept = crate::app::thread_settings::permission_profile_for_thread_settings(
        &profile,
        Some(&config.permissions.effective_permission_profile()),
        active.as_ref(),
        &settings,
    );
    assert_eq!(kept, profile);
    config
        .permissions
        .replace_permission_profile_from_session_snapshot(
            PermissionProfileSnapshot::from_session_snapshot(kept, active.clone()),
        )
        .unwrap();
    assert_eq!(current_values(&config, SecurityLevel::Permissive), before);

    // A real change (a different sandbox) is still applied.
    let mut changed = settings;
    changed.sandbox_policy = codex_app_server_protocol::SandboxPolicy::DangerFullAccess;
    assert_eq!(
        crate::app::thread_settings::permission_profile_for_thread_settings(
            &profile,
            /*materialized*/ None,
            active.as_ref(),
            &changed,
        ),
        codex_protocol::models::PermissionProfile::Disabled
    );
}

/// #747/#749: under Moderate no command leaves the sandbox, so the rows
/// never say approved commands can run outside it, and with the
/// untrusted-content labels the vault store and sign-in file are unreadable.
#[tokio::test]
async fn issue_749_moderate_rows_keep_commands_in_the_sandbox() {
    let home = tempfile::tempdir().unwrap();
    let cwd = tempfile::tempdir().unwrap();
    let extra = tempfile::tempdir().unwrap();
    let config = build(
        home.path(),
        cwd.path(),
        &with_windows_sandbox(&format!(
            "{}\n[features]\nsource_envelopes = true\n",
            workspace_write(extra.path(), /*network*/ false)
        )),
        /*aggressive_level*/ false,
    )
    .await;
    let [
        permissive_sandbox,
        _,
        permissive_network,
        permissive_vault,
        _,
        _,
    ] = current_values(&config, SecurityLevel::Permissive);
    assert!(permissive_sandbox.contains("can run outside the sandbox"));
    let [sandbox, _, network, vault, _, _] = current_values(&config, SecurityLevel::Moderate);
    assert!(!sandbox.contains("outside the sandbox"), "{sandbox}");
    assert!(
        sandbox
            .contains("under Moderate approved and allow-listed commands stay inside the sandbox"),
        "{sandbox}"
    );
    assert_eq!(network, "off for agent commands; web search live");
    assert_ne!(network, permissive_network);
    assert!(
        vault.starts_with("the vault store and sign-in file are unreadable (Moderate; your fresh approval of one command lifts this for that command) to agent commands"),
        "{vault}"
    );
    assert!(permissive_vault.contains("readable to agent commands"));

    // An external sandbox takes none of Core's rules: no Moderate claim.
    let mut external = config;
    external
        .permissions
        .set_permission_profile(codex_protocol::models::PermissionProfile::External {
            network: codex_protocol::permissions::NetworkSandboxPolicy::Restricted,
        })
        .unwrap();
    let vault = &current_values(&external, SecurityLevel::Moderate)[3];
    assert!(
        vault.starts_with("the vault store and sign-in file are readable to agent commands"),
        "{vault}"
    );
}

/// #792 (AGG-04): before the restart that follows confirming Aggressive in
/// `/security`, the session keeps its own sandbox (no denied reads of its
/// own) while Core's Aggressive is in force. With the untrusted-content
/// labels Core's rules deny the vault store and sign-in file to every agent
/// command and keep every run in the sandbox (#747's Aggressive cases), so
/// the rows no longer say the store is readable or that approved commands
/// leave the sandbox; `corbanu vault …` commands are refused (#443).
#[tokio::test]
async fn issue_792_aggressive_before_restart_rows_say_what_core_enforces() {
    let home = tempfile::tempdir().unwrap();
    let cwd = tempfile::tempdir().unwrap();
    let permissive = |labels: bool| {
        format!(
            r#"approval_policy = "on-request"
sandbox_mode = "workspace-write"
web_search = "cached"

[sandbox_workspace_write]
network_access = false
exclude_tmpdir_env_var = true
exclude_slash_tmp = true

[features]
source_envelopes = {labels}
"#
        )
    };
    let rows = |values: CurrentValues| {
        let [sandbox, _, network, vault, _, _] = values;
        let vault = vault.split("; ").next().unwrap().to_string();
        format!("Sandbox: {sandbox}\nNetwork: {network}\nVault: {vault}")
    };
    let config = build(
        home.path(),
        cwd.path(),
        &with_windows_sandbox(&permissive(true)),
        /*aggressive_level*/ false,
    )
    .await;
    let before_restart = rows(current_values(&config, SecurityLevel::Aggressive));
    insta::assert_snapshot!("issue_792_aggressive_before_restart_rows", before_restart);
    assert!(!before_restart.contains("are readable"), "{before_restart}");
    assert!(!before_restart.contains("run outside"), "{before_restart}");
    // Permissive in force: unchanged.
    let permissive_rows = rows(current_values(&config, SecurityLevel::Permissive));
    assert!(
        permissive_rows.contains("the vault store and sign-in file are readable to agent commands"),
        "{permissive_rows}"
    );
    assert!(
        permissive_rows.contains("approved or allow-listed commands can run outside the sandbox"),
        "{permissive_rows}"
    );
    assert!(
        !permissive_rows.contains("corbanu vault"),
        "{permissive_rows}"
    );

    // Without the labels Core's rules do not apply: only the vault
    // commands are refused.
    let unlabelled = build(
        home.path(),
        cwd.path(),
        &with_windows_sandbox(&permissive(false)),
        /*aggressive_level*/ false,
    )
    .await;
    let unlabelled = rows(current_values(&unlabelled, SecurityLevel::Aggressive));
    assert!(
        unlabelled.contains(
            "Vault: the vault store and sign-in file are readable to agent commands, and direct `corbanu vault …` commands are refused"
        ),
        "{unlabelled}"
    );
    assert!(
        unlabelled.contains("approved or allow-listed commands can run outside the sandbox"),
        "{unlabelled}"
    );

    // After the restart the session's own Aggressive profile denies them
    // too; the row names the level and the refused vault commands.
    let extra = tempfile::tempdir().unwrap();
    let restarted = build(
        home.path(),
        cwd.path(),
        &with_windows_sandbox(&format!(
            "{}\n[features]\nsource_envelopes = true\n",
            workspace_write(extra.path(), /*network*/ false)
        )),
        /*aggressive_level*/ true,
    )
    .await;
    let [sandbox, _, _, vault, _, _] = current_values(&restarted, SecurityLevel::Aggressive);
    assert!(!sandbox.contains("outside the sandbox"), "{sandbox}");
    assert!(
        vault.starts_with(
            "the vault store and sign-in file are unreadable (Aggressive, also when you approve a command) to agent commands, and direct `corbanu vault …` commands are refused; "
        ),
        "{vault}"
    );

    // An external sandbox takes none of Core's rules.
    let mut external = config;
    external
        .permissions
        .set_permission_profile(codex_protocol::models::PermissionProfile::External {
            network: codex_protocol::permissions::NetworkSandboxPolicy::Restricted,
        })
        .unwrap();
    let vault = &current_values(&external, SecurityLevel::Aggressive)[3];
    assert!(
        vault.starts_with("the vault store and sign-in file are readable to agent commands"),
        "{vault}"
    );
}

/// #816: what screen 07, the review and the level list say holds from
/// confirming Aggressive until the restart ([`before_restart`], read when
/// `/security` opens) is what the inspector's rows say once Core is
/// Aggressive and confirmation has set approval for every command (#505),
/// for each kind of session.
#[tokio::test]
async fn issue_816_before_restart_facts_match_the_inspector_rows() {
    let home = tempfile::tempdir().unwrap();
    let cwd = tempfile::tempdir().unwrap();
    let session = |approvals: &str, sandbox: &str, network: bool, web: &str, rest: &str| {
        format!(
            r#"approval_policy = "{approvals}"
sandbox_mode = "{sandbox}"
web_search = "{web}"

[sandbox_workspace_write]
network_access = {network}
exclude_tmpdir_env_var = true
exclude_slash_tmp = true

{rest}
"#
        )
    };
    let labels = "[features]\nsource_envelopes = true";
    let cases = [
        // The AGG-04 run and the host check.
        (
            "labels",
            session("on-request", "workspace-write", false, "cached", labels),
        ),
        // Without the labels approved commands can leave the sandbox.
        (
            "no labels",
            session("on-request", "workspace-write", false, "cached", ""),
        ),
        (
            "network on",
            session("on-request", "workspace-write", true, "cached", labels),
        ),
        (
            "permission requests",
            session(
                "on-request",
                "workspace-write",
                false,
                "cached",
                "[features]\nsource_envelopes = true\nrequest_permissions_tool = true",
            ),
        ),
        // No approvals before; confirmation sets them, so the tools count.
        (
            "never with permission requests",
            session(
                "never",
                "workspace-write",
                false,
                "cached",
                "[features]\nsource_envelopes = true\nrequest_permissions_tool = true",
            ),
        ),
        (
            "own settings",
            session(
                "on-request",
                "workspace-write",
                false,
                "disabled",
                "[features]\nsource_envelopes = true\nsecretless_agent_launch = true\nbroker_model_auth = true\nshell_snapshot = false",
            ),
        ),
        // Names set explicitly reach commands under protected launch too.
        (
            "protected launch with a secret set",
            session(
                "on-request",
                "workspace-write",
                false,
                "disabled",
                "[shell_environment_policy.set]\nGH_TOKEN = \"synthetic-not-a-token\"\n\n[features]\nsource_envelopes = true\nsecretless_agent_launch = true\nshell_snapshot = false",
            ),
        ),
        (
            "named profile",
            format!(
                "approval_policy = \"on-request\"\ndefault_permissions = \"locked\"\n\n[permissions.locked]\nextends = \":workspace\"\n\n[permissions.locked.filesystem]\n\":tmpdir\" = \"read\"\n\":slash_tmp\" = \"read\"\n\n[permissions.locked.network]\nenabled = false\n\n{labels}\n"
            ),
        ),
        (
            "no sandbox",
            session("on-request", "danger-full-access", false, "cached", labels),
        ),
        ("external sandbox", String::new()),
    ];
    let mut seen = Vec::new();
    for (name, user_config) in cases {
        let mut config = if name == "external sandbox" {
            let mut config = build(
                home.path(),
                cwd.path(),
                &with_windows_sandbox(&session(
                    "on-request",
                    "workspace-write",
                    false,
                    "cached",
                    labels,
                )),
                /*aggressive_level*/ false,
            )
            .await;
            config
                .permissions
                .set_permission_profile(codex_protocol::models::PermissionProfile::External {
                    network: codex_protocol::permissions::NetworkSandboxPolicy::Restricted,
                })
                .unwrap();
            config
        } else {
            build(
                home.path(),
                cwd.path(),
                &with_windows_sandbox(&user_config),
                /*aggressive_level*/ false,
            )
            .await
        };
        let facts = before_restart(&config);
        // What confirmation does to approvals (#505).
        config
            .permissions
            .approval_policy
            .set(AskForApproval::UnlessTrusted)
            .unwrap();
        let [sandbox, _, network, vault, model_keys, _] =
            current_values(&config, SecurityLevel::Aggressive);
        let kept = !sandbox.contains("no sandbox")
            && !sandbox.starts_with("an external sandbox")
            && !sandbox.contains("can run outside the sandbox");
        assert_eq!(facts.sandbox_kept.is_some(), kept, "{name}: {sandbox}");
        if let Some(writable) = &facts.sandbox_kept {
            assert!(sandbox.starts_with(writable.as_str()), "{name}: {sandbox}");
        }
        assert_eq!(
            facts.widenable,
            sandbox.ends_with("permission-request tools are on"),
            "{name}: {sandbox}"
        );
        assert_eq!(
            facts.network_off,
            network.starts_with("off for agent commands;"),
            "{name}: {network}"
        );
        assert_eq!(
            facts.web_search_off,
            network.ends_with("web search disabled"),
            "{name}: {network}"
        );
        // The row never says more than the facts: "removed" or "only
        // allowlisted" with no profile or snapshot; a secret set explicitly
        // is not removed, so the facts claim nothing then.
        if facts.environment {
            assert!(
                (vault.contains("; only allowlisted environment variables reach agent commands")
                    || vault.contains("; secret-like environment variables are removed;"))
                    && vault.ends_with("login profiles and shell snapshots are not used"),
                "{name}: {vault}"
            );
        }
        assert_eq!(
            facts.broker,
            model_keys.starts_with("held by the isolated credential broker"),
            "{name}: {model_keys}"
        );
        seen.push(format!("{name}: {facts:?}"));
    }
    insta::assert_snapshot!("issue_816_before_restart_facts", seen.join("\n"));
}

/// #1468 (from #807's review): core keeps every run inside a policy that
/// keeps the macOS keychain services away (`unsandboxed_execution_allowed`),
/// so on macOS the rows must not say approved commands leave the sandbox.
/// Other platforms ignore the rule, and the escape route stays.
#[tokio::test]
async fn issue_1468_keychain_rule_keeps_commands_in_the_sandbox_on_macos() {
    let home = tempfile::tempdir().unwrap();
    let cwd = tempfile::tempdir().unwrap();
    let user_config = r#"approval_policy = "on-request"
default_permissions = "locked"

[permissions.locked]
extends = ":workspace"

[permissions.locked.filesystem]
":tmpdir" = "read"
":slash_tmp" = "read"
":keychain_services" = "deny"

[permissions.locked.network]
enabled = false
"#;
    let config = build(
        home.path(),
        cwd.path(),
        &with_windows_sandbox(user_config),
        /*aggressive_level*/ false,
    )
    .await;
    assert!(
        config
            .permissions
            .file_system_sandbox_policy()
            .denies_keychain_services()
    );
    let [sandbox, _, network, vault, _, _] = current_values(&config, SecurityLevel::Permissive);
    let vault = vault.split("; ").next().unwrap().to_string();
    if cfg!(target_os = "macos") {
        assert_eq!(
            [sandbox, network, vault],
            [
                "profile locked: commands can write to the current folder; approved and allow-listed commands stay inside the sandbox, which keeps the macOS keychain away; permission-request tools are off".to_string(),
                "off for agent commands; web search cached".to_string(),
                "the vault store and sign-in file are readable to agent commands".to_string(),
            ]
        );
    } else {
        assert_eq!(
            [sandbox, network],
            [
                "profile locked: commands can write to the current folder; approved or allow-listed commands can run outside the sandbox; permission-request tools are off".to_string(),
                "off inside the sandbox, on for commands that run outside it; web search cached".to_string(),
            ]
        );
    }
}
