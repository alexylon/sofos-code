use crate::ui::UI;
use colored::Colorize;
use pulldown_cmark::Alignment;
use std::io::{self, Write, stdout};
use unicode_width::UnicodeWidthStr;

/// SGR code for bold-on. Shared between the markdown Strong Start handler
/// and the ambient-style restorer so the two never drift apart.
const SGR_BOLD: &str = "\x1b[1m";
/// SGR code for italic-on. Shared between Emphasis Start and the restorer.
const SGR_ITALIC: &str = "\x1b[3m";
/// SGR code for the markdown heading style (bold + cyan). Shared between
/// Heading Start and the restorer; restoring just `\x1b[36m` would silently
/// drop the bold half.
const SGR_HEADING: &str = "\x1b[1;36m";
/// SGR faint. Used for blockquote bodies and for the ambient dim of a
/// streamed thinking block. `\x1b[22m` and `\x1b[0m` both clear faint,
/// so the restorer re-applies it after those resets.
const SGR_FAINT: &str = "\x1b[2m";

/// Re-apply the ambient faint that a block-level SGR reset clears, so a
/// dimmed thinking block stays dim after content that ends on a reset.
fn restore_faint(out: &mut impl io::Write, dimmed: bool) -> io::Result<()> {
    if dimmed {
        write!(out, "{}", SGR_FAINT)?;
    }
    Ok(())
}

impl UI {
    pub fn print_markdown_highlighted(&self, md: &str) -> io::Result<()> {
        let mut out = stdout().lock();
        self.render_markdown_to(&mut out, md, false)?;
        out.flush()
    }

