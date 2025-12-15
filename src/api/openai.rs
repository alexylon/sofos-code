use super::types::*;
use crate::error::{Result, SofosError};
use reqwest::header::{HeaderMap, HeaderValue, AUTHORIZATION, CONTENT_TYPE};
use serde::Deserialize;
use serde_json::json;
use std::time::Duration;

// OpenAI responses API (for gpt-5 models) and chat completions (for other chat models)
const OPENAI_API_BASE: &str = "https://api.openai.com/v1";
const REQUEST_TIMEOUT: Duration = super::anthropic::REQUEST_TIMEOUT;

#[derive(Clone)]
pub struct OpenAIClient {
    client: reqwest::Client,
}

impl OpenAIClient {
    pub fn new(api_key: String) -> Result<Self> {
        let mut headers = HeaderMap::new();
        headers.insert(
            AUTHORIZATION,
            HeaderValue::from_str(&format!("Bearer {}", api_key))
                .map_err(|e| SofosError::Config(format!("Invalid OpenAI API key: {}", e)))?,
        );
        headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));

        let client = reqwest::Client::builder()
            .default_headers(headers)
            .timeout(REQUEST_TIMEOUT)
            .build()
            .map_err(|e| SofosError::Config(format!("Failed to create HTTP client: {}", e)))?;

        Ok(Self { client })
    }

    pub async fn create_openai_message(
        &self,
        request: CreateMessageRequest,
    ) -> Result<CreateMessageResponse> {
        self.call_responses(request).await
    }

    async fn call_responses(&self, request: CreateMessageRequest) -> Result<CreateMessageResponse> {
        let inputs = build_response_input(&request);

        let mut body = json!({
            "model": request.model,
            "input": inputs,
            "max_output_tokens": request.max_tokens,
            "reasoning": request.reasoning,
        });

        if let Some(tool_list) = request.tools.clone() {
            let tools: Vec<serde_json::Value> = tool_list
                .into_iter()
                .filter_map(|tool| match tool {
                    Tool::Regular {
                        name,
                        description,
                        input_schema,
                        ..
                    } => Some(json!({
                        "type": "function",
                        "name": name,
                        "description": description,
                        "parameters": input_schema
                    })),
                    Tool::OpenAIWebSearch { tool_type } => Some(json!({"type": tool_type})),
                    _ => None,
                })
                .collect();

            if !tools.is_empty() {
                body["tools"] = json!(tools);
                body["tool_choice"] = json!("auto");

                if std::env::var("SOFOS_DEBUG").is_ok() {
                    eprintln!("\n=== OpenAI /responses Request ===");
                    eprintln!("Sending {} tools to OpenAI", tools.len());
                    for tool in &tools {
                        if let Some(func) = tool.get("function") {
                            if let Some(name) = func.get("name") {
                                eprintln!("  - Tool: {}", name);
                            }
                        }
                    }
                    eprintln!("==================================\n");
                }
            }
        }

        let url = format!("{}/responses", OPENAI_API_BASE);

        if std::env::var("SOFOS_DEBUG").is_ok() {
            eprintln!("\n=== OpenAI /responses Request Body ===");
            eprintln!(
                "{}",
                serde_json::to_string_pretty(&body)
                    .unwrap_or_else(|_| "Failed to serialize".to_string())
            );
            eprintln!("======================================\n");
        }

        let response = self.client.post(&url).json(&body).send().await?;
        let response = super::utils::check_response_status(response).await?;

        let response_text = response.text().await?;

        if std::env::var("SOFOS_DEBUG").is_ok() {
            eprintln!("\n=== OpenAI Raw Response ===");
            eprintln!("{}", response_text);
            eprintln!("===========================\n");
        }

        let response_parsed: OpenAIResponse = serde_json::from_str(&response_text)
            .map_err(|e| SofosError::Api(format!("Failed to parse OpenAI response: {}", e)))?;

        if std::env::var("SOFOS_DEBUG").is_ok() {
            eprintln!("\n=== OpenAI /responses API Response ===");
            eprintln!("Model: {}", response_parsed.model);
            eprintln!("Output items count: {}", response_parsed.output.len());
            for (i, item) in response_parsed.output.iter().enumerate() {
                eprintln!(
                    "  Item {}: type={}, content_count={}, tool_calls={:?}",
                    i,
                    item.item_type,
                    item.content.len(),
                    item.tool_calls.as_ref().map(|tc| tc.len())
                );
                for (j, content) in item.content.iter().enumerate() {
                    eprintln!(
                        "    Content {}: type={}, text_len={}",
                        j,
                        content.content_type,
                        content.text.len()
                    );
                }
                if let Some(ref tool_calls) = item.tool_calls {
                    for (j, call) in tool_calls.iter().enumerate() {
                        eprintln!(
                            "    Tool call {}: name={}, args_len={}",
                            j,
                            call.name,
                            call.arguments.len()
                        );
                    }
                }
            }
            eprintln!("======================================\n");
        }

        let mut content_blocks = Vec::new();
        for item in response_parsed.output {
            match item.item_type.as_str() {
                "message" => {
                    for content in item.content {
                        if content.content_type == "output_text" && !content.text.trim().is_empty()
                        {
                            content_blocks.push(ContentBlock::Text { text: content.text });
                        }
                    }

                    if let Some(tool_calls) = item.tool_calls {
                        for call in tool_calls {
                            let input = serde_json::from_str::<serde_json::Value>(&call.arguments)
                                .unwrap_or_else(|_| json!({"raw_arguments": call.arguments}));
                            content_blocks.push(ContentBlock::ToolUse {
                                id: call.id,
                                name: call.name,
                                input,
                            });
                        }
                    }
                }
                "function_call" => {
                    if let (Some(name), Some(arguments), Some(call_id)) =
                        (item.name, item.arguments, item.call_id)
                    {
                        let input = serde_json::from_str::<serde_json::Value>(&arguments)
                            .unwrap_or_else(|_| json!({"raw_arguments": arguments}));
                        content_blocks.push(ContentBlock::ToolUse {
                            id: call_id,
                            name,
                            input,
                        });
                    }
                }
                "reasoning" => {
                    for summary in item.summary {
                        if summary.summary_type == "summary_text" && !summary.text.trim().is_empty()
                        {
                            content_blocks.push(ContentBlock::Summary {
                                summary: summary.text,
                            });
                        }
                    }
                }
                _ => {
                    if std::env::var("SOFOS_DEBUG").is_ok() {
                        eprintln!("  Unknown item type: {}", item.item_type);
                    }
                }
            }
        }

        if std::env::var("SOFOS_DEBUG").is_ok() {
            eprintln!(
                "=== Converted to {} content blocks ===\n",
                content_blocks.len()
            );
        }

        let usage = response_parsed.usage.unwrap_or_default();

        Ok(CreateMessageResponse {
            _id: response_parsed.id,
            _response_type: "message".to_string(),
            _role: "assistant".to_string(),
            content: content_blocks,
            _model: response_parsed.model,
            stop_reason: None,
            usage: Usage {
                input_tokens: usage.input_tokens.unwrap_or(0),
                output_tokens: usage.output_tokens.unwrap_or(0),
            },
        })
    }
}

