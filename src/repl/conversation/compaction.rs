//! Compaction-related operations on [`ConversationHistory`]: detect
//! when the conversation has grown past the auto-compact trigger,
//! pick a clean split point, shrink old tool results in place, render
//! the pre-split history into a plain-text summary block, and replace
//! the pre-split prefix with the resulting summary message.

use crate::api::{Message, utils::truncate_at_char_boundary};
use crate::repl::conversation::ConversationHistory;

/// Per-end cap on retained characters when [`ConversationHistory::truncate_tool_results`]
/// shortens a long tool output during compaction. The middle is
/// replaced with an elision marker.
const COMPACTION_TOOL_RESULT_KEEP_CHARS: usize = 500;

impl ConversationHistory {
    /// Check if conversation needs compaction. The trigger is the
    /// per-model `auto_compact_token_limit` (clamped to 90% of the
    /// API ceiling at lookup time), populated at REPL startup from
    /// [`crate::api::Model::auto_compact_at`].
    pub fn needs_compaction(&self) -> bool {
        self.estimate_total_tokens() > self.config.auto_compact_token_limit
    }

    /// Set the auto-compaction trigger, picked by model via
    /// [`crate::api::Model::auto_compact_at`]. Called once at
    /// REPL startup so compaction fires at the right point for the
    /// active model rather than the default fallback.
    pub fn set_auto_compact_token_limit(&mut self, n: usize) {
        self.config.auto_compact_token_limit = n;
    }

    /// Find a clean split point for compaction, keeping at least `preserve_recent` messages.
    /// Returns the index where "recent" messages start (split on user-message boundary).
    pub fn compaction_split_point(&self) -> usize {
        let preserve = self.config.compaction_preserve_recent;
        if self.messages.len() <= preserve + 5 {
            return 0;
        }

        // Clamp to `len - 1` so the indexing in the role-boundary walk
        // below is in-range even when the configured preserve count is
        // zero (default is 20, but a user-set 0 used to panic here).
        let mut split = self
            .messages
            .len()
            .saturating_sub(preserve)
            .min(self.messages.len() - 1);

        // Walk backward to land on a user-role message boundary
        while split > 0 && self.messages[split].role != "user" {
            split -= 1;
        }
        // Avoid orphaning tool results: if this user message contains tool_result blocks,
        // walk back further to include the preceding assistant tool_use
        while split > 0 {
            if let crate::api::MessageContent::Blocks { content } = &self.messages[split].content {
                let has_tool_result = content.iter().any(|b| {
                    matches!(
                        b,
                        crate::api::MessageContentBlock::ToolResult { .. }
                            | crate::api::MessageContentBlock::WebSearchToolResult { .. }
                    )
                });
                if has_tool_result {
                    split -= 1;
                    continue;
                }
            }
            break;
        }

        split
    }

    /// Truncate large tool results in messages[0..up_to] to save tokens cheaply.
    pub fn truncate_tool_results(&mut self, up_to: usize) {
        // In-place mutation of older message content changes the prefix
        // hash up to the anchor; invalidate so the next request doesn't
        // stamp a marker on a now-mismatched position.
        self.invalidate_cache_anchor();
        let threshold = self.config.tool_result_truncate_threshold;
        let keep_chars = COMPACTION_TOOL_RESULT_KEEP_CHARS;

        for msg in self.messages[..up_to].iter_mut() {
            if let crate::api::MessageContent::Blocks { content } = &mut msg.content {
                for block in content.iter_mut() {
                    if let crate::api::MessageContentBlock::ToolResult {
                        content: result_text,
                        ..
                    } = block
                    {
                        if result_text.len() > threshold {
                            let original_len = result_text.len();
                            let actual_keep = keep_chars.min(original_len / 3);
                            let start_end = truncate_at_char_boundary(result_text, actual_keep);
                            let end_start = {
                                let target = original_len.saturating_sub(actual_keep);
                                let mut i = target;
                                while i > 0 && !result_text.is_char_boundary(i) {
                                    i -= 1;
                                }
                                i
                            };
                            let start = &result_text[..start_end];
                            let end = &result_text[end_start..];
                            *result_text = format!(
                                "{}\n...[truncated {} chars]...\n{}",
                                start, original_len, end
                            );
                        }
                    }
                }
            }
        }
    }

    pub fn serialize_messages_for_summary(messages: &[Message]) -> String {
        let mut parts = Vec::new();

        for msg in messages {
            let role_label = if msg.role == "user" {
                "User"
            } else {
                "Assistant"
            };

            match &msg.content {
                crate::api::MessageContent::Text { content } => {
                    parts.push(format!("{}: {}", role_label, content));
                }
                crate::api::MessageContent::Blocks { content } => {
                    for block in content {
                        match block {
                            crate::api::MessageContentBlock::Text { text, .. } => {
                                parts.push(format!("{}: {}", role_label, text));
                            }
                            crate::api::MessageContentBlock::ToolUse { name, input, .. } => {
                                let input_str = serde_json::to_string(input).unwrap_or_default();
                                let input_preview = if input_str.len() > 200 {
                                    format!(
                                        "{}...",
                                        &input_str[..truncate_at_char_boundary(&input_str, 200)]
                                    )
                                } else {
                                    input_str
                                };
                                parts.push(format!("[Tool call: {}({})]", name, input_preview));
                            }
                            crate::api::MessageContentBlock::ToolResult { content, .. } => {
                                let preview = if content.len() > 300 {
                                    format!(
                                        "{}...",
                                        &content[..truncate_at_char_boundary(content, 300)]
                                    )
                                } else {
                                    content.clone()
                                };
                                parts.push(format!("[Tool result: {}]", preview));
                            }
                            crate::api::MessageContentBlock::Image { .. } => {
                                parts.push("[Image attached]".to_string());
                            }
                            // Skip thinking, summary, server tool use, web search results
                            _ => {}
                        }
                    }
                }
            }
        }

        parts.join("\n\n")
    }

    pub fn replace_with_summary(&mut self, summary: String, split_point: usize) {
        if split_point == 0 || split_point > self.messages.len() {
            return;
        }
        // Front-drain + insert shifts every remaining index; the anchor
        // can't carry across this transformation.
        self.invalidate_cache_anchor();
        self.messages.drain(0..split_point);
        let summary_msg = Message::user(format!(
            "[Conversation Summary]\n\nThe following is a summary of our earlier conversation:\n\n{}",
            summary
        ));
        self.messages.insert(0, summary_msg);
        self.maintain_cache_anchor();
    }
}
