use super::types::*;
use super::utils;
use crate::error::{Result, SofosError};
use reqwest::header::{AUTHORIZATION, HeaderMap, HeaderValue};
use serde::Deserialize;
use serde_json::json;

const OPENAI_API_BASE: &str = "https://api.openai.com/v1";
const TOOL_CHOICE_AUTO: &str = "auto";

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

        let client = utils::build_http_client(headers, utils::REQUEST_TIMEOUT)?;

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
        let body = build_responses_body(&request);

        if std::env::var("SOFOS_DEBUG").is_ok() {
            if let Some(tools) = body.get("tools").and_then(|v| v.as_array()) {
                eprintln!("\n=== OpenAI /responses Request ===");
                eprintln!("Sending {} tools to OpenAI", tools.len());
                for tool in tools {
                    if let Some(name) = tool.get("name").and_then(|v| v.as_str()) {
                        eprintln!("  - Tool: {}", name);
                    }
                }
                eprintln!("==================================\n");
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

        let response = utils::send_once("OpenAI", self.client.post(&url).json(&body)).await?;

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
                            let input = utils::parse_tool_arguments(&call.name, &call.arguments);
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
                        let input = utils::parse_tool_arguments(&name, &arguments);
                        content_blocks.push(ContentBlock::ToolUse {
                            id: call_id,
                            name,
                            input,
                        });
                    }
                }
                "reasoning" => {
                    let summary_texts: Vec<String> = item
                        .summary
                        .into_iter()
                        .filter(|s| s.summary_type == "summary_text" && !s.text.trim().is_empty())
                        .map(|s| s.text)
                        .collect();
                    content_blocks.extend(reasoning_item_to_blocks(
                        item.id,
                        summary_texts,
                        item.encrypted_content,
                    ));
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
        let cache_read = usage
            .input_tokens_details
            .as_ref()
            .and_then(|d| d.cached_tokens);

        // Map OpenAI's `status: "incomplete"` / `incomplete_details.reason`
        // onto the shared `stop_reason` field so the REPL's existing
        // "Response was cut off due to token limit" warning fires for
        // OpenAI the same way it already does for Anthropic. Without
        // this, a truncated tool call (e.g. `write_file` missing `path`
        // because the JSON was cut mid-emission) surfaces only as a
        // confusing "Missing 'path' parameter" error with no hint that
        // the root cause is `--max-tokens` being too small.
        let stop_reason = match (
            response_parsed.status.as_deref(),
            response_parsed
                .incomplete_details
                .as_ref()
                .and_then(|d| d.reason.as_deref()),
        ) {
            (Some("incomplete"), Some("max_output_tokens" | "max_tokens")) => {
                Some("max_tokens".to_string())
            }
            (Some("incomplete"), Some(other)) => Some(other.to_string()),
            _ => None,
        };

        Ok(utils::build_message_response(
            response_parsed.id,
            response_parsed.model,
            content_blocks,
            stop_reason,
            Usage {
                input_tokens: usage.input_tokens.unwrap_or(0),
                output_tokens: usage.output_tokens.unwrap_or(0),
                cache_read_input_tokens: cache_read,
                cache_creation_input_tokens: None,
            },
        ))
    }
}

fn build_responses_body(request: &CreateMessageRequest) -> serde_json::Value {
    let inputs = build_response_input(request);

    let mut body = json!({
        "model": request.model,
        "input": inputs,
        "max_output_tokens": request.max_tokens,
        "reasoning": request.reasoning,
    });

    // Ask the server to surface the encrypted hidden chain-of-thought
    // alongside reasoning items so we can round-trip it on the next
    // call. Without this, every tool round-trip forces the model to
    // rederive its reasoning from scratch — billed as fresh output
    // tokens at the reasoning-output rate.
    if request.reasoning.is_some() {
        body["include"] = json!(["reasoning.encrypted_content"]);
    }

    if let Some(ref cache_key) = request.prompt_cache_key {
        body["prompt_cache_key"] = json!(cache_key);
    }

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
            body["tool_choice"] = json!(TOOL_CHOICE_AUTO);
        }
    }

    body
}

