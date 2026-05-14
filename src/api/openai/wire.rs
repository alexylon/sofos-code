//! Request body shaping for the OpenAI `/responses` endpoint, the
//! response-side wire types, and the conversion back into the shared
//! [`CreateMessageResponse`] shape. `build_responses_body` /
//! `build_response_input` produce the request; `OpenAIResponse` plus
//! `build_response` decode the response. Both call paths
//! (`call_responses` and the streaming `parse_stream`) converge on
//! `build_response` so the streaming and non-streaming results stay
//! identical in shape.

use crate::api::types::*;
use crate::api::utils;
use crate::error::Result;
use serde::Deserialize;
use serde_json::json;

pub(super) const TOOL_CHOICE_AUTO: &str = "auto";

pub(super) fn build_responses_body(request: &CreateMessageRequest) -> serde_json::Value {
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
pub(super) struct OpenAIResponse {
    pub(super) id: String,
    pub(super) model: String,
    pub(super) output: Vec<OpenAIOutputItem>,
    #[serde(default)]
    pub(super) usage: Option<OpenAIResponseUsage>,
    #[serde(default)]
    pub(super) status: Option<String>,
    #[serde(default)]
    pub(super) incomplete_details: Option<OpenAIIncompleteDetails>,
}

#[derive(Debug, Deserialize)]
pub(super) struct OpenAIIncompleteDetails {
    #[serde(default)]
    pub(super) reason: Option<String>,
}

#[derive(Debug, Deserialize)]
pub(super) struct OpenAIOutputItem {
    #[serde(rename = "type")]
    pub(super) item_type: String,
    /// `reasoning` items carry an `id` (e.g. `rs_…`) that pairs with
    /// `encrypted_content` for round-trip continuity. `function_call`
    /// items also have an `id` we don't currently use (we use `call_id`
    /// instead), so this field is shared.
    #[serde(default)]
    pub(super) id: Option<String>,
    #[serde(default)]
    pub(super) content: Vec<OpenAIOutputContent>,
    #[serde(default)]
    pub(super) summary: Vec<OpenAIOutputSummary>,
    /// Opaque blob the server uses to resume hidden chain-of-thought
    /// on the next request. Returned only when the request set
    /// `include: ["reasoning.encrypted_content"]`.
    #[serde(default)]
    pub(super) encrypted_content: Option<String>,
    #[serde(default)]
    pub(super) tool_calls: Option<Vec<OpenAIOutputToolCall>>,
    // Fields for when the item type is "function_call"
    #[serde(default)]
    pub(super) name: Option<String>,
    #[serde(default)]
    pub(super) arguments: Option<String>,
    #[serde(default)]
    pub(super) call_id: Option<String>,
}

#[derive(Debug, Deserialize)]
pub(super) struct OpenAIOutputContent {
    #[serde(rename = "type")]
    pub(super) content_type: String,
    #[serde(default)]
    pub(super) text: String,
}

#[derive(Debug, Deserialize)]
pub(super) struct OpenAIOutputSummary {
    #[serde(rename = "type")]
    pub(super) summary_type: String,
    #[serde(default)]
    pub(super) text: String,
}

#[derive(Debug, Deserialize)]
pub(super) struct OpenAIOutputToolCall {
    pub(super) id: String,
    pub(super) name: String,
    pub(super) arguments: String,
}

#[derive(Debug, Deserialize, Default)]
pub(super) struct OpenAIResponseUsage {
    #[serde(default)]
    pub(super) input_tokens: Option<u32>,
    #[serde(default)]
    pub(super) output_tokens: Option<u32>,
    #[serde(default)]
    pub(super) input_tokens_details: Option<OpenAIInputTokensDetails>,
}

#[derive(Debug, Deserialize, Default)]
pub(super) struct OpenAIInputTokensDetails {
    #[serde(default)]
    pub(super) cached_tokens: Option<u32>,
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
pub(super) fn reasoning_item_to_blocks(
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

/// Convert a parsed OpenAI response into the shared `CreateMessageResponse`
/// shape. Used by both the non-streaming `call_responses` path and the
/// streaming `parse_stream` path so the two converge on the same
/// content-block assembly and the same `status: "incomplete"` →
/// `stop_reason: "max_tokens"` mapping.
pub(super) fn build_response(response_parsed: OpenAIResponse) -> Result<CreateMessageResponse> {
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
                    if content.content_type == "output_text" && !content.text.trim().is_empty() {
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
