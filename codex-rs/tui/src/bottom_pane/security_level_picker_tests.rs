use crossterm::event::KeyModifiers;
use pretty_assertions::assert_eq;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::widgets::Paragraph;
use ratatui::widgets::Widget;

use super::*;
use crate::keymap::RuntimeKeymap;

fn key(code: KeyCode) -> KeyEvent {
    KeyEvent::new(code, KeyModifiers::NONE)
}

fn context(home: &std::path::Path, active: ChosenLevel) -> LevelContext {
    LevelContext {
        codex_home: home.to_path_buf(),
        picker_enabled: true,
        active,
        preflight_enabled: false,
        boundary: None,
    }
}

/// Config C from the frozen code-blind design (case B-06), as the picker
/// would see it; `current_values` itself is covered in `aggressive_tests`.
fn current() -> CurrentValues {
    [
        "commands can write to the current folder, /tmp, /var/folders/tmp, /tmp/extra; approved or allow-listed commands can run outside the sandbox; permission-request tools are off",
        "on-request (reviewer: you)",
        "on for agent commands; web search live",
        "the vault store and sign-in file are readable to agent commands; secret-like environment variables are passed through (KEY, SECRET, TOKEN, VAULT, PASSWORD, PASSPHRASE, CREDENTIAL); login profiles or shell snapshots are used",
        "Corbanu reads provider keys itself (broker_model_auth off)",
        "spawned agents get this session's values",
    ]
    .map(str::to_string)
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

fn saved_level(picker: &SecurityLevelPicker) -> Option<ChosenLevel> {
    match &picker.screen {
        Screen::Saved(Ok(outcome)) => Some(outcome.level),
        _ => None,
    }
}

fn files(home: &std::path::Path) -> Vec<String> {
    let mut names = walk(home);
    names.sort();
    names
}

fn walk(dir: &std::path::Path) -> Vec<String> {
    std::fs::read_dir(dir)
        .map(|entries| {
            entries
                .flat_map(|entry| {
                    let path = entry.unwrap().path();
                    if path.is_dir() {
                        walk(&path)
                    } else {
                        vec![path.display().to_string()]
                    }
                })
                .collect()
        })
        .unwrap_or_default()
}

#[test]
fn list_review_and_saved_screens() {
    let home = tempfile::tempdir().unwrap();
    let mut picker = SecurityLevelPicker::new(
        &context(home.path(), ChosenLevel::Permissive),
        current(),
        /*preflight_input*/ None,
        RuntimeKeymap::defaults().list,
    );
    insta::assert_snapshot!("security_level_picker_list", render(&picker, /*width*/ 80));
    picker.handle_key_event(key(KeyCode::Down));
    picker.handle_key_event(key(KeyCode::Enter));
    // #750: Moderate opens its review; Esc goes back to the list.
    assert_eq!(picker.screen, Screen::Review(ChosenLevel::Moderate));
    picker.handle_key_event(key(KeyCode::Esc));
    picker.handle_key_event(key(KeyCode::Down));
    picker.handle_key_event(key(KeyCode::Enter));
    insta::assert_snapshot!(
        "security_level_picker_review_aggressive",
        render(&picker, /*width*/ 80)
    );
    picker.handle_key_event(key(KeyCode::Enter));
    insta::assert_snapshot!(
        "security_level_picker_saved_aggressive",
        render(&picker, /*width*/ 80)
    );
    assert_eq!(
        level::load(home.path()),
        StoredLevel::Chosen(ChosenLevel::Aggressive)
    );
    picker.handle_key_event(key(KeyCode::Enter));
    assert!(picker.closed);
}

#[test]
fn escape_and_typed_text_change_nothing() {
    let home = tempfile::tempdir().unwrap();
    let mut picker = SecurityLevelPicker::new(
        &context(home.path(), ChosenLevel::Permissive),
        current(),
        /*preflight_input*/ None,
        RuntimeKeymap::defaults().list,
    );
    picker.handle_key_event(key(KeyCode::Up));
    picker.handle_key_event(key(KeyCode::Enter));
    assert_eq!(picker.screen, Screen::Review(ChosenLevel::Aggressive));
    for typed in ['y', 'p', 'a', ' ', '1'] {
        picker.handle_key_event(key(KeyCode::Char(typed)));
    }
    assert_eq!(picker.screen, Screen::Review(ChosenLevel::Aggressive));
    picker.handle_key_event(key(KeyCode::Esc));
    assert_eq!(
        picker.screen,
        Screen::List {
            note: Some("Cancelled. Nothing changed.".to_string())
        }
    );
    picker.handle_key_event(key(KeyCode::Esc));
    assert_eq!(
        (picker.closed, level::load(home.path()), files(home.path())),
        (true, StoredLevel::Absent, Vec::<String>::new())
    );
}

#[test]
fn return_to_permissive_while_aggressive_is_active() {
    let home = tempfile::tempdir().unwrap();
    level::save(home.path(), ChosenLevel::Aggressive, NestedAgents::Refuse).unwrap();
    let mut picker = SecurityLevelPicker::new(
        &context(home.path(), ChosenLevel::Aggressive),
        current(),
        /*preflight_input*/ None,
        RuntimeKeymap::defaults().list,
    );
    assert_eq!(picker.selected, 2);
    picker.handle_key_event(key(KeyCode::Down));
    picker.handle_key_event(key(KeyCode::Enter));
    insta::assert_snapshot!(
        "security_level_picker_review_permissive",
        render(&picker, /*width*/ 80)
    );
    picker.handle_key_event(key(KeyCode::Enter));
    insta::assert_snapshot!(
        "security_level_picker_saved_permissive_pending",
        render(&picker, /*width*/ 80)
    );
    // The session is still Aggressive, so its vault rule file stays until the
    // restart that activates Permissive (#203).
    assert_eq!(
        (
            level::load(home.path()),
            level::rules_path(home.path()).exists()
        ),
        (StoredLevel::Chosen(ChosenLevel::Permissive), true)
    );
}

#[test]
fn unreadable_state_is_reported_and_aggressive_selected() {
    let home = tempfile::tempdir().unwrap();
    std::fs::write(
        level::state_path(home.path()),
        "version = 1\nlevel = \"max\"\n",
    )
    .unwrap();
    let picker = SecurityLevelPicker::new(
        &context(home.path(), ChosenLevel::Aggressive),
        current(),
        /*preflight_input*/ None,
        RuntimeKeymap::defaults().list,
    );
    // The state file's full path, so the snapshot holds on Windows too (#691).
    let rendered = render(&picker, /*width*/ 400).replace(
        &level::state_path(home.path()).display().to_string(),
        "<home>/security_level.toml",
    );
    insta::assert_snapshot!("security_level_picker_unreadable", rendered);
}

#[test]
fn choosing_the_saved_level_is_a_no_op() {
    let home = tempfile::tempdir().unwrap();
    let mut picker = SecurityLevelPicker::new(
        &context(home.path(), ChosenLevel::Permissive),
        current(),
        /*preflight_input*/ None,
        RuntimeKeymap::defaults().list,
    );
    picker.handle_key_event(key(KeyCode::Enter));
    assert_eq!(
        (picker.screen.clone(), files(home.path())),
        (
            Screen::List {
                note: Some("Permissive is already in effect. Nothing changed.".to_string())
            },
            Vec::<String>::new()
        )
    );
}

/// A review taller than the pane scrolls with the move keys, says so in the
/// footer, and stops at its last line (narrow or short terminals).
#[test]
fn tall_review_scrolls_to_its_last_line() {
    let home = tempfile::tempdir().unwrap();
    let mut picker = SecurityLevelPicker::new(
        &context(home.path(), ChosenLevel::Permissive),
        current(),
        /*preflight_input*/ None,
        RuntimeKeymap::defaults().list,
    );
    picker.handle_key_event(key(KeyCode::Up));
    picker.handle_key_event(key(KeyCode::Enter));
    let line_count = picker.lines(/*width*/ 80).len();
    let body_height = 12;
    let max = (line_count - usize::from(body_height)) as u16;
    assert_eq!(picker.scroll_for(line_count, body_height), 0);
    for _ in 0..line_count * 2 {
        picker.handle_key_event(key(KeyCode::Down));
    }
    assert_eq!(picker.scroll_for(line_count, body_height), max);
    let mut visible = picker.lines(/*width*/ 80).split_off(usize::from(max));
    visible.push(Line::from(picker.footer()));
    let area = Rect::new(0, 0, 80, visible.len() as u16);
    let mut buffer = Buffer::empty(area);
    Paragraph::new(visible).render(area, &mut buffer);
    let rendered = (0..area.height)
        .map(|y| {
            (0..area.width)
                .map(|x| buffer[(x, y)].symbol())
                .collect::<String>()
                .trim_end()
                .to_string()
        })
        .collect::<Vec<_>>()
        .join("\n");
    insta::assert_snapshot!("security_level_picker_review_scrolled_to_end", rendered);

    picker.handle_key_event(key(KeyCode::Up));
    assert_eq!(
        (picker.scroll_for(line_count, body_height), &picker.screen),
        (max - 1, &Screen::Review(ChosenLevel::Aggressive))
    );
}

/// A saved Aggressive reopens its review so only a person in `/security` can
/// switch nested agent launches between refuse and pass.
#[test]
fn nested_agents_switch_from_the_aggressive_review() {
    let home = tempfile::tempdir().unwrap();
    level::save(home.path(), ChosenLevel::Aggressive, NestedAgents::Refuse).unwrap();
    let mut picker = SecurityLevelPicker::new(
        &context(home.path(), ChosenLevel::Aggressive),
        current(),
        /*preflight_input*/ None,
        RuntimeKeymap::defaults().list,
    );
    picker.handle_key_event(key(KeyCode::Enter));
    assert_eq!(picker.screen, Screen::Review(ChosenLevel::Aggressive));
    picker.handle_key_event(key(KeyCode::Enter));
    assert_eq!(
        picker.screen,
        Screen::List {
            note: Some("Aggressive is already saved. Nothing changed.".to_string())
        }
    );

    picker.handle_key_event(key(KeyCode::Enter));
    picker.handle_key_event(key(KeyCode::Char('n')));
    let review = render(&picker, /*width*/ 80);
    assert!(
        review.contains("Nested agents: pass.") && review.contains("n refuse/pass"),
        "{review}"
    );
    picker.handle_key_event(key(KeyCode::Enter));
    insta::assert_snapshot!(
        "security_level_picker_saved_nested_pass",
        render(&picker, /*width*/ 80)
    );
    assert_eq!(
        level::load_state(home.path()),
        (
            StoredLevel::Chosen(ChosenLevel::Aggressive),
            NestedAgents::Pass
        )
    );
}

mod pf_29_s01 {
    use super::*;
    use crate::legacy_core::protected_preflight::InventorySources;
    use crate::legacy_core::protected_preflight::ReadinessFlags;
    use pretty_assertions::assert_eq;

    struct Machine {
        root: tempfile::TempDir,
    }

    impl Machine {
        fn new() -> Self {
            let root = tempfile::tempdir().unwrap();
            for dir in ["corbanu", "home", "work"] {
                std::fs::create_dir_all(root.path().join(dir)).unwrap();
            }
            Self { root }
        }

        fn corbanu(&self) -> std::path::PathBuf {
            self.root.path().join("corbanu")
        }

        fn home(&self) -> std::path::PathBuf {
            self.root.path().join("home")
        }

        fn picker(&self, flags_on: bool, active: ChosenLevel) -> SecurityLevelPicker {
            let input = PreflightInput {
                sources: InventorySources {
                    codex_home: self.corbanu(),
                    home: Some(self.home()),
                    cwd: self.root.path().join("work"),
                    env: Vec::new(),
                    config_layers: Vec::new(),
                },
                flags: ReadinessFlags {
                    secretless_launch: flags_on,
                    credential_broker: flags_on,
                    output_gate: flags_on,
                    untrusted_content: flags_on,
                },
            };
            let mut context = context(&self.corbanu(), active);
            context.preflight_enabled = true;
            SecurityLevelPicker::new(
                &context,
                current(),
                Some(input),
                RuntimeKeymap::defaults().list,
            )
        }
    }

    fn review_aggressive(picker: &mut SecurityLevelPicker) {
        picker.handle_key_event(key(KeyCode::Up));
        picker.handle_key_event(key(KeyCode::Enter));
        assert_eq!(picker.screen, Screen::Review(ChosenLevel::Aggressive));
    }

    fn text(picker: &SecurityLevelPicker) -> String {
        render(picker, /*width*/ 400)
    }

    #[test]
    fn pf_29_s01_blockers_keep_aggressive_unsaved() {
        let machine = Machine::new();
        let mut picker = machine.picker(/*flags_on*/ false, ChosenLevel::Permissive);
        review_aggressive(&mut picker);
        let review = text(&picker);
        assert!(review.contains("Preflight blocked"), "{review}");
        assert!(review.contains("turn on the `secretless_agent_launch` feature"));
        assert!(!review.contains("confirm and save"));

        picker.handle_key_event(key(KeyCode::Enter));
        assert_eq!(picker.screen, Screen::Review(ChosenLevel::Aggressive));
        assert!(text(&picker).contains("Not saved: resolve the blockers above first."));
        assert_eq!(files(&machine.corbanu()), Vec::<String>::new());

        picker.handle_key_event(key(KeyCode::Esc));
        assert_eq!(files(&machine.corbanu()), Vec::<String>::new());
    }

    #[test]
    #[cfg_attr(
        windows,
        ignore = "PF-27-S06: protected launch is not available on Windows yet, so readiness is never clean (#691)"
    )]
    fn pf_29_s01_clean_preflight_saves_level_and_receipt() {
        let machine = Machine::new();
        std::fs::write(machine.home().join(".netrc"), "machine x password fake").unwrap();
        let mut picker = machine.picker(/*flags_on*/ true, ChosenLevel::Permissive);
        review_aggressive(&mut picker);
        let review = text(&picker);
        assert!(review.contains("Preflight passed"), "{review}");
        assert!(
            review.contains("Credential files denied to agent commands after restart: ~/.netrc")
        );
        assert!(!review.contains("fake"));

        picker.handle_key_event(key(KeyCode::Enter));
        assert_eq!(saved_level(&picker), Some(ChosenLevel::Aggressive));
        assert_eq!(
            level::load(&machine.corbanu()),
            StoredLevel::Chosen(ChosenLevel::Aggressive)
        );
        let receipt = preflight::load_receipt(&machine.corbanu())
            .unwrap()
            .unwrap();
        assert_eq!(receipt.findings.len(), 1);
        assert!(
            !std::fs::read_to_string(preflight::receipt_path(&machine.corbanu()))
                .unwrap()
                .contains("netrc"),
            "receipt holds IDs, not paths"
        );
    }

    #[test]
    #[cfg_attr(
        windows,
        ignore = "PF-27-S06: protected launch is not available on Windows yet, so readiness is never clean (#691)"
    )]
    fn pf_29_s01_drift_after_review_refuses_save() {
        let machine = Machine::new();
        std::fs::write(machine.home().join(".netrc"), "one").unwrap();
        let mut picker = machine.picker(/*flags_on*/ true, ChosenLevel::Permissive);
        review_aggressive(&mut picker);
        std::fs::write(machine.home().join(".netrc"), "two").unwrap();
        picker.handle_key_event(key(KeyCode::Enter));
        assert_eq!(picker.screen, Screen::Review(ChosenLevel::Aggressive));
        assert!(text(&picker).contains("Not saved: 1 item changed since you reviewed this."));
        assert_eq!(level::load(&machine.corbanu()), StoredLevel::Absent);

        // The refreshed review can now be confirmed.
        picker.handle_key_event(key(KeyCode::Enter));
        assert_eq!(saved_level(&picker), Some(ChosenLevel::Aggressive));
    }

    /// #802: Corbanu's own start-up writes to its databases and history
    /// between the review and the confirmation do not refuse the save.
    #[test]
    #[cfg_attr(
        windows,
        ignore = "PF-27-S06: protected launch is not available on Windows yet, so readiness is never clean (#691)"
    )]
    fn pf_29_s01_corbanu_own_writes_after_review_still_save() {
        let machine = Machine::new();
        let corbanu = machine.corbanu();
        std::fs::write(corbanu.join("pfterminal_logs_2.sqlite"), "one").unwrap();
        let mut picker = machine.picker(/*flags_on*/ true, ChosenLevel::Permissive);
        review_aggressive(&mut picker);
        std::fs::write(corbanu.join("pfterminal_logs_2.sqlite"), "one, two").unwrap();
        std::fs::write(corbanu.join("pfterminal_state_5.sqlite"), "state").unwrap();
        std::fs::create_dir_all(corbanu.join("sessions/2026")).unwrap();
        picker.handle_key_event(key(KeyCode::Enter));
        assert_eq!(
            saved_level(&picker),
            Some(ChosenLevel::Aggressive),
            "{}",
            text(&picker)
        );
    }

    /// #1428: a symlink placed at a database's `-wal` file between the
    /// review and the confirmation refuses the save, while Corbanu's own
    /// writes to that file do not.
    #[cfg(unix)]
    #[test]
    fn pf_29_s01_symlinked_database_side_file_after_review_refuses_save() {
        let machine = Machine::new();
        let corbanu = machine.corbanu();
        std::fs::write(corbanu.join("pfterminal_state_5.sqlite"), "pages").unwrap();
        std::fs::write(corbanu.join("pfterminal_state_5.sqlite-wal"), "one").unwrap();
        std::fs::write(machine.home().join("secret.txt"), "TOKEN=1").unwrap();
        let mut picker = machine.picker(/*flags_on*/ true, ChosenLevel::Permissive);
        review_aggressive(&mut picker);
        std::fs::write(corbanu.join("pfterminal_state_5.sqlite-wal"), "one, two").unwrap();
        std::os::unix::fs::symlink(
            machine.home().join("secret.txt"),
            corbanu.join("pfterminal_state_5.sqlite-shm"),
        )
        .unwrap();
        picker.handle_key_event(key(KeyCode::Enter));
        assert_eq!(picker.screen, Screen::Review(ChosenLevel::Aggressive));
        assert!(
            text(&picker).contains("Not saved: 1 item changed since you reviewed this."),
            "{}",
            text(&picker)
        );
        assert_eq!(level::load(&machine.corbanu()), StoredLevel::Absent);
    }

    /// Toggling nested agents on a saved Aggressive skips the preflight only
    /// while a receipt is on record (review 3).
    #[test]
    #[cfg_attr(
        windows,
        ignore = "PF-27-S06: protected launch is not available on Windows yet, so readiness is never clean (#691)"
    )]
    fn pf_29_s01_saved_aggressive_without_receipt_needs_a_preflight() {
        let machine = Machine::new();
        level::save(
            &machine.corbanu(),
            ChosenLevel::Aggressive,
            NestedAgents::Refuse,
        )
        .unwrap();
        let mut picker = machine.picker(/*flags_on*/ true, ChosenLevel::Aggressive);
        picker.handle_key_event(key(KeyCode::Enter));
        assert_eq!(picker.screen, Screen::Review(ChosenLevel::Aggressive));
        // PF-24-S02: Core's record was never confirmed either, so the review
        // runs the preflight before anything can be saved.
        assert!(text(&picker).contains("Preflight passed"));
        picker.handle_key_event(key(KeyCode::Char('n')));
        assert!(!preflight::receipt_path(&machine.corbanu()).exists());

        picker.handle_key_event(key(KeyCode::Enter));
        assert_eq!(saved_level(&picker), Some(ChosenLevel::Aggressive));
        assert!(preflight::receipt_path(&machine.corbanu()).exists());
    }

    #[test]
    #[cfg_attr(
        windows,
        ignore = "PF-27-S06: protected launch is not available on Windows yet, so readiness is never clean (#691)"
    )]
    fn pf_29_s01_returning_to_permissive_removes_the_receipt() {
        let machine = Machine::new();
        let mut picker = machine.picker(/*flags_on*/ true, ChosenLevel::Permissive);
        review_aggressive(&mut picker);
        picker.handle_key_event(key(KeyCode::Enter));
        assert!(preflight::receipt_path(&machine.corbanu()).exists());

        let mut picker = machine.picker(/*flags_on*/ true, ChosenLevel::Aggressive);
        picker.handle_key_event(key(KeyCode::Down));
        picker.handle_key_event(key(KeyCode::Enter));
        assert_eq!(picker.screen, Screen::Review(ChosenLevel::Permissive));
        picker.handle_key_event(key(KeyCode::Enter));
        assert_eq!(saved_level(&picker), Some(ChosenLevel::Permissive));
        assert!(!preflight::receipt_path(&machine.corbanu()).exists());
    }
}

