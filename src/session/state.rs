use crate::repl::conversation::ConversationHistory;
use crate::session::DisplayMessage;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::Usage;
    use crate::repl::conversation::ConversationHistory;

    fn usage_with_inputs(input_tokens: u32, output_tokens: u32) -> Usage {
        Usage {
            input_tokens,
            output_tokens,
            cache_read_input_tokens: None,
            cache_creation_input_tokens: None,
        }
    }

    #[test]
    fn add_usage_saturates_at_u32_ceiling() {
        // A long-running session that crosses 2^32 tokens used to wrap
        // silently in release and panic in debug. Saturation keeps the
        // displayed total truthful as a lower bound.
        let mut state = SessionState::new("test".to_string(), ConversationHistory::new());
        state.total_input_tokens = u32::MAX - 5;
        state.total_output_tokens = u32::MAX - 5;

        state.add_usage(&usage_with_inputs(10, 10));

        assert_eq!(state.total_input_tokens, u32::MAX);
        assert_eq!(state.total_output_tokens, u32::MAX);
    }

    #[test]
    fn add_usage_normal_path_unchanged() {
        // The non-saturating path keeps its previous semantics so the
        // shift to `saturating_add` doesn't perturb cost reporting in
        // the common case.
        let mut state = SessionState::new("test".to_string(), ConversationHistory::new());
        state.add_usage(&usage_with_inputs(1_000, 200));
        state.add_usage(&usage_with_inputs(2_500, 600));

        assert_eq!(state.total_input_tokens, 3_500);
        assert_eq!(state.total_output_tokens, 800);
        assert_eq!(state.peak_single_turn_input_tokens, 2_500);
    }
}

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
    /// Largest input-token count observed on any single API call this
    /// session. Used to detect tiered-pricing cliffs (gpt-5.4/5.5
    /// flip the entire session to premium rates once any prompt
    /// crosses 272K input tokens). Compared against
    /// `ModelInfo::premium_tier.input_threshold` in `calculate_cost`
    /// so the displayed cost reflects what the provider actually
    /// bills, not the standard-tier rate.
    ///
    /// All five counters above are persisted through
    /// [`SessionTokenCounters`](crate::session::SessionTokenCounters)
    /// so a `--resume` keeps the cost summary accurate and the cliff
    /// detector remembers whether the threshold had already been
    /// crossed. Session files written before persistence was added
    /// default every counter to 0 via `#[serde(default)]`.
    pub peak_single_turn_input_tokens: u32,
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
            peak_single_turn_input_tokens: 0,
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
        self.peak_single_turn_input_tokens = 0;
    }

    pub fn add_usage(&mut self, usage: &crate::api::Usage) {
        // `saturating_add` instead of `+=`: each counter is `u32`, and a
        // session that survives across `--resume` invocations
        // accumulates over multiple turns. The 4.29-billion ceiling is
        // well above any realistic single session, but a wraparound
        // would silently corrupt the displayed cost summary, and a
        // debug build would panic. Saturating at the ceiling keeps the
        // displayed total honest about "at least this many" rather
        // than wrapping to a tiny number.
        self.total_input_tokens = self.total_input_tokens.saturating_add(usage.input_tokens);
        self.total_output_tokens = self.total_output_tokens.saturating_add(usage.output_tokens);
        self.total_cache_read_tokens = self
            .total_cache_read_tokens
            .saturating_add(usage.cache_read_input_tokens.unwrap_or(0));
        self.total_cache_creation_tokens = self
            .total_cache_creation_tokens
            .saturating_add(usage.cache_creation_input_tokens.unwrap_or(0));
        // Per-call high-water mark on input tokens. For OpenAI, the
        // figure already includes cached input (the provider's
        // documented basis for the 272K premium cliff); for Anthropic,
        // cache reads come on a separate counter, so this is uncached
        // input only — neither model has a documented Anthropic cliff,
        // so the asymmetry doesn't matter today.
        if usage.input_tokens > self.peak_single_turn_input_tokens {
            self.peak_single_turn_input_tokens = usage.input_tokens;
        }
    }
}
