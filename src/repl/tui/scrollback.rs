//! Push captured stdout/stderr into the terminal's native scrollback,
//! above the inline viewport.
//!
//! The public entry point is [`scroll_strings_above_viewport`], which
//! accepts the `Vec<String>` (ANSI-embedded) batches produced by
//! [`super::output::OutputCapture`]. The pipeline is:
//!
//! 1. Join the batch with `\n`, parse through `ansi-to-tui` into a
//!    styled [`Text`].
//! 2. Pre-wrap the text into a scratch [`Buffer`] via
//!    [`Paragraph::render`] so every row is ≤ `wrap_width` cells and
//!    line breaks land at ratatui's word boundaries (not the
//!    emulator's).
//! 3. Rebuild a [`Line`] per buffer row and emit each one span-by-span
//!    via [`emit_history_line`]. Writing span *content* (variable
//!    length) instead of full `width`-cell rows keeps the terminal
//!    cursor out of DECAWM "pending-wrap" state, which we found to
//!    otherwise produce drifting columns on Ghostty.
//! 4. Before the loop, shift the viewport down via DECSTBM reverse-
//!    index if the screen has room; after the loop the viewport sits
//!    just below the new history rows.
//!
//! The DECSTBM scroll-region + reverse-index approach to advancing
//! the above-viewport region is patterned on the OpenAI Codex CLI's
//! `insert_history_lines`
//! (<https://github.com/openai/codex/blob/main/codex-rs/tui/src/insert_history.rs>,
//! Apache-2.0); the sofos implementation drops the Zellij fallback
//! and Codex's `adaptive_wrap_line` (URL-preserving wrapper) in
//! favour of ratatui's `Paragraph::wrap`.

use std::fmt;
use std::io;
use std::io::Write;

use ansi_to_tui::IntoText;
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
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::layout::Size;
use ratatui::prelude::Backend;
use ratatui::prelude::IntoCrossterm;
use ratatui::style::Color;
use ratatui::style::Modifier;
use ratatui::style::Style;
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{Paragraph, Widget, Wrap};

use super::sgr::SgrModifierChange;

/// Insert captured output lines above the inline viewport.
///
/// Lines are concatenated with `\n`, parsed for ANSI, then emitted
/// using a DECSTBM scroll region confined to the rows above the
/// viewport. The viewport's `y` position is shifted down if there's
/// room below it, so the inline viewport effectively stays anchored
/// near the bottom of the terminal while content streams in.
pub fn scroll_strings_above_viewport<B>(
    terminal: &mut crate::repl::tui::inline_terminal::Terminal<B>,
    captured_lines: &[String],
) -> io::Result<()>
where
    B: Backend<Error = io::Error> + Write,
{
    if captured_lines.is_empty() {
        return Ok(());
    }

    // Join and parse once so SGR state flows across line boundaries
    // (already guaranteed upstream by `output::SgrState`). Pad the tail
    // so a blank trailing line from `println!()` still renders a row.
    let mut joined = captured_lines.join("\n");
    joined.push('\n');
    let text: Text<'static> = joined
        .as_bytes()
        .into_text()
        .unwrap_or_else(|_| Text::from(joined.clone()));

    scroll_text_above_viewport(terminal, text)
}

