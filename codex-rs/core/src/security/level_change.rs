//! PF-24-S02: the human `/security` confirmation's route into the PF-23-S03
//! transition (`effective_policy::transition`).
//!
//! The only caller is the trusted TUI, after a person pressed the confirm key
//! on the review screen. No `Op`, app-server method, tool or model output
//! reaches this module: a request cannot arrive over the wire, and the
//! controller it uses is never handed to agent runtimes.
//!
//! A commit goes through the policy tree of the session the person confirmed
//! in, so a stricter level and its revocation apply to it now and reach the
//! other trees of this process on that home. Without such a tree (no session
//! yet, or a remote app server) it goes through a tree built from the stored
//! state, which only saves. Either way the result is durable in `security_state.json` before it
//! is reported, except a stricter level whose save failed (it applies now and
//! says it will not survive a restart).
//!
//! The commit blocks for up to the store's lock wait (2 s), and a confirmed
//! Aggressive for up to 5 s more while the sessions of this process take
//! approval for every command (#505); call it off the async runtime.

use std::path::Path;

use codex_config::ConfigLayerSource;
use codex_protocol::SessionId;
use codex_protocol::ThreadId;
use codex_protocol::security::SecurityControlAction;
use codex_protocol::security::SecurityControlRequest;
use codex_security_policy::AuthorityEpoch;
use codex_security_policy::PolicyPrincipal;
use codex_security_policy::PrincipalKind;
pub use codex_security_policy::SecurityLevel;

pub use super::confirmed_approvals::ApprovalsAtConfirmation;
pub use super::confirmed_approvals::approvals_held;
use codex_security_policy::SecuritySettings;
use thiserror::Error;

use super::EffectivePolicyInitialization;
use super::EffectivePolicyView;
use super::PersistedHumanSecurityState;
use super::TrustedSecurityController;
use super::recovery;
use super::recovery::HomeTransitionStore;
use super::transition::ProbeOutcome;
use super::transition::TransitionError;
use super::transition::TransitionKind;
use super::transition::live_controller;
use crate::config::Config;

/// `security_state.json` as the next start reads it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum StoredSecurityState {
    Absent,
    Level(SecurityLevel),
    /// Present but unreadable: the next start enforces Aggressive and the
    /// kill switch.
    Unreadable(String),
}

impl StoredSecurityState {
    pub fn load(codex_home: &Path) -> Self {
        match recovery::load(codex_home) {
            Ok(None) => Self::Absent,
            Ok(Some(state)) => Self::Level(state.level),
            Err(recovery::LoadError::Io(reason) | recovery::LoadError::Corrupt(reason)) => {
                Self::Unreadable(reason)
            }
        }
    }
}

/// What the person reviewed. A confirmation commits only while it is still
/// true, so nothing they did not see is changed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LevelBasis {
    pub in_force: SecurityLevel,
    pub next_start: SecurityLevel,
    pub kill_switch_active: bool,
    pub stored: StoredSecurityState,
    /// Whether the session's own policy tree runs in this process, so a
    /// stricter level applies to it now.
    pub live: bool,
    /// That tree's epoch: any commit since the review moves it.
    epoch: Option<AuthorityEpoch>,
    /// The kill-switch event in force (PF-25-S02): a release is for this one
    /// only, also when no live tree's epoch would show a change.
    kill_switch_event: Option<String>,
}

impl LevelBasis {
    /// The live tree's epoch the review was shown under.
    pub(super) fn epoch(&self) -> Option<AuthorityEpoch> {
        self.epoch
    }

    /// `configured` is [`configured_level`] of the session's config;
    /// `thread` the session's thread, when it has started.
    pub fn read(codex_home: &Path, configured: SecurityLevel, thread: Option<ThreadId>) -> Self {
        let stored = StoredSecurityState::load(codex_home);
        if let Some(controller) = thread.and_then(|thread| live_controller(codex_home, thread))
            && let (Ok((in_force, next_start, kill_switch_active)), Ok(epoch)) =
                (controller.in_force(), controller.authority_epoch())
        {
            return Self {
                in_force,
                next_start,
                kill_switch_active,
                stored,
                live: true,
                epoch: Some(epoch),
                kill_switch_event: controller.kill_switch_event_id(),
            };
        }
        let recovered = recovery::recover(codex_home, configured);
        Self {
            in_force: recovered.level,
            next_start: recovered.level,
            kill_switch_active: recovered.revocations.kill_switch_active,
            stored,
            live: false,
            epoch: None,
            kill_switch_event: recovered
                .revocations
                .kill_switch_event_id()
                .map(|id| id.as_str().to_string()),
        }
    }
}

