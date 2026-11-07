use crossterm::event::KeyCode;
use crossterm::event::KeyEvent;
use crossterm::event::KeyModifiers;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::Stylize;
use ratatui::text::Line;
use ratatui::text::Span;
use ratatui::widgets::Clear;
use ratatui::widgets::Paragraph;
use ratatui::widgets::StatefulWidgetRef;
use ratatui::widgets::Widget;
use std::cell::RefCell;
use std::time::Duration;
use std::time::Instant;

use crate::key_hint;
use crate::key_hint::has_ctrl_or_alt;
use crate::render::renderable::Renderable;

use super::popup_consts::standard_popup_hint_line;

use super::CancellationEvent;
use super::bottom_pane_view::BottomPaneView;
use super::bottom_pane_view::ViewCompletion;
use super::paste_burst::PasteBurst;
use super::textarea::TextArea;
use super::textarea::TextAreaState;

/// Callback invoked when the user submits a custom prompt.
pub(crate) type PromptSubmitted = Box<dyn Fn(String) + Send + Sync>;

/// Callback invoked when a one-line field got more than one line.
pub(crate) type PromptRefused = Box<dyn Fn() + Send + Sync>;

/// How long a one-line field waits, after its submit key, for input to be
/// quiet before it submits (the paste-burst Enter window). Input that
/// arrives in that time was sent together with the submit key (a paste, a
/// tmux `send-keys`), so it is held here instead of reaching whatever the
/// submit opens or uncovers.
pub(crate) const SINGLE_LINE_SUBMIT_HOLD: Duration = Duration::from_millis(120);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CustomPromptSubmitMode {
    Enter,
    CtrlD,
    /// A one-line field (a name). Enter and the editor's other newline keys
    /// (Ctrl+J, Ctrl+M, Shift+Enter, Alt+Enter) submit; they never become a
    /// newline. The submit happens once input has been quiet for
    /// [`SINGLE_LINE_SUBMIT_HOLD`]. Input in that time, or a paste with more
    /// than one line, is discarded unshown and the field is refused instead.
    EnterSingleLine,
}

/// A one-line submit waiting for input to be quiet.
#[derive(Debug, Clone, Copy)]
struct HeldSubmit {
    until: Instant,
}

/// Minimal multi-line text input view to collect custom review instructions.
pub(crate) struct CustomPromptView {
    title: String,
    placeholder: String,
    context_label: Option<String>,
    on_submit: PromptSubmitted,
    on_refused: Option<PromptRefused>,
    submit_mode: CustomPromptSubmitMode,

    // UI state
    textarea: TextArea,
    textarea_state: RefCell<TextAreaState>,
    paste_burst: PasteBurst,
    completion: Option<ViewCompletion>,
    /// One-line mode: a submit key was pressed; see [`SINGLE_LINE_SUBMIT_HOLD`].
    held_submit: Option<HeldSubmit>,
    /// One-line mode: the field got more than one line (input after the
    /// submit key, or the rest of a multi-line paste); refuse, never submit.
    got_extra_lines: bool,
}

impl CustomPromptView {
    pub(crate) fn new(
        title: String,
        placeholder: String,
        initial_text: String,
        context_label: Option<String>,
        on_submit: PromptSubmitted,
    ) -> Self {
        let mut textarea = TextArea::new();
        if !initial_text.is_empty() {
            textarea.set_text_clearing_elements(&initial_text);
            textarea.set_cursor(initial_text.len());
        }

        Self {
            title,
            placeholder,
            context_label,
            on_submit,
            on_refused: None,
            submit_mode: CustomPromptSubmitMode::Enter,
            textarea,
            textarea_state: RefCell::new(TextAreaState::default()),
            paste_burst: PasteBurst::default(),
            completion: None,
            held_submit: None,
            got_extra_lines: false,
        }
    }

    pub(crate) fn with_submit_mode(mut self, submit_mode: CustomPromptSubmitMode) -> Self {
        self.submit_mode = submit_mode;
        self
    }

    /// One-line mode: what the field reports when it got more than one line.
    /// Nothing of the extra input is passed on.
    pub(crate) fn with_on_refused(mut self, on_refused: PromptRefused) -> Self {
        self.on_refused = Some(on_refused);
        self
    }

    fn submit_current_text(&mut self) {
        let text = self.textarea.text().trim().to_string();
        if !text.is_empty() {
            (self.on_submit)(text);
            self.completion = Some(ViewCompletion::Accepted);
        }
    }

    /// One-line mode: Enter and the editor's newline keys submit. The
    /// field's own keymap decides which keys those are, so none of them can
    /// insert a newline.
    fn is_single_line_submit_key(&self, key_event: KeyEvent) -> bool {
        key_event.code == KeyCode::Enter || self.textarea.is_insert_newline_key(key_event)
    }

    fn hold_single_line_submit(&mut self, now: Instant) {
        if self.textarea.text().trim().is_empty() && !self.got_extra_lines {
            return;
        }
        self.held_submit = Some(HeldSubmit {
            until: now + SINGLE_LINE_SUBMIT_HOLD,
        });
    }

