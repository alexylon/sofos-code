use super::types::*;
use crate::error::{Result, SofosError};
use reqwest::header::{HeaderMap, HeaderValue, AUTHORIZATION, CONTENT_TYPE};
use serde::Deserialize;
use serde_json::json;
use std::time::Duration;

// OpenAI responses API (for gpt-5.1-codex) and chat completions (for other chat models)
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

    pub async fn create_message(
        &self,
        request: CreateMessageRequest,
    ) -> Result<CreateMessageResponse> {
        // gpt-5.1-codex uses the /responses endpoint with `input`
        if request.model.contains("gpt-5.1-codex") {
            return self.call_responses(request).await;
        }

        self.call_chat_completions(request).await
    }

    async fn call_chat_completions(
        &self,
        request: CreateMessageRequest,
    ) -> Result<CreateMessageResponse> {
        let mut messages = Vec::new();

        if let Some(system) = &request.system {
            messages.push(json!({"role": "system", "content": system}));
        }

        for msg in request.messages {
            match msg.role.as_str() {
                "user" => match msg.content {
                    MessageContent::Text { content } => {
                        messages.push(json!({"role": "user", "content": content}));
                    }
                    MessageContent::Blocks { content } => {
                        for block in content {
                            match block {
                                MessageContentBlock::Text { text } => {
                                    messages.push(json!({"role": "user", "content": text}));
                                }
                                MessageContentBlock::ToolResult {
                                    tool_use_id,
                                    content,
                                } => {
                                    messages.push(json!({
                                        "role": "tool",
                                        "tool_call_id": tool_use_id,
                                        "content": content,
                                    }));
                                }
                                _ => {}
                            }
                        }
                    }
                },
                "assistant" => match msg.content {
                    MessageContent::Text { content } => {
                        messages.push(json!({"role": "assistant", "content": content}));
                    }
                    MessageContent::Blocks { content } => {
                        let mut text_parts = Vec::new();
                        let mut tool_calls = Vec::new();

                        for block in content {
                            match block {
                                MessageContentBlock::Text { text } => text_parts.push(text),
                                MessageContentBlock::ToolUse { id, name, input } => {
                                    tool_calls.push(json!({
                                        "id": id,
                                        "type": "function",
                                        "function": {
                                            "name": name,
                                            "arguments": serde_json::to_string(&input).unwrap_or_else(|_| "{}".to_string())
                                        }
                                    }));
                                }
                                _ => {}
                            }
                        }

                        if !text_parts.is_empty() || !tool_calls.is_empty() {
                            let content = if text_parts.is_empty() {
                                serde_json::Value::Null
                            } else {
                                json!(text_parts.join("\n"))
                            };

                            let mut message = json!({"role": "assistant"});
                            if !content.is_null() {
                                message["content"] = content;
                            }
                            if !tool_calls.is_empty() {
                                message["tool_calls"] = json!(tool_calls);
                            }
                            messages.push(message);
                        }
                    }
                },
                _ => {}
            }
        }

        let tools: Vec<serde_json::Value> = request
            .tools
            .unwrap_or_default()
            .into_iter()
            .filter_map(|tool| match tool {
                Tool::Regular {
                    name,
                    description,
                    input_schema,
                } => Some(json!({
                    "type": "function",
                    "function": {
                        "name": name,
                        "description": description,
                        "parameters": input_schema
                    }
                })),
                _ => None, // Web search isn't supported via OpenAI tools
            })
            .collect();

        let mut body = json!({
            "model": request.model,
            "messages": messages,
            "max_tokens": request.max_tokens,
        });

        if !tools.is_empty() {
            body["tools"] = json!(tools);
            body["tool_choice"] = json!("auto");
        }

        let url = format!("{}/chat/completions", OPENAI_API_BASE);
        let response = self.client.post(&url).json(&body).send().await?;
        let response = check_response_status(response).await?;
        let parsed: OpenAIChatResponse = response.json().await?;

        let choice =
            parsed.choices.into_iter().next().ok_or_else(|| {
                SofosError::Api("OpenAI response contained no choices".to_string())
            })?;

        let mut content_blocks = Vec::new();

        if let Some(text) = choice.message.content {
            if !text.trim().is_empty() {
                content_blocks.push(ContentBlock::Text { text });
            }
        }

        if let Some(tool_calls) = choice.message.tool_calls {
            for call in tool_calls {
                let input = serde_json::from_str::<serde_json::Value>(&call.function.arguments)
                    .unwrap_or_else(|_| json!({"raw_arguments": call.function.arguments}));
                content_blocks.push(ContentBlock::ToolUse {
                    id: call.id,
                    name: call.function.name,
                    input,
                });
            }
        }

        let usage = parsed.usage.unwrap_or_default();

        Ok(CreateMessageResponse {
            _id: parsed.id,
            _response_type: "message".to_string(),
            _role: "assistant".to_string(),
            content: content_blocks,
            _model: parsed.model,
            stop_reason: choice.finish_reason.map(|r| {
                if r == "length" {
                    "max_tokens".to_string()
                } else {
                    r
                }
            }),
            usage: Usage {
                input_tokens: usage.prompt_tokens.unwrap_or(0),
                output_tokens: usage.completion_tokens.unwrap_or(0),
            },
        })
    }

    async fn call_responses(&self, request: CreateMessageRequest) -> Result<CreateMessageResponse> {
        let inputs = build_response_input(&request);

        let mut body = json!({
            "model": request.model,
            "input": inputs,
            "max_output_tokens": request.max_tokens,
        });

        if let Some(tool_list) = request.tools.clone() {
            let tools: Vec<serde_json::Value> = tool_list
                .into_iter()
                .filter_map(|tool| match tool {
                    Tool::Regular {
                        name,
                        description,
                        input_schema,
                    } => Some(json!({
                        "type": "function",
                        "name": name,
                        "description": description,
                        "parameters": input_schema
                    })),
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
            eprintln!("{}", serde_json::to_string_pretty(&body).unwrap_or_else(|_| "Failed to serialize".to_string()));
            eprintln!("======================================\n");
        }
        
        let response = self.client.post(&url).json(&body).send().await?;
        let response = check_response_status(response).await?;
        
        let response_text = response.text().await?;
        if std::env::var("SOFOS_DEBUG").is_ok() {
            eprintln!("\n=== OpenAI Raw Response ===");
            eprintln!("{}", response_text);
            eprintln!("===========================\n");
        }
        
        let parsed: OpenAIResponse = serde_json::from_str(&response_text)
            .map_err(|e| SofosError::Api(format!("Failed to parse OpenAI response: {}", e)))?;

        if std::env::var("SOFOS_DEBUG").is_ok() {
            eprintln!("\n=== OpenAI /responses API Response ===");
            eprintln!("Model: {}", parsed.model);
            eprintln!("Output items count: {}", parsed.output.len());
            for (i, item) in parsed.output.iter().enumerate() {
                eprintln!("  Item {}: type={}, content_count={}, tool_calls={:?}", 
                    i, 
                    item.item_type, 
                    item.content.len(),
                    item.tool_calls.as_ref().map(|tc| tc.len())
                );
                for (j, content) in item.content.iter().enumerate() {
                    eprintln!("    Content {}: type={}, text_len={}", 
                        j, 
                        content.content_type, 
                        content.text.len()
                    );
                }
                if let Some(ref tool_calls) = item.tool_calls {
                    for (j, call) in tool_calls.iter().enumerate() {
                        eprintln!("    Tool call {}: name={}, args_len={}", 
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
        for item in parsed.output {
            match item.item_type.as_str() {
                "message" => {
                    for content in item.content {
                        if content.content_type == "output_text" && !content.text.trim().is_empty() {
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
                        (item.name, item.arguments, item.call_id) {
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
                    if std::env::var("SOFOS_DEBUG").is_ok() {
                        eprintln!("  Skipping reasoning item");
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
            eprintln!("=== Converted to {} content blocks ===\n", content_blocks.len());
        }

        let usage = parsed.usage.unwrap_or_default();

        Ok(CreateMessageResponse {
            _id: parsed.id,
            _response_type: "message".to_string(),
            _role: "assistant".to_string(),
            content: content_blocks,
            _model: parsed.model,
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

    if let Some(system) = &request.system {
        input.push(json!({
            "role": "system",
            "content": system
        }));
    }

    for msg in &request.messages {
        match &msg.content {
            MessageContent::Text { content } => {
                input.push(json!({"role": msg.role, "content": content}));
            }
            MessageContent::Blocks { content } => {
                for block in content {
                    match block {
                        MessageContentBlock::Text { text } => {
                            input.push(json!({"role": msg.role, "content": text}));
                        }
                        MessageContentBlock::ToolUse { id, name, input: tool_input } => {
                            // Responses API requires function_call items to match with function_call_output
                            input.push(json!({
                                "type": "function_call",
                                "call_id": id,
                                "name": name,
                                "arguments": serde_json::to_string(tool_input).unwrap_or_else(|_| "{}".to_string())
                            }));
                        }
                        MessageContentBlock::ToolResult {
                            tool_use_id,
                            content,
                        } => {
                            input.push(json!({
                                "type": "function_call_output",
                                "call_id": tool_use_id,
                                "output": content
                            }));
                        }
                        _ => {}
                    }
                }
            }
        }
    }

    input
}

#[derive(Debug, Deserialize)]
struct OpenAIChatResponse {
    id: String,
    model: String,
    choices: Vec<OpenAIChatChoice>,
    #[serde(default)]
    usage: Option<OpenAIUsage>,
}

#[derive(Debug, Deserialize)]
struct OpenAIChatChoice {
    #[serde(default)]
    finish_reason: Option<String>,
    message: OpenAIChatMessage,
}

#[derive(Debug, Deserialize)]
struct OpenAIChatMessage {
    #[serde(default)]
    content: Option<String>,
    #[serde(default)]
    tool_calls: Option<Vec<OpenAIToolCall>>,
}

#[derive(Debug, Deserialize)]
struct OpenAIToolCall {
    id: String,
    function: OpenAIToolFunction,
}

#[derive(Debug, Deserialize)]
struct OpenAIToolFunction {
    name: String,
    arguments: String,
}

#[derive(Debug, Deserialize, Default)]
struct OpenAIUsage {
    #[serde(default)]
    prompt_tokens: Option<u32>,
    #[serde(default)]
    completion_tokens: Option<u32>,
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

async fn check_response_status(response: reqwest::Response) -> Result<reqwest::Response> {
    if !response.status().is_success() {
        let status = response.status();
        let error_text = response.text().await.unwrap_or_default();
        return Err(SofosError::Api(format!(
            "API request failed with status {}: {}",
            status, error_text
        )));
    }
    Ok(response)
}
