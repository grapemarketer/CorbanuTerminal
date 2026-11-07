//! Contained Claude panes (issue #218, feature `contained_external_agents`).
//!
//! With the feature on, every Claude pane turn runs under the PF-27-S02
//! secretless launch contract instead of directly:
//! - a clean environment: the contract's allowlist, nothing proxy-related
//!   from the user, and only what the pane needs on top;
//! - every provider behind the per-turn loopback bridge, so Claude Code only
//!   ever holds the bridge token, never a provider key or `apiKeyHelper`;
//! - an OS sandbox around `claude` itself: writes only in the pane's folder
//!   and the pane's own Claude state folder (outside `CODEX_HOME`), the
//!   contract's protected reads denied, and network only to the bridge. The
//!   bridge is reached as `HTTP_PROXY`, which Seatbelt allows by port and the
//!   Linux sandbox carries into its network namespace over a Unix socket.

use std::collections::BTreeMap;
use std::collections::HashMap;
use std::path::Path;
use std::path::PathBuf;
use std::sync::OnceLock;

use anyhow::Context;
use anyhow::Result;
use anyhow::anyhow;
use codex_network_proxy::ManagedNetworkSandboxContext;
use codex_protocol::config_types::WindowsSandboxLevel;
use codex_protocol::models::PermissionProfile;
use codex_protocol::permissions::FileSystemAccessMode;
use codex_protocol::permissions::FileSystemPath;
use codex_protocol::permissions::FileSystemSandboxEntry;
use codex_protocol::permissions::FileSystemSandboxKind;
use codex_protocol::permissions::FileSystemSandboxPolicy;
use codex_protocol::permissions::NetworkSandboxPolicy;
use codex_protocol::secretless_launch;
use codex_sandboxing::SandboxCommand;
use codex_sandboxing::SandboxManager;
use codex_sandboxing::SandboxTransformRequest;
use codex_sandboxing::SandboxType;
use codex_sandboxing::get_platform_sandbox;
use codex_utils_absolute_path::AbsolutePathBuf;
use codex_utils_path_uri::PathUri;
use sha2::Digest;
use sha2::Sha256;

/// What the TUI's config says about contained panes, installed at startup.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ContainmentSettings {
    /// The `contained_external_agents` feature.
    pub(crate) enabled: bool,
    /// The Linux sandbox helper (`codex-linux-sandbox`), from the launch.
    pub(crate) linux_sandbox_exe: Option<PathBuf>,
    /// Where pane state folders go instead of the account's application
    /// state (tests).
    pub(crate) state_root: Option<PathBuf>,
}

static SETTINGS: OnceLock<ContainmentSettings> = OnceLock::new();

pub(crate) fn install(settings: ContainmentSettings) {
    let _ = SETTINGS.set(settings);
}

/// The installed settings when contained panes are on.
pub(crate) fn enabled() -> Option<ContainmentSettings> {
    #[cfg(test)]
    if let Some(settings) = test_settings::SETTINGS.with(|cell| cell.borrow().clone()) {
        return settings.enabled.then_some(settings);
    }
    SETTINGS.get().filter(|settings| settings.enabled).cloned()
}

/// Stand-in settings for tests, on the current thread only.
#[cfg(test)]
pub(crate) mod test_settings {
    use std::cell::Cell;
    use std::cell::RefCell;

    use super::ContainmentSettings;

    thread_local! {
        pub(super) static SETTINGS: RefCell<Option<ContainmentSettings>> =
            const { RefCell::new(None) };
        pub(super) static CONTRACT_ARMED: Cell<Option<bool>> = const { Cell::new(None) };
    }

    /// Stands in for whether the secretless launch contract is armed, until
    /// dropped.
    pub(crate) struct ArmedGuard;

    pub(crate) fn set_contract_armed(armed: bool) -> ArmedGuard {
        CONTRACT_ARMED.set(Some(armed));
        ArmedGuard
    }

