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
    /// Total input tokens consumed in this session
    pub total_input_tokens: u32,
    /// Total output tokens generated in this session
    pub total_output_tokens: u32,
}

impl SessionState {
    pub fn new(session_id: String, conversation: ConversationHistory) -> Self {
        Self {
            session_id,
            conversation,
            display_messages: Vec::new(),
            total_input_tokens: 0,
            total_output_tokens: 0,
        }
    }

    pub fn clear(&mut self, new_session_id: String) {
        self.session_id = new_session_id;
        self.conversation.clear();
        self.display_messages.clear();
        self.total_input_tokens = 0;
        self.total_output_tokens = 0;
    }

    pub fn add_tokens(&mut self, input_tokens: u32, output_tokens: u32) {
        self.total_input_tokens += input_tokens;
        self.total_output_tokens += output_tokens;
    }
}
