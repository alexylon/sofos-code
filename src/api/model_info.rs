//! Per-model metadata. One [`ModelInfo`] entry per supported model,
//! looked up at the boundary in `config::max_context_tokens_for`,
//! `anthropic::requires_adaptive_thinking`, `ui::calculate_cost`, and
//! [`ConversationHistory`](crate::repl::conversation::ConversationHistory).
//! Adding a model is one struct literal in [`lookup`].

use crate::api::ReasoningEffort;

/// Tiered-pricing rule. Some OpenAI models (gpt-5.4, gpt-5.5) charge a
/// premium for the *entire session* once a single prompt's input
/// crosses a documented threshold. Once tripped, every subsequent
/// turn in the session is billed at the premium rate, not just the
/// triggering turn.
#[derive(Debug, Clone, Copy)]
pub struct PremiumPricingTier {
    pub input_threshold: u32,
    pub price_input_per_m: f64,
    pub price_output_per_m: f64,
}

#[derive(Debug, Clone, Copy)]
pub struct ModelInfo {
    /// API context-window ceiling in tokens.
    pub context_window: u32,
    /// Cost-shaping override for the auto-compact trigger. When `Some`,
    /// auto-compaction fires at `min(override, 90% of context_window)`.
    /// When `None`, falls back to 90% of `context_window` (codex
    /// default — "don't crash the API" rather than "don't burn tokens").
    pub auto_compact_token_limit: Option<u32>,
    /// True for Anthropic models that use the `thinking: adaptive`
    /// shape with `output_config.effort`. Opus 4.7 rejects the legacy
    /// `{type: "enabled", budget_tokens}` shape outright; Opus 4.6
    /// and Sonnet 4.6 accept both shapes but Anthropic recommends
    /// adaptive, so sofos opts them in too.
    pub requires_adaptive_thinking: bool,
    /// True for Anthropic models that support the server-side
    /// compaction beta (`compact-2026-01-12`). When set, the request
    /// builder enables Anthropic's automatic compaction instead of
    /// running a client-side LLM-summary turn.
    pub supports_server_compaction: bool,
    /// Reasoning-effort levels this model accepts on the wire.
    /// Startup validation and the `/think` handler use this list to
    /// reject mismatched pairs (e.g. `xhigh` on Sonnet 4.6, `max` on
    /// any OpenAI model) before they reach the server.
    pub supported_efforts: &'static [ReasoningEffort],
    /// Per-million-token USD price for non-cached input.
    pub price_input_per_m: f64,
    /// Per-million-token USD price for output (including hidden
    /// reasoning tokens on OpenAI reasoning models).
    pub price_output_per_m: f64,
    /// Tiered-pricing rule when the model has one. `None` for models
    /// that bill at a flat per-token rate regardless of prompt size.
    pub premium_tier: Option<PremiumPricingTier>,
}

impl Default for ModelInfo {
    fn default() -> Self {
        // Sonnet-class fallback: matches the historical default in
        // `calculate_cost` and is the safest "I don't know this model"
        // bet — pricing won't under-report. Unknown models inherit
        // only the basic effort tiers, since we don't know whether
        // they'll accept `xhigh` or `max`.
        Self {
            context_window: 200_000,
            auto_compact_token_limit: Some(170_000),
            requires_adaptive_thinking: false,
            supports_server_compaction: false,
            supported_efforts: &[
                ReasoningEffort::Off,
                ReasoningEffort::Low,
                ReasoningEffort::Medium,
                ReasoningEffort::High,
            ],
            price_input_per_m: 3.0,
            price_output_per_m: 15.0,
            premium_tier: None,
        }
    }
}

impl ModelInfo {
    /// Auto-compaction trigger in tokens. The override is clamped
    /// against 90% of the API ceiling so a too-loose override can
    /// never push us past what the server will accept on the next
    /// turn.
    pub fn auto_compact_at(&self) -> u32 {
        let api_ceiling = ((self.context_window as u64).saturating_mul(9) / 10) as u32;
        match self.auto_compact_token_limit {
            Some(limit) => limit.min(api_ceiling),
            None => api_ceiling,
        }
    }

