//! Persistent shapes for the session subsystem. Every type here lands
//! on disk via the `serde_json` round-trip in `manager`, so a field
//! addition or rename is a wire-format change — guard new fields with
//! `#[serde(default)]` so older session files keep loading.

use crate::api::{Message, SystemPrompt};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum DisplayMessage {
    UserMessage {
        content: String,
    },
    AssistantMessage {
        content: String,
    },
    ToolExecution {
        tool_name: String,
        tool_input: serde_json::Value,
        tool_output: String,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionMetadata {
    pub id: String,
    pub preview: String,
    pub created_at: u64,
    pub updated_at: u64,
    pub message_count: usize,
}

/// Snapshot of session token counters persisted alongside the
/// conversation. Every field has `#[serde(default)]` so older session
/// files (written before persistence was added) load with all counters
/// at 0 — the cost line under-reports on resume of those old files
/// until the next API call replenishes the totals, same as the pre-
/// persistence behaviour. Files written after persistence was added
/// round-trip every counter, so the cost summary stays accurate
/// across a `--resume`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionTokenCounters {
    #[serde(default)]
    pub total_input_tokens: u32,
    #[serde(default)]
    pub total_output_tokens: u32,
    #[serde(default)]
    pub total_cache_read_tokens: u32,
    #[serde(default)]
    pub total_cache_creation_tokens: u32,
    /// Largest input-token count observed on any single API call.
    /// Used by `calculate_cost` to detect tiered-pricing cliffs
    /// (gpt-5.4/5.5 flip the entire session to premium rates once
    /// any prompt crosses 272K input tokens).
    #[serde(default)]
    pub peak_single_turn_input_tokens: u32,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct Session {
    pub id: String,
    /// Messages in API format (for continuing the conversation with AI)
    pub api_messages: Vec<Message>,
    /// Messages in display format (for reconstructing the original UI)
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub display_messages: Vec<DisplayMessage>,
    pub system_prompt: Vec<SystemPrompt>,
    pub created_at: u64,
    pub updated_at: u64,
    /// Token counters at save time. Flattened into the top level of
    /// the JSON so each counter is its own key.
    #[serde(default, flatten)]
    pub token_counters: SessionTokenCounters,
}
