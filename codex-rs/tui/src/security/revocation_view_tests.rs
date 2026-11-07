use super::*;
use crossterm::event::KeyModifiers;
use insta::assert_snapshot;
use pretty_assertions::assert_eq;
use tempfile::TempDir;
use tokio::sync::mpsc::unbounded_channel;

fn view(home: &TempDir, app_event_tx: Option<AppEventSender>) -> RevocationView {
    RevocationView::new(
        RevocationTarget {
            codex_home: home.path().to_path_buf(),
            configured: SecurityLevel::Permissive,
            thread: None,
            sessions_in_this_process: true,
        },
        crate::keymap::RuntimeKeymap::defaults().list,
        app_event_tx,
    )
}

fn key(view: &mut RevocationView, code: KeyCode) {
    view.handle_key_event(KeyEvent::new(code, KeyModifiers::NONE));
}

fn text(view: &RevocationView) -> String {
    let mut lines: Vec<String> = view
        .lines(/*width*/ 90)
        .iter()
        .map(|line| {
            line.spans
                .iter()
                .map(|span| span.content.as_ref())
                .collect::<String>()
        })
        .collect();
    lines.push(String::new());
    lines.push(view.footer());
    lines.join("\n")
}

fn kill_switch_saved(home: &TempDir) -> bool {
    LevelBasis::read(home.path(), SecurityLevel::Permissive, /*thread*/ None).kill_switch_active
}

/// The kill switch row opens a review; Esc changes nothing.
#[test]
fn pf_25_s02_kill_switch_review_and_esc_change_nothing() {
    let home = TempDir::new().unwrap();
    let mut view = view(&home, /*app_event_tx*/ None);
    assert_snapshot!("pf_25_s02_list", text(&view));
    key(&mut view, KeyCode::Down);
    key(&mut view, KeyCode::Enter);
    assert_snapshot!("pf_25_s02_kill_switch_on_review", text(&view));
    key(&mut view, KeyCode::Esc);
    assert!(text(&view).contains("Cancelled. Nothing changed."));
    assert!(!kill_switch_saved(&home));
    assert!(!view.closed);
    key(&mut view, KeyCode::Esc);
    assert!(view.closed);
}

/// Enter on the review turns the kill switch on and saves it; turning it
/// off is its own review and keeps the level.
#[test]
fn pf_25_s02_kill_switch_on_then_off() {
    let home = TempDir::new().unwrap();
    let mut view = view(&home, /*app_event_tx*/ None);
    key(&mut view, KeyCode::Down);
    key(&mut view, KeyCode::Enter);
    key(&mut view, KeyCode::Enter);
    assert!(kill_switch_saved(&home));
    assert!(
        text(&view).contains("Kill switch on for the next start. Saved: it holds after a restart."),
        "{}",
        text(&view)
    );
    key(&mut view, KeyCode::Enter);
    assert!(text(&view).contains("Kill switch: on"));
    assert!(text(&view).contains("> Turn the kill switch off"));
    key(&mut view, KeyCode::Enter);
    assert_snapshot!("pf_25_s02_kill_switch_off_review", text(&view));
    // A second Enter goes back: releasing needs an arrow key first.
    key(&mut view, KeyCode::Enter);
    assert!(kill_switch_saved(&home));
    assert!(text(&view).contains("Cancelled. Nothing changed."));
    key(&mut view, KeyCode::Enter);
    key(&mut view, KeyCode::Down);
    key(&mut view, KeyCode::Enter);
    assert!(!kill_switch_saved(&home));
    assert!(text(&view).contains("Kill switch off"), "{}", text(&view));
}

/// "Revoke all active authority" is reviewed, then saved; the kill switch
/// stays off.
#[test]
fn pf_25_s02_revoke_all_is_reviewed_and_saved() {
    let home = TempDir::new().unwrap();
    let mut view = view(&home, /*app_event_tx*/ None);
    key(&mut view, KeyCode::Enter);
    assert!(text(&view).contains("Revoke all active authority?"));
    // MOD-28: the review says saved "don't ask again" rules are kept.
    assert!(
        text(&view).contains("It does not suspend saved \"don't ask again\" rules"),
        "{}",
        text(&view)
    );
    assert_snapshot!("mod_28_revoke_all_review", text(&view));
    key(&mut view, KeyCode::Enter);
    assert!(
        text(&view).contains("All active authority revoked for the next start."),
        "{}",
        text(&view)
    );
    assert!(!kill_switch_saved(&home));
}

