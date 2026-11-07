use super::*;
use pretty_assertions::assert_eq;
use std::sync::mpsc::Receiver;

#[test]
fn paste_burst_newline_does_not_submit_short_first_line() {
    let now = Instant::now();

    for (first_line, second_line) in [("x", "rest"), ("id", "body"), ("foo", "bar")] {
        let (mut view, submitted_rx) = custom_prompt_view();
        let mut ms = 0;

        for ch in first_line.chars() {
            view.handle_key_event_at(KeyEvent::from(KeyCode::Char(ch)), now + elapsed(ms));
            ms += 1;
        }
        view.handle_key_event_at(KeyEvent::from(KeyCode::Enter), now + elapsed(ms));
        ms += 1;
        for ch in second_line.chars() {
            view.handle_key_event_at(KeyEvent::from(KeyCode::Char(ch)), now + elapsed(ms));
            ms += 1;
        }

        assert!(submitted_rx.try_recv().is_err());
        assert!(!view.is_complete());

        view.handle_key_event_at(KeyEvent::from(KeyCode::Enter), now + elapsed(/*ms*/ 200));

        assert_eq!(
            submitted_rx.try_recv(),
            Ok(format!("{first_line}\n{second_line}"))
        );
        assert!(view.is_complete());
    }
}

#[test]
fn paste_burst_newline_after_tab_does_not_submit() {
    let (mut view, submitted_rx) = custom_prompt_view();
    let now = Instant::now();
    let mut ms = 0;

    view.handle_key_event_at(KeyEvent::from(KeyCode::Char('x')), now + elapsed(ms));
    ms += 1;
    view.handle_key_event_at(KeyEvent::from(KeyCode::Tab), now + elapsed(ms));
    ms += 1;
    view.handle_key_event_at(KeyEvent::from(KeyCode::Enter), now + elapsed(ms));
    ms += 1;
    for ch in "rest".chars() {
        view.handle_key_event_at(KeyEvent::from(KeyCode::Char(ch)), now + elapsed(ms));
        ms += 1;
    }

    assert!(submitted_rx.try_recv().is_err());
    assert!(!view.is_complete());

    view.handle_key_event_at(KeyEvent::from(KeyCode::Enter), now + elapsed(/*ms*/ 200));

    assert_eq!(submitted_rx.try_recv(), Ok("x\nrest".to_string()));
    assert!(view.is_complete());
}

#[test]
fn delayed_enter_after_typing_submits() {
    let (mut view, submitted_rx) = custom_prompt_view();
    let now = Instant::now();

    for (idx, ch) in "foo".chars().enumerate() {
        view.handle_key_event_at(KeyEvent::from(KeyCode::Char(ch)), now + elapsed(idx * 20));
    }
    view.handle_key_event_at(KeyEvent::from(KeyCode::Enter), now + elapsed(/*ms*/ 80));

    assert_eq!(submitted_rx.try_recv(), Ok("foo".to_string()));
    assert!(view.is_complete());
}

#[test]
fn ctrl_d_submit_mode_uses_enter_for_newline() {
    let (mut view, submitted_rx) = custom_prompt_view_ctrl_d();
    let now = Instant::now();

    for (idx, ch) in "foo".chars().enumerate() {
        view.handle_key_event_at(KeyEvent::from(KeyCode::Char(ch)), now + elapsed(idx * 20));
    }
    view.handle_key_event_at(KeyEvent::from(KeyCode::Enter), now + elapsed(/*ms*/ 100));
    for (idx, ch) in "bar".chars().enumerate() {
        view.handle_key_event_at(
            KeyEvent::from(KeyCode::Char(ch)),
            now + elapsed(120 + idx * 20),
        );
    }

    assert!(submitted_rx.try_recv().is_err());
    assert!(!view.is_complete());

    view.handle_key_event_at(
        KeyEvent::new(KeyCode::Char('d'), KeyModifiers::CONTROL),
        now + elapsed(/*ms*/ 240),
    );

    assert_eq!(submitted_rx.try_recv(), Ok("foo\nbar".to_string()));
    assert!(view.is_complete());
}

