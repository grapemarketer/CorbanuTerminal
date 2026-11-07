//! PF-25-S02: grants and kill switch, opened with `g` from `/security`.
//!
//! The list shows the grants held now (secret-free: command, runs left,
//! expiry), "Revoke all active authority" and the kill switch. Enter on a
//! row opens its review; only Enter there commits, through
//! `legacy_core::security_revocation`, off the UI thread. Esc on a review
//! changes nothing. Turning the kill switch off is the one choice that
//! removes protection: its review opens on "Back" and needs an arrow key to
//! reach "Turn it off", so a double Enter cannot release it, and it keeps the
//! level. Nothing an agent sends reaches these keys.

use std::cell::Cell;
use std::path::PathBuf;
use std::sync::mpsc;

use crossterm::event::KeyCode;
use crossterm::event::KeyEvent;
use ratatui::style::Stylize;
use ratatui::text::Line;

use crate::app_event::AppEvent;
use crate::app_event_sender::AppEventSender;
use crate::key_hint;
use crate::key_hint::KeyBindingListExt;
use crate::keymap::ListKeymap;
use crate::legacy_core::security_grant::HeldGrant;
use crate::legacy_core::security_grant::held_grants;
use crate::legacy_core::security_level_change::LevelBasis;
use crate::legacy_core::security_level_change::SecurityLevel;
use crate::legacy_core::security_level_change::StoredSecurityState;
use crate::legacy_core::security_revocation::HumanRevocation;
use crate::legacy_core::security_revocation::RevocationReport;
use crate::legacy_core::security_revocation::commit_human_revocation;
use crate::legacy_core::security_revocation::revoke_grant;
use crate::security::grant_view::held_lines;
use crate::security::view::profile_name;

/// One row of the list.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Row {
    Grant(HeldGrant),
    AllActiveAuthority,
    KillSwitch,
}

/// What a review would commit.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Choice {
    Grant(HeldGrant),
    Core(HumanRevocation),
}

/// A finished change: what to say, and whether "restart now" makes sense
/// (a saved Core change; not a single grant or a failure).
#[derive(Debug)]
struct Outcome {
    message: Result<String, String>,
    restartable: bool,
}

/// Where the list's grants come from (Core's ledger; tests replace it).
type GrantSource = Box<dyn Fn() -> Vec<HeldGrant>>;

#[derive(Debug)]
enum Screen {
    List {
        note: Option<String>,
    },
    /// `armed`: for a release, the person moved to "Turn it off".
    Review {
        choice: Choice,
        armed: bool,
    },
    Saving(mpsc::Receiver<Outcome>),
    Done(Outcome),
}

/// #1094: why "restart now" is not offered after a revocation in a remote or
/// daemon window. Restarting this terminal doesn't restart those sessions, so
/// it can't confirm the change holds there.
pub(crate) const REVOCATION_REMOTE_RESTART_REFUSED: &str = "Restart now is not offered: this window's sessions run in an app server outside this Corbanu Terminal.";

/// Where a commit goes: the session's Corbanu home, its configured level
/// and thread (as for `LevelBasis::read`).
#[derive(Clone, Debug)]
pub(crate) struct RevocationTarget {
    pub(crate) codex_home: PathBuf,
    pub(crate) configured: SecurityLevel,
    pub(crate) thread: Option<codex_protocol::ThreadId>,
    /// False when the session runs on a remote app server or the local daemon.
    pub(crate) sessions_in_this_process: bool,
}

pub(crate) struct RevocationView {
    target: RevocationTarget,
    keymap: ListKeymap,
    basis: LevelBasis,
    grants: Vec<HeldGrant>,
    grant_source: GrantSource,
    selected: usize,
    /// The line of the selected row in the last `lines` (for scrolling).
    selected_line: Cell<usize>,
    screen: Screen,
    app_event_tx: Option<AppEventSender>,
    /// Run the commit on the calling thread (tests).
    pub(crate) commit_inline: bool,
    pub(crate) closed: bool,
}

impl RevocationView {
    pub(crate) fn new(
        target: RevocationTarget,
        keymap: ListKeymap,
        app_event_tx: Option<AppEventSender>,
    ) -> Self {
        Self::with_grant_source(target, keymap, app_event_tx, Box::new(held_grants))
    }

