use super::types::*;
use super::utils;
use crate::error::{Result, SofosError};
use futures::stream::{Stream, StreamExt};
use reqwest::header::{HeaderMap, HeaderValue};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

const ANTHROPIC_API_BASE: &str = "https://api.anthropic.com/v1";
/// Pinned `anthropic-version` header value. `2023-06-01` is the
/// version the public Anthropic Messages API is documented against;
/// bumping it changes streaming-event shapes and request fields, so
/// any change here needs a smoke test against both streaming and
/// non-streaming paths.
const ANTHROPIC_API_VERSION: &str = "2023-06-01";
const BETA_HEADER_NAME: &str = "anthropic-beta";

/// Universal beta token: shrinks tool-call envelopes. Supported on
/// every model in the registry, so it ships unconditionally.
const BETA_TOKEN_EFFICIENT: &str = "token-efficient-tools-2025-02-19";

/// Server-side compaction beta. Gated per-request by [`anthropic_beta_for`]
/// based on `ModelInfo::supports_server_compaction` so a Haiku 4.5
/// request doesn't depend on Anthropic's "ignore unknown beta tokens"
/// policy. The runtime header value comes from
/// [`BETA_TOKEN_EFFICIENT_AND_COMPACT`] (which embeds this string as a
/// literal); this const exists so the drift-detection test can verify
/// the literal stays in sync with its components.
#[allow(dead_code)]
const BETA_COMPACT: &str = "compact-2026-01-12";

/// Pre-joined string sent when both betas ship. Spelled out as a
/// literal because `concat!` only works on literals (so it can't
/// reference [`BETA_TOKEN_EFFICIENT`]/[`BETA_COMPACT`] directly).
/// The `beta_with_compact_matches_components` test enforces it stays
/// in sync with its components.
const BETA_TOKEN_EFFICIENT_AND_COMPACT: &str =
    "token-efficient-tools-2025-02-19,compact-2026-01-12";

/// Compute the `anthropic-beta` header value for a single request.
/// Adds `compact-2026-01-12` when the target model advertises server-
/// side compaction, otherwise returns the base token unchanged.
fn anthropic_beta_for(model: &str) -> &'static str {
    if super::model_info::lookup(model).supports_server_compaction {
        BETA_TOKEN_EFFICIENT_AND_COMPACT
    } else {
        BETA_TOKEN_EFFICIENT
    }
}

/// Per-effort `budget_tokens` value for Anthropic's *legacy* non-adaptive
/// extended-thinking shape (`{type: "enabled", budget_tokens}`). Models
/// that require adaptive thinking (Opus 4.7+) ignore these and drive
/// effort through `output_config.effort` instead.
pub const LEGACY_THINKING_BUDGET_LOW: u32 = 1024;
pub const LEGACY_THINKING_BUDGET_MEDIUM: u32 = 5120;
pub const LEGACY_THINKING_BUDGET_HIGH: u32 = 16384;

/// Anthropic's documented minimum trigger value for the
/// `compact_20260112` context-edit. Triggers below this 400 the
/// request, so the request builder clamps `auto_compact_at` against
/// this floor.
pub const COMPACTION_TRIGGER_FLOOR: u32 = 50_000;

/// Map a [`ReasoningEffort`] to the legacy `budget_tokens` value.
/// `Off` defensively collapses to `LOW` so callers that forget to
/// pre-guard with `is_enabled()` don't panic; the request builder
/// still gates the whole legacy branch behind `is_enabled()` so the
/// `Off` arm is unreachable in practice.
pub fn legacy_thinking_budget(effort: super::types::ReasoningEffort) -> u32 {
    use super::types::ReasoningEffort;
    match effort {
        ReasoningEffort::Off | ReasoningEffort::Low => LEGACY_THINKING_BUDGET_LOW,
        ReasoningEffort::Medium => LEGACY_THINKING_BUDGET_MEDIUM,
        ReasoningEffort::High => LEGACY_THINKING_BUDGET_HIGH,
    }
}

/// Return true for models that *only* accept `thinking.type = "adaptive"`
/// (paired with `output_config.effort`) and reject the legacy
/// `{type: "enabled", budget_tokens: N}` shape with HTTP 400.
///
/// The set is owned by [`crate::api::ModelInfo`]; this thin wrapper
/// preserves the call shape used by `request_builder` and `repl::mod`
/// without forcing those sites to dereference the struct just to
/// check one bool.
pub fn requires_adaptive_thinking(model: &str) -> bool {
    super::model_info::lookup(model).requires_adaptive_thinking
}

