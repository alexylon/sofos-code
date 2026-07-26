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

    /// A session can span more than one model — a `/model` switch, or a
    /// refusal answered on a fallback. Each response has to be priced at
    /// the rates of whichever model produced it, which a single total
    /// derived from the counters cannot express.
    #[test]
    fn cost_is_priced_per_response_at_the_serving_model() {
        use crate::api::model_info::{CLAUDE_HAIKU, CLAUDE_OPUS, lookup};

        let mut state = SessionState::new("test".to_string(), ConversationHistory::new());
        state.add_usage(&usage_with_inputs(1_000, 100), CLAUDE_OPUS, CLAUDE_OPUS);
        state.add_usage(&usage_with_inputs(1_000, 100), CLAUDE_HAIKU, CLAUDE_OPUS);

        let expected = lookup(CLAUDE_OPUS).turn_cost(1_000, 100, 0, 0)
            + lookup(CLAUDE_HAIKU).turn_cost(1_000, 100, 0, 0);
        assert!(
            (state.total_cost - expected).abs() < 1e-12,
            "expected {expected}, got {}",
            state.total_cost
        );
        // Pricing everything at the configured model would overcharge,
        // since the second response ran on the cheaper one.
        let all_at_configured = lookup(CLAUDE_OPUS).turn_cost(2_000, 200, 0, 0);
        assert!(state.total_cost < all_at_configured);
    }

    /// Anthropic can answer a declined request on a model that is not in
    /// the table, and there are no rates on file for one of those. The
    /// configured model stands in rather than the lookup silently
    /// falling through to the default model's prices.
    #[test]
    fn an_unpriceable_serving_model_falls_back_to_the_configured_one() {
        use crate::api::model_info::{CLAUDE_HAIKU, lookup};

        let mut state = SessionState::new("test".to_string(), ConversationHistory::new());
        state.add_usage(
            &usage_with_inputs(1_000, 100),
            "some-unlisted-model",
            CLAUDE_HAIKU,
        );

        let expected = lookup(CLAUDE_HAIKU).turn_cost(1_000, 100, 0, 0);
        assert!((state.total_cost - expected).abs() < 1e-12);
    }

    #[test]
    fn add_usage_saturates_at_u32_ceiling() {
        // A long-running session that crosses 2^32 tokens used to wrap
        // silently in release and panic in debug. Saturation keeps the
        // displayed total truthful as a lower bound.
        let mut state = SessionState::new("test".to_string(), ConversationHistory::new());
        state.total_input_tokens = u32::MAX - 5;
        state.total_output_tokens = u32::MAX - 5;

        state.add_usage(
            &usage_with_inputs(10, 10),
            crate::api::model_info::CLAUDE_SONNET,
            crate::api::model_info::CLAUDE_SONNET,
        );

        assert_eq!(state.total_input_tokens, u32::MAX);
        assert_eq!(state.total_output_tokens, u32::MAX);
    }

    #[test]
    fn add_usage_normal_path_unchanged() {
        // The non-saturating path keeps its previous semantics so the
        // shift to `saturating_add` doesn't perturb cost reporting in
        // the common case.
        let mut state = SessionState::new("test".to_string(), ConversationHistory::new());
        state.add_usage(
            &usage_with_inputs(1_000, 200),
            crate::api::model_info::CLAUDE_SONNET,
            crate::api::model_info::CLAUDE_SONNET,
        );
        state.add_usage(
            &usage_with_inputs(2_500, 600),
            crate::api::model_info::CLAUDE_SONNET,
            crate::api::model_info::CLAUDE_SONNET,
        );

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
    /// [`Model::turn_cost`](crate::api::model_info::Model::turn_cost)
    /// normalises the difference when pricing a response.
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
    /// session. Recorded as a session statistic; no supported model
    /// prices off it today.
    ///
    /// All five counters above, and the running cost below, are
    /// persisted through
    /// [`SessionTokenCounters`](crate::session::SessionTokenCounters)
    /// so a `--resume` keeps the summary accurate. Session files
    /// written before persistence was added default every counter to 0
    /// via `#[serde(default)]`.
    pub peak_single_turn_input_tokens: u32,
    /// Running USD estimate, accumulated per response at the rates of
    /// the model that actually produced it. Kept as a total rather than
    /// derived from the counters above because one session can span
    /// several models — a `/model` switch, or a request answered on a
    /// fallback model after a refusal — and each is priced differently.
    pub total_cost: f64,
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
            total_cost: 0.0,
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
        self.total_cost = 0.0;
    }

    /// Fold one response's usage into the session totals. `served_by`
    /// is the model that produced it, which is not always the model the
    /// session is configured with.
    pub fn add_usage(&mut self, usage: &crate::api::Usage, served_by: &str, configured: &str) {
        let priced_as = crate::api::model_info::pricing_model(served_by, configured);
        self.total_cost += crate::api::model_info::lookup(priced_as).turn_cost(
            usage.input_tokens,
            usage.output_tokens,
            usage.cache_read_input_tokens.unwrap_or(0),
            usage.cache_creation_input_tokens.unwrap_or(0),
        );
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
        // Per-call high-water mark on input tokens. For OpenAI the
        // figure already includes cached input; for Anthropic cache
        // reads come on a separate counter, so this is uncached input
        // only.
        if usage.input_tokens > self.peak_single_turn_input_tokens {
            self.peak_single_turn_input_tokens = usage.input_tokens;
        }
    }
}
