//! History-shaping operations on [`ConversationHistory`]: trim under
//! token pressure, maintain the Anthropic cache anchor against the
//! 20-block lookback window, drop leading orphaned tool results so
//! the OpenAI Responses API doesn't reject the request, and build
//! the mechanical drop-summary used by [`Self::fallback_trim`] when
//! summarisation isn't available.

use crate::api::{Message, utils::truncate_at_char_boundary};
use crate::repl::conversation::ConversationHistory;

/// Hard floor on the number of messages [`ConversationHistory::trim_if_needed`]
/// will keep, even when the per-message budget would normally drop more.
/// Below this, conversations lose enough context that the model starts
/// hallucinating prior tool results.
const TRIM_MIN_MESSAGES: usize = 10;

impl ConversationHistory {
    /// Trim messages to stay within token budget.
    pub(super) fn trim_if_needed(&mut self) {
        let len_before = self.messages.len();

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

        // Trimming from the front can strand a user message whose
        // ToolResult blocks reference a ToolUse in an already-dropped
        // assistant message. The OpenAI Responses API rejects this with
        // "No tool call found for function call output with call_id …".
        // Drop any leading messages that still carry orphaned tool
        // results so the serialized history stays self-consistent. The
        // drop can move the total token count, so recompute before the
        // "approaching limit" warning to avoid reporting stale numbers.
        let stripped_orphan = self.drop_leading_orphaned_tool_results();
        total_tokens = self.estimate_total_tokens();

        // Invalidate the anchor when ANY front-of-history mutation
        // happened — index shift OR in-place strip of `messages[0]`
        // (the latter changes the prefix hash for every anchor
        // position, since the prefix up to the anchor includes
        // `messages[0]`). Pure appends leave the anchor untouched.
        if self.messages.len() != len_before || stripped_orphan {
            self.invalidate_cache_anchor();
        }

        // The warning describes our internal trim heuristic, not the
        // model's API context window — those are different numbers.
        // The condition below means: we tried to trim down to budget
        // but hit the `TRIM_MIN_MESSAGES` floor. The model API will
        // still accept the request; this just warns the user that
        // auto-trim can't help further. Dedup with `warned_at_floor`
        // so a long agent loop doesn't print the warning on every
        // tool round-trip.
        let at_floor = total_tokens > self.config.max_context_tokens
            && self.messages.len() <= TRIM_MIN_MESSAGES;
        if at_floor {
            if !self.warned_at_floor {
                tracing::warn!(
                    floor = TRIM_MIN_MESSAGES,
                    tokens = total_tokens,
                    budget = self.config.max_context_tokens,
                    "auto-trim hit the message floor; run /compact or /clear if responses degrade"
                );
                self.warned_at_floor = true;
            }
        } else {
            self.warned_at_floor = false;
        }

        self.maintain_cache_anchor();
    }

    pub(super) fn message_block_count(msg: &Message) -> usize {
        match &msg.content {
            crate::api::MessageContent::Text { .. } => 1,
            crate::api::MessageContent::Blocks { content } => content.len(),
        }
    }

    /// Drives the "advance the anchor?" decision in
    /// `maintain_cache_anchor` against Anthropic's 20-block lookback
    /// window — note this excludes the rolling message itself, so a
    /// single very wide rolling message doesn't force an advance.
    pub(super) fn block_distance(&self, from: usize, to: usize) -> usize {
        self.messages[from..to]
            .iter()
            .map(Self::message_block_count)
            .sum()
    }

