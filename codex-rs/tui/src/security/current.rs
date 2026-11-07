//! This session's value for each Aggressive mapping row, shown next to the
//! Aggressive value on the `/security` review (code-blind case B-06).
//!
//! These lines describe protection, so each one states only what the loaded
//! configuration enforces. When there is doubt, the line names the weaker
//! outcome; for example, a sandbox without denied reads lets approved and
//! allow-listed commands run outside it, so "off" network or an unreadable
//! vault store is never claimed for that case.
//!
//! Values come from the chat widget's config: approvals, sandbox, reviewer and
//! workspace roots follow `/permissions` and the session; web search, the
//! environment policy, shell settings and roles are this process's launch
//! settings. Core's level in force now adds what Core enforces on top
//! (#749): under Moderate no command leaves the sandbox (#747) and, with the
//! untrusted-content labels, the vault store and sign-in file are denied.
//! #792: the same holds under Aggressive in force, also before the restart
//! that follows confirming it in `/security` (#443): with the
//! untrusted-content labels and a sandbox Corbanu applies, its
//! protected-path rules deny the vault store and sign-in file, no approval
//! lifts them, and their denied reads keep every run in the sandbox (#747's
//! Aggressive cases). Direct `corbanu vault …` commands are refused under
//! Aggressive in any case.

use std::path::Path;

use codex_features::Feature;
use codex_protocol::config_types::ApprovalsReviewer;
use codex_protocol::config_types::ShellEnvironmentPolicy;
use codex_protocol::config_types::WebSearchMode;
use codex_protocol::permissions::FileSystemSandboxKind;
use codex_protocol::permissions::FileSystemSandboxPolicy;
use codex_protocol::protocol::AskForApproval;
use codex_protocol::shell_environment::create_env_from_vars;

use super::aggressive::ROWS;
use super::aggressive::is_secret_name;
use crate::legacy_core::config::Config;

/// One value per row of [`ROWS`], in the same order.
pub(crate) type CurrentValues = [String; ROWS.len()];

/// Secret-like name fragments, each with a probe variable that contains it.
const SECRET_PROBES: [(&str, &str); 7] = [
    ("KEY", "API_KEY"),
    ("SECRET", "AWS_SECRET"),
    ("TOKEN", "GITHUB_TOKEN"),
    ("VAULT", "CORBANU_VAULT_X"),
    ("PASSWORD", "DB_PASSWORD"),
    ("PASSPHRASE", "GPG_PASSPHRASE"),
    ("CREDENTIAL", "SVC_CREDENTIAL"),
];

/// Role config keys that cannot change what a spawned child is allowed to do.
const SAFE_ROLE_KEYS: [&str; 9] = [
    "description",
    "developer_instructions",
    "model",
    "model_provider",
    "model_reasoning_effort",
    "model_reasoning_summary",
    "model_verbosity",
    "nickname_candidates",
    "personality",
];

/// Core's level for the session now: its config, or the live policy (also
/// right after `/security` confirmed a level).
pub(crate) fn level_now(
    config: &Config,
    thread: Option<codex_protocol::ThreadId>,
) -> crate::legacy_core::security_level_change::SecurityLevel {
    use crate::legacy_core::security_level_change::LevelBasis;
    use crate::legacy_core::security_level_change::configured_level;
    LevelBasis::read(&config.codex_home, configured_level(config), thread)
        .in_force
        .max(config.security_level)
}

/// #816: what this session enforces once Core's level is Aggressive and
/// before the restart applies Aggressive's own settings: the facts behind
/// [`current_values`] for Aggressive, which the inspector's rows show from
/// confirmation. Each field is true only when that row says so then.
///
/// Confirming Aggressive sets approval for every command (#505), so a
/// session's permission-request tools can widen its sandbox or turn its
/// network on from then on even if it asked for no approvals before:
/// these facts count them whatever the approval policy is now.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct BeforeRestart {
    /// Approved and allow-listed commands stay inside this session's
    /// sandbox; the folders it lets them write to, as the Sandbox row says
    /// ("commands can write to the current folder, /private/tmp").
    pub(crate) sandbox_kept: Option<String>,
    /// Permission requests you approve can widen that sandbox (its
    /// permission-request tools are on).
    pub(crate) widenable: bool,
    /// The Network row says "off for agent commands".
    pub(crate) network_off: bool,
    /// Web search is disabled.
    pub(crate) web_search_off: bool,
    /// No secret-like environment variable reaches agent commands (or only
    /// allowlisted ones do), and login profiles and shell snapshots are not
    /// used.
    pub(crate) environment: bool,
    /// Model keys are held by the isolated credential broker.
    pub(crate) broker: bool,
}

