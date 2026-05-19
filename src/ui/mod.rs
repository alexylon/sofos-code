pub mod cost;
pub mod diff;
pub mod markdown;
pub mod session_display;
pub mod syntax;

use crate::ui::markdown::MarkdownStreamRenderer;
use crate::ui::syntax::SyntaxHighlighter;
use colored::Colorize;
use crossterm::cursor::SetCursorStyle;
use crossterm::execute;
use std::io::{self, Write, stdout};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, OnceLock};

/// Accent colour used for the startup banner and other attention-grabbing
/// highlights in the legacy (non-TUI) `colored` output path. Matches the
/// ratatui `ACCENT` in `repl::tui::ui` so both code paths render sofos'
/// orange identically.
pub(crate) const ACCENT_RGB: (u8, u8, u8) = (0xFF, 0x99, 0x33);
/// Purple used for thinking / reasoning labels — visually distinct from
/// the orange accent so reasoning blocks stand out from regular output.
const THINKING_RGB: (u8, u8, u8) = (0x77, 0x00, 0xFF);
/// Orange used for the "Blocked:" prefix on permission-denied messages.
/// Same hue as the accent but called out separately because its semantic
/// meaning is "security restriction", not "highlight".
const BLOCKED_RGB: (u8, u8, u8) = (0xFF, 0xA5, 0x00);

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum MessageSeverity {
    /// Operation rejected as expected behaviour. Covers both
    /// system-enforced policy (path traversal, output redirection,
    /// outside-workspace access, structural validation) and
    /// interactive user denial of a permission prompt. The display
    /// prefix is `Blocked:` because the same UI path is reached
    /// whether the system or the user refused the operation.
    Blocked,
    /// Recoverable issues, non-critical problems.
    Warning,
    /// Actual failures (network, IO, parsing errors).
    Error,
}

impl MessageSeverity {
    pub fn prefix(&self) -> colored::ColoredString {
        let (br, bg, bb) = BLOCKED_RGB;
        match self {
            Self::Blocked => "Blocked:".truecolor(br, bg, bb).bold(),
            Self::Warning => "Warning:".bright_yellow().bold(),
            Self::Error => "Error:".bright_red().bold(),
        }
    }
}

/// UI utilities for displaying messages, animations, and formatting
pub struct UI {
    highlighter: SyntaxHighlighter,
}

impl UI {
    pub fn new() -> Self {
        Self {
            highlighter: SyntaxHighlighter::new(),
        }
    }