    /// Pick the secondary `cache_control` ("anchor") position so that
    /// even a single iteration adding more than 20 blocks still finds
    /// at least one cached entry within Anthropic's lookback window.
    /// The anchor stays put across turns until the rolling breakpoint
    /// has drifted more than ~18 blocks past it; then it advances to
    /// roughly 10 blocks behind the current rolling. Stamps only land
    /// on Blocks-variant messages — Text-variant has no per-block
    /// `cache_control` field, so picking one would silently waste the
    /// 4th breakpoint slot.
    pub(super) fn maintain_cache_anchor(&mut self) {
        const KEEP_DISTANCE_BLOCKS: usize = 18;
        const TARGET_OFFSET_BLOCKS: usize = 10;

        let len = self.messages.len();
        if len < 2 {
            self.cache_anchor_message_idx = None;
            return;
        }
        let rolling_idx = len - 1;

        if let Some(idx) = self.cache_anchor_message_idx {
            let still_valid = idx < rolling_idx
                && matches!(
                    self.messages[idx].content,
                    crate::api::MessageContent::Blocks { .. }
                );
            if !still_valid {
                self.cache_anchor_message_idx = None;
            }
        }

        if let Some(idx) = self.cache_anchor_message_idx {
            if self.block_distance(idx, rolling_idx) <= KEEP_DISTANCE_BLOCKS {
                return;
            }
        }

        let mut blocks_back = 0;
        for i in (0..rolling_idx).rev() {
            blocks_back += Self::message_block_count(&self.messages[i]);
            if blocks_back >= TARGET_OFFSET_BLOCKS
                && matches!(
                    self.messages[i].content,
                    crate::api::MessageContent::Blocks { .. }
                )
            {
                self.cache_anchor_message_idx = Some(i);
                return;
            }
        }

        self.cache_anchor_message_idx = None;
    }

    pub fn cache_anchor_message_idx(&self) -> Option<usize> {
        self.cache_anchor_message_idx
    }

    /// Drop the secondary cache-control breakpoint. Call this from any
    /// mutator that changes content at or before the current anchor or
    /// shifts indices into the anchored prefix; the next
    /// [`Self::maintain_cache_anchor`] re-establishes the anchor from
    /// the new state. Tail-only mutations (append, `last_mut`, pop the
    /// rolling) leave the anchor valid and must NOT call this.
    pub(super) fn invalidate_cache_anchor(&mut self) {
        self.cache_anchor_message_idx = None;
    }

    /// Drop leading messages whose content still references tool calls
    /// that have been trimmed away. Called after any operation that
    /// removes messages from the front of the history. Returns `true`
    /// if any blocks were stripped or any message was removed — the
    /// cache anchor must be invalidated in either case because the
    /// prefix bytes up to the anchor include `messages[0]`.
    ///
    /// Preserves sibling `Text` / `Image` blocks in mixed user messages.
    /// A user turn can legitimately carry `[ToolResult, Text]` — the
    /// `Text` is a steer message that was folded into the tool-results
    /// turn (see `response_handler::drain_steer_messages`). If trim
    /// drops the preceding assistant `ToolUse`, the `ToolResult` is
    /// orphaned but the `Text` isn't. Strip only the orphaned blocks;
    /// remove the whole message only when nothing survives the strip.
    pub(super) fn drop_leading_orphaned_tool_results(&mut self) -> bool {
        let mut mutated = false;
        loop {
            let head_has_orphan = self
                .messages
                .first()
                .is_some_and(|m| m.role == "user" && Self::message_has_tool_result(m));
            if !head_has_orphan {
                return mutated;
            }

            mutated = true;
            if let crate::api::MessageContent::Blocks { content } = &mut self.messages[0].content {
                content
                    .retain(|b| !matches!(b, crate::api::MessageContentBlock::ToolResult { .. }));
                if !content.is_empty() {
                    return mutated;
                }
            }
            self.messages.remove(0);
        }
    }

    pub(super) fn message_has_tool_result(msg: &Message) -> bool {
        matches!(
            &msg.content,
            crate::api::MessageContent::Blocks { content }
                if content.iter().any(|b| matches!(
                    b,
                    crate::api::MessageContentBlock::ToolResult { .. }
                ))
        )
    }

    /// Drop tool-use blocks from the tail assistant message when no
    /// matching tool-result follows. Triggered by `restore_messages`
    /// to guard against a session file written between
    /// `add_assistant_with_blocks` and `add_tool_results` — a hard
    /// kill at that moment would otherwise leave the resumed
    /// conversation with an orphan that the provider 400s on the
    /// next request.
    pub(super) fn drop_tail_orphaned_tool_uses(&mut self) -> bool {
        let Some(last) = self.messages.last_mut() else {
            return false;
        };
        if last.role != "assistant" {
            return false;
        }
        let crate::api::MessageContent::Blocks { content } = &mut last.content else {
            return false;
        };
        let had_initiator = content.iter().any(|b| {
            matches!(
                b,
                crate::api::MessageContentBlock::ToolUse { .. }
                    | crate::api::MessageContentBlock::ServerToolUse { .. }
                    | crate::api::MessageContentBlock::WebSearchToolResult { .. }
            )
        });
        if !had_initiator {
            return false;
        }
        content.retain(|b| {
            !matches!(
                b,
                crate::api::MessageContentBlock::ToolUse { .. }
                    | crate::api::MessageContentBlock::ServerToolUse { .. }
                    | crate::api::MessageContentBlock::WebSearchToolResult { .. }
            )
        });
        if content.is_empty() {
            content.push(crate::api::MessageContentBlock::Text {
                text: "[Tool call interrupted before execution]".to_string(),
                cache_control: None,
            });
        }
        true
    }