    /// Render `md` to `out` as ANSI-styled text. With `dimmed`, faint is
    /// re-applied after each internal SGR reset so streamed thinking
    /// stays dim across inline code, links, and headings.
    pub(super) fn render_markdown_to(
        &self,
        out: &mut impl io::Write,
        md: &str,
        dimmed: bool,
    ) -> io::Result<()> {
        use pulldown_cmark::{CodeBlockKind, Event, Options, Parser, Tag, TagEnd};

        // Re-emit any ambient inline styles after a full SGR reset, so nested inline
        // tags (Code/Link) and Strong don't leave the outer heading/strong/emphasis bare.
        fn restore_ambient(
            out: &mut impl io::Write,
            bold: bool,
            italic: bool,
            in_heading: bool,
            in_blockquote: bool,
            dimmed: bool,
        ) -> io::Result<()> {
            if bold {
                write!(out, "{}", SGR_BOLD)?;
            }
            if italic {
                write!(out, "{}", SGR_ITALIC)?;
            }
            if in_heading {
                write!(out, "{}", SGR_HEADING)?;
            }
            if in_blockquote || dimmed {
                write!(out, "{}", SGR_FAINT)?;
            }
            Ok(())
        }

        let parser = Parser::new_ext(md, Options::all());

        let mut in_code_block = false;
        let mut code_lang = String::new();
        let mut code_buf = String::new();
        let mut bold = false;
        let mut italic = false;
        let mut in_heading = false;
        let mut in_blockquote = false;
        let mut in_table = false;
        let mut table_aligns: Vec<Alignment> = Vec::new();
        let mut table_rows: Vec<Vec<String>> = Vec::new();
        let mut current_cell = String::new();

        for event in parser {
            match event {
                Event::Start(Tag::Heading { .. }) => {
                    in_heading = true;
                    write!(out, "{}", SGR_HEADING)?;
                }
                Event::End(TagEnd::Heading(_)) => {
                    in_heading = false;
                    write!(out, "\x1b[0m")?;
                    restore_faint(out, dimmed)?;
                    writeln!(out)?;
                }
                Event::Start(Tag::Strong) => {
                    bold = true;
                    write!(out, "{}", SGR_BOLD)?;
                }
                Event::End(TagEnd::Strong) => {
                    bold = false;
                    write!(out, "\x1b[22m")?;
                    restore_ambient(out, bold, italic, in_heading, in_blockquote, dimmed)?;
                }
                Event::Start(Tag::Emphasis) => {
                    italic = true;
                    write!(out, "{}", SGR_ITALIC)?;
                }
                Event::End(TagEnd::Emphasis) => {
                    italic = false;
                    write!(out, "\x1b[23m")?;
                }
                Event::Start(Tag::CodeBlock(kind)) => {
                    in_code_block = true;
                    code_buf.clear();
                    code_lang = match kind {
                        CodeBlockKind::Fenced(lang) => lang.to_string(),
                        _ => String::new(),
                    };
                }
                Event::End(TagEnd::CodeBlock) => {
                    in_code_block = false;
                    let highlighted = self.highlighter.highlight_code(&code_buf, &code_lang);
                    write!(out, "{}", highlighted)?;
                    restore_faint(out, dimmed)?;
                    writeln!(out)?;
                }
                Event::Code(code) => {
                    if in_table {
                        current_cell.push_str(&code);
                    } else {
                        write!(out, "\x1b[38;2;175;215;255m{}\x1b[0m", code)?;
                        restore_ambient(out, bold, italic, in_heading, in_blockquote, dimmed)?;
                    }
                }
                Event::Text(text) => {
                    if in_code_block {
                        code_buf.push_str(&text);
                    } else if in_table {
                        current_cell.push_str(&text);
                    } else {
                        write!(out, "{}", text)?;
                    }
                }
                Event::SoftBreak => {
                    if in_table {
                        current_cell.push(' ');
                    } else if !in_code_block {
                        writeln!(out)?;
                    }
                }
                Event::HardBreak => {
                    if in_table {
                        current_cell.push(' ');
                    } else {
                        writeln!(out)?;
                    }
                }
                Event::Start(Tag::Paragraph) => {}
                Event::End(TagEnd::Paragraph) => {
                    writeln!(out)?;
                    writeln!(out)?;
                }
                Event::Start(Tag::List(_)) => {}
                Event::End(TagEnd::List(_)) => {}
                Event::Start(Tag::Item) => {
                    write!(out, "  {} ", "•".dimmed())?;
                    restore_faint(out, dimmed)?;
                }
                Event::End(TagEnd::Item) => {
                    writeln!(out)?;
                }
                Event::Start(Tag::BlockQuote(_)) => {
                    in_blockquote = true;
                    write!(out, "{}> ", SGR_FAINT)?;
                }
                Event::End(TagEnd::BlockQuote(_)) => {
                    in_blockquote = false;
                    write!(out, "\x1b[0m")?;
                    restore_faint(out, dimmed)?;
                    writeln!(out)?;
                }
                Event::Start(Tag::Link { dest_url, .. }) => {
                    // OSC 8 URI terminates on BEL/ESC; bypass the wrapper if dest_url has any control byte.
                    if dest_url.chars().any(|c| c.is_ascii_control()) {
                        write!(out, "\x1b[4;34m")?;
                    } else {
                        write!(out, "\x1b]8;;{}\x07\x1b[4;34m", dest_url)?;
                    }
                }
                Event::End(TagEnd::Link) => {
                    write!(out, "\x1b[0m\x1b]8;;\x07")?;
                    restore_ambient(out, bold, italic, in_heading, in_blockquote, dimmed)?;
                }
                Event::Start(Tag::Table(aligns)) => {
                    in_table = true;
                    table_aligns = aligns;
                    table_rows.clear();
                }
                Event::Start(Tag::TableHead) | Event::Start(Tag::TableRow) => {
                    table_rows.push(Vec::new());
                }
                Event::Start(Tag::TableCell) => {
                    current_cell.clear();
                }
                Event::End(TagEnd::TableCell) => {
                    if let Some(row) = table_rows.last_mut() {
                        row.push(std::mem::take(&mut current_cell));
                    }
                }
                Event::End(TagEnd::Table) => {
                    in_table = false;
                    write_table(out, &table_rows, &table_aligns, dimmed)?;
                    // Separate the table from the next block with a blank line;
                    // the rows are already newline-terminated by write_table.
                    writeln!(out)?;
                }
                Event::Rule => {
                    write!(out, "{}", "─".repeat(40).dimmed())?;
                    restore_faint(out, dimmed)?;
                    writeln!(out)?;
                }
                _ => {}
            }
        }

        Ok(())
    }
}

/// Return the byte offset of the last newline in `buf` that is **not**
/// inside an open fenced code block or an in-progress table. Lines opening
/// with ``` toggle the fence state; the trailing newline of an open-fence
/// line is therefore not a safe commit point. A table is held from its
/// header row until the blank line that closes it, because aligned column
/// widths depend on every row and a partial table, once committed, can't
/// be redrawn at the final widths. A potential header — a pipe-bearing
/// line that is the last line, or is followed only by a partial delimiter
/// row still streaming in — is held too, so it is never committed as a
/// paragraph and then redrawn as a table.
///
/// Returns 0 when no safe newline exists yet (caller commits nothing).
///
/// **Known limitation:** the toggle treats any line whose first
/// non-space character is ``` as a fence boundary regardless of the
/// opening fence length. CommonMark allows nesting via longer fences
/// (e.g., a 4-backtick block containing a 3-backtick line), and this
/// algorithm miscounts those — pulldown still renders correctly, but
/// the safe-commit point may be conservative or skewed inside such
/// blocks. Rare in assistant output; accept the limitation.
fn safe_commit_end(buf: &str) -> usize {
    let lines: Vec<&str> = buf.split_inclusive('\n').collect();
    let mut fence_open = false;
    let mut in_table = false;
    let mut last_safe = 0usize;
    let mut pos = 0usize;
    for (i, line) in lines.iter().enumerate() {
        if is_fence_line(line) {
            fence_open = !fence_open;
        }
        if !fence_open {
            if in_table {
                if line.trim().is_empty() {
                    in_table = false;
                }
            } else if lines
                .get(i + 1)
                .is_some_and(|next| is_table_delimiter(next))
            {
                // This line is a table header: its successor is the delimiter row.
                in_table = true;
            }
        }
        pos += line.len();
        if line.ends_with('\n') && !fence_open && !in_table && !pending_table_header(&lines, i) {
            last_safe = pos;
        }
    }
    last_safe
}

