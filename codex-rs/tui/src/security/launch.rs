//! Launch-time application of the stored `/security` level. With no state
//! file this is a no-op, so a flag-off install starts exactly as before.

use std::path::Path;
use std::path::PathBuf;

use codex_config::types::ShellEnvironmentPolicyToml;
use codex_features::Feature;

use super::aggressive;
use super::level;
use super::level::ChosenLevel;
use super::level::LevelContext;
use super::level::StoredLevel;
use super::preflight;
use crate::legacy_core::config::Config;
use crate::legacy_core::config::ConfigOverrides;

pub(crate) struct LaunchPlan {
    codex_home: PathBuf,
    stored: StoredLevel,
    replaced_flags: Vec<&'static str>,
    /// PF-29-S01: credential paths denied to agent commands.
    isolated: Vec<PathBuf>,
    /// PF-24-S02: another process enforcing Aggressive runs on this home, so
    /// this Permissive launch left its rule file and registry entry alone.
    shared_with_aggressive: bool,
}

/// Held while this process enforces Aggressive (see
/// [`level::hold_aggressive_lock`]); released when it exits or restarts.
static AGGRESSIVE_LOCK: std::sync::Mutex<Option<std::fs::File>> = std::sync::Mutex::new(None);

/// Before "restart now" starts the next process.
pub(crate) fn release_aggressive_lock() {
    AGGRESSIVE_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .take();
}

impl LaunchPlan {
    /// Read the stored level before any config is loaded, keep the vault rule
    /// file in step and add the Aggressive overrides.
    pub(crate) fn prepare(
        codex_home: &Path,
        cli_kv_overrides: &mut Vec<(String, toml::Value)>,
    ) -> Result<Self, String> {
        let stored = level::load(codex_home);
        // A Permissive launch must not remove the rule file (or the registry
        // entry) another process on this home still enforces Aggressive with.
        let shared_with_aggressive = stored.enforced() != ChosenLevel::Aggressive
            && stored != StoredLevel::Absent
            && level::aggressive_running(codex_home);
        if stored.enforced() == ChosenLevel::Aggressive {
            match level::hold_aggressive_lock(codex_home) {
                Ok(lock) => {
                    *AGGRESSIVE_LOCK
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(lock);
                }
                Err(err) => tracing::warn!("could not hold the Aggressive lock: {err}"),
            }
        }
        if stored != StoredLevel::Absent && !shared_with_aggressive {
            level::sync_rules(codex_home, stored.enforced()).map_err(|err| {
                format!(
                    "Could not apply the stored security level ({}): {err}",
                    level::rules_path(codex_home).display()
                )
            })?;
        }
        let mut plan = Self {
            codex_home: codex_home.to_path_buf(),
            stored,
            replaced_flags: Vec::new(),
            isolated: Vec::new(),
            shared_with_aggressive,
        };
        if plan.aggressive() && codex_home.to_str().is_none() {
            return Err(format!(
                "Security level Aggressive needs a UTF-8 Corbanu home path; {} is not.",
                codex_home.display()
            ));
        }
        if plan.aggressive() {
            cli_kv_overrides.extend(aggressive::base_overrides(codex_home, codex_home));
            // The profile denies reading it; an existing folder makes that
            // denial visible to nested-launch detection (`super::nested`).
            // The vault itself only looks for files inside.
            if let Err(err) = std::fs::create_dir_all(codex_home.join("secrets")) {
                tracing::warn!("could not create the vault store folder: {err}");
            }
            // The profile keeps the registry read-only; it must exist before
            // the first agent command so a command cannot create it first.
            if let Some(registry) = super::nested::origin_registry_dir()
                && let Err(err) = std::fs::create_dir_all(&registry)
            {
                tracing::warn!("could not create the Aggressive-homes registry: {err}");
            }
            // Only after a preflight (its receipt exists, even if unreadable);
            // denying more paths never weakens Aggressive.
            let cwd = std::env::current_dir().ok();
            plan.isolated = preflight::isolation_paths(
                codex_home,
                crate::legacy_core::protected_preflight::home_dir().as_deref(),
                cwd.as_deref(),
            );
            // #651: also a database folder given with `-c sqlite_home=…`.
            if !plan.isolated.is_empty() {
                let given = preflight::override_database_isolation(
                    codex_home,
                    cli_kv_overrides,
                    cwd.as_deref(),
                );
                plan.isolated.extend(given);
            }
            aggressive::deny_reads(cli_kv_overrides, &plan.isolated);
        }
        Ok(plan)
    }

    /// Whether this home should be recorded as an Aggressive origin for
    /// nested-launch detection (`Some(true)`), forgotten (`Some(false)`), or
    /// left alone because the level was never chosen.
    pub(crate) fn origin_registry_update(&self) -> Option<bool> {
        (self.stored != StoredLevel::Absent && !self.shared_with_aggressive)
            .then(|| self.aggressive())
    }

