//! Aggressive built only from controls that already exist (PF-24-S03 mapping
//! table). The launch path applies these values at the highest config
//! precedence, then [`verify`] checks the loaded config before Aggressive is
//! reported as active. Nothing here writes `config.toml`, so returning to
//! Permissive restores the user's own settings exactly.
//!
//! The sandbox row uses a named permission profile rather than a legacy
//! `SandboxPolicy`: under `untrusted`, an approved command that the legacy
//! sandbox blocks is retried outside it without asking again. A profile with a
//! denied-read path (the vault store) can never run unsandboxed, so approved
//! commands stay inside the current folder with network off.

use std::path::Path;

use codex_config::ConfigLayerSource;
use codex_config::ConfigLayerStackOrdering;
use codex_config::types::ShellEnvironmentPolicyToml;
use codex_execpolicy::Decision;
use codex_features::Feature;
use codex_protocol::config_types::ApprovalsReviewer;
use codex_protocol::config_types::WebSearchMode;
use codex_protocol::permissions::CREDENTIAL_FILE_NAME_PATTERNS;
use codex_protocol::permissions::ReadDenyMatcher;
use codex_protocol::permissions::credential_file_globs;
use codex_protocol::permissions::credential_file_names_shown;
use codex_protocol::permissions::is_credential_file_name;
use codex_protocol::permissions::unsafe_credential_entry_message;
use codex_protocol::protocol::AskForApproval;
use codex_protocol::shell_environment::create_env_from_vars;

use crate::legacy_core::ExecPolicyError;
use crate::legacy_core::config::Config;
use crate::legacy_core::config::ConfigOverrides;
use crate::legacy_core::format_exec_policy_error_with_source;
use crate::legacy_core::load_exec_policy;

/// The permission profile Aggressive defines and selects.
pub(crate) const PROFILE_ID: &str = codex_security_level::level::AGGRESSIVE_PROFILE_ID;

/// Removed from agent command environments in addition to the built-in
/// `*KEY*`, `*SECRET*` and `*TOKEN*` excludes.
const SECRET_ENV_PATTERNS: [&str; 4] = ["*VAULT*", "*PASSWORD*", "*PASSPHRASE*", "*CREDENTIAL*"];

/// One row of the mapping table, as shown before confirmation.
pub(crate) const ROWS: [(&str, &str); 6] = [
    (
        "Sandbox",
        "write only in the current folder (where the Corbanu home and the nested-launch registry stay read-only): no extra writable folders, no /tmp or $TMPDIR; approved commands stay inside it too; escalation and permission-request tools are off; the project's .git, .codex and .agents stay read-only, even with Grant once (it only lets one command write git hooks and config kept outside a .git folder and, when the current folder holds your home folder, start-up files, login agents and .claude settings); reading credential files (.env, .env.* and .envrc files, also ones created later; not .env.example, .env.sample or .env.template; SSH, cloud and CLI tokens) or Corbanu's sign-in, vault, config and history is denied to agent commands and is never granted, not even once (switch to Permissive in /security to allow it); on Linux and Windows a command is stopped when such a file appears while it runs, and a process already waiting for that file can read it in the moment before the stop; a credential file that is a link must be replaced with a regular file or moved out of the folder",
    ),
    (
        "Approvals",
        "untrusted: you approve every command that is not known read-only (reviewer: you, never auto-review)",
    ),
    ("Network", "off for agent commands; web search off"),
    (
        "Vault",
        "the vault store and sign-in file are unreadable to agent commands, and direct `corbanu vault …` commands are refused; secret-like environment variables (KEY, SECRET, TOKEN, VAULT, PASSWORD, PASSPHRASE, CREDENTIAL) are removed, and with secretless_agent_launch on so is every other variable outside an allowlist (for example PATH, HOME, locale, terminal, proxy and tool locations; set others explicitly in shell_environment_policy.set); login profiles and shell snapshots are not used",
    ),
    (MODEL_KEYS_LABEL, MODEL_KEYS_ROW),
    (
        "Child agents",
        "spawned agents get the same values; custom roles that would change them are refused at start; Claude panes are refused because Claude Code runs outside the sandbox, unless contained_external_agents and secretless_agent_launch run them in it, asking you before every command, file edit and web request (reading files stays automatic) (Claude Code sign-in still runs)",
    ),
];