    /// Input that arrives while a one-line submit is held is discarded
    /// unshown; it makes the submit a refusal and keeps the hold open until
    /// input is quiet again.
    fn discard_input_while_held(&mut self, now: Instant) {
        self.got_extra_lines = true;
        self.held_submit = Some(HeldSubmit {
            until: now + SINGLE_LINE_SUBMIT_HOLD,
        });
    }

    fn finish_held_submit_if_due(&mut self, now: Instant) -> bool {
        let Some(held) = self.held_submit else {
            return false;
        };
        if now < held.until {
            return false;
        }
        self.held_submit = None;
        if self.got_extra_lines {
            if let Some(on_refused) = &self.on_refused {
                on_refused();
            }
            self.completion = Some(ViewCompletion::Accepted);
        } else {
            self.submit_current_text();
        }
        true
    }

    fn handle_key_event_at(&mut self, key_event: KeyEvent, now: Instant) {
        let single_line = self.submit_mode == CustomPromptSubmitMode::EnterSingleLine;
        match key_event {
            KeyEvent {
                code: KeyCode::Esc, ..
            } => {
                self.on_ctrl_c();
            }
            _ if single_line && self.held_submit.is_some() => {
                self.discard_input_while_held(now);
            }
            event
                if self.submit_mode == CustomPromptSubmitMode::CtrlD
                    && key_hint::ctrl(KeyCode::Char('d')).is_press(event) =>
            {
                self.submit_current_text();
            }
            event if single_line && self.is_single_line_submit_key(event) => {
                self.hold_single_line_submit(now);
            }
            KeyEvent {
                code: KeyCode::Enter,
                modifiers,
                ..
            } => {
                if self.paste_burst.direct_insert_newline_should_insert(now) {
                    self.paste_burst.extend_window(now);
                    self.textarea.insert_str("\n");
                    return;
                }
                if self.submit_mode == CustomPromptSubmitMode::CtrlD {
                    self.textarea.insert_str("\n");
                    return;
                }
                if modifiers == KeyModifiers::NONE {
                    self.submit_current_text();
                } else {
                    self.textarea.input(key_event);
                }
            }
            KeyEvent {
                code: KeyCode::Char(_),
                modifiers,
                ..
            } if !has_ctrl_or_alt(modifiers) && self.textarea.allows_paste_burst() => {
                let paste_like_burst = self.paste_burst.on_plain_char_no_hold(now).is_some();
                self.textarea.input(key_event);
                if paste_like_burst {
                    self.paste_burst.extend_window(now);
                }
            }
            KeyEvent {
                code: KeyCode::Tab,
                modifiers,
                ..
            } if !has_ctrl_or_alt(modifiers) && self.textarea.allows_paste_burst() => {
                let in_paste_burst = self.paste_burst.direct_insert_newline_should_insert(now);
                self.textarea.input(key_event);
                if in_paste_burst {
                    self.paste_burst.extend_window(now);
                }
            }
            other => {
                self.textarea.input(other);
                self.paste_burst.clear_after_explicit_paste();
            }
        }
    }
}

impl BottomPaneView for CustomPromptView {
    fn handle_key_event(&mut self, key_event: KeyEvent) {
        self.handle_key_event_at(key_event, Instant::now());
    }

    fn on_ctrl_c(&mut self) -> CancellationEvent {
        self.completion = Some(ViewCompletion::Cancelled);
        CancellationEvent::Handled
    }

    fn is_complete(&self) -> bool {
        self.completion.is_some()
    }

    fn completion(&self) -> Option<ViewCompletion> {
        self.completion
    }

    fn handle_paste(&mut self, pasted: String) -> bool {
        if pasted.is_empty() {
            return false;
        }
        if self.submit_mode == CustomPromptSubmitMode::EnterSingleLine {
            if self.held_submit.is_some() {
                self.discard_input_while_held(Instant::now());
                return false;
            }
            // Only the first line is kept; any other line is discarded
            // unshown, and the field is refused when it is submitted.
            if let Some((first_line, rest)) = pasted.split_once('\n') {
                if !rest.trim().is_empty() {
                    self.got_extra_lines = true;
                }
                self.textarea.insert_str(first_line);
                self.paste_burst.clear_after_explicit_paste();
                return true;
            }
        }
        self.textarea.insert_str(&pasted);
        self.paste_burst.clear_after_explicit_paste();
        true
    }

    fn is_in_paste_burst(&self) -> bool {
        // Keeps the draw loop ticking until the held submit is due.
        self.held_submit
            .is_some_and(|held| Instant::now() < held.until)
    }

    fn pre_draw_tick(&mut self, now: Instant) -> bool {
        self.finish_held_submit_if_due(now)
    }

    fn next_frame_delay(&self) -> Option<Duration> {
        self.held_submit
            .map(|held| held.until.saturating_duration_since(Instant::now()))
    }
}

