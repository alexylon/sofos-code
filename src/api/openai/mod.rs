//! OpenAI `/responses` API client. The submodules carry the four
//! concerns:
//!
//! - [`client`] — HTTP client construction, connectivity check, and
//!   the non-streaming `create_message` path.
//! - [`wire`] — request body shaping, the on-the-wire response shape,
//!   `reasoning_item_to_blocks`, and the `build_response` conversion
//!   into the shared `CreateMessageResponse`.
//! - [`stream`] — the SSE parser and the streaming `create_message`
//!   path; both feed `build_response` so the streaming and
//!   non-streaming call shapes match one-to-one.

pub mod client;
pub mod stream;
pub mod wire;

#[derive(Clone)]
pub struct OpenAIClient {
    pub(super) client: reqwest::Client,
}

#[cfg(test)]
mod tests {
    use crate::api::Message;
    use crate::api::openai::stream::parse_stream;
    use crate::api::openai::wire::{
        OpenAIResponseUsage, build_responses_body, reasoning_item_to_blocks,
    };
    use crate::api::types::*;

    fn req_with_cache_key(key: Option<&str>) -> CreateMessageRequest {
        CreateMessageRequest {
            model: "gpt-5.3".to_string(),
            max_tokens: 4096,
            messages: vec![Message::user("hi".to_string())],
            system: None,
            tools: None,
            stream: None,
            thinking: None,
            output_config: None,
            reasoning: None,
            prompt_cache_key: key.map(str::to_string),
            context_management: None,
        }
    }

    #[test]
    fn responses_body_includes_prompt_cache_key_when_set() {
        let body = build_responses_body(&req_with_cache_key(Some("session-xyz")));
        assert_eq!(body["prompt_cache_key"], "session-xyz");
    }

    #[test]
    fn responses_body_omits_prompt_cache_key_when_none() {
        let body = build_responses_body(&req_with_cache_key(None));
        assert!(body.get("prompt_cache_key").is_none());
    }

    #[test]
    fn responses_body_sets_include_when_reasoning_is_set() {
        let mut req = req_with_cache_key(None);
        req.reasoning = Some(crate::api::Reasoning::with_effort("medium"));
        let body = build_responses_body(&req);
        let include = body.get("include").and_then(|v| v.as_array()).cloned();
        assert_eq!(
            include,
            Some(vec![serde_json::json!("reasoning.encrypted_content")]),
            "reasoning round-trip requires include[reasoning.encrypted_content]"
        );
    }

    #[test]
    fn responses_body_omits_include_when_reasoning_is_none() {
        let body = build_responses_body(&req_with_cache_key(None));
        assert!(
            body.get("include").is_none(),
            "no reasoning means nothing to round-trip; sending include would be wasted bytes"
        );
    }

    #[test]
    fn reasoning_block_serializes_back_with_encrypted_content() {
        use crate::api::{CreateMessageRequest, Message, MessageContent, MessageContentBlock};
        let req = CreateMessageRequest {
            model: "gpt-5.5".to_string(),
            max_tokens: 4096,
            messages: vec![Message {
                role: "assistant".to_string(),
                content: MessageContent::Blocks {
                    content: vec![MessageContentBlock::Reasoning {
                        id: "rs_abc123".to_string(),
                        summary: vec!["Thought one.".to_string(), "Thought two.".to_string()],
                        encrypted_content: Some("OPAQUE_BLOB".to_string()),
                        cache_control: None,
                    }],
                },
            }],
            system: None,
            tools: None,
            stream: None,
            thinking: None,
            output_config: None,
            reasoning: None,
            prompt_cache_key: None,
            context_management: None,
        };
        let body = build_responses_body(&req);
        let inputs = body
            .get("input")
            .and_then(|v| v.as_array())
            .expect("input array");
        let reasoning_item = inputs
            .iter()
            .find(|item| item.get("type") == Some(&serde_json::json!("reasoning")))
            .expect("reasoning input item");
        assert_eq!(reasoning_item["id"], "rs_abc123");
        assert_eq!(reasoning_item["encrypted_content"], "OPAQUE_BLOB");
        let summary = reasoning_item["summary"].as_array().unwrap();
        assert_eq!(summary.len(), 2);
        assert_eq!(summary[0]["type"], "summary_text");
        assert_eq!(summary[0]["text"], "Thought one.");
        assert_eq!(summary[1]["text"], "Thought two.");
    }

