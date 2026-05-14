//! SSE parser for the Anthropic streaming Messages API. Drives the
//! per-event state machine ([`StreamBlockKind`]) and reassembles the
//! final [`CreateMessageResponse`] from the streamed deltas so the
//! return value matches the non-streaming call shape one-to-one.

use crate::api::anthropic::client::AnthropicClient;
use crate::api::anthropic::wire::{BETA_HEADER_NAME, anthropic_beta_for, prepare_request};
use crate::api::types::*;
use crate::api::utils;
use crate::error::{Result, SofosError};
use futures::stream::{Stream, StreamExt};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use crate::api::anthropic::client::ANTHROPIC_API_BASE;

/// Discriminant for the content block currently being assembled while
/// parsing an Anthropic streaming response. Replaces the earlier
/// `Option<String>` so the match in `content_block_stop` is exhaustive
/// and unknown wire-level block types stay as `None`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum StreamBlockKind {
    Text,
    Thinking,
    ToolUse,
    ServerToolUse,
}

#[derive(serde::Deserialize)]
struct WebSearchToolResultBlock {
    tool_use_id: String,
    #[serde(default)]
    content: Vec<WebSearchResult>,
}

impl AnthropicClient {
    pub async fn create_message_streaming<FText, FThink>(
        &self,
        request: CreateMessageRequest,
        on_text_delta: FText,
        on_thinking_delta: FThink,
        interrupt_flag: Arc<AtomicBool>,
    ) -> Result<CreateMessageResponse>
    where
        FText: Fn(&str) + Send + Sync,
        FThink: Fn(&str) + Send + Sync,
    {
        let mut request = prepare_request(request);
        request.stream = Some(true);
        let beta = anthropic_beta_for(&request.model);

        let url = format!("{}/messages", ANTHROPIC_API_BASE);

        let response = utils::send_once(
            "Anthropic",
            self.client
                .post(&url)
                .header(BETA_HEADER_NAME, beta)
                .json(&request),
        )
        .await?;

        let byte_stream = response.bytes_stream().map(|chunk_result| {
            chunk_result.map_err(|e| SofosError::NetworkError(format!("Stream read error: {}", e)))
        });
        parse_stream(
            byte_stream,
            on_text_delta,
            on_thinking_delta,
            interrupt_flag,
        )
        .await
    }
}