    /// Effective context window after reserving 5% for output
    /// headroom. Used as the trim-safety floor: above this, older
    /// messages are dropped without summary as a last resort.
    pub fn effective_window(&self) -> u32 {
        ((self.context_window as u64).saturating_mul(95) / 100) as u32
    }

    /// Comma-separated lowercase labels of every effort level this
    /// model accepts (`"off, low, medium, high, xhigh"` and so on).
    /// Surfaced verbatim in the CLI startup error and in
    /// [`effort_support_error`] so both messages list the same set.
    pub fn supported_efforts_label(&self) -> String {
        self.supported_efforts
            .iter()
            .map(|e| e.as_label())
            .collect::<Vec<_>>()
            .join(", ")
    }
}

/// LLM vendor a model belongs to. Used to pick the right API client
/// at startup and to detect a cross-provider resume without
/// instantiating both clients.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Provider {
    Anthropic,
    OpenAI,
}

impl Provider {
    pub fn label(self) -> &'static str {
        match self {
            Provider::Anthropic => "Anthropic",
            Provider::OpenAI => "OpenAI",
        }
    }
}

/// Provider that owns this model id. The lookup is case-insensitive
/// and prefix-based so versioned ids (`gpt-5.5-2026-mm-dd`,
/// `claude-opus-4-7-20260301`) route to the right client. Unknown
/// ids fall back to Anthropic, matching the historical behaviour of
/// the startup script. Adding a new vendor is one extra prefix here.
pub fn provider_for(model: &str) -> Provider {
    let m = model.to_ascii_lowercase();
    const OPENAI_PREFIXES: &[&str] = &["gpt-", "o1", "o3", "o4"];
    if OPENAI_PREFIXES.iter().any(|p| m.starts_with(p)) {
        return Provider::OpenAI;
    }
    Provider::Anthropic
}

