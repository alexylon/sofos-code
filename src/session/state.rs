use crate::repl::conversation::ConversationHistory;
use crate::session::DisplayMessage;

/// Manages the state of a single REPL session
#[derive(Clone)]
pub struct SessionState {
    /// Unique identifier for this session
    pub session_id: String,
    /// Conversation history for API
    pub conversation: ConversationHistory,
    /// Display-friendly message history for UI
    pub display_messages: Vec<DisplayMessage>,
    /// Total input tokens consumed in this session.
    /// Provider semantics differ:
    ///
    /// - OpenAI Responses API: this is the **total** count, of which
    ///   `total_cache_read_tokens` is a subset.
    /// - Anthropic Messages API: this is **uncached** new tokens only;
    ///   cache read/creation are tracked separately and disjoint.
    ///
    /// `calculate_cost` normalizes this when computing the bill.
    pub total_input_tokens: u32,
    /// Total output tokens generated in this session
    pub total_output_tokens: u32,
    /// Tokens served from the provider prompt cache (charged at a
    /// reduced rate). Both providers report this; semantics relative to
    /// `total_input_tokens` differ as documented above.
    pub total_cache_read_tokens: u32,
    /// Tokens written to the Anthropic prompt cache (charged at a
    /// premium). OpenAI does not surface a creation counter and leaves
    /// this at 0.
    pub total_cache_creation_tokens: u32,
}

impl SessionState {
    pub fn new(session_id: String, conversation: ConversationHistory) -> Self {
        Self {
            session_id,
            conversation,
            display_messages: Vec::new(),
            total_input_tokens: 0,
            total_output_tokens: 0,
            total_cache_read_tokens: 0,
            total_cache_creation_tokens: 0,
        }
    }

    pub fn clear(&mut self, new_session_id: String) {
        self.session_id = new_session_id;
        self.conversation.clear();
        self.display_messages.clear();
        self.total_input_tokens = 0;
        self.total_output_tokens = 0;
        self.total_cache_read_tokens = 0;
        self.total_cache_creation_tokens = 0;
    }

    pub fn add_usage(&mut self, usage: &crate::api::Usage) {
        self.total_input_tokens += usage.input_tokens;
        self.total_output_tokens += usage.output_tokens;
        self.total_cache_read_tokens += usage.cache_read_input_tokens.unwrap_or(0);
        self.total_cache_creation_tokens += usage.cache_creation_input_tokens.unwrap_or(0);
    }
}