/// True when `line` opens or closes a fenced code block under CommonMark
/// indentation rules: at most three leading spaces, then either ``` or
/// ~~~. A line with four or more leading spaces is part of an indented
/// code block instead, even if its first non-space content is a fence.
fn is_fence_line(line: &str) -> bool {
    let after_spaces = line.trim_start_matches(' ');
    let indent = line.len() - after_spaces.len();
    indent <= 3 && (after_spaces.starts_with("```") || after_spaces.starts_with("~~~"))
}

/// True when `line` is a GFM table delimiter row — the `|---|:--:|` line
/// beneath a header. Requires a pipe so a thematic break or setext
/// underline (`---`) is not mistaken for one.
fn is_table_delimiter(line: &str) -> bool {
    if !line.contains('|') {
        return false;
    }
    let inner = line.trim().trim_start_matches('|').trim_end_matches('|');
    !inner.is_empty()
        && inner.split('|').all(|cell| {
            let trimmed = cell.trim();
            !trimmed.is_empty()
                && trimmed.contains('-')
                && trimmed.bytes().all(|b| b == b'-' || b == b':')
        })
}

/// True when line `i` may be a table header whose delimiter row has not
/// finished streaming, so committing it now would show it as a paragraph
/// and then redraw it as a table. Holds when the header is the last line
/// (the delimiter has not started) or the only line after it is a partial
/// delimiter row still being received. Only a pipe-bearing line qualifies,
/// because pulldown does not treat a pipe-less line as a table header.
fn pending_table_header(lines: &[&str], i: usize) -> bool {
    if !lines[i].contains('|') {
        return false;
    }
    match lines.get(i + 1) {
        None => true,
        Some(next) => i + 2 == lines.len() && !next.ends_with('\n') && is_delimiter_prefix(next),
    }
}

/// True when `s`, a partial line still being streamed, contains only the
/// characters a delimiter row is built from, so it may still grow into one.
fn is_delimiter_prefix(s: &str) -> bool {
    let trimmed = s.trim();
    !trimmed.is_empty()
        && trimmed
            .bytes()
            .all(|b| matches!(b, b'|' | b'-' | b':' | b' '))
}

/// Render a parsed table as aligned, box-drawn columns. Column widths are
/// the max display width per column across every row; the header (row 0)
/// is followed by a `─┼─` rule. Cells carry plain text only — inline
/// styling inside a cell is flattened — so padding is computed on visible
/// width without having to discount ANSI escapes.
fn write_table(
    out: &mut impl io::Write,
    rows: &[Vec<String>],
    aligns: &[Alignment],
    dimmed: bool,
) -> io::Result<()> {
    let col_count = rows.iter().map(|row| row.len()).max().unwrap_or(0);
    if col_count == 0 {
        return Ok(());
    }
    let mut widths = vec![0usize; col_count];
    for row in rows {
        for (col, cell) in row.iter().enumerate() {
            widths[col] = widths[col].max(cell.width());
        }
    }
    let sep = format!(" {} ", "│".dimmed());
    for (row_idx, row) in rows.iter().enumerate() {
        let mut line = String::new();
        for (col, &width) in widths.iter().enumerate() {
            if col > 0 {
                line.push_str(&sep);
            }
            let cell = row.get(col).map_or("", |s| s.as_str());
            let align = aligns.get(col).copied().unwrap_or(Alignment::None);
            line.push_str(&pad_cell(cell, width, align));
        }
        writeln!(out, "{}", line.trim_end())?;
        restore_faint(out, dimmed)?;
        if row_idx == 0 {
            let rule: Vec<String> = widths.iter().map(|width| "─".repeat(*width)).collect();
            writeln!(out, "{}", rule.join("─┼─").dimmed())?;
            restore_faint(out, dimmed)?;
        }
    }
    Ok(())
}

/// Pad `text` to `width` display columns per `align` (`None` renders left).
fn pad_cell(text: &str, width: usize, align: Alignment) -> String {
    let fill = width.saturating_sub(text.width());
    match align {
        Alignment::Right => format!("{}{text}", " ".repeat(fill)),
        Alignment::Center => {
            let left = fill / 2;
            format!("{}{text}{}", " ".repeat(left), " ".repeat(fill - left))
        }
        Alignment::Left | Alignment::None => format!("{text}{}", " ".repeat(fill)),
    }
}

