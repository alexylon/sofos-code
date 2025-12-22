use crate::api::{Message, SystemPrompt};
use crate::config::SofosConfig;

/// Manages conversation history for the REPL
#[derive(Clone)]
pub struct ConversationHistory {
    messages: Vec<Message>,
    system_prompt: Vec<SystemPrompt>,
    config: SofosConfig,
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

When helping users:
- Be concise and practical
- ALWAYS explore first: Use list_directory to find files before trying to read them if you're unsure of their location
- Use your tools to read files before suggesting changes
{}
- Search the web when you need current information or documentation
- Execute bash commands safely with 3-tier permission system:
  * Tier 1 (Allowed): Build tools (cargo, npm, python), read-only ops (ls, cat, grep) execute automatically
  * Tier 2 (Forbidden): Destructive commands (rm, chmod, sudo) are always blocked
  * Tier 3 (Ask): Unknown commands prompt user for permission
  * All commands are sandboxed to workspace (no parent traversal, no absolute paths)
- Never run destructive or irreversible shell commands (e.g., rm -rf, rm, rmdir, dd, mkfs*, fdisk/parted, wipefs, chmod/chown -R on broad paths, truncate, :>, >/dev/sd*, kill -9 on system services).
Do not modify or delete files outside the working directory.
Prefer read-only commands and dry-runs; if a potentially destructive action seems necessary, stop and request explicit confirmation before proceeding.
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
            config: SofosConfig::default(),
        }
    }

    /// Estimate token count for a string
    fn estimate_tokens(text: &str) -> usize {
        // Conservative: 1 token per 3.5 chars (accounts for code/JSON being token-heavy)
        (text.len() as f64 / 3.5).ceil() as usize
    }

    /// Estimate total tokens in system prompt
    fn estimate_system_tokens(&self) -> usize {
        self.system_prompt
            .iter()
            .map(|sp| Self::estimate_tokens(&sp.text))
            .sum()
    }

    /// Estimate tokens for a single message
    fn estimate_message_tokens(msg: &Message) -> usize {
        use crate::api::{MessageContent, MessageContentBlock};

        match &msg.content {
            MessageContent::Text { content } => Self::estimate_tokens(content),
            MessageContent::Blocks { content } => content
                .iter()
                .map(|block| match block {
                    MessageContentBlock::Text { text, .. } => Self::estimate_tokens(text),
                    MessageContentBlock::Thinking {
                        thinking,
                        signature,
                        ..
                    } => Self::estimate_tokens(thinking) + Self::estimate_tokens(signature) + 10,
                    MessageContentBlock::Summary { summary, .. } => {
                        Self::estimate_tokens(summary) + 10
                    }
                    MessageContentBlock::ToolUse {
                        id, name, input, ..
                    } => {
                        let input_str = serde_json::to_string(input).unwrap_or_default();
                        Self::estimate_tokens(id)
                            + Self::estimate_tokens(name)
                            + Self::estimate_tokens(&input_str)
                            + 10
                    }
                    MessageContentBlock::ToolResult {
                        tool_use_id,
                        content,
                        ..
                    } => Self::estimate_tokens(tool_use_id) + Self::estimate_tokens(content) + 10,
                    MessageContentBlock::ServerToolUse {
                        id, name, input, ..
                    } => {
                        let input_str = serde_json::to_string(input).unwrap_or_default();
                        Self::estimate_tokens(id)
                            + Self::estimate_tokens(name)
                            + Self::estimate_tokens(&input_str)
                            + 10
                    }
                    MessageContentBlock::WebSearchToolResult {
                        tool_use_id,
                        content,
                        ..
                    } => {
                        let content_str = serde_json::to_string(content).unwrap_or_default();
                        Self::estimate_tokens(tool_use_id)
                            + Self::estimate_tokens(&content_str)
                            + 20
                    }
                })
                .sum(),
        }
    }

    /// Calculate total estimated tokens for current conversation
    fn estimate_total_tokens(&self) -> usize {
        let system_tokens = self.estimate_system_tokens();
        let message_tokens: usize = self
            .messages
            .iter()
            .map(|m| Self::estimate_message_tokens(m))
            .sum();

        system_tokens + message_tokens
    }

    /// Trim messages to stay within token budget
    fn trim_if_needed(&mut self) {
        if self.messages.len() > self.config.max_messages {
            let remove_count = self.messages.len() - self.config.max_messages;
            self.messages.drain(0..remove_count);
        }

        let mut total_tokens = self.estimate_total_tokens();

        while total_tokens > self.config.max_context_tokens && self.messages.len() > 10 {
            let removed_tokens = Self::estimate_message_tokens(&self.messages[0]);
            self.messages.remove(0);
            total_tokens -= removed_tokens;
        }

        if total_tokens > self.config.max_context_tokens && self.messages.len() <= 10 {
            eprintln!(
                "⚠️  Warning: Conversation approaching token limit ({} tokens). Consider starting a new session.",
                total_tokens
            );
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

    #[test]
    fn test_token_limit_trimming() {
        let mut history = ConversationHistory::new();
        history.config.max_context_tokens = 5000;

        // ~1000 chars = ~286 tokens; system prompt ~857 tokens; need enough to exceed 5000
        let large_content = "x".repeat(1000);

        for i in 0..20 {
            history.add_user_message(format!("{} {}", i, large_content));
        }

        assert!(history.messages().len() < 20);
        assert!(history.messages().len() >= 10);

        if let crate::api::MessageContent::Text { content } = &history.messages()[0].content {
            assert!(!content.starts_with("0 "));
        }
    }

    #[test]
    fn test_token_estimation() {
        // 35 chars = 10 tokens at 3.5 chars/token
        let tokens = ConversationHistory::estimate_tokens("12345678901234567890123456789012345");
        assert_eq!(tokens, 10);

        let tokens = ConversationHistory::estimate_tokens("");
        assert_eq!(tokens, 0);
    }
}