/// #978: whether sessions of this process run on `codex_home`, so a
/// confirmation reaches them now even when the confirming session's own
/// policy does not run here (no thread yet). False with a
/// remote app server: its sessions run in another process. #1015: sessions
/// other than `thread`'s, whose policy is initialized.
pub fn sessions_run_here(codex_home: &Path, thread: Option<ThreadId>) -> bool {
    super::transition::trees_run_on(codex_home, thread)
}

/// #807: the strictest level in force now in the live policy of the
/// sessions of this process on `codex_home` (also right after a person
/// confirms a level in `/security`); `None` when none runs here (no session
/// yet, or a remote app server).
pub fn strictest_live_level(codex_home: &Path) -> Option<SecurityLevel> {
    super::transition::strictest_live_level(codex_home)
}

/// Whether a downgrade to `target` rewrites `[security] level` in the user's
/// `config.toml` (it sets a stricter level there).
pub fn downgrade_rewrites_user_config(codex_home: &Path, target: SecurityLevel) -> bool {
    recovery::user_config_needs_level(codex_home, target)
}

/// The strictest `[security] level` any config layer sets, without the
/// stored state.
pub fn configured_level(config: &Config) -> SecurityLevel {
    crate::config::layered_security_floor(&config.config_layer_stack)
}

/// The strictest level a layer other than the user's own `config.toml` sets
/// (a repository, a profile, `-c`, managed configuration). A downgrade below
/// it is saved but the next start still enforces it.
pub fn non_user_level_floor(config: &Config) -> SecurityLevel {
    config
        .config_layer_stack
        .layers_high_to_low()
        .into_iter()
        .filter(|layer| !matches!(layer.name, ConfigLayerSource::User { .. }))
        .filter_map(|layer| {
            layer
                .config
                .get("security")?
                .get("level")?
                .clone()
                .try_into::<SecurityLevel>()
                .ok()
        })
        .max()
        .unwrap_or_default()
}