/// Newline-gated markdown renderer for streaming output. Accumulates
/// deltas, and on every commit re-renders the full buffer prefix up to
/// the last newline as markdown, emitting only the lines past what's
/// already been printed. The partial last line is held back until the
/// next newline or [`finalize`] arrives.
///
/// Modelled after `codex-rs/tui/src/markdown_stream.rs` — same invariant
/// (no commit until newline), same convergence property (the sum of
/// streamed emissions equals what a single non-streaming render of the
/// full buffer would produce). The cost is one full re-render per
/// committed chunk, which is acceptable because pulldown_cmark is fast
/// and assistant turns rarely exceed a few KB.
/// Above this buffer length, `commit` batches incremental updates
/// instead of re-rendering on every new line. Streaming a 50 KB
/// reply with one re-render per line otherwise costs O(N^2) work in
/// the markdown renderer; batching at this threshold keeps the
/// experience snappy for short replies (where re-rendering each
/// line is cheap) and linear for long ones.
const COMMIT_THROTTLE_BUFFER_BYTES: usize = 16 * 1024;
/// Once the buffer crosses [`COMMIT_THROTTLE_BUFFER_BYTES`], wait
/// until at least this many bytes have been appended since the last
/// commit before re-rendering again.
const COMMIT_THROTTLE_STEP_BYTES: usize = 1024;

pub(super) struct MarkdownStreamRenderer {
    buffer: String,
    committed_lines: usize,
    /// Byte offset of the last `safe_end` that was actually emitted —
    /// used by the throttle to decide whether enough new content has
    /// arrived to justify another full re-render.
    last_safe_end: usize,
    /// Passed to the renderer so thinking output keeps its ambient dim.
    dimmed: bool,
}

impl MarkdownStreamRenderer {
    pub(super) fn new() -> Self {
        Self {
            buffer: String::new(),
            committed_lines: 0,
            last_safe_end: 0,
            dimmed: false,
        }
    }

    /// Like [`new`](Self::new), but renders for a dimmed thinking block.
    pub(super) fn new_dimmed() -> Self {
        Self {
            dimmed: true,
            ..Self::new()
        }
    }

    pub(super) fn push_delta(&mut self, delta: &str) {
        self.buffer.push_str(delta);
    }

    /// Render the buffer prefix up to the last "safe" newline — one
    /// that's outside any open fenced code block — and return only the
    /// lines past `committed_lines`. Returns an empty string when there
    /// is no safe commit point yet.
    ///
    /// Fence-awareness matters because the markdown renderer synthesises
    /// a closing border for an unclosed code fence; committing that
    /// premature border leaves a wrong line on screen that we can't
    /// unprint when the real closing fence arrives.
    pub(super) fn commit(&mut self) -> io::Result<String> {
        let safe_end = safe_commit_end(&self.buffer);
        if safe_end == 0 {
            return Ok(String::new());
        }
        // Throttle once the buffer gets large: hold off re-rendering
        // until at least `COMMIT_THROTTLE_STEP_BYTES` of new content
        // have arrived since the last actual commit. Short turns
        // (under `COMMIT_THROTTLE_BUFFER_BYTES`) re-render per line
        // and stream as fluidly as before; long turns stay linear in
        // total work.
        if self.buffer.len() > COMMIT_THROTTLE_BUFFER_BYTES {
            let new_bytes = safe_end.saturating_sub(self.last_safe_end);
            if new_bytes < COMMIT_THROTTLE_STEP_BYTES {
                return Ok(String::new());
            }
        }
        // Drop the trailing blank during commit: pulldown's render is
        // not monotonic when a paragraph stays open across deltas (the
        // End-of-Paragraph blank "moves" further down as more text
        // arrives). Holding that trailing blank back means continuing
        // text gets emitted on the next commit instead of being lost
        // behind a prematurely-committed paragraph terminator.
        let (new, total) = self.render_new_lines(&self.buffer[..safe_end], true)?;
        self.committed_lines = total;
        self.last_safe_end = safe_end;
        Ok(new)
    }

    /// Emit the residual: anything past `committed_lines`, including the
    /// partial last line and any trailing blank line that `commit`
    /// deliberately held back. Resets internal state so the renderer
    /// can be reused for the next stream.
    pub(super) fn finalize(&mut self) -> io::Result<String> {
        let mut source = self.buffer.clone();
        // pulldown_cmark needs the trailing newline to close the last
        // paragraph; without it the final line renders as if it were
        // still being built.
        if !source.ends_with('\n') {
            source.push('\n');
        }
        let (new, _) = self.render_new_lines(&source, false)?;
        self.buffer.clear();
        self.committed_lines = 0;
        self.last_safe_end = 0;
        Ok(new)
    }