/// Shared implementation for [`scroll_strings_above_viewport`]: runs
/// Phase 1 (optional DECSTBM reverse-index to shift the viewport down)
/// and Phase 2 (paint each pre-wrapped row above the viewport via a
/// scroll region).
fn scroll_text_above_viewport<B>(
    terminal: &mut crate::repl::tui::inline_terminal::Terminal<B>,
    text: Text<'static>,
) -> io::Result<()>
where
    B: Backend<Error = io::Error> + Write,
{
    let screen_size = terminal.backend().size().unwrap_or(Size::new(0, 0));
    if screen_size.width == 0 || screen_size.height == 0 {
        return Ok(());
    }

    let mut area = terminal.viewport_area;
    if area.width == 0 {
        return Ok(());
    }

    let wrap_width = area.width.max(1);
    // Pre-wrap the text into a scratch `Buffer` via `Paragraph::render`
    // so every row is guaranteed ≤ `wrap_width` cells and line breaks
    // land at ratatui's word boundaries (not the emulator's). We then
    // rebuild a `Line` from each row of the buffer and emit it via
    // `emit_history_line` — span-based, variable-length, DECAWM-safe.
    // Doing the wrap ahead of time instead of relying on terminal
    // auto-wrap is what keeps per-row cursor positioning deterministic
    // across Ghostty / iTerm / Terminal.app.
    let wrapped_paragraph = Paragraph::new(text.clone()).wrap(Wrap { trim: false });
    let wrapped_row_count = wrapped_paragraph.line_count(wrap_width).max(1);
    let wrapped_rows = u16::try_from(wrapped_row_count).unwrap_or(u16::MAX);

    let buf_rect = Rect::new(0, 0, wrap_width, wrapped_rows);
    let mut buf = Buffer::empty(buf_rect);
    Paragraph::new(text)
        .wrap(Wrap { trim: false })
        .render(buf_rect, &mut buf);

    let wrapped_lines: Vec<Line<'static>> = (0..wrapped_rows)
        .map(|row| buffer_row_as_line(&buf, row, wrap_width))
        .collect();

    let last_cursor_pos = terminal.last_known_cursor_pos;
    let writer = terminal.backend_mut();

    let mut should_update_area = false;
    // Phase 1: if the viewport isn't flush with the screen bottom, shift
    // it down using DECSTBM + reverse index so new rows appear between
    // the existing scrollback and the viewport.
    let cursor_top = if area.bottom() < screen_size.height {
        let scroll_amount = wrapped_rows.min(screen_size.height - area.bottom());
        let top_1based = area.top() + 1;
        queue!(writer, SetScrollRegion(top_1based..screen_size.height))?;
        queue!(writer, MoveTo(0, area.top()))?;
        for _ in 0..scroll_amount {
            queue!(writer, Print("\x1bM"))?; // reverse index
        }
        queue!(writer, ResetScrollRegion)?;
        let cursor_top = area.top().saturating_sub(1);
        area.y = area.y.saturating_add(scroll_amount);
        should_update_area = true;
        cursor_top
    } else {
        area.top().saturating_sub(1)
    };

    // Phase 2 — install a DECSTBM scroll region covering rows
    // 1..area.top() (1-based inclusive, i.e. every row above the
    // viewport). With the cursor parked at the bottom of the region,
    // each `\r\n` scrolls the region up by one row (the oldest row
    // falls into the emulator's native scrollback on Ghostty /
    // iTerm2) and we paint the next pre-wrapped line in its place.
    // Every line is ≤ `wrap_width` by construction, so
    // `emit_history_line` writes a variable-length payload that
    // doesn't park the cursor in DECAWM "pending-wrap" state.
    if area.top() > 0 {
        queue!(writer, SetScrollRegion(1..area.top()))?;
        queue!(writer, MoveTo(0, cursor_top))?;
        for line in wrapped_lines.iter() {
            queue!(writer, Print("\r\n"))?;
            emit_history_line(writer, line, wrap_width as usize)?;
        }
        queue!(writer, ResetScrollRegion)?;
    }
    let _ = cursor_top;

    // Restore the real cursor so ratatui's diff engine still believes
    // the cursor is wherever it placed it after the previous draw.
    queue!(writer, MoveTo(last_cursor_pos.x, last_cursor_pos.y))?;
    std::io::Write::flush(writer)?;

    if should_update_area {
        terminal.set_viewport_area(area);
    }
    if wrapped_rows > 0 {
        terminal.record_history_rows(wrapped_rows);
    }
    Ok(())
}

/// Rebuild a styled [`Line`] from row `row` of `buf`, grouping
/// consecutive cells with identical style into single [`Span`]s and
/// trimming trailing default-style blanks. The trim is what lets
/// [`emit_history_line`] write ≤ `wrap_width - 1` printable cells
/// per row, keeping the cursor out of DECAWM "pending-wrap" state
/// after the write finishes.
fn buffer_row_as_line(buf: &Buffer, row: u16, width: u16) -> Line<'static> {
    let last_content_col = (0..width).rev().find(|&col| {
        let cell = &buf[(col, row)];
        cell.symbol() != " " || cell.bg != Color::Reset || cell.modifier != Modifier::empty()
    });
    let Some(end) = last_content_col else {
        return Line::from("");
    };

    let mut spans: Vec<Span<'static>> = Vec::new();
    let mut run_style: Option<Style> = None;
    let mut run_text = String::new();
    for col in 0..=end {
        let cell = &buf[(col, row)];
        let cell_style = Style::default()
            .fg(cell.fg)
            .bg(cell.bg)
            .add_modifier(cell.modifier);
        match run_style {
            Some(style) if style == cell_style => run_text.push_str(cell.symbol()),
            _ => {
                if let Some(style) = run_style.take() {
                    spans.push(Span::styled(run_text.clone(), style));
                    run_text.clear();
                }
                run_style = Some(cell_style);
                run_text.push_str(cell.symbol());
            }
        }
    }
    if let Some(style) = run_style {
        spans.push(Span::styled(run_text, style));
    }
    Line::from(spans)
}

