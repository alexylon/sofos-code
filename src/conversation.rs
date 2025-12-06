use crate::api::{ContentBlock, Message};

/// Manages conversation history for the REPL
pub struct ConversationHistory {
    messages: Vec<Message>,
    system_prompt: String,
}

impl ConversationHistory {
    pub fn new() -> Self {
        Self::with_features(false, false)
    }

    pub fn with_features(has_morph: bool, has_code_search: bool) -> Self {
        let mut features = vec![
            "1. Read files in the current project directory",
            "2. Write/create files in the current project directory",
            "3. List directory contents",
            "4. Create directories",
            "5. Search the web for information",
            "6. Execute read-only bash commands (for testing code)",
        ];

        if has_code_search {
            features.push("7. Search code using ripgrep");
        }

        let edit_instruction = if has_morph {
            "- When creating new files, use the write_file tool\n- When editing existing files, ALWAYS use the morph_edit_file tool (ultra-fast, 10,500+ tokens/sec)"
        } else {
            "- When creating or editing code, use the write_file tool"
        };

        let system_prompt = format!(
            r#"You are Sofos, an AI coding assistant. You have access to tools that allow you to:
{}

IMPORTANT: All file operations are restricted to the current working directory for security. You cannot access files outside this directory.

When helping users:
- Be concise and practical
- Use your tools to read files before suggesting changes
{}
- Search the web when you need current information or documentation
- Execute bash commands to test code and verify functionality (read-only, no sudo or file modifications)
- Explain your reasoning when using tools

File deletion safety:
- ALWAYS ask the user for explicit confirmation before using delete_file or delete_directory
- List the files/directories that will be deleted and wait for user approval
- Never delete files without prior user confirmation

Testing after code changes:
- After editing code files (not comments, README, or documentation), ALWAYS test the changes using execute_bash
- Run appropriate build/test commands based on the project type:
  * Rust: 'cargo build' and/or 'cargo test'
  * JavaScript/TypeScript: 'npm run build' and/or 'npm test'
  * Python: 'python -m pytest' or 'python -m unittest'
  * Go: 'go build' and/or 'go test'
- If tests fail, fix the errors and test again
- Do NOT run tests for changes to: comments only, README.md, documentation files, or configuration files

Your goal is to help users with coding tasks efficiently and accurately."#,
            features.join("\n"),
            edit_instruction
        );

        Self {
            messages: Vec::new(),
            system_prompt,
        }
    }

    pub fn add_user_message(&mut self, content: String) {
        self.messages.push(Message::user(content));
    }

    pub fn add_assistant_message(&mut self, content: String) {
        self.messages.push(Message::assistant(content));
    }

    /// Add assistant content blocks (including tool uses)
    pub fn add_assistant_content(&mut self, content_blocks: &[ContentBlock]) {
        let mut content_parts = Vec::new();

        for block in content_blocks {
            match block {
                ContentBlock::Text { text } => {
                    content_parts.push(text.clone());
                }
                ContentBlock::ToolUse { name, input, .. } => {
                    content_parts.push(format!(
                        "[Used tool: {} with input: {}]",
                        name,
                        serde_json::to_string_pretty(input).unwrap_or_default()
                    ));
                }
            }
        }

        if !content_parts.is_empty() {
            self.add_assistant_message(content_parts.join("\n\n"));
        }
    }

    pub fn add_tool_result(&mut self, tool_name: &str, result: &str) {
        self.messages.push(Message::user(format!(
            "[Tool result for {}]\n{}",
            tool_name, result
        )));
    }

    pub fn messages(&self) -> &[Message] {
        &self.messages
    }

    pub fn system_prompt(&self) -> &str {
        &self.system_prompt
    }

    pub fn clear(&mut self) {
        self.messages.clear();
    }

    pub fn _len(&self) -> usize {
        self.messages.len()
    }

    pub fn _is_empty(&self) -> bool {
        self.messages.is_empty()
    }
}

impl Default for ConversationHistory {
    fn default() -> Self {
        Self::new()
    }
}
