//! Anthropic Messages API client. The submodules carry the four
//! concerns:
//!
//! - [`client`] — HTTP client construction, connectivity check, and
//!   the non-streaming `create_message` path.
//! - [`wire`] — request transformation, per-model beta-header
//!   selection, and the public reasoning-effort helpers.
//! - [`stream`] — the SSE parser and the streaming `create_message`
//!   path; both feed `parse_stream` so the streaming and non-streaming
//!   call shapes match one-to-one.

pub mod client;
pub mod stream;
pub mod wire;

pub use client::AnthropicClient;
pub use wire::{
    COMPACTION_TRIGGER_FLOOR, LEGACY_THINKING_BUDGET_HIGH, effort_label, legacy_thinking_budget,
    requires_adaptive_thinking,
};

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::anthropic::stream::parse_stream;
    use crate::api::anthropic::wire::{
        BETA_COMPACT, BETA_TOKEN_EFFICIENT, BETA_TOKEN_EFFICIENT_AND_COMPACT,
        LEGACY_THINKING_BUDGET_LOW, LEGACY_THINKING_BUDGET_MEDIUM, anthropic_beta_for,
        prepare_request, sanitize_messages_for_anthropic,
    };
    use crate::api::types::*;
    use std::sync::Arc;
    use std::sync::atomic::AtomicBool;

    #[test]
    fn test_client_creation() {
        let client = AnthropicClient::new("test-key".to_string());
        assert!(client.is_ok());
    }

    #[test]
    fn test_thinking_serialization() {
        let thinking = Thinking::enabled(5120);
        assert_eq!(thinking.thinking_type, "enabled");
        assert_eq!(thinking.budget_tokens, Some(5120));

        let json = serde_json::to_value(&thinking).unwrap();
        assert_eq!(json["type"], "enabled");
        assert_eq!(json["budget_tokens"], 5120);
    }

    #[test]
    fn test_adaptive_thinking_serialization() {
        let thinking = Thinking::adaptive();
        let json = serde_json::to_value(&thinking).unwrap();
        assert_eq!(json["type"], "adaptive");
        // `budget_tokens` must be omitted for adaptive — Opus 4.7 rejects it.
        assert!(json.get("budget_tokens").is_none());
    }

    #[test]
    fn requires_adaptive_thinking_covers_supported_anthropic_models() {
        // Opus 4.7 requires adaptive (legacy shape 400s); Sonnet 4.6
        // accepts both shapes but Anthropic recommends adaptive, so
        // sofos opts it in too. Haiku 4.5 stays on the legacy
        // `budget_tokens` shape.
        assert!(requires_adaptive_thinking("claude-opus-4-7"));
        assert!(requires_adaptive_thinking("claude-sonnet-4-6"));
        assert!(!requires_adaptive_thinking("claude-haiku-4-5"));
    }

    #[test]
    fn anthropic_beta_for_gates_compaction_to_supported_models() {
        // Opus 4.7 is on the compaction-supported list — both betas ship.
        let with_compact = anthropic_beta_for("claude-opus-4-7");
        assert!(with_compact.contains(BETA_TOKEN_EFFICIENT));
        assert!(with_compact.contains(BETA_COMPACT));

        // Haiku 4.5 isn't — only the universal beta should appear so
        // we don't depend on Anthropic's "ignore unknown beta tokens"
        // policy if validation ever tightens.
        let without = anthropic_beta_for("claude-haiku-4-5");
        assert!(without.contains(BETA_TOKEN_EFFICIENT));
        assert!(!without.contains(BETA_COMPACT));
    }

    #[test]
    fn anthropic_beta_for_matches_model_info_predicate() {
        // The beta header and the request body's `context_management`
        // are gated off the same `Model::supports_server_compaction`
        // flag. An earlier version used a separate prefix list here
        // that could disagree with the per-model record; cross-check
        // the two sources of truth so any future drift surfaces here
        // instead of as a wire-format 400. Iterates the whitelist so
        // every supported model is covered automatically.
        let supported_models = crate::api::model_info::SUPPORTED_MODELS
            .iter()
            .map(|m| m.name);
        for model in supported_models.chain(std::iter::once("some-unknown-future-model")) {
            let expected = crate::api::model_info::lookup(model).supports_server_compaction;
            let header = anthropic_beta_for(model);
            assert_eq!(
                header.contains(BETA_COMPACT),
                expected,
                "{model}: beta header must agree with model info on compaction support"
            );
        }
    }

    #[test]
    fn beta_with_compact_matches_components() {
        // `BETA_TOKEN_EFFICIENT_AND_COMPACT` is a literal that must
        // stay in lockstep with its two component consts. Catch drift
        // here so renaming one component without the other is a test
        // failure rather than a silent header mismatch in production.
        assert_eq!(
            BETA_TOKEN_EFFICIENT_AND_COMPACT,
            format!("{BETA_TOKEN_EFFICIENT},{BETA_COMPACT}")
        );
    }

    #[test]
    fn legacy_thinking_budget_helper_scales_with_effort() {
        assert_eq!(
            legacy_thinking_budget(ReasoningEffort::Low),
            LEGACY_THINKING_BUDGET_LOW
        );
        assert_eq!(
            legacy_thinking_budget(ReasoningEffort::Medium),
            LEGACY_THINKING_BUDGET_MEDIUM
        );
        assert_eq!(
            legacy_thinking_budget(ReasoningEffort::High),
            LEGACY_THINKING_BUDGET_HIGH
        );
        // `XHigh` and `Max` are adaptive-only rungs; legacy models
        // clamp them to the highest budget they expose.
        assert_eq!(
            legacy_thinking_budget(ReasoningEffort::XHigh),
            LEGACY_THINKING_BUDGET_HIGH
        );
        assert_eq!(
            legacy_thinking_budget(ReasoningEffort::Max),
            LEGACY_THINKING_BUDGET_HIGH
        );
        // Defensive default: `Off` collapses to `LOW` rather than
        // panicking, even though the legacy branch is upstream-guarded.
        assert_eq!(
            legacy_thinking_budget(ReasoningEffort::Off),
            LEGACY_THINKING_BUDGET_LOW
        );
        // Compile-time guard: the three tier values must stay strictly
        // increasing. Runtime `assert!` would be a tautology on consts
        // (clippy::assertions_on_constants), so check at const-eval time.
        const _: () = {
            assert!(LEGACY_THINKING_BUDGET_LOW < LEGACY_THINKING_BUDGET_MEDIUM);
            assert!(LEGACY_THINKING_BUDGET_MEDIUM < LEGACY_THINKING_BUDGET_HIGH);
        };
    }

    #[test]
    fn effort_label_maps_reasoning_levels() {
        assert_eq!(effort_label(ReasoningEffort::Off), "low");
        assert_eq!(effort_label(ReasoningEffort::Low), "low");
        assert_eq!(effort_label(ReasoningEffort::Medium), "medium");
        assert_eq!(effort_label(ReasoningEffort::High), "high");
        assert_eq!(effort_label(ReasoningEffort::XHigh), "xhigh");
        assert_eq!(effort_label(ReasoningEffort::Max), "max");
    }

    #[test]
    fn adaptive_request_sends_output_config_and_omits_budget() {
        let request = CreateMessageRequest {
            model: "claude-opus-4-7".to_string(),
            max_tokens: 8192,
            messages: vec![],
            system: None,
            tools: None,
            stream: None,
            thinking: Some(Thinking::adaptive()),
            output_config: Some(OutputConfig::with_effort("high")),
            reasoning: None,
            prompt_cache_key: None,
            context_management: None,
        };

        let json = serde_json::to_value(&request).unwrap();
        assert_eq!(json["thinking"]["type"], "adaptive");
        assert!(json["thinking"].get("budget_tokens").is_none());
        assert_eq!(json["output_config"]["effort"], "high");
    }

    #[test]
    fn test_request_with_thinking() {
        let thinking = Some(Thinking::enabled(3000));
        let request = CreateMessageRequest {
            model: "claude-sonnet-4-6".to_string(),
            max_tokens: 8192,
            messages: vec![],
            system: None,
            tools: None,
            stream: None,
            thinking,
            output_config: None,
            reasoning: None,
            prompt_cache_key: None,
            context_management: None,
        };

        let json = serde_json::to_value(&request).unwrap();
        assert!(json["thinking"].is_object());
        assert_eq!(json["thinking"]["type"], "enabled");
        assert_eq!(json["thinking"]["budget_tokens"], 3000);
    }

    #[test]
    fn prepare_request_strips_prompt_cache_key() {
        let request = CreateMessageRequest {
            model: "claude-sonnet-4-6".to_string(),
            max_tokens: 8192,
            messages: vec![],
            system: None,
            tools: None,
            stream: None,
            thinking: None,
            output_config: None,
            reasoning: None,
            prompt_cache_key: Some("session-1".to_string()),
            context_management: None,
        };

        let prepared = prepare_request(request);
        assert!(prepared.prompt_cache_key.is_none());
    }

    #[test]
    fn sanitizer_drops_openai_reasoning_blocks_before_anthropic_call() {
        // Regression: a session that started on OpenAI accumulates
        // `Reasoning` blocks with `id` + `encrypted_content`. Switching
        // to Anthropic mid-session and forwarding those blocks would
        // 400 on a content-block-type the server doesn't know.
        let messages = vec![Message {
            role: "assistant".to_string(),
            content: MessageContent::Blocks {
                content: vec![
                    MessageContentBlock::Reasoning {
                        id: "rs_abc".to_string(),
                        summary: vec!["thought".to_string()],
                        encrypted_content: Some("blob".to_string()),
                        cache_control: None,
                    },
                    MessageContentBlock::Text {
                        text: "real reply".to_string(),
                        cache_control: None,
                    },
                ],
            },
        }];
        let cleaned = sanitize_messages_for_anthropic(messages);
        let MessageContent::Blocks { content } = &cleaned[0].content else {
            panic!("expected blocks");
        };
        assert_eq!(content.len(), 1, "Reasoning block must be dropped");
        assert!(matches!(content[0], MessageContentBlock::Text { .. }));
    }

    mod streaming {
        use super::*;
        use crate::api::utils::sse_test_support::sse_stream_from_events;
        use serde_json::json;
        use std::sync::Mutex;

        fn flag() -> Arc<AtomicBool> {
            Arc::new(AtomicBool::new(false))
        }

        #[tokio::test]
        async fn text_block_streams_through_callback_and_aggregates_in_response() {
            let events = vec![
                json!({
                    "type": "message_start",
                    "message": {
                        "id": "msg_test",
                        "model": "claude-sonnet-4-6",
                        "usage": {"input_tokens": 12, "cache_read_input_tokens": 3}
                    }
                }),
                json!({"type": "content_block_start", "index": 0, "content_block": {"type": "text", "text": ""}}),
                json!({"type": "content_block_delta", "index": 0, "delta": {"type": "text_delta", "text": "Hi "}}),
                json!({"type": "content_block_delta", "index": 0, "delta": {"type": "text_delta", "text": "there"}}),
                json!({"type": "content_block_stop", "index": 0}),
                json!({
                    "type": "message_delta",
                    "delta": {"stop_reason": "end_turn"},
                    "usage": {"output_tokens": 7}
                }),
                json!({"type": "message_stop"}),
            ];

            let text_chunks: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
            let t = text_chunks.clone();
            let stream = sse_stream_from_events(events);

            let response = parse_stream(
                stream,
                move |s| t.lock().unwrap().push(s.to_string()),
                |_| {},
                flag(),
            )
            .await
            .expect("parse_stream succeeds");

            assert_eq!(
                text_chunks.lock().unwrap().as_slice(),
                &["Hi ".to_string(), "there".to_string()]
            );
            assert_eq!(response.id, "msg_test");
            assert_eq!(response.stop_reason.as_deref(), Some("end_turn"));
            assert_eq!(response.usage.input_tokens, 12);
            assert_eq!(response.usage.output_tokens, 7);
            assert_eq!(response.usage.cache_read_input_tokens, Some(3));
            assert_eq!(response.content.len(), 1);
            assert!(matches!(
                &response.content[0],
                ContentBlock::Text { text } if text == "Hi there"
            ));
        }

        #[tokio::test]
        async fn thinking_block_with_signature_streams_through_thinking_callback() {
            // A `thinking` block must arrive with a `signature_delta`; the
            // parser drops thinking blocks without one because echoing
            // unsigned thinking back to the server 400s the next turn.
            let events = vec![
                json!({"type": "message_start", "message": {"id": "msg_t", "model": "claude-opus-4-7", "usage": {"input_tokens": 5}}}),
                json!({"type": "content_block_start", "index": 0, "content_block": {"type": "thinking"}}),
                json!({"type": "content_block_delta", "index": 0, "delta": {"type": "thinking_delta", "thinking": "let me think..."}}),
                json!({"type": "content_block_delta", "index": 0, "delta": {"type": "signature_delta", "signature": "abc123sig"}}),
                json!({"type": "content_block_stop", "index": 0}),
                json!({"type": "message_delta", "delta": {"stop_reason": "end_turn"}, "usage": {"output_tokens": 2}}),
            ];

            let think_chunks: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
            let th = think_chunks.clone();
            let stream = sse_stream_from_events(events);

            let response = parse_stream(
                stream,
                |_| {},
                move |s| th.lock().unwrap().push(s.to_string()),
                flag(),
            )
            .await
            .expect("parse_stream succeeds");

            assert_eq!(
                think_chunks.lock().unwrap().as_slice(),
                &["let me think...".to_string()]
            );
            assert_eq!(response.content.len(), 1);
            assert!(matches!(
                &response.content[0],
                ContentBlock::Thinking { thinking, signature }
                if thinking == "let me think..." && signature == "abc123sig"
            ));
        }

        #[tokio::test]
        async fn thinking_block_without_signature_is_dropped() {
            // Pins the invariant documented at `content_block_stop` /
            // `Some("thinking")`: an unsigned thinking block can't be
            // echoed back on the next turn, so the parser must not
            // include it in the response.
            let events = vec![
                json!({"type": "message_start", "message": {"id": "msg_t", "model": "claude-opus-4-7", "usage": {"input_tokens": 5}}}),
                json!({"type": "content_block_start", "index": 0, "content_block": {"type": "thinking"}}),
                json!({"type": "content_block_delta", "index": 0, "delta": {"type": "thinking_delta", "thinking": "unsigned"}}),
                json!({"type": "content_block_stop", "index": 0}),
                json!({"type": "message_delta", "delta": {"stop_reason": "end_turn"}, "usage": {"output_tokens": 1}}),
            ];

            let stream = sse_stream_from_events(events);
            let response = parse_stream(stream, |_| {}, |_| {}, flag())
                .await
                .expect("parse_stream succeeds");
            assert!(
                response.content.is_empty(),
                "unsigned thinking must be dropped, got {:?}",
                response.content
            );
        }

        #[tokio::test]
        async fn error_event_returns_api_error() {
            let events = vec![
                json!({"type": "message_start", "message": {"id": "msg_e", "model": "claude-sonnet-4-6", "usage": {"input_tokens": 1}}}),
                json!({"type": "error", "error": {"message": "overloaded"}}),
            ];

            let stream = sse_stream_from_events(events);
            let err = parse_stream(stream, |_| {}, |_| {}, flag())
                .await
                .expect_err("error event must surface as error");
            assert!(
                matches!(&err, crate::error::SofosError::Api(msg) if msg.contains("overloaded")),
                "got: {err:?}"
            );
        }

        #[tokio::test]
        async fn error_event_keeps_partial_reply_text() {
            // A mid-stream error must keep the reply text already
            // streamed to the screen so the next turn is not blind to it.
            let events = vec![
                json!({"type": "message_start", "message": {"id": "msg_e", "model": "claude-sonnet-4-6", "usage": {"input_tokens": 1}}}),
                json!({"type": "content_block_start", "index": 0, "content_block": {"type": "text", "text": ""}}),
                json!({"type": "content_block_delta", "index": 0, "delta": {"type": "text_delta", "text": "Here is the fix: "}}),
                json!({"type": "content_block_delta", "index": 0, "delta": {"type": "text_delta", "text": "rename the field"}}),
                json!({"type": "error", "error": {"message": "overloaded"}}),
            ];

            let stream = sse_stream_from_events(events);
            let err = parse_stream(stream, |_| {}, |_| {}, flag())
                .await
                .expect_err("error event must surface as error");
            let crate::error::SofosError::Api(msg) = &err else {
                panic!("expected SofosError::Api, got: {err:?}");
            };
            assert!(
                msg.contains("overloaded"),
                "keeps the provider message: {msg}"
            );
            assert!(
                msg.contains("Here is the fix: rename the field"),
                "must carry the partial reply text: {msg}"
            );
        }

        #[tokio::test]
        async fn multibyte_codepoint_split_across_chunks_is_preserved() {
            // Reproduces the pre-fix corruption: a UTF-8 codepoint
            // straddling two HTTP chunks used to be decoded chunk-by-
            // chunk through `from_utf8_lossy`, replacing the split
            // bytes with U+FFFD in both the streamed callback and the
            // aggregated response. Split a single SSE line — and a
            // single codepoint within it — across two chunks here.
            use futures::stream;
            let line = format!(
                "data: {}\n",
                serde_json::to_string(&json!({
                    "type": "content_block_delta",
                    "index": 0,
                    "delta": {"type": "text_delta", "text": "café"}
                }))
                .unwrap()
            );
            let bytes = line.into_bytes();
            // "café" is `caf` + 0xc3 0xa9. Find the 0xc3 0xa9 pair and
            // split between them so the codepoint genuinely spans two
            // chunks rather than landing on a clean boundary.
            let split_at = bytes
                .windows(2)
                .position(|w| w == [0xc3, 0xa9])
                .expect("UTF-8 encoding of café contains 0xc3 0xa9")
                + 1;
            let prefix = bytes[..split_at].to_vec();
            let suffix = bytes[split_at..].to_vec();

            let start = format!(
                "data: {}\n",
                serde_json::to_string(&json!({
                    "type": "message_start",
                    "message": {"id": "msg_mb", "model": "claude-sonnet-4-6", "usage": {"input_tokens": 1}}
                }))
                .unwrap()
            )
            .into_bytes();
            let block_start = format!(
                "data: {}\n",
                serde_json::to_string(&json!({
                    "type": "content_block_start", "index": 0,
                    "content_block": {"type": "text", "text": ""}
                }))
                .unwrap()
            )
            .into_bytes();
            let block_stop = format!(
                "data: {}\n",
                serde_json::to_string(&json!({"type": "content_block_stop", "index": 0})).unwrap()
            )
            .into_bytes();
            let message_delta = format!(
                "data: {}\n",
                serde_json::to_string(&json!({
                    "type": "message_delta",
                    "delta": {"stop_reason": "end_turn"},
                    "usage": {"output_tokens": 1}
                }))
                .unwrap()
            )
            .into_bytes();

            let chunks: Vec<crate::error::Result<Vec<u8>>> = vec![
                Ok(start),
                Ok(block_start),
                Ok(prefix),
                Ok(suffix),
                Ok(block_stop),
                Ok(message_delta),
            ];
            let byte_stream = stream::iter(chunks);

            let text_chunks: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
            let t = text_chunks.clone();
            let response = parse_stream(
                byte_stream,
                move |s| t.lock().unwrap().push(s.to_string()),
                |_| {},
                flag(),
            )
            .await
            .expect("parse_stream succeeds");

            let streamed = text_chunks.lock().unwrap().concat();
            assert_eq!(streamed, "café", "streamed text must not contain U+FFFD");
            assert!(
                matches!(&response.content[0], ContentBlock::Text { text } if text == "café"),
                "aggregated text must round-trip the multibyte codepoint"
            );
        }

        #[tokio::test]
        async fn oversized_input_tokens_saturate_at_u32_max() {
            // `Usage::input_tokens` is `u32`, but Anthropic ships the
            // count as a JSON number that parses through `as_u64`. A
            // pathologically large turn used to wrap around silently
            // (`u32` truncation on `as u32`); now it saturates at the
            // ceiling so the cost line is at worst over-reported, not
            // wrapped to a tiny number.
            let events = vec![
                json!({
                    "type": "message_start",
                    "message": {
                        "id": "msg_big",
                        "model": "claude-sonnet-4-6",
                        "usage": {"input_tokens": 9_999_999_999u64}
                    }
                }),
                json!({"type": "message_delta", "delta": {"stop_reason": "end_turn"}, "usage": {"output_tokens": 9_999_999_999u64}}),
            ];

            let stream = sse_stream_from_events(events);
            let response = parse_stream(stream, |_| {}, |_| {}, flag())
                .await
                .expect("parse_stream succeeds");
            assert_eq!(response.usage.input_tokens, u32::MAX);
            assert_eq!(response.usage.output_tokens, u32::MAX);
        }
    }
}