/// A state changed since the review (another session turned the kill switch
/// on) is refused, and nothing is changed.
#[test]
fn pf_25_s02_changed_state_is_refused() {
    let home = TempDir::new().unwrap();
    let mut view = view(&home, /*app_event_tx*/ None);
    key(&mut view, KeyCode::Enter);
    let mut other = super::super::revocation_view::RevocationView::new(
        RevocationTarget {
            codex_home: home.path().to_path_buf(),
            configured: SecurityLevel::Permissive,
            thread: None,
            sessions_in_this_process: true,
        },
        crate::keymap::RuntimeKeymap::defaults().list,
        /*app_event_tx*/ None,
    );
    key(&mut other, KeyCode::Down);
    key(&mut other, KeyCode::Enter);
    key(&mut other, KeyCode::Enter);
    assert!(kill_switch_saved(&home));
    key(&mut view, KeyCode::Enter);
    assert!(
        text(&view).contains("Not changed: the security state changed since you reviewed it"),
        "{}",
        text(&view)
    );
}

/// After a change, `r` asks for "restart now".
#[test]
fn pf_25_s02_restart_after_a_change() {
    let home = TempDir::new().unwrap();
    let (tx, mut rx) = unbounded_channel::<AppEvent>();
    let mut view = view(&home, Some(AppEventSender::new(tx)));
    key(&mut view, KeyCode::Down);
    key(&mut view, KeyCode::Enter);
    key(&mut view, KeyCode::Enter);
    assert!(view.footer().starts_with("r restart now"));
    key(&mut view, KeyCode::Char('r'));
    let mut restart = false;
    while let Ok(event) = rx.try_recv() {
        restart |= matches!(event, AppEvent::RestartForSecurityLevel);
    }
    assert_eq!(restart, true);
}

/// #507: turning the kill switch on asks the app to stop the Claude panes'
/// running turns (Core stops its own sessions); turning it off or "Revoke
/// all" does not.
#[test]
fn issue_507_kill_switch_on_stops_claude_panes() {
    let home = TempDir::new().unwrap();
    let (tx, mut rx) = unbounded_channel::<AppEvent>();
    let mut view = view(&home, Some(AppEventSender::new(tx)));
    let stops = |rx: &mut tokio::sync::mpsc::UnboundedReceiver<AppEvent>| {
        let mut stops = 0;
        while let Ok(event) = rx.try_recv() {
            stops += usize::from(matches!(event, AppEvent::KillSwitchTurnedOn));
        }
        stops
    };
    // Revoke all: nothing to stop.
    key(&mut view, KeyCode::Enter);
    key(&mut view, KeyCode::Enter);
    assert_eq!(stops(&mut rx), 0);
    key(&mut view, KeyCode::Enter);
    // Kill switch on.
    key(&mut view, KeyCode::Down);
    key(&mut view, KeyCode::Enter);
    key(&mut view, KeyCode::Enter);
    assert!(kill_switch_saved(&home));
    assert_eq!(stops(&mut rx), 1);
    // Kill switch off.
    key(&mut view, KeyCode::Enter);
    key(&mut view, KeyCode::Enter);
    key(&mut view, KeyCode::Down);
    key(&mut view, KeyCode::Enter);
    assert!(!kill_switch_saved(&home));
    assert_eq!(stops(&mut rx), 0);
}

fn grant(id: &str, command: &str) -> HeldGrant {
    HeldGrant {
        thread: codex_protocol::ThreadId::from_string("019a0000-0000-7000-8000-000000000002")
            .expect("thread"),
        grant_id: id.to_string(),
        label: command.to_string(),
        command: vec!["cat".to_string(), command.to_string()],
        uses_left: None,
        expires_at_unix_seconds: 0,
    }
}

