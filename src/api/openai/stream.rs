//! SSE parser for the OpenAI `/responses` streaming endpoint. Routes
//! text and reasoning-summary deltas through their callbacks as they
//! arrive, captures the full response object from the terminal
//! `response.completed` / `response.incomplete` event, and hands it
//! to [`super::wire::build_response`] so the streaming and
//! non-streaming code paths produce identical [`CreateMessageResponse`]
//! values.

use crate::api::openai::OpenAIClient;
use crate::api::openai::client::OPENAI_API_BASE;
use crate::api::openai::wire::{OpenAIResponse, build_response, build_responses_body};
use crate::api::types::*;
use crate::api::utils;
use crate::error::{Result, SofosError};
use futures::stream::{Stream, StreamExt};
use serde_json::json;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

impl OpenAIClient {
    /// Streaming counterpart to [`OpenAIClient::create_message`]. Fires
    /// `on_text_delta` for each `response.output_text.delta` event and
    /// `on_thinking_delta` for each `response.reasoning_summary_text.delta`
    /// event. The final response is built from the full response object
    /// embedded in `response.completed` / `response.incomplete`, so the
    /// non-streaming and streaming paths converge on the same
    /// [`build_response`] code — no parallel item-accumulation logic to
    /// drift against the non-streaming serde path.
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
        let mut body = build_responses_body(&request);
        body["stream"] = json!(true);

        let url = format!("{}/responses", OPENAI_API_BASE);
        let response = utils::send_once("OpenAI", self.client.post(&url).json(&body)).await?;

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

/// Drive a pre-built SSE byte stream through the OpenAI parser. Split
/// out from [`OpenAIClient::create_message_streaming`] so tests can feed
/// hand-crafted fixtures without an HTTP layer; production callers
/// reach this only via `create_message_streaming`.
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
    let mut final_response: Option<OpenAIResponse> = None;

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
                        "failed to parse OpenAI streaming event"
                    );
                    continue;
                }
            };

            let event_type = event.get("type").and_then(|t| t.as_str()).unwrap_or("");

            match event_type {
                "response.output_text.delta" => {
                    if let Some(delta) = event.get("delta").and_then(|v| v.as_str()) {
                        on_text_delta(delta);
                    }
                }
                "response.reasoning_summary_text.delta" => {
                    if let Some(delta) = event.get("delta").and_then(|v| v.as_str()) {
                        on_thinking_delta(delta);
                    }
                }
                // Refusals are still user-facing model output — surface
                // them through the same callback as normal text so the
                // user sees a streaming refusal rather than a sudden
                // chunk on stream completion.
                "response.refusal.delta" => {
                    if let Some(delta) = event.get("delta").and_then(|v| v.as_str()) {
                        on_text_delta(delta);
                    }
                }
                // Both terminal-success events carry the same full
                // `response` object the non-streaming path receives;
                // routing them through `build_response` keeps
                // `status: "incomplete"` → `stop_reason: "max_tokens"`
                // mapping in one place.
                "response.completed" | "response.incomplete" => {
                    if let Some(resp) = event.get("response") {
                        match serde_json::from_value::<OpenAIResponse>(resp.clone()) {
                            Ok(parsed) => final_response = Some(parsed),
                            Err(e) => {
                                return Err(SofosError::Api(format!(
                                    "Failed to parse OpenAI streaming final response: {}",
                                    e
                                )));
                            }
                        }
                    }
                }
                "response.failed" => {
                    let error_msg = event
                        .get("response")
                        .and_then(|r| r.get("error"))
                        .and_then(|e| e.get("message"))
                        .and_then(|m| m.as_str())
                        .unwrap_or("Unknown streaming error");
                    return Err(SofosError::Api(format!("Streaming error: {}", error_msg)));
                }
                "error" => {
                    let error_msg = event
                        .get("message")
                        .and_then(|m| m.as_str())
                        .unwrap_or("Unknown streaming error");
                    return Err(SofosError::Api(format!("Streaming error: {}", error_msg)));
                }
                _ => {}
            }
        }
    }

    let parsed = final_response.ok_or_else(|| {
        SofosError::Api(
            "OpenAI stream ended without a response.completed/incomplete event".to_string(),
        )
    })?;

    build_response(parsed)
}
