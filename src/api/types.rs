use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Message {
    pub role: String,
    #[serde(flatten)]
    pub content: MessageContent,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum MessageContent {
    Text { content: String },
    Blocks { content: Vec<MessageContentBlock> },
}

impl Message {
    pub fn user(content: impl Into<String>) -> Self {
        Self {
            role: "user".to_string(),
            content: MessageContent::Text {
                content: content.into(),
            },
        }
    }

    /// Create a user message with content blocks (text and/or images)
    pub fn user_with_blocks(content_blocks: Vec<MessageContentBlock>) -> Self {
        Self {
            role: "user".to_string(),
            content: MessageContent::Blocks {
                content: content_blocks,
            },
        }
    }

    pub fn assistant_with_blocks(content_blocks: Vec<MessageContentBlock>) -> Self {
        Self {
            role: "assistant".to_string(),
            content: MessageContent::Blocks {
                content: content_blocks,
            },
        }
    }

    pub fn user_with_tool_results(results: Vec<MessageContentBlock>) -> Self {
        Self {
            role: "user".to_string(),
            content: MessageContent::Blocks { content: results },
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct CreateMessageRequest {
    pub model: String,
    pub max_tokens: u32,
    pub messages: Vec<Message>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub system: Option<Vec<SystemPrompt>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tools: Option<Vec<Tool>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stream: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub thinking: Option<Thinking>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub output_config: Option<OutputConfig>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reasoning: Option<Reasoning>,
    /// Stable per-session id forwarded to OpenAI Responses as
    /// `prompt_cache_key` so consecutive requests share a prompt-cache
    /// shard. Cleared on the Anthropic path before sending.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub prompt_cache_key: Option<String>,
    /// Anthropic-only `context_management` block, populated for models
    /// that support server-side compaction. The Messages API generates
    /// a summary when input tokens cross `trigger.value`, returns a
    /// `compaction` content block, and on subsequent requests drops
    /// every message before it. Cleared on the OpenAI path.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub context_management: Option<ContextManagement>,
}

/// Anthropic `context_management` configuration. Currently models a
/// single `compact_20260112` edit; the API accepts an array of edits
/// for forward compatibility, but the wire shape on this end stays a
/// single Vec entry until a second edit type ships.
#[derive(Debug, Clone, Serialize)]
pub struct ContextManagement {
    pub edits: Vec<ContextEdit>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ContextEdit {
    /// Server-side compaction (`compact-2026-01-12` beta). Triggered
    /// when the request's input crosses `trigger`. Anthropic returns a
    /// `compaction` content block; on the next request, every message
    /// before that block is dropped server-side.
    #[serde(rename = "compact_20260112")]
    Compact20260112 {
        #[serde(skip_serializing_if = "Option::is_none")]
        trigger: Option<CompactionTrigger>,
        /// Optional override for the summarisation prompt. Left as
        /// `None` so Anthropic's default — which preserves recent
        /// tool outputs and code references — applies.
        #[serde(skip_serializing_if = "Option::is_none")]
        instructions: Option<String>,
    },
}

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum CompactionTrigger {
    /// Compaction fires when the request's input-token count exceeds
    /// `value`. Anthropic's documented minimum is 50,000.
    InputTokens { value: u32 },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum Tool {
    Regular {
        name: String,
        description: String,
        input_schema: serde_json::Value,
        #[serde(rename = "cache_control", skip_serializing_if = "Option::is_none")]
        cache_control: Option<CacheControl>,
    },
    AnthropicWebSearch {
        #[serde(rename = "type")]
        tool_type: String,
        name: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        max_uses: Option<u32>,
        #[serde(skip_serializing_if = "Option::is_none")]
        allowed_domains: Option<Vec<String>>,
        #[serde(skip_serializing_if = "Option::is_none")]
        blocked_domains: Option<Vec<String>>,
        #[serde(rename = "cache_control", skip_serializing_if = "Option::is_none")]
        cache_control: Option<CacheControl>,
    },
    OpenAIWebSearch {
        #[serde(rename = "type")]
        tool_type: String,
    },
}

#[derive(Debug, Deserialize)]
#[allow(dead_code)] // id / response_type / role / model are real wire fields kept for completeness
pub struct CreateMessageResponse {
    pub id: String,
    #[serde(rename = "type")]
    pub response_type: String,
    pub role: String,
    pub content: Vec<ContentBlock>,
    pub model: String,
    pub stop_reason: Option<String>,
    pub usage: Usage,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum ContentBlock {
    #[serde(rename = "text")]
    Text { text: String },
    #[serde(rename = "thinking")]
    Thinking { thinking: String, signature: String },
    #[serde(rename = "summary")]
    Summary { summary: String },
    /// Anthropic server-side compaction summary. Anthropic returns
    /// this block at the start of an assistant response when the
    /// request's input crossed the configured trigger threshold.
    /// On the next request, the API drops every message before this
    /// block server-side; we only need to keep it in the round-trip.
    #[serde(rename = "compaction")]
    Compaction { content: String },
    /// OpenAI Responses API reasoning item, packed as a single block so
    /// the `id` and `encrypted_content` (an opaque blob the server uses
    /// to resume hidden chain-of-thought) round-trip together with all
    /// of the visible summary entries belonging to the same reasoning
    /// turn. Sending the encrypted blob back on the next call lets the
    /// model continue its hidden CoT instead of rederiving it, which
    /// directly cuts hidden-reasoning output tokens on multi-call
    /// agentic turns.
    #[serde(rename = "reasoning")]
    Reasoning {
        id: String,
        #[serde(default)]
        summary: Vec<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        encrypted_content: Option<String>,
    },
    #[serde(rename = "tool_use")]
    ToolUse {
        id: String,
        name: String,
        input: serde_json::Value,
    },
    #[serde(rename = "server_tool_use")]
    ServerToolUse {
        id: String,
        name: String,
        input: serde_json::Value,
    },
    #[serde(rename = "web_search_tool_result")]
    WebSearchToolResult {
        tool_use_id: String,
        content: Vec<WebSearchResult>,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Thinking {
    #[serde(rename = "type")]
    pub thinking_type: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub budget_tokens: Option<u32>,
}

impl Thinking {
    pub fn enabled(budget_tokens: u32) -> Self {
        Self {
            thinking_type: "enabled".to_string(),
            budget_tokens: Some(budget_tokens),
        }
    }

    /// Opus 4.7+ uses adaptive thinking: the server picks the budget based
    /// on the prompt, and the caller expresses intent via
    /// [`OutputConfig::effort`] on the request instead of a token count.
    pub fn adaptive() -> Self {
        Self {
            thinking_type: "adaptive".to_string(),
            budget_tokens: None,
        }
    }
}

/// Top-level `output_config` block on the Messages API. Currently used to
/// set the `effort` level that pairs with adaptive thinking on Opus 4.7+.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OutputConfig {
    pub effort: String,
}

impl OutputConfig {
    pub fn with_effort(effort: impl Into<String>) -> Self {
        Self {
            effort: effort.into(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Reasoning {
    pub effort: String,
    /// Omitted when `None` so the model returns no summary blocks at all.
    /// Reasoning summaries bill as output tokens, so we suppress them on
    /// the thinking-off path.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
}

impl Reasoning {
    pub fn with_effort(effort: impl Into<String>) -> Self {
        Self {
            effort: effort.into(),
            summary: Some("auto".to_string()),
        }
    }

    /// Lowest-cost reasoning configuration for the thinking-off path:
    /// minimal hidden reasoning and no summary stream.
    pub fn minimal() -> Self {
        Self {
            effort: "minimal".to_string(),
            summary: None,
        }
    }
}

/// User-facing reasoning level. Default is `Medium`; `High` is opt-in
/// because it materially raises hidden-reasoning token cost on routine
/// coding work, and `Off` skips reasoning entirely (cheapest). `XHigh`
/// and `Max` are the extra-capability rungs and have model-specific
/// support — see [`crate::api::model_info::effort_support_error`] for
/// the per-model matrix. Picking an unsupported combination is
/// rejected at startup (in `main.rs`) and at the `/effort` command, so
/// the wire layer can assume every effort it sees is acceptable for
/// the active model.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, clap::ValueEnum)]
#[clap(rename_all = "lower")]
pub enum ReasoningEffort {
    Off,
    Low,
    #[default]
    Medium,
    High,
    XHigh,
    Max,
}

impl ReasoningEffort {
    pub fn parse(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "off" | "none" | "disabled" => Some(Self::Off),
            "low" => Some(Self::Low),
            "medium" | "med" => Some(Self::Medium),
            "high" => Some(Self::High),
            "xhigh" | "x-high" => Some(Self::XHigh),
            "max" | "maximum" => Some(Self::Max),
            _ => None,
        }
    }

    pub fn as_label(self) -> &'static str {
        match self {
            Self::Off => "off",
            Self::Low => "low",
            Self::Medium => "medium",
            Self::High => "high",
            Self::XHigh => "xhigh",
            Self::Max => "max",
        }
    }

    pub fn is_enabled(self) -> bool {
        !matches!(self, Self::Off)
    }
}

impl std::str::FromStr for ReasoningEffort {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Self::parse(s).ok_or_else(|| {
            format!(
                "invalid reasoning effort `{}`; expected one of: off, low, medium, high, xhigh, max",
                s
            )
        })
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WebSearchResult {
    #[serde(rename = "type")]
    pub result_type: String,
    pub url: String,
    pub title: String,
    pub encrypted_content: String,
    pub page_age: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SystemPrompt {
    #[serde(rename = "type")]
    pub system_type: String,
    pub text: String,
    #[serde(rename = "cache_control", skip_serializing_if = "Option::is_none")]
    pub cache_control: Option<CacheControl>,
}

impl SystemPrompt {
    pub fn new_cached_with_ttl(text: String, ttl: Option<String>) -> Self {
        Self {
            system_type: "text".to_string(),
            text,
            cache_control: Some(CacheControl::ephemeral(ttl)),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct CacheControl {
    #[serde(rename = "type")]
    pub cache_type: String,
    /// Optional TTL per Anthropic docs ("5m" default, or "1h" when allowed)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ttl: Option<String>,
}

impl CacheControl {
    /// Default ephemeral cache with ttl "5m"
    pub fn ephemeral(ttl: Option<String>) -> Self {
        Self {
            cache_type: "ephemeral".to_string(),
            ttl,
        }
    }

    /// 1-hour ephemeral cache. Write cost is 2x the base input rate
    /// (vs. 1.25x for 5m), reads are 0.1x for both. Worth the write
    /// premium only on prefixes that don't change between turns —
    /// system prompt, tool definitions, and the sticky anchor — where
    /// a single user pause longer than 5 minutes would otherwise force
    /// a full prefix re-bill at the cache-creation rate.
    pub fn ephemeral_one_hour() -> Self {
        Self::ephemeral(Some("1h".to_string()))
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum MessageContentBlock {
    #[serde(rename = "text")]
    Text {
        text: String,
        #[serde(rename = "cache_control", skip_serializing_if = "Option::is_none")]
        cache_control: Option<CacheControl>,
    },
    #[serde(rename = "image")]
    Image {
        source: ImageSource,
        #[serde(rename = "cache_control", skip_serializing_if = "Option::is_none")]
        cache_control: Option<CacheControl>,
    },
    #[serde(rename = "thinking")]
    Thinking {
        thinking: String,
        signature: String,
        #[serde(rename = "cache_control", skip_serializing_if = "Option::is_none")]
        cache_control: Option<CacheControl>,
    },
    #[serde(rename = "summary")]
    Summary {
        summary: String,
        #[serde(rename = "cache_control", skip_serializing_if = "Option::is_none")]
        cache_control: Option<CacheControl>,
    },
    /// Anthropic compaction summary, round-tripped to the API on
    /// subsequent turns so the server knows where to truncate. See
    /// [`ContentBlock::Compaction`].
    #[serde(rename = "compaction")]
    Compaction {
        content: String,
        #[serde(rename = "cache_control", skip_serializing_if = "Option::is_none")]
        cache_control: Option<CacheControl>,
    },
    /// OpenAI Responses API reasoning item, packed so `id`,
    /// `encrypted_content`, and the array of summary texts round-trip
    /// as a single conversation block. See [`ContentBlock::Reasoning`].
    #[serde(rename = "reasoning")]
    Reasoning {
        id: String,
        #[serde(default)]
        summary: Vec<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        encrypted_content: Option<String>,
        #[serde(rename = "cache_control", skip_serializing_if = "Option::is_none")]
        cache_control: Option<CacheControl>,
    },
    #[serde(rename = "tool_use")]
    ToolUse {
        id: String,
        name: String,
        input: serde_json::Value,
        #[serde(rename = "cache_control", skip_serializing_if = "Option::is_none")]
        cache_control: Option<CacheControl>,
    },
    #[serde(rename = "tool_result")]
    ToolResult {
        tool_use_id: String,
        content: String,
        #[serde(rename = "cache_control", skip_serializing_if = "Option::is_none")]
        cache_control: Option<CacheControl>,
    },
    #[serde(rename = "server_tool_use")]
    ServerToolUse {
        id: String,
        name: String,
        input: serde_json::Value,
        #[serde(rename = "cache_control", skip_serializing_if = "Option::is_none")]
        cache_control: Option<CacheControl>,
    },
    #[serde(rename = "web_search_tool_result")]
    WebSearchToolResult {
        tool_use_id: String,
        content: Vec<WebSearchResult>,
        #[serde(rename = "cache_control", skip_serializing_if = "Option::is_none")]
        cache_control: Option<CacheControl>,
    },
}

/// Image source for the API - can be base64-encoded or a URL
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum ImageSource {
    #[serde(rename = "base64")]
    Base64 { media_type: String, data: String },
    #[serde(rename = "url")]
    Url { url: String },
}

impl MessageContentBlock {
    pub fn from_content_block_for_api(block: &ContentBlock) -> Self {
        match block {
            ContentBlock::Text { text } => MessageContentBlock::Text {
                text: text.clone(),
                cache_control: None,
            },
            // Claude's extended thinking. When thinking is enabled the complete
            // unmodified block (including signature) must round-trip to preserve
            // reasoning continuity across tool use.
            ContentBlock::Thinking {
                thinking,
                signature,
            } => MessageContentBlock::Thinking {
                thinking: thinking.clone(),
                signature: signature.clone(),
                cache_control: None,
            },
            // GPT's reasoning summary
            ContentBlock::Summary { summary } => MessageContentBlock::Summary {
                summary: summary.clone(),
                cache_control: None,
            },
            // Anthropic server-side compaction summary; must be
            // round-tripped verbatim so the server can drop earlier
            // messages on the next turn.
            ContentBlock::Compaction { content } => MessageContentBlock::Compaction {
                content: content.clone(),
                cache_control: None,
            },
            // OpenAI Responses API reasoning item — round-trip with the
            // encrypted CoT blob so the model resumes its hidden chain
            // of thought across tool calls instead of rederiving it.
            ContentBlock::Reasoning {
                id,
                summary,
                encrypted_content,
            } => MessageContentBlock::Reasoning {
                id: id.clone(),
                summary: summary.clone(),
                encrypted_content: encrypted_content.clone(),
                cache_control: None,
            },
            ContentBlock::ToolUse { id, name, input } => MessageContentBlock::ToolUse {
                id: id.clone(),
                name: name.clone(),
                input: input.clone(),
                cache_control: None,
            },
            ContentBlock::ServerToolUse { id, name, input } => MessageContentBlock::ServerToolUse {
                id: id.clone(),
                name: name.clone(),
                input: input.clone(),
                cache_control: None,
            },
            ContentBlock::WebSearchToolResult {
                tool_use_id,
                content,
            } => MessageContentBlock::WebSearchToolResult {
                tool_use_id: tool_use_id.clone(),
                content: content.clone(),
                cache_control: None,
            },
        }
    }
}

// Tool enum defined later with cache_control support

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Usage {
    pub input_tokens: u32,
    pub output_tokens: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cache_read_input_tokens: Option<u32>,
    /// Anthropic-only; the OpenAI Responses API doesn't surface a
    /// separate creation counter.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cache_creation_input_tokens: Option<u32>,
}

#[cfg(test)]
mod block_serde_tests {
    use super::*;

    #[test]
    fn compaction_block_deserializes_without_cache_control_field() {
        // A saved session predating server-side compaction won't carry
        // a `cache_control` field on compaction blocks (they only
        // existed after this branch). Verify the new variant tolerates
        // its absence rather than failing the whole session load.
        let json = r#"{"type":"compaction","content":"summary text"}"#;
        let block: MessageContentBlock = serde_json::from_str(json).unwrap();
        match block {
            MessageContentBlock::Compaction {
                content,
                cache_control,
            } => {
                assert_eq!(content, "summary text");
                assert!(cache_control.is_none());
            }
            other => panic!("expected Compaction, got {:?}", other),
        }
    }

    #[test]
    fn reasoning_block_deserializes_with_only_id_field() {
        // Edge case: reasoning items can arrive without a summary
        // array (effort=minimal) and without encrypted_content (when
        // include flag wasn't set on the prior request). Both fields
        // are marked `#[serde(default)]`, so the bare item should
        // round-trip.
        let json = r#"{"type":"reasoning","id":"rs_only"}"#;
        let block: MessageContentBlock = serde_json::from_str(json).unwrap();
        match block {
            MessageContentBlock::Reasoning {
                id,
                summary,
                encrypted_content,
                cache_control,
            } => {
                assert_eq!(id, "rs_only");
                assert!(summary.is_empty());
                assert!(encrypted_content.is_none());
                assert!(cache_control.is_none());
            }
            other => panic!("expected Reasoning, got {:?}", other),
        }
    }

    #[test]
    fn content_block_compaction_deserializes_from_anthropic_response_shape() {
        // Anthropic's docs show the response payload as
        // `{"type":"compaction","content":"..."}`. The non-streaming
        // path goes through serde, so verify that exact wire shape
        // hits the right variant.
        let json = r#"{"type":"compaction","content":"earlier turns summarised"}"#;
        let block: ContentBlock = serde_json::from_str(json).unwrap();
        match block {
            ContentBlock::Compaction { content } => {
                assert_eq!(content, "earlier turns summarised");
            }
            other => panic!("expected Compaction, got {:?}", other),
        }
    }
}
