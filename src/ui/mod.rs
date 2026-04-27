pub mod diff;
pub mod syntax;

use crate::session::DisplayMessage;
use crate::session::history::Session;
use crate::ui::syntax::SyntaxHighlighter;
use colored::Colorize;
use crossterm::cursor::SetCursorStyle;
use crossterm::execute;
use std::io::{self, Write, stdout};
use std::sync::atomic::{AtomicBool, Ordering};

/// Accent colour used for the startup banner and other attention-grabbing
/// highlights in the legacy (non-TUI) `colored` output path. Matches the
/// ratatui `ACCENT` in `repl::tui::ui` so both code paths render sofos'
/// orange identically.
const ACCENT_RGB: (u8, u8, u8) = (0xFF, 0x99, 0x33);
/// Purple used for thinking / reasoning labels — visually distinct from
/// the orange accent so reasoning blocks stand out from regular output.
const THINKING_RGB: (u8, u8, u8) = (0x77, 0x00, 0xFF);
/// Orange used for the "Blocked:" prefix on permission-denied messages.
/// Same hue as the accent but called out separately because its semantic
/// meaning is "security restriction", not "highlight".
const BLOCKED_RGB: (u8, u8, u8) = (0xFF, 0xA5, 0x00);

/// Message severity levels for consistent UI feedback
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum MessageSeverity {
    /// Security restrictions, permission denials - expected behavior
    Blocked,
    /// Recoverable issues, non-critical problems
    Warning,
    /// Actual failures (network, IO, parsing errors)
    Error,
    /// Informational messages
    #[allow(dead_code)]
    Info,
    /// Success messages
    #[allow(dead_code)]
    Success,
}

impl MessageSeverity {
    pub fn prefix(&self) -> colored::ColoredString {
        let (br, bg, bb) = BLOCKED_RGB;
        match self {
            Self::Blocked => "Blocked:".truecolor(br, bg, bb).bold(),
            Self::Warning => "Warning:".bright_yellow().bold(),
            Self::Error => "Error:".bright_red().bold(),
            Self::Info => "Info:".bright_cyan().bold(),
            Self::Success => "Success:".bright_green().bold(),
        }
    }

    #[allow(dead_code)]
    pub fn icon(&self) -> &'static str {
        match self {
            Self::Blocked => "🔒",
            Self::Warning => "⚠️ ",
            Self::Error => "✗",
            Self::Info => "ℹ️ ",
            Self::Success => "✓",
        }
    }
}

/// Structured message for consistent formatting (reserved for future use)
#[allow(dead_code)]
pub struct FormattedMessage {
    pub severity: MessageSeverity,
    pub title: String,
    pub details: Option<String>,
    pub hint: Option<String>,
}

#[allow(dead_code)]
impl FormattedMessage {
    pub fn blocked(title: impl Into<String>) -> Self {
        Self {
            severity: MessageSeverity::Blocked,
            title: title.into(),
            details: None,
            hint: None,
        }
    }

    pub fn warning(title: impl Into<String>) -> Self {
        Self {
            severity: MessageSeverity::Warning,
            title: title.into(),
            details: None,
            hint: None,
        }
    }

    pub fn error(title: impl Into<String>) -> Self {
        Self {
            severity: MessageSeverity::Error,
            title: title.into(),
            details: None,
            hint: None,
        }
    }

    pub fn with_details(mut self, details: impl Into<String>) -> Self {
        self.details = Some(details.into());
        self
    }

    pub fn with_hint(mut self, hint: impl Into<String>) -> Self {
        self.hint = Some(hint.into());
        self
    }

    pub fn print(&self) {
        eprintln!("{} {}", self.severity.prefix(), self.title);
        if let Some(ref details) = self.details {
            eprintln!("  {}", details.dimmed());
        }
        if let Some(ref hint) = self.hint {
            eprintln!("  {} {}", "Hint:".bright_cyan(), hint);
        }
    }
}

/// UI utilities for displaying messages, animations, and formatting
#[allow(dead_code)]
pub struct UI {
    highlighter: SyntaxHighlighter,
}