/// Look up metadata for a model by id. Matching is case-insensitive
/// and prefix-based so versioned ids (`claude-opus-4-7-20260301`,
/// `gpt-5.5-2026-mm-dd`) resolve to the canonical entry. Unknown
/// models return [`ModelInfo::default`].
pub fn lookup(model: &str) -> ModelInfo {
    let m = model.to_ascii_lowercase();

    // Anthropic. Order matters: more-specific prefixes first so a
    // versioned id (`claude-opus-4-7-20260301`) resolves to the right
    // entry instead of the closest shorter prefix.
    if m.starts_with("claude-opus-4-7") {
        return ModelInfo {
            context_window: 1_000_000,
            auto_compact_token_limit: Some(250_000),
            requires_adaptive_thinking: true,
            supports_server_compaction: true,
            supported_efforts: &[
                ReasoningEffort::Off,
                ReasoningEffort::Low,
                ReasoningEffort::Medium,
                ReasoningEffort::High,
                ReasoningEffort::XHigh,
                ReasoningEffort::Max,
            ],
            price_input_per_m: 5.0,
            price_output_per_m: 25.0,
            premium_tier: None,
        };
    }
    if m.starts_with("claude-opus-4-6") {
        return ModelInfo {
            context_window: 1_000_000,
            auto_compact_token_limit: Some(250_000),
            requires_adaptive_thinking: true,
            supports_server_compaction: true,
            supported_efforts: &[
                ReasoningEffort::Off,
                ReasoningEffort::Low,
                ReasoningEffort::Medium,
                ReasoningEffort::High,
                ReasoningEffort::Max,
            ],
            price_input_per_m: 5.0,
            price_output_per_m: 25.0,
            premium_tier: None,
        };
    }
    if m.starts_with("claude-sonnet-4-6") {
        return ModelInfo {
            context_window: 1_000_000,
            auto_compact_token_limit: Some(250_000),
            requires_adaptive_thinking: true,
            supports_server_compaction: true,
            supported_efforts: &[
                ReasoningEffort::Off,
                ReasoningEffort::Low,
                ReasoningEffort::Medium,
                ReasoningEffort::High,
                ReasoningEffort::Max,
            ],
            price_input_per_m: 3.0,
            price_output_per_m: 15.0,
            premium_tier: None,
        };
    }
    if m.starts_with("claude-haiku-4-5") {
        return ModelInfo {
            context_window: 200_000,
            auto_compact_token_limit: Some(170_000),
            requires_adaptive_thinking: false,
            supports_server_compaction: false,
            supported_efforts: &[
                ReasoningEffort::Off,
                ReasoningEffort::Low,
                ReasoningEffort::Medium,
                ReasoningEffort::High,
            ],
            price_input_per_m: 1.0,
            price_output_per_m: 5.0,
            premium_tier: None,
        };
    }

    // OpenAI. Codex variants are matched first because their slug
    // contains `codex` regardless of the gpt-5.x prefix.
    if m.contains("codex") {
        return ModelInfo {
            context_window: 400_000,
            auto_compact_token_limit: Some(250_000),
            requires_adaptive_thinking: false,
            supports_server_compaction: false,
            supported_efforts: &[
                ReasoningEffort::Off,
                ReasoningEffort::Low,
                ReasoningEffort::Medium,
                ReasoningEffort::High,
                ReasoningEffort::XHigh,
            ],
            price_input_per_m: 1.75,
            price_output_per_m: 14.0,
            premium_tier: None,
        };
    }
    // gpt-5.4 / gpt-5.5 charge 2x input / 1.5x output for the *entire
    // session* once any single prompt crosses 272K input tokens. The
    // 250K auto-compact trigger sits below that cliff, so the listed
    // `price_*` values stay on the standard tier — by design. Raising
    // the override past 272K would silently double the input bill and
    // is the wrong knob to pull for cost. The `premium_tier` value
    // here is what `ui::calculate_cost` uses to tell honest billing
    // if the cliff is ever tripped (e.g. by a huge pasted file).
    if m.starts_with("gpt-5.4") {
        return ModelInfo {
            context_window: 1_050_000,
            auto_compact_token_limit: Some(250_000),
            requires_adaptive_thinking: false,
            supports_server_compaction: false,
            supported_efforts: &[
                ReasoningEffort::Off,
                ReasoningEffort::Low,
                ReasoningEffort::Medium,
                ReasoningEffort::High,
                ReasoningEffort::XHigh,
            ],
            price_input_per_m: 2.5,
            price_output_per_m: 15.0,
            premium_tier: Some(PremiumPricingTier {
                input_threshold: 272_000,
                price_input_per_m: 5.0,
                price_output_per_m: 22.5,
            }),
        };
    }
    if m.starts_with("gpt-5.5") {
        return ModelInfo {
            context_window: 1_050_000,
            auto_compact_token_limit: Some(250_000),
            requires_adaptive_thinking: false,
            supports_server_compaction: false,
            supported_efforts: &[
                ReasoningEffort::Off,
                ReasoningEffort::Low,
                ReasoningEffort::Medium,
                ReasoningEffort::High,
                ReasoningEffort::XHigh,
            ],
            price_input_per_m: 5.0,
            price_output_per_m: 30.0,
            premium_tier: Some(PremiumPricingTier {
                input_threshold: 272_000,
                price_input_per_m: 10.0,
                price_output_per_m: 45.0,
            }),
        };
    }

    ModelInfo::default()
}

