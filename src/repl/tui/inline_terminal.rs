//! Sofos' inline-viewport-friendly replacement for `ratatui::Terminal`.
//!
//! Ratatui's stock `Terminal` with `Viewport::Inline` runs
//! `compute_inline_size` on every resize, which (a) issues a
//! `cursor::position` DSR that deadlocks against our input-reader
//! thread, and (b) on a grow repositions the viewport without
//! clearing the old rows, leaving ghost copies of the hint / input /
//! status visible above the new viewport.
//!
//! This [`Terminal`] makes [`Terminal::resize`] a pure state update
//! (no `compute_inline_size`, no cursor query). Viewport placement is
//! the application's responsibility — see
//! [`super::inline_tui::InlineTui::fit_viewport_height`] for the logic
//! that anchors the inline viewport to the cursor row on startup and
//! to the screen bottom after a resize, scrolling content above it
//! via DECSTBM when needed. That split keeps resize deterministic:
//! this file never talks to the cursor after construction.
//!
//! The diff/draw loop and its supporting types are derived from the
//! OpenAI Codex CLI's `custom_terminal.rs`
//! (<https://github.com/openai/codex/blob/main/codex-rs/tui/src/custom_terminal.rs>,
//! ratatui's MIT license attribution is preserved below).
//
// This is derived from `ratatui::Terminal`, which is licensed under the following terms:
//
// The MIT License (MIT)
// Copyright (c) 2016-2022 Florian Dehau
// Copyright (c) 2023-2025 The Ratatui Developers
//
// Permission is hereby granted, free of charge, to any person obtaining a copy
// of this software and associated documentation files (the "Software"), to deal
// in the Software without restriction, including without limitation the rights
// to use, copy, modify, merge, publish, distribute, sublicense, and/or sell
// copies of the Software, and to permit persons to whom the Software is
// furnished to do so, subject to the following conditions:
//
// The above copyright notice and this permission notice shall be included in all
// copies or substantial portions of the Software.
//
// THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
// IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
// FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
// AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
// LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM,
// OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE
// SOFTWARE.

use std::io;
use std::io::Write;

use crossterm::cursor::MoveTo;
use crossterm::queue;
use crossterm::style::Colors;
use crossterm::style::Print;
use crossterm::style::SetAttribute;
use crossterm::style::SetBackgroundColor;
use crossterm::style::SetColors;
use crossterm::style::SetForegroundColor;
use crossterm::terminal::Clear;
use ratatui::backend::Backend;
use ratatui::backend::ClearType;
use ratatui::buffer::Buffer;
use ratatui::buffer::Cell;
use ratatui::layout::Position;
use ratatui::layout::Rect;
use ratatui::layout::Size;
use ratatui::prelude::IntoCrossterm;
use ratatui::style::Color;
use ratatui::style::Modifier;
use ratatui::widgets::Widget;
use unicode_width::UnicodeWidthStr;

use super::sgr::SgrModifierChange;

/// Returns the display width of a cell symbol, ignoring OSC escape sequences.
///
/// OSC sequences (e.g. OSC 8 hyperlinks: `\x1B]8;;URL\x07`) are terminal
/// control sequences that don't consume display columns.  The standard
/// `UnicodeWidthStr::width()` method incorrectly counts the printable
/// characters inside OSC payloads (like `]`, `8`, `;`, and URL characters).
/// This function strips them first so that only visible characters contribute
/// to the width.
fn display_width(s: &str) -> usize {
    // Fast path: no escape sequences present.
    if !s.contains('\x1B') {
        return s.width();
    }

    // Strip OSC sequences: ESC ] ... BEL
    let mut visible = String::with_capacity(s.len());
    let mut chars = s.chars();
    while let Some(ch) = chars.next() {
        if ch == '\x1B' && chars.clone().next() == Some(']') {
            chars.next();
            for c in chars.by_ref() {
                if c == '\x07' {
                    break;
                }
            }
            continue;
        }
        visible.push(ch);
    }
    visible.width()
}