mod pf_29_s02 {
    use std::cell::Cell;
    use std::cell::RefCell;
    use std::collections::BTreeMap;

    use pretty_assertions::assert_eq;
    use zeroize::Zeroizing;

    use super::*;
    use crate::legacy_core::protected_preflight::InventorySources;
    use crate::legacy_core::protected_preflight::ReadinessFlags;
    use crate::legacy_core::protected_preflight::migration::journal_path;

    const VALUE: &str = "fake-openai-value-0001";

    #[derive(Default)]
    struct FakeStore {
        values: RefCell<BTreeMap<String, String>>,
        locked: Cell<bool>,
    }

    impl CredentialStore for FakeStore {
        fn put(&self, label: &str, _name: &str, _origin: &str, value: &str) -> Result<(), String> {
            if self.locked.get() {
                return Err("the vault is locked".to_string());
            }
            self.values
                .borrow_mut()
                .insert(label.to_string(), value.to_string());
            Ok(())
        }

        fn get(&self, label: &str) -> Result<Option<Zeroizing<String>>, String> {
            Ok(self.values.borrow().get(label).cloned().map(Zeroizing::new))
        }
    }

    struct Machine {
        root: tempfile::TempDir,
        store: Rc<FakeStore>,
    }

    impl Machine {
        fn new() -> Self {
            let root = tempfile::tempdir().unwrap();
            for dir in ["corbanu", "home", "work"] {
                std::fs::create_dir_all(root.path().join(dir)).unwrap();
            }
            std::fs::write(
                root.path().join("home/.zshrc"),
                format!("alias ll='ls -l'\nexport OPENAI_API_KEY={VALUE}\n"),
            )
            .unwrap();
            Self {
                root,
                store: Rc::new(FakeStore::default()),
            }
        }

