pub mod compaction;
pub mod lifecycle;
pub mod messages;
pub mod tokens;

use crate::api::{Message, SystemPrompt};
use crate::config::SofosConfig;

#[derive(Clone)]
pub struct ConversationHistory {
    pub(super) messages: Vec<Message>,
    pub(super) system_prompt: Vec<SystemPrompt>,
    pub(super) config: SofosConfig,
    /// Latches the floor-hit warning so it fires once per stuck-at-floor
    /// episode, not on every append.
    pub(super) warned_at_floor: bool,
    /// Index of the message whose last block carries the secondary
    /// Anthropic `cache_control` marker (the "anchor"). Stays put across
    /// turns whenever the rolling breakpoint stays within the 20-block
    /// lookback window; advances only when it would otherwise fall out
    /// of range. Without this, a single iteration adding more than ~20
    /// blocks (wide multi-tool turn) would cold-miss every cache entry.
    ///
    /// Invariant — the anchor's index points at content that is
    /// byte-stable in the prefix `messages[0..=anchor_idx]`. Any
    /// `&mut self` operation that
    ///   (a) inserts a message at or before the anchor,
    ///   (b) drops a leading message,
    ///   (c) mutates content inside `messages[0..=anchor_idx]`,
    /// must call [`Self::invalidate_cache_anchor`] *before* returning.
    /// Tail-only mutations (append a new message, edit
    /// `messages.last_mut()`, pop the rolling) are safe because the
    /// anchor sits strictly before the rolling by construction; the
    /// next [`Self::maintain_cache_anchor`] re-validates the index.
    pub(super) cache_anchor_message_idx: Option<usize>,
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
            // The system prompt is stable across the session, so a 1-hour
            // breakpoint here pays the 2x write premium once and avoids
            // re-billing the system + workspace text on every pause that
            // crosses the 5-minute default TTL.
            system_prompt: vec![SystemPrompt::new_cached_with_ttl(
                system_text.to_string(),
                Some("1h".to_string()),
            )],
            config: SofosConfig::default(),
            warned_at_floor: false,
            cache_anchor_message_idx: None,
        }
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
    fn test_trim_orphan_round_trips_through_both_provider_paths() {
        // Multi-call scenario: trim severs the first ToolUse/ToolResult
        // pair (call_lost); a later pair (call_keep) survives. The
        // serialised request must not reference call_lost on either
        // provider, and the surviving pair must round-trip intact.
        // OpenAI rejects orphan function_call_output with "No tool call
        // found for function call output with call_id …"; Anthropic
        // rejects unmatched tool_result.tool_use_id at the validator.
        use crate::api::CreateMessageRequest;
        use crate::api::anthropic::wire::sanitize_messages_for_anthropic;
        use crate::api::openai::build_response_input;
        use std::collections::HashSet;

        let mut history = ConversationHistory::new();
        history.config.max_messages = 4;

        let messages = vec![
            Message::user("initial query".to_string()),
            Message::assistant_with_blocks(vec![MessageContentBlock::ToolUse {
                id: "call_lost".to_string(),
                name: "read_file".to_string(),
                input: serde_json::json!({"path": "a.rs"}),
                cache_control: None,
            }]),
            Message::user_with_tool_results(vec![MessageContentBlock::ToolResult {
                tool_use_id: "call_lost".to_string(),
                content: "lost result".to_string(),
                cache_control: None,
            }]),
            Message::assistant_with_blocks(vec![MessageContentBlock::ToolUse {
                id: "call_keep".to_string(),
                name: "read_file".to_string(),
                input: serde_json::json!({"path": "b.rs"}),
                cache_control: None,
            }]),
            Message::user_with_tool_results(vec![MessageContentBlock::ToolResult {
                tool_use_id: "call_keep".to_string(),
                content: "kept result".to_string(),
                cache_control: None,
            }]),
            Message::user("next".to_string()),
        ];
        history.restore_messages(messages);

        // OpenAI Responses path: every function_call_output must
        // reference a prior function_call with the same call_id.
        let request = CreateMessageRequest {
            model: "gpt-5.5".to_string(),
            max_tokens: 100,
            messages: history.messages().to_vec(),
            system: None,
            tools: None,
            stream: None,
            thinking: None,
            output_config: None,
            reasoning: None,
            prompt_cache_key: None,
            context_management: None,
        };
        let openai_input = build_response_input(&request);
        let mut seen_call_ids: HashSet<String> = HashSet::new();
        let mut saw_kept_output = false;
        for item in &openai_input {
            match item.get("type").and_then(|v| v.as_str()) {
                Some("function_call") => {
                    let id = item
                        .get("call_id")
                        .and_then(|v| v.as_str())
                        .expect("function_call must carry call_id");
                    seen_call_ids.insert(id.to_string());
                }
                Some("function_call_output") => {
                    let id = item
                        .get("call_id")
                        .and_then(|v| v.as_str())
                        .expect("function_call_output must carry call_id");
                    assert!(
                        seen_call_ids.contains(id),
                        "OpenAI function_call_output references unknown call_id {} \
                         (would be rejected with 'No tool call found …')",
                        id
                    );
                    if id == "call_keep" {
                        saw_kept_output = true;
                    }
                }
                _ => {}
            }
        }
        assert!(
            saw_kept_output,
            "surviving function_call_output for call_keep was lost from OpenAI input"
        );

        // Anthropic Messages path: every tool_result.tool_use_id must
        // reference a tool_use.id from a prior assistant turn.
        let anthropic_msgs = sanitize_messages_for_anthropic(history.messages().to_vec());
        let mut seen_tool_use_ids: HashSet<String> = HashSet::new();
        let mut saw_kept_result = false;
        for msg in &anthropic_msgs {
            if let crate::api::MessageContent::Blocks { content } = &msg.content {
                for block in content {
                    match block {
                        MessageContentBlock::ToolUse { id, .. } => {
                            seen_tool_use_ids.insert(id.clone());
                        }
                        MessageContentBlock::ToolResult { tool_use_id, .. } => {
                            assert!(
                                seen_tool_use_ids.contains(tool_use_id),
                                "Anthropic tool_result references unknown tool_use_id {} \
                                 (rejected by tool_use_id validator)",
                                tool_use_id
                            );
                            if tool_use_id == "call_keep" {
                                saw_kept_result = true;
                            }
                        }
                        _ => {}
                    }
                }
            }
        }
        assert!(
            saw_kept_result,
            "surviving tool_result for call_keep was lost from Anthropic messages"
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
        // Auto-compact triggers at this token count regardless of
        // the API ceiling — picked by `ModelInfo::auto_compact_at`
        // at startup, set directly here for the test.
        history.config.auto_compact_token_limit = 80_000;

        // Should not need compaction with small messages
        history.add_user_message("hello".to_string());
        assert!(!history.needs_compaction());

        // Add enough messages to exceed 80k
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

    #[test]
    fn cache_anchor_preserved_when_appending_to_last_user_blocks() {
        // Pins the tail-only safety half of the field's invariant:
        // append_text_to_last_user_blocks edits messages.last_mut() —
        // the rolling — which by construction sits strictly after
        // the anchor, so the anchored prefix bytes don't change.
        let mut history = ConversationHistory::new();
        for _ in 0..12 {
            history.messages.push(blocks_msg_with("user", 1));
        }
        history.maintain_cache_anchor();
        let anchor_before = history
            .cache_anchor_message_idx()
            .expect("12 single-block messages cross the threshold");

        let appended = history.append_text_to_last_user_blocks("steer".to_string());
        assert!(appended, "tail is user-blocks, append should succeed");

        assert_eq!(
            history.cache_anchor_message_idx(),
            Some(anchor_before),
            "rolling-tail mutation must leave the anchor in place"
        );
    }

    #[test]
    fn cache_anchor_preserved_when_remove_last_message_pops_rolling() {
        // Pop the rolling and confirm the anchor stays put. We keep
        // the history wide enough that the new rolling is still
        // strictly after the anchor, so maintain doesn't clear it
        // on the idx < rolling_idx check.
        let mut history = ConversationHistory::new();
        for _ in 0..14 {
            history.messages.push(blocks_msg_with("user", 1));
        }
        history.maintain_cache_anchor();
        let anchor_before = history
            .cache_anchor_message_idx()
            .expect("14 single-block messages cross the threshold");
        assert!(
            anchor_before < history.messages.len() - 2,
            "test relies on anchor being < rolling - 1 so popping the \
             rolling doesn't push the anchor through the maintain bound"
        );

        history.remove_last_message();

        assert_eq!(
            history.cache_anchor_message_idx(),
            Some(anchor_before),
            "popping the rolling alone leaves the anchored prefix unchanged"
        );
    }
}