    /// Render `source` and return the slice of rendered lines past
    /// `committed_lines` plus the post-drop total line count. The total
    /// is what `commit` writes back to `self.committed_lines`; `finalize`
    /// drops it because it resets the counter anyway. When
    /// `drop_trailing_blank` is true a trailing whitespace-only line is
    /// excluded from both the emitted slice and the returned total —
    /// see [`commit`] for why.
    fn render_new_lines(
        &self,
        source: &str,
        drop_trailing_blank: bool,
    ) -> io::Result<(String, usize)> {
        let mut buf: Vec<u8> = Vec::new();
        UI::shared().render_markdown_to(&mut buf, source, self.dimmed)?;
        let rendered = String::from_utf8_lossy(&buf).into_owned();
        let lines: Vec<&str> = rendered.split_inclusive('\n').collect();
        let mut effective_len = lines.len();
        if drop_trailing_blank && effective_len > 0 && lines[effective_len - 1].trim().is_empty() {
            effective_len -= 1;
        }
        let new = if self.committed_lines >= effective_len {
            String::new()
        } else {
            lines[self.committed_lines..effective_len].concat()
        };
        Ok((new, effective_len))
    }
}

impl Default for MarkdownStreamRenderer {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod render_tests {
    use super::*;

    fn render(md: &str) -> String {
        let ui = UI::new();
        let mut buf = Vec::new();
        ui.render_markdown_to(&mut buf, md, false).unwrap();
        String::from_utf8(buf).unwrap()
    }

    fn render_dimmed(md: &str) -> String {
        let ui = UI::new();
        let mut buf = Vec::new();
        ui.render_markdown_to(&mut buf, md, true).unwrap();
        String::from_utf8(buf).unwrap()
    }

    #[test]
    fn link_emits_osc8_hyperlink_for_normal_url() {
        let out = render("[example](https://example.com)");
        assert!(
            out.contains("\x1b]8;;https://example.com\x07"),
            "OSC 8 opener with URL not found in: {:?}",
            out
        );
        assert!(out.contains("example"), "link text not found");
        assert!(
            out.contains("\x1b]8;;\x07"),
            "OSC 8 closer not found in: {:?}",
            out
        );
    }

    #[test]
    fn strong_in_heading_restores_heading_style() {
        let out = render("# title with **bold** rest");
        let after_strong_end = out
            .split("\x1b[22m")
            .nth(1)
            .expect("Strong End must emit \\x1b[22m");
        let rest_idx = after_strong_end
            .find(" rest")
            .expect("trailing text must be present");
        assert!(
            after_strong_end[..rest_idx].contains(SGR_HEADING),
            "heading style not restored between Strong End and trailing text; segment={:?}",
            &after_strong_end[..rest_idx]
        );
    }

    #[test]
    fn code_in_heading_restores_heading_style() {
        let out = render("# title with `code` rest");
        // Inline Code closes with \x1b[0m before restore_ambient runs.
        let after_code_reset = out
            .split("\x1b[0m")
            .nth(1)
            .expect("inline Code emits \\x1b[0m");
        let rest_idx = after_code_reset
            .find(" rest")
            .expect("trailing text must be present");
        assert!(
            after_code_reset[..rest_idx].contains(SGR_HEADING),
            "heading style not restored between inline Code and trailing text; segment={:?}",
            &after_code_reset[..rest_idx]
        );
    }

    #[test]
    fn link_in_heading_restores_heading_style() {
        let out = render("# title with [link](https://example.com) rest");
        let after_link_close = out
            .split("\x1b]8;;\x07")
            .nth(1)
            .expect("Link End must emit OSC 8 close");
        let rest_idx = after_link_close
            .find(" rest")
            .expect("trailing text must be present");
        assert!(
            after_link_close[..rest_idx].contains(SGR_HEADING),
            "heading style not restored between Link End and trailing text; segment={:?}",
            &after_link_close[..rest_idx]
        );
    }

    #[test]
    fn strong_in_blockquote_restores_dim() {
        let out = render("> **bold** rest");
        let after_strong_end = out
            .split("\x1b[22m")
            .nth(1)
            .expect("Strong End must emit \\x1b[22m");
        let rest_idx = after_strong_end
            .find(" rest")
            .expect("trailing text must be present");
        assert!(
            after_strong_end[..rest_idx].contains(SGR_FAINT),
            "blockquote dim not restored between Strong End and trailing text; segment={:?}",
            &after_strong_end[..rest_idx]
        );
    }

