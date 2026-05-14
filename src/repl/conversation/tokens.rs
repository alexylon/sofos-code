//! Token-estimation helpers on [`ConversationHistory`]. The estimates
//! are deliberately coarse — fixed character-per-token ratio plus a
//! small per-block constant for the wire-format overhead. The numbers
//! drive the trim-floor and compaction-trigger paths, not API billing
//! (which uses the real provider-reported counts).

use crate::api::Message;
use crate::repl::conversation::ConversationHistory;

impl ConversationHistory {
    /// Override the per-model context-window ceiling used by
    /// [`Self::trim_if_needed`] as the trim floor. Called from REPL
    /// startup so the trim floor matches the model's real context
    /// window rather than the 165k default fallback.
    pub fn set_max_context_tokens(&mut self, n: usize) {
        self.config.max_context_tokens = n;
    }

    pub fn estimate_tokens(text: &str) -> usize {
        // Conservative: 1 token per 3.5 chars (accounts for code/JSON being token-heavy)
        (text.len() as f64 / 3.5).ceil() as usize
    }

    pub(super) fn estimate_system_tokens(&self) -> usize {
        self.system_prompt
            .iter()
            .map(|sp| Self::estimate_tokens(&sp.text))
            .sum()
    }

    pub(super) fn estimate_message_tokens(msg: &Message) -> usize {
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
                    MessageContentBlock::Compaction { content, .. } => {
                        Self::estimate_tokens(content) + 10
                    }
                    MessageContentBlock::Reasoning {
                        id,
                        summary,
                        encrypted_content,
                        ..
                    } => {
                        let summary_tokens: usize =
                            summary.iter().map(|s| Self::estimate_tokens(s)).sum();
                        let enc_tokens = encrypted_content
                            .as_ref()
                            .map(|s| Self::estimate_tokens(s))
                            .unwrap_or(0);
                        Self::estimate_tokens(id) + summary_tokens + enc_tokens + 10
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
                    MessageContentBlock::Image { source, .. } => {
                        // Images are tokenized based on pixel dimensions
                        // Estimate ~1000 tokens per image (typical for medium-sized images)
                        // Actual formula: tokens = (width * height) / 750
                        match source {
                            crate::api::ImageSource::Base64 { data, .. } => {
                                // Rough estimate based on base64 data size
                                // Base64 encodes 3 bytes into 4 chars, so decode estimate
                                let estimated_bytes = data.len() * 3 / 4;
                                // Assume typical compression, estimate pixels
                                // Very rough: ~10 bytes per pixel after compression
                                let estimated_pixels = estimated_bytes / 10;
                                (estimated_pixels / 750).max(100)
                            }
                            crate::api::ImageSource::Url { .. } => {
                                // Can't know size without fetching; assume medium image
                                1000
                            }
                        }
                    }
                })
                .sum(),
        }
    }

    pub fn estimate_total_tokens(&self) -> usize {
        let system_tokens = self.estimate_system_tokens();
        let message_tokens: usize = self
            .messages
            .iter()
            .map(Self::estimate_message_tokens)
            .sum();

        system_tokens + message_tokens
    }
}