/// A view whose grant list the test controls.
fn view_with(
    home: &TempDir,
    grants: std::rc::Rc<std::cell::RefCell<Vec<HeldGrant>>>,
) -> RevocationView {
    RevocationView::with_grant_source(
        RevocationTarget {
            codex_home: home.path().to_path_buf(),
            configured: SecurityLevel::Permissive,
            thread: None,
            sessions_in_this_process: true,
        },
        crate::keymap::RuntimeKeymap::defaults().list,
        /*app_event_tx*/ None,
        Box::new(move || grants.borrow().clone()),
    )
}

/// Held grants are listed first; Enter reviews one. A grant that is no
/// longer held (here never in Core's ledger) is reported, nothing changes,
/// and "restart now" is not offered for it.
#[test]
fn pf_25_s02_grant_rows_review_and_already_ended() {
    let home = TempDir::new().unwrap();
    let grants = std::rc::Rc::new(std::cell::RefCell::new(vec![
        grant("a", "one.txt"),
        grant("b", "two.txt"),
    ]));
    let (tx, _rx) = unbounded_channel::<AppEvent>();
    let mut view = view_with(&home, grants);
    view.app_event_tx = Some(AppEventSender::new(tx));
    let list = text(&view);
    assert!(list.contains("> Grant: `cat one.txt`"), "{list}");
    assert!(list.contains("  Grant: `cat two.txt`"), "{list}");
    key(&mut view, KeyCode::Down);
    key(&mut view, KeyCode::Enter);
    let review = text(&view);
    assert!(review.contains("Revoke this grant?"), "{review}");
    assert!(review.contains("cat two.txt"), "{review}");
    key(&mut view, KeyCode::Enter);
    let done = text(&view);
    assert!(done.contains("had already ended"), "{done}");
    assert!(!view.footer().contains("restart"), "{}", view.footer());
}

/// The selection follows the grant, not the row number: a grant that ended
/// while the list was shown is reported instead of opening another row.
#[test]
fn pf_25_s02_selection_follows_the_grant() {
    let home = TempDir::new().unwrap();
    let grants = std::rc::Rc::new(std::cell::RefCell::new(vec![
        grant("a", "one.txt"),
        grant("b", "two.txt"),
    ]));
    let mut view = view_with(&home, std::rc::Rc::clone(&grants));
    key(&mut view, KeyCode::Down);
    grants.borrow_mut().remove(1);
    key(&mut view, KeyCode::Enter);
    assert!(
        text(&view).contains("That grant has already ended"),
        "{}",
        text(&view)
    );
    grants.borrow_mut().insert(0, grant("c", "zero.txt"));
    // "Revoke all" stays "Revoke all" when a grant row is added above it.
    key(&mut view, KeyCode::Down);
    assert!(text(&view).contains("> Revoke all active authority"));
    key(&mut view, KeyCode::Enter);
    assert!(
        text(&view).contains("Revoke all active authority?"),
        "{}",
        text(&view)
    );
}