    #[test]
    fn reasoning_serializes_before_its_assistant_message_text() {
        // Order matters: OpenAI's response chronology is
        // reasoning → text → tool_calls. Round-tripping reasoning
        // *after* its message would feed the server an out-of-order
        // input array. The assistant message in this test mixes a
        // Reasoning block followed by a Text block (recorded in
        // generation order); the wire output must preserve that.
        use crate::api::{CreateMessageRequest, Message, MessageContent, MessageContentBlock};
        let req = CreateMessageRequest {
            model: "gpt-5.5".to_string(),
            max_tokens: 4096,
            messages: vec![Message {
                role: "assistant".to_string(),
                content: MessageContent::Blocks {
                    content: vec![
                        MessageContentBlock::Reasoning {
                            id: "rs_abc".to_string(),
                            summary: vec!["thinking".to_string()],
                            encrypted_content: None,
                            cache_control: None,
                        },
                        MessageContentBlock::Text {
                            text: "and the answer is 42".to_string(),
                            cache_control: None,
                        },
                    ],
                },
            }],
            system: None,
            tools: None,
            stream: None,
            thinking: None,
            output_config: None,
            reasoning: None,
            prompt_cache_key: None,
            context_management: None,
        };
        let body = build_responses_body(&req);
        let inputs = body.get("input").and_then(|v| v.as_array()).unwrap();
        let reasoning_idx = inputs
            .iter()
            .position(|item| item.get("type") == Some(&serde_json::json!("reasoning")))
            .expect("reasoning input item");
        let message_idx = inputs
            .iter()
            .position(|item| item.get("role").is_some())
            .expect("assistant message item");
        assert!(
            reasoning_idx < message_idx,
            "reasoning must come before its assistant message (got reasoning@{}, message@{})",
            reasoning_idx,
            message_idx
        );
    }

    #[test]
    fn parses_cached_tokens_from_input_tokens_details() {
        let json = serde_json::json!({
            "input_tokens": 12000,
            "output_tokens": 300,
            "input_tokens_details": { "cached_tokens": 9500 }
        });
        let usage: OpenAIResponseUsage = serde_json::from_value(json).unwrap();
        assert_eq!(usage.input_tokens, Some(12000));
        assert_eq!(
            usage.input_tokens_details.and_then(|d| d.cached_tokens),
            Some(9500)
        );
    }

    #[test]
    fn parses_usage_without_cache_details() {
        let json = serde_json::json!({
            "input_tokens": 50,
            "output_tokens": 10
        });
        let usage: OpenAIResponseUsage = serde_json::from_value(json).unwrap();
        assert!(usage.input_tokens_details.is_none());
    }

    #[test]
    fn reasoning_item_drops_empty_shell_when_neither_summary_nor_encrypted() {
        // `{type: "reasoning", id, summary: []}` with no
        // encrypted_content carries no signal and some OpenAI models
        // reject the wire shape — drop instead of round-tripping.
        let blocks = reasoning_item_to_blocks(Some("rs_abc".to_string()), Vec::new(), None);
        assert!(
            blocks.is_empty(),
            "empty reasoning shell must be dropped, got {blocks:?}"
        );
    }

    #[test]
    fn reasoning_item_keeps_block_when_encrypted_content_present() {
        // Encrypted CoT alone is enough signal to round-trip — the
        // server uses it to resume hidden reasoning even with no
        // visible summary.
        let blocks = reasoning_item_to_blocks(
            Some("rs_abc".to_string()),
            Vec::new(),
            Some("encrypted_blob".to_string()),
        );
        assert_eq!(blocks.len(), 1);
        assert!(matches!(
            &blocks[0],
            ContentBlock::Reasoning {
                summary,
                encrypted_content: Some(_),
                ..
            } if summary.is_empty()
        ));
    }

    #[test]
    fn reasoning_item_keeps_block_when_summary_present() {
        let blocks = reasoning_item_to_blocks(
            Some("rs_abc".to_string()),
            vec!["thought".to_string()],
            None,
        );
        assert_eq!(blocks.len(), 1);
        assert!(matches!(
            &blocks[0],
            ContentBlock::Reasoning { summary, .. } if summary == &vec!["thought".to_string()]
        ));
    }

    #[test]
    fn reasoning_item_keeps_block_when_both_summary_and_encrypted_present() {
        // Common path — a reasoning model with `summary: "auto"` and
        // `include[reasoning.encrypted_content]` returns both. Both
        // must round-trip on the same block to preserve the link
        // between the visible summary and the hidden CoT.
        let blocks = reasoning_item_to_blocks(
            Some("rs_abc".to_string()),
            vec!["thought".to_string()],
            Some("encrypted_blob".to_string()),
        );
        assert_eq!(blocks.len(), 1);
        assert!(matches!(
            &blocks[0],
            ContentBlock::Reasoning {
                summary,
                encrypted_content: Some(_),
                ..
            } if summary == &vec!["thought".to_string()]
        ));
    }