/// #442 (Travis, 2026-10-12): what "Grant once" lifts under Aggressive, and
/// what it never does, wherever it is described (grant review, inspector).
/// It clears `ReadDenials`' lifted write rules (`issue_442_a_grant_lifts_only
/// _the_described_kinds` in Core pins that set). The real profile writes only
/// in the current folder, where the project's `.git` stays read-only whatever
/// the grant, and `.codex` and `.agents` are never lifted.
pub(crate) const GRANT_ONCE_LIFTS: &str = "lets one command write files that run code later, which Aggressive otherwise keeps read-only, and only inside the current folder: git hooks and config kept outside a .git folder (a hooks folder set with core.hooksPath, such as Husky's .husky, or a git folder with another name), and, when the current folder holds your home folder, your shell and git start-up files (such as .zshrc, .profile, .gitconfig and .local/bin), login agents and services, and .claude settings. Nothing else changes: credential files and Corbanu's sign-in, vault, config and history stay unreadable; Corbanu's own files and the project's .git, .codex and .agents stay read-only; other folders and the network stay closed.";

/// Not checked by [`verify`]: the person's own config may keep it off.
pub(crate) const MODEL_KEYS_LABEL: &str = "Model keys";

/// #391: Aggressive turns `broker_model_auth` on where the broker runs.
pub(crate) const MODEL_KEYS_ROW: &str = if cfg!(any(
    target_os = "macos",
    target_os = "linux",
    windows
)) {
    "provider API keys from the environment or the vault are held by the isolated credential broker, which sends model requests for Corbanu (sign-in tokens are passed to it; command, AWS and header sign-ins are not brokered; realtime and websockets are off); if it cannot start, model requests are refused, never sent directly (broker_model_auth; `broker_model_auth = false` in your own config keeps it off)"
} else {
    "unchanged: the credential broker does not run on this system, so Corbanu reads provider keys itself (broker_model_auth)"
};

/// The nested-launch line shown under the rows.
pub(crate) fn nested_row(nested: super::level::NestedAgents) -> &'static str {
    match nested {
        super::level::NestedAgents::Refuse => {
            "Nested agents: refuse. An agent command that starts another agent (`corbanu exec`, `review`, `resume`, `fork`, a new session, `app-server` or `mcp-server`) is refused."
        }
        super::level::NestedAgents::Pass => {
            "Nested agents: pass. `corbanu exec` and `review` started by an agent command run with Aggressive enforced; interactive sessions, `app-server` and `mcp-server` stay refused."
        }
    }
}

pub(crate) const UNCHANGED: &str = "Unchanged: model and provider, MCP servers, apps and hooks (they run outside the sandbox), wallet scopes, and commands you have already allowed permanently (they skip the prompt but stay sandboxed). `corbanu exec` that you start yourself and IDE sessions are not covered yet, except that they also hold provider keys in the credential broker and, while they run on this Corbanu home, take Core's Aggressive protections and approval for every command.";

/// Role config keys (dotted) that would give a spawned child different values.
const ROLE_KEYS: [&str; 15] = [
    "approval_policy",
    "approvals_reviewer",
    "sandbox_mode",
    "sandbox_workspace_write",
    "profile",
    "default_permissions",
    "permissions",
    "web_search",
    "tools.web_search",
    "shell_environment_policy",
    "allow_login_shell",
    "features.shell_snapshot",
    "features.request_permissions_tool",
    "features.exec_permission_approvals",
    "strict_rules",
];

fn string(value: &str) -> toml::Value {
    toml::Value::String(value.to_string())
}