    impl Drop for ArmedGuard {
        fn drop(&mut self) {
            CONTRACT_ARMED.set(None);
        }
    }

    /// Sets the settings until dropped.
    pub(crate) struct Guard;

    pub(crate) fn set(settings: ContainmentSettings) -> Guard {
        SETTINGS.with(|cell| *cell.borrow_mut() = Some(settings));
        Guard
    }

    impl Drop for Guard {
        fn drop(&mut self) {
            SETTINGS.with(|cell| *cell.borrow_mut() = None);
        }
    }
}

/// Whether Claude panes can run contained here: the feature is on, the
/// secretless launch contract is armed, and the platform has a sandbox.
pub(crate) fn contained_launch_ready() -> bool {
    #[cfg(test)]
    let armed = test_settings::CONTRACT_ARMED
        .get()
        .unwrap_or_else(crate::legacy_core::external_agent_contract_armed);
    #[cfg(not(test))]
    let armed = crate::legacy_core::external_agent_contract_armed();
    enabled().is_some()
        && armed
        && get_platform_sandbox(/*windows_sandbox_enabled*/ false).is_some()
}

/// How one contained turn is launched.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ClaudeContainment {
    /// The pane's own Claude state (`CLAUDE_CONFIG_DIR` and
    /// `CLAUDE_CODE_TMPDIR`), the only writable place besides the pane's
    /// folder.
    pub(crate) state_dir: PathBuf,
    /// `CODEX_HOME/panes`: every pane's transcripts and audits, unreadable
    /// to a contained pane, like the contract's `sessions`.
    pub(crate) panes_dir: PathBuf,
    pub(crate) linux_sandbox_exe: Option<PathBuf>,
}

impl ClaudeContainment {
    /// Corbanu's settings for the turn, read-only to Claude Code.
    pub(crate) fn settings_path(&self) -> PathBuf {
        self.state_dir.join("settings.json")
    }

    pub(crate) fn config_dir(&self) -> PathBuf {
        self.state_dir.join("config")
    }

    pub(crate) fn tmp_dir(&self) -> PathBuf {
        self.state_dir.join("tmp")
    }

    /// What a contained Claude Code gets on top of the launch environment
    /// (besides the bridge as its proxy): its own config and temporary
    /// folders, and no updater or other traffic.
    ///
    /// #743 (Travis, 2026-10-14, option a): `TMPDIR` stays the session's.
    /// The Linux sandbox keeps the placeholders it shares between runs (for
    /// missing protected paths in the home, such as `~/.aws`) in a registry
    /// under `TMPDIR`; a pane with its own `TMPDIR` could not see the main
    /// session's placeholders, and a command in either could fail to start.
    /// Claude Code keeps its own files under `CLAUDE_CODE_TMPDIR`. On Linux
    /// it starts inside the sandbox with the pane's `TMPDIR` (see
    /// [`pane_command`]); on macOS its Bash commands see the session's
    /// `TMPDIR`, which the pane cannot write.
    pub(crate) fn claude_env(&self) -> BTreeMap<String, String> {
        let config = self.config_dir().to_string_lossy().into_owned();
        let tmp = self.tmp_dir().to_string_lossy().into_owned();
        BTreeMap::from([
            ("CLAUDE_CONFIG_DIR".to_string(), config),
            ("CLAUDE_CODE_TMPDIR".to_string(), tmp),
            (
                "CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC".to_string(),
                "1".to_string(),
            ),
            ("DISABLE_AUTOUPDATER".to_string(), "1".to_string()),
        ])
    }
}