    #[test]
    fn code_in_blockquote_restores_dim() {
        let out = render("> `code` rest");
        // The blockquote's own dim opens with \x1b[2m before the inline Code event;
        // skip past that prefix so we land between Code's \x1b[0m and the trailing text.
        let after_code_reset = out
            .split("\x1b[0m")
            .nth(1)
            .expect("inline Code emits \\x1b[0m");
        let rest_idx = after_code_reset
            .find(" rest")
            .expect("trailing text must be present");
        assert!(
            after_code_reset[..rest_idx].contains(SGR_FAINT),
            "blockquote dim not restored between inline Code and trailing text; segment={:?}",
            &after_code_reset[..rest_idx]
        );
    }

    #[test]
    fn link_in_blockquote_restores_dim() {
        let out = render("> [link](https://example.com) rest");
        let after_link_close = out
            .split("\x1b]8;;\x07")
            .nth(1)
            .expect("Link End must emit OSC 8 close");
        let rest_idx = after_link_close
            .find(" rest")
            .expect("trailing text must be present");
        assert!(
            after_link_close[..rest_idx].contains(SGR_FAINT),
            "blockquote dim not restored between Link End and trailing text; segment={:?}",
            &after_link_close[..rest_idx]
        );
    }

    #[test]
    fn link_in_emphasis_restores_italic() {
        let out = render("*italic [link](https://example.com) rest*");
        let after_link_close = out
            .split("\x1b]8;;\x07")
            .nth(1)
            .expect("Link End must emit OSC 8 close");
        let rest_idx = after_link_close
            .find(" rest")
            .expect("trailing text must be present");
        assert!(
            after_link_close[..rest_idx].contains(SGR_ITALIC),
            "italic not restored between Link End and trailing text; segment={:?}",
            &after_link_close[..rest_idx]
        );
    }

    #[test]
    fn code_in_dimmed_restores_faint() {
        // Inline Code closes with \x1b[0m, wiping the thinking block's
        // ambient faint; the dimmed renderer must put it back.
        let out = render_dimmed("the readme uses `gulp watch` then more");
        let after_code_reset = out
            .split("\x1b[0m")
            .nth(1)
            .expect("inline Code emits \\x1b[0m");
        let rest_idx = after_code_reset
            .find(" then more")
            .expect("trailing text must be present");
        assert!(
            after_code_reset[..rest_idx].contains(SGR_FAINT),
            "faint not restored between inline Code and trailing text; segment={:?}",
            &after_code_reset[..rest_idx]
        );
    }

    #[test]
    fn strong_in_dimmed_restores_faint() {
        // Strong End emits \x1b[22m, which clears faint as well as bold.
        let out = render_dimmed("plain **bold** rest");
        let after_strong_end = out
            .split("\x1b[22m")
            .nth(1)
            .expect("Strong End must emit \\x1b[22m");
        let rest_idx = after_strong_end
            .find(" rest")
            .expect("trailing text must be present");
        assert!(
            after_strong_end[..rest_idx].contains(SGR_FAINT),
            "faint not restored between Strong End and trailing text; segment={:?}",
            &after_strong_end[..rest_idx]
        );
    }

    #[test]
    fn heading_end_in_dimmed_restores_faint() {
        // Heading End emits \x1b[0m; the paragraph after it must come
        // back dim.
        let out = render_dimmed("# Title\n\nbody text");
        let body_idx = out.find("body text").expect("body must be present");
        let last_reset = out[..body_idx]
            .rfind("\x1b[0m")
            .expect("Heading End emits \\x1b[0m");
        assert!(
            out[last_reset..body_idx].contains(SGR_FAINT),
            "faint not restored between Heading End and body; segment={:?}",
            &out[last_reset..body_idx]
        );
    }

    #[test]
    fn code_block_in_dimmed_restores_faint() {
        // A code block's syntax highlighting leaves the terminal on a
        // non-dim style; the prose after it must come back dim.
        let out = render_dimmed("```\nlet x = 1;\n```\n\nafter the block");
        assert!(
            out.contains("\x1b[2m\nafter the block"),
            "faint not restored between code block and trailing text; out={:?}",
            out
        );
    }

    #[test]
    fn list_item_in_dimmed_restores_faint() {
        // The bullet's own styling closes on a reset; the item text
        // after it must stay dim.
        let out = render_dimmed("- first item\n- second item");
        assert!(
            out.contains("\x1b[2mfirst item"),
            "faint not restored between bullet and item text; out={:?}",
            out
        );
    }

    #[test]
    fn table_renders_aligned_columns_with_separators() {
        let out = render("| Module | Tests |\n|--------|------:|\n| api | 120 |\n| repl | 5 |\n");
        // The header cells are no longer fused: a separator sits between them.
        assert!(
            !out.contains("ModuleTests"),
            "header cells must not be fused; out={:?}",
            out
        );
        assert!(out.contains("Module"), "header cell missing; out={:?}", out);
        assert!(out.contains('│'), "column separator missing; out={:?}", out);
        // A rule line separates the header from the body.
        assert!(out.contains('┼'), "header rule missing; out={:?}", out);
        // Body rows render their own cells rather than collapsing onto one line.
        assert!(out.contains("api"), "first body row missing; out={:?}", out);
        assert!(
            out.contains("repl"),
            "second body row missing; out={:?}",
            out
        );
        // The right-aligned numeric column pads on the left: "120" is the
        // widest cell, so "5" gets four leading spaces.
        assert!(
            out.contains("    5"),
            "right alignment not applied; out={:?}",
            out
        );
    }