pub(crate) fn build_response_input(request: &CreateMessageRequest) -> Vec<serde_json::Value> {
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
        // Reasoning items round-trip as top-level input items emitted
        // **before** the surrounding message, matching OpenAI's
        // response-time ordering (reasoning → text → tool_calls).
        // Putting them after would still serialize, but breaks the
        // chronology the server uses to pair reasoning with its reply.
        let mut pre_message_items: Vec<serde_json::Value> = Vec::new();
        // function_call / function_call_output items come **after** the
        // message because OpenAI's reference shape pairs each call to
        // the assistant turn that emitted it.
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
                        MessageContentBlock::Compaction { .. } => {
                            // Anthropic-only block; OpenAI never sees
                            // it because we don't enable server-side
                            // compaction on the OpenAI path. If a
                            // session was migrated from Anthropic, the
                            // compaction summary is already accounted
                            // for in the surviving conversation tail,
                            // so silently skipping it here is correct.
                        }
                        MessageContentBlock::Reasoning {
                            id,
                            summary,
                            encrypted_content,
                            ..
                        } => {
                            // Round-trip the OpenAI reasoning item as a
                            // top-level input item so the server resumes
                            // the prior hidden chain-of-thought instead
                            // of regenerating it. `summary` items must
                            // be wrapped in `{type: "summary_text"}`,
                            // and `encrypted_content` is only present
                            // when the prior request set
                            // `include: ["reasoning.encrypted_content"]`.
                            let summary_items: Vec<serde_json::Value> = summary
                                .iter()
                                .map(|text| {
                                    json!({
                                        "type": "summary_text",
                                        "text": text,
                                    })
                                })
                                .collect();
                            let mut item = json!({
                                "type": "reasoning",
                                "id": id,
                                "summary": summary_items,
                            });
                            if let Some(enc) = encrypted_content {
                                item["encrypted_content"] = json!(enc);
                            }
                            pre_message_items.push(item);
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

        // Reasoning first (it preceded the assistant text in the
        // original response), then the message block, then tool calls
        // and tool results.
        input.extend(pre_message_items);

        if !parts.is_empty() {
            input.push(json!({
                "role": msg.role,
                "content": parts,
            }));
        }

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
    #[serde(default)]
    status: Option<String>,
    #[serde(default)]
    incomplete_details: Option<OpenAIIncompleteDetails>,
}

#[derive(Debug, Deserialize)]
struct OpenAIIncompleteDetails {
    #[serde(default)]
    reason: Option<String>,
}

#[derive(Debug, Deserialize)]
struct OpenAIOutputItem {
    #[serde(rename = "type")]
    item_type: String,
    /// `reasoning` items carry an `id` (e.g. `rs_…`) that pairs with
    /// `encrypted_content` for round-trip continuity. `function_call`
    /// items also have an `id` we don't currently use (we use `call_id`
    /// instead), so this field is shared.
    #[serde(default)]
    id: Option<String>,
    #[serde(default)]
    content: Vec<OpenAIOutputContent>,
    #[serde(default)]
    summary: Vec<OpenAIOutputSummary>,
    /// Opaque blob the server uses to resume hidden chain-of-thought
    /// on the next request. Returned only when the request set
    /// `include: ["reasoning.encrypted_content"]`.
    #[serde(default)]
    encrypted_content: Option<String>,
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
    #[serde(default)]
    input_tokens_details: Option<OpenAIInputTokensDetails>,
}

#[derive(Debug, Deserialize, Default)]
struct OpenAIInputTokensDetails {
    #[serde(default)]
    cached_tokens: Option<u32>,
}

/// Convert a single OpenAI `reasoning` output item into the content
/// blocks sofos stores in conversation history.
///
/// With an `id` present, the whole item (id + visible summary +
/// encrypted CoT) packs into one [`ContentBlock::Reasoning`] so the
/// next request can round-trip it as a single `{type: "reasoning"}`
/// input — splitting into per-summary blocks would lose the shared
/// `id`/`encrypted_content` and force the server to rederive the
/// hidden chain-of-thought on every tool round-trip.
///
/// Two edge cases:
/// 1. `id` present but neither summary nor encrypted_content — drop
///    the block. The wire shape `{type: "reasoning", id, summary: []}`
///    is rejected by some OpenAI models, and the block carries no
///    signal worth round-tripping anyway.
/// 2. No `id` (old payloads predating the field) — fall back to
///    per-text [`ContentBlock::Summary`] blocks so the visible
///    reasoning still surfaces.
fn reasoning_item_to_blocks(
    id: Option<String>,
    summary_texts: Vec<String>,
    encrypted_content: Option<String>,
) -> Vec<ContentBlock> {
    if let Some(rid) = id {
        if summary_texts.is_empty() && encrypted_content.is_none() {
            return Vec::new();
        }
        vec![ContentBlock::Reasoning {
            id: rid,
            summary: summary_texts,
            encrypted_content,
        }]
    } else {
        summary_texts
            .into_iter()
            .map(|text| ContentBlock::Summary { summary: text })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::Message;

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
}