#[derive(Debug, Hash)]
pub struct Frame<'a> {
    pub(crate) cursor_position: Option<Position>,
    pub(crate) viewport_area: Rect,
    pub(crate) buffer: &'a mut Buffer,
}

#[allow(dead_code)]
impl Frame<'_> {
    pub const fn area(&self) -> Rect {
        self.viewport_area
    }

    /// Render a ratatui [`Widget`] into the frame's buffer. Matches the
    /// stock ratatui `Frame::render_widget` signature so existing draw
    /// code can drop in without changes.
    pub fn render_widget<W: Widget>(&mut self, widget: W, area: Rect) {
        widget.render(area, self.buffer);
    }

    pub fn set_cursor_position<P: Into<Position>>(&mut self, position: P) {
        self.cursor_position = Some(position.into());
    }

    pub fn buffer_mut(&mut self) -> &mut Buffer {
        self.buffer
    }
}

#[derive(Debug, Default, Clone, Eq, PartialEq, Hash)]
pub struct Terminal<B>
where
    B: Backend<Error = io::Error> + Write,
{
    backend: B,
    buffers: [Buffer; 2],
    current: usize,
    pub hidden_cursor: bool,
    pub viewport_area: Rect,
    pub last_known_screen_size: Size,
    pub last_known_cursor_pos: Position,
    visible_history_rows: u16,
}

impl<B> Drop for Terminal<B>
where
    B: Backend<Error = io::Error> + Write,
{
    fn drop(&mut self) {
        if self.hidden_cursor {
            let _ = self.show_cursor();
        }
    }
}

