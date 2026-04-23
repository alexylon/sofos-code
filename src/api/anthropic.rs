use super::types::*;
use super::utils;
use crate::error::{Result, SofosError};
use futures::stream::StreamExt;
use reqwest::header::{HeaderMap, HeaderValue};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

const API_BASE: &str = "https://api.anthropic.com/v1";
const API_VERSION: &str = "2023-06-01";
const ANTHROPIC_BETA: &str = "token-efficient-tools-2025-02-19";

/// Return true for models that *only* accept `thinking.type = "adaptive"`
/// (paired with `output_config.effort`) and reject the legacy
/// `{type: "enabled", budget_tokens: N}` shape with HTTP 400.
///
/// Currently Opus 4.7 is the sole member of this set; Sonnet/Opus 4.6 and
/// older continue to accept manual budgets, so we keep them on the old path
/// to preserve the user's `--thinking-budget` knob.
pub fn requires_adaptive_thinking(model: &str) -> bool {
    model.starts_with("claude-opus-4-7")
}

/// The string form of an "effort" level derived from the user's
/// thinking-on/off toggle. Used both for Anthropic's `output_config.effort`
/// (adaptive models) and OpenAI's `reasoning.effort` — the two APIs
/// happen to share the same `high` / `low` vocabulary, so one helper
/// keeps the request builder, TUI status line, startup banner, and
/// `/think` messages in sync without each site hand-mapping the bool.
pub fn effort_label(enable_thinking: bool) -> &'static str {
    if enable_thinking { "high" } else { "low" }
}

#[derive(Clone)]
pub struct AnthropicClient {
    client: reqwest::Client,
}

impl AnthropicClient {
    pub fn new(api_key: String) -> Result<Self> {
        let mut headers = HeaderMap::new();
        headers.insert(
            "x-api-key",
            HeaderValue::from_str(&api_key)
                .map_err(|e| SofosError::Config(format!("Invalid API key format: {}", e)))?,
        );
        headers.insert("anthropic-version", HeaderValue::from_static(API_VERSION));
        headers.insert("anthropic-beta", HeaderValue::from_static(ANTHROPIC_BETA));

        let client = utils::build_http_client(headers, utils::REQUEST_TIMEOUT)?;

        Ok(Self { client })
    }

    /// Check if we can reach the API endpoint
    pub async fn check_connectivity(&self) -> Result<()> {
        utils::check_api_connectivity(
            &self.client,
            API_BASE,
            "Anthropic",
            "https://status.anthropic.com",
        )
        .await
    }

    fn prepare_request(mut request: CreateMessageRequest) -> CreateMessageRequest {
        request.messages = sanitize_messages_for_anthropic(request.messages);

        if let Some(tools) = request.tools.take() {
            let filtered: Vec<Tool> = tools
                .into_iter()
                .filter(|t| !matches!(t, Tool::OpenAIWebSearch { tool_type: _ }))
                .collect();

            if !filtered.is_empty() {
                request.tools = Some(filtered);
            }
        }

        request
    }

    pub async fn create_anthropic_message(
        &self,
        request: CreateMessageRequest,
    ) -> Result<CreateMessageResponse> {
        let url = format!("{}/messages", API_BASE);
        let request = Self::prepare_request(request);

        let response = utils::send_once("Anthropic", self.client.post(&url).json(&request)).await?;

        let result = response.json::<CreateMessageResponse>().await?;
        Ok(result)
    }

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
        let mut request = Self::prepare_request(request);
        request.stream = Some(true);

        let url = format!("{}/messages", API_BASE);

        let response = utils::send_once("Anthropic", self.client.post(&url).json(&request)).await?;

        let mut byte_stream = response.bytes_stream();
        let mut buffer = String::new();

        let mut message_id = String::new();
        let mut model_name = String::new();
        let mut content_blocks: Vec<ContentBlock> = Vec::new();
        let mut input_tokens: u32 = 0;
        let mut output_tokens: u32 = 0;
        let mut stop_reason: Option<String> = None;

        let mut current_block_type: Option<String> = None;
        let mut current_text = String::new();
        let mut current_thinking = String::new();
        let mut current_signature = String::new();
        let mut current_tool_id = String::new();
        let mut current_tool_name = String::new();
        let mut current_tool_json = String::new();

