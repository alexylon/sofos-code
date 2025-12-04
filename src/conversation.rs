use crate::api::{ContentBlock, Message};

/// Manages conversation history for the REPL
pub struct ConversationHistory {
    messages: Vec<Message>,
    system_prompt: String,
}

impl ConversationHistory {
    pub fn new() -> Self {
        let system_prompt = r#"You are Sofos, an AI coding assistant. You have access to tools that allow you to:
1. Read files in the current project directory
2. Write/create files in the current project directory
3. List directory contents
4. Create directories
5. Search the web for information

IMPORTANT: All file operations are restricted to the current working directory for security. You cannot access files outside this directory.

When helping users:
- Be concise and practical
- Use your tools to read files before suggesting changes
- When creating or editing code, use the write_file tool
- Search the web when you need current information or documentation
- Explain your reasoning when using tools

Your goal is to help users with coding tasks efficiently and accurately."#;

        Self {
            messages: Vec::new(),
            system_prompt: system_prompt.to_string(),
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