/// #816: [`BeforeRestart`] for this session.
pub(crate) fn before_restart(config: &Config) -> BeforeRestart {
    evaluate(
        config,
        crate::legacy_core::security_level_change::SecurityLevel::Aggressive,
    )
    .1
}

/// `level` is Core's level in force now ([`level_now`]).
pub(crate) fn current_values(
    config: &Config,
    level: crate::legacy_core::security_level_change::SecurityLevel,
) -> CurrentValues {
    evaluate(config, level).0
}

/// The rows and, for Aggressive in force, what they say holds (#816).
fn evaluate(
    config: &Config,
    level: crate::legacy_core::security_level_change::SecurityLevel,
) -> (CurrentValues, BeforeRestart) {
    let cwd = config.cwd.as_path();
    let file_system = config.permissions.file_system_sandbox_policy();
    let restricted = matches!(file_system.kind, FileSystemSandboxKind::Restricted);
    // #747 (D3): under Moderate no command leaves the sandbox.
    let moderate = level == crate::legacy_core::security_level_change::SecurityLevel::Moderate;
    let aggressive = level == crate::legacy_core::security_level_change::SecurityLevel::Aggressive;
    // #792: Aggressive's protected-path rules (with the untrusted-content
    // labels) deny the vault store and sign-in file to every agent command,
    // from confirmation, and no approval lifts them (#442). Only a sandbox
    // takes the rules, and an external one keeps its own.
    let aggressive_rules =
        aggressive && restricted && config.features.enabled(Feature::SourceEnvelopes);
    let secretless = config.features.enabled(Feature::SecretlessAgentLaunch)
        || codex_protocol::secretless_launch::is_armed();
    // Without denied reads, escalated, allow-listed and retried commands run
    // outside the sandbox (`unsandboxed_execution_allowed` in core); the
    // denied reads of Aggressive's rules keep them in it. #739: protected
    // launch (secretless_agent_launch) refuses every run outside it, so none
    // is offered. #1468: on macOS a policy that keeps the keychain services
    // away keeps them in it too (#807), as in core.
    let escapes = restricted
        && !file_system.has_denied_read_restrictions()
        && !keychain_keeps_sandbox(&file_system)
        && !moderate
        && !aggressive_rules
        && !secretless;
    // #749: Moderate's rules deny the vault store and sign-in file to every
    // agent command (PF-23, #239), with the untrusted-content labels.
    // An external sandbox takes none of Core's rules (it keeps its own).
    let moderate_denies_store = moderate
        && config.features.enabled(Feature::SourceEnvelopes)
        && !matches!(file_system.kind, FileSystemSandboxKind::ExternalSandbox);

    let mut sandbox = match file_system.kind {
        FileSystemSandboxKind::Unrestricted => {
            "no sandbox: commands can write anywhere".to_string()
        }
        FileSystemSandboxKind::ExternalSandbox => {
            "an external sandbox decides; Corbanu Terminal applies none".to_string()
        }
        FileSystemSandboxKind::Restricted if file_system.has_full_disk_write_access() => {
            "commands can write anywhere".to_string()
        }
        FileSystemSandboxKind::Restricted => {
            let mut roots = file_system.get_writable_roots_with_cwd(cwd);
            // Roots come back with top-level aliases resolved (#467: `/tmp`
            // on macOS); nested links stay as written.
            let resolved_cwd = codex_utils_absolute_path::canonicalize_preserving_symlinks(cwd)
                .unwrap_or_else(|_| cwd.to_path_buf());
            let is_cwd = |root: &Path| root == cwd || root == resolved_cwd;
            // The current folder first; the rest in policy order.
            roots.sort_by_key(|root| !is_cwd(root.root.as_path()));
            let roots = roots
                .into_iter()
                .map(|root| {
                    if is_cwd(root.root.as_path()) {
                        "the current folder".to_string()
                    } else {
                        root.root.display().to_string()
                    }
                })
                .collect::<Vec<_>>();
            if roots.is_empty() {
                "commands cannot write files".to_string()
            } else {
                format!("commands can write to {}", roots.join(", "))
            }
        }
    };
    if let Some(profile) = config.permissions.active_permission_profile()
        && !profile.id.starts_with(':')
    {
        sandbox = format!("profile {}: {sandbox}", profile.id);
    }
    // #816: no command leaves a sandbox Corbanu applies.
    let sandbox_kept = (restricted && !escapes).then(|| sandbox.clone());
    if escapes {
        sandbox.push_str("; approved or allow-listed commands can run outside the sandbox");
    } else if moderate && restricted {
        sandbox.push_str(
            "; under Moderate approved and allow-listed commands stay inside the sandbox",
        );
    } else if aggressive_rules && !file_system.has_denied_read_restrictions() {
        // #792: before the restart, Core's rules do it.
        sandbox.push_str(
            "; under Aggressive approved and allow-listed commands stay inside the sandbox",
        );
    } else if secretless && restricted && !file_system.has_denied_read_restrictions() {
        // #739: protected launch does it; under Permissive nothing offers it.
        sandbox.push_str(if moderate || aggressive {
            "; with protected launch (secretless_agent_launch) no command runs outside the sandbox"
        } else {
            "; with protected launch (secretless_agent_launch) no command runs outside the sandbox, so none is offered"
        });
    } else if restricted
        && !file_system.has_denied_read_restrictions()
        && keychain_keeps_sandbox(&file_system)
    {
        // #1468: the keychain rule does it (#807).
        sandbox.push_str(
            "; approved and allow-listed commands stay inside the sandbox, which keeps the macOS keychain away",
        );
    }
    let request_tools = config.features.enabled(Feature::RequestPermissionsTool)
        || config.features.enabled(Feature::ExecPermissionApprovals);
    // An approved permission request can add network or reads inside the
    // sandbox, even when denied reads keep commands from leaving it.
    let widenable = restricted
        && request_tools
        && config.permissions.approval_policy.value() != AskForApproval::Never;
    sandbox.push_str(if request_tools {
        "; permission-request tools are on"
    } else {
        "; permission-request tools are off"
    });

    let reviewer = match config.approvals_reviewer {
        ApprovalsReviewer::User => "you",
        ApprovalsReviewer::AutoReview => "auto-review",
    };
    let approvals = format!(
        "{} (reviewer: {reviewer})",
        config.permissions.approval_policy.value()
    );

    // #816: from confirmation every command needs approval (#505), so the
    // tools can widen the sandbox whatever the policy is now.
    let network_off = restricted
        && !config.permissions.network_sandbox_policy().is_enabled()
        && !escapes
        && !request_tools;
    let network = if !restricted || config.permissions.network_sandbox_policy().is_enabled() {
        "on for agent commands"
    } else if escapes {
        "off inside the sandbox, on for commands that run outside it"
    } else if widenable {
        "off unless an approved permission request turns it on"
    } else {
        "off for agent commands"
    };
    let network = format!("{network}; web search {}", config.web_search_mode.value());

    let secrets = config.codex_home.join("secrets");
    // #739: the launch contract denies them to every agent command and
    // refuses a launch that could read them.
    let store_readable = !moderate_denies_store
        && !aggressive_rules
        && !secretless
        && (escapes
            || !restricted
            || [
                secrets.as_path(),
                config.codex_home.join("auth.json").as_path(),
            ]
            .iter()
            .any(|path| file_system.can_read_path_with_cwd(path, cwd))
            || file_system
                .get_readable_roots_with_cwd(cwd)
                .iter()
                .any(|root| root.as_path().starts_with(secrets.as_path())));
    let kept = kept_secret_kinds(
        &config.permissions.shell_environment_policy,
        std::env::vars(),
    );
    let profiles_used = config.features.enabled(Feature::ShellSnapshot)
        || config.permissions.allow_login_shell
        || config.permissions.shell_environment_policy.use_profile;
    let vault = format!(
        "the vault store and sign-in file are {} to agent commands{}; {}; {}",
        if store_readable {
            "readable"
        } else if aggressive_rules {
            "unreadable (Aggressive, also when you approve a command)"
        } else if moderate_denies_store && secretless {
            "unreadable (Moderate and secretless_agent_launch, also when you approve a command)"
        } else if moderate_denies_store {
            "unreadable (Moderate; your fresh approval of one command lifts this for that command)"
        } else if secretless {
            "unreadable (secretless_agent_launch, also when you approve a command)"
        } else if widenable {
            "unreadable (unless an approved permission request grants access)"
        } else {
            "unreadable"
        },
        // #443: Core refuses them under Aggressive from confirmation.
        if aggressive {
            ", and direct `corbanu vault …` commands are refused"
        } else {
            ""
        },
        // #440: the secretless launch keeps only allowlisted names and names
        // set explicitly, not every name that is not secret-like (and not
        // secret-like names the policy would otherwise pass through).
        if secretless {
            "only allowlisted environment variables reach agent commands (for example PATH, HOME, locale, terminal, proxy and tool locations; secretless_agent_launch); set others explicitly in shell_environment_policy.set".to_string()
        } else if kept.is_empty() {
            "secret-like environment variables are removed".to_string()
        } else {
            format!(
                "secret-like environment variables are passed through ({})",
                kept.join(", ")
            )
        },
        if profiles_used {
            "login profiles or shell snapshots are used"
        } else {
            "login profiles and shell snapshots are not used"
        }
    );

    let broker = config.features.enabled(Feature::BrokerModelAuth);
    let model_keys = if broker {
        "held by the isolated credential broker (broker_model_auth on)"
    } else {
        "Corbanu reads provider keys itself (broker_model_auth off)"
    }
    .to_string();

    let changing_roles = config
        .agent_roles
        .iter()
        .filter(|(_, role)| role.config_file.as_deref().is_some_and(role_changes_values))
        .map(|(name, _)| format!("`{name}`"))
        .collect::<Vec<_>>();
    let children = if changing_roles.is_empty() {
        "spawned agents get this session's values".to_string()
    } else {
        format!(
            "spawned agents get this session's values, except custom roles that change them: {}",
            changing_roles.join(", ")
        )
    };

    // Names set explicitly in shell_environment_policy.set reach commands
    // under protected launch too.
    let set_secret = config
        .permissions
        .shell_environment_policy
        .r#set
        .keys()
        .any(|name| is_secret_name(name));
    let facts = BeforeRestart {
        sandbox_kept,
        widenable: request_tools,
        network_off,
        web_search_off: config.web_search_mode.value() == WebSearchMode::Disabled,
        environment: (if secretless {
            !set_secret
        } else {
            kept.is_empty()
        }) && !profiles_used,
        broker,
    };
    (
        [sandbox, approvals, network, vault, model_keys, children],
        facts,
    )
}

