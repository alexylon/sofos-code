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

/// Fraction of the base input price charged for tokens served from the
/// provider prompt cache. Both Anthropic and OpenAI publish this at 10%
/// for their current model families.
const CACHE_READ_RATE: f64 = 0.10;
/// Multiplier applied to the base input price for tokens written to a
/// 5-minute Anthropic cache breakpoint. OpenAI has no separate creation
/// charge.
const CACHE_CREATION_RATE: f64 = 1.25;

/// True for OpenAI model identifiers (`gpt-*`). Used by the cost
/// and token-display paths to route into the OpenAI pricing /
/// uncached-tokens branches.
fn is_openai_model(model: &str) -> bool {
    model.starts_with("gpt")
}

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
            "Enter to send  ·  Shift+Enter for newline  ·  ESC/Ctrl+C to interrupt".dimmed()
        );
        println!(
            "  {}",
            "/exit  /clear  /resume  /compact  /think [off|low|medium|high]  /s  /n".dimmed()
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
        total_cache_read_tokens: u32,
        total_cache_creation_tokens: u32,
        peak_single_turn_input_tokens: u32,
    ) -> bool {
        if total_input_tokens == 0 && total_output_tokens == 0 {
            return false;
        }

        println!();
        println!("{}", "─".repeat(50).bright_cyan());
        println!("{}", "Session Summary".bright_cyan().bold());
        println!("{}", "─".repeat(50).bright_cyan());

        let estimated_cost = Self::calculate_cost(
            model,
            total_input_tokens,
            total_output_tokens,
            total_cache_read_tokens,
            total_cache_creation_tokens,
            peak_single_turn_input_tokens,
        );

        let total_input_seen =
            Self::total_input_seen_by_model(model, total_input_tokens, total_cache_read_tokens)
                + total_cache_creation_tokens;
        let cache_hit_pct = if total_input_seen > 0 {
            (total_cache_read_tokens as f64 / total_input_seen as f64) * 100.0
        } else {
            0.0
        };

        println!(
            "{:<20} {}",
            "Input tokens:".bright_white(),
            Self::format_number(total_input_seen).bright_green()
        );
        if total_cache_read_tokens > 0 || total_cache_creation_tokens > 0 {
            println!(
                "{:<20} {} {}",
                "  cache read:".bright_white(),
                Self::format_number(total_cache_read_tokens).bright_green(),
                format!("({:.0}% hit)", cache_hit_pct).dimmed()
            );
            if total_cache_creation_tokens > 0 {
                println!(
                    "{:<20} {}",
                    "  cache write:".bright_white(),
                    Self::format_number(total_cache_creation_tokens).bright_green()
                );
            }
        }
        println!(
            "{:<20} {}",
            "Output tokens:".bright_white(),
            Self::format_number(total_output_tokens).bright_green()
        );
        println!(
            "{:<20} {}",
            "Total tokens:".bright_white(),
            Self::format_number(total_input_seen + total_output_tokens).bright_green()
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

    /// Returns the count of input tokens the model actually saw (cached
    /// plus uncached, excluding cache-creation writes which are billed
    /// separately). Hides the per-provider semantic difference of
    /// `total_input_tokens` (OpenAI already includes cached, Anthropic
    /// excludes them).
    fn total_input_seen_by_model(
        model: &str,
        total_input_tokens: u32,
        cache_read_tokens: u32,
    ) -> u32 {
        if is_openai_model(model) {
            total_input_tokens
        } else {
            total_input_tokens + cache_read_tokens
        }
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

    fn calculate_cost(
        model: &str,
        input_tokens: u32,
        output_tokens: u32,
        cache_read_tokens: u32,
        cache_creation_tokens: u32,
        peak_single_turn_input_tokens: u32,
    ) -> f64 {
        let info = crate::api::model_info::lookup(model);
        // Tiered pricing: gpt-5.4/5.5 flip the entire session to a
        // premium rate once any single prompt's input crosses the
        // documented threshold. Compare the per-call high-water mark
        // (not the cumulative session total) against the threshold,
        // because the cliff is per-prompt, not per-session-cumulative.
        let (input_price, output_price) = match info.premium_tier {
            Some(tier) if peak_single_turn_input_tokens > tier.input_threshold => {
                (tier.price_input_per_m, tier.price_output_per_m)
            }
            _ => (info.price_input_per_m, info.price_output_per_m),
        };

        // OpenAI's `input_tokens` is the total (cached + uncached);
        // Anthropic's is uncached new tokens only. Normalize to "tokens
        // billed at the full input rate" before pricing.
        let uncached = if is_openai_model(model) {
            input_tokens.saturating_sub(cache_read_tokens)
        } else {
            input_tokens
        };

        let uncached_cost = (uncached as f64 / 1_000_000.0) * input_price;
        let cached_cost = (cache_read_tokens as f64 / 1_000_000.0) * input_price * CACHE_READ_RATE;
        let creation_cost =
            (cache_creation_tokens as f64 / 1_000_000.0) * input_price * CACHE_CREATION_RATE;
        let output_cost = (output_tokens as f64 / 1_000_000.0) * output_price;

        uncached_cost + cached_cost + creation_cost + output_cost
    }

    /// Render the elapsed turn time as a short human-readable string for
    /// the "your turn" prompt-ready signal at the end of a completed
    /// agent loop. Unit picks adapt to magnitude so quick turns stay
    /// concise and long agent runs stay legible.
    pub fn format_turn_finished(elapsed: std::time::Duration) -> String {
        let total_secs = elapsed.as_secs();
        if total_secs < 1 {
            "Finished in <1s".to_string()
        } else if total_secs < 60 {
            format!("Finished in {}s", total_secs)
        } else if total_secs < 3600 {
            format!("Finished in {}m {}s", total_secs / 60, total_secs % 60)
        } else {
            format!(
                "Finished in {}h {}m",
                total_secs / 3600,
                (total_secs % 3600) / 60
            )
        }
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

#[cfg(test)]
mod cost_tests {
    use super::*;

    fn approx(a: f64, b: f64) {
        assert!(
            (a - b).abs() < 1e-9,
            "expected ≈{}, got {} (delta {})",
            b,
            a,
            (a - b).abs()
        );
    }

    #[test]
    fn openai_cost_uses_full_rate_when_no_cache() {
        // 100k input @ $5/M, 5k output @ $30/M, no cache. Peak below
        // the 272K cliff so standard pricing applies.
        let cost = UI::calculate_cost("gpt-5.5", 100_000, 5_000, 0, 0, 100_000);
        approx(cost, 100_000.0 / 1e6 * 5.0 + 5_000.0 / 1e6 * 30.0);
    }

    #[test]
    fn openai_cost_discounts_cache_reads_at_10pct() {
        let cost = UI::calculate_cost("gpt-5.5", 100_000, 5_000, 75_000, 0, 100_000);
        approx(cost, 0.1625 + 0.15);
    }

    #[test]
    fn openai_cost_3x_lower_than_pre_fix_at_75pct_hit_input_only() {
        let pre_fix_input = 100_000.0 / 1e6 * 5.0;
        let post_fix_input = UI::calculate_cost("gpt-5.5", 100_000, 0, 75_000, 0, 100_000);
        let ratio = pre_fix_input / post_fix_input;
        assert!(
            (2.9..=3.2).contains(&ratio),
            "expected pre/post ratio ≈3x at 75% hit, got {:.2}x",
            ratio
        );
    }

    #[test]
    fn anthropic_cost_input_tokens_already_excludes_cache() {
        let cost = UI::calculate_cost("claude-opus-4-7", 25_000, 5_000, 75_000, 0, 100_000);
        approx(cost, 0.1625 + 0.125);
    }

    #[test]
    fn anthropic_cost_charges_creation_at_125pct() {
        let cost = UI::calculate_cost("claude-opus-4-7", 0, 0, 0, 50_000, 0);
        approx(cost, 50_000.0 / 1e6 * 5.0 * 1.25);
    }

    #[test]
    fn cache_hit_does_not_underflow_when_read_exceeds_input() {
        let cost = UI::calculate_cost("gpt-5.5", 50_000, 0, 100_000, 0, 100_000);
        approx(cost, 100_000.0 / 1e6 * 5.0 * 0.10);
    }

    #[test]
    fn cliff_crossing_doubles_input_rate_for_gpt_5_5() {
        // Below cliff: standard rate ($5/M input). 100K input × $5/M = $0.50.
        let standard = UI::calculate_cost("gpt-5.5", 100_000, 0, 0, 0, 200_000);
        approx(standard, 100_000.0 / 1e6 * 5.0);

        // Above cliff (peak observed > 272K): premium rate ($10/M input
        // for gpt-5.5). 100K × $10/M = $1.00. Same input/cache numbers,
        // double the bill — that's the user-visible effect of the cliff.
        let premium = UI::calculate_cost("gpt-5.5", 100_000, 0, 0, 0, 300_000);
        approx(premium, 100_000.0 / 1e6 * 10.0);
        assert!((premium / standard - 2.0).abs() < 0.01);
    }

    #[test]
    fn turn_finished_format_picks_unit_by_magnitude() {
        use std::time::Duration;
        assert_eq!(
            UI::format_turn_finished(Duration::from_millis(400)),
            "Finished in <1s"
        );
        assert_eq!(
            UI::format_turn_finished(Duration::from_secs(7)),
            "Finished in 7s"
        );
        assert_eq!(
            UI::format_turn_finished(Duration::from_secs(94)),
            "Finished in 1m 34s"
        );
        assert_eq!(
            UI::format_turn_finished(Duration::from_secs(60)),
            "Finished in 1m 0s"
        );
        assert_eq!(
            UI::format_turn_finished(Duration::from_secs(3725)),
            "Finished in 1h 2m"
        );
    }

    #[test]
    fn unknown_model_falls_back_without_panic() {
        // Default fallback uses Sonnet 4.5 pricing ($3 / $15) and the
        // Anthropic semantics branch (input_tokens is uncached).
        let cost = UI::calculate_cost("some-future-model", 1_000, 1_000, 0, 0, 1_000);
        approx(cost, 1_000.0 / 1e6 * 3.0 + 1_000.0 / 1e6 * 15.0);
    }
}