#[allow(dead_code)]
impl<B> Terminal<B>
where
    B: Backend<Error = io::Error> + Write,
{
    /// Create a new [`Terminal`] wrapping `backend`. Queries
    /// `cursor::position` once to anchor the initial viewport row; on
    /// PTYs that don't answer CPR we fall back to the screen origin
    /// rather than aborting startup.
    pub fn new(mut backend: B) -> io::Result<Self> {
        let screen_size = backend.size()?;
        let cursor_pos = backend.get_cursor_position().unwrap_or_else(|err| {
            // Some PTYs do not answer CPR (`ESC[6n`); continue with a safe default
            // instead of failing TUI startup.
            tracing::warn!("failed to read initial cursor position; defaulting to origin: {err}");
            Position { x: 0, y: 0 }
        });
        Ok(Self {
            backend,
            buffers: [Buffer::empty(Rect::ZERO), Buffer::empty(Rect::ZERO)],
            current: 0,
            hidden_cursor: false,
            viewport_area: Rect::new(0, cursor_pos.y, 0, 0),
            last_known_screen_size: screen_size,
            last_known_cursor_pos: cursor_pos,
            visible_history_rows: 0,
        })
    }

    pub fn get_frame(&mut self) -> Frame<'_> {
        Frame {
            cursor_position: None,
            viewport_area: self.viewport_area,
            buffer: self.current_buffer_mut(),
        }
    }

    fn current_buffer(&self) -> &Buffer {
        &self.buffers[self.current]
    }

    fn current_buffer_mut(&mut self) -> &mut Buffer {
        &mut self.buffers[self.current]
    }

    fn previous_buffer(&self) -> &Buffer {
        &self.buffers[1 - self.current]
    }

    fn previous_buffer_mut(&mut self) -> &mut Buffer {
        &mut self.buffers[1 - self.current]
    }

    pub const fn backend(&self) -> &B {
        &self.backend
    }

    pub fn backend_mut(&mut self) -> &mut B {
        &mut self.backend
    }

    pub fn flush(&mut self) -> io::Result<()> {
        let updates = diff_buffers(self.previous_buffer(), self.current_buffer());
        let last_cell_command = updates.iter().rfind(|command| command.is_cell());
        if let Some(&DrawCommand::Cell { x, y, .. }) = last_cell_command {
            self.last_known_cursor_pos = Position { x, y };
        }
        draw(&mut self.backend, updates.into_iter())
    }

    /// Deliberately a pure state update. Ratatui's stock `resize` also calls
    /// `compute_inline_size`, which issues a DSR that deadlocks against our
    /// input reader and repositions the viewport without clearing the old
    /// rows. The application handles viewport placement in
    /// [`super::inline_tui::InlineTui::draw`].
    pub fn resize(&mut self, screen_size: Size) -> io::Result<()> {
        self.last_known_screen_size = screen_size;
        Ok(())
    }

    pub fn set_viewport_area(&mut self, area: Rect) {
        self.current_buffer_mut().resize(area);
        self.previous_buffer_mut().resize(area);
        self.viewport_area = area;
        self.visible_history_rows = self.visible_history_rows.min(area.top());
    }

    pub fn autoresize(&mut self) -> io::Result<()> {
        let screen_size = self.size()?;
        if screen_size != self.last_known_screen_size {
            self.resize(screen_size)?;
        }
        Ok(())
    }

    pub fn draw<F>(&mut self, render_callback: F) -> io::Result<()>
    where
        F: FnOnce(&mut Frame),
    {
        self.try_draw(|frame| {
            render_callback(frame);
            io::Result::Ok(())
        })
    }

    pub fn try_draw<F, E>(&mut self, render_callback: F) -> io::Result<()>
    where
        F: FnOnce(&mut Frame) -> Result<(), E>,
        E: Into<io::Error>,
    {
        self.autoresize()?;

        let mut frame = self.get_frame();
        render_callback(&mut frame).map_err(Into::into)?;
        let cursor_position = frame.cursor_position;

        self.flush()?;

        match cursor_position {
            None => self.hide_cursor()?,
            Some(position) => {
                self.show_cursor()?;
                self.set_cursor_position(position)?;
            }
        }

        self.swap_buffers();
        Backend::flush(&mut self.backend)?;
        Ok(())
    }

    pub fn hide_cursor(&mut self) -> io::Result<()> {
        self.backend.hide_cursor()?;
        self.hidden_cursor = true;
        Ok(())
    }

    pub fn show_cursor(&mut self) -> io::Result<()> {
        self.backend.show_cursor()?;
        self.hidden_cursor = false;
        Ok(())
    }

    #[allow(dead_code)]
    pub fn get_cursor_position(&mut self) -> io::Result<Position> {
        self.backend.get_cursor_position()
    }

    pub fn set_cursor_position<P: Into<Position>>(&mut self, position: P) -> io::Result<()> {
        let position = position.into();
        self.backend.set_cursor_position(position)?;
        self.last_known_cursor_pos = position;
        Ok(())
    }

    /// Clear the current viewport region on screen and force a full redraw on
    /// the next draw call.
    pub fn clear(&mut self) -> io::Result<()> {
        if self.viewport_area.is_empty() {
            return Ok(());
        }
        self.backend
            .set_cursor_position(self.viewport_area.as_position())?;
        self.backend.clear_region(ClearType::AfterCursor)?;
        self.previous_buffer_mut().reset();
        Ok(())
    }

    /// Force the next draw to repaint the entire viewport by resetting the
    /// diff buffer. Call this after operations that move screen content
    /// outside of ratatui's knowledge (e.g. an `\e[2J` wipe during a
    /// resize replay) since the diff buffer's assumptions no longer
    /// match the on-screen state.
    pub fn invalidate_viewport(&mut self) {
        self.previous_buffer_mut().reset();
    }

    /// Clear the entire visible screen (not just the viewport) and force a
    /// full redraw. Some terminals (notably Terminal.app) behave more
    /// reliably if we pair ED2 with an explicit cursor-home before/after,
    /// matching the shell `clear` sequence (`CSI 2J` + `CSI H`).
    pub fn clear_visible_screen(&mut self) -> io::Result<()> {
        let home = Position { x: 0, y: 0 };
        self.set_cursor_position(home)?;
        self.backend.clear_region(ClearType::All)?;
        self.set_cursor_position(home)?;
        std::io::Write::flush(&mut self.backend)?;
        self.visible_history_rows = 0;
        self.previous_buffer_mut().reset();
        Ok(())
    }

    pub fn visible_history_rows(&self) -> u16 {
        self.visible_history_rows
    }

    pub(crate) fn record_history_rows(&mut self, inserted_rows: u16) {
        self.visible_history_rows = self
            .visible_history_rows
            .saturating_add(inserted_rows)
            .min(self.viewport_area.top());
    }

    pub fn swap_buffers(&mut self) {
        self.previous_buffer_mut().reset();
        self.current = 1 - self.current;
    }

    pub fn size(&self) -> io::Result<Size> {
        self.backend.size()
    }
}

