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
    pub reasoning: Option<Reasoning>,
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
pub struct CreateMessageResponse {
    #[serde(rename = "id")]
    pub _id: String,
    #[serde(rename = "type")]
    pub _response_type: String,
    #[serde(rename = "role")]
    pub _role: String,
    pub content: Vec<ContentBlock>,
    #[serde(rename = "model")]
    pub _model: String,
    #[serde(rename = "stop_reason")]
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
    pub budget_tokens: u32,
}

impl Thinking {
    pub fn enabled(budget_tokens: u32) -> Self {
        Self {
            thinking_type: "enabled".to_string(),
            budget_tokens,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Reasoning {
    pub effort: String,
    summary: String,
}

impl Reasoning {
    pub fn enabled() -> Self {
        Self {
            effort: "high".to_string(),
            summary: "auto".to_string(),
        }
    }

    pub fn disabled() -> Self {
        Self {
            effort: "low".to_string(),
            summary: "auto".to_string(),
        }
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

    pub fn _ephemeral_one_hour() -> Self {
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

impl MessageContentBlock {
    pub fn from_content_block_for_api(block: &ContentBlock) -> Option<Self> {
        match block {
            ContentBlock::Text { text } => Some(MessageContentBlock::Text {
                text: text.clone(),
                cache_control: None,
            }),
            // Claude's extended thinking
            ContentBlock::Thinking {
                thinking,
                signature,
            } => {
                // When thinking is enabled, we must include the complete unmodified thinking block
                // with signature to maintain reasoning continuity during tool use
                Some(MessageContentBlock::Thinking {
                    thinking: thinking.clone(),
                    signature: signature.clone(),
                    cache_control: None,
                })
            }
            // GPT's reasoning summary
            ContentBlock::Summary { summary } => Some(MessageContentBlock::Summary {
                summary: summary.clone(),
                cache_control: None,
            }),
            ContentBlock::ToolUse { id, name, input } => Some(MessageContentBlock::ToolUse {
                id: id.clone(),
                name: name.clone(),
                input: input.clone(),
                cache_control: None,
            }),
            ContentBlock::ServerToolUse { id, name, input } => {
                Some(MessageContentBlock::ServerToolUse {
                    id: id.clone(),
                    name: name.clone(),
                    input: input.clone(),
                    cache_control: None,
                })
            }
            ContentBlock::WebSearchToolResult {
                tool_use_id,
                content,
            } => Some(MessageContentBlock::WebSearchToolResult {
                tool_use_id: tool_use_id.clone(),
                content: content.clone(),
                cache_control: None,
            }),
        }
    }
}

// Tool enum defined later with cache_control support

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum _ContentDelta {
    #[serde(rename = "text_delta")]
    TextDelta { text: String },
    #[serde(rename = "input_json_delta")]
    InputJsonDelta { partial_json: String },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Usage {
    pub input_tokens: u32,
    pub output_tokens: u32,
}

// Streaming types
#[derive(Debug, Deserialize)]
pub struct _StreamEvent {
    #[serde(rename = "type")]
    pub event_type: String,
    #[serde(flatten)]
    pub data: serde_json::Value,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "type")]
pub enum _StreamEventType {
    #[serde(rename = "message_start")]
    MessageStart { message: serde_json::Value },
    #[serde(rename = "content_block_start")]
    ContentBlockStart {
        index: usize,
        content_block: ContentBlock,
    },
    #[serde(rename = "content_block_delta")]
    ContentBlockDelta { index: usize, delta: _ContentDelta },
    #[serde(rename = "content_block_stop")]
    ContentBlockStop { index: usize },
    #[serde(rename = "message_delta")]
    MessageDelta { delta: serde_json::Value },
    #[serde(rename = "message_stop")]
    MessageStop,
    #[serde(rename = "ping")]
    Ping,
}