impl UI {
    pub fn new() -> Self {
        Self {
            highlighter: SyntaxHighlighter::new(),
        }
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

    #[allow(dead_code)]
    pub fn print_info(message: &str) {
        Self::print_message(MessageSeverity::Info, message);
    }

    /// Return the ASCII-art banner as a ready-to-print string. The
    /// interactive path collects this into `Repl::startup_banner` so
    /// the TUI can emit it through `OutputCapture`, which in turn
    /// places it above the inline viewport via the history-scroll
    /// path — the only way to avoid the viewport overpainting the
    /// banner on terminals that drop the cursor-position DSR
    /// (notably Ghostty).
    pub fn banner_text() -> String {
        // "SOFOS" rendered at 3 rows — half the height of the previous
        // 6-row ANSI Shadow figlet. Each letter is 3 columns wide with no
        // separator between letters so the word reads as a single unit.
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
            "Enter to send  ·  Shift+Enter for newline  ·  ESC/Ctrl+C to interrupt".dimmed()
        );
        println!(
            "  {}",
            "/exit  /clear  /resume  /compact  /think [on|off]  /s  /n".dimmed()
        );
        println!();
    }

    pub fn print_goodbye() {
        println!("{}", "Goodbye!".bright_cyan());
    }

    /// Print the post-turn usage summary. Returns `true` when something
    /// was printed, `false` when the early-return path skipped it — the
    /// TUI teardown uses that return to decide whether to emit its own
    /// escape-newline before [`Self::print_goodbye`] so "Goodbye!"
    /// never collides with the status row.
    pub fn display_session_summary(
        model: &str,
        total_input_tokens: u32,
        total_output_tokens: u32,
    ) -> bool {
        if total_input_tokens == 0 && total_output_tokens == 0 {
            return false;
        }

        println!();
        println!("{}", "─".repeat(50).bright_cyan());
        println!("{}", "Session Summary".bright_cyan().bold());
        println!("{}", "─".repeat(50).bright_cyan());

        let estimated_cost = Self::calculate_cost(model, total_input_tokens, total_output_tokens);

        println!(
            "{:<20} {}",
            "Input tokens:".bright_white(),
            Self::format_number(total_input_tokens).bright_green()
        );
        println!(
            "{:<20} {}",
            "Output tokens:".bright_white(),
            Self::format_number(total_output_tokens).bright_green()
        );
        println!(
            "{:<20} {}",
            "Total tokens:".bright_white(),
            Self::format_number(total_input_tokens + total_output_tokens).bright_green()
        );
        println!();
        println!(
            "{:<20} {}",
            "Estimated cost:".bright_white().bold(),
            format!("${:.4}", estimated_cost).bright_yellow().bold()
        );

        println!("{}", "─".repeat(50).bright_cyan());
        println!();
        true
    }

    pub fn display_session(&self, session: &Session) -> io::Result<()> {
        if session.display_messages.is_empty() {
            println!(
                "{}",
                "Note: No display history available for this session.".dimmed()
            );
            println!();
            return Ok(());
        }

        println!("{}", "═".repeat(80).bright_cyan());
        println!("{}", "Previous Conversation:".bright_cyan().bold());
        println!("{}", "═".repeat(80).bright_cyan());
        println!();

        for display_msg in &session.display_messages {
            match display_msg {
                DisplayMessage::UserMessage { content } => {
                    println!("{} {}", ">".bright_green().bold(), content);
                    println!();
                }
                DisplayMessage::AssistantMessage { content } => {
                    println!("{}", "Assistant:".bright_blue().bold());
                    self.print_assistant_text(content)?;
                }
                DisplayMessage::ToolExecution {
                    tool_name,
                    tool_input: _,
                    tool_output,
                } => {
                    if tool_name == "execute_bash" {
                        if let Ok(input_val) = serde_json::from_value::<serde_json::Value>(
                            serde_json::to_value(tool_output).unwrap_or_default(),
                        ) {
                            if let Some(command) = input_val.get("command").and_then(|v| v.as_str())
                            {
                                self.print_tool_header(tool_name, Some(command));
                            }
                        }
                    } else {
                        self.print_tool_header(tool_name, None);
                    }
                    self.print_tool_output(tool_output);
                }
            }
        }

        println!("{}", "═".repeat(80).bright_cyan());
        println!();
        Ok(())
    }

    pub fn print_assistant_text(&self, text: &str) -> io::Result<()> {
        self.print_markdown_highlighted(text)?;
        Ok(())
    }

    pub fn print_thinking(&self, thinking: &str) {
        if thinking.trim().is_empty() {
            return;
        }
        let (tr, tg, tb) = THINKING_RGB;
        println!("\n{}", "Thinking:".truecolor(tr, tg, tb).bold().dimmed());
        // Style each line individually so every captured line carries its
        // own SGR wrapper. The TUI pipe reader splits on '\n' and parses
        // each line in isolation, which would drop the style from every
        // line after the first if we wrapped the whole block at once.
        for line in thinking.lines() {
            println!("{}", line.dimmed().italic());
        }
        println!();
    }