#[derive(Debug)]
enum DrawCommand {
    Cell { x: u16, y: u16, cell: Cell },
    ClearTrailingCells { x: u16, y: u16, bg: Color },
}

impl DrawCommand {
    fn is_cell(&self) -> bool {
        matches!(self, DrawCommand::Cell { .. })
    }
}

fn diff_buffers(a: &Buffer, b: &Buffer) -> Vec<DrawCommand> {
    let previous_buffer = &a.content;
    let next_buffer = &b.content;

    let mut updates = vec![];
    let mut last_nonblank_columns = vec![0; a.area.height as usize];
    for y in 0..a.area.height {
        let row_start = y as usize * a.area.width as usize;
        let row_end = row_start + a.area.width as usize;
        let row = &next_buffer[row_start..row_end];
        let bg = row.last().map(|cell| cell.bg).unwrap_or(Color::Reset);

        let mut last_nonblank_column = 0usize;
        let mut column = 0usize;
        while column < row.len() {
            let cell = &row[column];
            let width = display_width(cell.symbol());
            if cell.symbol() != " " || cell.bg != bg || cell.modifier != Modifier::empty() {
                last_nonblank_column = column + (width.saturating_sub(1));
            }
            column += width.max(1);
        }

        if last_nonblank_column + 1 < row.len() {
            let (x, y) = a.pos_of(row_start + last_nonblank_column + 1);
            updates.push(DrawCommand::ClearTrailingCells { x, y, bg });
        }

        last_nonblank_columns[y as usize] = last_nonblank_column as u16;
    }

    let mut invalidated: usize = 0;
    let mut to_skip: usize = 0;
    for (i, (current, previous)) in next_buffer.iter().zip(previous_buffer.iter()).enumerate() {
        if !current.skip && (current != previous || invalidated > 0) && to_skip == 0 {
            let (x, y) = a.pos_of(i);
            let row = i / a.area.width as usize;
            if x <= last_nonblank_columns[row] {
                updates.push(DrawCommand::Cell {
                    x,
                    y,
                    cell: next_buffer[i].clone(),
                });
            }
        }

        to_skip = display_width(current.symbol()).saturating_sub(1);

        let affected_width = std::cmp::max(
            display_width(current.symbol()),
            display_width(previous.symbol()),
        );
        invalidated = std::cmp::max(affected_width, invalidated).saturating_sub(1);
    }
    updates
}

