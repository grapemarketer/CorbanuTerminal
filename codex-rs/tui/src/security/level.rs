//! PF-24-S03: the human-chosen `/security` level behind the `security_levels`
//! flag.
//!
//! The stored level itself (`security_level.toml`, the Aggressive rule file)
//! lives in [`codex_security_level::level`]. It takes effect when Corbanu
//! Terminal starts: the launch path turns a stored Aggressive level into
//! existing controls (see [`super::aggressive`]) and verifies them before
//! anything is shown as active.

use std::path::Path;
use std::path::PathBuf;
use std::sync::OnceLock;

pub(crate) use codex_security_level::level::ChosenLevel;
pub(crate) use codex_security_level::level::ConfirmedLevel;
pub use codex_security_level::level::NestedAgents;
#[cfg(test)]
pub(crate) use codex_security_level::level::RULES_DIR;
pub(crate) use codex_security_level::level::STATE_FILE;
pub(crate) use codex_security_level::level::StoredLevel;
pub(crate) use codex_security_level::level::VAULT_PROGRAMS;
pub(crate) use codex_security_level::level::aggressive_running;
pub(crate) use codex_security_level::level::hold_aggressive_lock;
pub(crate) use codex_security_level::level::load;
pub(crate) use codex_security_level::level::load_confirmed;
pub(crate) use codex_security_level::level::load_state;
#[cfg(test)]
pub(crate) use codex_security_level::level::rules_contents;
pub(crate) use codex_security_level::level::rules_path;
pub(crate) use codex_security_level::level::save;
pub(crate) use codex_security_level::level::state_path;
pub(crate) use codex_security_level::level::sync_rules;

/// What this process enforces, fixed at launch.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct LevelContext {
    pub(crate) codex_home: PathBuf,
    /// The `security_levels` flag, or a stored non-Permissive level that the
    /// human must be able to see and change.
    pub(crate) picker_enabled: bool,
    /// Verified at launch; Aggressive only when every control was observed.
    pub(crate) active: ChosenLevel,
    /// The `protected_mode_preflight` flag (PF-29-S01).
    pub(crate) preflight_enabled: bool,
    /// What may be claimed about the protected boundary; `None` when no
    /// preflight applies (flag off or not Aggressive).
    pub(crate) boundary: Option<super::preflight::Boundary>,
}

static CONTEXT: OnceLock<LevelContext> = OnceLock::new();

/// Set once by the launch path after the Aggressive controls are verified.
pub(crate) fn install_context(context: LevelContext) {
    let _ = CONTEXT.set(context);
}

pub(crate) fn context() -> Option<&'static LevelContext> {
    CONTEXT.get()
}

/// The level the next start enforces: the saved one, or Moderate where Core
/// keeps it (`[security] level` in config, or Core's record over a
/// Permissive file) and the saved level says less (#750).
pub(crate) fn next_start(
    stored: &StoredLevel,
    core_next: crate::legacy_core::security_level_change::SecurityLevel,
) -> ChosenLevel {
    match stored.enforced() {
        ChosenLevel::Permissive
            if core_next == crate::legacy_core::security_level_change::SecurityLevel::Moderate =>
        {
            ChosenLevel::Moderate
        }
        saved => saved,
    }
}

/// `/status` line for the flagged level: what is active now and, when it
/// differs, what the next start will enforce (`core_next` is Core's level
/// for the next start).
pub(crate) fn status_line(
    active: ChosenLevel,
    stored: &StoredLevel,
    core_next: crate::legacy_core::security_level_change::SecurityLevel,
) -> String {
    let next = next_start(stored, core_next);
    match stored {
        StoredLevel::Invalid(_) => format!(
            "{} active; stored level unreadable, Aggressive enforced (/security)",
            active.name()
        ),
        StoredLevel::Absent | StoredLevel::Chosen(_) if next != active => format!(
            "{} active; {} saved for next start (/security)",
            active.name(),
            next.name()
        ),
        StoredLevel::Absent | StoredLevel::Chosen(_) => {
            format!("{} active (/security)", active.name())
        }
    }
}

/// Human permission changes are refused while Aggressive is stored, active
/// or (#1468) in force in Core's live policy on this home: `/permissions`
/// must not quietly undo Aggressive.
pub(crate) fn permission_change_block_reason() -> Option<String> {
    let context = context()?;
    permission_change_block_reason_in([
        context.active,
        load(&context.codex_home).enforced(),
        live_level(&context.codex_home),
    ])
}

fn permission_change_block_reason_in(levels: [ChosenLevel; 3]) -> Option<String> {
    levels.contains(&ChosenLevel::Aggressive).then(|| {
        "Security level Aggressive manages permissions. Choose Permissive in /security and restart before changing them.".to_string()
    })
}