fn build_response_input(request: &CreateMessageRequest) -> Vec<serde_json::Value> {
    let mut input = Vec::new();

    if let Some(system_prompts) = &request.system {
        for system in system_prompts {
            input.push(json!({
                "role": "system",
                "content": [{
                    "type": "input_text",
                    "text": system.text
                }]
            }));
        }
    }

    for msg in &request.messages {
        let role = msg.role.as_str();
        let text_type = if role == "assistant" {
            "output_text"
        } else {
            "input_text"
        };

        let mut parts = Vec::new();

        match &msg.content {
            MessageContent::Text { content } => {
                parts.push(json!({"type": text_type, "text": content}));
            }
            MessageContent::Blocks { content } => {
                for block in content {
                    match block {
                        MessageContentBlock::Text { text, .. } => {
                            parts.push(json!({"type": text_type, "text": text}));
                        }
                        MessageContentBlock::Thinking { .. } => {
                            // Thinking blocks are Claude-only; skip for OpenAI
                        }
                        MessageContentBlock::Summary { summary, .. } => {
                            parts.push(json!({"type": "output_text", "text": summary}));
                        }
                        MessageContentBlock::ToolUse {
                            id,
                            name,
                            input: tool_input,
                            ..
                        } => {
                            // Responses API doesn't support tool_use in content; encode as text for context
                            let tool_args = serde_json::to_string(tool_input)
                                .unwrap_or_else(|_| "{}".to_string());
                            parts.push(json!({
                                "type": "output_text",
                                "text": format!("Tool call {} -> {} with args: {}", id, name, tool_args),
                            }));
                        }
                        MessageContentBlock::ToolResult {
                            tool_use_id,
                            content: tool_content,
                            ..
                        } => {
                            parts.push(json!({
                                "type": "input_text",
                                "text": format!("Tool result for {}:\n{}", tool_use_id, tool_content),
                            }));
                        }
                        MessageContentBlock::ServerToolUse {
                            name,
                            input: tool_input,
                            ..
                        } => {
                            let tool_args = serde_json::to_string(tool_input)
                                .unwrap_or_else(|_| "{}".to_string());
                            parts.push(json!({
                                "type": "output_text",
                                "text": format!("Server tool call {} with args: {}", name, tool_args),
                            }));
                        }
                        MessageContentBlock::WebSearchToolResult {
                            tool_use_id,
                            content,
                            ..
                        } => {
                            let search_summary = format!(
                                "Web search results for {} ({} items)",
                                tool_use_id,
                                content.len()
                            );
                            parts.push(json!({"type": "input_text", "text": search_summary}));
                        }
                    }
                }
            }
        }

        if !parts.is_empty() {
            input.push(json!({
                "role": msg.role,
                "content": parts,
            }));
        }
    }

    input
}

#[derive(Debug, Deserialize)]
struct OpenAIResponse {
    id: String,
    model: String,
    output: Vec<OpenAIOutputItem>,
    #[serde(default)]
    usage: Option<OpenAIResponseUsage>,
}

#[derive(Debug, Deserialize)]
struct OpenAIOutputItem {
    #[serde(rename = "type")]
    item_type: String,
    #[serde(default)]
    content: Vec<OpenAIOutputContent>,
    #[serde(default)]
    summary: Vec<OpenAIOutputSummary>,
    #[serde(default)]
    tool_calls: Option<Vec<OpenAIOutputToolCall>>,
    // Fields for when the item type is "function_call"
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    arguments: Option<String>,
    #[serde(default)]
    call_id: Option<String>,
}

#[derive(Debug, Deserialize)]
struct OpenAIOutputContent {
    #[serde(rename = "type")]
    content_type: String,
    #[serde(default)]
    text: String,
}

#[derive(Debug, Deserialize)]
struct OpenAIOutputSummary {
    #[serde(rename = "type")]
    summary_type: String,
    #[serde(default)]
    text: String,
}

#[derive(Debug, Deserialize)]
struct OpenAIOutputToolCall {
    id: String,
    name: String,
    arguments: String,
}

#[derive(Debug, Deserialize, Default)]
struct OpenAIResponseUsage {
    #[serde(default)]
    input_tokens: Option<u32>,
    #[serde(default)]
    output_tokens: Option<u32>,
}