    fn aggressive(&self) -> bool {
        self.stored.enforced() == ChosenLevel::Aggressive
    }

    /// #750: what this process enforces. Moderate is Core's level (D1: no
    /// launch-time overrides), so it is active only where the loaded config
    /// enforces it: saved in `/security` with Core's record, or set as
    /// `[security] level` (#749: the inspector and picker no longer say
    /// Permissive next to Moderate in force). Records the Moderate warnings.
    pub(super) fn active_level(&self, config: &mut Config) -> ChosenLevel {
        use crate::legacy_core::security_level_change::SecurityLevel;
        let core = config.security_level;
        match self.stored.enforced() {
            ChosenLevel::Aggressive => return ChosenLevel::Aggressive,
            ChosenLevel::Moderate if core < SecurityLevel::Moderate => {
                config.startup_warnings.push(
                    "Security level Moderate was saved, but Core's level was not confirmed with it, so Moderate's protections are not on. Choose Moderate again in /security.".to_string(),
                );
                return ChosenLevel::Permissive;
            }
            ChosenLevel::Moderate => {}
            ChosenLevel::Permissive if core == SecurityLevel::Moderate => {}
            ChosenLevel::Permissive => return ChosenLevel::Permissive,
        }
        // D2: the credential broker is on by default where it runs; only an
        // explicit setting keeps it off.
        if cfg!(any(target_os = "macos", target_os = "linux", windows))
            && !config.features.enabled(Feature::BrokerModelAuth)
        {
            config.startup_warnings.push(
                "Security level Moderate: broker_model_auth is off (set in your configuration or by policy), so Corbanu reads provider keys itself instead of the credential broker.".to_string(),
            );
        }
        if !config.features.enabled(Feature::SourceEnvelopes) {
            config.startup_warnings.push(
                "Security level Moderate needs the source_envelopes feature: without it Corbanu refuses every model request. Turn it on, or choose Permissive in /security.".to_string(),
            );
        }
        // #594 (D225): a home reached through a link agent commands could
        // replace (Linux).
        if let Some(warning) = super::home_link::moderate_startup_warning(
            &self.codex_home,
            &config.permissions.file_system_sandbox_policy(),
            config.cwd.as_path(),
        ) {
            config.startup_warnings.push(warning);
        }
        ChosenLevel::Moderate
    }

    /// A remote app server does not load these overrides (nor, for
    /// Moderate, this home's Core record); refuse rather than show a level
    /// over a server that does not enforce it.
    pub(crate) fn check_target(&self, uses_remote_app_server: bool) -> Result<(), String> {
        let stored = self.stored.enforced();
        if stored != ChosenLevel::Permissive && uses_remote_app_server {
            return Err(format!(
                "Security level {} cannot be enforced on a remote app server. Start locally, or choose Permissive in /security first ({}).",
                stored.name(),
                level::state_path(&self.codex_home).display()
            ));
        }
        Ok(())
    }

    pub(crate) fn extend_env_overrides(
        &self,
        user_env: &ShellEnvironmentPolicyToml,
        cli_kv_overrides: &mut Vec<(String, toml::Value)>,
    ) {
        if self.aggressive() {
            cli_kv_overrides.extend(aggressive::env_overrides(user_env));
        }
    }

    pub(crate) fn apply_launch_overrides(&mut self, overrides: &mut ConfigOverrides) {
        if self.aggressive() {
            self.replaced_flags = aggressive::apply_launch_overrides(overrides);
        }
    }