/// Human-readable rejection message for an unsupported `(model, effort)`
/// pair, or `None` if the pair is supported. The message names the
/// model and lists every effort level the model does accept, so the
/// user can pick a valid alternative without consulting the docs.
/// Surfaced to the user from the startup validator and the `/think`
/// handler so the failure mode is the same in both places.
pub fn effort_support_error(model: &str, effort: ReasoningEffort) -> Option<String> {
    let info = lookup(model);
    if info.supported_efforts.contains(&effort) {
        return None;
    }
    Some(format!(
        "Model `{}` does not accept reasoning effort `{}`. Supported levels: {}.",
        model,
        effort.as_label(),
        info.supported_efforts_label(),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn provider_routes_gpt_and_claude_correctly() {
        assert_eq!(provider_for("gpt-5.5"), Provider::OpenAI);
        assert_eq!(provider_for("gpt-5.4-codex"), Provider::OpenAI);
        assert_eq!(provider_for("GPT-5.5"), Provider::OpenAI);
        assert_eq!(provider_for("claude-opus-4-7"), Provider::Anthropic);
        assert_eq!(
            provider_for("claude-sonnet-4-6-20260301"),
            Provider::Anthropic
        );
        assert_eq!(provider_for("o1-mini"), Provider::OpenAI);
        assert_eq!(provider_for("unknown-model"), Provider::Anthropic);
    }

    #[test]
    fn provider_label_is_human_readable() {
        assert_eq!(Provider::OpenAI.label(), "OpenAI");
        assert_eq!(Provider::Anthropic.label(), "Anthropic");
    }

    #[test]
    fn opus_4_7_has_1m_context_and_server_compaction() {
        let info = lookup("claude-opus-4-7");
        assert_eq!(info.context_window, 1_000_000);
        assert!(info.requires_adaptive_thinking);
        assert!(info.supports_server_compaction);
    }

    #[test]
    fn lookup_matches_versioned_opus_4_7_ids() {
        assert!(lookup("claude-opus-4-7").requires_adaptive_thinking);
        assert!(lookup("claude-opus-4-7-20260301").requires_adaptive_thinking);
        assert!(lookup("Claude-Opus-4-7").requires_adaptive_thinking);
    }

    #[test]
    fn anthropic_1m_models_all_use_adaptive_thinking() {
        // Opus 4.7 requires adaptive (legacy shape 400s); Opus 4.6 and
        // Sonnet 4.6 accept both but Anthropic recommends adaptive, so
        // sofos opts them in alongside 4.7.
        for slug in ["claude-opus-4-7", "claude-opus-4-6", "claude-sonnet-4-6"] {
            assert!(
                lookup(slug).requires_adaptive_thinking,
                "{slug} should use adaptive thinking"
            );
        }
    }

    #[test]
    fn legacy_anthropic_models_still_use_manual_thinking() {
        // Sonnet 4.5 / Opus 4.5 / Haiku 4.5 don't expose adaptive
        // thinking, so they keep the legacy `budget_tokens` shape.
        for slug in ["claude-sonnet-4-5", "claude-opus-4-5", "claude-haiku-4-5"] {
            assert!(
                !lookup(slug).requires_adaptive_thinking,
                "{slug} should keep the legacy budget_tokens shape"
            );
        }
    }

    #[test]
    fn unknown_model_falls_back_to_sonnet_class_pricing() {
        let info = lookup("some-future-model-2099");
        assert_eq!(info.price_input_per_m, 3.0);
        assert_eq!(info.price_output_per_m, 15.0);
    }

    #[test]
    fn auto_compact_at_clamps_override_against_api_ceiling() {
        let info = ModelInfo {
            context_window: 100_000,
            auto_compact_token_limit: Some(200_000),
            ..ModelInfo::default()
        };
        assert_eq!(info.auto_compact_at(), 90_000);
    }

    #[test]
    fn auto_compact_at_falls_back_to_90pct_when_unset() {
        let info = ModelInfo {
            context_window: 200_000,
            auto_compact_token_limit: None,
            ..ModelInfo::default()
        };
        assert_eq!(info.auto_compact_at(), 180_000);
    }

    #[test]
    fn effective_window_reserves_5pct_headroom() {
        let info = ModelInfo {
            context_window: 1_000_000,
            ..ModelInfo::default()
        };
        assert_eq!(info.effective_window(), 950_000);
    }

    #[test]
    fn cliff_models_compact_below_272k_premium_threshold() {
        for slug in ["gpt-5.5", "gpt-5.4"] {
            let info = lookup(slug);
            assert!(info.auto_compact_at() < 272_000);
            let tier = info
                .premium_tier
                .expect("cliff models carry a premium tier");
            assert_eq!(tier.input_threshold, 272_000);
            assert!(tier.price_input_per_m > info.price_input_per_m);
        }
    }

    #[test]
    fn anthropic_1m_models_advertise_server_compaction() {
        for slug in ["claude-opus-4-7", "claude-opus-4-6", "claude-sonnet-4-6"] {
            assert!(
                lookup(slug).supports_server_compaction,
                "{slug} should opt into server-side compaction"
            );
        }
    }

    #[test]
    fn haiku_does_not_advertise_server_compaction() {
        // Haiku 4.5 isn't on Anthropic's compaction-supported list,
        // so the request builder must not send the beta header for it.
        assert!(!lookup("claude-haiku-4-5").supports_server_compaction);
    }

    #[test]
    fn effort_support_matches_provider_matrix() {
        use ReasoningEffort::*;

        let supports = |slug: &str, e: ReasoningEffort| effort_support_error(slug, e).is_none();

        // Basic tiers are universal.
        for slug in [
            "claude-opus-4-7",
            "claude-opus-4-6",
            "claude-sonnet-4-6",
            "claude-haiku-4-5",
            "claude-sonnet-4-5",
            "gpt-5.5",
            "gpt-5.4",
            "gpt-5.3-codex",
        ] {
            for e in [Off, Low, Medium, High] {
                assert!(supports(slug, e), "{slug} should accept {e:?}");
            }
        }

        // `xhigh`: Opus 4.7 + every gpt-5.x reasoning model only.
        assert!(supports("claude-opus-4-7", XHigh));
        assert!(supports("gpt-5.5", XHigh));
        assert!(supports("gpt-5.4", XHigh));
        assert!(supports("gpt-5.3-codex", XHigh));
        assert!(!supports("claude-opus-4-6", XHigh));
        assert!(!supports("claude-sonnet-4-6", XHigh));
        assert!(!supports("claude-haiku-4-5", XHigh));

        // `max`: Anthropic adaptive models only.
        assert!(supports("claude-opus-4-7", Max));
        assert!(supports("claude-opus-4-6", Max));
        assert!(supports("claude-sonnet-4-6", Max));
        assert!(!supports("claude-haiku-4-5", Max));
        assert!(!supports("gpt-5.5", Max));
        assert!(!supports("gpt-5.3-codex", Max));
    }

    #[test]
    fn effort_support_error_lists_supported_levels_for_the_model() {
        let err = effort_support_error("gpt-5.5", ReasoningEffort::Max)
            .expect("max on gpt-5.5 should be rejected");
        assert!(err.contains("gpt-5.5"));
        assert!(err.contains("`max`"));
        let listed = err
            .split("Supported levels: ")
            .nth(1)
            .expect("error message lists supported levels");
        for label in ["off", "low", "medium", "high", "xhigh"] {
            assert!(listed.contains(label), "expected {label} in {listed}");
        }
        // gpt-5.5 doesn't accept `max`, so the supported-list tail
        // must not mention it.
        assert!(!listed.contains("max"));

        let err = effort_support_error("claude-sonnet-4-6", ReasoningEffort::XHigh)
            .expect("xhigh on sonnet-4-6 should be rejected");
        assert!(err.contains("claude-sonnet-4-6"));
        assert!(err.contains("`xhigh`"));
        // Sonnet 4.6 supports `max` but not `xhigh`.
        let listed = err
            .split("Supported levels: ")
            .nth(1)
            .expect("error message lists supported levels");
        assert!(listed.contains("max"));
        assert!(!listed.contains("xhigh"));

        // Supported combinations: no error.
        assert!(effort_support_error("claude-opus-4-7", ReasoningEffort::Max).is_none());
        assert!(effort_support_error("gpt-5.5", ReasoningEffort::XHigh).is_none());
        assert!(effort_support_error("claude-haiku-4-5", ReasoningEffort::High).is_none());
    }
}
