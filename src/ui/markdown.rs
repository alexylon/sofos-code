use crate::ui::UI;
use colored::Colorize;
use std::io::{self, Write, stdout};

/// SGR code for bold-on. Shared between the markdown Strong Start handler
/// and the ambient-style restorer so the two never drift apart.
const SGR_BOLD: &str = "\x1b[1m";
/// SGR code for italic-on. Shared between Emphasis Start and the restorer.
const SGR_ITALIC: &str = "\x1b[3m";
/// SGR code for the markdown heading style (bold + cyan). Shared between
/// Heading Start and the restorer; restoring just `\x1b[36m` would silently
/// drop the bold half.
const SGR_HEADING: &str = "\x1b[1;36m";
/// SGR code for the blockquote dim. Strong End (`\x1b[22m`) clears bold
/// *and* faint, and inline Code/Link close with `\x1b[0m` which clears
/// every attribute — so the restorer re-applies this when a tag closes
/// inside a blockquote.
const SGR_BLOCKQUOTE: &str = "\x1b[2m";

impl UI {
    pub fn print_markdown_highlighted(&self, md: &str) -> io::Result<()> {
        let mut out = stdout().lock();
        self.render_markdown_to(&mut out, md)?;
        out.flush()
    }

    pub(super) fn render_markdown_to(&self, out: &mut impl io::Write, md: &str) -> io::Result<()> {
        use pulldown_cmark::{CodeBlockKind, Event, Options, Parser, Tag, TagEnd};

        // Re-emit any ambient inline styles after a full SGR reset, so nested inline
        // tags (Code/Link) and Strong don't leave the outer heading/strong/emphasis bare.
        fn restore_ambient(
            out: &mut impl io::Write,
            bold: bool,
            italic: bool,
            in_heading: bool,
            in_blockquote: bool,
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
            if in_blockquote {
                write!(out, "{}", SGR_BLOCKQUOTE)?;
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

        for event in parser {
            match event {
                Event::Start(Tag::Heading { .. }) => {
                    in_heading = true;
                    write!(out, "{}", SGR_HEADING)?;
                }
                Event::End(TagEnd::Heading(_)) => {
                    in_heading = false;
                    writeln!(out, "\x1b[0m")?;
                }
                Event::Start(Tag::Strong) => {
                    bold = true;
                    write!(out, "{}", SGR_BOLD)?;
                }
                Event::End(TagEnd::Strong) => {
                    bold = false;
                    write!(out, "\x1b[22m")?;
                    restore_ambient(out, bold, italic, in_heading, in_blockquote)?;
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
                    writeln!(out, "{}", highlighted)?;
                }
                Event::Code(code) => {
                    write!(out, "\x1b[38;2;175;215;255m{}\x1b[0m", code)?;
                    restore_ambient(out, bold, italic, in_heading, in_blockquote)?;
                }
                Event::Text(text) => {
                    if in_code_block {
                        code_buf.push_str(&text);
                    } else {
                        write!(out, "{}", text)?;
                    }
                }
                Event::SoftBreak => {
                    if !in_code_block {
                        writeln!(out)?;
                    }
                }
                Event::HardBreak => {
                    writeln!(out)?;
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
                }
                Event::End(TagEnd::Item) => {
                    writeln!(out)?;
                }
                Event::Start(Tag::BlockQuote(_)) => {
                    in_blockquote = true;
                    write!(out, "{}> ", SGR_BLOCKQUOTE)?;
                }
                Event::End(TagEnd::BlockQuote(_)) => {
                    in_blockquote = false;
                    writeln!(out, "\x1b[0m")?;
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
                    restore_ambient(out, bold, italic, in_heading, in_blockquote)?;
                }
                Event::Rule => {
                    writeln!(out, "{}", "─".repeat(40).dimmed())?;
                }
                _ => {}
            }
        }

        Ok(())
    }
}

/// Return the byte offset of the last newline in `buf` that is **not**
/// inside an open fenced code block. Lines opening with ``` toggle the
/// fence state; the trailing newline of an open-fence line is therefore
/// not a safe commit point.
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
    let mut fence_open = false;
    let mut last_safe = 0usize;
    let mut pos = 0usize;
    for line in buf.split_inclusive('\n') {
        if is_fence_line(line) {
            fence_open = !fence_open;
        }
        pos += line.len();
        if line.ends_with('\n') && !fence_open {
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
}

impl MarkdownStreamRenderer {
    pub(super) fn new() -> Self {
        Self {
            buffer: String::new(),
            committed_lines: 0,
            last_safe_end: 0,
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
        UI::shared().render_markdown_to(&mut buf, source)?;
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
        ui.render_markdown_to(&mut buf, md).unwrap();
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
            after_strong_end[..rest_idx].contains(SGR_BLOCKQUOTE),
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
            after_code_reset[..rest_idx].contains(SGR_BLOCKQUOTE),
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
            after_link_close[..rest_idx].contains(SGR_BLOCKQUOTE),
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
}

#[cfg(test)]
mod stream_tests {
    use super::*;

    fn full_render(md: &str) -> String {
        let ui = UI::new();
        let mut buf = Vec::new();
        ui.render_markdown_to(&mut buf, md).unwrap();
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
}