/// `-c`-level overrides; they also reach the embedded app server, so every
/// thread it starts (and every child it spawns) is built from them.
///
/// `origin` is the Corbanu home whose stored level chose Aggressive: this
/// home, or for a nested launch the home of the session that started it.
/// Both paths must be valid UTF-8 (checked by the launch path) so the
/// profile's path keys are exact.
pub(crate) fn base_overrides(codex_home: &Path, origin: &Path) -> Vec<(String, toml::Value)> {
    base_overrides_with_registry(
        codex_home,
        origin,
        super::nested::origin_registry_dir().as_deref(),
    )
}

/// The registry path as a profile key: valid UTF-8 and no glob characters,
/// which only `deny` entries may use. `None` leaves it out of the profile;
/// verification then reports it if the workspace contains it.
fn registry_profile_key(registry: &Path) -> Option<&str> {
    registry
        .to_str()
        .filter(|path| !path.contains(['*', '?', '[', ']', '{', '}']))
}

/// [`base_overrides`] with the Aggressive-homes registry given explicitly.
pub(crate) fn base_overrides_with_registry(
    codex_home: &Path,
    origin: &Path,
    registry: Option<&Path>,
) -> Vec<(String, toml::Value)> {
    let profile = toml::toml! {
        extends = ":workspace"
        [filesystem]
        ":tmpdir" = "read"
        ":slash_tmp" = "read"
        [network]
        enabled = false
    };
    let mut profile = toml::Value::Table(profile);
    if let Some(filesystem) = profile
        .get_mut("filesystem")
        .and_then(toml::Value::as_table_mut)
    {
        // Read-only even when the workspace contains it: the stored level,
        // rules and config cannot be rewritten by an agent command.
        for home in [codex_home, origin] {
            let path = |child: &str| home.join(child).to_string_lossy().into_owned();
            filesystem.insert(home.to_string_lossy().into_owned(), string("read"));
            filesystem.insert(path("secrets"), string("deny"));
            filesystem.insert(path("auth.json"), string("deny"));
        }
        // The Aggressive-homes registry that nested-launch detection reads
        // (`super::nested`) is read-only too, so an agent whose workspace
        // contains it (the home folder) cannot delete its entries.
        if let Some(registry) = registry.and_then(registry_profile_key) {
            filesystem.insert(registry.to_string(), string("read"));
        }
        // #442 (Travis, 2026-10-11): credential files by name, so one created
        // after launch is denied too, not only those the preflight found.
        let workspace = CREDENTIAL_FILE_NAME_PATTERNS
            .iter()
            .map(|name| (format!("**/{name}"), string("deny")))
            .collect::<toml::Table>();
        filesystem.insert(":workspace_roots".to_string(), workspace.into());
        // Seatbelt matches the names everywhere by regex; the other sandboxes
        // can only expand them below the workspace (see `verify_sandbox`).
        if cfg!(target_os = "macos") {
            for glob in credential_file_globs("/") {
                filesystem.insert(glob, string("deny"));
            }
        }
    }
    vec![
        ("approval_policy".to_string(), string("untrusted")),
        ("approvals_reviewer".to_string(), string("user")),
        (format!("permissions.{PROFILE_ID}"), profile),
        ("default_permissions".to_string(), string(PROFILE_ID)),
        ("web_search".to_string(), string("disabled")),
        // Every thread this process starts later, including `/new`, resume,
        // fork and children with their own config folder, reloads the rules;
        // a file broken after launch must stop that thread, not drop the
        // vault rule.
        ("strict_rules".to_string(), toml::Value::Boolean(true)),
        // The shell snapshot and login-shell profiles re-export variables the
        // environment policy removed; agent commands must not load either.
        (
            "features.shell_snapshot".to_string(),
            toml::Value::Boolean(false),
        ),
        ("allow_login_shell".to_string(), toml::Value::Boolean(false)),
        // Both would let an approved request leave the profile.
        (
            "features.request_permissions_tool".to_string(),
            toml::Value::Boolean(false),
        ),
        (
            "features.exec_permission_approvals".to_string(),
            toml::Value::Boolean(false),
        ),
        (
            "shell_environment_policy.ignore_default_excludes".to_string(),
            toml::Value::Boolean(false),
        ),
        // Tells a `corbanu` started by an agent command which home chose
        // Aggressive (see `super::nested`).
        (
            format!("shell_environment_policy.set.{}", super::nested::ORIGIN_ENV),
            string(&origin.to_string_lossy()),
        ),
    ]
}