/// The PF-29 preflight for a stricter level: passed, or why not.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Probes {
    Passed,
    Blocked(Vec<String>),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LevelChangeKind {
    /// Applies now.
    Stricter,
    /// Saved now; enforced from the next start.
    Downgrade,
    /// The level in force is chosen again; a pending downgrade is undone.
    Unchanged,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LevelChangeReport {
    pub kind: LevelChangeKind,
    /// Level in force in this process's sessions now.
    pub in_force: SecurityLevel,
    pub next_start: SecurityLevel,
    /// Whether running sessions of this process took it.
    pub live: bool,
    /// Whether it reached other sessions of this process on the home (a
    /// stricter level applies to them now, also when `live` is false).
    pub other_sessions: bool,
    /// A stricter level applied now but could not be saved.
    pub not_saved: Option<String>,
    /// #505: Aggressive confirmed in this process: what requiring approval
    /// for every command did to its sessions (#978: with or without a live
    /// tree).
    pub approvals: Option<super::confirmed_approvals::ApprovalsAtConfirmation>,
}

#[derive(Debug, Error)]
pub enum LevelChangeError {
    #[error("the security state changed since you reviewed it; review it again")]
    Changed,
    #[error("{0}")]
    Refused(String),
    /// Nothing was saved and nothing changed.
    #[error("{0}")]
    NotSaved(String),
}

impl From<TransitionError> for LevelChangeError {
    fn from(error: TransitionError) -> Self {
        match error {
            TransitionError::StoredLevelChanged(_) | TransitionError::StoredStateChanged => {
                Self::Changed
            }
            TransitionError::Persist(_) => Self::NotSaved(error.to_string()),
            TransitionError::Policy(error) => error.into(),
            TransitionError::Blocked(_)
            | TransitionError::NotATransition
            | TransitionError::Revocation(_) => Self::Refused(error.to_string()),
        }
    }
}

impl From<super::SecurityPolicyError> for LevelChangeError {
    fn from(error: super::SecurityPolicyError) -> Self {
        match error {
            // The epoch moved since the review.
            super::SecurityPolicyError::AuthorityMismatch => Self::Changed,
            error => Self::Refused(error.to_string()),
        }
    }
}

/// Commit the level a person confirmed in `/security`. `reviewed` is the
/// basis shown on the review screen; `configured` as for [`LevelBasis::read`].
pub fn commit_human_level_change(
    codex_home: &Path,
    configured: SecurityLevel,
    thread: Option<ThreadId>,
    reviewed: &LevelBasis,
    target: SecurityLevel,
    probes: Probes,
    now_unix_seconds: i64,
) -> Result<LevelChangeReport, LevelChangeError> {
    if LevelBasis::read(codex_home, configured, thread) != *reviewed {
        return Err(LevelChangeError::Changed);
    }
    let (controller, live) = match thread.and_then(|thread| live_controller(codex_home, thread)) {
        Some(controller) => (controller, true),
        None => (stored_controller(codex_home, configured)?, false),
    };
    // Bound to the epoch the person reviewed: any commit since refuses.
    let epoch = match reviewed.epoch {
        Some(epoch) if live => epoch,
        Some(_) | None => controller.authority_epoch()?,
    };
    let request =
        SecurityControlRequest::new(epoch, SecurityControlAction::SetLevel { level: target })
            .map_err(|error| LevelChangeError::Refused(error.to_string()))?;
    let confirmed = controller.confirm_security_request(request, now_unix_seconds)?;
    // A stricter level saved by another session, which the person saw: a
    // lower choice lowers it, without raising this session first.
    let reviewed_stored = match reviewed.stored {
        StoredSecurityState::Level(level) => level,
        StoredSecurityState::Absent | StoredSecurityState::Unreadable(_) => {
            SecurityLevel::Permissive
        }
    };
    let prepared = controller.prepare_reviewed_transition(
        confirmed,
        match probes {
            Probes::Passed => ProbeOutcome::Passed,
            Probes::Blocked(blockers) => ProbeOutcome::Blocked(blockers),
        },
        Some(reviewed_stored),
    )?;
    let committed = controller.commit_transition(
        prepared,
        &HomeTransitionStore::new(codex_home),
        now_unix_seconds,
    )?;
    // #505: approval for every command, in the stored settings of every
    // session of this process on the home, from its next turn, and in those
    // it starts later. #978: also without a live tree here: the commit still
    // raised this process's other trees on the home.
    let approvals = (target == SecurityLevel::Aggressive
        && committed.level == SecurityLevel::Aggressive)
        .then(|| super::confirmed_approvals::confirm(codex_home));
    Ok(LevelChangeReport {
        kind: match committed.kind {
            TransitionKind::Restrictive => LevelChangeKind::Stricter,
            TransitionKind::Downgrade => LevelChangeKind::Downgrade,
            TransitionKind::KillSwitchRelease | TransitionKind::Unchanged => {
                LevelChangeKind::Unchanged
            }
        },
        in_force: committed.level,
        next_start: committed.next_start_level,
        live,
        other_sessions: committed.reached_other_trees,
        not_saved: committed.not_saved,
        approvals,
    })
}

/// A policy tree built from the stored state, for a commit made while no
/// session of this process uses the home. It saves; nothing else holds it.
pub(super) fn stored_controller(
    codex_home: &Path,
    configured: SecurityLevel,
) -> Result<TrustedSecurityController, LevelChangeError> {
    let recovered = recovery::recover(codex_home, configured);
    let root = ThreadId::new();
    let persisted = PersistedHumanSecurityState::new(
        SecuritySettings::new(recovered.level),
        PolicyPrincipal::new(PrincipalKind::Human, "human:security-picker")
            .map_err(|error| LevelChangeError::Refused(error.to_string()))?,
        recovered.revocations,
    )?;
    Ok(TrustedSecurityController::initialize(
        &EffectivePolicyView::default(),
        persisted,
        root,
        SessionId::from(root),
        EffectivePolicyInitialization::Root,
    )?)
}

#[cfg(test)]
#[path = "level_change_tests.rs"]
mod tests;