        while let Some(chunk_result) = byte_stream.next().await {
            if interrupt_flag.load(Ordering::Relaxed) {
                return Err(SofosError::Interrupted);
            }

            let chunk = chunk_result
                .map_err(|e| SofosError::NetworkError(format!("Stream read error: {}", e)))?;
            buffer.push_str(&String::from_utf8_lossy(&chunk));

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
                    Err(_) => continue,
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
                                    u.get("input_tokens").and_then(|v| v.as_u64()).unwrap_or(0)
                                        as u32;
                            }
                        }
                    }
                    "content_block_start" => {
                        if let Some(block) = event.get("content_block") {
                            let btype = block.get("type").and_then(|t| t.as_str()).unwrap_or("");
                            current_block_type = Some(btype.to_string());
                            match btype {
                                "text" => current_text.clear(),
                                "thinking" => {
                                    current_thinking.clear();
                                    current_signature.clear();
                                }
                                "tool_use" | "server_tool_use" => {
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
                                }
                                "web_search_tool_result" => {
                                    if let Ok(result) =
                                        serde_json::from_value::<WebSearchToolResultBlock>(
                                            block.clone(),
                                        )
                                    {
                                        content_blocks.push(ContentBlock::WebSearchToolResult {
                                            tool_use_id: result.tool_use_id,
                                            content: result.content,
                                        });
                                    }
                                    current_block_type = None;
                                }
                                _ => {}
                            }
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
                                    if let Some(sig) =
                                        delta.get("signature").and_then(|v| v.as_str())
                                    {
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
                        match current_block_type.as_deref() {
                            Some("text") => {
                                content_blocks.push(ContentBlock::Text {
                                    text: current_text.clone(),
                                });
                            }
                            Some("thinking") => {
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
                            Some("tool_use") => {
                                let input = utils::parse_tool_arguments(
                                    &current_tool_name,
                                    &current_tool_json,
                                );
                                content_blocks.push(ContentBlock::ToolUse {
                                    id: current_tool_id.clone(),
                                    name: current_tool_name.clone(),
                                    input,
                                });
                            }
                            Some("server_tool_use") => {
                                let input = utils::parse_tool_arguments(
                                    &current_tool_name,
                                    &current_tool_json,
                                );
                                content_blocks.push(ContentBlock::ServerToolUse {
                                    id: current_tool_id.clone(),
                                    name: current_tool_name.clone(),
                                    input,
                                });
                            }
                            _ => {}
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
            input_tokens,
            output_tokens,
        ))
    }
}

#[derive(serde::Deserialize)]
struct WebSearchToolResultBlock {
    tool_use_id: String,
    #[serde(default)]
    content: Vec<WebSearchResult>,
}

fn sanitize_messages_for_anthropic(messages: Vec<Message>) -> Vec<Message> {
    messages
        .into_iter()
        .map(|mut msg| {
            if let MessageContent::Blocks { content } = msg.content {
                let filtered_content = content
                    .into_iter()
                    .filter_map(|block| match block {
                        MessageContentBlock::Summary { .. } => None,
                        other => Some(other),
                    })
                    .collect();

                msg.content = MessageContent::Blocks {
                    content: filtered_content,
                };
            }
            msg
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

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
    fn requires_adaptive_thinking_matches_opus_4_7_only() {
        assert!(requires_adaptive_thinking("claude-opus-4-7"));
        assert!(requires_adaptive_thinking("claude-opus-4-7-20260301"));
        assert!(!requires_adaptive_thinking("claude-opus-4-6"));
        assert!(!requires_adaptive_thinking("claude-sonnet-4-6"));
        assert!(!requires_adaptive_thinking("claude-opus-4-5"));
        assert!(!requires_adaptive_thinking(""));
    }

    #[test]
    fn effort_label_maps_bool_to_high_low() {
        assert_eq!(effort_label(true), "high");
        assert_eq!(effort_label(false), "low");
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
        };

        let json = serde_json::to_value(&request).unwrap();
        assert!(json["thinking"].is_object());
        assert_eq!(json["thinking"]["type"], "enabled");
        assert_eq!(json["thinking"]["budget_tokens"], 3000);
    }
}