/// PF-29-S01: deny agent reads of `paths` in the Aggressive profile that
/// [`base_overrides`] defined.
pub(crate) fn deny_reads(overrides: &mut [(String, toml::Value)], paths: &[std::path::PathBuf]) {
    let key = format!("permissions.{PROFILE_ID}");
    for (_, profile) in overrides.iter_mut().filter(|(name, _)| *name == key) {
        if let Some(filesystem) = profile
            .get_mut("filesystem")
            .and_then(toml::Value::as_table_mut)
        {
            for path in paths {
                filesystem.insert(path.to_string_lossy().into_owned(), string("deny"));
            }
        }
    }
}

/// Environment overrides that extend, never replace, the user's own policy.
pub(crate) fn env_overrides(user_env: &ShellEnvironmentPolicyToml) -> Vec<(String, toml::Value)> {
    let mut overrides = Vec::new();
    // Keep the user's filters; legacy arrays and `filters` cannot be mixed.
    if user_env.filters.is_some() {
        overrides.extend(SECRET_ENV_PATTERNS.iter().map(|pattern| {
            (
                format!("shell_environment_policy.filters.{pattern}"),
                string("exclude"),
            )
        }));
    } else {
        let mut exclude = user_env.exclude.clone().unwrap_or_default();
        for pattern in SECRET_ENV_PATTERNS {
            if !exclude.iter().any(|existing| existing == pattern) {
                exclude.push(pattern.to_string());
            }
        }
        overrides.push((
            "shell_environment_policy.exclude".to_string(),
            toml::Value::Array(exclude.iter().map(|value| string(value)).collect()),
        ));
    }
    // Explicit `set` entries are applied after excludes; blank secret-named ones.
    let mut secret_sets = user_env
        .r#set
        .iter()
        .flatten()
        .map(|(name, _)| name)
        .filter(|name| is_secret_name(name))
        .collect::<Vec<_>>();
    secret_sets.sort();
    overrides.extend(
        secret_sets
            .into_iter()
            .map(|name| (format!("shell_environment_policy.set.{name}"), string(""))),
    );
    overrides
}

pub(super) fn is_secret_name(name: &str) -> bool {
    let upper = name.to_ascii_uppercase();
    [
        "KEY",
        "SECRET",
        "TOKEN",
        "VAULT",
        "PASSWORD",
        "PASSPHRASE",
        "CREDENTIAL",
    ]
    .iter()
    .any(|needle| upper.contains(needle))
}

/// Launch flags (`--sandbox`, `--ask-for-approval`, `--add-dir`, `--yolo`)
/// cannot weaken a stored Aggressive level. Returns what was overridden.
pub(crate) fn apply_launch_overrides(overrides: &mut ConfigOverrides) -> Vec<&'static str> {
    let mut replaced = Vec::new();
    if overrides
        .approval_policy
        .is_some_and(|policy| policy != AskForApproval::UnlessTrusted)
    {
        replaced.push("--ask-for-approval");
    }
    if overrides.sandbox_mode.is_some() || overrides.permission_profile.is_some() {
        replaced.push("--sandbox");
    }
    if !overrides.additional_writable_roots.is_empty() {
        replaced.push("--add-dir");
    }
    if overrides.bypass_hook_trust == Some(true) {
        replaced.push("--dangerously-bypass-hook-trust");
    }
    overrides.approval_policy = Some(AskForApproval::UnlessTrusted);
    overrides.approvals_reviewer = Some(ApprovalsReviewer::User);
    overrides.sandbox_mode = None;
    overrides.permission_profile = None;
    overrides.default_permissions = Some(PROFILE_ID.to_string());
    overrides.additional_writable_roots.clear();
    overrides.bypass_hook_trust = None;
    overrides.workspace_roots = None;
    overrides.tools_web_search_request = None;
    replaced
}