    #[test]
    fn table_is_followed_by_blank_line() {
        // A table is separated from the following block by a blank line.
        let out = render("| A | B |\n|---|---|\n| 1 | 2 |\n\nNext.\n");
        assert!(
            out.contains("\n\nNext."),
            "blank line after table missing; out={:?}",
            out
        );
    }

    #[test]
    fn empty_table_body_still_shows_header() {
        // A header-only table (the "empty summary" case) renders the
        // header and rule instead of fusing the cells into one token.
        let out = render("| Module | Tests |\n|--------|-------|\n");
        assert!(
            !out.contains("ModuleTests"),
            "header cells must not be fused; out={:?}",
            out
        );
        assert!(out.contains('┼'), "header rule missing; out={:?}", out);
    }
}

#[cfg(test)]
mod stream_tests {
    use super::*;

    fn full_render(md: &str) -> String {
        let ui = UI::new();
        let mut buf = Vec::new();
        ui.render_markdown_to(&mut buf, md, false).unwrap();
        String::from_utf8(buf).unwrap()
    }

    /// Drive the streaming renderer with the given delta sequence and
    /// return the concatenation of every commit + finalize. The
    /// streaming output must match what a one-shot render of the
    /// concatenated deltas produces, otherwise the stream visibly drifts
    /// from the canonical view.
    fn stream(deltas: &[&str]) -> String {
        let mut renderer = MarkdownStreamRenderer::new();
        let mut out = String::new();
        for d in deltas {
            renderer.push_delta(d);
            out.push_str(&renderer.commit().unwrap());
        }
        out.push_str(&renderer.finalize().unwrap());
        out
    }

    #[test]
    fn no_commit_until_newline() {
        let mut r = MarkdownStreamRenderer::new();
        r.push_delta("Hello, world");
        assert!(
            r.commit().unwrap().is_empty(),
            "commit before newline must hold the line back"
        );
        r.push_delta("!\n");
        let out = r.commit().unwrap();
        assert!(out.contains("Hello, world!"), "got: {out:?}");
    }

    #[test]
    fn finalize_emits_partial_last_line() {
        let mut r = MarkdownStreamRenderer::new();
        r.push_delta("partial without trailing newline");
        assert!(r.commit().unwrap().is_empty());
        let out = r.finalize().unwrap();
        assert!(out.contains("partial"), "got: {out:?}");
    }

    #[test]
    fn streamed_heading_matches_full_render() {
        let md = "### Core loop\n";
        let streamed = stream(&["### Core ", "loop\n"]);
        let full = full_render(md);
        assert_eq!(streamed, full, "streamed heading must match full render");
    }

    #[test]
    fn streamed_numbered_list_matches_full_render() {
        // The exact case the user reported: numbered list with bold
        // inside an item must come out styled, not as raw markdown.
        let md = "1. **You type a prompt**. then more text.\n2. **Second**.\n";
        let streamed = stream(&[
            "1. **You type a prompt**.",
            " then more text.\n2. ",
            "**Second**.\n",
        ]);
        let full = full_render(md);
        assert_eq!(
            streamed, full,
            "streamed numbered list with bold must match full render"
        );
    }

    #[test]
    fn streamed_paragraphs_match_full_render() {
        let md = "First paragraph.\n\nSecond paragraph with **bold**.\n";
        let streamed = stream(&[
            "First paragraph.\n",
            "\nSecond paragraph",
            " with **bold**.\n",
        ]);
        let full = full_render(md);
        assert_eq!(streamed, full);
    }

    #[test]
    fn streamed_fenced_code_matches_full_render() {
        let md = "Here:\n\n```rust\nlet x = 1;\nlet y = 2;\n```\n";
        let streamed = stream(&["Here:\n\n```", "rust\nlet x = 1;\n", "let y = 2;\n```\n"]);
        let full = full_render(md);
        assert_eq!(
            streamed, full,
            "streamed fenced code must match full render"
        );
    }

    #[test]
    fn indented_backticks_are_not_treated_as_fence() {
        // CommonMark §4.5: a fenced code block opener allows at most
        // 3 leading spaces. A line with 4+ leading spaces followed by
        // ``` is part of an indented code block and must not toggle
        // fence state — otherwise the streaming renderer stalls
        // commits inside the surrounding indented block.
        assert!(is_fence_line("```\n"));
        assert!(is_fence_line(" ```\n"));
        assert!(is_fence_line("  ```\n"));
        assert!(is_fence_line("   ```\n"));
        assert!(!is_fence_line("    ```\n"));
        assert!(!is_fence_line("\t```\n"));
        assert!(!is_fence_line("text```\n"));
    }