        fn path(&self, name: &str) -> std::path::PathBuf {
            self.root.path().join(name)
        }

        fn zshrc(&self) -> String {
            std::fs::read_to_string(self.path("home/.zshrc")).unwrap()
        }

        fn picker(&self) -> SecurityLevelPicker {
            let input = PreflightInput {
                sources: InventorySources {
                    codex_home: self.path("corbanu"),
                    home: Some(self.path("home")),
                    cwd: self.path("work"),
                    env: Vec::new(),
                    config_layers: Vec::new(),
                },
                flags: ReadinessFlags {
                    secretless_launch: true,
                    credential_broker: true,
                    output_gate: true,
                    untrusted_content: true,
                },
            };
            let mut context = context(&self.path("corbanu"), ChosenLevel::Permissive);
            context.preflight_enabled = true;
            let mut picker = SecurityLevelPicker::new(
                &context,
                current(),
                Some(input),
                RuntimeKeymap::defaults().list,
            );
            picker.store = Some(self.store.clone());
            picker.handle_key_event(key(KeyCode::Up));
            picker.handle_key_event(key(KeyCode::Enter));
            assert_eq!(picker.screen, Screen::Review(ChosenLevel::Aggressive));
            picker
        }
    }

    fn text(picker: &SecurityLevelPicker) -> String {
        render(picker, /*width*/ 400)
    }

