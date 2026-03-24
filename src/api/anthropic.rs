use super::types::*;
use super::utils::{self, REQUEST_TIMEOUT};
use crate::error::{Result, SofosError};
use futures::stream::StreamExt;
use reqwest::header::{HeaderMap, HeaderValue, CONTENT_TYPE};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

const API_BASE: &str = "https://api.anthropic.com/v1";
const API_VERSION: &str = "2023-06-01";

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
        headers.insert(
            "anthropic-beta",
            HeaderValue::from_static("token-efficient-tools-2025-02-19"),
        );
        headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));

        let client = reqwest::Client::builder()
            .default_headers(headers)
            .timeout(REQUEST_TIMEOUT)
            .build()
            .map_err(|e| SofosError::Config(format!("Failed to create HTTP client: {}", e)))?;

        Ok(Self { client })
    }

    /// Check if we can reach the API endpoint
    pub async fn check_connectivity(&self) -> Result<()> {
        match tokio::time::timeout(Duration::from_secs(5), self.client.head(API_BASE).send()).await
        {
            Ok(Ok(_)) => Ok(()),
            Ok(Err(e)) => Err(SofosError::NetworkError(format!(
                "Cannot reach Anthropic API. Please check:\n  \
                 1. Your internet connection\n  \
                 2. Firewall/proxy settings\n  \
                 3. API status at https://status.anthropic.com\n\
                 Original error: {}",
                e
            ))),
            Err(_) => Err(SofosError::NetworkError(
                "Connection timeout. Please check your network connection.".into(),
            )),
        }
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

        let client = self.client.clone();
        let response = utils::with_retries("Anthropic", || {
            let client = client.clone();
            let url = url.clone();
            let request = request.clone();
            async move { client.post(&url).json(&request).send().await }
        })
        .await?;

        let response = utils::check_response_status(response).await?;
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

        let client = self.client.clone();
        let response = utils::with_retries("Anthropic", || {
            let client = client.clone();
            let url = url.clone();
            let request = request.clone();
            async move {
                client
                    .post(&url)
                    .json(&request)
                    .timeout(Duration::from_secs(600))
                    .send()
                    .await
            }
        })
        .await?;

        let response = utils::check_response_status(response).await?;

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
                    Some(s) if s == "[DONE]" => continue,
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
                                content_blocks.push(ContentBlock::Thinking {
                                    thinking: current_thinking.clone(),
                                    signature: current_signature.clone(),
                                });
                            }
                            Some("tool_use") => {
                                let input = serde_json::from_str(&current_tool_json)
                                    .unwrap_or(serde_json::Value::Object(serde_json::Map::new()));
                                content_blocks.push(ContentBlock::ToolUse {
                                    id: current_tool_id.clone(),
                                    name: current_tool_name.clone(),
                                    input,
                                });
                            }
                            Some("server_tool_use") => {
                                let input = serde_json::from_str(&current_tool_json)
                                    .unwrap_or(serde_json::Value::Object(serde_json::Map::new()));
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

        Ok(CreateMessageResponse {
            _id: message_id,
            _response_type: "message".to_string(),
            _role: "assistant".to_string(),
            content: content_blocks,
            _model: model_name,
            stop_reason,
            usage: Usage {
                input_tokens,
                output_tokens,
            },
        })
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
        assert_eq!(thinking.budget_tokens, 5120);

        let json = serde_json::to_value(&thinking).unwrap();
        assert_eq!(json["type"], "enabled");
        assert_eq!(json["budget_tokens"], 5120);
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
            reasoning: None,
        };

        let json = serde_json::to_value(&request).unwrap();
        assert!(json["thinking"].is_object());
        assert_eq!(json["thinking"]["type"], "enabled");
        assert_eq!(json["thinking"]["budget_tokens"], 3000);
    }
}
