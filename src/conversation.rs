use crate::api::{Message, SystemPrompt};

const MAX_MESSAGES: usize = 500;

/// Manages conversation history for the REPL
#[derive(Clone)]
pub struct ConversationHistory {
    messages: Vec<Message>,
    system_prompt: Vec<SystemPrompt>,
}

impl ConversationHistory {
    pub fn new() -> Self {
        Self::with_features(false, false, None)
    }

    pub fn with_features(
        has_morph: bool,
        has_code_search: bool,
        custom_instructions: Option<String>,
    ) -> Self {
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

        let mut system_text = format!(
            r#"You are Sofos, an AI coding assistant. You have access to tools that allow you to:
{}

IMPORTANT: All file operations are restricted to the current working directory for security. You cannot access files outside this directory.

When helping users:
- Be concise and practical
- ALWAYS explore first: Use list_directory to find files before trying to read them if you're unsure of their location
- Use your tools to read files before suggesting changes
{}
- Search the web when you need current information or documentation
- Execute bash commands to test code and verify functionality (read-only, no sudo or file modifications)
- Explain your reasoning when using tools

CRITICAL - Making Changes:
- NEVER make code changes or file modifications unless explicitly instructed by the user
- When the user asks for suggestions or improvements, DESCRIBE what you would change without implementing it
- Only implement changes when the user gives explicit approval (e.g., "do it", "implement that", "make the change")
- If unsure whether to implement or just suggest, always ask first

Testing after code changes:
- After editing code files (not comments, README, or documentation), ALWAYS test the changes using execute_bash
- Run appropriate build/test commands based on the project type:
  * Rust: 'cargo build' and/or 'cargo test'
  * JavaScript/TypeScript: 'npm run build' and/or 'npm test'
  * Python: 'python -m pytest' or 'python -m unittest'
  * Go: 'go build' and/or 'go test'
- If tests fail, fix the errors and test again
- Do NOT run tests for changes to: comments only, README.md, documentation files, or configuration files

Your goal is to help users with coding tasks efficiently and accurately.
Always use the metric system for all measurements. If the user uses other units, convert them and answer in metric.
Show imperial units only when the user explicitly asks for them."#,
            features.join("\n"),
            edit_instruction
        );

        // Append custom instructions if provided
        if let Some(instructions) = custom_instructions {
            system_text.push_str("\n\n");
            system_text.push_str(&instructions);
        }

        Self {
            messages: Vec::new(),
            system_prompt: vec![SystemPrompt::new_cached_with_ttl(
                system_text.to_string(),
                None,
            )],
        }
    }

    fn trim_if_needed(&mut self) {
        if self.messages.len() > MAX_MESSAGES {
            let remove_count = self.messages.len() - MAX_MESSAGES;
            self.messages.drain(0..remove_count);
        }
    }

    pub fn add_user_message(&mut self, content: String) {
        self.messages.push(Message::user(content));
        self.trim_if_needed();
    }

    pub fn add_assistant_with_blocks(&mut self, blocks: Vec<crate::api::MessageContentBlock>) {
        self.messages.push(Message::assistant_with_blocks(blocks));
        self.trim_if_needed();
    }

    pub fn add_tool_results(&mut self, results: Vec<crate::api::MessageContentBlock>) {
        self.messages.push(Message::user_with_tool_results(results));
        self.trim_if_needed();
    }

    pub fn messages(&self) -> &[Message] {
        &self.messages
    }

    pub fn system_prompt(&self) -> &Vec<SystemPrompt> {
        &self.system_prompt
    }

    pub fn clear(&mut self) {
        self.messages.clear();
    }

    pub fn restore_messages(&mut self, messages: Vec<Message>) {
        self.messages = messages;
        self.trim_if_needed();
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::MessageContentBlock;

    #[test]
    fn test_message_limit_trimming() {
        let mut history = ConversationHistory::new();

        for i in 0..510 {
            history.add_user_message(format!("Message {}", i));
        }

        assert_eq!(history.messages().len(), 500);

        if let crate::api::MessageContent::Text { content } = &history.messages()[0].content {
            assert_eq!(content, "Message 10");
        }
    }

    #[test]
    fn test_message_limit_with_blocks() {
        let mut history = ConversationHistory::new();

        for i in 0..260 {
            history.add_user_message(format!("User {}", i));
            history.add_assistant_with_blocks(vec![MessageContentBlock::Text {
                text: format!("Assistant {}", i),
                cache_control: None,
            }]);
        }

        assert_eq!(history.messages().len(), 500);
    }

    #[test]
    fn test_no_trimming_below_limit() {
        let mut history = ConversationHistory::new();

        for i in 0..20 {
            history.add_user_message(format!("Message {}", i));
        }

        assert_eq!(history.messages().len(), 20);
    }
}