/// The pane's Claude state folder. It must sit outside `CODEX_HOME`, which
/// the contract never lets a launch write, so it lives in the account's
/// application state, keyed by the Corbanu home and the pane.
pub(crate) fn state_dir(
    codex_home: &Path,
    pane_id: &str,
    state_root: Option<&Path>,
) -> Result<PathBuf> {
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .filter(|home| home.is_absolute())
        .ok_or_else(|| anyhow!("contained Claude panes need an absolute HOME"))?;
    let base = if let Some(root) = state_root {
        root.to_path_buf()
    } else if cfg!(target_os = "macos") {
        home.join("Library/Application Support/Corbanu/claude-panes")
    } else {
        std::env::var_os("XDG_STATE_HOME")
            .map(PathBuf::from)
            .filter(|dir| dir.is_absolute())
            .unwrap_or_else(|| home.join(".local/state"))
            .join("corbanu/claude-panes")
    };
    let digest = Sha256::digest(codex_home.as_os_str().as_encoded_bytes());
    let home_key = digest
        .iter()
        .take(8)
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    Ok(base.join(home_key).join(pane_id))
}

/// The launch environment: the contract's allowlist applied to this process's
/// environment, without any proxy setting of the user's (the Linux sandbox
/// would carry a loopback proxy into the sandbox), then the pane's own
/// variables.
pub(crate) fn launch_env(
    inherited: impl IntoIterator<Item = (String, String)>,
    pane_env: &BTreeMap<String, String>,
) -> HashMap<String, String> {
    let mut env = inherited
        .into_iter()
        .filter(|(name, _)| !name.to_ascii_uppercase().contains("PROXY"))
        .collect::<HashMap<_, _>>();
    secretless_launch::retain_launch_env(&mut env, |_| false);
    env.extend(
        pane_env
            .iter()
            .map(|(name, value)| (name.clone(), value.clone())),
    );
    env
}

/// Writes only in the pane's folder and its Claude state folder, network off
/// except the bridge. The contract adds its protected-read denials.
pub(crate) fn base_profile(containment: &ClaudeContainment) -> Result<PermissionProfile> {
    let absolute = |path: &Path| {
        AbsolutePathBuf::from_absolute_path(path)
            .with_context(|| format!("`{}` is not absolute", path.display()))
    };
    let mut file_system = FileSystemSandboxPolicy::workspace_write(
        &[absolute(&containment.state_dir)?],
        /*exclude_tmpdir_env_var*/ true,
        /*exclude_slash_tmp*/ true,
    );
    file_system.entries.extend([
        FileSystemSandboxEntry::new(
            FileSystemPath::Path {
                path: absolute(&containment.settings_path())?,
            },
            FileSystemAccessMode::Read,
        ),
        FileSystemSandboxEntry::new(
            FileSystemPath::Path {
                path: absolute(&containment.panes_dir)?,
            },
            FileSystemAccessMode::Deny,
        ),
    ]);
    // Other panes' Claude state (their session transcripts) sits next to
    // this pane's. Each one is denied; the folder holding them is not,
    // because Claude Code examines every component of its own paths.
    if let Some(parent) = containment.state_dir.parent()
        && let Ok(siblings) = std::fs::read_dir(parent)
    {
        for sibling in siblings.flatten() {
            let path = sibling.path();
            if path != containment.state_dir {
                file_system.entries.push(FileSystemSandboxEntry::new(
                    FileSystemPath::Path {
                        path: absolute(&path)?,
                    },
                    FileSystemAccessMode::Deny,
                ));
            }
        }
    }
    Ok(PermissionProfile::from_runtime_permissions(
        &file_system,
        NetworkSandboxPolicy::Restricted,
    ))
}

/// A contained launch, ready to spawn.
#[derive(Debug)]
pub(crate) struct ContainedCommand {
    pub(crate) argv: Vec<String>,
    #[cfg_attr(
        not(unix),
        allow(dead_code, reason = "argv[0] is only overridden on Unix")
    )]
    pub(crate) arg0: Option<String>,
    pub(crate) env: HashMap<String, String>,
    /// #891: the security level ("Moderate" or "Aggressive") under which
    /// the launch denies creating user namespaces (Linux), if it does.
    pub(crate) user_namespaces_denied: Option<&'static str>,
}