/// Drive a pre-built SSE byte stream through the Anthropic parser.
/// Split out from [`AnthropicClient::create_message_streaming`] so
/// tests can feed hand-crafted fixtures without an HTTP layer;
/// production callers reach this only via `create_message_streaming`.
pub(crate) async fn parse_stream<S, B, FText, FThink>(
    byte_stream: S,
    on_text_delta: FText,
    on_thinking_delta: FThink,
    interrupt_flag: Arc<AtomicBool>,
) -> Result<CreateMessageResponse>
where
    S: Stream<Item = Result<B>> + Unpin,
    B: AsRef<[u8]>,
    FText: Fn(&str) + Send + Sync,
    FThink: Fn(&str) + Send + Sync,
{
    let mut byte_stream = byte_stream;
    let mut buffer = String::new();

    let mut message_id = String::new();
    let mut model_name = String::new();
    let mut content_blocks: Vec<ContentBlock> = Vec::new();
    let mut input_tokens: u32 = 0;
    let mut output_tokens: u32 = 0;
    let mut cache_read_input_tokens: Option<u32> = None;
    let mut cache_creation_input_tokens: Option<u32> = None;
    let mut stop_reason: Option<String> = None;

    let mut current_block_type: Option<StreamBlockKind> = None;
    let mut current_text = String::new();
    let mut current_thinking = String::new();
    let mut current_signature = String::new();
    let mut current_tool_id = String::new();
    let mut current_tool_name = String::new();
    let mut current_tool_json = String::new();

    while let Some(chunk_result) = byte_stream.next().await {
        if interrupt_flag.load(Ordering::SeqCst) {
            return Err(SofosError::Interrupted);
        }

        let chunk = chunk_result?;
        buffer.push_str(&String::from_utf8_lossy(chunk.as_ref()));

        while let Some(pos) = buffer.find('\n') {
            let line = buffer[..pos].to_string();
            buffer = buffer[pos + 1..].to_string();

            let line = line.trim_end();
            let json_str = match line.strip_prefix("data: ") {
                Some("[DONE]") => continue,
                Some(s) => s,
                None => continue,
            };

            let event: serde_json::Value = match serde_json::from_str(json_str) {
                Ok(v) => v,
                Err(e) => {
                    tracing::debug!(
                        error = %e,
                        preview = %json_str.chars().take(200).collect::<String>(),
                        "failed to parse Anthropic streaming event"
                    );
                    continue;
                }
            };

            let event_type = event.get("type").and_then(|t| t.as_str()).unwrap_or("");

            match event_type {
                "message_start" => {
                    if let Some(msg) = event.get("message") {
                        message_id = msg
                            .get("id")
                            .and_then(|v| v.as_str())
                            .unwrap_or("")
                            .to_string();
                        model_name = msg
                            .get("model")
                            .and_then(|v| v.as_str())
                            .unwrap_or("")
                            .to_string();
                        if let Some(u) = msg.get("usage") {
                            input_tokens =
                                u.get("input_tokens").and_then(|v| v.as_u64()).unwrap_or(0) as u32;
                            cache_read_input_tokens = u
                                .get("cache_read_input_tokens")
                                .and_then(|v| v.as_u64())
                                .map(|n| n as u32);
                            cache_creation_input_tokens = u
                                .get("cache_creation_input_tokens")
                                .and_then(|v| v.as_u64())
                                .map(|n| n as u32);
                        }
                    }
                }
                "content_block_start" => {
                    if let Some(block) = event.get("content_block") {
                        let btype = block.get("type").and_then(|t| t.as_str()).unwrap_or("");
                        current_block_type = match btype {
                            "text" => {
                                current_text.clear();
                                Some(StreamBlockKind::Text)
                            }
                            "thinking" => {
                                current_thinking.clear();
                                current_signature.clear();
                                Some(StreamBlockKind::Thinking)
                            }
                            "tool_use" => {
                                current_tool_id = block
                                    .get("id")
                                    .and_then(|v| v.as_str())
                                    .unwrap_or("")
                                    .to_string();
                                current_tool_name = block
                                    .get("name")
                                    .and_then(|v| v.as_str())
                                    .unwrap_or("")
                                    .to_string();
                                current_tool_json.clear();
                                Some(StreamBlockKind::ToolUse)
                            }
                            "server_tool_use" => {
                                current_tool_id = block
                                    .get("id")
                                    .and_then(|v| v.as_str())
                                    .unwrap_or("")
                                    .to_string();
                                current_tool_name = block
                                    .get("name")
                                    .and_then(|v| v.as_str())
                                    .unwrap_or("")
                                    .to_string();
                                current_tool_json.clear();
                                Some(StreamBlockKind::ServerToolUse)
                            }
                            "web_search_tool_result" => {
                                if let Ok(result) = serde_json::from_value::<WebSearchToolResultBlock>(
                                    block.clone(),
                                ) {
                                    content_blocks.push(ContentBlock::WebSearchToolResult {
                                        tool_use_id: result.tool_use_id,
                                        content: result.content,
                                    });
                                }
                                None
                            }
                            // Server-side compaction
                            // (`compact-2026-01-12` beta) emits a
                            // `compaction` content block with the
                            // full summary in `content`. Mirror
                            // the non-streaming serde path so the
                            // next turn can round-trip the summary
                            // and Anthropic doesn't re-compact.
                            "compaction" => {
                                // Mirror the existing "drop malformed
                                // payloads silently" pattern used by
                                // `web_search_tool_result` above. The
                                // non-streaming serde path would error
                                // on a missing `content`; in streaming
                                // we don't want to kill the whole
                                // response over one block, so skip it
                                // — losing the summary forces a
                                // re-compact next turn but doesn't
                                // 400 the request.
                                if let Some(content) = block.get("content").and_then(|v| v.as_str())
                                {
                                    content_blocks.push(ContentBlock::Compaction {
                                        content: content.to_string(),
                                    });
                                }
                                None
                            }
                            _ => None,
                        };
                    }
                }
                "content_block_delta" => {
                    if let Some(delta) = event.get("delta") {
                        let dtype = delta.get("type").and_then(|t| t.as_str()).unwrap_or("");
                        match dtype {
                            "text_delta" => {
                                if let Some(text) = delta.get("text").and_then(|v| v.as_str()) {
                                    current_text.push_str(text);
                                    on_text_delta(text);
                                }
                            }
                            "thinking_delta" => {
                                if let Some(thinking) =
                                    delta.get("thinking").and_then(|v| v.as_str())
                                {
                                    current_thinking.push_str(thinking);
                                    on_thinking_delta(thinking);
                                }
                            }
                            "signature_delta" => {
                                if let Some(sig) = delta.get("signature").and_then(|v| v.as_str()) {
                                    current_signature.push_str(sig);
                                }
                            }
                            "input_json_delta" => {
                                if let Some(json_part) =
                                    delta.get("partial_json").and_then(|v| v.as_str())
                                {
                                    current_tool_json.push_str(json_part);
                                }
                            }
                            _ => {}
                        }
                    }
                }
                "content_block_stop" => {
                    match current_block_type {
                        Some(StreamBlockKind::Text) => {
                            content_blocks.push(ContentBlock::Text {
                                text: current_text.clone(),
                            });
                        }
                        Some(StreamBlockKind::Thinking) => {
                            // Every legitimate thinking block the server emits
                            // is paired with a signature. An empty signature
                            // means no `signature_delta` ever arrived for this
                            // block — echoing it back on the next turn would
                            // fail server-side verification and 400 the whole
                            // request. Drop the block; an empty-thinking
                            // adaptive block *with* a real signature (Opus 4.7
                            // `display: "omitted"`) is still preserved.
                            if !current_signature.is_empty() {
                                content_blocks.push(ContentBlock::Thinking {
                                    thinking: current_thinking.clone(),
                                    signature: current_signature.clone(),
                                });
                            }
                        }
                        Some(StreamBlockKind::ToolUse) => {
                            let input =
                                utils::parse_tool_arguments(&current_tool_name, &current_tool_json);
                            content_blocks.push(ContentBlock::ToolUse {
                                id: current_tool_id.clone(),
                                name: current_tool_name.clone(),
                                input,
                            });
                        }
                        Some(StreamBlockKind::ServerToolUse) => {
                            let input =
                                utils::parse_tool_arguments(&current_tool_name, &current_tool_json);
                            content_blocks.push(ContentBlock::ServerToolUse {
                                id: current_tool_id.clone(),
                                name: current_tool_name.clone(),
                                input,
                            });
                        }
                        None => {}
                    }
                    current_block_type = None;
                }
                "message_delta" => {
                    if let Some(delta) = event.get("delta") {
                        stop_reason = delta
                            .get("stop_reason")
                            .and_then(|v| v.as_str())
                            .map(String::from);
                    }
                    if let Some(u) = event.get("usage") {
                        output_tokens =
                            u.get("output_tokens").and_then(|v| v.as_u64()).unwrap_or(0) as u32;
                    }
                }
                "error" => {
                    let error_msg = event
                        .get("error")
                        .and_then(|e| e.get("message"))
                        .and_then(|m| m.as_str())
                        .unwrap_or("Unknown streaming error");
                    return Err(SofosError::Api(format!("Streaming error: {}", error_msg)));
                }
                _ => {}
            }
        }
    }

    Ok(utils::build_message_response(
        message_id,
        model_name,
        content_blocks,
        stop_reason,
        Usage {
            input_tokens,
            output_tokens,
            cache_read_input_tokens,
            cache_creation_input_tokens,
        },
    ))
}
