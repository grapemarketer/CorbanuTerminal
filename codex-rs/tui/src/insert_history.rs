//! Inserts finalized history rows into terminal scrollback.
//!
//! Codex uses the terminal scrollback itself for finalized chat history, so inserting a history
//! cell is an escape-sequence operation rather than a normal ratatui render.

use std::fmt;
use std::io;
use std::io::Write;

use crate::render::line_utils::line_to_static;
use crate::terminal_hyperlinks::HyperlinkLine;
use crate::terminal_hyperlinks::decorate_spans;
use crate::terminal_hyperlinks::plain_hyperlink_lines;
use crate::terminal_hyperlinks::remap_wrapped_line;
use crate::wrapping::RtOptions;
use crate::wrapping::adaptive_wrap_line;
use crate::wrapping::line_contains_url_like;
use crate::wrapping::line_has_mixed_url_and_non_url_tokens;
use crossterm::Command;
use crossterm::cursor::MoveDown;
use crossterm::cursor::MoveTo;
use crossterm::cursor::MoveToColumn;
use crossterm::cursor::RestorePosition;
use crossterm::cursor::SavePosition;
use crossterm::queue;
use crossterm::style::Color as CColor;
use crossterm::style::Colors;
use crossterm::style::Print;
use crossterm::style::SetAttribute;
use crossterm::style::SetBackgroundColor;
use crossterm::style::SetColors;
use crossterm::style::SetForegroundColor;
use crossterm::terminal::Clear;
use crossterm::terminal::ClearType;
use ratatui::backend::IntoCrossterm;
use ratatui::layout::Size;
use ratatui::prelude::Backend;
use ratatui::style::Color;
use ratatui::style::Modifier;
use ratatui::text::Line;
use ratatui::text::Span;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HistoryLineWrapPolicy {
    PreWrap,
    Terminal,
}

/// Selects the terminal escape strategy used when writing history above the viewport.
///
/// Raw lines intentionally remain unbroken so terminal selection copies their source faithfully.
/// Zellij does not constrain soft-wrapped continuation rows to Codex's scroll region, so its raw
/// path appends history through the terminal and reserves blank rows for the next viewport draw.
/// The standard path also takes it when its scroll region would be a single row (#1039) or
/// empty, because the viewport fills the screen (#1082).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum InsertHistoryMode {
    Standard,
    ZellijRaw,
}

/// Insert `lines` above the viewport using the terminal's backend writer
/// (avoids direct stdout references).
pub fn insert_history_lines<B>(
    terminal: &mut crate::custom_terminal::Terminal<B>,
    lines: Vec<Line>,
) -> io::Result<()>
where
    B: Backend<Error = io::Error> + Write,
{
    insert_history_lines_with_wrap_policy(terminal, lines, HistoryLineWrapPolicy::PreWrap)
}

pub fn insert_history_lines_with_wrap_policy<B>(
    terminal: &mut crate::custom_terminal::Terminal<B>,
    lines: Vec<Line>,
    wrap_policy: HistoryLineWrapPolicy,
) -> io::Result<()>
where
    B: Backend<Error = io::Error> + Write,
{
    insert_history_lines_with_mode_and_wrap_policy(
        terminal,
        lines,
        InsertHistoryMode::Standard,
        wrap_policy,
    )
}

pub(crate) fn insert_history_lines_with_mode_and_wrap_policy<B>(
    terminal: &mut crate::custom_terminal::Terminal<B>,
    lines: Vec<Line>,
    mode: InsertHistoryMode,
    wrap_policy: HistoryLineWrapPolicy,
) -> io::Result<()>
where
    B: Backend<Error = io::Error> + Write,
{
    insert_history_hyperlink_lines_with_mode_and_wrap_policy(
        terminal,
        &plain_hyperlink_lines(lines.iter().map(line_to_static).collect()),
        mode,
        wrap_policy,
    )
}