/// Every row must be observed in the loaded config; otherwise the failing
/// rows are returned and Aggressive must not be shown as active.
pub(crate) fn verify(config: &Config, rules_present: bool, origin: &Path) -> Vec<String> {
    verify_with_registry(
        config,
        rules_present,
        origin,
        super::nested::origin_registry_dir().as_deref(),
    )
}

/// [`verify`] with the Aggressive-homes registry given explicitly.
pub(crate) fn verify_with_registry(
    config: &Config,
    rules_present: bool,
    origin: &Path,
    registry: Option<&Path>,
) -> Vec<String> {
    let mut failures = Vec::new();
    verify_sandbox(config, &mut failures);
    if let Some(registry) = registry {
        verify_registry(config, registry, &mut failures);
    }
    let marker = config
        .permissions
        .shell_environment_policy
        .r#set
        .get(super::nested::ORIGIN_ENV);
    if marker.map(String::as_str) != origin.to_str() {
        failures.push(format!(
            "Child agents: agent commands would not get {}={}",
            super::nested::ORIGIN_ENV,
            origin.display()
        ));
    }
    let approval = config.permissions.approval_policy.value();
    if approval != AskForApproval::UnlessTrusted {
        failures.push(format!("Approvals: expected untrusted, got {approval}"));
    }
    if config.approvals_reviewer != ApprovalsReviewer::User {
        failures.push("Approvals: reviewer is not you".to_string());
    }
    if config.permissions.network_sandbox_policy().is_enabled() {
        failures.push("Network: sandbox network is enabled".to_string());
    }
    let web_search = config.web_search_mode.value();
    if web_search != WebSearchMode::Disabled {
        failures.push(format!("Network: web search is {web_search}"));
    }
    verify_vault(config, rules_present, &mut failures);
    verify_roles(config, &mut failures);
    failures
}

fn verify_sandbox(config: &Config, failures: &mut Vec<String>) {
    let cwd = config.cwd.as_path();
    let profile = config.permissions.active_permission_profile();
    if profile.as_ref().map(|profile| profile.id.as_str()) != Some(PROFILE_ID) {
        failures.push(format!(
            "Sandbox: expected profile {PROFILE_ID}, got {profile:?}"
        ));
    }
    // Only the launch overrides may define the profile; any other layer
    // (user, `-p` profile, project) could widen it through a table merge.
    for layer in config.config_layer_stack.get_layers(
        ConfigLayerStackOrdering::LowestPrecedenceFirst,
        /*include_disabled*/ false,
    ) {
        let defines_profile = layer
            .config
            .get("permissions")
            .and_then(|permissions| permissions.get(PROFILE_ID))
            .is_some();
        if defines_profile && !matches!(layer.name, ConfigLayerSource::SessionFlags) {
            failures.push(format!(
                "Sandbox: {:?} also defines permissions.{PROFILE_ID}",
                layer.name
            ));
        }
    }
    if cwd.parent().is_none() {
        failures.push("Sandbox: the current folder is the filesystem root".to_string());
    }
    if config.permissions.workspace_roots() != [config.cwd.clone()]
        || !config.permissions.profile_workspace_roots().is_empty()
    {
        failures.push("Sandbox: writable roots are not exactly the current folder".to_string());
    }
    if config.features.enabled(Feature::RequestPermissionsTool)
        || config.features.enabled(Feature::ExecPermissionApprovals)
    {
        failures.push("Sandbox: a permission-request tool is enabled".to_string());
    }
    let file_system = config.permissions.file_system_sandbox_policy();
    if !file_system.can_write_path_with_cwd(cwd, cwd) {
        failures.push("Sandbox: the current folder is not writable".to_string());
    }
    if !file_system.has_denied_read_restrictions() {
        failures.push("Sandbox: approved commands could run outside the sandbox".to_string());
    }
    let state_file = config
        .codex_home
        .join(super::level::STATE_FILE)
        .to_path_buf();
    if file_system.can_write_path_with_cwd(&state_file, cwd) {
        failures.push(format!("Sandbox: {} is writable", state_file.display()));
    }
    for outside in outside_paths(config) {
        if !outside.starts_with(cwd) && file_system.can_write_path_with_cwd(&outside, cwd) {
            failures.push(format!("Sandbox: {} is writable", outside.display()));
        }
    }
    verify_credential_names(&file_system, cwd, failures);
    // #594 (D225): on Linux, a home reached through a link agent commands
    // could replace; at launch and for every later folder.
    if super::home_link::applies()
        && let Some((real, links)) = super::home_link::check(&config.codex_home, |given| {
            super::home_link::exposed_links_under_aggressive(given, Some(&file_system), cwd)
        })
    {
        failures.push(super::home_link::aggressive_refusal(&real, &links));
    }
}