    pub(crate) fn with_grant_source(
        target: RevocationTarget,
        keymap: ListKeymap,
        app_event_tx: Option<AppEventSender>,
        grant_source: GrantSource,
    ) -> Self {
        let basis = LevelBasis::read(&target.codex_home, target.configured, target.thread);
        Self {
            target,
            keymap,
            basis,
            grants: grant_source(),
            grant_source,
            selected: 0,
            selected_line: Cell::new(0),
            screen: Screen::List { note: None },
            app_event_tx,
            commit_inline: cfg!(test),
            closed: false,
        }
    }

    /// Read Core's state and the grants again, keeping the selected row by
    /// what it is (a grant by id). Returns false when the selected grant is
    /// gone.
    fn refresh(&mut self) -> bool {
        let selected = self.rows().get(self.selected).cloned();
        self.basis = LevelBasis::read(
            &self.target.codex_home,
            self.target.configured,
            self.target.thread,
        );
        self.grants = (self.grant_source)();
        let rows = self.rows();
        let found = selected.and_then(|selected| {
            rows.iter().position(|row| match (row, &selected) {
                (Row::Grant(row), Row::Grant(selected)) => row.grant_id == selected.grant_id,
                (row, selected) => row == selected,
            })
        });
        self.selected = found.unwrap_or(0).min(rows.len().saturating_sub(1));
        found.is_some()
    }

    /// How far to scroll `total` lines into `height` rows: the selected row
    /// stays visible in the list, and a review shows its end (its choices).
    pub(crate) fn scroll_for(&self, total: usize, height: u16) -> u16 {
        let height = usize::from(height.max(1));
        let scroll = match self.screen {
            Screen::List { .. } => (self.selected_line.get() + 1).saturating_sub(height),
            _ => total.saturating_sub(height),
        };
        u16::try_from(scroll).unwrap_or(u16::MAX)
    }

    fn rows(&self) -> Vec<Row> {
        let mut rows: Vec<Row> = self.grants.iter().cloned().map(Row::Grant).collect();
        rows.push(Row::AllActiveAuthority);
        rows.push(Row::KillSwitch);
        rows
    }

    pub(crate) fn saving(&self) -> bool {
        matches!(self.screen, Screen::Saving(_))
    }

    /// Collect a finished commit. Returns whether the screen changed.
    pub(crate) fn poll(&mut self) -> bool {
        let Screen::Saving(receiver) = &self.screen else {
            return false;
        };
        let outcome = match receiver.try_recv() {
            Ok(outcome) => outcome,
            Err(mpsc::TryRecvError::Empty) => return false,
            Err(mpsc::TryRecvError::Disconnected) => Outcome {
                message: Err("The change stopped unexpectedly; check /security again.".to_string()),
                restartable: false,
            },
        };
        self.refresh();
        self.screen = Screen::Done(outcome);
        true
    }

    pub(crate) fn handle_key_event(&mut self, key: KeyEvent) {
        let cancel =
            key_hint::plain(KeyCode::Esc).is_press(key) || self.keymap.cancel.is_pressed(key);
        let accept = self.keymap.accept.is_pressed(key);
        match &self.screen {
            Screen::List { .. } => {
                let rows = self.rows();
                if cancel {
                    self.closed = true;
                } else if self.keymap.move_up.is_pressed(key) {
                    self.selected = (self.selected + rows.len() - 1) % rows.len();
                    self.screen = Screen::List { note: None };
                } else if self.keymap.move_down.is_pressed(key) {
                    self.selected = (self.selected + 1) % rows.len();
                    self.screen = Screen::List { note: None };
                } else if accept {
                    if !self.refresh() {
                        self.screen = Screen::List {
                            note: Some(
                                "That grant has already ended (used up, expired or revoked)."
                                    .to_string(),
                            ),
                        };
                        return;
                    }
                    let rows = self.rows();
                    let choice = match rows.get(self.selected) {
                        Some(Row::Grant(grant)) => Choice::Grant(grant.clone()),
                        Some(Row::AllActiveAuthority) => {
                            Choice::Core(HumanRevocation::AllActiveAuthority)
                        }
                        Some(Row::KillSwitch) if self.basis.kill_switch_active => {
                            Choice::Core(HumanRevocation::KillSwitchOff)
                        }
                        Some(Row::KillSwitch) | None => Choice::Core(HumanRevocation::KillSwitchOn),
                    };
                    self.screen = Screen::Review {
                        choice,
                        armed: false,
                    };
                }
            }
            Screen::Review { choice, armed } => {
                let release = *choice == Choice::Core(HumanRevocation::KillSwitchOff);
                let navigate =
                    self.keymap.move_up.is_pressed(key) || self.keymap.move_down.is_pressed(key);
                if cancel || (release && accept && !armed) {
                    self.screen = Screen::List {
                        note: Some("Cancelled. Nothing changed.".to_string()),
                    };
                } else if release && navigate {
                    self.screen = Screen::Review {
                        choice: choice.clone(),
                        armed: !armed,
                    };
                } else if accept {
                    let choice = choice.clone();
                    self.commit(choice);
                }
            }
            Screen::Saving(_) => {}
            Screen::Done(outcome) => {
                if key_hint::plain(KeyCode::Char('r')).is_press(key)
                    && outcome.restartable
                    && let Some(tx) = &self.app_event_tx
                {
                    tx.send(AppEvent::RestartForSecurityLevel);
                } else if cancel || accept {
                    self.screen = Screen::List { note: None };
                }
            }
        }
    }