pub(crate) fn insert_history_hyperlink_lines_with_mode_and_wrap_policy<B>(
    terminal: &mut crate::custom_terminal::Terminal<B>,
    lines: &[HyperlinkLine],
    mode: InsertHistoryMode,
    wrap_policy: HistoryLineWrapPolicy,
) -> io::Result<()>
where
    B: Backend<Error = io::Error> + Write,
{
    let screen_size = terminal.backend().size().unwrap_or(Size::new(0, 0));

    let mut area = terminal.viewport_area;
    let mut should_update_area = false;
    let last_cursor_pos = terminal.last_known_cursor_pos;

    // Pre-wrap lines for terminal scrollback. Three paths:
    //
    // - URL-only-ish lines are kept intact (no hard newlines inserted) so that
    //   terminal emulators can match them as clickable links. The
    //   terminal will character-wrap these lines at the viewport
    //   boundary.
    // - Mixed lines (URL + non-URL prose) are adaptively wrapped so
    //   non-URL text still wraps naturally while URL tokens remain
    //   unsplit.
    // - Non-URL lines also flow through adaptive wrapping; behavior is
    //   equivalent to standard wrapping when no URL is present.
    let wrap_width = area.width.max(1) as usize;
    let mut wrapped = Vec::new();
    let mut wrapped_rows = 0usize;

    for line in lines {
        let line_wrapped = match wrap_policy {
            HistoryLineWrapPolicy::Terminal => vec![line.clone()],
            HistoryLineWrapPolicy::PreWrap
                if line_contains_url_like(&line.line)
                    && !line_has_mixed_url_and_non_url_tokens(&line.line) =>
            {
                vec![line.clone()]
            }
            HistoryLineWrapPolicy::PreWrap => remap_wrapped_line(
                line,
                adaptive_wrap_line(
                    &line.line,
                    RtOptions::new(wrap_width)
                        .subsequent_indent(leading_whitespace_prefix(&line.line)),
                )
                .into_iter()
                .map(|line| line_to_static(&line))
                .collect(),
            ),
        };
        wrapped_rows += line_wrapped
            .iter()
            .map(|wrapped_line| wrapped_line.width().max(1).div_ceil(wrap_width))
            .sum::<usize>();
        wrapped.extend(line_wrapped);
    }
    let wrapped_lines = wrapped_rows as u16;
    // #1039: the standard path confines scrolling to rows `1..viewport top`. When that region is
    // a single row (for example the first line written above a viewport at the top of a freshly
    // cleared screen), `CSI 1;1 r` is invalid and terminals ignore it, so the line is written
    // into the viewport's first row and the next draw erases it. Append through the terminal
    // instead. #1082: so does a viewport filling the screen: there is no region at all, and
    // `CSI 1;0 r` selects the whole screen, so the line would land inside the viewport and the
    // diff-based redraw would leave it over the composer.
    let standard_scroll_amount = if area.bottom() < screen_size.height {
        wrapped_lines.min(screen_size.height - area.bottom())
    } else {
        0
    };
    let mode = if mode == InsertHistoryMode::Standard
        && wrapped_lines > 0
        && area.top().saturating_add(standard_scroll_amount) <= 1
    {
        InsertHistoryMode::ZellijRaw
    } else {
        mode
    };
    match mode {
        InsertHistoryMode::ZellijRaw => {
            // The existing viewport is immediately replaced in the same draw pass. Clear it
            // before terminal scrolling can move composer contents into scrollback.
            // `clear_after_position` erases row by row, so a viewport filling the screen does
            // not copy the composer or popup into tmux scrollback (#1340).
            terminal.clear_after_position(area.as_position())?;
            let writer = terminal.backend_mut();
            queue!(writer, MoveTo(/*x*/ 0, area.top()))?;
            for (index, line) in wrapped.iter().enumerate() {
                if index > 0 {
                    queue!(writer, Print("\r\n"))?;
                }
                write_history_line(writer, line, wrap_width)?;
            }

            // Writing raw source text through the terminal preserves its soft-wrap metadata.
            // Advance through empty rows for the viewport so history ends immediately above the
            // composer even when a replay batch is taller than the visible history region.
            for _ in 0..area.height {
                queue!(writer, Print("\r\n"), Clear(ClearType::UntilNewLine))?;
            }
            queue!(writer, MoveTo(last_cursor_pos.x, last_cursor_pos.y))?;

            let viewport_top = area
                .top()
                .saturating_add(wrapped_lines)
                .min(screen_size.height.saturating_sub(area.height));
            if area.y != viewport_top {
                area.y = viewport_top;
                should_update_area = true;
            }
        }
        InsertHistoryMode::Standard => {
            let writer = terminal.backend_mut();
            let cursor_top = if area.bottom() < screen_size.height {
                // If the viewport is not at the bottom of the screen, scroll it down to make room.
                // Don't scroll it past the bottom of the screen.
                let scroll_amount = standard_scroll_amount;

                let top_1based = area.top() + 1;
                queue!(writer, SetScrollRegion(top_1based..screen_size.height))?;
                queue!(writer, MoveTo(/*x*/ 0, area.top()))?;
                for _ in 0..scroll_amount {
                    queue!(writer, Print("\x1bM"))?;
                }
                queue!(writer, ResetScrollRegion)?;

                let cursor_top = area.top().saturating_sub(1);
                area.y += scroll_amount;
                should_update_area = true;
                cursor_top
            } else {
                area.top().saturating_sub(1)
            };

            // Limit the scroll region to the lines from the top of the screen to the
            // top of the viewport. With this in place, when we add lines inside this
            // area, only the lines in this area will be scrolled. We place the cursor
            // at the end of the scroll region, and add lines starting there.
            //
            // ┌─Screen───────────────────────┐
            // │┌╌Scroll region╌╌╌╌╌╌╌╌╌╌╌╌╌╌┐│
            // │┆                            ┆│
            // │┆                            ┆│
            // │┆                            ┆│
            // │█╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌┘│
            // │╭─Viewport───────────────────╮│
            // ││                            ││
            // │╰────────────────────────────╯│
            // └──────────────────────────────┘
            queue!(writer, SetScrollRegion(1..area.top()))?;

            // NB: we are using MoveTo instead of set_cursor_position here to avoid messing with the
            // terminal's last_known_cursor_position, which hopefully will still be accurate after we
            // fetch/restore the cursor position. insert_history_lines should be cursor-position-neutral :)
            queue!(writer, MoveTo(/*x*/ 0, cursor_top))?;

            for line in &wrapped {
                queue!(writer, Print("\r\n"))?;
                write_history_line(writer, line, wrap_width)?;
            }

            queue!(writer, ResetScrollRegion)?;
            queue!(writer, MoveTo(last_cursor_pos.x, last_cursor_pos.y))?;
        }
    }

    if should_update_area {
        terminal.set_viewport_area(area);
    }
    if wrapped_lines > 0 {
        terminal.note_history_rows_inserted(wrapped_lines);
    }

    Ok(())
}