/// #442: a credential file created after launch is denied by its name: below
/// the current folder on every system (the exact globs the Linux sandbox
/// watches for), and anywhere under Seatbelt.
fn verify_credential_names(
    file_system: &codex_protocol::permissions::FileSystemSandboxPolicy,
    cwd: &Path,
    failures: &mut Vec<String>,
) {
    if cwd
        .to_string_lossy()
        .contains(['*', '?', '[', ']', '{', '}'])
    {
        failures.push(format!(
            "Sandbox: the current folder's path ({}) contains glob characters, so credential files created later cannot be denied by name; start from another folder",
            cwd.display()
        ));
        return;
    }
    let globs = file_system.get_unreadable_globs_with_cwd(cwd);
    let mut expected = credential_file_globs(&cwd.to_string_lossy());
    if cfg!(target_os = "macos") {
        expected.extend(credential_file_globs("/"));
    }
    let matcher = ReadDenyMatcher::try_new(file_system, cwd).ok().flatten();
    let probe = |path: &Path| matcher.as_ref().is_some_and(|m| m.is_read_denied(path));
    let probes_denied = [".env", "app/.env.local", ".envrc"]
        .iter()
        .all(|name| probe(&cwd.join("corbanu-new").join(name)));
    if !probes_denied || expected.iter().any(|glob| !globs.contains(glob)) {
        failures.push(format!(
            "Sandbox: credential files created later ({}) would be readable to agent commands",
            credential_file_names_shown()
        ));
    }
    failures.extend(unsafe_credential_entries(cwd));
}

/// Folder entries [`unsafe_credential_entries`] looks at, at most.
const MAX_CHECKED_ENTRIES: usize = 1_000_000;

/// #442: credential-named entries in the current folder that the sandbox
/// cannot protect, because what the person writes to them could land where
/// agent commands can read it (links, extra hard links, pipes). Agent
/// commands cannot create such names under Aggressive on macOS; this catches
/// ones made before (another session, a checkout). Only the current folder
/// is checked.
fn unsafe_credential_entries(cwd: &Path) -> Vec<String> {
    let Ok(root) = std::fs::canonicalize(cwd) else {
        return vec![format!("Sandbox: cannot read {}", cwd.display())];
    };
    let mut problems = Vec::new();
    let mut pending = vec![root];
    let mut seen = 0_usize;
    while let Some(folder) = pending.pop() {
        let entries = match std::fs::read_dir(&folder) {
            Ok(entries) => entries,
            Err(err) => {
                problems.push(format!(
                    "Sandbox: cannot check {} for unsafe credential files: {err}",
                    folder.display()
                ));
                continue;
            }
        };
        for entry in entries {
            let entry = match entry {
                Ok(entry) => entry,
                Err(err) => {
                    problems.push(format!(
                        "Sandbox: cannot check {} for unsafe credential files: {err}",
                        folder.display()
                    ));
                    continue;
                }
            };
            seen += 1;
            if seen > MAX_CHECKED_ENTRIES {
                problems.push(format!(
                    "Sandbox: more than {MAX_CHECKED_ENTRIES} files below {} to check for unsafe credential files; start from a smaller folder",
                    cwd.display()
                ));
                return problems;
            }
            let path = entry.path();
            let metadata = match std::fs::symlink_metadata(&path) {
                Ok(metadata) => metadata,
                Err(err) if err.kind() == std::io::ErrorKind::NotFound => continue,
                Err(err) => {
                    problems.push(format!(
                        "Sandbox: cannot check {} for unsafe credential files: {err}",
                        path.display()
                    ));
                    continue;
                }
            };
            if metadata.is_dir() {
                pending.push(path);
            } else if is_credential_file_name(&entry.file_name())
                && let Some(message) = unsafe_credential_entry_message(&path, &metadata)
            {
                // #442 (Travis, 2026-10-12): it names the entry and the fix.
                problems.push(format!("Sandbox: {message}"));
            }
        }
    }
    problems
}