fn draw<I>(writer: &mut impl Write, commands: I) -> io::Result<()>
where
    I: Iterator<Item = DrawCommand>,
{
    let mut fg = Color::Reset;
    let mut bg = Color::Reset;
    let mut modifier = Modifier::empty();
    let mut last_pos: Option<Position> = None;
    for command in commands {
        let (x, y) = match command {
            DrawCommand::Cell { x, y, .. } => (x, y),
            DrawCommand::ClearTrailingCells { x, y, .. } => (x, y),
        };
        if !matches!(last_pos, Some(p) if x == p.x + 1 && y == p.y) {
            queue!(writer, MoveTo(x, y))?;
        }
        last_pos = Some(Position { x, y });
        match command {
            DrawCommand::Cell { cell, .. } => {
                if cell.modifier != modifier {
                    let diff = SgrModifierChange {
                        from: modifier,
                        to: cell.modifier,
                    };
                    diff.queue(writer)?;
                    modifier = cell.modifier;
                }
                if cell.fg != fg || cell.bg != bg {
                    queue!(
                        writer,
                        SetColors(Colors::new(
                            cell.fg.into_crossterm(),
                            cell.bg.into_crossterm()
                        ))
                    )?;
                    fg = cell.fg;
                    bg = cell.bg;
                }
                queue!(writer, Print(cell.symbol()))?;
            }
            DrawCommand::ClearTrailingCells { bg: clear_bg, .. } => {
                queue!(writer, SetAttribute(crossterm::style::Attribute::Reset))?;
                modifier = Modifier::empty();
                queue!(writer, SetBackgroundColor(clear_bg.into_crossterm()))?;
                bg = clear_bg;
                queue!(writer, Clear(crossterm::terminal::ClearType::UntilNewLine))?;
            }
        }
    }

    queue!(
        writer,
        SetForegroundColor(crossterm::style::Color::Reset),
        SetBackgroundColor(crossterm::style::Color::Reset),
        SetAttribute(crossterm::style::Attribute::Reset),
    )?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::style::Style;

    #[test]
    fn display_width_ascii_is_column_count() {
        assert_eq!(display_width("hello"), 5);
        assert_eq!(display_width(""), 0);
    }

    #[test]
    fn display_width_counts_wide_glyphs_correctly() {
        // CJK glyphs occupy 2 display columns each.
        assert_eq!(display_width("中"), 2);
        assert_eq!(display_width("中文"), 4);
    }

    #[test]
    fn display_width_strips_osc_hyperlinks() {
        // OSC 8 hyperlink payload (ESC ] … BEL) shouldn't contribute to
        // display width — only the visible "link" text does. Without the
        // OSC stripping `UnicodeWidthStr::width` would count the URL bytes.
        let s = "\x1b]8;;https://example.com\x07link\x1b]8;;\x07";
        assert_eq!(display_width(s), "link".len());
    }

    #[test]
    fn diff_buffers_emits_put_for_changed_cell() {
        let area = Rect::new(0, 0, 3, 1);
        let previous = Buffer::empty(area);
        let mut next = Buffer::empty(area);
        next.set_string(0, 0, "abc", Style::default());
        let commands = diff_buffers(&previous, &next);
        let puts = commands
            .iter()
            .filter(|c| matches!(c, DrawCommand::Cell { .. }))
            .count();
        assert_eq!(puts, 3, "one Put per changed cell; got {commands:?}");
    }

    #[test]
    fn diff_buffers_skips_unchanged_rows() {
        let area = Rect::new(0, 0, 4, 2);
        let mut previous = Buffer::empty(area);
        let mut next = Buffer::empty(area);
        previous.set_string(0, 0, "same", Style::default());
        next.set_string(0, 0, "same", Style::default());
        next.set_string(0, 1, "diff", Style::default());
        let commands = diff_buffers(&previous, &next);
        let put_ys: Vec<u16> = commands
            .iter()
            .filter_map(|c| match c {
                DrawCommand::Cell { y, .. } => Some(*y),
                _ => None,
            })
            .collect();
        assert!(
            put_ys.iter().all(|&y| y == 1),
            "row 0 was unchanged; all Puts should be row 1: {put_ys:?}"
        );
    }

    #[test]
    fn diff_buffers_uses_clear_to_end_for_blank_tail() {
        // A row whose tail is blank should use ClearToEnd rather than a
        // stream of space Puts — that's the optimisation lifted from the
        // Codex-derived diff path.
        let area = Rect::new(0, 0, 20, 1);
        let previous = Buffer::empty(area);
        let mut next = Buffer::empty(area);
        next.set_string(0, 0, "hi", Style::default());
        let commands = diff_buffers(&previous, &next);
        assert!(
            commands
                .iter()
                .any(|c| matches!(c, DrawCommand::ClearTrailingCells { .. })),
            "expected a ClearToEnd for the blank tail; got {commands:?}",
        );
    }
}
