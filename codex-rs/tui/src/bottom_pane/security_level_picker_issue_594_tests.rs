//! #594 (D225): with the Corbanu home reached through a link agent commands
//! could replace (Linux), Aggressive cannot be chosen in `/security`: the
//! list says why, Enter explains the recovery and nothing is saved, also from
//! a review reached another way. Permissive and Moderate stay offered.

use crossterm::event::KeyModifiers;
use pretty_assertions::assert_eq;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::widgets::Paragraph;
use ratatui::widgets::Widget;

use super::*;
use crate::keymap::RuntimeKeymap;

const BLOCKER: &str = "Corbanu home: it is reached through a link (/home/user/.corbanu) in a folder agent commands can write, so a command could replace it and the next start would lose Aggressive. Start Corbanu Terminal with CORBANU_HOME=/home/user/dotfiles/corbanu (the real folder), or start it from a folder that does not contain the link.";

fn key(code: KeyCode) -> KeyEvent {
    KeyEvent::new(code, KeyModifiers::NONE)
}

fn render(picker: &SecurityLevelPicker, width: u16) -> String {
    let mut lines = picker.lines(width);
    lines.push(Line::from(picker.footer()));
    let area = Rect::new(0, 0, width, lines.len() as u16);
    let mut buffer = Buffer::empty(area);
    Paragraph::new(lines).render(area, &mut buffer);
    (0..area.height)
        .map(|y| {
            (0..area.width)
                .map(|x| buffer[(x, y)].symbol())
                .collect::<String>()
                .trim_end()
                .to_string()
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn open_picker(home: &std::path::Path) -> SecurityLevelPicker {
    let context = LevelContext {
        codex_home: home.to_path_buf(),
        picker_enabled: true,
        active: ChosenLevel::Permissive,
        preflight_enabled: false,
        boundary: None,
    };
    let current = [
        "commands can write to the current folder; permission-request tools are off",
        "on-request (reviewer: you)",
        "off for agent commands; web search cached",
        "the vault store and sign-in file are readable to agent commands",
        "held by the isolated credential broker (broker_model_auth on)",
        "spawned agents get this session's values",
    ]
    .map(str::to_string);
    let mut picker = SecurityLevelPicker::new(
        &context,
        current,
        /*preflight_input*/ None,
        RuntimeKeymap::defaults().list,
    );
    picker.set_moderate_facts(crate::security::moderate::Facts {
        untrusted_content: true,
        secretless_launch: true,
        output_gate: true,
    });
    picker
}

fn choose(picker: &mut SecurityLevelPicker, row: Row) {
    while ROWS[picker.selected] != row {
        picker.handle_key_event(key(KeyCode::Down));
    }
    picker.handle_key_event(key(KeyCode::Enter));
}

#[test]
fn issue_594_aggressive_is_refused_with_the_recovery_and_nothing_is_saved() {
    let home = tempfile::tempdir().unwrap();
    let mut picker = open_picker(home.path());
    picker.set_aggressive_home_link(Some(BLOCKER.to_string()));
    choose(&mut picker, Row::Aggressive);
    insta::assert_snapshot!(
        "security_level_picker_aggressive_refused_home_link",
        render(&picker, /*width*/ 80)
    );
    assert_eq!(
        picker.screen,
        Screen::List {
            note: Some(format!(
                "Aggressive cannot be chosen here. {BLOCKER} Nothing changed."
            ))
        }
    );
    assert_eq!(level::load(home.path()), StoredLevel::Absent);

    // A review reached another way (after a credential migration) cannot
    // save it either.
    picker.screen = Screen::Review(ChosenLevel::Aggressive);
    picker.handle_key_event(key(KeyCode::Enter));
    assert_eq!(picker.preflight_note, Some(format!("Not saved: {BLOCKER}")));
    assert_eq!(picker.screen, Screen::Review(ChosenLevel::Aggressive));
    assert_eq!(level::load(home.path()), StoredLevel::Absent);

    // Moderate is still offered.
    picker.screen = Screen::List { note: None };
    picker.selected = 0;
    choose(&mut picker, Row::Moderate);
    assert_eq!(picker.screen, Screen::Review(ChosenLevel::Moderate));

    // Without the link, Aggressive's review opens as before.
    picker.screen = Screen::List { note: None };
    picker.selected = 0;
    picker.set_aggressive_home_link(/*blocker*/ None);
    choose(&mut picker, Row::Aggressive);
    assert_eq!(picker.screen, Screen::Review(ChosenLevel::Aggressive));
}