/// #1289: in a one-line field, Enter right after fast typing submits; it is
/// never taken into the field as a newline.
#[test]
fn single_line_mode_enter_inside_paste_burst_window_submits() {
    let (mut view, submitted_rx, refused_rx) = single_line_view();
    let now = Instant::now();
    let mut ms = 0;

    for ch in "work".chars() {
        view.handle_key_event_at(KeyEvent::from(KeyCode::Char(ch)), now + elapsed(ms));
        ms += 1;
    }
    // Within the burst window, where the multi-line mode inserts a newline.
    view.handle_key_event_at(KeyEvent::from(KeyCode::Enter), now + elapsed(ms));
    assert_eq!(view.textarea.text(), "work");
    assert!(!view.pre_draw_tick(now + elapsed(ms) + SINGLE_LINE_SUBMIT_HOLD / 2));
    assert!(!view.is_complete());
    assert!(view.pre_draw_tick(now + elapsed(ms) + SINGLE_LINE_SUBMIT_HOLD));

    assert_eq!(submitted_rx.try_recv(), Ok("work".to_string()));
    assert!(refused_rx.try_recv().is_err());
    assert!(view.is_complete());
}

/// #1337 review: the editor's newline keys (Ctrl+J, Ctrl+M, a raw line
/// feed, Shift+Enter, Alt+Enter) submit a one-line field like Enter.
#[test]
fn single_line_mode_newline_keys_submit() {
    for key in [
        KeyEvent::new(KeyCode::Char('j'), KeyModifiers::CONTROL),
        KeyEvent::new(KeyCode::Char('m'), KeyModifiers::CONTROL),
        KeyEvent::new(KeyCode::Char('\n'), KeyModifiers::NONE),
        KeyEvent::new(KeyCode::Enter, KeyModifiers::SHIFT),
        KeyEvent::new(KeyCode::Enter, KeyModifiers::ALT),
    ] {
        let (mut view, submitted_rx, refused_rx) = single_line_view();
        let now = Instant::now();
        for (idx, ch) in "work".chars().enumerate() {
            view.handle_key_event_at(KeyEvent::from(KeyCode::Char(ch)), now + elapsed(idx));
        }
        view.handle_key_event_at(key, now + elapsed(/*ms*/ 4));
        assert_eq!(view.textarea.text(), "work", "{key:?}");
        view.pre_draw_tick(now + elapsed(/*ms*/ 4) + SINGLE_LINE_SUBMIT_HOLD);

        assert_eq!(submitted_rx.try_recv(), Ok("work".to_string()), "{key:?}");
        assert!(refused_rx.try_recv().is_err(), "{key:?}");
        assert!(view.is_complete(), "{key:?}");
    }
}

/// #1337 review: input sent together with the submit key (a key after the
/// name) is never shown and never passed on: the field is refused once the
/// input is quiet, and the prompt stays open until then so nothing reaches a
/// view or the composer under it.
#[test]
fn single_line_mode_input_after_submit_key_is_refused_unshown() {
    for submit in [
        KeyEvent::from(KeyCode::Enter),
        KeyEvent::new(KeyCode::Char('j'), KeyModifiers::CONTROL),
    ] {
        let (mut view, submitted_rx, refused_rx) = single_line_view();
        let now = Instant::now();
        let mut ms = 0;
        for ch in "work".chars() {
            view.handle_key_event_at(KeyEvent::from(KeyCode::Char(ch)), now + elapsed(ms));
            ms += 1;
        }
        view.handle_key_event_at(submit, now + elapsed(ms));
        for ch in "sk-fake-0000".chars() {
            ms += 1;
            view.handle_key_event_at(KeyEvent::from(KeyCode::Char(ch)), now + elapsed(ms));
        }
        // The hold runs from the last key, not from the submit key.
        assert!(!view.pre_draw_tick(now + elapsed(/*ms*/ 4) + SINGLE_LINE_SUBMIT_HOLD));
        assert!(!view.is_complete());
        assert_eq!(view.textarea.text(), "work");
        assert!(!render_view(&view).contains("sk-fake"));

        assert!(view.pre_draw_tick(now + elapsed(ms) + SINGLE_LINE_SUBMIT_HOLD));
        assert!(submitted_rx.try_recv().is_err(), "{submit:?}");
        assert_eq!(refused_rx.try_recv(), Ok(()), "{submit:?}");
        assert!(view.is_complete());
    }
}