    /// Process-wide shared `UI`. The underlying `SyntaxSet` and
    /// `ThemeSet` are read-only after construction and the rendering
    /// methods all take `&self`, so callers that only need to render
    /// can borrow this instead of paying for a fresh syntect load each
    /// time. Lazy: nothing is loaded until the first call.
    pub fn shared() -> &'static UI {
        static SHARED: OnceLock<UI> = OnceLock::new();
        SHARED.get_or_init(UI::new)
    }

    pub fn print_message(severity: MessageSeverity, message: &str) {
        eprintln!("{} {}", severity.prefix(), message);
    }

    pub fn print_blocked(message: &str) {
        Self::print_message(MessageSeverity::Blocked, message);
    }

    /// Print a blocked message with proper formatting for multi-line content.
    /// First line gets the "Blocked:" prefix, subsequent lines are indented.
    pub fn print_blocked_multiline(message: &str) {
        let mut lines = message.lines();
        if let Some(first_line) = lines.next() {
            eprintln!("{} {}", MessageSeverity::Blocked.prefix(), first_line);
            for line in lines {
                if line.trim().starts_with("Hint:") {
                    let hint_content = line.trim().strip_prefix("Hint:").unwrap_or("").trim();
                    eprintln!("  {} {}", "Hint:".bright_cyan(), hint_content);
                } else {
                    eprintln!("  {}", line.dimmed());
                }
            }
        } else {
            Self::print_blocked(message);
        }
    }

    pub fn print_warning(message: &str) {
        Self::print_message(MessageSeverity::Warning, message);
    }

    pub fn print_error(message: &str) {
        Self::print_message(MessageSeverity::Error, message);
    }

    pub fn print_error_with_hint(error: &crate::error::SofosError) {
        eprintln!("{} {}", MessageSeverity::Error.prefix(), error);
        if let Some(hint) = error.hint() {
            eprintln!("  {} {}", "Hint:".bright_cyan(), hint);
        }
    }

    pub fn print_blocked_with_hint(error: &crate::error::SofosError) {
        let msg = error.to_string();
        if msg.contains('\n') && msg.contains("Hint:") {
            Self::print_blocked_multiline(&msg);
        } else {
            eprintln!("{} {}", MessageSeverity::Blocked.prefix(), error);
            if let Some(hint) = error.hint() {
                eprintln!("  {} {}", "Hint:".bright_cyan(), hint);
            }
        }
    }

    /// Return the ASCII-art banner as a ready-to-print string. The
    /// interactive path collects this into `Repl::startup_banner` so
    /// the TUI can emit it through `OutputCapture`, which in turn
    /// places it above the inline viewport via the history-scroll
    /// path — the only way to avoid the viewport overpainting the
    /// banner on terminals that drop the cursor-position DSR
    /// (notably Ghostty).
    pub fn banner_text() -> String {
        // "SOFOS" rendered at 3 rows × 3 columns per letter, no
        // inter-letter separator (so the word reads as a single unit).
        // Half the height of the previous 6-row ANSI Shadow figlet.
        const BANNER: [&str; 3] = [
            r" ╭─╮╭─╮╭─╮╭─╮╭─╮",
            r" ╰─╮│ │├─ │ │╰─╮",
            r" ╰─╯╰─╯╵  ╰─╯╰─╯",
        ];
        let (r, g, b) = ACCENT_RGB;
        let mut out = String::new();
        out.push('\n');
        for line in BANNER {
            out.push_str(&format!("{}\n", line.truecolor(r, g, b).bold()));
        }
        out.push_str(&format!(" {}\n", "AI Coding Assistant".truecolor(r, g, b)));
        out.push('\n');
        out
    }

    pub fn print_welcome() {
        println!(
            "  {}",
            "Enter to send · Shift+Enter for newline · / to open command menu · Esc/Ctrl+C to interrupt".dimmed()
        );
        println!();
    }

    pub fn print_goodbye() {
        println!("{}", "Goodbye!".bright_cyan());
    }

    pub fn print_assistant_text(&self, text: &str) -> io::Result<()> {
        self.print_markdown_highlighted(text)?;
        Ok(())
    }

    pub fn print_tool_header(&self, tool_name: &str, command: Option<&str>) {
        if tool_name == crate::tools::ToolName::UpdatePlan.as_str() {
            return;
        }
        if tool_name == crate::tools::ToolName::ExecuteBash.as_str() {
            if let Some(cmd) = command {
                print!(
                    "{} {}",
                    "Executing:".bright_green().bold(),
                    cmd.bright_cyan()
                );
                let _ = stdout().flush();
            }
        } else {
            println!(
                "{} {}",
                "Using tool:".bright_yellow().bold(),
                tool_name.bright_yellow()
            );
        }
    }

    pub fn print_tool_output(&self, tool_output: &str) {
        if tool_output.contains('\x1b') {
            println!("{}\n", tool_output);
        } else {
            println!("{}\n", tool_output.dimmed());
        }
    }
}

/// Handles real-time output during response streaming. Visible
/// assistant text is fed through a [`MarkdownStreamRenderer`] so
/// headings, lists, emphasis, and code fences render with ANSI styling
/// instead of leaking raw markdown to the terminal. Thinking deltas go
/// through a separate renderer of the same type, with the rendered
/// output wrapped in a faint SGR pair so the body keeps the dim
/// "thinking" look without losing markdown formatting.
pub struct StreamPrinter {
    thinking_started: AtomicBool,
    text_started: AtomicBool,
    text_renderer: Mutex<MarkdownStreamRenderer>,
    thinking_renderer: Mutex<MarkdownStreamRenderer>,
}

impl StreamPrinter {
    pub fn new() -> Self {
        Self {
            thinking_started: AtomicBool::new(false),
            text_started: AtomicBool::new(false),
            text_renderer: Mutex::new(MarkdownStreamRenderer::new()),
            thinking_renderer: Mutex::new(MarkdownStreamRenderer::new()),
        }
    }