    /// Verify the loaded config, record warnings and publish what is active.
    pub(crate) async fn finish(self, config: &mut Config) -> Result<(), String> {
        let launch_warning = match &self.stored {
            StoredLevel::Invalid(reason) => Some(reason.clone()),
            StoredLevel::Absent | StoredLevel::Chosen(_) => None,
        };
        let preflight_enabled = config.features.enabled(Feature::ProtectedModePreflight);
        let mut boundary = None;
        if self.aggressive() {
            verify_aggressive(&self.codex_home, &self.codex_home, config).await?;
            let mut unisolated = preflight::verify_isolation(config, &self.isolated);
            // #651: with a preflight, also a database folder set apart.
            if !self.isolated.is_empty() {
                unisolated.extend(preflight::verify_database_folder(config));
            }
            if !unisolated.is_empty() {
                return Err(format!(
                    "Security level Aggressive could not be fully applied:\n  {}\nChoose Permissive by editing {}.",
                    unisolated.join("\n  "),
                    level::state_path(&self.codex_home).display()
                ));
            }
            if preflight_enabled {
                let audited = preflight::audit_at_launch(&self.codex_home, config);
                match &audited {
                    preflight::Boundary::Clean { .. } => {}
                    preflight::Boundary::NotClean { blockers } => {
                        config.startup_warnings.push(format!(
                            "Security level Aggressive is enforced, but the protected boundary is not clean ({} blocker{}). Earlier conversations cannot be resumed. See /security:\n  {}",
                            blockers.len(),
                            if blockers.len() == 1 { "" } else { "s" },
                            blockers.join("\n  ")
                        ));
                    }
                    preflight::Boundary::Unverified(reason) => {
                        config.startup_warnings.push(format!(
                            "Security level Aggressive is enforced, but the protected boundary is unverified ({reason}). Earlier conversations cannot be resumed."
                        ));
                    }
                }
                boundary = Some(audited);
            }
            if let Some(home) = std::env::var_os("HOME")
                && config.cwd.as_path() == Path::new(&home)
            {
                config.startup_warnings.push(
                    "Security level Aggressive: the current folder is your home directory, so agent commands can write anywhere under it.".to_string(),
                );
            }
            if let Some(reason) = &launch_warning {
                config.startup_warnings.push(format!(
                    "Stored security level is unreadable or was changed outside /security ({reason}). Aggressive is enforced; choose a level in /security to repair it."
                ));
            }
            // PF-24-S02: a save that raised Core's level writes both records;
            // one missing means it did not finish. Nothing weaker applies.
            if preflight_enabled
                && preflight::receipt_path(&self.codex_home).exists()
                && level::load_confirmed(&self.codex_home)
                    != level::ConfirmedLevel::Level("aggressive".to_string())
            {
                config.startup_warnings.push(
                    "Security level Aggressive was saved, but Core's level was not confirmed with it, so Core's protections are not on. Choose Aggressive again in /security.".to_string(),
                );
            }
            // #391: Aggressive turns broker_model_auth on where the broker
            // runs; only an explicit setting keeps it off.
            if cfg!(any(target_os = "macos", target_os = "linux", windows))
                && !config.features.enabled(Feature::BrokerModelAuth)
            {
                config.startup_warnings.push(
                    "Security level Aggressive: broker_model_auth is off (set in your configuration or by policy), so Corbanu reads provider keys itself instead of the credential broker.".to_string(),
                );
            }
            if !self.replaced_flags.is_empty() {
                config.startup_warnings.push(format!(
                    "Security level Aggressive ignored {}.",
                    self.replaced_flags.join(", ")
                ));
            }
        }
        let active = self.active_level(config);
        if self.shared_with_aggressive {
            config.startup_warnings.push(
                "Another Corbanu Terminal using this Corbanu home still enforces Aggressive; its vault rule file stays until it exits.".to_string(),
            );
        }
        crate::claude_panes::containment::install(
            crate::claude_panes::containment::ContainmentSettings {
                enabled: config.features.enabled(Feature::ContainedExternalAgents),
                linux_sandbox_exe: config.codex_linux_sandbox_exe.clone(),
                state_root: None,
            },
        );
        level::install_context(LevelContext {
            codex_home: self.codex_home,
            picker_enabled: config.features.enabled(Feature::SecurityLevels)
                || !matches!(
                    self.stored,
                    StoredLevel::Absent | StoredLevel::Chosen(ChosenLevel::Permissive)
                ),
            active,
            preflight_enabled,
            boundary,
        });
        Ok(())
    }
}

/// `origin` is the home whose level chose Aggressive: `codex_home`, or for a
/// nested `corbanu exec` the home of the session that started it.
pub(crate) async fn verify_aggressive(
    codex_home: &Path,
    origin: &Path,
    config: &Config,
) -> Result<(), String> {
    let rules_present = std::fs::read_to_string(level::rules_path(codex_home))
        // Either nested-launch form: a session that saved another level
        // keeps the file it was started with until the next launch.
        .is_ok_and(|contents| codex_security_level::level::is_rules_contents(&contents));
    let mut failures = aggressive::verify(config, rules_present, origin);
    failures.extend(aggressive::verify_exec_policy(config).await);
    if failures.is_empty() {
        return Ok(());
    }
    Err(not_fully_applied(codex_home, &failures))
}

/// Why Aggressive refuses to start (or to keep running in a new folder).
pub(crate) fn not_fully_applied(codex_home: &Path, failures: &[String]) -> String {
    format!(
        "Security level Aggressive could not be fully applied:\n  {}\nFix the setting, or choose Permissive by editing {}.",
        failures.join("\n  "),
        level::state_path(codex_home).display()
    )
}

/// Every later config build (onboarding reload, resume/fork, new cwd) must
/// pass the same checks while this process enforces Aggressive.
pub(crate) async fn verify_reloaded(config: &Config) -> Result<(), String> {
    match level::context() {
        Some(context) if context.active == ChosenLevel::Aggressive => {
            verify_aggressive(&context.codex_home, &context.codex_home, config).await
        }
        Some(_) | None => Ok(()),
    }
}

#[cfg(test)]
#[path = "launch_tests.rs"]
mod tests;