pub(crate) fn leading_whitespace_prefix(line: &Line<'_>) -> Line<'static> {
    let mut spans = Vec::new();
    for span in &line.spans {
        let prefix_end = span
            .content
            .char_indices()
            .find_map(|(idx, ch)| (!ch.is_whitespace()).then_some(idx))
            .unwrap_or(span.content.len());
        if prefix_end > 0 {
            spans.push(Span::styled(
                span.content[..prefix_end].to_string(),
                span.style,
            ));
        }
        if prefix_end < span.content.len() {
            break;
        }
    }
    Line::from(spans).style(line.style)
}

/// Render a single wrapped history line: clear continuation rows for wide lines,
/// set foreground/background colors, and write styled spans. Caller is responsible
/// for cursor positioning and any leading `\r\n`.
fn write_history_line<W: Write>(
    writer: &mut W,
    line: &HyperlinkLine,
    wrap_width: usize,
) -> io::Result<()> {
    let physical_rows = line.width().max(1).div_ceil(wrap_width) as u16;
    if physical_rows > 1 {
        queue!(writer, SavePosition)?;
        for _ in 1..physical_rows {
            queue!(writer, MoveDown(1), MoveToColumn(0))?;
            queue!(writer, Clear(ClearType::UntilNewLine))?;
        }
        queue!(writer, RestorePosition)?;
    }
    queue!(
        writer,
        SetColors(Colors::new(
            line.line
                .style
                .fg
                .map(IntoCrossterm::into_crossterm)
                .unwrap_or(CColor::Reset),
            line.line
                .style
                .bg
                .map(IntoCrossterm::into_crossterm)
                .unwrap_or(CColor::Reset)
        ))
    )?;
    queue!(writer, Clear(ClearType::UntilNewLine))?;
    // Merge line-level style into each span so that ANSI colors reflect
    // line styles (e.g., blockquotes with green fg).
    let merged_spans: Vec<Span> = line
        .line
        .spans
        .iter()
        .map(|s| Span {
            style: s.style.patch(line.line.style),
            content: s.content.clone(),
        })
        .collect();
    let merged_line = HyperlinkLine {
        line: Line::from(merged_spans),
        hyperlinks: line.hyperlinks.clone(),
    };
    let decorated = decorate_spans(&merged_line);
    write_spans(writer, decorated.iter())
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SetScrollRegion(pub std::ops::Range<u16>);

impl Command for SetScrollRegion {
    fn write_ansi(&self, f: &mut impl fmt::Write) -> fmt::Result {
        write!(f, "\x1b[{};{}r", self.0.start, self.0.end)
    }

    #[cfg(windows)]
    fn execute_winapi(&self) -> std::io::Result<()> {
        panic!("tried to execute SetScrollRegion command using WinAPI, use ANSI instead");
    }

    #[cfg(windows)]
    fn is_ansi_code_supported(&self) -> bool {
        // TODO(nornagon): is this supported on Windows?
        true
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ResetScrollRegion;

impl Command for ResetScrollRegion {
    fn write_ansi(&self, f: &mut impl fmt::Write) -> fmt::Result {
        write!(f, "\x1b[r")
    }

    #[cfg(windows)]
    fn execute_winapi(&self) -> std::io::Result<()> {
        panic!("tried to execute ResetScrollRegion command using WinAPI, use ANSI instead");
    }

    #[cfg(windows)]
    fn is_ansi_code_supported(&self) -> bool {
        // TODO(nornagon): is this supported on Windows?
        true
    }
}

struct ModifierDiff {
    pub from: Modifier,
    pub to: Modifier,
}

impl ModifierDiff {
    fn queue<W>(self, mut w: W) -> io::Result<()>
    where
        W: io::Write,
    {
        use crossterm::style::Attribute as CAttribute;
        let removed = self.from - self.to;
        if removed.contains(Modifier::REVERSED) {
            queue!(w, SetAttribute(CAttribute::NoReverse))?;
        }
        if removed.contains(Modifier::BOLD) {
            queue!(w, SetAttribute(CAttribute::NormalIntensity))?;
            if self.to.contains(Modifier::DIM) {
                queue!(w, SetAttribute(CAttribute::Dim))?;
            }
        }
        if removed.contains(Modifier::ITALIC) {
            queue!(w, SetAttribute(CAttribute::NoItalic))?;
        }
        if removed.contains(Modifier::UNDERLINED) {
            queue!(w, SetAttribute(CAttribute::NoUnderline))?;
        }
        if removed.contains(Modifier::DIM) {
            queue!(w, SetAttribute(CAttribute::NormalIntensity))?;
        }
        if removed.contains(Modifier::CROSSED_OUT) {
            queue!(w, SetAttribute(CAttribute::NotCrossedOut))?;
        }
        if removed.contains(Modifier::SLOW_BLINK) || removed.contains(Modifier::RAPID_BLINK) {
            queue!(w, SetAttribute(CAttribute::NoBlink))?;
        }

        let added = self.to - self.from;
        if added.contains(Modifier::REVERSED) {
            queue!(w, SetAttribute(CAttribute::Reverse))?;
        }
        if added.contains(Modifier::BOLD) {
            queue!(w, SetAttribute(CAttribute::Bold))?;
        }
        if added.contains(Modifier::ITALIC) {
            queue!(w, SetAttribute(CAttribute::Italic))?;
        }
        if added.contains(Modifier::UNDERLINED) {
            queue!(w, SetAttribute(CAttribute::Underlined))?;
        }
        if added.contains(Modifier::DIM) {
            queue!(w, SetAttribute(CAttribute::Dim))?;
        }
        if added.contains(Modifier::CROSSED_OUT) {
            queue!(w, SetAttribute(CAttribute::CrossedOut))?;
        }
        if added.contains(Modifier::SLOW_BLINK) {
            queue!(w, SetAttribute(CAttribute::SlowBlink))?;
        }
        if added.contains(Modifier::RAPID_BLINK) {
            queue!(w, SetAttribute(CAttribute::RapidBlink))?;
        }

        Ok(())
    }
}

fn write_spans<'a, I>(mut writer: &mut impl Write, content: I) -> io::Result<()>
where
    I: IntoIterator<Item = &'a Span<'a>>,
{
    let mut fg = Color::Reset;
    let mut bg = Color::Reset;
    let mut last_modifier = Modifier::empty();
    for span in content {
        let mut modifier = Modifier::empty();
        modifier.insert(span.style.add_modifier);
        modifier.remove(span.style.sub_modifier);
        if modifier != last_modifier {
            let diff = ModifierDiff {
                from: last_modifier,
                to: modifier,
            };
            diff.queue(&mut writer)?;
            last_modifier = modifier;
        }
        let next_fg = span.style.fg.unwrap_or(Color::Reset);
        let next_bg = span.style.bg.unwrap_or(Color::Reset);
        if next_fg != fg || next_bg != bg {
            queue!(
                writer,
                SetColors(Colors::new(
                    next_fg.into_crossterm(),
                    next_bg.into_crossterm()
                ))
            )?;
            fg = next_fg;
            bg = next_bg;
        }

        queue!(writer, Print(span.content.clone()))?;
    }

    queue!(
        writer,
        SetForegroundColor(CColor::Reset),
        SetBackgroundColor(CColor::Reset),
        SetAttribute(crossterm::style::Attribute::Reset),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::markdown_render::render_markdown_text;
    use crate::test_backend::VT100Backend;
    use ratatui::layout::Rect;
    use ratatui::style::Color;

    #[test]
    fn writes_bold_then_regular_spans() {
        use ratatui::style::Stylize;

        let spans = ["A".bold(), "B".into()];

        let mut actual: Vec<u8> = Vec::new();
        write_spans(&mut actual, spans.iter()).unwrap();

        let mut expected: Vec<u8> = Vec::new();
        queue!(
            expected,
            SetAttribute(crossterm::style::Attribute::Bold),
            Print("A"),
            SetAttribute(crossterm::style::Attribute::NormalIntensity),
            Print("B"),
            SetForegroundColor(CColor::Reset),
            SetBackgroundColor(CColor::Reset),
            SetAttribute(crossterm::style::Attribute::Reset),
        )
        .unwrap();

        assert_eq!(
            String::from_utf8(actual).unwrap(),
            String::from_utf8(expected).unwrap()
        );
    }

    #[test]
    fn writes_semantic_web_link_without_changing_visible_text() {
        let destination = "https://example.com/long/path";
        let line = crate::terminal_hyperlinks::annotate_web_urls_in_line(Line::from(destination));
        let mut actual = Vec::new();

        write_history_line(&mut actual, &line, /*wrap_width*/ 80).expect("write history line");

        let output = String::from_utf8(actual).expect("UTF-8 terminal output");
        assert!(output.contains("\x1b]8;;https://example.com/long/path\x07"));
        assert_eq!(line.line.spans[0].content, destination);
    }

    #[test]
    fn vt100_blockquote_line_emits_green_fg() {
        // Set up a small off-screen terminal
        let width: u16 = 40;
        let height: u16 = 10;
        let backend = VT100Backend::new(width, height);
        let mut term = crate::custom_terminal::Terminal::with_options(backend).expect("terminal");
        // Place viewport on the last line so history inserts scroll upward
        let viewport = Rect::new(0, height - 1, width, 1);
        term.set_viewport_area(viewport);

        // Build a blockquote-like line: apply line-level green style and prefix "> "
        let mut line: Line<'static> = Line::from(vec!["> ".into(), "Hello world".into()]);
        line = line.style(Color::Green);
        insert_history_lines(&mut term, vec![line])
            .expect("Failed to insert history lines in test");

        let mut saw_colored = false;
        'outer: for row in 0..height {
            for col in 0..width {
                if let Some(cell) = term.backend().vt100().screen().cell(row, col)
                    && cell.has_contents()
                    && cell.fgcolor() != vt100::Color::Default
                {
                    saw_colored = true;
                    break 'outer;
                }
            }
        }
        assert!(
            saw_colored,
            "expected at least one colored cell in vt100 output"
        );
    }

    #[test]
    fn vt100_blockquote_wrap_preserves_color_on_all_wrapped_lines() {
        // Force wrapping by using a narrow viewport width and a long blockquote line.
        let width: u16 = 20;
        let height: u16 = 8;
        let backend = VT100Backend::new(width, height);
        let mut term = crate::custom_terminal::Terminal::with_options(backend).expect("terminal");
        // Viewport is the last line so history goes directly above it.
        let viewport = Rect::new(0, height - 1, width, 1);
        term.set_viewport_area(viewport);

        // Create a long blockquote with a distinct prefix and enough text to wrap.
        let mut line: Line<'static> = Line::from(vec![
            "> ".into(),
            "This is a long quoted line that should wrap".into(),
        ]);
        line = line.style(Color::Green);

        insert_history_lines(&mut term, vec![line])
            .expect("Failed to insert history lines in test");

        // Parse and inspect the final screen buffer.
        let screen = term.backend().vt100().screen();

        // Collect rows that are non-empty; these should correspond to our wrapped lines.
        let mut non_empty_rows: Vec<u16> = Vec::new();
        for row in 0..height {
            let mut any = false;
            for col in 0..width {
                if let Some(cell) = screen.cell(row, col)
                    && cell.has_contents()
                    && cell.contents() != "\0"
                    && cell.contents() != " "
                {
                    any = true;
                    break;
                }
            }
            if any {
                non_empty_rows.push(row);
            }
        }

        // Expect at least two rows due to wrapping.
        assert!(
            non_empty_rows.len() >= 2,
            "expected wrapped output to span >=2 rows, got {non_empty_rows:?}",
        );

        // For each non-empty row, ensure all non-space cells are using a non-default fg color.
        for row in non_empty_rows {
            for col in 0..width {
                if let Some(cell) = screen.cell(row, col) {
                    let contents = cell.contents();
                    if !contents.is_empty() && contents != " " {
                        assert!(
                            cell.fgcolor() != vt100::Color::Default,
                            "expected non-default fg on row {row} col {col}, got {:?}",
                            cell.fgcolor()
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn vt100_colored_prefix_then_plain_text_resets_color() {
        let width: u16 = 40;
        let height: u16 = 6;
        let backend = VT100Backend::new(width, height);
        let mut term = crate::custom_terminal::Terminal::with_options(backend).expect("terminal");
        let viewport = Rect::new(0, height - 1, width, 1);
        term.set_viewport_area(viewport);

        // First span colored, rest plain.
        let line: Line<'static> = Line::from(vec![
            Span::styled("1. ", ratatui::style::Style::default().fg(Color::LightBlue)),
            Span::raw("Hello world"),
        ]);

        insert_history_lines(&mut term, vec![line])
            .expect("Failed to insert history lines in test");

        let screen = term.backend().vt100().screen();

        // Find the first non-empty row; verify first three cells are colored, following cells default.
        'rows: for row in 0..height {
            let mut has_text = false;
            for col in 0..width {
                if let Some(cell) = screen.cell(row, col)
                    && cell.has_contents()
                    && cell.contents() != " "
                {
                    has_text = true;
                    break;
                }
            }
            if !has_text {
                continue;
            }

            // Expect "1. Hello world" starting at col 0.
            for col in 0..3 {
                let cell = screen.cell(row, col).unwrap();
                assert!(
                    cell.fgcolor() != vt100::Color::Default,
                    "expected colored prefix at col {col}, got {:?}",
                    cell.fgcolor()
                );
            }
            for col in 3..(3 + "Hello world".len() as u16) {
                let cell = screen.cell(row, col).unwrap();
                assert_eq!(
                    cell.fgcolor(),
                    vt100::Color::Default,
                    "expected default color for plain text at col {col}, got {:?}",
                    cell.fgcolor()
                );
            }
            break 'rows;
        }
    }

    #[test]
    fn vt100_deep_nested_mixed_list_third_level_marker_is_colored() {
        // Markdown with five levels (ordered → unordered → ordered → unordered → unordered).
        let md = "1. First\n   - Second level\n     1. Third level (ordered)\n        - Fourth level (bullet)\n          - Fifth level to test indent consistency\n";
        let text = render_markdown_text(md);
        let lines: Vec<Line<'static>> = text.lines.clone();

        let width: u16 = 60;
        let height: u16 = 12;
        let backend = VT100Backend::new(width, height);
        let mut term = crate::custom_terminal::Terminal::with_options(backend).expect("terminal");
        let viewport = ratatui::layout::Rect::new(0, height - 1, width, 1);
        term.set_viewport_area(viewport);

        insert_history_lines(&mut term, lines).expect("Failed to insert history lines in test");

        let screen = term.backend().vt100().screen();

        // Reconstruct screen rows as strings to locate the 3rd level line.
        let rows: Vec<String> = screen.rows(0, width).collect();

        let needle = "1. Third level (ordered)";
        let row_idx = rows
            .iter()
            .position(|r| r.contains(needle))
            .unwrap_or_else(|| {
                panic!("expected to find row containing {needle:?}, have rows: {rows:?}")
            });
        let col_start = rows[row_idx].find(needle).unwrap() as u16; // column where '1' starts

        // Verify that the numeric marker ("1.") at the third level is colored
        // (non-default fg) and the content after the following space resets to default.
        for c in [col_start, col_start + 1] {
            let cell = screen.cell(row_idx as u16, c).unwrap();
            assert!(
                cell.fgcolor() != vt100::Color::Default,
                "expected colored 3rd-level marker at row {row_idx} col {c}, got {:?}",
                cell.fgcolor()
            );
        }
        let content_col = col_start + 3; // skip '1', '.', and the space
        if let Some(cell) = screen.cell(row_idx as u16, content_col) {
            assert_eq!(
                cell.fgcolor(),
                vt100::Color::Default,
                "expected default color for 3rd-level content at row {row_idx} col {content_col}, got {:?}",
                cell.fgcolor()
            );
        }
    }

    #[test]
    fn vt100_prefixed_url_keeps_prefix_and_url_on_same_row() {
        let width: u16 = 48;
        let height: u16 = 8;
        let backend = VT100Backend::new(width, height);
        let mut term = crate::custom_terminal::Terminal::with_options(backend).expect("terminal");
        let viewport = Rect::new(0, height - 1, width, 1);
        term.set_viewport_area(viewport);

        let url = "http://a-long-url.com/this/that/blablablab/new.aspx/many_people_like_how";
        let line: Line<'static> = Line::from(vec!["  │ ".into(), url.into()]);

        insert_history_lines(&mut term, vec![line]).expect("insert history");

        let rows: Vec<String> = term.backend().vt100().screen().rows(0, width).collect();

        assert!(
            rows.iter().any(|r| r.contains("│ http://a-long-url.com")),
            "expected prefix and URL on same row, rows: {rows:?}"
        );
        assert!(
            !rows.iter().any(|r| r.trim_end() == "│"),
            "unexpected orphan prefix row, rows: {rows:?}"
        );
    }

    #[test]
    fn vt100_prefixed_url_like_without_scheme_keeps_prefix_and_token_on_same_row() {
        let width: u16 = 48;
        let height: u16 = 8;
        let backend = VT100Backend::new(width, height);
        let mut term = crate::custom_terminal::Terminal::with_options(backend).expect("terminal");
        let viewport = Rect::new(0, height - 1, width, 1);
        term.set_viewport_area(viewport);

        let url_like =
            "example.test/api/v1/projects/alpha-team/releases/2026-02-17/builds/1234567890";
        let line: Line<'static> = Line::from(vec!["  │ ".into(), url_like.into()]);

        insert_history_lines(&mut term, vec![line]).expect("insert history");

        let rows: Vec<String> = term.backend().vt100().screen().rows(0, width).collect();

        assert!(
            rows.iter()
                .any(|r| r.contains("│ example.test/api/v1/projects")),
            "expected prefix and URL-like token on same row, rows: {rows:?}"
        );
        assert!(
            !rows.iter().any(|r| r.trim_end() == "│"),
            "unexpected orphan prefix row, rows: {rows:?}"
        );
    }

    #[test]
    fn vt100_prefixed_mixed_url_line_wraps_suffix_words_together() {
        let width: u16 = 24;
        let height: u16 = 10;
        let backend = VT100Backend::new(width, height);
        let mut term = crate::custom_terminal::Terminal::with_options(backend).expect("terminal");
        let viewport = Rect::new(0, height - 1, width, 1);
        term.set_viewport_area(viewport);

        let url = "https://example.test/path/abcdef12345";
        let line: Line<'static> = Line::from(vec![
            "  │ ".into(),
            "see ".into(),
            url.into(),
            " tail words".into(),
        ]);

        insert_history_lines(&mut term, vec![line]).expect("insert mixed history");

        let rows: Vec<String> = term.backend().vt100().screen().rows(0, width).collect();
        assert!(
            rows.iter().any(|r| r.contains("│ see")),
            "expected prefixed prose before URL, rows: {rows:?}"
        );
        assert!(
            rows.iter().any(|r| r.contains("tail words")),
            "expected suffix words to wrap as a phrase, rows: {rows:?}"
        );
    }

    #[test]
    fn vt100_prefixed_mixed_url_line_preserves_prefix_on_wrapped_rows() {
        let width: u16 = 24;
        let height: u16 = 10;
        let backend = VT100Backend::new(width, height);
        let mut term = crate::custom_terminal::Terminal::with_options(backend).expect("terminal");
        let viewport = Rect::new(
            /*x*/ 0,
            /*y*/ height - 1,
            /*width*/ width,
            /*height*/ 1,
        );
        term.set_viewport_area(viewport);

        let line: Line<'static> = Line::from(vec![
            "  ".into(),
            "see https://example.com and enough trailing prose to force another wrapped row".into(),
        ]);

        insert_history_lines(&mut term, vec![line]).expect("insert mixed history");

        let rows: Vec<String> = term.backend().vt100().screen().rows(0, width).collect();
        let continuation_row = rows
            .iter()
            .find(|row| row.contains("prose to force another"))
            .unwrap_or_else(|| panic!("expected continuation row in screen rows: {rows:?}"));

        assert!(
            continuation_row.starts_with("  "),
            "expected wrapped continuation row to keep the original prefix, rows: {rows:?}"
        );
    }

    #[test]
    fn vt100_prefixed_non_url_line_preserves_prefix_on_wrapped_rows() {
        let width: u16 = 32;
        let height: u16 = 10;
        let backend = VT100Backend::new(width, height);
        let mut term = crate::custom_terminal::Terminal::with_options(backend).expect("terminal");
        let viewport = Rect::new(
            /*x*/ 0,
            /*y*/ height - 1,
            /*width*/ width,
            /*height*/ 1,
        );
        term.set_viewport_area(viewport);

        let line = Line::from(
            "      dog while this deliberately long string tests code block scrolling versus soft wrapping",
        );

        insert_history_lines(&mut term, vec![line]).expect("insert prefixed history");

        let rows: Vec<String> = term.backend().vt100().screen().rows(0, width).collect();
        let continuation_row = rows
            .iter()
            .find(|row| row.contains("tests code block scrolling"))
            .unwrap_or_else(|| panic!("expected continuation row in screen rows: {rows:?}"));

        assert!(
            continuation_row.starts_with("      "),
            "expected wrapped continuation row to keep the original prefix, rows: {rows:?}"
        );
    }

    #[test]
    fn vt100_terminal_wrap_policy_does_not_pre_wrap_long_paragraph() {
        let width: u16 = 20;
        let height: u16 = 8;
        let backend = VT100Backend::new(width, height);
        let mut term = crate::custom_terminal::Terminal::with_options(backend).expect("terminal");
        let viewport = Rect::new(0, height - 1, width, 1);
        term.set_viewport_area(viewport);

        let line = Line::from("alpha beta gamma delta epsilon zeta");

        insert_history_lines_with_wrap_policy(
            &mut term,
            vec![line],
            HistoryLineWrapPolicy::Terminal,
        )
        .expect("insert raw history");

        let rows: Vec<String> = term.backend().vt100().screen().rows(0, width).collect();
        assert!(
            rows.iter()
                .any(|row| row.trim_end() == "alpha beta gamma del"),
            "expected terminal soft-wrap instead of Codex word pre-wrap, rows: {rows:?}"
        );
    }

    #[test]
    fn vt100_zellij_raw_insert_keeps_soft_wrapped_tail_above_viewport() {
        let width: u16 = 20;
        let height: u16 = 8;
        let backend = VT100Backend::new(width, height);
        let mut term = crate::custom_terminal::Terminal::with_options(backend).expect("terminal");
        let viewport = Rect::new(
            /*x*/ 0,
            /*y*/ height - 2,
            /*width*/ width,
            /*height*/ 2,
        );
        term.set_viewport_area(viewport);

        let line = Line::from("raw-start-aaaaaaaaaaaaaaaaaaaaaaaa-tail-must-remain");
        insert_history_lines_with_mode_and_wrap_policy(
            &mut term,
            vec![line],
            InsertHistoryMode::ZellijRaw,
            HistoryLineWrapPolicy::Terminal,
        )
        .expect("insert Zellij raw history");

        let rows: Vec<String> = term.backend().vt100().screen().rows(0, width).collect();
        insta::assert_snapshot!("zellij_raw_terminal_wrap_above_viewport", rows.join("\n"));
        let history_rows = rows[..usize::from(term.viewport_area.y)]
            .iter()
            .map(|row| row.trim_end())
            .collect::<String>();
        let viewport_rows = rows[usize::from(term.viewport_area.y)..].join("\n");
        assert!(
            history_rows.contains("tail-must-remain"),
            "expected wrapped raw tail above the viewport, rows: {rows:?}"
        );
        assert!(
            !viewport_rows.contains("tail-must-remain"),
            "raw tail must not be written through the viewport, rows: {rows:?}"
        );
    }

    #[test]
    fn vt100_zellij_raw_replay_keeps_overflowing_soft_wrapped_tail_above_viewport() {
        let width: u16 = 20;
        let height: u16 = 8;
        let backend = VT100Backend::new(width, height);
        let mut term = crate::custom_terminal::Terminal::with_options(backend).expect("terminal");
        term.set_viewport_area(Rect::new(
            /*x*/ 0, /*y*/ 0, /*width*/ width, /*height*/ 2,
        ));

        let line = Line::from(format!("raw-start-{}tail-must-remain", "a".repeat(130)));
        insert_history_lines_with_mode_and_wrap_policy(
            &mut term,
            vec![line],
            InsertHistoryMode::ZellijRaw,
            HistoryLineWrapPolicy::Terminal,
        )
        .expect("replay Zellij raw history");

        let rows: Vec<String> = term.backend().vt100().screen().rows(0, width).collect();
        insta::assert_snapshot!(
            "zellij_raw_terminal_wrap_overflow_above_viewport",
            rows.join("\n")
        );
        let history_rows = rows[..usize::from(term.viewport_area.y)]
            .iter()
            .map(|row| row.trim_end())
            .collect::<String>();
        let viewport_rows = rows[usize::from(term.viewport_area.y)..].join("\n");
        assert!(
            history_rows.contains("tail-must-remain"),
            "expected overflowing raw tail above the viewport, rows: {rows:?}"
        );
        assert!(
            !viewport_rows.contains("tail-must-remain"),
            "overflowing raw tail must not be written through the viewport, rows: {rows:?}"
        );
    }

    fn draw_composer(term: &mut crate::custom_terminal::Terminal<VT100Backend>) {
        use ratatui::widgets::Widget;

        term.draw(|frame| {
            let area = frame.area();
            ratatui::widgets::Paragraph::new(vec![
                Line::from(""),
                Line::from("› Will this algorithm scale well?"),
                Line::from(""),
                Line::from("  Side from main thread · ctrl + c to close"),
            ])
            .render(area, frame.buffer_mut());
        })
        .expect("draw composer");
    }

    /// #1039: `/review` in `/side` writes its refusal as the first history line above a viewport
    /// at the top of a freshly cleared screen. The one-row scroll region (`CSI 1;1 r`) is invalid,
    /// so the line used to land in the viewport and the next draw erased it.
    #[test]
    fn vt100_first_line_above_top_viewport_stays_visible_after_redraw() {
        // Wide enough that the refusal takes one row, as on the 149-column screen in #1039.
        let width: u16 = 120;
        let height: u16 = 10;
        let backend = VT100Backend::new(width, height);
        let mut term = crate::custom_terminal::Terminal::with_options(backend).expect("terminal");
        term.set_viewport_area(Rect::new(
            /*x*/ 0, /*y*/ 0, /*width*/ width, /*height*/ 4,
        ));
        draw_composer(&mut term);

        insert_history_lines(
            &mut term,
            vec![Line::from(
                "■ '/review' is unavailable in side conversations. Press Ctrl+C to return to the main thread first.",
            )],
        )
        .expect("insert refusal");
        draw_composer(&mut term);

        let rows: Vec<String> = term.backend().vt100().screen().rows(0, width).collect();
        insta::assert_snapshot!(
            "first_line_above_top_viewport_after_redraw",
            rows.join("\n")
        );
        let viewport_top = usize::from(term.viewport_area.y);
        let history = rows[..viewport_top].join("\n");
        assert!(
            history.contains("'/review' is unavailable in side conversations.")
                && history.contains("return to the main thread first."),
            "expected the whole refusal above the viewport, rows: {rows:?}"
        );
    }

    /// #1039: the same one-row history region arises when the viewport fills every row but the
    /// first; earlier lines must scroll off and the last one must stay above the viewport.
    #[test]
    fn vt100_one_row_history_region_keeps_last_inserted_line() {
        let width: u16 = 40;
        let height: u16 = 6;
        let backend = VT100Backend::new(width, height);
        let mut term = crate::custom_terminal::Terminal::with_options(backend).expect("terminal");
        term.set_viewport_area(Rect::new(
            /*x*/ 0,
            /*y*/ 1,
            /*width*/ width,
            /*height*/ height - 1,
        ));
        draw_composer(&mut term);

        insert_history_lines(
            &mut term,
            vec![
                Line::from("first history line"),
                Line::from("second history line"),
            ],
        )
        .expect("insert history");
        draw_composer(&mut term);

        let rows: Vec<String> = term.backend().vt100().screen().rows(0, width).collect();
        assert_eq!(
            term.viewport_area.y, 1,
            "viewport must stay put, rows: {rows:?}"
        );
        assert_eq!(rows[0].trim_end(), "second history line", "rows: {rows:?}");
        assert!(
            rows[1..].iter().all(|row| !row.contains("history line")),
            "history must not be written into the viewport, rows: {rows:?}"
        );
    }

    /// #1082: with the viewport filling the screen there is no history region; the standard
    /// path's `CSI 1;0 r` selected the whole screen and wrote the line inside the viewport, where
    /// the diff-based redraw left it over the composer. It now goes to scrollback.
    #[test]
    fn vt100_full_screen_viewport_sends_history_to_scrollback() {
        let width: u16 = 50;
        let height: u16 = 4;
        let backend = VT100Backend::with_scrollback(width, height, /*scrollback*/ 10);
        let mut term = crate::custom_terminal::Terminal::with_options(backend).expect("terminal");
        term.set_viewport_area(Rect::new(
            /*x*/ 0, /*y*/ 0, /*width*/ width, /*height*/ height,
        ));
        draw_composer(&mut term);

        insert_history_lines(&mut term, vec![Line::from("inserted history line")])
            .expect("insert history");
        draw_composer(&mut term);

        let rows: Vec<String> = term.backend().vt100().screen().rows(0, width).collect();
        assert_eq!(term.viewport_area, Rect::new(0, 0, width, height));
        assert_eq!(
            rows.iter().map(|row| row.trim_end()).collect::<Vec<_>>(),
            vec![
                "",
                "› Will this algorithm scale well?",
                "",
                "  Side from main thread · ctrl + c to close",
            ],
            "the composer must be intact, with no history inside it"
        );
        let screen = term.backend_mut().vt100_mut().screen_mut();
        screen.set_scrollback(1);
        let top = screen.rows(0, width).next().unwrap_or_default();
        assert_eq!(
            top.trim_end(),
            "inserted history line",
            "kept in scrollback"
        );
    }

    /// #1340: inserting history while the viewport fills the screen must not erase the whole
    /// screen from the home position, which tmux (`scroll-on-clear`, default on) saves into
    /// scrollback: each batch used to leave a copy of the popup there. Scrollback holds each
    /// history line once, in order, and nothing else.
    #[test]
    fn vt100_full_screen_viewport_batches_leave_no_popup_copy_in_scrollback() {
        let width: u16 = 50;
        let height: u16 = 4;
        let backend = VT100Backend::with_scrollback(width, height, /*scrollback*/ 20);
        let mut term = crate::custom_terminal::Terminal::with_options(backend).expect("terminal");
        term.set_viewport_area(Rect::new(
            /*x*/ 0, /*y*/ 0, /*width*/ width, /*height*/ height,
        ));
        draw_composer(&mut term);

        for batch in [
            ["line 1", "line 2"],
            ["line 3", "line 4"],
            ["line 5", "line 6"],
        ] {
            insert_history_lines(&mut term, batch.into_iter().map(Line::from).collect())
                .expect("insert history");
            draw_composer(&mut term);
        }

        assert_eq!(
            term.backend().scroll_on_clear_saves(),
            &[] as &[String],
            "no whole-screen erase while inserting history"
        );
        let rows: Vec<String> = term.backend().vt100().screen().rows(0, width).collect();
        assert_eq!(
            rows.iter().map(|row| row.trim_end()).collect::<Vec<_>>(),
            vec![
                "",
                "› Will this algorithm scale well?",
                "",
                "  Side from main thread · ctrl + c to close",
            ],
            "the composer must be intact"
        );
        let screen = term.backend_mut().vt100_mut().screen_mut();
        screen.set_scrollback(usize::MAX);
        let scrolled = screen.scrollback();
        let history: Vec<String> = (1..=scrolled)
            .rev()
            .map(|offset| {
                screen.set_scrollback(offset);
                screen
                    .rows(0, width)
                    .next()
                    .unwrap_or_default()
                    .trim_end()
                    .to_string()
            })
            .collect();
        assert_eq!(
            history,
            vec!["line 1", "line 2", "line 3", "line 4", "line 5", "line 6"],
            "scrollback holds each history line once"
        );
    }

    /// #1039: a viewport at the top with one free row below scrolls down one row, leaving a
    /// one-row history region; a two-line batch must end with its last line above the viewport.
    #[test]
    fn vt100_top_viewport_with_one_free_row_keeps_last_line_of_a_batch() {
        let width: u16 = 40;
        let height: u16 = 6;
        let backend = VT100Backend::new(width, height);
        let mut term = crate::custom_terminal::Terminal::with_options(backend).expect("terminal");
        term.set_viewport_area(Rect::new(
            /*x*/ 0,
            /*y*/ 0,
            /*width*/ width,
            /*height*/ height - 1,
        ));
        draw_composer(&mut term);

        insert_history_lines(
            &mut term,
            vec![
                Line::from("first history line"),
                Line::from("second history line"),
            ],
        )
        .expect("insert history");
        draw_composer(&mut term);

        let rows: Vec<String> = term.backend().vt100().screen().rows(0, width).collect();
        assert_eq!(term.viewport_area.y, 1, "rows: {rows:?}");
        assert_eq!(rows[0].trim_end(), "second history line", "rows: {rows:?}");
        assert!(
            rows[1..].iter().all(|row| !row.contains("history line")),
            "history must not be written into the viewport, rows: {rows:?}"
        );
    }

    #[test]
    fn vt100_unwrapped_url_like_clears_continuation_rows() {
        let width: u16 = 20;
        let height: u16 = 10;
        let backend = VT100Backend::new(width, height);
        let mut term = crate::custom_terminal::Terminal::with_options(backend).expect("terminal");
        let viewport = Rect::new(0, height - 1, width, 1);
        term.set_viewport_area(viewport);

        let filler_line: Line<'static> = Line::from(vec![
            "  │ ".into(),
            "XXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXX".into(),
        ]);
        insert_history_lines(&mut term, vec![filler_line]).expect("insert filler history");

        let url_like = "example.test/api/v1/short";
        let url_line: Line<'static> = Line::from(vec!["  │ ".into(), url_like.into()]);
        insert_history_lines(&mut term, vec![url_line]).expect("insert url-like history");

        let rows: Vec<String> = term.backend().vt100().screen().rows(0, width).collect();
        let first_row = rows
            .iter()
            .position(|row| row.contains("│ example.test/api"))
            .unwrap_or_else(|| panic!("expected url-like first row in screen rows: {rows:?}"));
        assert!(
            first_row + 1 < rows.len(),
            "expected a continuation row for wrapped URL-like line, rows: {rows:?}"
        );
        let continuation_row = rows[first_row + 1].trim_end();

        assert!(
            continuation_row.contains("/v1/short") || continuation_row.contains("short"),
            "expected continuation row to contain wrapped URL-like tail, got: {continuation_row:?}"
        );
        assert!(
            !continuation_row.contains('X'),
            "expected continuation row to be cleared before writing wrapped URL-like content, got: {continuation_row:?}"
        );
    }

    #[test]
    fn vt100_long_unwrapped_url_does_not_insert_extra_blank_gap_before_content() {
        let width: u16 = 56;
        let height: u16 = 24;
        let backend = VT100Backend::new(width, height);
        let mut term = crate::custom_terminal::Terminal::with_options(backend).expect("terminal");
        let viewport = Rect::new(0, height - 1, width, 1);
        term.set_viewport_area(viewport);

        let prompt = "Write a long URL as output for testing";
        insert_history_lines(&mut term, vec![Line::from(prompt)]).expect("insert prompt line");

        let long_url = format!(
            "https://example.test/api/v1/projects/alpha-team/releases/2026-02-17/builds/1234567890/{}",
            "very-long-segment-".repeat(16),
        );
        let url_line: Line<'static> = Line::from(vec!["• ".into(), long_url.into()]);
        insert_history_lines(&mut term, vec![url_line]).expect("insert long url line");

        let rows: Vec<String> = term.backend().vt100().screen().rows(0, width).collect();
        let prompt_row = rows
            .iter()
            .position(|row| row.contains("Write a long URL as output for testing"))
            .unwrap_or_else(|| panic!("expected prompt row in screen rows: {rows:?}"));
        let url_row = rows
            .iter()
            .position(|row| row.contains("• https://example.test/api"))
            .unwrap_or_else(|| panic!("expected URL first row in screen rows: {rows:?}"));

        assert!(
            url_row <= prompt_row + 2,
            "expected URL content to appear immediately after prompt (allowing at most one spacer row), got prompt_row={prompt_row}, url_row={url_row}, rows={rows:?}",
        );
    }
}