    #[test]
    fn pf_29_s02_preview_then_esc_changes_nothing() {
        let machine = Machine::new();
        let before = machine.zshrc();
        let mut picker = machine.picker();
        assert!(picker.footer().contains("m move credentials to the vault"));
        picker.handle_key_event(key(KeyCode::Char('m')));
        assert_eq!(picker.screen, Screen::MigrationPreview);
        let preview = text(&picker);
        assert!(
            preview.contains("~/.zshrc:2 OPENAI_API_KEY → vault label migrated/openai_api_key")
        );
        assert!(preview.contains("readable only by you (0600)"));
        assert!(preview.contains("Rotate every value"));
        assert!(!preview.contains(VALUE));

        picker.handle_key_event(key(KeyCode::Esc));
        assert_eq!(picker.screen, Screen::Review(ChosenLevel::Aggressive));
        assert!(text(&picker).contains("Migration cancelled. Nothing changed."));
        assert_eq!(machine.zshrc(), before);
        assert!(machine.store.values.borrow().is_empty());
        assert!(!journal_path(&machine.path("corbanu")).exists());
    }

    #[test]
    #[cfg_attr(
        windows,
        ignore = "PF-27-S06: protected launch is not available on Windows yet, so readiness is never clean (#691)"
    )]
    fn pf_29_s02_confirm_moves_reaudits_and_then_saves() {
        let machine = Machine::new();
        let mut picker = machine.picker();
        picker.handle_key_event(key(KeyCode::Char('m')));
        picker.handle_key_event(key(KeyCode::Enter));
        let result = text(&picker);
        assert!(
            result.contains("Moved 1 credential into the vault"),
            "{result}"
        );
        assert!(result.contains("Rotate at the provider: OPENAI_API_KEY"));
        assert!(!result.contains(VALUE));
        assert_eq!(
            machine.zshrc(),
            "alias ll='ls -l'\nexport OPENAI_API_KEY=\"$(corbanu vault auth-helper migrated/openai_api_key)\"\n"
        );

        picker.handle_key_event(key(KeyCode::Enter));
        let review = text(&picker);
        assert!(review.contains("Preflight passed"), "{review}");
        assert!(review.contains("Moved 1 credential into the vault. Rotate OPENAI_API_KEY"));
        picker.handle_key_event(key(KeyCode::Enter));
        assert_eq!(saved_level(&picker), Some(ChosenLevel::Aggressive));
        assert!(preflight::receipt_path(&machine.path("corbanu")).exists());
    }

    #[test]
    #[cfg_attr(
        windows,
        ignore = "PF-27-S06: protected launch is not available on Windows yet, so readiness is never clean (#691)"
    )]
    fn pf_29_s02_failure_locks_aggressive_until_recovery() {
        let machine = Machine::new();
        machine.store.locked.set(true);
        let mut picker = machine.picker();
        picker.handle_key_event(key(KeyCode::Char('m')));
        picker.handle_key_event(key(KeyCode::Enter));
        assert!(text(&picker).contains("Migration stopped"));
        picker.handle_key_event(key(KeyCode::Enter));
        let review = text(&picker);
        assert!(
            review.contains("a credential migration did not finish"),
            "{review}"
        );
        assert!(picker.footer().contains("r finish the migration"));
        // Saving stays refused while the migration is unfinished.
        picker.handle_key_event(key(KeyCode::Enter));
        assert_eq!(picker.screen, Screen::Review(ChosenLevel::Aggressive));
        assert_eq!(level::load(&machine.path("corbanu")), StoredLevel::Absent);

        machine.store.locked.set(false);
        picker.handle_key_event(key(KeyCode::Char('r')));
        assert!(text(&picker).contains("Moved 1 credential into the vault"));
        picker.handle_key_event(key(KeyCode::Enter));
        assert!(text(&picker).contains("Preflight passed"));
        assert!(!journal_path(&machine.path("corbanu")).exists());
    }

    #[test]
    fn pf_29_s02_change_after_preview_moves_nothing() {
        let machine = Machine::new();
        let mut picker = machine.picker();
        picker.handle_key_event(key(KeyCode::Char('m')));
        std::fs::write(
            machine.path("home/.zshrc"),
            "export OPENAI_API_KEY=fake-other-value-0002\n",
        )
        .unwrap();
        picker.handle_key_event(key(KeyCode::Enter));
        assert_eq!(picker.screen, Screen::Review(ChosenLevel::Aggressive));
        assert!(text(&picker).contains("Not moved: something changed since the preview"));
        assert!(machine.store.values.borrow().is_empty());
        assert_eq!(
            machine.zshrc(),
            "export OPENAI_API_KEY=fake-other-value-0002\n"
        );
    }
}