/// Wrap `executable args` in the platform sandbox under the armed contract.
/// `bridge_port` is the only network destination allowed.
pub(crate) fn contain(
    containment: &ClaudeContainment,
    executable: &str,
    args: &[String],
    pane_env: &BTreeMap<String, String>,
    cwd: &Path,
    bridge_port: u16,
) -> Result<ContainedCommand> {
    for dir in [containment.config_dir(), containment.tmp_dir()] {
        std::fs::create_dir_all(&dir)
            .with_context(|| format!("failed to create `{}`", dir.display()))?;
    }
    let sandbox = get_platform_sandbox(/*windows_sandbox_enabled*/ false)
        .ok_or_else(|| anyhow!("contained Claude panes need macOS or Linux"))?;
    let inherited = std::env::vars_os()
        .filter_map(|(name, value)| Some((name.into_string().ok()?, value.into_string().ok()?)));
    let mut env = launch_env(inherited, pane_env);
    let mut argv = vec![executable.to_string()];
    argv.extend(args.iter().cloned());
    let profile = crate::legacy_core::protect_external_agent_launch(
        sandbox,
        &argv,
        &mut env,
        &base_profile(containment)?,
        cwd,
    )
    .map_err(|err| anyhow!(err))?;
    let (program, program_args) = pane_command(sandbox, &containment.tmp_dir(), executable, args);
    // #1468: one reading of the levels for both rules, so a level that
    // rises meanwhile cannot mix them.
    let level = crate::security::level::protected_level_for_panes();
    let user_namespaces = user_namespaces_denied_at(level);
    let mut command = sandboxed(
        containment,
        &program,
        &program_args,
        env,
        cwd,
        bridge_port,
        &rules_at(
            profile,
            keychain_denied_at(level),
            user_namespaces.is_some(),
        ),
    )?;
    command.user_namespaces_denied = user_namespaces;
    Ok(command)
}

/// #891 (Travis, 2026-10-16, D12 option a): on Linux, under Moderate and
/// Aggressive (the strictest of the active level, the saved level and, since
/// #807, Core's live policy on this home), a contained
/// pane may not create user namespaces either, as agent commands may not
/// (#826): `unshare -Ur` could otherwise open what the sandbox hides and
/// defeat nested-launch detection. Claude Code's own Bash sandbox
/// (bubblewrap) needs one, so it cannot start in such a pane; the pane says
/// so when a command reports the refusal ([`super::output_parse`]).
/// Corbanu's sandbox still applies. Permissive, macOS and Windows keep the
/// profile. `level` is [`crate::security::level::protected_level_for_panes`],
/// read once by [`contain`] for this rule and the keychain's (#1468).
fn user_namespaces_denied_at(
    level: Option<crate::security::level::ChosenLevel>,
) -> Option<&'static str> {
    if !cfg!(target_os = "linux") {
        return None;
    }
    level.map(crate::security::level::ChosenLevel::name)
}

/// #758, #807: under Aggressive (the strictest of the active level, the
/// saved level and Core's live policy, as for [`user_namespaces_denied_at`]) a
/// contained pane may not reach the macOS keychain services ([`rules_at`]).
/// `level` is [`crate::security::level::protected_level_for_panes`].
fn keychain_denied_at(level: Option<crate::security::level::ChosenLevel>) -> bool {
    level == Some(crate::security::level::ChosenLevel::Aggressive)
}

/// The program and arguments run inside the sandbox for `executable args`.
///
/// #743 follow-up (Travis, 2026-10-14, option b): on Linux, Claude Code
/// starts inside the sandbox with `TMPDIR` set to the pane's own temporary
/// folder `tmp_dir`, through `/usr/bin/env`, so `mktemp` and here-documents
/// in its Bash commands work. The sandbox itself keeps the session's
/// `TMPDIR`, under which it keeps the placeholder registry it shares with the
/// main session, so that bookkeeping and the sandbox policy are unchanged.
/// macOS is unchanged. `env` would take an `executable` containing `=` as an
/// assignment; such a path then fails the `--version` check and the launch is
/// refused.
pub(crate) fn pane_command(
    sandbox: SandboxType,
    tmp_dir: &Path,
    executable: &str,
    args: &[String],
) -> (String, Vec<String>) {
    if sandbox != SandboxType::LinuxSeccomp {
        return (executable.to_string(), args.to_vec());
    }
    let mut env_args = vec![
        format!("TMPDIR={}", tmp_dir.to_string_lossy()),
        executable.to_string(),
    ];
    env_args.extend(args.iter().cloned());
    ("/usr/bin/env".to_string(), env_args)
}

