//! SSE parser for the Anthropic streaming Messages API. Drives the
//! per-event state machine ([`StreamBlockKind`]) and reassembles the
//! final [`CreateMessageResponse`] from the streamed deltas so the
//! return value matches the non-streaming call shape one-to-one.

use crate::api::anthropic::client::AnthropicClient;
use crate::api::anthropic::wire::{BETA_HEADER_NAME, anthropic_beta_for, prepare_request};
use crate::api::types::*;
use crate::api::utils;
use crate::api::utils::MAX_SSE_BUFFER_BYTES;
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

/// Saturating `u64 -> u32` conversion used on token-count fields
/// arriving on the wire as `u64`. The `Usage` struct still stores
/// `u32`, so a pathological million-plus-token turn is reported as
/// `u32::MAX` rather than silently wrapping around to a small number.
fn saturate_u32(n: u64) -> u32 {
    u32::try_from(n).unwrap_or(u32::MAX)
}

/// Cap on streamed reply text folded into a mid-stream error, so the
/// error and the saved session stay bounded.
const MAX_PARTIAL_REPLY_BYTES: usize = 2000;

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
    // Hold raw bytes across chunks. Decoding each HTTP chunk eagerly
    // through `String::from_utf8_lossy` used to corrupt any multi-byte
    // codepoint that straddled a chunk boundary into a U+FFFD glyph,
    // both in the streamed callbacks and the aggregated response.
    // Buffering bytes and only decoding at SSE line boundaries keeps
    // codepoints intact.
    let mut buffer: Vec<u8> = Vec::new();

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
        buffer.extend_from_slice(chunk.as_ref());
        if buffer.len() > MAX_SSE_BUFFER_BYTES {
            return Err(SofosError::Api(format!(
                "Anthropic SSE buffer exceeded {} MB without a line terminator; \
                 likely a misbehaving server or middlebox",
                MAX_SSE_BUFFER_BYTES / (1024 * 1024)
            )));
        }

        while let Some(pos) = buffer.iter().position(|b| *b == b'\n') {
            // The complete line is in-buffer, so codepoints aren't
            // split across chunks. `from_utf8_lossy` still tolerates
            // genuinely malformed payloads without panicking.
            let line = String::from_utf8_lossy(&buffer[..pos]).into_owned();
            // Drop the line plus its trailing `\n` in place — this used
            // to reallocate the rest of the buffer on every iteration,
            // turning long SSE responses into an O(n^2) parser.
            buffer.drain(..=pos);

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
                            input_tokens = saturate_u32(
                                u.get("input_tokens").and_then(|v| v.as_u64()).unwrap_or(0),
                            );
                            cache_read_input_tokens = u
                                .get("cache_read_input_tokens")
                                .and_then(|v| v.as_u64())
                                .map(saturate_u32);
                            cache_creation_input_tokens = u
                                .get("cache_creation_input_tokens")
                                .and_then(|v| v.as_u64())
                                .map(saturate_u32);
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
                                match serde_json::from_value::<WebSearchToolResultBlock>(
                                    block.clone(),
                                ) {
                                    Ok(result) => {
                                        content_blocks.push(ContentBlock::WebSearchToolResult {
                                            tool_use_id: result.tool_use_id,
                                            content: result.content,
                                        });
                                    }
                                    Err(e) => {
                                        tracing::debug!(
                                            error = %e,
                                            "dropping malformed web_search_tool_result block"
                                        );
                                    }
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
                                // The non-streaming serde path would
                                // error on a missing `content`; in
                                // streaming we don't kill the whole
                                // response over one block, so skip and
                                // log instead. Losing the summary
                                // forces a re-compact next turn but
                                // doesn't 400 the request.
                                if let Some(content) = block.get("content").and_then(|v| v.as_str())
                                {
                                    content_blocks.push(ContentBlock::Compaction {
                                        content: content.to_string(),
                                    });
                                } else {
                                    tracing::debug!(
                                        "dropping compaction block with missing or non-string content"
                                    );
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
                        output_tokens = saturate_u32(
                            u.get("output_tokens").and_then(|v| v.as_u64()).unwrap_or(0),
                        );
                        // Server-side compaction can settle the cache-
                        // related usage numbers only on the trailing
                        // `message_delta`, not the opening
                        // `message_start`. Refresh both totals when
                        // present so the cost line picks up the cache-
                        // creation premium on turns where compaction
                        // landed late.
                        if let Some(read) =
                            u.get("cache_read_input_tokens").and_then(|v| v.as_u64())
                        {
                            cache_read_input_tokens = Some(saturate_u32(read));
                        }
                        if let Some(create) = u
                            .get("cache_creation_input_tokens")
                            .and_then(|v| v.as_u64())
                        {
                            cache_creation_input_tokens = Some(saturate_u32(create));
                        }
                    }
                }
                "error" => {
                    let error_msg = event
                        .get("error")
                        .and_then(|e| e.get("message"))
                        .and_then(|m| m.as_str())
                        .unwrap_or("Unknown streaming error");
                    // Returning here drops the accumulated content_blocks,
                    // so keep the streamed text in the error for the next turn.
                    let mut partial: String = content_blocks
                        .iter()
                        .filter_map(|b| match b {
                            ContentBlock::Text { text } => Some(text.as_str()),
                            _ => None,
                        })
                        .collect();
                    if current_block_type == Some(StreamBlockKind::Text) {
                        partial.push_str(&current_text);
                    }
                    let partial = partial.trim();
                    let context = if partial.is_empty() {
                        String::new()
                    } else {
                        let cutoff =
                            utils::truncate_at_char_boundary(partial, MAX_PARTIAL_REPLY_BYTES);
                        let ellipsis = if cutoff < partial.len() { "…" } else { "" };
                        format!(
                            " (partial reply before the error: {}{})",
                            &partial[..cutoff],
                            ellipsis
                        )
                    };
                    return Err(SofosError::Api(format!(
                        "Streaming error: {}{}",
                        error_msg, context
                    )));
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