/// PF-24-S02: confirm, cancel, downgrade, failure, restart, unknown state.
mod pf_24_s02 {
    use super::*;
    use crate::app_event::AppEvent;
    use crate::app_event_sender::AppEventSender;
    use crate::legacy_core::protected_preflight::InventorySources;
    use crate::legacy_core::protected_preflight::ReadinessFlags;
    use pretty_assertions::assert_eq;
    use tokio::sync::mpsc::UnboundedReceiver;
    use tokio::sync::mpsc::unbounded_channel;

    struct Machine {
        root: tempfile::TempDir,
    }

    impl Machine {
        fn new() -> Self {
            let root = tempfile::tempdir().unwrap();
            for dir in ["corbanu", "home", "work"] {
                std::fs::create_dir_all(root.path().join(dir)).unwrap();
            }
            Self { root }
        }

        fn corbanu(&self) -> std::path::PathBuf {
            self.root.path().join("corbanu")
        }

        fn picker(
            &self,
            active: ChosenLevel,
        ) -> (SecurityLevelPicker, UnboundedReceiver<AppEvent>) {
            let input = PreflightInput {
                sources: InventorySources {
                    codex_home: self.corbanu(),
                    home: Some(self.root.path().join("home")),
                    cwd: self.root.path().join("work"),
                    env: Vec::new(),
                    config_layers: Vec::new(),
                },
                flags: ReadinessFlags {
                    secretless_launch: true,
                    credential_broker: true,
                    output_gate: true,
                    untrusted_content: true,
                },
            };
            let mut context = context(&self.corbanu(), active);
            context.preflight_enabled = true;
            let mut picker = SecurityLevelPicker::new(
                &context,
                current(),
                Some(input),
                RuntimeKeymap::defaults().list,
            );
            let (tx, rx) = unbounded_channel();
            picker.set_app_event_tx(AppEventSender::new(tx));
            (picker, rx)
        }