/// #1468: core's `unsandboxed_execution_allowed` keeps a run inside a
/// policy that keeps the macOS keychain services away (#807): leaving it
/// would drop that rule.
fn keychain_keeps_sandbox(file_system: &FileSystemSandboxPolicy) -> bool {
    cfg!(target_os = "macos") && file_system.denies_keychain_services()
}

/// Secret-like name fragments whose variables reach agent commands: the fixed
/// probes plus every secret-like name in `env` (this process's environment).
/// Only names are kept.
fn kept_secret_kinds(
    policy: &ShellEnvironmentPolicy,
    env: impl IntoIterator<Item = (String, String)>,
) -> Vec<&'static str> {
    let probes = SECRET_PROBES.map(|(_, name)| (name.to_string(), "x".to_string()));
    let names = create_env_from_vars(probes, policy, /*thread_id*/ None)
        .into_iter()
        .chain(create_env_from_vars(env, policy, /*thread_id*/ None))
        .filter(|(name, value)| !value.is_empty() && is_secret_name(name))
        .map(|(name, _)| name.to_ascii_uppercase())
        .collect::<Vec<_>>();
    SECRET_PROBES
        .iter()
        .map(|(kind, _)| *kind)
        .filter(|kind| names.iter().any(|name| name.contains(kind)))
        .collect()
}

/// A role changes its children's values unless its config file sets only
/// [`SAFE_ROLE_KEYS`]; an unreadable file counts as changing them.
fn role_changes_values(file: &Path) -> bool {
    std::fs::read_to_string(file)
        .ok()
        .and_then(|contents| toml::from_str::<toml::Table>(&contents).ok())
        .is_none_or(|table| {
            table
                .keys()
                .any(|key| !SAFE_ROLE_KEYS.contains(&key.as_str()))
        })
}

#[cfg(test)]
#[path = "current_tests.rs"]
mod tests;