    /// The person pressed Enter on the review of `choice`.
    fn commit(&mut self, choice: Choice) {
        match choice {
            Choice::Grant(grant) => {
                let message = if revoke_grant(&grant.grant_id) {
                    Ok(format!(
                        "Grant revoked: `{}` now runs with the write protections again.",
                        grant.label
                    ))
                } else {
                    Err("That grant had already ended (used up, expired or revoked). Nothing changed.".to_string())
                };
                self.refresh();
                self.screen = Screen::Done(Outcome {
                    message,
                    restartable: false,
                });
            }
            Choice::Core(revocation) => {
                let target = self.target.clone();
                let reviewed = self.basis.clone();
                let app_event_tx = self.app_event_tx.clone();
                let run = move || {
                    let now = std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .map_or(0, |elapsed| elapsed.as_secs() as i64);
                    commit_human_revocation(
                        &target.codex_home,
                        target.configured,
                        target.thread,
                        &reviewed,
                        revocation,
                        now,
                    )
                    .map_or_else(
                        |error| Outcome {
                            message: Err(format!("Not changed: {error}.")),
                            restartable: false,
                        },
                        |report| {
                            // #507: Core stopped its sessions; the Claude
                            // panes are the app's to stop.
                            if let Some(tx) = &app_event_tx {
                                match (revocation, report.kill_switch_active) {
                                    (HumanRevocation::KillSwitchOn, true) => {
                                        tx.send(AppEvent::KillSwitchTurnedOn);
                                    }
                                    (HumanRevocation::KillSwitchOff, false) => {
                                        tx.send(AppEvent::KillSwitchTurnedOff);
                                    }
                                    _ => {}
                                }
                            }
                            let message = outcome_line(revocation, &report).map(|message| {
                                if !target.sessions_in_this_process && report.not_saved.is_none() {
                                    format!("{message} {REVOCATION_REMOTE_RESTART_REFUSED}")
                                } else {
                                    message
                                }
                            });
                            Outcome {
                                restartable: target.sessions_in_this_process
                                    && message.is_ok()
                                    && report.not_saved.is_none()
                                    && super::restart::restart_blocker().is_none(),
                                message,
                            }
                        },
                    )
                };
                if self.commit_inline {
                    let outcome = run();
                    self.refresh();
                    self.screen = Screen::Done(outcome);
                } else {
                    let (tx, rx) = mpsc::channel();
                    std::thread::spawn(move || {
                        let _ = tx.send(run());
                    });
                    self.screen = Screen::Saving(rx);
                }
            }
        }
    }