        fn text(&self, picker: &SecurityLevelPicker) -> String {
            render(picker, /*width*/ 400).replace(&self.root.path().display().to_string(), "<root>")
        }

        fn stored(&self) -> (StoredLevel, StoredSecurityState) {
            (
                level::load(&self.corbanu()),
                StoredSecurityState::load(&self.corbanu()),
            )
        }

        /// Aggressive saved and confirmed with Core, as by a previous run.
        fn confirmed_aggressive(&self) {
            let (mut picker, _rx) = self.picker(ChosenLevel::Permissive);
            picker.handle_key_event(key(KeyCode::Up));
            picker.handle_key_event(key(KeyCode::Enter));
            picker.handle_key_event(key(KeyCode::Enter));
            assert_eq!(saved_level(&picker), Some(ChosenLevel::Aggressive));
        }
    }

    #[test]
    #[cfg_attr(
        windows,
        ignore = "PF-27-S06: protected launch is not available on Windows yet, so the preflight never passes and Aggressive is never confirmed with Core (#691)"
    )]
    fn security_confirm_aggressive_raises_core_and_offers_restart() {
        let machine = Machine::new();
        let (mut picker, mut rx) = machine.picker(ChosenLevel::Permissive);
        picker.handle_key_event(key(KeyCode::Up));
        picker.handle_key_event(key(KeyCode::Enter));
        let review = machine.text(&picker);
        // #1015: no session has started in this window yet; it starts under
        // the saved level.
        assert!(
            review.contains("Core's level is saved as Aggressive as soon as you confirm"),
            "{review}"
        );
        picker.handle_key_event(key(KeyCode::Enter));
        insta::assert_snapshot!(
            "security_confirm_saved_aggressive_core",
            render(&picker, /*width*/ 80)
        );
        assert_eq!(
            machine.stored(),
            (
                StoredLevel::Chosen(ChosenLevel::Aggressive),
                StoredSecurityState::Level(SecurityLevel::Aggressive)
            )
        );
        picker.handle_key_event(key(KeyCode::Char('r')));
        assert!(picker.closed);
        assert!(matches!(
            rx.try_recv(),
            Ok(AppEvent::RestartForSecurityLevel)
        ));
    }

    /// #739: with protected launch on, the move from Aggressive to
    /// Permissive does not list Corbanu's own files as no longer denied: the
    /// launch contract keeps them unreadable.
    #[test]
    #[cfg_attr(
        windows,
        ignore = "PF-27-S06: protected launch is not available on Windows yet, so the preflight never passes and Aggressive is never confirmed with Core (#691)"
    )]
    fn issue_739_downgrade_under_protected_launch_keeps_corbanu_files_denied() {
        let machine = Machine::new();
        machine.confirmed_aggressive();
        for (protected, kept, removed) in [(true, true, false), (false, false, true)] {
            let (mut picker, _rx) = machine.picker(ChosenLevel::Aggressive);
            picker.set_moderate_facts(crate::security::moderate::Facts {
                secretless_launch: protected,
                ..Default::default()
            });
            picker.handle_key_event(key(KeyCode::Down));
            picker.handle_key_event(key(KeyCode::Enter));
            let review = machine.text(&picker);
            assert_eq!(
                review.contains("Corbanu's sign-in, vault, config and history, and a few CLI token files (such as .netrc, .aws/credentials and .npmrc), stay unreadable to them (secretless_agent_launch)"),
                kept,
                "{review}"
            );
            assert_eq!(
                review.contains("Corbanu's history and state are no longer denied"),
                removed,
                "{review}"
            );
            picker.handle_key_event(key(KeyCode::Esc));
        }
    }

    #[test]
    #[cfg_attr(
        windows,
        ignore = "PF-27-S06: protected launch is not available on Windows yet, so the preflight never passes and Aggressive is never confirmed with Core (#691)"
    )]
    fn security_confirm_downgrade_shows_removed_protections_first() {
        let machine = Machine::new();
        machine.confirmed_aggressive();
        let (mut picker, _rx) = machine.picker(ChosenLevel::Aggressive);
        picker.handle_key_event(key(KeyCode::Down));
        picker.handle_key_event(key(KeyCode::Enter));
        insta::assert_snapshot!(
            "security_confirm_review_downgrade",
            render(&picker, /*width*/ 80)
        );
        // Esc restores the prior state: nothing was written.
        picker.handle_key_event(key(KeyCode::Esc));
        assert_eq!(
            machine.stored(),
            (
                StoredLevel::Chosen(ChosenLevel::Aggressive),
                StoredSecurityState::Level(SecurityLevel::Aggressive)
            )
        );
        picker.handle_key_event(key(KeyCode::Enter));
        picker.handle_key_event(key(KeyCode::Enter));
        assert_eq!(saved_level(&picker), Some(ChosenLevel::Permissive));
        assert!(
            machine
                .text(&picker)
                .contains("Core's level is Permissive from the next start")
        );
        assert_eq!(
            machine.stored(),
            (
                StoredLevel::Chosen(ChosenLevel::Permissive),
                StoredSecurityState::Level(SecurityLevel::Permissive)
            )
        );

        // Reopened before restarting: the list offers the restart too.
        let (mut picker, mut rx) = machine.picker(ChosenLevel::Aggressive);
        assert!(
            picker.footer().contains("r restart now"),
            "{}",
            picker.footer()
        );
        picker.handle_key_event(key(KeyCode::Char('r')));
        assert!(picker.closed);
        assert!(matches!(
            rx.try_recv(),
            Ok(AppEvent::RestartForSecurityLevel)
        ));
    }

    /// #436 (AGG-26): back to Permissive from an Aggressive session whose
    /// provider key came from the environment. The broker holds the key and
    /// the restarted process would not get it, so "restart now" is not
    /// offered; the review lists every Aggressive protection.
    #[test]
    #[cfg_attr(
        windows,
        ignore = "PF-27-S06: protected launch is not available on Windows yet, so the preflight never passes and Aggressive is never confirmed with Core (#691)"
    )]
    fn security_confirm_downgrade_never_restarts_without_the_environment_key() {
        let machine = Machine::new();
        machine.confirmed_aggressive();
        let (mut picker, mut rx) = machine.picker(ChosenLevel::Aggressive);
        picker.restart_blocked =
            crate::security::restart::restart_blocker_for(&["ZAI_API_KEY".to_string()]);
        picker.handle_key_event(key(KeyCode::Down));
        picker.handle_key_event(key(KeyCode::Enter));
        let review = machine.text(&picker);
        for removed in ["• Nested agents:", "• Protected paths:"] {
            assert!(review.contains(removed), "{removed} missing: {review}");
        }
        picker.handle_key_event(key(KeyCode::Enter));
        assert_eq!(saved_level(&picker), Some(ChosenLevel::Permissive));
        let saved = machine.text(&picker);
        assert!(
            saved.contains("Restart now is not available: this session removed ZAI_API_KEY"),
            "{saved}"
        );
        assert!(!saved.contains("Press r to restart now"), "{saved}");
        assert!(!picker.footer().contains("restart"), "{}", picker.footer());
        picker.handle_key_event(key(KeyCode::Char('r')));
        assert!(rx.try_recv().is_err());
        assert!(!picker.closed);
    }

    #[test]
    #[cfg_attr(
        windows,
        ignore = "PF-27-S06: protected launch is not available on Windows yet, so the preflight never passes and Aggressive is never confirmed with Core (#691)"
    )]
    fn security_confirm_write_failure_keeps_the_view_open() {
        let machine = Machine::new();
        machine.confirmed_aggressive();
        let before = machine.stored();
        std::fs::remove_file(machine.corbanu().join("security_state.lock")).unwrap();
        std::fs::create_dir(machine.corbanu().join("security_state.lock")).unwrap();
        let (mut picker, _rx) = machine.picker(ChosenLevel::Aggressive);
        picker.handle_key_event(key(KeyCode::Down));
        picker.handle_key_event(key(KeyCode::Enter));
        picker.handle_key_event(key(KeyCode::Enter));
        let failed = machine.text(&picker);
        assert!(failed.contains("Security level not saved"), "{failed}");
        assert!(failed.contains("Nothing changed."), "{failed}");
        assert!(
            failed.contains("Active in this session: Aggressive."),
            "{failed}"
        );
        assert!(!picker.closed);
        assert_eq!(machine.stored(), before);
        // Enter goes back to the review to try again; it works once the
        // store can be written.
        picker.handle_key_event(key(KeyCode::Enter));
        assert_eq!(picker.screen, Screen::Review(ChosenLevel::Permissive));
        std::fs::remove_dir(machine.corbanu().join("security_state.lock")).unwrap();
        picker.handle_key_event(key(KeyCode::Enter));
        assert_eq!(saved_level(&picker), Some(ChosenLevel::Permissive));
    }

    #[test]
    #[cfg_attr(
        windows,
        ignore = "PF-27-S06: protected launch is not available on Windows yet, so the preflight never passes and Aggressive is never confirmed with Core (#691)"
    )]
    fn security_confirm_state_changed_elsewhere_reviews_again() {
        let machine = Machine::new();
        let (mut picker, _rx) = machine.picker(ChosenLevel::Permissive);
        picker.handle_key_event(key(KeyCode::Up));
        picker.handle_key_event(key(KeyCode::Enter));
        // Another session confirms Aggressive while this review is open.
        machine.confirmed_aggressive();
        picker.handle_key_event(key(KeyCode::Enter));
        assert!(
            machine
                .text(&picker)
                .contains("the security state changed since you reviewed it"),
            "{}",
            machine.text(&picker)
        );
        picker.handle_key_event(key(KeyCode::Enter));
        assert_eq!(picker.screen, Screen::Review(ChosenLevel::Aggressive));
        assert!(
            machine
                .text(&picker)
                .contains("The state changed; this review shows it now.")
        );
    }

    /// An agent command (or anything else) that rewrites the level file to
    /// Permissive outside `/security` does not lower the next start.
    #[test]
    #[cfg_attr(
        windows,
        ignore = "PF-27-S06: protected launch is not available on Windows yet, so the preflight never passes and Aggressive is never confirmed with Core (#691)"
    )]
    fn security_confirm_level_file_edited_by_an_agent_is_caught() {
        let machine = Machine::new();
        machine.confirmed_aggressive();
        std::fs::write(
            level::state_path(&machine.corbanu()),
            "version = 1\nlevel = \"permissive\"\n",
        )
        .unwrap();
        assert_eq!(
            level::load(&machine.corbanu()).enforced(),
            ChosenLevel::Aggressive
        );
        let (picker, _rx) = machine.picker(ChosenLevel::Aggressive);
        let list = machine.text(&picker);
        // The message embeds the temp path, so where it wraps (at a space or
        // after a '/') depends on TMPDIR length; match with whitespace removed.
        let unwrapped: String = list.split_whitespace().collect();
        assert!(
            unwrapped.contains("mayhavebeenchangedoutside/security"),
            "{list}"
        );
    }

    #[test]
    fn security_confirm_unknown_core_state_is_shown_and_repaired() {
        let machine = Machine::new();
        std::fs::write(machine.corbanu().join("security_state.json"), "{").unwrap();
        let (mut picker, _rx) = machine.picker(ChosenLevel::Aggressive);
        let list = machine.text(&picker);
        assert!(
            list.contains("Core's security state is unreadable"),
            "{list}"
        );
        picker.handle_key_event(key(KeyCode::Down));
        picker.handle_key_event(key(KeyCode::Enter));
        assert_eq!(picker.screen, Screen::Review(ChosenLevel::Permissive));
        picker.handle_key_event(key(KeyCode::Enter));
        assert_eq!(saved_level(&picker), Some(ChosenLevel::Permissive));
        assert_eq!(
            machine.stored(),
            (
                StoredLevel::Chosen(ChosenLevel::Permissive),
                StoredSecurityState::Level(SecurityLevel::Permissive)
            )
        );
    }

    /// The commit runs off the UI thread: the screen says so until `poll`
    /// collects the result, and keys do nothing meanwhile.
    #[test]
    #[cfg_attr(
        windows,
        ignore = "PF-27-S06: protected launch is not available on Windows yet, so the preflight never passes and Aggressive is never confirmed with Core (#691)"
    )]
    fn security_confirm_saving_screen_until_the_commit_finishes() {
        let machine = Machine::new();
        let (mut picker, _rx) = machine.picker(ChosenLevel::Permissive);
        picker.commit_inline = false;
        picker.handle_key_event(key(KeyCode::Up));
        picker.handle_key_event(key(KeyCode::Enter));
        picker.handle_key_event(key(KeyCode::Enter));
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        while picker.saving() {
            picker.handle_key_event(key(KeyCode::Esc));
            assert!(!picker.closed);
            assert!(
                std::time::Instant::now() < deadline,
                "the commit did not finish"
            );
            std::thread::sleep(std::time::Duration::from_millis(5));
            picker.poll();
        }
        assert_eq!(saved_level(&picker), Some(ChosenLevel::Aggressive));
    }
}