    #[test]
    fn streamed_soft_break_paragraph_matches_full_render() {
        // Two lines belonging to one paragraph (soft break, no blank
        // line between). pulldown emits End-of-Paragraph blank only
        // after the LAST text in the paragraph, so the rendered line
        // count for the partial-paragraph prefix is shifted relative
        // to the full-paragraph render — the regression this test
        // pins is that the second line would get lost behind a
        // prematurely-committed End-of-Paragraph blank.
        let md = "line one\nline two\n";
        let streamed = stream(&["line one\n", "line two\n"]);
        let full = full_render(md);
        assert_eq!(
            streamed, full,
            "second line of a soft-break paragraph must not be lost"
        );
    }

    #[test]
    fn finalize_resets_state_for_reuse() {
        let mut r = MarkdownStreamRenderer::new();
        r.push_delta("first turn\n");
        let _ = r.commit().unwrap();
        let _ = r.finalize().unwrap();

        r.push_delta("second turn\n");
        let out = r.commit().unwrap();
        assert!(
            out.contains("second turn"),
            "renderer must be reusable after finalize; got {out:?}"
        );
    }

    #[test]
    fn streamed_table_matches_full_render() {
        // The reported bug: a table streamed in pieces dropped its body
        // rows because the unhandled table collapsed onto one line and the
        // already-committed header line was never redrawn. The renderer now
        // holds the table until it is complete, so streamed output equals a
        // one-shot render — every row survives.
        let md = "Summary:\n\n| Module | Tests |\n|--------|-------|\n| api | 120 |\n| repl | 95 |\n\nDone.\n";
        let streamed = stream(&[
            "Summary:\n\n| Module ",
            "| Tests |\n|--------|",
            "-------|\n| api | 120 |\n",
            "| repl | 95 |\n\nDone.\n",
        ]);
        let full = full_render(md);
        assert_eq!(streamed, full, "streamed table must match full render");
    }

    #[test]
    fn streamed_table_header_before_delimiter_matches_full_render() {
        // The header row (with its newline) arrives in one delta and the
        // delimiter in the next, so a commit fires between them. The header
        // must not be committed as a paragraph before the table is known.
        let md = "| Module | Tests |\n|--------|-------|\n| api | 120 |\n\nDone.\n";
        let streamed = stream(&[
            "| Module | Tests |\n",
            "|--------|-------|\n| api | 120 |\n\nDone.\n",
        ]);
        let full = full_render(md);
        assert_eq!(
            streamed, full,
            "header committed before its delimiter must not diverge"
        );
    }

    #[test]
    fn streamed_table_at_message_end_matches_full_render() {
        // A table with no trailing blank line is held until finalize; it
        // must still come out identical to a one-shot render.
        let md = "| A | B |\n|---|---|\n| 1 | 2 |\n";
        let streamed = stream(&["| A | B |\n|---|", "---|\n| 1 | 2 |\n"]);
        let full = full_render(md);
        assert_eq!(
            streamed, full,
            "table at message end must match full render"
        );
    }

    #[test]
    fn table_delimiter_detection() {
        assert!(is_table_delimiter("|---|---|\n"));
        assert!(is_table_delimiter("| --- | :---: |\n"));
        assert!(is_table_delimiter(":--|--:\n"));
        // No pipe: a thematic break or setext underline, not a delimiter.
        assert!(!is_table_delimiter("---\n"));
        assert!(!is_table_delimiter("------\n"));
        // Cells with non-dash content are not a delimiter row.
        assert!(!is_table_delimiter("| a | b |\n"));
    }

    #[test]
    fn delimiter_prefix_detection() {
        assert!(is_delimiter_prefix("|"));
        assert!(is_delimiter_prefix("|--"));
        assert!(is_delimiter_prefix(":--|--"));
        assert!(!is_delimiter_prefix("more"));
        assert!(!is_delimiter_prefix(""));
        assert!(!is_delimiter_prefix("   "));
    }

    #[test]
    fn streamed_table_with_split_delimiter_matches_full_render() {
        // The delimiter row arrives across deltas, so a partial delimiter
        // (just "|") briefly sits after the header. The header must not be
        // committed as a paragraph before the delimiter row completes.
        let md = "| a | b |\n|--|--|\n| 1 | 2 |\n\nz\n";
        let streamed = stream(&["| a", " | b |\n|", "--|--|\n| 1 ", "| 2 |\n\nz\n"]);
        let full = full_render(md);
        assert_eq!(
            streamed, full,
            "table with a split delimiter row must match full render"
        );
    }
}
