use colored::Colorize;
use crossterm::event::{self, Event, KeyCode, KeyEvent};
use std::io::{self, Write};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use crate::history::DisplayMessage;
use crate::syntax::SyntaxHighlighter;

const THREAD_JOIN_TIMEOUT: Duration = Duration::from_secs(1);

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

    pub fn display_session(&self, session: &crate::history::Session) {
        if session.display_messages.is_empty() {
            println!(
                "{}",
                "Note: No display history available for this session.".dimmed()
            );
            println!();
            return;
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
                    let highlighted = self.highlighter.highlight_text(content);
                    println!("{}", highlighted);
                    println!();
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
                                println!(
                                    "{} {}",
                                    "Executing:".bright_green().bold(),
                                    command.bright_cyan()
                                );
                            }
                        }
                    } else {
                        println!(
                            "{} {}",
                            "Using tool:".bright_yellow().bold(),
                            tool_name.bright_yellow()
                        );
                    }
                    println!("{}", tool_output.dimmed());
                    println!();
                }
            }
        }

        println!("{}", "═".repeat(80).bright_cyan());
        println!();
    }

    pub fn print_assistant_text(&self, text: &str) {
        let highlighted = self.highlighter.highlight_text(text);
        println!("{}", highlighted);
    }

    pub fn print_thinking(&self, thinking: &str) {
        if !thinking.trim().is_empty() {
            println!(
                "\n{}",
                "Thinking:".truecolor(0x77, 0x00, 0xFF).bold().dimmed()
            );
            println!("{}", thinking.dimmed());
            println!();
        }
    }

    pub fn print_tool_header(&self, tool_name: &str, command: Option<&str>) {
        if tool_name == "execute_bash" {
            if let Some(cmd) = command {
                println!(
                    "{} {}",
                    "Executing:".bright_green().bold(),
                    cmd.bright_cyan()
                );
            }
        } else {
            println!(
                "{} {}",
                "Using tool:".bright_yellow().bold(),
                tool_name.bright_yellow()
            );
        }
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

            // Hide cursor
            print!("\n\x1B[?25l");
            let _ = io::stdout().flush();

            while running_anim.load(Ordering::Relaxed) {
                print!(
                    "\r{} {} {}",
                    frames[frame_idx].truecolor(0xFF, 0x99, 0x33),
                    action_message.truecolor(0xFF, 0x99, 0x33),
                    interrupt_message.dimmed(),
                );
                let _ = io::stdout().flush();
                frame_idx = (frame_idx + 1) % frames.len();
                thread::sleep(Duration::from_millis(80));
            }

            // Clear the line and show cursor
            print!("\r{}\r", " ".repeat(70));
            print!("\x1B[?25h");
            let _ = io::stdout().flush();
        });

        let key_handle = thread::spawn(move || {
            if crossterm::terminal::enable_raw_mode().is_err() {
                return;
            }

            while running_key.load(Ordering::Relaxed) {
                if event::poll(Duration::from_millis(100)).unwrap_or(false) {
                    if let Ok(Event::Key(KeyEvent {
                        code: KeyCode::Esc, ..
                    })) = event::read()
                    {
                        interrupted_clone.store(true, Ordering::Relaxed);
                        running_key.store(false, Ordering::Relaxed);
                        break;
                    }
                }
            }

            let _ = crossterm::terminal::disable_raw_mode();
        });

        // Wait for threads with timeout to prevent hanging
        Self::join_with_timeout(animation_handle, THREAD_JOIN_TIMEOUT);
        Self::join_with_timeout(key_handle, THREAD_JOIN_TIMEOUT);

        // Ensure terminal is in a good state even if threads didn't clean up
        let _ = crossterm::terminal::disable_raw_mode();
        print!("\x1B[?25h"); // Show cursor
        let _ = io::stdout().flush();
    }

    /// Join a thread with a timeout. If the thread doesn't finish in time, abandon it.
    fn join_with_timeout<T>(handle: JoinHandle<T>, timeout: Duration) {
        let start = Instant::now();
        while !handle.is_finished() {
            if start.elapsed() > timeout {
                // Thread is stuck, abandon it (it will be cleaned up when process exits)
                return;
            }
            thread::sleep(Duration::from_millis(10));
        }
        let _ = handle.join();
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
            "morph_edit_file" => output.to_string(),
            _ => output.to_string(),
        }
    }

    fn calculate_cost(model: &str, input_tokens: u32, output_tokens: u32) -> f64 {
        // Prices per million tokens in USD
        let (input_price, output_price) = match model {
            "claude-sonnet-4-5" => (3.0, 15.0),
            "claude-opus-4-5" => (5.0, 25.0),
            "claude-haiku-4-5" => (1.0, 5.0),
            "gpt-5.1-codex-max" | "gpt-5.1-codex" | "gpt-5-codex" => (1.25, 10.0),
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