/// Emit one logical line to `writer`, letting the terminal handle
/// any wrapping past `wrap_width`. Pre-clears the physical
/// continuation rows the wrap will land on — otherwise the
/// auto-wrapped tail paints over whatever was there before.
///
/// The line-style patch + span merge is what makes styled blockquotes
/// (e.g. our italic "Thinking:" blocks) render with their fg intact
/// across the whole wrapped line.
fn emit_history_line<W: Write>(writer: &mut W, line: &Line, wrap_width: usize) -> io::Result<()> {
    // Defensive `MoveToColumn(0)`: the caller's `\r\n` *should* land
    // cursor at col 0 of the next row, but on emulators that defer
    // DECAWM ("pending wrap") state the `\r` sometimes doesn't cancel
    // it, so the next print lands one row further down than we think.
    // Symptom in the wild: first char of a line missing ("When…" →
    // "hen…") or two lines concatenated ("fn" + "This program:" →
    // "fnThis program:"). Emitting CSI col-0 unconditionally puts us
    // in a known position no matter what DECAWM state the last row
    // left behind.
    queue!(writer, MoveToColumn(0))?;

    let physical_rows = (line.width().max(1).div_ceil(wrap_width)) as u16;
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
            line.style
                .fg
                .map(IntoCrossterm::into_crossterm)
                .unwrap_or(CColor::Reset),
            line.style
                .bg
                .map(IntoCrossterm::into_crossterm)
                .unwrap_or(CColor::Reset)
        ))
    )?;
    queue!(writer, Clear(ClearType::UntilNewLine))?;
    let merged_spans: Vec<Span<'_>> = line
        .spans
        .iter()
        .map(|s| Span {
            style: s.style.patch(line.style),
            content: s.content.clone(),
        })
        .collect();
    emit_styled_spans(writer, merged_spans.iter())
}

/// Emit a sequence of styled spans, tracking the previously-applied
/// fg / bg / modifier so we don't reset SGR between every cell.
fn emit_styled_spans<'a, W, I>(mut writer: W, spans: I) -> io::Result<()>
where
    W: Write,
    I: IntoIterator<Item = &'a Span<'a>>,
{
    let mut fg = Color::Reset;
    let mut bg = Color::Reset;
    let mut last_modifier = Modifier::empty();
    for span in spans {
        let mut modifier = Modifier::empty();
        modifier.insert(span.style.add_modifier);
        modifier.remove(span.style.sub_modifier);
        if modifier != last_modifier {
            let diff = SgrModifierChange {
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

        queue!(writer, Print(span.content.as_ref()))?;
    }
    queue!(
        writer,
        SetForegroundColor(CColor::Reset),
        SetBackgroundColor(CColor::Reset),
        SetAttribute(crossterm::style::Attribute::Reset),
    )
}

/// DECSTBM: `ESC [ top ; bottom r` — restrict scrolling to rows
/// `start..end` (1-based, inclusive on both ends). Zellij silently
/// drops DECSTBM, so users running sofos under Zellij will see
/// scrollback regions misbehave; handling that would mean adding an
/// emulator-detection + raw-newline fallback and we haven't found a
/// sofos user hitting it.
#[derive(Debug, Clone, PartialEq, Eq)]
struct SetScrollRegion(std::ops::Range<u16>);

impl Command for SetScrollRegion {
    fn write_ansi(&self, f: &mut impl fmt::Write) -> fmt::Result {
        write!(f, "\x1b[{};{}r", self.0.start, self.0.end)
    }

    #[cfg(windows)]
    fn execute_winapi(&self) -> std::io::Result<()> {
        panic!("SetScrollRegion via WinAPI unsupported — use ANSI");
    }

    #[cfg(windows)]
    fn is_ansi_code_supported(&self) -> bool {
        true
    }
}

/// `ESC [ r` — restore the default (full-screen) scroll region.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ResetScrollRegion;

impl Command for ResetScrollRegion {
    fn write_ansi(&self, f: &mut impl fmt::Write) -> fmt::Result {
        write!(f, "\x1b[r")
    }

    #[cfg(windows)]
    fn execute_winapi(&self) -> std::io::Result<()> {
        panic!("ResetScrollRegion via WinAPI unsupported — use ANSI");
    }

    #[cfg(windows)]
    fn is_ansi_code_supported(&self) -> bool {
        true
    }
}
