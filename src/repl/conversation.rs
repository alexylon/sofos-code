use crate::api::{Message, SystemPrompt, utils::truncate_at_char_boundary};
use crate::config::SofosConfig;

#[derive(Clone)]
pub struct ConversationHistory {
    messages: Vec<Message>,
    system_prompt: Vec<SystemPrompt>,
    config: SofosConfig,
    /// Set when `trim_if_needed` printed the floor-hit warning; cleared
    /// the next time we end a trim under budget. Stops the warning from
    /// firing on every message append once we're stuck at the 10-message
    /// floor.
    warned_at_floor: bool,
    /// Index of the message whose last block carries the secondary
    /// Anthropic `cache_control` marker (the "anchor"). Stays put across
    /// turns whenever the rolling breakpoint stays within the 20-block
    /// lookback window; advances only when it would otherwise fall out
    /// of range. Without this, a single iteration adding more than ~20
    /// blocks (wide multi-tool turn) would cold-miss every cache entry.
    cache_anchor_message_idx: Option<usize>,
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
            "7. View images (user includes image path or URL in their message)",
        ];

        if has_code_search {
            features.push("8. Search code using ripgrep");
        }

        let edit_instruction = if has_morph {
            "- When creating new files, use the write_file tool\n- When editing existing files, ALWAYS use the morph_edit_file tool (ultra-fast, 10,500+ tokens/sec)"
        } else {
            "- When creating or editing code, use the write_file tool"
        };

        let write_scope_tools = if has_morph {
            "write_file, edit_file, and morph_edit_file"
        } else {
            "write_file and edit_file"
        };

        let mut system_text = format!(
            r#"You are Sofos, an AI coding assistant. You have access to tools that allow you to:
{}

When helping users:
- Be concise and practical
- Context interpretation: When users refer to "this code", "these files", or similar context-dependent terms without specifying a path, they mean the code in the current working directory
- ALWAYS explore first: Use list_directory to find files before trying to read them if you're unsure of their location
- Use your tools to read files before suggesting changes
{}
- Search the web when you need current information or documentation
- Execute bash commands safely with 3-tier permission system:
  * Tier 1 (Allowed): Build tools (cargo, npm, python), read-only ops (ls, cat, grep) execute automatically
  * Tier 2 (Forbidden): Destructive commands (rm, chmod, sudo) are always blocked
  * Tier 3 (Ask): Unknown commands prompt user for permission
  * Parent directory traversal (..) is always blocked in bash commands
- Never run destructive or irreversible shell commands (e.g., rm -rf, rm, rmdir, dd, mkfs*, fdisk/parted, wipefs, chmod/chown -R on broad paths, truncate, :>, >/dev/sd*, kill -9 on system services).
Prefer read-only commands and dry-runs; if a potentially destructive action seems necessary, stop and request explicit confirmation before proceeding.
- Explain your reasoning when using tools

Outside Workspace Access (three separate scopes, each prompted independently):
- Read scope: read_file and list_directory can access absolute or ~/ paths. If not pre-configured, the user is prompted to allow access and can optionally remember the decision.
- Write scope: {} can write to absolute or ~/ paths. The user is prompted for Write access separately from Read.
- Bash scope: bash commands can reference absolute or ~/ paths. The user is prompted for Bash path access. Use absolute paths (not ..) for external directories.
- All three scopes are independent — Read access does not grant Write or Bash access.
- When accessing external paths, just use the absolute or ~/ path directly. If not yet allowed, the user will be prompted interactively.
- Images: users can view images by including the path in their message (works for both workspace and permitted outside paths)

Image Vision:
- When users include image paths (.jpg, .png, .gif, .webp) or URLs in their message, you will see the image
- Local images: relative paths (in workspace) or absolute/~/ paths (if permitted in config)
- Web images: URLs starting with http:// or https://
- You do NOT need to use any tool to view images - they are automatically loaded and shown to you
- If asked to view an image, tell the user to include the image path or URL in their message

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
            edit_instruction,
            write_scope_tools
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
            warned_at_floor: false,
            cache_anchor_message_idx: None,
        }
    }

    /// Set the trim threshold, typically picked by model via
    /// `crate::config::max_context_tokens_for`. Called once at REPL
    /// startup so the trim floor matches the model's real context
    /// window rather than the 165k default fallback.
    pub fn set_max_context_tokens(&mut self, n: usize) {
        self.config.max_context_tokens = n;
    }

    pub fn estimate_tokens(text: &str) -> usize {
        // Conservative: 1 token per 3.5 chars (accounts for code/JSON being token-heavy)
        (text.len() as f64 / 3.5).ceil() as usize
    }

    fn estimate_system_tokens(&self) -> usize {
        self.system_prompt
            .iter()
            .map(|sp| Self::estimate_tokens(&sp.text))
            .sum()
    }

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

    /// Trim messages to stay within token budget.
    fn trim_if_needed(&mut self) {
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
            self.cache_anchor_message_idx = None;
        }

        // The warning describes our internal trim heuristic, not the
        // model's API context window — those are different numbers.
        // The condition below means: we tried to trim down to budget
        // but hit the 10-message floor. The model API will still accept
        // the request; this just warns the user that auto-trim can't
        // help further. Dedup with `warned_at_floor` so a long agent
        // loop doesn't print the warning on every tool round-trip.
        let at_floor = total_tokens > self.config.max_context_tokens && self.messages.len() <= 10;
        if at_floor {
            if !self.warned_at_floor {
                eprintln!(
                    "⚠️  Auto-trim hit the 10-message floor at ~{} tokens (budget {}). \
                     Run /compact or /clear if responses start degrading.",
                    total_tokens, self.config.max_context_tokens
                );
                self.warned_at_floor = true;
            }
        } else {
            self.warned_at_floor = false;
        }

        self.maintain_cache_anchor();
    }

    fn message_block_count(msg: &Message) -> usize {
        match &msg.content {
            crate::api::MessageContent::Text { .. } => 1,
            crate::api::MessageContent::Blocks { content } => content.len(),
        }
    }

    /// Drives the "advance the anchor?" decision in
    /// `maintain_cache_anchor` against Anthropic's 20-block lookback
    /// window — note this excludes the rolling message itself, so a
    /// single very wide rolling message doesn't force an advance.
    fn block_distance(&self, from: usize, to: usize) -> usize {
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
    fn maintain_cache_anchor(&mut self) {
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
    fn drop_leading_orphaned_tool_results(&mut self) -> bool {
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

    fn message_has_tool_result(msg: &Message) -> bool {
        matches!(
            &msg.content,
            crate::api::MessageContent::Blocks { content }
                if content.iter().any(|b| matches!(
                    b,
                    crate::api::MessageContentBlock::ToolResult { .. }
                ))
        )
    }

    /// Build a brief summary of messages about to be dropped (no LLM, just key facts).
    fn build_drop_summary(messages: &[Message]) -> String {
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

    /// Check if conversation needs compaction (token usage > trigger ratio)
    pub fn needs_compaction(&self) -> bool {
        let threshold =
            (self.config.max_context_tokens as f64 * self.config.compaction_trigger_ratio) as usize;
        self.estimate_total_tokens() > threshold
    }

    /// Find a clean split point for compaction, keeping at least `preserve_recent` messages.
    /// Returns the index where "recent" messages start (split on user-message boundary).
    pub fn compaction_split_point(&self) -> usize {
        let preserve = self.config.compaction_preserve_recent;
        if self.messages.len() <= preserve + 5 {
            return 0;
        }

        let mut split = self.messages.len().saturating_sub(preserve);

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
        self.cache_anchor_message_idx = None;
        let threshold = self.config.tool_result_truncate_threshold;
        let keep_chars = 500;

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
        self.cache_anchor_message_idx = None;
        self.messages.drain(0..split_point);
        let summary_msg = Message::user(format!(
            "[Conversation Summary]\n\nThe following is a summary of our earlier conversation:\n\n{}",
            summary
        ));
        self.messages.insert(0, summary_msg);
        self.maintain_cache_anchor();
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

    pub fn add_user_message(&mut self, content: String) {
        self.messages.push(Message::user(content));
        self.trim_if_needed();
    }

    pub fn add_user_with_blocks(&mut self, blocks: Vec<crate::api::MessageContentBlock>) {
        self.messages.push(Message::user_with_blocks(blocks));
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

    /// Append a plain-text block to the last user message when it already
    /// carries `Blocks` content (e.g. a user turn holding `ToolResult`
    /// blocks). Returns `true` if the append happened, `false` if there
    /// is no suitable user-role tail to extend — callers should fall
    /// back to [`add_user_message`] in that case.
    ///
    /// Used by the post-tool interrupt path to avoid emitting two
    /// consecutive user messages (the tool-results turn plus an interrupt
    /// notice), which OpenAI's strict role-alternation validator rejects.
    pub fn append_text_to_last_user_blocks(&mut self, text: String) -> bool {
        if let Some(last) = self.messages.last_mut() {
            if last.role == "user" {
                if let crate::api::MessageContent::Blocks { content } = &mut last.content {
                    content.push(crate::api::MessageContentBlock::Text {
                        text,
                        cache_control: None,
                    });
                    return true;
                }
            }
        }
        false
    }

    pub fn messages(&self) -> &[Message] {
        &self.messages
    }

    pub fn system_prompt(&self) -> &Vec<SystemPrompt> {
        &self.system_prompt
    }

    pub fn clear(&mut self) {
        self.messages.clear();
        self.cache_anchor_message_idx = None;
    }

    pub fn restore_messages(&mut self, messages: Vec<Message>) {
        // The new history has no relationship to the prior conversation;
        // any inherited anchor index is meaningless content-wise.
        self.cache_anchor_message_idx = None;
        self.messages = messages;
        self.trim_if_needed();
    }

    /// Remove the last message from the conversation (used for error recovery)
    pub fn remove_last_message(&mut self) {
        self.messages.pop();
        self.maintain_cache_anchor();
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
    fn test_append_text_to_last_user_blocks_extends_tool_results_turn() {
        let mut history = ConversationHistory::new();
        // Pair a user query + assistant ToolUse before the tool-results
        // turn so `drop_leading_orphaned_tool_results` (called from
        // `trim_if_needed`) doesn't drop our message as an orphan.
        history.add_user_message("query".to_string());
        history.add_assistant_with_blocks(vec![MessageContentBlock::ToolUse {
            id: "call_xyz".to_string(),
            name: "read_file".to_string(),
            input: serde_json::json!({"path": "a.rs"}),
            cache_control: None,
        }]);
        history.add_tool_results(vec![MessageContentBlock::ToolResult {
            tool_use_id: "call_xyz".to_string(),
            content: "file contents".to_string(),
            cache_control: None,
        }]);

        let appended = history.append_text_to_last_user_blocks("hello".to_string());
        assert!(appended, "append should succeed on a user-blocks tail");

        let last = history.messages().last().unwrap();
        assert_eq!(last.role, "user");
        if let crate::api::MessageContent::Blocks { content } = &last.content {
            assert_eq!(content.len(), 2, "expected ToolResult + Text");
            assert!(matches!(
                &content[0],
                MessageContentBlock::ToolResult { .. }
            ));
            match &content[1] {
                MessageContentBlock::Text { text, .. } => assert_eq!(text, "hello"),
                _ => panic!("expected Text block at index 1"),
            }
        } else {
            panic!("expected Blocks content");
        }
    }

    #[test]
    fn test_append_text_to_last_user_blocks_noop_when_last_is_text_only() {
        let mut history = ConversationHistory::new();
        history.add_user_message("just text".to_string());

        let appended = history.append_text_to_last_user_blocks("suffix".to_string());
        assert!(
            !appended,
            "append should refuse to extend a Text-variant user message"
        );
    }

    #[test]
    fn test_append_text_to_last_user_blocks_noop_on_empty_history() {
        let mut history = ConversationHistory::new();
        let appended = history.append_text_to_last_user_blocks("text".to_string());
        assert!(!appended, "append should refuse on empty history");
    }

    #[test]
    fn test_drop_orphaned_tool_results_preserves_mixed_text_block() {
        // A user turn carrying `[ToolResult, Text]` models the mid-turn
        // steer flow: drain_steer_messages folded the user's text into
        // the tool-results turn. If trim severs the preceding ToolUse,
        // the ToolResult is orphaned but the Text isn't — it should
        // survive the orphan drop.
        let mut history = ConversationHistory::new();
        let messages = vec![Message::user_with_blocks(vec![
            MessageContentBlock::ToolResult {
                tool_use_id: "call_orphan".to_string(),
                content: "doesn't matter".to_string(),
                cache_control: None,
            },
            MessageContentBlock::Text {
                text: "please reconsider".to_string(),
                cache_control: None,
            },
        ])];
        history.restore_messages(messages);
        history.drop_leading_orphaned_tool_results();

        assert_eq!(
            history.messages().len(),
            1,
            "message should survive — it still has the Text block"
        );
        if let crate::api::MessageContent::Blocks { content } = &history.messages()[0].content {
            assert_eq!(content.len(), 1, "only the Text block should remain");
            assert!(
                matches!(&content[0], MessageContentBlock::Text { text, .. } if text == "please reconsider")
            );
        } else {
            panic!("expected Blocks content");
        }
    }

    #[test]
    fn test_drop_orphaned_tool_results_removes_pure_tool_result_turn() {
        let mut history = ConversationHistory::new();
        let messages = vec![Message::user_with_tool_results(vec![
            MessageContentBlock::ToolResult {
                tool_use_id: "call_x".to_string(),
                content: "orphaned".to_string(),
                cache_control: None,
            },
        ])];
        history.restore_messages(messages);
        history.drop_leading_orphaned_tool_results();
        assert_eq!(history.messages().len(), 0);
    }

    #[test]
    fn test_drop_orphaned_tool_results_leaves_assistant_head_alone() {
        // An assistant at the head carries ToolUse, not ToolResult. The
        // orphan drop must not touch it — the ToolUse pairs with a
        // user-tool-results turn deeper in the surviving history.
        let mut history = ConversationHistory::new();
        let messages = vec![
            Message::assistant_with_blocks(vec![MessageContentBlock::ToolUse {
                id: "call_live".to_string(),
                name: "read_file".to_string(),
                input: serde_json::json!({"path": "b.rs"}),
                cache_control: None,
            }]),
            Message::user_with_tool_results(vec![MessageContentBlock::ToolResult {
                tool_use_id: "call_live".to_string(),
                content: "paired".to_string(),
                cache_control: None,
            }]),
        ];
        history.restore_messages(messages);
        history.drop_leading_orphaned_tool_results();
        assert_eq!(history.messages().len(), 2);
    }

    #[test]
    fn test_trim_drops_orphaned_leading_tool_results() {
        let mut history = ConversationHistory::new();
        history.config.max_messages = 3;

        // Shape the history so a naive trim would leave a user-with-
        // ToolResult stranded at the front — the exact pattern the
        // OpenAI Responses API rejects with "No tool call found for
        // function call output with call_id …".
        let messages = vec![
            Message::user("initial query".to_string()),
            Message::assistant_with_blocks(vec![MessageContentBlock::ToolUse {
                id: "call_abc".to_string(),
                name: "read_file".to_string(),
                input: serde_json::json!({"path": "a.rs"}),
                cache_control: None,
            }]),
            Message::user_with_tool_results(vec![MessageContentBlock::ToolResult {
                tool_use_id: "call_abc".to_string(),
                content: "file contents".to_string(),
                cache_control: None,
            }]),
            Message::assistant_with_blocks(vec![MessageContentBlock::Text {
                text: "done".to_string(),
                cache_control: None,
            }]),
            Message::user("next".to_string()),
        ];

        history.restore_messages(messages);

        // Both the original user query and the assistant ToolUse must
        // be trimmed. The orphaned ToolResult that used to follow them
        // should also be dropped, leaving the assistant's text reply
        // as the new front.
        let first = &history.messages()[0];
        let first_has_tool_result = matches!(
            &first.content,
            crate::api::MessageContent::Blocks { content }
                if content.iter().any(|b| matches!(
                    b,
                    crate::api::MessageContentBlock::ToolResult { .. }
                ))
        );
        assert!(
            !first_has_tool_result,
            "front of history still references a trimmed tool call"
        );
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

    #[test]
    fn test_needs_compaction() {
        let mut history = ConversationHistory::new();
        // Use a large token limit so the system prompt alone doesn't trigger it
        history.config.max_context_tokens = 100_000;
        history.config.compaction_trigger_ratio = 0.80;

        // Should not need compaction with small messages
        history.add_user_message("hello".to_string());
        assert!(!history.needs_compaction());

        // Add enough messages to exceed 80% of 100k
        let large_content = "x".repeat(10_000);
        for _ in 0..30 {
            history.messages.push(Message::user(large_content.clone()));
        }
        assert!(history.needs_compaction());
    }

    #[test]
    fn test_compaction_split_point() {
        let mut history = ConversationHistory::new();
        history.config.compaction_preserve_recent = 4;

        for i in 0..10 {
            history.messages.push(Message::user(format!("msg {}", i)));
        }

        let split = history.compaction_split_point();
        assert_eq!(split, 6); // 10 - 4 = 6
    }

    #[test]
    fn test_compaction_split_too_few_messages() {
        let mut history = ConversationHistory::new();
        history.config.compaction_preserve_recent = 20;

        for i in 0..10 {
            history.messages.push(Message::user(format!("msg {}", i)));
        }

        let split = history.compaction_split_point();
        assert_eq!(split, 0); // not enough to compact
    }

    #[test]
    fn test_truncate_tool_results() {
        let mut history = ConversationHistory::new();
        history.config.tool_result_truncate_threshold = 100;

        let large_content = "x".repeat(500);
        history.messages.push(Message::user_with_tool_results(vec![
            MessageContentBlock::ToolResult {
                tool_use_id: "id1".to_string(),
                content: large_content,
                cache_control: None,
            },
        ]));

        history.truncate_tool_results(1);

        if let crate::api::MessageContent::Blocks { content } = &history.messages()[0].content {
            if let MessageContentBlock::ToolResult { content, .. } = &content[0] {
                assert!(content.contains("truncated"));
                assert!(content.len() < 500); // keep 500/3=166 each side + marker < 500
            } else {
                panic!("Expected ToolResult");
            }
        } else {
            panic!("Expected Blocks");
        }
    }

    #[test]
    fn test_replace_with_summary() {
        let mut history = ConversationHistory::new();

        for i in 0..10 {
            history.messages.push(Message::user(format!("msg {}", i)));
        }

        history.replace_with_summary("This is the summary".to_string(), 7);

        // 7 removed, 1 summary inserted, 3 remaining = 4
        assert_eq!(history.messages().len(), 4);

        if let crate::api::MessageContent::Text { content } = &history.messages()[0].content {
            assert!(content.contains("Conversation Summary"));
            assert!(content.contains("This is the summary"));
        }
    }

    #[test]
    fn test_serialize_messages_for_summary() {
        let messages = vec![
            Message::user("Hello, help me with code".to_string()),
            Message::assistant_with_blocks(vec![MessageContentBlock::Text {
                text: "Sure, let me look at the files.".to_string(),
                cache_control: None,
            }]),
        ];

        let serialized = ConversationHistory::serialize_messages_for_summary(&messages);
        assert!(serialized.contains("User: Hello, help me with code"));
        assert!(serialized.contains("Assistant: Sure, let me look at the files."));
    }

    #[test]
    fn test_build_drop_summary_extracts_tools_and_files() {
        let messages = vec![
            Message::user("fix the bug in auth".to_string()),
            Message::assistant_with_blocks(vec![MessageContentBlock::ToolUse {
                id: "1".to_string(),
                name: "read_file".to_string(),
                input: serde_json::json!({"path": "/src/auth.rs"}),
                cache_control: None,
            }]),
            Message::assistant_with_blocks(vec![MessageContentBlock::ToolUse {
                id: "2".to_string(),
                name: "edit_file".to_string(),
                input: serde_json::json!({"path": "/src/auth.rs", "old_string": "a", "new_string": "b"}),
                cache_control: None,
            }]),
            Message::user("now fix the tests".to_string()),
            Message::assistant_with_blocks(vec![MessageContentBlock::ToolUse {
                id: "3".to_string(),
                name: "read_file".to_string(),
                input: serde_json::json!({"path": "/src/tests.rs"}),
                cache_control: None,
            }]),
        ];

        let summary = ConversationHistory::build_drop_summary(&messages);
        assert!(summary.contains("read_file"));
        assert!(summary.contains("edit_file"));
        assert!(summary.contains("/src/auth.rs"));
        assert!(summary.contains("/src/tests.rs"));
        assert!(summary.contains("fix the bug"));
        assert!(summary.contains("fix the tests"));
    }

    #[test]
    fn test_build_drop_summary_empty_messages() {
        let summary = ConversationHistory::build_drop_summary(&[]);
        assert!(summary.is_empty());
    }

    #[test]
    fn test_build_drop_summary_limits_topics_and_files() {
        let mut messages = Vec::new();
        for i in 0..10 {
            messages.push(Message::user(format!("request {}", i)));
        }
        for i in 0..25 {
            messages.push(Message::assistant_with_blocks(vec![
                MessageContentBlock::ToolUse {
                    id: format!("id{}", i),
                    name: "read_file".to_string(),
                    input: serde_json::json!({"path": format!("/file{}.rs", i)}),
                    cache_control: None,
                },
            ]));
        }

        let summary = ConversationHistory::build_drop_summary(&messages);
        // User topics capped at 5 (+ 1 in "User requests:" header)
        let topic_line = summary
            .lines()
            .find(|l| l.starts_with("User requests:"))
            .unwrap();
        let topic_count = topic_line.matches("request ").count();
        assert_eq!(topic_count, 5);
        // Files capped at 20
        let file_count = summary.matches("/file").count();
        assert_eq!(file_count, 20);
    }

    #[test]
    fn test_build_drop_summary_skips_assistant_text() {
        let messages = vec![
            Message::user("user question".to_string()),
            Message::assistant_with_blocks(vec![MessageContentBlock::Text {
                text: "assistant answer".to_string(),
                cache_control: None,
            }]),
        ];

        let summary = ConversationHistory::build_drop_summary(&messages);
        assert!(summary.contains("user question"));
        assert!(!summary.contains("assistant answer"));
    }

    #[test]
    fn test_fallback_trim_inserts_summary() {
        let mut history = ConversationHistory::new();
        history.config.max_context_tokens = 3000;

        // Add enough large messages to trigger token-based trimming
        for i in 0..30 {
            let content = format!("request {} {}", i, "x".repeat(500));
            history.messages.push(Message::user(content));
        }

        history.fallback_trim();

        // Should have trimmed and inserted a summary at position 0
        assert!(history.messages().len() < 30);
        if let crate::api::MessageContent::Text { content } = &history.messages()[0].content {
            assert!(content.starts_with("[Context trimmed"));
        } else {
            panic!("Expected text summary message at position 0");
        }
    }

    #[test]
    fn test_fallback_trim_no_summary_for_small_drop() {
        let mut history = ConversationHistory::new();
        // max_messages = 500 by default, so adding 502 drops only 2
        for i in 0..502 {
            history.messages.push(Message::user(format!("msg {}", i)));
        }

        history.fallback_trim();

        // Only 2 dropped — no summary should be inserted
        if let crate::api::MessageContent::Text { content } = &history.messages()[0].content {
            assert!(!content.starts_with("[Context trimmed"));
        }
    }

    #[test]
    fn test_fallback_trim_few_messages_no_panic() {
        let mut history = ConversationHistory::new();
        for i in 0..5 {
            history.messages.push(Message::user(format!("msg {}", i)));
        }
        history.fallback_trim();
        assert_eq!(history.messages().len(), 5);
    }

    fn blocks_msg_with(role: &str, n: usize) -> Message {
        let blocks: Vec<MessageContentBlock> = (0..n)
            .map(|i| MessageContentBlock::Text {
                text: format!("{}-{}", role, i),
                cache_control: None,
            })
            .collect();
        if role == "user" {
            Message::user_with_blocks(blocks)
        } else {
            Message::assistant_with_blocks(blocks)
        }
    }

    #[test]
    fn cache_anchor_stays_unset_when_history_under_10_blocks() {
        let mut history = ConversationHistory::new();
        // 1 user (1 block) + 1 assistant (3 blocks) + 1 user (3 blocks) = 7 blocks. Under 10.
        history.add_user_message("hi".to_string());
        history.add_assistant_with_blocks(vec![
            MessageContentBlock::Text {
                text: "ok".to_string(),
                cache_control: None,
            },
            MessageContentBlock::ToolUse {
                id: "1".to_string(),
                name: "read_file".to_string(),
                input: serde_json::json!({}),
                cache_control: None,
            },
            MessageContentBlock::ToolUse {
                id: "2".to_string(),
                name: "read_file".to_string(),
                input: serde_json::json!({}),
                cache_control: None,
            },
        ]);
        history.add_tool_results(vec![
            MessageContentBlock::ToolResult {
                tool_use_id: "1".to_string(),
                content: "ok".to_string(),
                cache_control: None,
            },
            MessageContentBlock::ToolResult {
                tool_use_id: "2".to_string(),
                content: "ok".to_string(),
                cache_control: None,
            },
        ]);
        assert!(
            history.cache_anchor_message_idx().is_none(),
            "anchor should stay unset under 10 blocks of history"
        );
    }

    #[test]
    fn cache_anchor_set_when_threshold_crossed_and_lands_on_blocks_message() {
        let mut history = ConversationHistory::new();
        // Build up >10 blocks with all-Blocks messages so the anchor lands cleanly.
        history.add_user_with_blocks(vec![MessageContentBlock::Text {
            text: "user-0".to_string(),
            cache_control: None,
        }]);
        for _ in 0..6 {
            history.add_assistant_with_blocks(vec![
                MessageContentBlock::Text {
                    text: "asst".to_string(),
                    cache_control: None,
                },
                MessageContentBlock::Text {
                    text: "more".to_string(),
                    cache_control: None,
                },
            ]);
            history.add_user_with_blocks(vec![MessageContentBlock::Text {
                text: "u".to_string(),
                cache_control: None,
            }]);
        }
        let idx = history
            .cache_anchor_message_idx()
            .expect("anchor should be set once history exceeds 10 blocks");
        assert!(
            idx < history.messages().len() - 1,
            "anchor must not be the rolling (last) message"
        );
        assert!(matches!(
            history.messages()[idx].content,
            crate::api::MessageContent::Blocks { .. }
        ));
    }

    #[test]
    fn cache_anchor_preserved_while_distance_within_18_blocks() {
        let mut history = ConversationHistory::new();
        // Push enough to trigger the anchor.
        for _ in 0..12 {
            history.messages.push(blocks_msg_with("user", 1));
        }
        history.maintain_cache_anchor();
        let first_anchor = history
            .cache_anchor_message_idx()
            .expect("anchor should be set");

        // Add 5 more 1-block messages — distance grows by 5, still <= 18.
        for _ in 0..5 {
            history.messages.push(blocks_msg_with("assistant", 1));
            history.maintain_cache_anchor();
        }
        assert_eq!(
            history.cache_anchor_message_idx(),
            Some(first_anchor),
            "anchor must stay put while distance to rolling stays within 18 blocks"
        );
    }

    #[test]
    fn cache_anchor_persists_through_a_wide_iteration() {
        // A single new message with many blocks doesn't push the anchor
        // forward because `block_distance` measures intermediate
        // messages only. The anchor's own cache lookup (at its earlier
        // block position) still hits across the wide turn — which is
        // the saving the anchor provides when the rolling lookup
        // cold-misses.
        let mut history = ConversationHistory::new();
        for _ in 0..12 {
            history.messages.push(blocks_msg_with("user", 1));
        }
        history.maintain_cache_anchor();
        let first_anchor = history.cache_anchor_message_idx().unwrap();

        history.messages.push(blocks_msg_with("assistant", 25));
        history.maintain_cache_anchor();

        assert_eq!(
            history.cache_anchor_message_idx(),
            Some(first_anchor),
            "anchor must stay put across a wide iteration"
        );
    }

    #[test]
    fn cache_anchor_advances_after_intermediate_growth() {
        // Many normal-width iterations push the cumulative
        // intermediate distance past 18 blocks; the anchor then
        // refreshes to ~10 blocks behind the new rolling.
        let mut history = ConversationHistory::new();
        for _ in 0..12 {
            history.messages.push(blocks_msg_with("user", 1));
        }
        history.maintain_cache_anchor();
        let first_anchor = history.cache_anchor_message_idx().unwrap();

        for _ in 0..20 {
            history.messages.push(blocks_msg_with("user", 1));
            history.maintain_cache_anchor();
        }

        let new_anchor = history.cache_anchor_message_idx().unwrap();
        assert_ne!(
            new_anchor, first_anchor,
            "anchor must advance once cumulative intermediate distance exceeds 18 blocks"
        );
        assert!(
            new_anchor < history.messages().len() - 1,
            "advanced anchor must not equal the rolling"
        );
    }

    #[test]
    fn cache_anchor_skips_text_variant_messages_when_picking() {
        let mut history = ConversationHistory::new();
        // First message is plain Text variant; cannot carry per-block cache_control.
        history
            .messages
            .push(Message::user("plain-text".to_string()));
        // Then a series of Blocks messages building up >10 blocks.
        for _ in 0..12 {
            history.messages.push(blocks_msg_with("user", 1));
        }
        history.maintain_cache_anchor();
        let idx = history.cache_anchor_message_idx().unwrap();
        assert!(matches!(
            history.messages()[idx].content,
            crate::api::MessageContent::Blocks { .. }
        ));
        assert_ne!(idx, 0, "must skip the Text-variant message at index 0");
    }

    #[test]
    fn cache_anchor_invalidated_on_clear() {
        let mut history = ConversationHistory::new();
        for _ in 0..12 {
            history.messages.push(blocks_msg_with("user", 1));
        }
        history.maintain_cache_anchor();
        assert!(history.cache_anchor_message_idx().is_some());
        history.clear();
        assert!(history.cache_anchor_message_idx().is_none());
    }

    #[test]
    fn cache_anchor_invalidated_when_trim_drops_front_messages() {
        // Aggressive max_messages so the next add forces trim to drop
        // most of the history, leaving fewer than 10 blocks — the
        // post-invalidation maintain leaves the anchor at None. Without
        // the invalidation fix, the anchor would persist at its
        // pre-trim index, silently pointing to shifted content.
        let mut history = ConversationHistory::new();
        history.config.max_messages = 5;
        for _ in 0..15 {
            history.messages.push(blocks_msg_with("user", 1));
        }
        history.maintain_cache_anchor();
        assert!(history.cache_anchor_message_idx().is_some());

        history.add_user_message("trigger trim".to_string());

        assert!(
            history.cache_anchor_message_idx().is_none(),
            "trim that shrinks history below 10 blocks must leave anchor None, \
             which only happens if invalidation ran before maintain"
        );
    }

    #[test]
    fn cache_anchor_invalidated_by_replace_with_summary() {
        let mut history = ConversationHistory::new();
        for _ in 0..12 {
            history.messages.push(blocks_msg_with("user", 1));
        }
        history.maintain_cache_anchor();
        assert!(history.cache_anchor_message_idx().is_some());

        history.replace_with_summary("done".to_string(), 8);

        // After summary replacement, only the first 5 messages remain
        // (12 - 8 = 4 + 1 summary). 5 messages × 1 block = 5 blocks,
        // under the 10-block threshold, so the anchor stays None.
        assert!(
            history.cache_anchor_message_idx().is_none(),
            "anchor must be invalidated; small remaining history stays unset"
        );
    }

    #[test]
    fn cache_anchor_invalidated_by_truncate_tool_results() {
        let mut history = ConversationHistory::new();
        for _ in 0..12 {
            history.messages.push(blocks_msg_with("user", 1));
        }
        history.maintain_cache_anchor();
        assert!(history.cache_anchor_message_idx().is_some());

        history.truncate_tool_results(10);

        assert!(
            history.cache_anchor_message_idx().is_none(),
            "in-place content mutation must invalidate the anchor"
        );
    }

    #[test]
    fn cache_anchor_invalidated_when_orphan_strip_mutates_front() {
        // Construct an orphaned [ToolResult, Text] at the front (no
        // preceding ToolUse). Limits high so trim doesn't drain — the
        // orphan strip is the SOLE mutation, and it leaves the Text
        // block intact, so `len` doesn't change. Without the
        // strip-tracking fix the anchor would persist with a stale
        // prefix hash; with the fix it's invalidated and re-established.
        let mut history = ConversationHistory::new();
        history.config.max_messages = 1000;
        history.config.max_context_tokens = 10_000_000;

        history.messages.push(Message::user_with_blocks(vec![
            MessageContentBlock::ToolResult {
                tool_use_id: "orphan".to_string(),
                content: "result".to_string(),
                cache_control: None,
            },
            MessageContentBlock::Text {
                text: "steer".to_string(),
                cache_control: None,
            },
        ]));
        for _ in 0..15 {
            history.messages.push(blocks_msg_with("user", 1));
        }

        history.maintain_cache_anchor();
        assert!(
            history.cache_anchor_message_idx().is_some(),
            "anchor should be set with 17+ blocks of history"
        );
        let len_before_call = history.messages.len();

        history.add_user_message("trigger".to_string());

        // No drain — the only mutation was the orphan strip on msg[0]
        // plus the new add. Net len change is exactly +1.
        assert_eq!(history.messages.len(), len_before_call + 1);
        // Strip ran: the ToolResult is gone, the Text survives.
        match &history.messages[0].content {
            crate::api::MessageContent::Blocks { content } => {
                assert_eq!(content.len(), 1, "only the Text block should survive");
                assert!(matches!(&content[0], MessageContentBlock::Text { .. }));
            }
            _ => panic!("msg[0] should remain Blocks variant after strip"),
        }
        // Strong invariant: the anchor reflects the post-strip state.
        // Without the strip-tracking fix, the anchor would silently
        // keep its pre-strip index (the validation in maintain only
        // checks idx-in-bounds + Blocks-variant, both still satisfied).
        // We catch that by clearing the anchor and re-running maintain
        // from scratch — if the post-strip code ran correctly, the
        // result must match.
        let final_anchor = history.cache_anchor_message_idx();
        history.cache_anchor_message_idx = None;
        history.maintain_cache_anchor();
        assert_eq!(
            history.cache_anchor_message_idx(),
            final_anchor,
            "anchor must be the one maintain produces on the post-strip state, \
             not a stale carry-over"
        );
    }

    #[test]
    fn cache_anchor_invalidated_by_restore_messages() {
        let mut history = ConversationHistory::new();
        for _ in 0..12 {
            history.messages.push(blocks_msg_with("user", 1));
        }
        history.maintain_cache_anchor();
        let prior_anchor = history.cache_anchor_message_idx();
        assert!(prior_anchor.is_some());

        // Restore to a totally new conversation with the same shape.
        let new_messages: Vec<Message> = (0..12).map(|_| blocks_msg_with("user", 1)).collect();
        history.restore_messages(new_messages);

        // The anchor must be re-established from current state, not
        // carried across from the prior conversation. After
        // re-establishment by trim_if_needed → maintain, it can be
        // Some at a freshly chosen position — but its identity
        // semantically belongs to the *new* messages.
        // The strong invariant we're enforcing: invalidation happened
        // before maintain ran. We can verify by replacing with a
        // smaller history that's under the 10-block threshold and
        // checking the anchor is None (would be Some(stale) without
        // the fix).
        let small: Vec<Message> = (0..3).map(|_| blocks_msg_with("user", 1)).collect();
        history.restore_messages(small);
        assert!(
            history.cache_anchor_message_idx().is_none(),
            "small restored history must leave anchor None"
        );
    }
}