    pub fn print_tool_header(&self, tool_name: &str, command: Option<&str>) {
        if tool_name == "execute_bash" {
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

    pub fn print_markdown_highlighted(&self, md: &str) -> io::Result<()> {
        use pulldown_cmark::{CodeBlockKind, Event, Options, Parser, Tag, TagEnd};

        let parser = Parser::new_ext(md, Options::all());
        let mut out = stdout().lock();

        let mut in_code_block = false;
        let mut code_lang = String::new();
        let mut code_buf = String::new();
        let mut bold = false;
        let mut italic = false;
        let mut in_heading = false;

        for event in parser {
            match event {
                Event::Start(Tag::Heading { .. }) => {
                    in_heading = true;
                    write!(out, "\x1b[1;36m")?;
                }
                Event::End(TagEnd::Heading(_)) => {
                    in_heading = false;
                    writeln!(out, "\x1b[0m")?;
                }
                Event::Start(Tag::Strong) => {
                    bold = true;
                    write!(out, "\x1b[1m")?;
                }
                Event::End(TagEnd::Strong) => {
                    bold = false;
                    write!(out, "\x1b[22m")?;
                    if italic {
                        write!(out, "\x1b[3m")?;
                    }
                }
                Event::Start(Tag::Emphasis) => {
                    italic = true;
                    write!(out, "\x1b[3m")?;
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
                    if bold {
                        write!(out, "\x1b[1m")?;
                    }
                    if italic {
                        write!(out, "\x1b[3m")?;
                    }
                    if in_heading {
                        write!(out, "\x1b[36m")?;
                    }
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
                    write!(out, "\x1b[2m> ")?;
                }
                Event::End(TagEnd::BlockQuote(_)) => {
                    writeln!(out, "\x1b[0m")?;
                }
                Event::Start(Tag::Link { dest_url, .. }) => {
                    write!(out, "\x1b[4;34m")?;
                    let _ = dest_url;
                }
                Event::End(TagEnd::Link) => {
                    write!(out, "\x1b[0m")?;
                }
                Event::Rule => {
                    writeln!(out, "{}", "─".repeat(40).dimmed())?;
                }
                _ => {}
            }
        }

        out.flush()
    }

    pub fn create_tool_display_message(
        tool_name: &str,
        tool_input: &serde_json::Value,
        output: &str,
    ) -> String {
        match tool_name {
            "read_file" => {
                let file_path = tool_input
                    .get("path")
                    .and_then(|v| v.as_str())
                    .unwrap_or("");

                let offset = tool_input
                    .get("offset")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(1);

                let line_count = output.lines().count() as u64;

                if line_count == 0 {
                    if file_path.is_empty() {
                        "Read file (empty or not found)".to_string()
                    } else {
                        format!(
                            "Read file from {} - empty or not found",
                            file_path.bright_cyan()
                        )
                    }
                } else {
                    let end_line = offset + line_count - 1;
                    if file_path.is_empty() {
                        format!("Read lines {}-{}", offset, end_line)
                    } else {
                        format!(
                            "Read lines {}-{} from {}",
                            offset,
                            end_line,
                            file_path.bright_cyan()
                        )
                    }
                }
            }
            "list_directory" => {
                let path = tool_input
                    .get("path")
                    .and_then(|v| v.as_str())
                    .unwrap_or(".");

                let item_count = output
                    .lines()
                    .filter(|line| !line.trim().is_empty() && !line.starts_with("Contents of"))
                    .count();

                if item_count == 0 {
                    format!("Found 0 items in {}", path.bright_cyan())
                } else if item_count == 1 {
                    format!("Found 1 item in {}", path.bright_cyan())
                } else {
                    format!("Found {} items in {}", item_count, path.bright_cyan())
                }
            }
            "web_fetch" => {
                let url = tool_input.get("url").and_then(|v| v.as_str()).unwrap_or("");
                let char_count = output.len();
                format!("Fetched {} ({} chars)", url.bright_cyan(), char_count)
            }
            "morph_edit_file" => output.to_string(),
            "search_code" => {
                let pattern = tool_input
                    .get("pattern")
                    .and_then(|v| v.as_str())
                    .unwrap_or("");

                let body = output
                    .strip_prefix(crate::tools::codesearch::SEARCH_RESULTS_PREFIX)
                    .unwrap_or(output);

                // ripgrep --heading output groups matches under file headings
                // separated by blank lines. Lines starting with `<digits>:` are
                // matches; non-empty lines without that prefix are file
                // headings.
                let mut files = 0usize;
                let mut matches = 0usize;
                for line in body.lines() {
                    if line.is_empty() {
                        continue;
                    }
                    if line.starts_with("No matches found") {
                        continue;
                    }
                    let is_match_line = line.split_once(':').is_some_and(|(prefix, _)| {
                        !prefix.is_empty() && prefix.chars().all(|c| c.is_ascii_digit())
                    });
                    if is_match_line {
                        matches += 1;
                    } else {
                        files += 1;
                    }
                }

                if matches == 0 {
                    format!("No matches for {}", pattern.bright_cyan())
                } else {
                    format!(
                        "Found {} match{} in {} file{} for {}",
                        matches,
                        if matches == 1 { "" } else { "es" },
                        files,
                        if files == 1 { "" } else { "s" },
                        pattern.bright_cyan()
                    )
                }
            }
            _ => output.to_string(),
        }
    }

    fn calculate_cost(model: &str, input_tokens: u32, output_tokens: u32) -> f64 {
        // Prices per million tokens in USD
        let (input_price, output_price) = match model {
            "claude-sonnet-4-6" => (3.0, 15.0),
            "claude-opus-4-6" | "claude-opus-4-7" => (5.0, 25.0),
            "claude-haiku-4-5" => (1.0, 5.0),
            "gpt-5.3-codex" => (1.75, 14.0),
            "gpt-5.4" => (2.5, 15.0),
            "gpt-5.5" => (5.0, 30.0),
            // Default fallback (use Sonnet 4.5 pricing)
            _ => (3.0, 15.0),
        };

        let input_cost = (input_tokens as f64 / 1_000_000.0) * input_price;
        let output_cost = (output_tokens as f64 / 1_000_000.0) * output_price;

        input_cost + output_cost
    }

    fn format_number(n: u32) -> String {
        let s = n.to_string();
        let mut result = String::new();
        for (i, c) in s.chars().rev().enumerate() {
            if i > 0 && i % 3 == 0 {
                result.push(',');
            }
            result.push(c);
        }
        result.chars().rev().collect()
    }
}

/// Handles real-time output during response streaming
pub struct StreamPrinter {
    thinking_started: AtomicBool,
    text_started: AtomicBool,
}

impl StreamPrinter {
    pub fn new() -> Self {
        Self {
            thinking_started: AtomicBool::new(false),
            text_started: AtomicBool::new(false),
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
        print!("{}", delta.dimmed());
        let _ = stdout().flush();
    }

    pub fn on_text_delta(&self, delta: &str) {
        if !self.text_started.swap(true, Ordering::SeqCst) {
            if self.thinking_started.load(Ordering::SeqCst) {
                println!();
            }
            println!("{}", "Assistant:".bright_blue().bold());
        }
        print!("{}", delta);
        let _ = stdout().flush();
    }

    pub fn finish(&self) {
        if self.text_started.load(Ordering::SeqCst) || self.thinking_started.load(Ordering::SeqCst)
        {
            println!();
        }
    }
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

#[allow(dead_code)]
pub fn set_normal_mode_cursor_style() -> io::Result<()> {
    set_cursor_style(SetCursorStyle::DefaultUserShape)
}

#[cfg(test)]
mod tool_display_tests {
    use super::*;
    use serde_json::json;

    fn strip_ansi(s: &str) -> String {
        let mut out = String::with_capacity(s.len());
        let mut chars = s.chars();
        while let Some(c) = chars.next() {
            if c == '\x1b' {
                for cc in chars.by_ref() {
                    if cc.is_ascii_alphabetic() {
                        break;
                    }
                }
            } else {
                out.push(c);
            }
        }
        out
    }

    #[test]
    fn search_code_summarizes_matches_and_files() {
        let output = "Code search results:\n\nsrc/foo.rs\n12:    let x = 1;\n34:    let y = 2;\n\nsrc/bar.rs\n7:    let z = 3;\n";
        let msg =
            UI::create_tool_display_message("search_code", &json!({"pattern": "let"}), output);
        assert_eq!(strip_ansi(&msg), "Found 3 matches in 2 files for let");
    }

    #[test]
    fn search_code_handles_single_match_singular() {
        let output = "Code search results:\n\nsrc/foo.rs\n12:    let x = 1;\n";
        let msg =
            UI::create_tool_display_message("search_code", &json!({"pattern": "let"}), output);
        assert_eq!(strip_ansi(&msg), "Found 1 match in 1 file for let");
    }

    #[test]
    fn search_code_handles_no_matches() {
        let output = "Code search results:\n\nNo matches found for pattern: 'foo'";
        let msg =
            UI::create_tool_display_message("search_code", &json!({"pattern": "foo"}), output);
        assert_eq!(strip_ansi(&msg), "No matches for foo");
    }
}