#[cfg(test)]
#[path = "custom_prompt_view_tests.rs"]
mod tests;

impl Renderable for CustomPromptView {
    fn desired_height(&self, width: u16) -> u16 {
        let extra_top: u16 = if self.context_label.is_some() { 1 } else { 0 };
        1u16 + extra_top + self.input_height(width) + 3u16
    }

    fn render(&self, area: Rect, buf: &mut Buffer) {
        if area.height == 0 || area.width == 0 {
            return;
        }

        let input_height = self.input_height(area.width);

        // Title line
        let title_area = Rect {
            x: area.x,
            y: area.y,
            width: area.width,
            height: 1,
        };
        let title_spans: Vec<Span<'static>> = vec![gutter(), self.title.clone().bold()];
        Paragraph::new(Line::from(title_spans)).render(title_area, buf);

        // Optional context line
        let mut input_y = area.y.saturating_add(1);
        if let Some(context_label) = &self.context_label {
            let context_area = Rect {
                x: area.x,
                y: input_y,
                width: area.width,
                height: 1,
            };
            let spans: Vec<Span<'static>> = vec![gutter(), context_label.clone().cyan()];
            Paragraph::new(Line::from(spans)).render(context_area, buf);
            input_y = input_y.saturating_add(1);
        }

        // Input line
        let input_area = Rect {
            x: area.x,
            y: input_y,
            width: area.width,
            height: input_height,
        };
        if input_area.width >= 2 {
            for row in 0..input_area.height {
                Paragraph::new(Line::from(vec![gutter()])).render(
                    Rect {
                        x: input_area.x,
                        y: input_area.y.saturating_add(row),
                        width: 2,
                        height: 1,
                    },
                    buf,
                );
            }

            let text_area_height = input_area.height.saturating_sub(1);
            if text_area_height > 0 {
                if input_area.width > 2 {
                    let blank_rect = Rect {
                        x: input_area.x.saturating_add(2),
                        y: input_area.y,
                        width: input_area.width.saturating_sub(2),
                        height: 1,
                    };
                    Clear.render(blank_rect, buf);
                }
                let textarea_rect = Rect {
                    x: input_area.x.saturating_add(2),
                    y: input_area.y.saturating_add(1),
                    width: input_area.width.saturating_sub(2),
                    height: text_area_height,
                };
                let mut state = self.textarea_state.borrow_mut();
                StatefulWidgetRef::render_ref(&(&self.textarea), textarea_rect, buf, &mut state);
                if self.textarea.text().is_empty() {
                    Paragraph::new(Line::from(self.placeholder.clone().dim()))
                        .render(textarea_rect, buf);
                }
            }
        }

        let hint_blank_y = input_area.y.saturating_add(input_height);
        if hint_blank_y < area.y.saturating_add(area.height) {
            let blank_area = Rect {
                x: area.x,
                y: hint_blank_y,
                width: area.width,
                height: 1,
            };
            Clear.render(blank_area, buf);
        }

        let hint_y = hint_blank_y.saturating_add(1);
        if hint_y < area.y.saturating_add(area.height) {
            Paragraph::new(self.footer_hint_line()).render(
                Rect {
                    x: area.x,
                    y: hint_y,
                    width: area.width,
                    height: 1,
                },
                buf,
            );
        }
    }

    fn cursor_pos(&self, area: Rect) -> Option<(u16, u16)> {
        if area.height < 2 || area.width <= 2 {
            return None;
        }
        let text_area_height = self.input_height(area.width).saturating_sub(1);
        if text_area_height == 0 {
            return None;
        }
        let extra_offset: u16 = if self.context_label.is_some() { 1 } else { 0 };
        let top_line_count = 1u16 + extra_offset;
        let textarea_rect = Rect {
            x: area.x.saturating_add(2),
            y: area.y.saturating_add(top_line_count).saturating_add(1),
            width: area.width.saturating_sub(2),
            height: text_area_height,
        };
        let state = *self.textarea_state.borrow();
        self.textarea.cursor_pos_with_state(textarea_rect, state)
    }
}

impl CustomPromptView {
    fn input_height(&self, width: u16) -> u16 {
        let usable_width = width.saturating_sub(2);
        let text_height = self.textarea.desired_height(usable_width).clamp(1, 8);
        text_height.saturating_add(1).min(9)
    }

    fn footer_hint_line(&self) -> Line<'static> {
        match self.submit_mode {
            CustomPromptSubmitMode::Enter | CustomPromptSubmitMode::EnterSingleLine => {
                standard_popup_hint_line()
            }
            CustomPromptSubmitMode::CtrlD => Line::from(vec![
                "Press ".into(),
                key_hint::ctrl(KeyCode::Char('d')).into(),
                " when done editing, ".into(),
                key_hint::plain(KeyCode::Enter).into(),
                " for newline, or ".into(),
                key_hint::plain(KeyCode::Esc).into(),
                " to go back".into(),
            ]),
        }
    }
}

fn gutter() -> Span<'static> {
    "▌ ".cyan()
}