/// PF-25-S02: with the kill switch saved on, the list says so and `g` asks
/// for the grants and kill switch view; `k` still moves up.
#[test]
fn pf_25_s02_kill_switch_status_and_g_key() {
    let home = tempfile::TempDir::new().unwrap();
    // The kill switch saved on, as a confirmed /security change saves it.
    {
        use crate::legacy_core::security_level_change::LevelBasis;
        use crate::legacy_core::security_level_change::SecurityLevel;
        use crate::legacy_core::security_revocation::HumanRevocation;
        use crate::legacy_core::security_revocation::commit_human_revocation;
        let basis = LevelBasis::read(home.path(), SecurityLevel::Permissive, /*thread*/ None);
        commit_human_revocation(
            home.path(),
            SecurityLevel::Permissive,
            /*thread*/ None,
            &basis,
            HumanRevocation::KillSwitchOn,
            /*now_unix_seconds*/ 1_000,
        )
        .unwrap();
    }
    let mut picker = SecurityLevelPicker::new(
        &context(home.path(), ChosenLevel::Permissive),
        current(),
        /*preflight_input*/ None,
        RuntimeKeymap::defaults().list,
    );
    let text = render(&picker, /*width*/ 120);
    assert!(text.contains("Kill switch: on"), "{text}");
    assert!(text.contains("g grants and kill switch"), "{text}");
    picker.handle_key_event(key(KeyCode::Char('k')));
    assert!(!picker.open_revocations);
    picker.handle_key_event(key(KeyCode::Char('g')));
    assert!(picker.open_revocations);
}