/// Map a [`ReasoningEffort`] to the string Anthropic's adaptive thinking
/// expects in `output_config.effort` (Opus 4.7+). The API accepts
/// `low` / `medium` / `high`; `Off` collapses to `low` because adaptive
/// thinking has no off-switch — the conversation may already carry
/// thinking blocks that the server cross-checks against the request,
/// and dropping `output_config` would 400 the next turn.
pub fn effort_label(effort: super::types::ReasoningEffort) -> &'static str {
    use super::types::ReasoningEffort;
    match effort {
        ReasoningEffort::Off | ReasoningEffort::Low => "low",
        ReasoningEffort::Medium => "medium",
        ReasoningEffort::High => "high",
    }
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
        headers.insert(
            "anthropic-version",
            HeaderValue::from_static(ANTHROPIC_API_VERSION),
        );
        // `anthropic-beta` is set per-request by `anthropic_beta_for`
        // so the compaction beta only ships when the target model
        // actually supports it.

        let client = utils::build_http_client(headers, utils::REQUEST_TIMEOUT)?;

        Ok(Self { client })
    }

    /// Check if we can reach the API endpoint
    pub async fn check_connectivity(&self) -> Result<()> {
        utils::check_api_connectivity(
            &self.client,
            ANTHROPIC_API_BASE,
            "Anthropic",
            "https://status.anthropic.com",
        )
        .await
    }

    fn prepare_request(mut request: CreateMessageRequest) -> CreateMessageRequest {
        request.messages = sanitize_messages_for_anthropic(request.messages);

        // OpenAI-only; drop before serializing for Anthropic.
        request.prompt_cache_key = None;

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
        let url = format!("{}/messages", ANTHROPIC_API_BASE);
        let request = Self::prepare_request(request);
        let beta = anthropic_beta_for(&request.model);

        let response = utils::send_once(
            "Anthropic",
            self.client
                .post(&url)
                .header(BETA_HEADER_NAME, beta)
                .json(&request),
        )
        .await?;

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
        Self::parse_stream(
            byte_stream,
            on_text_delta,
            on_thinking_delta,
            interrupt_flag,
        )
        .await
    }

    /// Drive a pre-built SSE byte stream through the Anthropic parser.
    /// Split out from [`create_message_streaming`] so tests can feed
    /// hand-crafted fixtures without an HTTP layer; production callers
    /// reach this only via [`create_message_streaming`].
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

        let mut current_block_type: Option<String> = None;
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
                                    u.get("input_tokens").and_then(|v| v.as_u64()).unwrap_or(0)
                                        as u32;
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
                                    if let Some(content) =
                                        block.get("content").and_then(|v| v.as_str())
                                    {
                                        content_blocks.push(ContentBlock::Compaction {
                                            content: content.to_string(),
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
            Usage {
                input_tokens,
                output_tokens,
                cache_read_input_tokens,
                cache_creation_input_tokens,
            },
        ))
    }
}

#[derive(serde::Deserialize)]
struct WebSearchToolResultBlock {
    tool_use_id: String,
    #[serde(default)]
    content: Vec<WebSearchResult>,
}