    #[test]
    fn reasoning_item_without_id_falls_back_to_summary_blocks() {
        // Old payloads predating the `id` field — the visible
        // reasoning still surfaces but loses its round-trip handle.
        let blocks = reasoning_item_to_blocks(None, vec!["a".to_string(), "b".to_string()], None);
        assert_eq!(blocks.len(), 2);
        assert!(matches!(blocks[0], ContentBlock::Summary { .. }));
        assert!(matches!(blocks[1], ContentBlock::Summary { .. }));
    }

    mod streaming {
        use super::*;
        use crate::api::utils::sse_test_support::sse_stream_from_events;
        use crate::error::SofosError;
        use serde_json::json;
        use std::sync::Arc;
        use std::sync::Mutex;
        use std::sync::atomic::AtomicBool;

        fn flag() -> Arc<AtomicBool> {
            Arc::new(AtomicBool::new(false))
        }

        fn completed_response(text: &str) -> serde_json::Value {
            json!({
                "type": "response.completed",
                "response": {
                    "id": "resp_test",
                    "model": "gpt-5.5",
                    "status": "completed",
                    "output": [{
                        "type": "message",
                        "content": [{"type": "output_text", "text": text}]
                    }],
                    "usage": {"input_tokens": 5, "output_tokens": 3}
                }
            })
        }

        #[tokio::test]
        async fn text_deltas_stream_through_callback_and_final_response_builds() {
            let events = vec![
                json!({"type": "response.created"}),
                json!({"type": "response.output_text.delta", "delta": "Hello"}),
                json!({"type": "response.output_text.delta", "delta": ", world"}),
                completed_response("Hello, world"),
            ];

            let text_chunks: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
            let think_chunks: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
            let t = text_chunks.clone();
            let th = think_chunks.clone();

            let stream = sse_stream_from_events(events);
            let response = parse_stream(
                stream,
                move |s| t.lock().unwrap().push(s.to_string()),
                move |s| th.lock().unwrap().push(s.to_string()),
                flag(),
            )
            .await
            .expect("parse_stream succeeds");

            assert_eq!(
                text_chunks.lock().unwrap().as_slice(),
                &["Hello".to_string(), ", world".to_string()]
            );
            assert!(think_chunks.lock().unwrap().is_empty());
            assert_eq!(response.id, "resp_test");
            assert_eq!(response.content.len(), 1);
            assert!(matches!(
                &response.content[0],
                ContentBlock::Text { text } if text == "Hello, world"
            ));
        }

        #[tokio::test]
        async fn reasoning_deltas_route_to_thinking_callback() {
            let events = vec![
                json!({"type": "response.reasoning_summary_text.delta", "delta": "step 1"}),
                json!({"type": "response.reasoning_summary_text.delta", "delta": " then 2"}),
                completed_response("done"),
            ];

            let think_chunks: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
            let th = think_chunks.clone();
            let stream = sse_stream_from_events(events);

            parse_stream(
                stream,
                |_| {},
                move |s| th.lock().unwrap().push(s.to_string()),
                flag(),
            )
            .await
            .expect("parse_stream succeeds");

            assert_eq!(
                think_chunks.lock().unwrap().as_slice(),
                &["step 1".to_string(), " then 2".to_string()]
            );
        }

        #[tokio::test]
        async fn refusal_deltas_stream_through_text_callback() {
            // Refusal text is user-facing model output: it should reach
            // the user via the same callback as normal text so they see
            // the refusal stream in rather than appear all at once on
            // completion.
            let events = vec![
                json!({"type": "response.refusal.delta", "delta": "I can't "}),
                json!({"type": "response.refusal.delta", "delta": "help with that."}),
                completed_response("I can't help with that."),
            ];

            let text_chunks: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
            let t = text_chunks.clone();
            let stream = sse_stream_from_events(events);

            parse_stream(
                stream,
                move |s| t.lock().unwrap().push(s.to_string()),
                |_| {},
                flag(),
            )
            .await
            .expect("parse_stream succeeds");

            assert_eq!(
                text_chunks.lock().unwrap().as_slice(),
                &["I can't ".to_string(), "help with that.".to_string()]
            );
        }