/// #691 (Travis, 2026-10-14): on Windows without its sandbox set up,
/// Aggressive cannot be chosen: the list says so, Enter explains how to set
/// it up, and nothing is saved.
#[test]
fn issue_691_aggressive_waits_for_the_windows_sandbox() {
    let home = tempfile::tempdir().unwrap();
    let mut picker = SecurityLevelPicker::new(
        &context(home.path(), ChosenLevel::Permissive),
        current(),
        /*preflight_input*/ None,
        RuntimeKeymap::defaults().list,
    );
    picker.set_windows_sandbox_steps(Some(
        "The Windows sandbox is not set up. Open /permissions, choose Default and pick \
         \"Set up default sandbox\" (it needs Administrator permission)"
            .to_string(),
    ));
    picker.handle_key_event(key(KeyCode::Up));
    picker.handle_key_event(key(KeyCode::Enter));
    insta::assert_snapshot!(
        "security_level_picker_aggressive_needs_windows_sandbox",
        render(&picker, /*width*/ 80)
    );
    assert!(matches!(picker.screen, Screen::List { .. }));
    assert_eq!(level::load(home.path()), StoredLevel::Absent);
    assert_eq!(files(home.path()), Vec::<String>::new());

    // Once it is set up, Aggressive's review opens as before.
    picker.set_windows_sandbox_steps(/*steps*/ None);
    picker.handle_key_event(key(KeyCode::Enter));
    assert_eq!(picker.screen, Screen::Review(ChosenLevel::Aggressive));
}

/// #691: with Aggressive saved and active, its review (nested-agent setting)
/// also waits for the Windows sandbox; switching to Permissive still works.
#[test]
fn issue_691_saved_aggressive_waits_for_the_windows_sandbox_and_permissive_stays() {
    let home = tempfile::tempdir().unwrap();
    level::save(home.path(), ChosenLevel::Aggressive, NestedAgents::Refuse).unwrap();
    let mut picker = SecurityLevelPicker::new(
        &context(home.path(), ChosenLevel::Aggressive),
        current(),
        /*preflight_input*/ None,
        RuntimeKeymap::defaults().list,
    );
    picker.set_windows_sandbox_steps(Some("Run /setup-default-sandbox".to_string()));
    assert!(
        render(&picker, /*width*/ 80)
            .contains("> Aggressive  (active), needs the Windows sandbox set up"),
        "{}",
        render(&picker, /*width*/ 80)
    );
    picker.handle_key_event(key(KeyCode::Enter));
    assert_eq!(
        picker.screen,
        Screen::List {
            note: Some(
                "Aggressive needs the default Windows sandbox. Run /setup-default-sandbox, \
                 then choose Aggressive again. Nothing changed."
                    .to_string()
            )
        }
    );
    assert_eq!(
        level::load(home.path()),
        StoredLevel::Chosen(ChosenLevel::Aggressive)
    );
    picker.handle_key_event(key(KeyCode::Down));
    picker.handle_key_event(key(KeyCode::Enter));
    assert_eq!(picker.screen, Screen::Review(ChosenLevel::Permissive));
}
