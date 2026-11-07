use std::fmt::{self};
use std::io::Write;
use std::io::{self};

use ratatui::prelude::CrosstermBackend;

use ratatui::backend::Backend;
use ratatui::backend::ClearType;
use ratatui::backend::WindowSize;
use ratatui::buffer::Cell;
use ratatui::layout::Position;
use ratatui::layout::Size;

/// This wraps a CrosstermBackend and a vt100::Parser to mock
/// a "real" terminal.
///
/// Importantly, this wrapper avoids calling any crossterm methods
/// which write to stdout regardless of the writer. This includes:
/// - getting the terminal size
/// - getting the cursor position
pub struct VT100Backend {
    crossterm_backend: CrosstermBackend<ScrollOnClearParser>,
}

/// A vt100 parser that also records what tmux would save into scrollback with its default
/// `scroll-on-clear` option: tmux copies the whole screen into history before an erase that
/// clears all of it, that is `ED 2`, or `ED 0` with the cursor at the home position (#1340).
/// vt100 itself never does this, so tests read the copies from [`VT100Backend::scroll_on_clear_saves`].
struct ScrollOnClearParser {
    parser: vt100::Parser,
    // Read only by the crate's unit tests, not by `tests/all`, which shares this file.
    #[allow(dead_code)]
    saves: Vec<String>,
}

impl ScrollOnClearParser {
    fn new(height: u16, width: u16, scrollback: usize) -> Self {
        Self {
            parser: vt100::Parser::new(height, width, scrollback),
            saves: Vec::new(),
        }
    }
}

impl Write for ScrollOnClearParser {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        const WHOLE_SCREEN: &[u8] = b"\x1b[2J";
        const TO_END: [&[u8]; 2] = [b"\x1b[J", b"\x1b[0J"];
        let mut rest = buf;
        while let Some((start, len)) = (0..rest.len()).find_map(|start| {
            std::iter::once(WHOLE_SCREEN)
                .chain(TO_END)
                .find(|pattern| rest[start..].starts_with(pattern))
                .map(|pattern| (start, pattern.len()))
        }) {
            self.parser.process(&rest[..start]);
            let erase = &rest[start..start + len];
            let screen = self.parser.screen();
            if erase == WHOLE_SCREEN || screen.cursor_position() == (0, 0) {
                self.saves.push(screen.contents());
            }
            self.parser.process(erase);
            rest = &rest[start + len..];
        }
        self.parser.process(rest);
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl VT100Backend {
    /// Creates a new `TestBackend` with the specified width and height.
    pub fn new(width: u16, height: u16) -> Self {
        crossterm::style::force_color_output(true);
        Self {
            crossterm_backend: CrosstermBackend::new(ScrollOnClearParser::new(
                height, width, /*scrollback*/ 0,
            )),
        }
    }

    pub fn vt100(&self) -> &vt100::Parser {
        &self.crossterm_backend.writer().parser
    }

    /// Screens tmux (`scroll-on-clear`, its default) would have copied into scrollback.
    #[allow(dead_code)]
    pub fn scroll_on_clear_saves(&self) -> &[String] {
        &self.crossterm_backend.writer().saves
    }

    /// Like [`Self::new`], keeping up to `scrollback` rows that scroll off the top.
    // Used by the crate's unit tests, not by `tests/all`, which shares this file.
    #[allow(dead_code)]
    pub fn with_scrollback(width: u16, height: u16, scrollback: usize) -> Self {
        crossterm::style::force_color_output(true);
        Self {
            crossterm_backend: CrosstermBackend::new(ScrollOnClearParser::new(
                height, width, scrollback,
            )),
        }
    }

    #[allow(dead_code)]
    pub fn vt100_mut(&mut self) -> &mut vt100::Parser {
        &mut self.crossterm_backend.writer_mut().parser
    }
}

impl Write for VT100Backend {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.crossterm_backend.writer_mut().write(buf)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.crossterm_backend.writer_mut().flush()
    }
}

impl fmt::Display for VT100Backend {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.vt100().screen().contents())
    }
}

impl Backend for VT100Backend {
    type Error = io::Error;

    fn draw<'a, I>(&mut self, content: I) -> io::Result<()>
    where
        I: Iterator<Item = (u16, u16, &'a Cell)>,
    {
        self.crossterm_backend.draw(content)?;
        Ok(())
    }

    fn hide_cursor(&mut self) -> io::Result<()> {
        self.crossterm_backend.hide_cursor()?;
        Ok(())
    }

    fn show_cursor(&mut self) -> io::Result<()> {
        self.crossterm_backend.show_cursor()?;
        Ok(())
    }

    fn get_cursor_position(&mut self) -> io::Result<Position> {
        Ok(self.vt100().screen().cursor_position().into())
    }

    fn set_cursor_position<P: Into<Position>>(&mut self, position: P) -> io::Result<()> {
        self.crossterm_backend.set_cursor_position(position)
    }

    fn clear(&mut self) -> io::Result<()> {
        self.crossterm_backend.clear()
    }

    fn clear_region(&mut self, clear_type: ClearType) -> io::Result<()> {
        self.crossterm_backend.clear_region(clear_type)
    }

    fn append_lines(&mut self, line_count: u16) -> io::Result<()> {
        self.crossterm_backend.append_lines(line_count)
    }

    fn size(&self) -> io::Result<Size> {
        let (rows, cols) = self.vt100().screen().size();
        Ok(Size::new(cols, rows))
    }

    fn window_size(&mut self) -> io::Result<WindowSize> {
        Ok(WindowSize {
            columns_rows: self.vt100().screen().size().into(),
            // Arbitrary size, we don't rely on this in testing.
            pixels: Size {
                width: 640,
                height: 480,
            },
        })
    }

    fn flush(&mut self) -> io::Result<()> {
        self.crossterm_backend.writer_mut().flush()
    }

    fn scroll_region_up(&mut self, region: std::ops::Range<u16>, scroll_by: u16) -> io::Result<()> {
        self.crossterm_backend.scroll_region_up(region, scroll_by)
    }

    fn scroll_region_down(
        &mut self,
        region: std::ops::Range<u16>,
        scroll_by: u16,
    ) -> io::Result<()> {
        self.crossterm_backend.scroll_region_down(region, scroll_by)
    }
}
