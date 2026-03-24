pub mod diff;
pub mod syntax;

use crate::session::history::Session;
use crate::session::DisplayMessage;
use crate::ui::syntax::SyntaxHighlighter;
use colored::Colorize;
use crossterm::cursor::SetCursorStyle;
use crossterm::event::{self, Event, KeyCode, KeyEvent};
use crossterm::execute;
use std::io::{self, stdout, Write};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::Duration;

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
        match self {
            Self::Blocked => "Blocked:".truecolor(0xFF, 0xA5, 0x00).bold(), // Orange
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

/// RAII guard that ensures raw mode is disabled when dropped.
/// Prevents terminal corruption if a panic occurs while in raw mode.
struct RawModeGuard;

impl RawModeGuard {
    fn new() -> Option<Self> {
        if crossterm::terminal::enable_raw_mode().is_ok() {
            Some(Self)
        } else {
            None
        }
    }
}

impl Drop for RawModeGuard {
    fn drop(&mut self) {
        let _ = crossterm::terminal::disable_raw_mode();
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

    pub fn print_welcome() {
        println!("{}", "Sofos - AI Coding Assistant".bright_cyan().bold());
        println!("{}", "Type your message or 'exit' to quit.".dimmed());
        println!("{}", "Type 'clear' to clear conversation history.".dimmed());
        println!("{}", "Type 'resume' to load a previous session.".dimmed());
        println!(
            "{}",
            "Type 'think on/off' to toggle extended thinking.".dimmed()
        );
        println!(
            "{}",
            "Press ESC while processing to interrupt and provide guidance.".dimmed()
        );
        println!();
    }

    pub fn print_goodbye() {
        println!("{}", "Goodbye!".bright_cyan());
    }

    pub fn display_session_summary(model: &str, total_input_tokens: u32, total_output_tokens: u32) {
        if total_input_tokens == 0 && total_output_tokens == 0 {
            return;
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
                    println!("{} {}", "λ>".bright_green().bold(), content);
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
        if !thinking.trim().is_empty() {
            println!(
                "\n{}",
                "Thinking:".truecolor(0x77, 0x00, 0xFF).bold().dimmed()
            );
            println!("{}\n", thinking.dimmed().italic());
        }
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

    pub fn run_animation_with_interrupt(
        action_message: String,
        interrupt_message: String,
        running: Arc<AtomicBool>,
        interrupted: Arc<AtomicBool>,
    ) {
        let running_anim = Arc::clone(&running);
        let running_key = Arc::clone(&running);
        let interrupted_clone = Arc::clone(&interrupted);

        // Animation thread
        let animation_handle = thread::spawn(move || {
            let frames = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];
            let mut frame_idx = 0;

            print!("\n\x1B[?25l");
            let _ = stdout().flush();

            while running_anim.load(Ordering::SeqCst) {
                print!(
                    "\r{} {} {}",
                    frames[frame_idx].truecolor(0xFF, 0x99, 0x33),
                    action_message.truecolor(0xFF, 0x99, 0x33),
                    interrupt_message.dimmed(),
                );
                let _ = stdout().flush();
                frame_idx = (frame_idx + 1) % frames.len();
                thread::sleep(Duration::from_millis(80));
            }

            print!("\r{}\r", " ".repeat(70));
            print!("\x1B[?25h");
            println!(); // Move to new line so next output doesn't conflict
            let _ = stdout().flush();
        });

        let key_handle = thread::spawn(move || {
            let _guard = match RawModeGuard::new() {
                Some(g) => g,
                None => return,
            };

            while running_key.load(Ordering::SeqCst) {
                if event::poll(Duration::from_millis(50)).unwrap_or(false) {
                    if let Ok(Event::Key(KeyEvent {
                        code: KeyCode::Esc, ..
                    })) = event::read()
                    {
                        interrupted_clone.store(true, Ordering::SeqCst);
                        running_key.store(false, Ordering::SeqCst);
                        break;
                    }
                }
            }
        });

        // Wait for both threads to complete properly
        // Don't use timeout - abandoning threads can corrupt terminal state
        let _ = animation_handle.join();
        let _ = key_handle.join();

        // Final cleanup - ensure terminal is in a known good state
        let _ = crossterm::terminal::disable_raw_mode();
        print!("\x1B[?25h");
        let _ = stdout().flush();
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
            _ => output.to_string(),
        }
    }

    fn calculate_cost(model: &str, input_tokens: u32, output_tokens: u32) -> f64 {
        // Prices per million tokens in USD
        let (input_price, output_price) = match model {
            "claude-sonnet-4-6" => (3.0, 15.0),
            "claude-opus-4-6" => (5.0, 25.0),
            "claude-haiku-4-5" => (1.0, 5.0),
            "gpt-5.1-codex-max" | "gpt-5.1-codex" | "gpt-5-codex" => (1.25, 10.0),
            "gpt-5.2" => (1.75, 14.0),
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
        if !self.thinking_started.swap(true, Ordering::SeqCst) {
            print!(
                "\n{}\n",
                "Thinking:".truecolor(0x77, 0x00, 0xFF).bold().dimmed()
            );
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

pub fn set_normal_mode_cursor_style() -> io::Result<()> {
    set_cursor_style(SetCursorStyle::DefaultUserShape)
}