    /// Build a brief summary of messages about to be dropped (no LLM, just key facts).
    pub(super) fn build_drop_summary(messages: &[Message]) -> String {
        let mut tools_used = Vec::new();
        let mut files_mentioned = Vec::new();
        let mut user_topics = Vec::new();

        let text_preview = |text: &str| -> Option<String> {
            let preview = if text.len() > 100 {
                format!("{}...", &text[..truncate_at_char_boundary(text, 100)])
            } else {
                text.to_string()
            };
            if preview.trim().is_empty() {
                None
            } else {
                Some(preview)
            }
        };

        for msg in messages {
            let is_user = msg.role == "user";
            match &msg.content {
                crate::api::MessageContent::Blocks { content } => {
                    for block in content {
                        match block {
                            crate::api::MessageContentBlock::ToolUse { name, input, .. } => {
                                if !tools_used.contains(name) {
                                    tools_used.push(name.clone());
                                }
                                if let Some(path) = input.get("path").and_then(|v| v.as_str()) {
                                    let p = path.to_string();
                                    if !files_mentioned.contains(&p) {
                                        files_mentioned.push(p);
                                    }
                                }
                            }
                            crate::api::MessageContentBlock::Text { text, .. } if is_user => {
                                if let Some(preview) = text_preview(text) {
                                    user_topics.push(preview);
                                }
                            }
                            _ => {}
                        }
                    }
                }
                crate::api::MessageContent::Text { content } if is_user => {
                    if let Some(preview) = text_preview(content) {
                        user_topics.push(preview);
                    }
                }
                _ => {}
            }
        }

        let mut parts = Vec::new();
        if !user_topics.is_empty() {
            let topics: Vec<_> = user_topics.into_iter().take(5).collect();
            parts.push(format!("User requests: {}", topics.join(" | ")));
        }
        if !tools_used.is_empty() {
            parts.push(format!("Tools used: {}", tools_used.join(", ")));
        }
        if !files_mentioned.is_empty() {
            let files: Vec<_> = files_mentioned.into_iter().take(20).collect();
            parts.push(format!("Files: {}", files.join(", ")));
        }
        parts.join("\n")
    }

    /// Fallback trim used when compaction fails.
    /// Builds a mechanical summary of dropped messages before trimming.
    pub fn fallback_trim(&mut self) {
        let msg_count_before = self.messages.len();
        if msg_count_before <= 10 {
            self.trim_if_needed();
            return;
        }

        // Simulate trim_if_needed to find which messages will be dropped
        let max_msg_drop = self.messages.len().saturating_sub(self.config.max_messages);
        let mut token_drop = 0;
        let mut simulated_tokens = self.estimate_total_tokens();
        for msg in self.messages.iter().take(max_msg_drop) {
            simulated_tokens -= Self::estimate_message_tokens(msg);
        }
        let remaining = self.messages.len() - max_msg_drop;
        for i in 0..remaining.saturating_sub(10) {
            if simulated_tokens <= self.config.max_context_tokens {
                break;
            }
            simulated_tokens -= Self::estimate_message_tokens(&self.messages[max_msg_drop + i]);
            token_drop += 1;
        }
        let total_drop = max_msg_drop + token_drop;

        let summary = if total_drop >= 5 {
            Self::build_drop_summary(&self.messages[..total_drop])
        } else {
            String::new()
        };

        self.trim_if_needed();

        if !summary.is_empty() {
            let dropped = msg_count_before - self.messages.len();
            let summary_msg = Message::user(format!(
                "[Context trimmed — {} earlier messages dropped]\n\n{}",
                dropped, summary
            ));
            self.messages.insert(0, summary_msg);
        }
    }
}