/// #1337 review (optional item): a paste with more than one line keeps only
/// its first line, so a key-like second line is never shown; the field is
/// then refused. A single trailing line break is just dropped.
#[test]
fn single_line_mode_multi_line_paste_keeps_first_line() {
    let (mut view, submitted_rx, refused_rx) = single_line_view();
    assert!(view.handle_paste("work\nsk-fake-0000".to_string()));
    assert_eq!(view.textarea.text(), "work");
    assert!(!render_view(&view).contains("sk-fake"));
    let now = Instant::now();
    view.handle_key_event_at(KeyEvent::from(KeyCode::Enter), now);
    view.pre_draw_tick(now + SINGLE_LINE_SUBMIT_HOLD);
    assert!(submitted_rx.try_recv().is_err());
    assert_eq!(refused_rx.try_recv(), Ok(()));

    let (mut view, submitted_rx, refused_rx) = single_line_view();
    assert!(view.handle_paste("work\n".to_string()));
    assert_eq!(view.textarea.text(), "work");
    view.handle_key_event_at(KeyEvent::from(KeyCode::Enter), now);
    view.pre_draw_tick(now + SINGLE_LINE_SUBMIT_HOLD);
    assert_eq!(submitted_rx.try_recv(), Ok("work".to_string()));
    assert!(refused_rx.try_recv().is_err());
}

fn single_line_view() -> (CustomPromptView, Receiver<String>, Receiver<()>) {
    let (submitted, submitted_rx) = std::sync::mpsc::channel();
    let (refused, refused_rx) = std::sync::mpsc::channel();
    let view = CustomPromptView::new(
        "Add a Z.AI account".to_string(),
        "Account name".to_string(),
        String::new(),
        /*context_label*/ None,
        Box::new(move |text| {
            submitted.send(text).expect("send submitted text");
        }),
    )
    .with_submit_mode(CustomPromptSubmitMode::EnterSingleLine)
    .with_on_refused(Box::new(move || {
        refused.send(()).expect("send refusal");
    }));
    (view, submitted_rx, refused_rx)
}

fn render_view(view: &CustomPromptView) -> String {
    let width = 60;
    let area = Rect::new(0, 0, width, view.desired_height(width));
    let mut buf = Buffer::empty(area);
    view.render(area, &mut buf);
    (0..area.height)
        .map(|y| {
            (0..area.width)
                .map(|x| buf[(x, y)].symbol().to_string())
                .collect::<String>()
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn custom_prompt_view() -> (CustomPromptView, Receiver<String>) {
    let (submitted, submitted_rx) = std::sync::mpsc::channel();
    let view = CustomPromptView::new(
        "Edit goal".to_string(),
        "Type a goal objective and press Enter".to_string(),
        String::new(),
        /*context_label*/ None,
        Box::new(move |text| {
            submitted.send(text).expect("send submitted text");
        }),
    );
    (view, submitted_rx)
}

fn custom_prompt_view_ctrl_d() -> (CustomPromptView, Receiver<String>) {
    let (submitted, submitted_rx) = std::sync::mpsc::channel();
    let view = CustomPromptView::new(
        "Edit context".to_string(),
        "Type context".to_string(),
        String::new(),
        /*context_label*/ None,
        Box::new(move |text| {
            submitted.send(text).expect("send submitted text");
        }),
    )
    .with_submit_mode(CustomPromptSubmitMode::CtrlD);
    (view, submitted_rx)
}

fn elapsed(ms: usize) -> std::time::Duration {
    std::time::Duration::from_millis(ms as u64)
}
