use crate::session::DisplayMessage;
use crate::session::history::Session;
use crate::ui::UI;
use colored::Colorize;
use std::io;

impl UI {
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
                    tool_input,
                    tool_output,
                } => {
                    let command = if tool_name == crate::tools::ToolName::ExecuteBash.as_str() {
                        tool_input.get("command").and_then(|v| v.as_str())
                    } else {
                        None
                    };
                    self.print_tool_header(tool_name, command);
                    // `print_tool_header` doesn't terminate the bash
                    // header with a newline — the live path relies on
                    // the post-execution `println!()` to do that. Replay
                    // it here so the header doesn't run into the output.
                    if tool_name == crate::tools::ToolName::ExecuteBash.as_str()
                        && command.is_some()
                    {
                        println!();
                    }
                    self.print_tool_output(tool_output);
                }
            }
        }

        println!("{}", "═".repeat(80).bright_cyan());
        println!();
        Ok(())
    }

    pub fn create_tool_display_message(
        tool_name: &str,
        tool_input: &serde_json::Value,
        output: &str,
    ) -> String {
        match crate::tools::ToolName::from_str(tool_name) {
            Ok(tool) => tool.display_summary(tool_input, output),
            // Unknown tool name (e.g. an MCP-routed tool); fall back to
            // raw output. Matches the legacy default-arm behaviour.
            Err(_) => output.to_string(),
        }
    }
}

#[cfg(test)]
mod tests {
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