/// The registry nested-launch detection reads must be read-only to agent
/// commands, and must exist when the workspace contains it: on Linux a
/// missing one would be created by the sandbox as a placeholder.
fn verify_registry(config: &Config, registry: &Path, failures: &mut Vec<String>) {
    let cwd = config.cwd.as_path();
    let file_system = config.permissions.file_system_sandbox_policy();
    if registry_profile_key(registry).is_none() {
        if registry.starts_with(cwd) {
            failures.push(format!(
                "Sandbox: the Aggressive-homes registry path {} cannot be written into the profile (not UTF-8, or it contains glob characters)",
                registry.display()
            ));
        }
        return;
    }
    if registry.starts_with(cwd) && !registry.is_dir() {
        failures.push(format!(
            "Sandbox: the Aggressive-homes registry {} is missing",
            registry.display()
        ));
    }
    if file_system.can_write_path_with_cwd(&registry.join("corbanu-aggressive-probe"), cwd) {
        failures.push(format!(
            "Sandbox: the Aggressive-homes registry {} is writable",
            registry.display()
        ));
    }
}

fn verify_vault(config: &Config, rules_present: bool, failures: &mut Vec<String>) {
    let cwd = config.cwd.as_path();
    let file_system = config.permissions.file_system_sandbox_policy();
    for protected in ["secrets", "auth.json"] {
        let path = config.codex_home.join(protected).to_path_buf();
        if file_system.can_read_path_with_cwd(&path, cwd) {
            failures.push(format!("Vault: {} is readable", path.display()));
        }
    }
    if !rules_present {
        failures.push("Vault: the exec-policy rule file is missing".to_string());
    }
    if !config.strict_rules {
        failures.push(
            "Vault: a rules file that breaks later would let new threads drop the vault rule"
                .to_string(),
        );
    }
    if config
        .config_layer_stack
        .ignore_user_and_project_exec_policy_rules()
    {
        failures.push("Vault: user exec-policy rules are ignored".to_string());
    }
    let probe = [
        "CORBANU_VAULT_X",
        "DB_PASSWORD",
        "GITHUB_TOKEN",
        "API_KEY",
        "AWS_SECRET",
    ]
    .map(|name| (name.to_string(), "x".to_string()));
    let leaked = create_env_from_vars(
        probe,
        &config.permissions.shell_environment_policy,
        /*thread_id*/ None,
    )
    .into_iter()
    .filter(|(name, value)| is_secret_name(name) && !value.is_empty())
    .map(|(name, _)| name)
    .collect::<Vec<_>>();
    if !leaked.is_empty() {
        failures.push(format!("Vault: environment keeps {}", leaked.join(", ")));
    }
    if config.features.enabled(Feature::ShellSnapshot) || config.permissions.allow_login_shell {
        failures.push("Vault: shell snapshots or login profiles can re-add variables".to_string());
    }
    let explicit = config
        .permissions
        .shell_environment_policy
        .r#set
        .iter()
        .filter(|(name, value)| is_secret_name(name) && !value.is_empty())
        .map(|(name, _)| name.as_str())
        .collect::<Vec<_>>();
    if !explicit.is_empty() {
        failures.push(format!("Vault: environment sets {}", explicit.join(", ")));
    }
}