pub(crate) fn sanitize_messages_for_anthropic(messages: Vec<Message>) -> Vec<Message> {
    messages
        .into_iter()
        .map(|mut msg| {
            if let MessageContent::Blocks { content } = msg.content {
                let filtered_content = content
                    .into_iter()
                    .filter_map(|block| match block {
                        // OpenAI reasoning summary block — not part of
                        // Anthropic's content-block schema; the server
                        // would reject the unknown type.
                        MessageContentBlock::Summary { .. } => None,
                        // OpenAI Responses API reasoning item, packed
                        // with `id` + `encrypted_content`. Carries no
                        // meaning to Anthropic and uses a `type`
                        // string the server doesn't recognise. Drop
                        // before sending so a session that switched
                        // providers doesn't 400 on the next turn.
                        MessageContentBlock::Reasoning { .. } => None,
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
    fn anthropic_beta_for_gates_compaction_to_supported_models() {
        // Opus 4.7 is on the compaction-supported list — both betas ship.
        let with_compact = anthropic_beta_for("claude-opus-4-7");
        assert!(with_compact.contains(BETA_TOKEN_EFFICIENT));
        assert!(with_compact.contains(BETA_COMPACT));

        // Haiku 4.5 isn't — only the universal beta should appear so
        // we don't depend on Anthropic's "ignore unknown beta tokens"
        // policy if validation ever tightens.
        let without = anthropic_beta_for("claude-haiku-4-5");
        assert!(without.contains(BETA_TOKEN_EFFICIENT));
        assert!(!without.contains(BETA_COMPACT));
    }

    #[test]
    fn beta_with_compact_matches_components() {
        // `BETA_TOKEN_EFFICIENT_AND_COMPACT` is a literal that must
        // stay in lockstep with its two component consts. Catch drift
        // here so renaming one component without the other is a test
        // failure rather than a silent header mismatch in production.
        assert_eq!(
            BETA_TOKEN_EFFICIENT_AND_COMPACT,
            format!("{BETA_TOKEN_EFFICIENT},{BETA_COMPACT}")
        );
    }

    #[test]
    fn legacy_thinking_budget_helper_scales_with_effort() {
        use super::super::types::ReasoningEffort;
        assert_eq!(
            legacy_thinking_budget(ReasoningEffort::Low),
            LEGACY_THINKING_BUDGET_LOW
        );
        assert_eq!(
            legacy_thinking_budget(ReasoningEffort::Medium),
            LEGACY_THINKING_BUDGET_MEDIUM
        );
        assert_eq!(
            legacy_thinking_budget(ReasoningEffort::High),
            LEGACY_THINKING_BUDGET_HIGH
        );
        // Defensive default: `Off` collapses to `LOW` rather than
        // panicking, even though the legacy branch is upstream-guarded.
        assert_eq!(
            legacy_thinking_budget(ReasoningEffort::Off),
            LEGACY_THINKING_BUDGET_LOW
        );
        // Compile-time guard: the three tier values must stay strictly
        // increasing. Runtime `assert!` would be a tautology on consts
        // (clippy::assertions_on_constants), so check at const-eval time.
        const _: () = {
            assert!(LEGACY_THINKING_BUDGET_LOW < LEGACY_THINKING_BUDGET_MEDIUM);
            assert!(LEGACY_THINKING_BUDGET_MEDIUM < LEGACY_THINKING_BUDGET_HIGH);
        };
    }

    #[test]
    fn effort_label_maps_reasoning_levels() {
        use super::super::types::ReasoningEffort;
        assert_eq!(effort_label(ReasoningEffort::Off), "low");
        assert_eq!(effort_label(ReasoningEffort::Low), "low");
        assert_eq!(effort_label(ReasoningEffort::Medium), "medium");
        assert_eq!(effort_label(ReasoningEffort::High), "high");
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
            prompt_cache_key: None,
            context_management: None,
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
            prompt_cache_key: None,
            context_management: None,
        };

        let json = serde_json::to_value(&request).unwrap();
        assert!(json["thinking"].is_object());
        assert_eq!(json["thinking"]["type"], "enabled");
        assert_eq!(json["thinking"]["budget_tokens"], 3000);
    }

    #[test]
    fn prepare_request_strips_prompt_cache_key() {
        let request = CreateMessageRequest {
            model: "claude-sonnet-4-6".to_string(),
            max_tokens: 8192,
            messages: vec![],
            system: None,
            tools: None,
            stream: None,
            thinking: None,
            output_config: None,
            reasoning: None,
            prompt_cache_key: Some("session-1".to_string()),
            context_management: None,
        };

        let prepared = AnthropicClient::prepare_request(request);
        assert!(prepared.prompt_cache_key.is_none());
    }

    #[test]
    fn sanitizer_drops_openai_reasoning_blocks_before_anthropic_call() {
        // Regression: a session that started on OpenAI accumulates
        // `Reasoning` blocks with `id` + `encrypted_content`. Switching
        // to Anthropic mid-session and forwarding those blocks would
        // 400 on a content-block-type the server doesn't know.
        let messages = vec![Message {
            role: "assistant".to_string(),
            content: MessageContent::Blocks {
                content: vec![
                    MessageContentBlock::Reasoning {
                        id: "rs_abc".to_string(),
                        summary: vec!["thought".to_string()],
                        encrypted_content: Some("blob".to_string()),
                        cache_control: None,
                    },
                    MessageContentBlock::Text {
                        text: "real reply".to_string(),
                        cache_control: None,
                    },
                ],
            },
        }];
        let cleaned = sanitize_messages_for_anthropic(messages);
        let MessageContent::Blocks { content } = &cleaned[0].content else {
            panic!("expected blocks");
        };
        assert_eq!(content.len(), 1, "Reasoning block must be dropped");
        assert!(matches!(content[0], MessageContentBlock::Text { .. }));
    }

    mod streaming {
        use super::*;
        use crate::api::utils::sse_test_support::sse_stream_from_events;
        use serde_json::json;
        use std::sync::Mutex;

        fn flag() -> Arc<AtomicBool> {
            Arc::new(AtomicBool::new(false))
        }

        #[tokio::test]
        async fn text_block_streams_through_callback_and_aggregates_in_response() {
            let events = vec![
                json!({
                    "type": "message_start",
                    "message": {
                        "id": "msg_test",
                        "model": "claude-sonnet-4-6",
                        "usage": {"input_tokens": 12, "cache_read_input_tokens": 3}
                    }
                }),
                json!({"type": "content_block_start", "index": 0, "content_block": {"type": "text", "text": ""}}),
                json!({"type": "content_block_delta", "index": 0, "delta": {"type": "text_delta", "text": "Hi "}}),
                json!({"type": "content_block_delta", "index": 0, "delta": {"type": "text_delta", "text": "there"}}),
                json!({"type": "content_block_stop", "index": 0}),
                json!({
                    "type": "message_delta",
                    "delta": {"stop_reason": "end_turn"},
                    "usage": {"output_tokens": 7}
                }),
                json!({"type": "message_stop"}),
            ];

            let text_chunks: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
            let t = text_chunks.clone();
            let stream = sse_stream_from_events(events);

            let response = AnthropicClient::parse_stream(
                stream,
                move |s| t.lock().unwrap().push(s.to_string()),
                |_| {},
                flag(),
            )
            .await
            .expect("parse_stream succeeds");

            assert_eq!(
                text_chunks.lock().unwrap().as_slice(),
                &["Hi ".to_string(), "there".to_string()]
            );
            assert_eq!(response._id, "msg_test");
            assert_eq!(response.stop_reason.as_deref(), Some("end_turn"));
            assert_eq!(response.usage.input_tokens, 12);
            assert_eq!(response.usage.output_tokens, 7);
            assert_eq!(response.usage.cache_read_input_tokens, Some(3));
            assert_eq!(response.content.len(), 1);
            assert!(matches!(
                &response.content[0],
                ContentBlock::Text { text } if text == "Hi there"
            ));
        }

        #[tokio::test]
        async fn thinking_block_with_signature_streams_through_thinking_callback() {
            // A `thinking` block must arrive with a `signature_delta`; the
            // parser drops thinking blocks without one because echoing
            // unsigned thinking back to the server 400s the next turn.
            let events = vec![
                json!({"type": "message_start", "message": {"id": "msg_t", "model": "claude-opus-4-7", "usage": {"input_tokens": 5}}}),
                json!({"type": "content_block_start", "index": 0, "content_block": {"type": "thinking"}}),
                json!({"type": "content_block_delta", "index": 0, "delta": {"type": "thinking_delta", "thinking": "let me think..."}}),
                json!({"type": "content_block_delta", "index": 0, "delta": {"type": "signature_delta", "signature": "abc123sig"}}),
                json!({"type": "content_block_stop", "index": 0}),
                json!({"type": "message_delta", "delta": {"stop_reason": "end_turn"}, "usage": {"output_tokens": 2}}),
            ];

            let think_chunks: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
            let th = think_chunks.clone();
            let stream = sse_stream_from_events(events);

            let response = AnthropicClient::parse_stream(
                stream,
                |_| {},
                move |s| th.lock().unwrap().push(s.to_string()),
                flag(),
            )
            .await
            .expect("parse_stream succeeds");

            assert_eq!(
                think_chunks.lock().unwrap().as_slice(),
                &["let me think...".to_string()]
            );
            assert_eq!(response.content.len(), 1);
            assert!(matches!(
                &response.content[0],
                ContentBlock::Thinking { thinking, signature }
                if thinking == "let me think..." && signature == "abc123sig"
            ));
        }

        #[tokio::test]
        async fn thinking_block_without_signature_is_dropped() {
            // Pins the invariant documented at `content_block_stop` /
            // `Some("thinking")`: an unsigned thinking block can't be
            // echoed back on the next turn, so the parser must not
            // include it in the response.
            let events = vec![
                json!({"type": "message_start", "message": {"id": "msg_t", "model": "claude-opus-4-7", "usage": {"input_tokens": 5}}}),
                json!({"type": "content_block_start", "index": 0, "content_block": {"type": "thinking"}}),
                json!({"type": "content_block_delta", "index": 0, "delta": {"type": "thinking_delta", "thinking": "unsigned"}}),
                json!({"type": "content_block_stop", "index": 0}),
                json!({"type": "message_delta", "delta": {"stop_reason": "end_turn"}, "usage": {"output_tokens": 1}}),
            ];

            let stream = sse_stream_from_events(events);
            let response = AnthropicClient::parse_stream(stream, |_| {}, |_| {}, flag())
                .await
                .expect("parse_stream succeeds");
            assert!(
                response.content.is_empty(),
                "unsigned thinking must be dropped, got {:?}",
                response.content
            );
        }

        #[tokio::test]
        async fn error_event_returns_api_error() {
            let events = vec![
                json!({"type": "message_start", "message": {"id": "msg_e", "model": "claude-sonnet-4-6", "usage": {"input_tokens": 1}}}),
                json!({"type": "error", "error": {"message": "overloaded"}}),
            ];

            let stream = sse_stream_from_events(events);
            let err = AnthropicClient::parse_stream(stream, |_| {}, |_| {}, flag())
                .await
                .expect_err("error event must surface as error");
            assert!(
                matches!(&err, SofosError::Api(msg) if msg.contains("overloaded")),
                "got: {err:?}"
            );
        }
    }
}
