use super::types::*;
use super::utils::{self, REQUEST_TIMEOUT};
use crate::error::{Result, SofosError};
use crate::tools::tool_name::ToolName;
use reqwest::header::{AUTHORIZATION, CONTENT_TYPE, HeaderMap, HeaderValue};
use serde::Deserialize;
use serde_json::json;

const OPENAI_API_BASE: &str = "https://api.openai.com/v1";

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

    pub async fn check_connectivity(&self) -> Result<()> {
        utils::check_api_connectivity(
            &self.client,
            OPENAI_API_BASE,
            "OpenAI",
            "https://status.openai.com",
        )
        .await
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

        let client = self.client.clone();
        let response = utils::with_retries("OpenAI", || {
            let client = client.clone();
            let url = url.clone();
            let body = body.clone();
            async move { client.post(&url).json(&body).send().await }
        })
        .await?;

        let response = utils::check_response_status(response).await?;
        let response_text = response.text().await?;

        if std::env::var("SOFOS_DEBUG").is_ok() {
            eprintln!("\n=== OpenAI Raw Response ===");
            eprintln!("{}", response_text);
            eprintln!("===========================\n");
        }

        let response_parsed: OpenAIResponse = serde_json::from_str(&response_text)
            .map_err(|e| SofosError::Api(format!("Failed to parse OpenAI response: {}", e)))?;

        self.build_response(response_parsed)
    }

    fn build_response(&self, response_parsed: OpenAIResponse) -> Result<CreateMessageResponse> {
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
                            let input = parse_openai_tool_arguments(&call.name, &call.arguments);
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
                        let input = parse_openai_tool_arguments(&name, &arguments);
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

/// Parse tool-call arguments emitted by OpenAI into a JSON value.
///
/// `morph_edit_file` uses strict parsing because its `code_edit` field carries
/// arbitrary source code; the repair heuristics below can "succeed" on a
/// truncated payload and silently merge corrupted code into a file. For every
/// other tool we attempt a small repair ladder (trim, drop trailing commas,
/// close a missing brace) and fall back to `{"raw_arguments": args}` so the
/// model can see its own malformed output and self-correct on the next turn —
/// rather than receiving an empty `{}` that round-trips back to OpenAI as a
/// `function_call` with no args.
fn parse_openai_tool_arguments(name: &str, args: &str) -> serde_json::Value {
    if name == ToolName::MorphEditFile.as_str() {
        return serde_json::from_str(args)
            .unwrap_or_else(|_| serde_json::Value::Object(serde_json::Map::new()));
    }

    if let Ok(v) = serde_json::from_str::<serde_json::Value>(args) {
        return v;
    }

    let trimmed = args.trim();
    if trimmed != args {
        if let Ok(v) = serde_json::from_str::<serde_json::Value>(trimmed) {
            return v;
        }
    }

    let fixed = trimmed.replace(",}", "}").replace(",]", "]");
    if fixed != trimmed {
        if let Ok(v) = serde_json::from_str::<serde_json::Value>(&fixed) {
            return v;
        }
    }

    if trimmed.starts_with('{') && !trimmed.ends_with('}') {
        let braced = format!("{}}}", trimmed.trim_end_matches(','));
        if let Ok(v) = serde_json::from_str::<serde_json::Value>(&braced) {
            return v;
        }
    }

    let preview_end = utils::truncate_at_char_boundary(args, 200);
    eprintln!(
        "  \x1b[33m⚠\x1b[0m Failed to parse tool arguments as JSON for {}: {}",
        name,
        &args[..preview_end]
    );
    json!({"raw_arguments": args})
}

fn build_response_input(request: &CreateMessageRequest) -> Vec<serde_json::Value> {
    let mut input = Vec::new();

    let text_part = |part_type: &str, text: &str| -> serde_json::Value {
        json!({
            "type": part_type,
            "text": text,
        })
    };

    if let Some(system_prompts) = &request.system {
        for system in system_prompts {
            let content = text_part("input_text", &system.text);
            input.push(json!({"role": "system", "content": [content]}));
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
        // Collect function_call and function_call_output items to emit after the message
        let mut deferred_items: Vec<serde_json::Value> = Vec::new();

        match &msg.content {
            MessageContent::Text { content } => {
                parts.push(text_part(text_type, content));
            }
            MessageContent::Blocks { content } => {
                for block in content {
                    match block {
                        MessageContentBlock::Text { text, .. } => {
                            parts.push(text_part(text_type, text));
                        }
                        MessageContentBlock::Thinking { .. } => {
                            // Thinking blocks are Claude-only; skip for OpenAI
                        }
                        MessageContentBlock::Summary { summary, .. } => {
                            parts.push(text_part("output_text", summary));
                        }
                        MessageContentBlock::ToolUse {
                            id,
                            name,
                            input: tool_input,
                            ..
                        } => {
                            // Use native function_call items so the model doesn't
                            // learn to emit tool-call syntax as plain text.
                            let tool_args = serde_json::to_string(tool_input)
                                .unwrap_or_else(|_| "{}".to_string());
                            deferred_items.push(json!({
                                "type": "function_call",
                                "name": name,
                                "arguments": tool_args,
                                "call_id": id,
                            }));
                        }
                        MessageContentBlock::ToolResult {
                            tool_use_id,
                            content: tool_content,
                            ..
                        } => {
                            deferred_items.push(json!({
                                "type": "function_call_output",
                                "call_id": tool_use_id,
                                "output": tool_content,
                            }));
                        }
                        MessageContentBlock::ServerToolUse {
                            name,
                            input: tool_input,
                            ..
                        } => {
                            let tool_args = serde_json::to_string(tool_input)
                                .unwrap_or_else(|_| "{}".to_string());
                            parts.push(text_part(
                                "output_text",
                                &format!("Server tool call {} with args: {}", name, tool_args),
                            ));
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
                            parts.push(text_part("input_text", &search_summary));
                        }
                        MessageContentBlock::Image { source, .. } => match source {
                            ImageSource::Url { url } => {
                                parts.push(json!({
                                    "type": "input_image",
                                    "image_url": url
                                }));
                            }
                            ImageSource::Base64 { media_type, data } => {
                                parts.push(json!({
                                    "type": "input_image",
                                    "image_url": format!("data:{};base64,{}", media_type, data)
                                }));
                            }
                        },
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

        // Emit function_call / function_call_output as top-level input items
        input.extend(deferred_items);
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_args_valid_object_round_trips() {
        let v = parse_openai_tool_arguments("read_file", r#"{"path":"src/main.rs"}"#);
        assert_eq!(v["path"], "src/main.rs");
    }

    #[test]
    fn parse_args_repairs_trailing_comma() {
        let v = parse_openai_tool_arguments("read_file", r#"{"path":"src/main.rs",}"#);
        assert_eq!(v["path"], "src/main.rs");
    }

    #[test]
    fn parse_args_repairs_missing_closing_brace() {
        let v = parse_openai_tool_arguments("read_file", r#"{"path":"src/main.rs""#);
        assert_eq!(v["path"], "src/main.rs");
    }

    #[test]
    fn parse_args_unrepairable_falls_back_to_raw_arguments() {
        let v = parse_openai_tool_arguments("read_file", "not json at all");
        assert_eq!(v["raw_arguments"], "not json at all");
    }

    #[test]
    fn parse_args_morph_edit_strict_returns_empty_object_on_failure() {
        // Truncated code_edit must NOT be silently "repaired" — that would
        // merge corrupted source into the user's file. Strict parse → empty
        // object → tool dispatch reports a clear "missing parameter" error.
        let v = parse_openai_tool_arguments(
            ToolName::MorphEditFile.as_str(),
            r#"{"target_filepath":"src/lib.rs","code_edit":"fn x() { let y = [1,2,"#,
        );
        assert!(v.is_object());
        assert_eq!(v.as_object().unwrap().len(), 0);
    }

    #[test]
    fn parse_args_empty_string_falls_back_to_raw_arguments() {
        let v = parse_openai_tool_arguments("read_file", "");
        assert_eq!(v["raw_arguments"], "");
    }

    #[test]
    fn parse_args_whitespace_only_falls_back_to_raw_arguments() {
        let v = parse_openai_tool_arguments("read_file", "   \n\t");
        assert_eq!(v["raw_arguments"], "   \n\t");
    }

    #[test]
    fn parse_args_array_root_returned_as_is() {
        // Non-object root: dispatcher will surface a missing-parameter error,
        // which the model can self-correct from. We just need to not panic.
        let v = parse_openai_tool_arguments("read_file", "[1,2,3]");
        assert!(v.is_array());
    }

    #[test]
    fn parse_args_morph_edit_valid_round_trips() {
        let v = parse_openai_tool_arguments(
            ToolName::MorphEditFile.as_str(),
            r#"{"target_filepath":"src/lib.rs","instructions":"add fn","code_edit":"fn x() {}"}"#,
        );
        assert_eq!(v["target_filepath"], "src/lib.rs");
        assert_eq!(v["code_edit"], "fn x() {}");
    }
}