/// Load the exec policy exactly as a session will and require it to forbid
/// every `<program> vault` command. A session that fails to parse any
/// `.rules` file falls back to an empty policy with only a warning, which
/// would silently drop the Aggressive vault rule.
pub(crate) async fn verify_exec_policy(config: &Config) -> Vec<String> {
    let policy = match load_exec_policy(&config.config_layer_stack).await {
        Ok(policy) => policy,
        Err(err @ ExecPolicyError::ParsePolicy { .. }) => {
            return vec![format!(
                "Vault: an exec-policy rules file does not parse, so sessions would drop every rule, including the vault rule: {}",
                format_exec_policy_error_with_source(&err)
            )];
        }
        Err(err) => {
            return vec![format!(
                "Vault: the exec-policy rules cannot be read: {}",
                format_exec_policy_error_with_source(&err)
            )];
        }
    };
    let mut failures = Vec::new();
    let allowed = super::level::VAULT_PROGRAMS
        .into_iter()
        .filter(|program| {
            let command = [*program, "vault", "auth-helper", "probe"].map(str::to_string);
            policy.check(&command, &|_| Decision::Allow).decision != Decision::Forbidden
        })
        .map(|program| format!("`{program} vault`"))
        .collect::<Vec<_>>();
    if !allowed.is_empty() {
        failures.push(format!(
            "Vault: the loaded exec policy does not forbid {}",
            allowed.join(", ")
        ));
    }
    // Sessions resolve absolute program paths through `host_executable`
    // entries; one for a vault program makes the rule skip every other path.
    let restricted = super::level::VAULT_PROGRAMS
        .into_iter()
        .filter(|program| policy.host_executables().contains_key(*program))
        .map(|program| format!("`{program}`"))
        .collect::<Vec<_>>();
    if !restricted.is_empty() {
        failures.push(format!(
            "Vault: host_executable rules limit {} to listed paths, so the vault rule would not apply at other paths",
            restricted.join(", ")
        ));
    }
    failures
}

/// A custom role's config layer is applied to spawned children after these
/// overrides; refuse roles that could give a child different values.
fn verify_roles(config: &Config, failures: &mut Vec<String>) {
    let cwd = config.cwd.as_path();
    let file_system = config.permissions.file_system_sandbox_policy();
    for (name, role) in &config.agent_roles {
        let Some(file) = role.config_file.as_ref() else {
            continue;
        };
        if file_system.can_write_path_with_cwd(file, cwd) {
            failures.push(format!(
                "Child agents: role `{name}` config {} is writable by agent commands",
                file.display()
            ));
        }
        let keys = std::fs::read_to_string(file)
            .ok()
            .and_then(|contents| toml::from_str::<toml::Table>(&contents).ok())
            .map(toml::Value::Table)
            .map(|value| {
                ROLE_KEYS
                    .into_iter()
                    .filter(|key| {
                        key.split('.')
                            .try_fold(&value, |node, part| node.get(part))
                            .is_some()
                    })
                    .collect::<Vec<_>>()
            });
        match keys {
            Some(keys) if keys.is_empty() => {}
            Some(keys) => failures.push(format!(
                "Child agents: role `{name}` sets {}",
                keys.join(", ")
            )),
            None => failures.push(format!(
                "Child agents: role `{name}` config {} cannot be read",
                file.display()
            )),
        }
    }
}

fn outside_paths(config: &Config) -> Vec<std::path::PathBuf> {
    let mut paths = vec![std::path::PathBuf::from("/tmp/corbanu-aggressive-probe")];
    if let Some(parent) = config.cwd.as_path().parent() {
        paths.push(parent.join("corbanu-aggressive-probe"));
    }
    if let Some(tmpdir) = std::env::var_os("TMPDIR") {
        paths.push(Path::new(&tmpdir).join("corbanu-aggressive-probe"));
    }
    paths
}

#[cfg(test)]
#[path = "aggressive_tests.rs"]
mod tests;