/// Claude panes run Claude Code outside Corbanu's sandbox, with this
/// process's environment, network and Claude's own permission bypass, so no
/// protected level can contain them. Refused while a protected level is
/// active or saved: a pane could otherwise rewrite the saved level before the
/// restart that activates it. Contained panes (#218, feature
/// `contained_external_agents` with the secretless launch contract armed)
/// run sandboxed, behind the bridge and with a person's approval for every
/// command, file edit and web request, so they are allowed.
pub(crate) fn external_agent_block_reason() -> Option<String> {
    if crate::claude_panes::containment::contained_launch_ready() {
        return None;
    }
    #[cfg(test)]
    if let Some((active, stored)) = test_levels::LEVELS.get() {
        return external_agent_block_reason_in(active, stored, test_levels::LIVE.get());
    }
    let context = context()?;
    external_agent_block_reason_in(
        context.active,
        load(&context.codex_home).enforced(),
        live_level(&context.codex_home),
    )
}

/// #891, #807: the strictest of the active level, the saved level and the
/// level in force in Core's live policy on this home (the sessions of this
/// process) when that is a protected level (Moderate or Aggressive), for
/// the rules contained Claude panes take under it (#758: the keychain under
/// Aggressive; #891: user namespaces). The saved level counts as soon as it
/// is saved and the live policy as soon as it rises, as Core's own rules
/// follow the stricter of the configured level and the live policy.
pub(crate) fn protected_level_for_panes() -> Option<ChosenLevel> {
    #[cfg(test)]
    if let Some((active, stored)) = test_levels::LEVELS.get() {
        return strictest_protected([active, stored, test_levels::LIVE.get()]);
    }
    let context = context()?;
    strictest_protected([
        context.active,
        load(&context.codex_home).enforced(),
        live_level(&context.codex_home),
    ])
}

/// #807: the level in force in Core's live policy on `codex_home` for the
/// sessions of this process; Permissive when none runs here.
fn live_level(codex_home: &Path) -> ChosenLevel {
    use crate::legacy_core::security_level_change::SecurityLevel;
    match crate::legacy_core::security_level_change::strictest_live_level(codex_home) {
        Some(SecurityLevel::Aggressive) => ChosenLevel::Aggressive,
        Some(SecurityLevel::Moderate) => ChosenLevel::Moderate,
        Some(SecurityLevel::Permissive) | None => ChosenLevel::Permissive,
    }
}

fn strictest_protected(levels: [ChosenLevel; 3]) -> Option<ChosenLevel> {
    levels
        .into_iter()
        .max()
        .filter(|level| *level != ChosenLevel::Permissive)
}

/// For entry points that may have no launch context (`corbanu
/// claude-pane-smoke`): `codex_home`'s saved level, and the active level when
/// there is one.
pub(crate) fn external_agent_block_reason_for_home(codex_home: &Path) -> Option<String> {
    let active = context().map_or(ChosenLevel::Permissive, |context| context.active);
    external_agent_block_reason_in(active, load(codex_home).enforced(), live_level(codex_home))
}

/// Stand-in active and saved levels for the Claude pane gate tests, on the
/// current thread only.
#[cfg(test)]
pub(crate) mod test_levels {
    use std::cell::Cell;

    use super::ChosenLevel;

    thread_local! {
        pub(super) static LEVELS: Cell<Option<(ChosenLevel, ChosenLevel)>> =
            const { Cell::new(None) };
        /// #807: Core's live level, with [`LEVELS`] set.
        pub(super) static LIVE: Cell<ChosenLevel> = const { Cell::new(ChosenLevel::Permissive) };
    }

    /// Sets the levels until dropped (the live level back to Permissive).
    pub(crate) struct Guard;

    pub(crate) fn set(active: ChosenLevel, stored: ChosenLevel) -> Guard {
        LEVELS.set(Some((active, stored)));
        Guard
    }

    /// [`set`] with Core's live level too.
    pub(crate) fn set_with_live(
        active: ChosenLevel,
        stored: ChosenLevel,
        live: ChosenLevel,
    ) -> Guard {
        LIVE.set(live);
        set(active, stored)
    }

    impl Drop for Guard {
        fn drop(&mut self) {
            LEVELS.set(None);
            LIVE.set(ChosenLevel::Permissive);
        }
    }
}

/// #1468: `live` is the level in force in Core's live policy on this home,
/// which counts as the active and saved levels do. The message names the
/// strictest.
fn external_agent_block_reason_in(
    active: ChosenLevel,
    stored: ChosenLevel,
    live: ChosenLevel,
) -> Option<String> {
    strictest_protected([active, stored, live]).map(|level| {
        format!(
            "Claude panes are off under security level {}: Claude Code would run outside Corbanu's sandbox with your environment and network. Choose Permissive in /security and restart to use them, or turn on the contained_external_agents and secretless_agent_launch features to run them in the sandbox, asking you before every command, file edit and web request; /panes switches back to Main.",
            level.name()
        )
    })
}

#[cfg(test)]
#[path = "level_tests.rs"]
mod tests;