    pub(crate) fn lines(&self, width: u16) -> Vec<Line<'static>> {
        let width = usize::from(width.max(1));
        let wrap = |text: &str| -> Vec<Line<'static>> {
            textwrap::wrap(text, width)
                .into_iter()
                .map(|line| Line::from(line.into_owned()))
                .collect()
        };
        let kill = if self.basis.kill_switch_active {
            "on"
        } else {
            "off"
        };
        let mut lines: Vec<Line<'static>> = vec!["Grants and kill switch".bold().into()];
        lines.extend(wrap(&format!(
            "Core's level: {} · Kill switch: {kill}",
            profile_name(self.basis.in_force)
        )));
        if matches!(self.basis.stored, StoredSecurityState::Unreadable(_)) {
            lines.extend(
                wrap("Core's saved security state is unreadable: Aggressive and the kill switch are enforced until you choose a level in /security.")
                    .into_iter()
                    .map(Stylize::red),
            );
        }
        lines.push(Line::default());
        match &self.screen {
            Screen::List { note } => {
                if self.grants.is_empty() {
                    lines.extend(
                        wrap("No grants are held now.")
                            .into_iter()
                            .map(Stylize::dim),
                    );
                }
                let grant_lines = held_lines(&self.grants);
                for (index, row) in self.rows().iter().enumerate() {
                    let text = match row {
                        Row::Grant(_) => grant_lines.get(index).cloned().unwrap_or_default(),
                        Row::AllActiveAuthority => "Revoke all active authority".to_string(),
                        Row::KillSwitch if self.basis.kill_switch_active => {
                            "Turn the kill switch off".to_string()
                        }
                        Row::KillSwitch => "Turn the kill switch on".to_string(),
                    };
                    let selected = index == self.selected;
                    if selected {
                        self.selected_line.set(lines.len());
                    }
                    let marker = if selected { "> " } else { "  " };
                    for (line_index, line) in textwrap::wrap(
                        &text,
                        textwrap::Options::new(width.max(4).saturating_sub(2)),
                    )
                    .into_iter()
                    .enumerate()
                    {
                        let prefix = if line_index == 0 { marker } else { "  " };
                        let line = format!("{prefix}{line}");
                        lines.push(if selected {
                            line.cyan().bold().into()
                        } else {
                            line.into()
                        });
                    }
                }
                if let Some(note) = note {
                    lines.push(Line::default());
                    lines.extend(wrap(note).into_iter().map(Stylize::cyan));
                }
            }
            Screen::Review { choice, armed } => {
                let (title, body) = review_text(choice, self.basis.in_force);
                lines.push(title.bold().into());
                for paragraph in body {
                    lines.extend(wrap(&paragraph));
                }
                if *choice == Choice::Core(HumanRevocation::KillSwitchOff) {
                    lines.push(Line::default());
                    for (selected, text) in [
                        (!armed, "Back (the kill switch stays on)"),
                        (*armed, "Turn the kill switch off"),
                    ] {
                        lines.push(if selected {
                            format!("› {text}").cyan().bold().into()
                        } else {
                            format!("  {text}").into()
                        });
                    }
                }
            }
            Screen::Saving(_) => lines.extend(wrap("Saving…")),
            Screen::Done(Outcome {
                message: Ok(message),
                ..
            }) => lines.extend(wrap(message).into_iter().map(Stylize::green)),
            Screen::Done(Outcome {
                message: Err(message),
                ..
            }) => lines.extend(wrap(message).into_iter().map(Stylize::red)),
        }
        lines
    }

    pub(crate) fn footer(&self) -> String {
        match &self.screen {
            Screen::List { .. } => "↑/↓ choose · enter review · esc back to the levels".to_string(),
            Screen::Review {
                choice: Choice::Core(HumanRevocation::KillSwitchOff),
                ..
            } => "↑/↓ choose · enter confirm · esc cancel, nothing changes".to_string(),
            Screen::Review { .. } => "enter confirm · esc cancel, nothing changes".to_string(),
            Screen::Saving(_) => "saving…".to_string(),
            Screen::Done(Outcome {
                restartable: true, ..
            }) if self.app_event_tx.is_some() => {
                "r restart now to check it holds · enter or esc back".to_string()
            }
            Screen::Done(_) => "enter or esc back".to_string(),
        }
    }
}