        #[tokio::test]
        async fn response_failed_event_returns_api_error() {
            let events = vec![
                json!({"type": "response.output_text.delta", "delta": "partial"}),
                json!({
                    "type": "response.failed",
                    "response": {"error": {"message": "rate limit hit"}}
                }),
            ];

            let stream = sse_stream_from_events(events);
            let err = parse_stream(stream, |_| {}, |_| {}, flag())
                .await
                .expect_err("response.failed must surface as error");
            assert!(
                matches!(&err, SofosError::Api(msg) if msg.contains("rate limit hit")),
                "got: {err:?}"
            );
        }

        #[tokio::test]
        async fn stream_ending_without_completion_returns_api_error() {
            let events = vec![json!({"type": "response.output_text.delta", "delta": "incomplete"})];
            let stream = sse_stream_from_events(events);

            let err = parse_stream(stream, |_| {}, |_| {}, flag())
                .await
                .expect_err("missing completion event must surface");
            assert!(
                matches!(&err, SofosError::Api(msg) if msg.contains("response.completed")),
                "got: {err:?}"
            );
        }

        #[tokio::test]
        async fn nested_error_envelope_surfaces_through_api_error() {
            // OpenAI's `/responses` endpoint commonly emits the nested
            // `{type: "error", error: {message: "..."}}` shape. The
            // earlier parser only inspected `event.get("message")` and
            // dropped the nested message text into "Unknown streaming
            // error", losing the diagnostic.
            let events = vec![json!({
                "type": "error",
                "error": {"message": "context_length_exceeded"}
            })];

            let stream = sse_stream_from_events(events);
            let err = parse_stream(stream, |_| {}, |_| {}, flag())
                .await
                .expect_err("nested error event must surface as error");
            assert!(
                matches!(&err, SofosError::Api(msg) if msg.contains("context_length_exceeded")),
                "got: {err:?}"
            );
        }

        #[tokio::test]
        async fn flat_error_envelope_still_surfaces_through_api_error() {
            // Flat `{type: "error", message: "..."}` shape is also
            // tolerated so neither envelope arrives as "Unknown
            // streaming error".
            let events = vec![json!({"type": "error", "message": "rate_limit_exceeded"})];

            let stream = sse_stream_from_events(events);
            let err = parse_stream(stream, |_| {}, |_| {}, flag())
                .await
                .expect_err("flat error event must surface as error");
            assert!(
                matches!(&err, SofosError::Api(msg) if msg.contains("rate_limit_exceeded")),
                "got: {err:?}"
            );
        }

        #[tokio::test]
        async fn multibyte_codepoint_split_across_chunks_is_preserved() {
            // Reproduces the pre-fix corruption: a UTF-8 codepoint
            // straddling two HTTP chunks used to be decoded chunk-by-
            // chunk through `from_utf8_lossy`, replacing the split
            // bytes with U+FFFD. Split a single delta line across
            // two chunks within the codepoint itself.
            use futures::stream;
            let delta_line = format!(
                "data: {}\n",
                serde_json::to_string(&json!({
                    "type": "response.output_text.delta",
                    "delta": "café"
                }))
                .unwrap()
            );
            let bytes = delta_line.into_bytes();
            let split_at = bytes
                .windows(2)
                .position(|w| w == [0xc3, 0xa9])
                .expect("UTF-8 encoding of café contains 0xc3 0xa9")
                + 1;
            let prefix = bytes[..split_at].to_vec();
            let suffix = bytes[split_at..].to_vec();
            let completion = format!(
                "data: {}\n",
                serde_json::to_string(&completed_response("café")).unwrap()
            )
            .into_bytes();

            let chunks: Vec<crate::error::Result<Vec<u8>>> =
                vec![Ok(prefix), Ok(suffix), Ok(completion)];
            let byte_stream = stream::iter(chunks);

            let text_chunks: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
            let t = text_chunks.clone();
            parse_stream(
                byte_stream,
                move |s| t.lock().unwrap().push(s.to_string()),
                |_| {},
                flag(),
            )
            .await
            .expect("parse_stream succeeds");

            let streamed = text_chunks.lock().unwrap().concat();
            assert_eq!(streamed, "café", "streamed text must not contain U+FFFD");
        }