    pub fn on_thinking_delta(&self, delta: &str) {
        // Skip empty deltas (Opus 4.7 with `display: omitted` can emit
        // a thinking block that never carries any body). Claiming we've
        // started printing thinking would leave a bare "Thinking:"
        // label with no content below it.
        if delta.is_empty() {
            return;
        }
        if !self.thinking_started.swap(true, Ordering::SeqCst) {
            let (tr, tg, tb) = THINKING_RGB;
            print!("\n{}\n", "Thinking:".truecolor(tr, tg, tb).bold().dimmed());
        }
        let to_print = {
            let mut renderer = self.lock_thinking_renderer();
            renderer.push_delta(delta);
            renderer.commit().unwrap_or_default()
        };
        if !to_print.is_empty() {
            print_dim(&to_print);
            let _ = stdout().flush();
        }
    }

    pub fn on_text_delta(&self, delta: &str) {
        if !self.text_started.swap(true, Ordering::SeqCst) {
            if self.thinking_started.load(Ordering::SeqCst) {
                self.flush_thinking_tail();
                // Blank line between the thinking block and the
                // assistant text header. `finalize` guarantees a
                // trailing newline, so one extra `println!()` puts a
                // visible blank line between the two sections.
                println!();
            }
            println!("{}", "Assistant:".bright_blue().bold());
        }
        let to_print = {
            let mut renderer = self.lock_text_renderer();
            renderer.push_delta(delta);
            renderer.commit().unwrap_or_default()
        };
        if !to_print.is_empty() {
            print!("{}", to_print);
            let _ = stdout().flush();
        }
    }

    pub fn finish(&self) {
        if self.text_started.load(Ordering::SeqCst) {
            let to_print = self.lock_text_renderer().finalize().unwrap_or_default();
            if !to_print.is_empty() {
                print!("{}", to_print);
            }
            // The finalised buffer ends with a newline, so the cursor
            // is already at column 0 — no extra println! needed for
            // text.
            let _ = stdout().flush();
        } else if self.thinking_started.load(Ordering::SeqCst) {
            self.flush_thinking_tail();
            // `finalize` ends with a newline. One extra blank line
            // separates the thinking body from whatever the turn
            // renders next (a tool call header, the input prompt).
            println!();
            let _ = stdout().flush();
        }
    }

    /// Drain the thinking renderer's residual buffer (a partial last
    /// line, a still-open code fence, anything held back by `commit`)
    /// and emit it under the same dim wrap the streaming path uses.
    /// Shared between the thinking-to-text transition in `on_text_delta`
    /// and the thinking-only path in `finish`.
    fn flush_thinking_tail(&self) {
        let tail = self.lock_thinking_renderer().finalize().unwrap_or_default();
        if !tail.is_empty() {
            print_dim(&tail);
        }
    }

    /// Acquire the text renderer lock, recovering from poison so a
    /// panic in one delta callback doesn't kill subsequent streaming
    /// output. A partial markdown buffer is recoverable; the worst
    /// case is one mid-stream paragraph rendering as plain text.
    fn lock_text_renderer(&self) -> std::sync::MutexGuard<'_, MarkdownStreamRenderer> {
        self.text_renderer.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Same poison-recovery contract as [`Self::lock_text_renderer`],
    /// for the parallel thinking-side renderer.
    fn lock_thinking_renderer(&self) -> std::sync::MutexGuard<'_, MarkdownStreamRenderer> {
        self.thinking_renderer
            .lock()
            .unwrap_or_else(|e| e.into_inner())
    }
}

/// Emit `text` wrapped in the faint SGR pair so the thinking body
/// keeps its dim look. The renderer may have embedded its own ANSI
/// for markdown emphasis or fenced-code highlighting; the wrap lets
/// the prose dim cleanly and leaves those inner sequences intact
/// where they apply.
fn print_dim(text: &str) {
    print!("\x1b[2m{text}\x1b[22m");
}

fn set_cursor_style(style: SetCursorStyle) -> io::Result<()> {
    let mut out = stdout();
    execute!(out, style)?;
    out.flush()?;
    Ok(())
}

pub fn set_safe_mode_cursor_style() -> io::Result<()> {
    set_cursor_style(SetCursorStyle::BlinkingUnderScore)
}

/// Reset the terminal cursor to the default blinking block used in
/// normal (non-safe) mode. Mirror of [`set_safe_mode_cursor_style`]
/// so the `/normal` toggle can put the cursor shape back.
pub fn set_normal_mode_cursor_style() -> io::Result<()> {
    set_cursor_style(SetCursorStyle::DefaultUserShape)
}