/// #758: under Aggressive a contained pane may not reach the macOS keychain
/// services either, although its bridge port selects Seatbelt's network
/// rules, which allow `com.apple.SecurityServer` (Travis, 2026-10-14). Claude
/// Code reaches the bridge over plain HTTP with the bridge's token, so it
/// needs neither TLS nor the keychain. #891: on Linux, under Moderate and
/// Aggressive, no user namespaces ([`user_namespaces_denied_at`]). Otherwise
/// the profile is kept. [`contain`] applies them.
fn rules_at(
    profile: PermissionProfile,
    deny_keychain: bool,
    deny_user_namespaces: bool,
) -> PermissionProfile {
    if !deny_keychain && !deny_user_namespaces {
        return profile;
    }
    let (mut file_system, network) = profile.to_runtime_permissions();
    if !matches!(file_system.kind, FileSystemSandboxKind::Restricted) {
        return profile;
    }
    if deny_keychain {
        file_system
            .entries
            .push(FileSystemSandboxEntry::deny_keychain_services());
    }
    if deny_user_namespaces {
        file_system
            .entries
            .push(FileSystemSandboxEntry::deny_user_namespaces());
    }
    PermissionProfile::from_runtime_permissions(&file_system, network)
}

/// The level rules [`contain`] applies, at the current levels.
#[cfg(test)]
pub(crate) fn level_rules(profile: PermissionProfile) -> PermissionProfile {
    let level = crate::security::level::protected_level_for_panes();
    rules_at(
        profile,
        keychain_denied_at(level),
        user_namespaces_denied_at(level).is_some(),
    )
}

/// `executable args` wrapped in the platform sandbox under `profile`, with
/// network only to `bridge_port` (`enforce_managed_network`).
pub(crate) fn sandboxed(
    containment: &ClaudeContainment,
    executable: &str,
    args: &[String],
    env: HashMap<String, String>,
    cwd: &Path,
    bridge_port: u16,
    profile: &PermissionProfile,
) -> Result<ContainedCommand> {
    let sandbox = get_platform_sandbox(/*windows_sandbox_enabled*/ false)
        .ok_or_else(|| anyhow!("contained Claude panes need macOS or Linux"))?;
    let cwd = AbsolutePathBuf::from_absolute_path(cwd).context("pane folder is not absolute")?;
    let cwd = PathUri::from_abs_path(&cwd);
    let request = SandboxManager::new()
        .transform(SandboxTransformRequest {
            command: SandboxCommand {
                program: executable.into(),
                args: args.to_vec(),
                cwd: cwd.clone(),
                env,
                managed_network: Some(ManagedNetworkSandboxContext {
                    loopback_ports: vec![bridge_port],
                    allow_local_binding: false,
                }),
                additional_permissions: None,
            },
            permissions: profile,
            sandbox,
            enforce_managed_network: true,
            environment_id: None,
            network: None,
            sandbox_policy_cwd: &cwd,
            codex_linux_sandbox_exe: containment.linux_sandbox_exe.as_deref(),
            use_legacy_landlock: false,
            windows_sandbox_level: WindowsSandboxLevel::Disabled,
            windows_sandbox_private_desktop: false,
        })
        .map_err(|err| anyhow!("could not sandbox Claude Code: {err}"))?;
    Ok(ContainedCommand {
        argv: request.command,
        arg0: request.arg0,
        env: request.env,
        user_namespaces_denied: None,
    })
}

#[cfg(test)]
#[path = "containment_tests.rs"]
mod tests;