        #[tokio::test]
        async fn interrupt_set_mid_buffer_stops_before_the_next_line() {
            // The parser used to check the interrupt flag only between
            // HTTP chunks. A single chunk carrying many SSE lines
            // (notably the terminal `response.completed` chunk) would
            // process every line before noticing the flag. The new
            // inner check picks the flag up between lines, so an ESC
            // press during the burst aborts on the very next line.
            use futures::stream;
            let flag = Arc::new(AtomicBool::new(false));
            let f = flag.clone();
            let first = format!(
                "data: {}\n",
                serde_json::to_string(
                    &json!({"type": "response.output_text.delta", "delta": "one"})
                )
                .unwrap()
            );
            let second = format!(
                "data: {}\n",
                serde_json::to_string(
                    &json!({"type": "response.output_text.delta", "delta": "two"})
                )
                .unwrap()
            );
            // One chunk that carries both lines back-to-back.
            let combined = [first.as_bytes(), second.as_bytes()].concat();
            let chunks: Vec<crate::error::Result<Vec<u8>>> = vec![Ok(combined)];
            let byte_stream = stream::iter(chunks);

            let text_chunks: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
            let t = text_chunks.clone();
            let err = parse_stream(
                byte_stream,
                move |s| {
                    t.lock().unwrap().push(s.to_string());
                    // After the first delta lands, raise the flag so the
                    // inner-loop check fires before line two is parsed.
                    f.store(true, std::sync::atomic::Ordering::SeqCst);
                },
                |_| {},
                flag.clone(),
            )
            .await
            .expect_err("interrupt mid-buffer must abort");
            assert!(matches!(err, SofosError::Interrupted), "got: {err:?}");
            assert_eq!(
                text_chunks.lock().unwrap().as_slice(),
                &["one".to_string()],
                "the second delta must not reach the callback after the flag is set"
            );
        }

        #[tokio::test]
        async fn duplicate_tool_calls_across_legacy_and_top_level_shapes_are_deduped() {
            // Transitional and Azure-style backends sometimes emit
            // the same tool call in both the legacy `message.tool_calls`
            // shape and the current top-level `function_call` shape.
            // Without dedup the call would execute twice on the next
            // round-trip. Pin that the parser keeps only the first copy.
            let events = vec![json!({
                "type": "response.completed",
                "response": {
                    "id": "resp_dup",
                    "model": "gpt-5.5",
                    "status": "completed",
                    "output": [
                        {
                            "type": "message",
                            "content": [],
                            "tool_calls": [
                                {"id": "call_42", "name": "read_file", "arguments": "{\"path\":\"x\"}"}
                            ]
                        },
                        {
                            "type": "function_call",
                            "call_id": "call_42",
                            "name": "read_file",
                            "arguments": "{\"path\":\"x\"}"
                        }
                    ],
                    "usage": {"input_tokens": 5, "output_tokens": 3}
                }
            })];
            let stream = sse_stream_from_events(events);
            let response = parse_stream(stream, |_| {}, |_| {}, flag())
                .await
                .expect("parse_stream succeeds");
            let tool_uses: Vec<_> = response
                .content
                .iter()
                .filter(|b| matches!(b, ContentBlock::ToolUse { .. }))
                .collect();
            assert_eq!(
                tool_uses.len(),
                1,
                "duplicate tool calls with the same id must collapse to one"
            );
            assert!(matches!(
                &tool_uses[0],
                ContentBlock::ToolUse { id, .. } if id == "call_42"
            ));
        }

        #[tokio::test]
        async fn completed_status_carries_end_turn_stop_reason() {
            // OpenAI used to leave `stop_reason` as `None` on a normal
            // stop because the parser only mapped `status: "incomplete"`.
            // Anthropic always sets a `stop_reason`, so downstream code
            // that checks `if let Some(_) = stop_reason` would treat
            // OpenAI normal stops differently. Pin the convergence.
            let events = vec![completed_response("done")];
            let stream = sse_stream_from_events(events);
            let response = parse_stream(stream, |_| {}, |_| {}, flag())
                .await
                .expect("parse_stream succeeds");
            assert_eq!(response.stop_reason.as_deref(), Some("end_turn"));
        }

        #[tokio::test]
        async fn incomplete_status_maps_to_max_tokens_stop_reason() {
            // `response.incomplete` carries the same shape as
            // `response.completed`; the non-streaming `build_response`
            // path maps `status: "incomplete"` + reason `max_output_tokens`
            // onto `stop_reason: "max_tokens"`. Pin that the streaming
            // path inherits the mapping rather than reimplementing it.
            let events = vec![json!({
                "type": "response.incomplete",
                "response": {
                    "id": "resp_test",
                    "model": "gpt-5.5",
                    "status": "incomplete",
                    "incomplete_details": {"reason": "max_output_tokens"},
                    "output": [],
                    "usage": {"input_tokens": 100, "output_tokens": 32}
                }
            })];

            let stream = sse_stream_from_events(events);
            let response = parse_stream(stream, |_| {}, |_| {}, flag())
                .await
                .expect("parse_stream succeeds on response.incomplete");
            assert_eq!(response.stop_reason.as_deref(), Some("max_tokens"));
        }
    }
}