fn review_text(choice: &Choice, level: SecurityLevel) -> (String, Vec<String>) {
    let level = profile_name(level);
    match choice {
        Choice::Grant(grant) => (
            "Revoke this grant?".to_string(),
            vec![
                held_lines(std::slice::from_ref(grant)).concat(),
                format!("Agent session: {}", grant.thread),
                "It ends now: the command runs with the write protections again. Other grants stay.".to_string(),
            ],
        ),
        Choice::Core(HumanRevocation::AllActiveAuthority) => (
            "Revoke all active authority?".to_string(),
            vec![
                "Ends now, in every session of this Corbanu Terminal on this Corbanu home: every grant, every \"for this session\" approval, and the credential broker's open channels. Work already running is not stopped.".to_string(),
                format!("Saved in security_state.json: revoked authority does not come back after a restart. The level stays {level}; the kill switch is unchanged."),
                // MOD-28 (Travis, 2026-10-15): saved rules are not authority
                // this revokes.
                "It does not suspend saved \"don't ask again\" rules: the commands they cover still run without asking, as before (under Moderate inside the sandbox; protected actions after untrusted content still ask). They are kept in rules/default.rules in the Corbanu home.".to_string(),
            ],
        ),
        Choice::Core(HumanRevocation::KillSwitchOn) => (
            "Turn the kill switch on?".to_string(),
            vec![
                "Everything \"Revoke all\" ends, and work running now in every pane of this Corbanu Terminal stops, Claude panes included: a reply in progress is cancelled and its commands are stopped, as when you interrupt it, and background terminals end.".to_string(),
                "Until you turn it off here: this Corbanu Terminal sends nothing to the model (each new turn is refused with a message saying so), messages and tasks from other agents wait (a Claude pane keeps only the newest task; a task for a Corbanu pane you opened yourself in /panes is refused instead), assignments and whips are paused, no grant can be issued, protected actions after untrusted content are refused, the browser and memory writes are refused, and the credential broker stays closed. Another Corbanu Terminal on this home takes the switch when it next starts.".to_string(),
                "Other Corbanu Terminals running on this home take it too, within about a second: their running work stops the same way and they refuse new turns.".to_string(),
                format!("Saved in security_state.json: it holds after a restart. The level stays {level}. Turning it off later restarts nothing: work resumes only when someone acts."),
            ],
        ),
        Choice::Core(HumanRevocation::KillSwitchOff) => (
            "Turn the kill switch off?".to_string(),
            vec![
                "Turns are sent to the model again, in Codex and Claude panes, but nothing restarts on its own: no pane starts a turn, stopped replies and commands do not restart, and paused assignments and whips stay paused until you resume them. Messages and tasks from other agents that waited go with each pane's next turn, when someone types in it or it gets new work. Protected actions can be approved again and grants can be issued. Revoked grants and approvals do not come back. Other Corbanu Terminals running on this home take it within about a second.".to_string(),
                format!("The level stays {level}: turning the kill switch off never lowers it."),
            ],
        ),
    }
}

fn outcome_line(choice: HumanRevocation, report: &RevocationReport) -> Result<String, String> {
    if choice == HumanRevocation::KillSwitchOn && !report.kill_switch_active {
        return Err("All active authority was revoked, but another session changed the kill switch first, so it is off. Review it again.".to_string());
    }
    if choice == HumanRevocation::KillSwitchOff && report.kill_switch_active {
        return Err(
            "The kill switch is still on (another session turned it on again). Review it again."
                .to_string(),
        );
    }
    let what = match choice {
        HumanRevocation::AllActiveAuthority => "All active authority revoked",
        HumanRevocation::KillSwitchOn => "Kill switch on",
        HumanRevocation::KillSwitchOff => "Kill switch off",
    };
    let level = profile_name(report.in_force);
    // #1035: the scope the review promised. Revoking (and turning the switch
    // on) reaches every session of this process on the home, so no other
    // session took it only when none runs; a release reaches only the
    // sessions that held that very switch.
    let scope = match (choice, report.live, report.other_sessions) {
        (_, false, false) => "for the next start",
        (HumanRevocation::KillSwitchOff, true, true) => {
            "in this session and the others of this Corbanu Terminal that held it"
        }
        (HumanRevocation::KillSwitchOff, true, false) => "in this session",
        (HumanRevocation::KillSwitchOff, false, true) => {
            "in the running sessions of this Corbanu Terminal that held it"
        }
        (_, true, true) => "in every session of this Corbanu Terminal on this Corbanu home",
        (_, true, false) => {
            "in every session of this Corbanu Terminal on this Corbanu home (only this session is running)"
        }
        (_, false, true) => {
            "in every running session of this Corbanu Terminal on this Corbanu home"
        }
    };
    // Only a saved switch reaches the other Corbanu Terminals: they watch
    // the saved state.
    let elsewhere = match choice {
        HumanRevocation::KillSwitchOn | HumanRevocation::KillSwitchOff => {
            " Other Corbanu Terminals running on this home take it within about a second."
        }
        HumanRevocation::AllActiveAuthority => "",
    };
    Ok(match &report.not_saved {
        None => {
            format!("{what} {scope}. Saved: it holds after a restart.{elsewhere} Level: {level}.")
        }
        Some(reason) => format!(
            "{what} {scope}, but it could not be saved ({reason}): it holds until Corbanu Terminal exits. Level: {level}."
        ),
    })
}

#[cfg(test)]
#[path = "revocation_view_tests.rs"]
mod tests;