/// Off the test thread, the commit runs in the background and its result is
/// collected by `poll`.
#[test]
fn pf_25_s02_commit_runs_off_the_ui_thread() {
    let home = TempDir::new().unwrap();
    let mut view = view(&home, /*app_event_tx*/ None);
    view.commit_inline = false;
    key(&mut view, KeyCode::Down);
    key(&mut view, KeyCode::Enter);
    key(&mut view, KeyCode::Enter);
    assert!(view.saving());
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    while !view.poll() {
        assert!(
            std::time::Instant::now() < deadline,
            "the commit never finished"
        );
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    assert!(kill_switch_saved(&home));
    assert!(text(&view).contains("Kill switch on"), "{}", text(&view));
}

/// Only this view commits a revocation or the kill switch: no other product
/// source names Core's entry points.
#[test]
fn pf_25_s02_only_this_view_revokes() {
    fn walk(dir: &std::path::Path, found: &mut Vec<std::path::PathBuf>) {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let name = entry.file_name();
            if path.is_dir() {
                if name != "target" && name != "node_modules" && name != ".git" {
                    walk(&path, found);
                }
            } else if path.extension().is_some_and(|extension| extension == "rs") {
                found.push(path);
            }
        }
    }
    // This scans the whole codex-rs source tree. Bazel runs tests from
    // runfiles, which hold only this crate's declared inputs, so the scan
    // would find nothing there; `cargo test` (postmerge CI) runs it.
    if option_env!("BAZEL_PACKAGE").is_some() {
        return;
    }
    let workspace = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("codex-rs");
    let mut files = Vec::new();
    walk(workspace, &mut files);
    let callers: Vec<String> = files
        .iter()
        .filter(|path| {
            let text = std::fs::read_to_string(path).unwrap_or_default();
            ["commit_human_revocation", "revoke_grant("]
                .iter()
                .any(|name| text.contains(name))
        })
        .filter_map(|path| path.strip_prefix(workspace).ok())
        // Forward slashes on every platform, so the names below match on Windows.
        .map(|path| path.display().to_string().replace('\\', "/"))
        .filter(|path| {
            !path.ends_with("_tests.rs")
                && path != "tui/src/claude_panes/tests.rs"
                && path != "tui/src/app/tests.rs"
                && !path.contains("/tests/")
        })
        .filter(|path| !path.starts_with("core/src/security/"))
        .collect();
    assert_eq!(
        callers,
        vec!["tui/src/security/revocation_view.rs".to_string()]
    );
}

/// A kill switch that lost a race to another session: authority was revoked,
/// the switch is off, and "restart now" is not offered.
#[test]
fn pf_25_s02_lost_race_is_reported() {
    let report = RevocationReport {
        in_force: SecurityLevel::Aggressive,
        kill_switch_active: false,
        live: true,
        other_sessions: false,
        not_saved: None,
    };
    let message = outcome_line(HumanRevocation::KillSwitchOn, &report);
    assert!(
        matches!(&message, Err(text) if text.contains("another session changed the kill switch first")),
        "{message:?}"
    );
    let message = outcome_line(
        HumanRevocation::KillSwitchOn,
        &RevocationReport {
            kill_switch_active: true,
            live: false,
            other_sessions: true,
            ..report
        },
    );
    assert_eq!(
        message,
        Ok("Kill switch on in every running session of this Corbanu Terminal on this Corbanu home. Saved: it holds after a restart. Other Corbanu Terminals running on this home take it within about a second. Level: Aggressive.".to_string())
    );
}

/// #1035: the confirmation names the scope the review promised ("every
/// session of this Corbanu Terminal on this Corbanu home"), not only "this
/// session".
#[test]
fn issue_1035_revoke_all_confirmation_names_every_session() {
    let report = RevocationReport {
        in_force: SecurityLevel::Moderate,
        kill_switch_active: false,
        live: true,
        other_sessions: false,
        not_saved: None,
    };
    let outcome = |report: &RevocationReport| {
        outcome_line(HumanRevocation::AllActiveAuthority, report).unwrap()
    };
    assert_eq!(
        outcome(&report),
        "All active authority revoked in every session of this Corbanu Terminal on this Corbanu home (only this session is running). Saved: it holds after a restart. Level: Moderate."
    );
    assert_eq!(
        outcome(&RevocationReport {
            other_sessions: true,
            ..report.clone()
        }),
        "All active authority revoked in every session of this Corbanu Terminal on this Corbanu home. Saved: it holds after a restart. Level: Moderate."
    );
    assert_eq!(
        outcome(&RevocationReport {
            other_sessions: true,
            not_saved: Some("disk full".to_string()),
            ..report
        }),
        "All active authority revoked in every session of this Corbanu Terminal on this Corbanu home, but it could not be saved (disk full): it holds until Corbanu Terminal exits. Level: Moderate."
    );
    // A release reaches only the sessions that held that switch.
    assert_eq!(
        outcome_line(
            HumanRevocation::KillSwitchOff,
            &RevocationReport {
                other_sessions: true,
                ..report
            }
        ),
        Ok("Kill switch off in this session and the others of this Corbanu Terminal that held it. Saved: it holds after a restart. Other Corbanu Terminals running on this home take it within about a second. Level: Moderate.".to_string())
    );
}
