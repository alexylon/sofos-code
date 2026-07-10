//! Per-model metadata. The application only accepts the entries in
//! [`SUPPORTED_MODELS`] — everywhere a model is named, the slug
//! resolves to one of those entries. `--model` rejects anything else
//! at startup, and the `/model` picker, the request builder, and the
//! cost calculator all reach for the same table through [`lookup`].
//!
//! Every model id lives in exactly one place: the version-free
//! constants below (`CLAUDE_OPUS`, `GPT_FLAGSHIP`, and so on). Their
//! value is the slug sent on the wire, so renaming a model is a
//! one-line change to that value — the table, the request builders,
//! and the tests all reference the constant, never the raw string.
//!
//! Adding a model is one struct literal in [`SUPPORTED_MODELS`] (in
//! the order it should appear in the picker). Removing a model is
//! one deletion in the same array.

use crate::api::ReasoningEffort;

/// Tiered-pricing rule. Some OpenAI models charge a premium for the
/// *entire session* once a single prompt's input crosses a documented
/// threshold. Once tripped, every subsequent turn in the session is
/// billed at the premium rate, not just the triggering turn.
#[derive(Debug, Clone, Copy)]
pub struct PremiumPricingTier {
    pub input_threshold: u32,
    pub price_input_per_m: f64,
    pub price_output_per_m: f64,
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

/// A model the application knows about. Carries everything any other
/// module needs to know to talk to the provider: the slug sent on the
/// wire, the user-facing description, the LLM vendor, context-window
/// and compaction limits, reasoning-effort support, and pricing.
#[derive(Debug, Clone, Copy)]
pub struct Model {
    /// Slug the user types and the provider sees on the wire.
    pub name: &'static str,
    /// Short blurb rendered next to the name inside the `/model`
    /// picker.
    pub description: &'static str,
    /// LLM vendor that hosts this model. The picker greys out rows
    /// whose provider doesn't match the running session's client.
    pub provider: Provider,
    /// API context-window ceiling in tokens.
    pub context_window: u32,
    /// Cost-shaping override for the auto-compact trigger. When
    /// `Some`, auto-compaction fires at `min(override, 90% of
    /// context_window)`. When `None`, falls back to 90% of
    /// `context_window`.
    pub auto_compact_token_limit: Option<u32>,
    /// True for Anthropic models that use the `thinking: adaptive`
    /// shape with `output_config.effort` rather than the legacy
    /// `{type: "enabled", budget_tokens}` shape. Some adaptive models
    /// reject the legacy shape outright; others accept both, but
    /// Anthropic recommends adaptive, so the request builder opts in
    /// wherever this flag is set.
    pub requires_adaptive_thinking: bool,
    /// True for Anthropic models that support the server-side
    /// compaction beta (`compact-2026-01-12`). When set, the request
    /// builder enables Anthropic's automatic compaction instead of
    /// running a client-side LLM-summary turn.
    pub supports_server_compaction: bool,
    /// Reasoning-effort levels this model accepts on the wire.
    /// Startup validation and the `/effort` handler use this list to
    /// reject mismatched pairs (for example `xhigh` on a model that
    /// tops out at `high`) before they reach the server.
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

impl Model {
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

impl Default for Model {
    /// Default model returned by [`lookup`] when the caller passes a
    /// slug outside the whitelist. Matches the CLI's `--model`
    /// default, so any path that runs before the user has chosen a
    /// model (`SofosConfig::default`, internal helpers) starts on the
    /// same numbers the default model would produce.
    fn default() -> Self {
        SUPPORTED_MODELS[DEFAULT_MODEL_INDEX]
    }
}

/// Position of the application-wide default model inside
/// [`SUPPORTED_MODELS`]. Kept as a named index so the array order can
/// be reshuffled without losing track of which entry the rest of the
/// code treats as the default.
const DEFAULT_MODEL_INDEX: usize = 2;

/// Slug of the application-wide default model. Exposed as a `const`
/// so the CLI `default_value` attribute and the `Model::default`
/// fallback can share one source of truth without duplicating the
/// string.
pub const DEFAULT_MODEL_NAME: &str = SUPPORTED_MODELS[DEFAULT_MODEL_INDEX].name;

/// Compile-time guard: the default-model index must stay inside the
/// array. A reorder that drops below the index (or a refactor that
/// renames the constant out of step with the array) trips here
/// instead of panicking at runtime when the first request hits.
const _: () = assert!(DEFAULT_MODEL_INDEX < SUPPORTED_MODELS.len());

/// Input-token threshold at which OpenAI's premium-pricing cliff
/// fires. Sessions that cross this on any single prompt are billed at
/// the premium rate for every subsequent turn, so the auto-compact
/// triggers on the premium-tier OpenAI models are kept below it on
/// purpose.
const OPENAI_PREMIUM_INPUT_THRESHOLD: u32 = 272_000;

/// Canonical model-id strings — the slug each model is known by on the
/// wire. Every reference to a model id (the [`SUPPORTED_MODELS`] table,
/// the request builders, the tests) goes through one of these
/// constants, so changing what a model is called is a one-line edit to
/// the value here. The identifiers are deliberately version-free for
/// the same reason: a rename never touches an identifier or its uses.
pub const CLAUDE_FABLE: &str = "claude-fable-5";
pub const CLAUDE_OPUS: &str = "claude-opus-4-8";
pub const CLAUDE_SONNET: &str = "claude-sonnet-5";
pub const CLAUDE_HAIKU: &str = "claude-haiku-4-5";
pub const GPT_SOL: &str = "gpt-5.6-sol";
pub const GPT_TERRA: &str = "gpt-5.6-terra";
pub const GPT_LUNA: &str = "gpt-5.6-luna";
pub const GPT_FLAGSHIP: &str = "gpt-5.5";
pub const GPT_MID_TIER: &str = "gpt-5.4";
pub const GPT_MINI: &str = "gpt-5.4-mini";
pub const GPT_CODEX: &str = "gpt-5.3-codex";

/// Every model the application accepts on `--model`, in the order they
/// appear in the `/model` picker. The strongest models come first; the
/// default model sits just below them, so a user without a preference
/// lands on a balanced everyday model rather than the most expensive
/// one.
pub const SUPPORTED_MODELS: &[Model] = &[
    Model {
        name: CLAUDE_FABLE,
        description: "Anthropic's most capable model - demanding reasoning, 1M context",
        provider: Provider::Anthropic,
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
        price_input_per_m: 10.0,
        price_output_per_m: 50.0,
        premium_tier: None,
    },
    Model {
        name: CLAUDE_OPUS,
        description: "Powerful Anthropic reasoning model, 1M context",
        provider: Provider::Anthropic,
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
    },
    Model {
        name: CLAUDE_SONNET,
        description: "Balanced Anthropic model - default for day-to-day coding",
        provider: Provider::Anthropic,
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
        price_input_per_m: 3.0,
        price_output_per_m: 15.0,
        premium_tier: None,
    },
    Model {
        name: CLAUDE_HAIKU,
        description: "Fastest, cheapest Anthropic model - 200k context",
        provider: Provider::Anthropic,
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
    },
    // The newest OpenAI family (sol/terra/luna) bills flat at every
    // prompt size — no premium cliff — unlike the older 1M-context
    // OpenAI models below.
    Model {
        name: GPT_SOL,
        description: "OpenAI frontier model for complex professional work",
        provider: Provider::OpenAI,
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
            ReasoningEffort::Max,
        ],
        price_input_per_m: 5.0,
        price_output_per_m: 30.0,
        premium_tier: None,
    },
    Model {
        name: GPT_TERRA,
        description: "Balanced OpenAI model - intelligence at mid-tier cost",
        provider: Provider::OpenAI,
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
            ReasoningEffort::Max,
        ],
        price_input_per_m: 2.5,
        price_output_per_m: 15.0,
        premium_tier: None,
    },
    Model {
        name: GPT_LUNA,
        description: "OpenAI model optimised for cost-sensitive workloads",
        provider: Provider::OpenAI,
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
            ReasoningEffort::Max,
        ],
        price_input_per_m: 1.0,
        price_output_per_m: 6.0,
        premium_tier: None,
    },
    // The premium-tier OpenAI models charge 2x input / 1.5x output for
    // the *entire session* once any single prompt crosses
    // `OPENAI_PREMIUM_INPUT_THRESHOLD` input tokens. The 250K
    // auto-compact trigger sits below that cliff, so the listed
    // `price_*` values stay on the standard tier — by design. Raising
    // the override past the threshold would silently double the input
    // bill and is the wrong knob to pull for cost. The `premium_tier`
    // value is what `ui::calculate_cost` uses to bill honestly when the
    // cliff is tripped (e.g. by a huge pasted file).
    Model {
        name: GPT_FLAGSHIP,
        description: "Previous OpenAI flagship - strong GPT for code and long context",
        provider: Provider::OpenAI,
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
            input_threshold: OPENAI_PREMIUM_INPUT_THRESHOLD,
            price_input_per_m: 10.0,
            price_output_per_m: 45.0,
        }),
    },
    Model {
        name: GPT_MID_TIER,
        description: "Mid-tier OpenAI reasoning model - cheaper than the flagship",
        provider: Provider::OpenAI,
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
            input_threshold: OPENAI_PREMIUM_INPUT_THRESHOLD,
            price_input_per_m: 5.0,
            price_output_per_m: 22.5,
        }),
    },
    Model {
        name: GPT_MINI,
        description: "Compact OpenAI model - best price for coding and tool use",
        provider: Provider::OpenAI,
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
        price_input_per_m: 0.75,
        price_output_per_m: 4.5,
        premium_tier: None,
    },
    Model {
        name: GPT_CODEX,
        description: "Code-specialised OpenAI model for software engineering",
        provider: Provider::OpenAI,
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
    },
];

/// Comma-separated list of every supported model id, in catalog
/// order. Used by [`model_support_error`] and surfaced in the CLI
/// startup error so the user sees the same labels both places.
pub fn supported_models_label() -> String {
    SUPPORTED_MODELS
        .iter()
        .map(|m| m.name)
        .collect::<Vec<_>>()
        .join(", ")
}

/// Canonical entry for `name` when the slug (case-insensitively)
/// matches one of [`SUPPORTED_MODELS`]. Returns `None` otherwise so
/// the caller can refuse the value at the boundary.
pub fn canonical_model(name: &str) -> Option<&'static Model> {
    SUPPORTED_MODELS
        .iter()
        .find(|m| m.name.eq_ignore_ascii_case(name))
}

/// Human-readable rejection message for an unsupported model id, or
/// `None` when the slug is in [`SUPPORTED_MODELS`]. Used by the CLI
/// validator and the `/model <name>` handler so the failure mode is
/// the same in both places.
pub fn model_support_error(name: &str) -> Option<String> {
    if canonical_model(name).is_some() {
        return None;
    }
    Some(format!(
        "Model `{}` is not supported. Available models: {}.",
        name,
        supported_models_label()
    ))
}

/// Provider that owns this model id. The CLI rejects unknown slugs
/// up front, so in normal use the name is always in the whitelist;
/// the fallback to the default model's provider exists only for
/// internal call sites that look up arbitrary strings.
pub fn provider_for(name: &str) -> Provider {
    lookup(name).provider
}

/// Look up metadata for a model by id. Matching is case-insensitive.
/// Unsupported slugs return the default model — the same entry
/// `--model` falls back to when the user does not pass the flag; the
/// CLI rejects unknown ids up front so this fallback only fires from
/// internal call sites that pass arbitrary strings.
pub fn lookup(name: &str) -> &'static Model {
    canonical_model(name).unwrap_or(&SUPPORTED_MODELS[DEFAULT_MODEL_INDEX])
}

/// Human-readable rejection message for an unsupported `(model, effort)`
/// pair, or `None` if the pair is supported. The message names the
/// model and lists every effort level the model does accept, so the
/// user can pick a valid alternative without consulting the docs.
/// Surfaced to the user from the startup validator and the `/effort`
/// handler so the failure mode is the same in both places.
pub fn effort_support_error(name: &str, effort: ReasoningEffort) -> Option<String> {
    let info = lookup(name);
    if info.supported_efforts.contains(&effort) {
        return None;
    }
    Some(format!(
        "Model `{}` does not accept reasoning effort `{}`. Supported levels: {}.",
        name,
        effort.as_label(),
        info.supported_efforts_label(),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn provider_routes_supported_models_correctly() {
        assert_eq!(provider_for(CLAUDE_FABLE), Provider::Anthropic);
        assert_eq!(provider_for(CLAUDE_OPUS), Provider::Anthropic);
        assert_eq!(provider_for(CLAUDE_SONNET), Provider::Anthropic);
        assert_eq!(provider_for(CLAUDE_HAIKU), Provider::Anthropic);
        assert_eq!(provider_for(GPT_SOL), Provider::OpenAI);
        assert_eq!(provider_for(GPT_TERRA), Provider::OpenAI);
        assert_eq!(provider_for(GPT_LUNA), Provider::OpenAI);
        assert_eq!(provider_for(GPT_FLAGSHIP), Provider::OpenAI);
        assert_eq!(provider_for(GPT_MID_TIER), Provider::OpenAI);
        assert_eq!(provider_for(GPT_MINI), Provider::OpenAI);
        assert_eq!(provider_for(GPT_CODEX), Provider::OpenAI);
        // Case insensitivity covers an upper-cased `--model` argument.
        assert_eq!(
            provider_for(&CLAUDE_OPUS.to_uppercase()),
            Provider::Anthropic
        );
        // Unsupported slugs fall back to the default model's provider
        // (Anthropic, since the default is an Anthropic model).
        assert_eq!(provider_for("unknown-model"), Provider::Anthropic);
    }

    #[test]
    fn provider_label_is_human_readable() {
        assert_eq!(Provider::OpenAI.label(), "OpenAI");
        assert_eq!(Provider::Anthropic.label(), "Anthropic");
    }

    #[test]
    fn supported_models_contains_every_whitelisted_id_in_order() {
        let names: Vec<&str> = SUPPORTED_MODELS.iter().map(|m| m.name).collect();
        assert_eq!(
            names,
            vec![
                CLAUDE_FABLE,
                CLAUDE_OPUS,
                CLAUDE_SONNET,
                CLAUDE_HAIKU,
                GPT_SOL,
                GPT_TERRA,
                GPT_LUNA,
                GPT_FLAGSHIP,
                GPT_MID_TIER,
                GPT_MINI,
                GPT_CODEX,
            ]
        );
    }

    #[test]
    fn default_model_is_the_cli_default() {
        assert_eq!(Model::default().name, CLAUDE_SONNET);
        assert_eq!(SUPPORTED_MODELS[DEFAULT_MODEL_INDEX].name, CLAUDE_SONNET);
    }

    #[test]
    fn canonical_model_normalises_case() {
        let m = canonical_model(&CLAUDE_SONNET.to_uppercase()).expect("matches whitelist");
        assert_eq!(m.name, CLAUDE_SONNET);
    }

    #[test]
    fn model_support_error_accepts_whitelist_and_rejects_others() {
        for m in SUPPORTED_MODELS {
            assert!(
                model_support_error(m.name).is_none(),
                "{} should be accepted",
                m.name
            );
        }
        let err = model_support_error("gpt-9.9-imaginary").expect("imaginary model is rejected");
        assert!(err.contains("gpt-9.9-imaginary"));
        for m in SUPPORTED_MODELS {
            assert!(
                err.contains(m.name),
                "supported list must mention {}",
                m.name
            );
        }
    }

    #[test]
    fn flagship_has_1m_context_and_server_compaction() {
        let info = lookup(CLAUDE_FABLE);
        assert_eq!(info.context_window, 1_000_000);
        assert!(info.requires_adaptive_thinking);
        assert!(info.supports_server_compaction);
    }

    #[test]
    fn anthropic_adaptive_models_match_their_lookup_flag() {
        for slug in [CLAUDE_FABLE, CLAUDE_OPUS, CLAUDE_SONNET] {
            assert!(
                lookup(slug).requires_adaptive_thinking,
                "{slug} should use adaptive thinking"
            );
        }
        // The fastest model stays on the legacy `budget_tokens` shape.
        assert!(!lookup(CLAUDE_HAIKU).requires_adaptive_thinking);
    }

    #[test]
    fn unknown_model_resolves_to_the_default_model() {
        // Slugs outside the whitelist fall back to the default model
        // entry — `--model` rejects them up front, but internal
        // helpers that pass arbitrary strings should still see real
        // model values rather than a phantom fallback shape.
        let info = lookup("some-future-model-2099");
        assert_eq!(info.name, Model::default().name);
        assert_eq!(info.price_input_per_m, Model::default().price_input_per_m);
    }

    #[test]
    fn gpt_mini_uses_its_own_pricing_not_full_size() {
        let mini = lookup(GPT_MINI);
        let full = lookup(GPT_MID_TIER);
        assert!(
            mini.price_input_per_m < full.price_input_per_m,
            "mini should be cheaper than full"
        );
        // The mini variant should not inherit the full-size premium
        // tier.
        assert!(mini.premium_tier.is_none());
        assert!(full.premium_tier.is_some());
    }

    #[test]
    fn auto_compact_at_clamps_override_against_api_ceiling() {
        let info = Model {
            context_window: 100_000,
            auto_compact_token_limit: Some(200_000),
            ..Model::default()
        };
        assert_eq!(info.auto_compact_at(), 90_000);
    }

    #[test]
    fn auto_compact_at_falls_back_to_90pct_when_unset() {
        let info = Model {
            context_window: 200_000,
            auto_compact_token_limit: None,
            ..Model::default()
        };
        assert_eq!(info.auto_compact_at(), 180_000);
    }

    #[test]
    fn effective_window_reserves_5pct_headroom() {
        let info = Model {
            context_window: 1_000_000,
            ..Model::default()
        };
        assert_eq!(info.effective_window(), 950_000);
    }

    #[test]
    fn cliff_models_compact_below_premium_threshold() {
        for slug in [GPT_FLAGSHIP, GPT_MID_TIER] {
            let info = lookup(slug);
            assert!(info.auto_compact_at() < OPENAI_PREMIUM_INPUT_THRESHOLD);
            let tier = info
                .premium_tier
                .expect("cliff models carry a premium tier");
            assert_eq!(tier.input_threshold, OPENAI_PREMIUM_INPUT_THRESHOLD);
            assert!(tier.price_input_per_m > info.price_input_per_m);
        }
    }

    #[test]
    fn anthropic_adaptive_models_advertise_server_compaction() {
        for slug in [CLAUDE_FABLE, CLAUDE_OPUS, CLAUDE_SONNET] {
            assert!(
                lookup(slug).supports_server_compaction,
                "{slug} should opt into server-side compaction"
            );
        }
    }

    #[test]
    fn fastest_model_does_not_advertise_server_compaction() {
        // The fastest model isn't on Anthropic's compaction-supported
        // list, so the request builder must not send the beta header
        // for it.
        assert!(!lookup(CLAUDE_HAIKU).supports_server_compaction);
    }

    #[test]
    fn effort_support_matches_provider_matrix() {
        use ReasoningEffort::*;

        let supports = |slug: &str, e: ReasoningEffort| effort_support_error(slug, e).is_none();

        // Basic tiers are universal across every supported model.
        for m in SUPPORTED_MODELS {
            for e in [Off, Low, Medium, High] {
                assert!(supports(m.name, e), "{} should accept {e:?}", m.name);
            }
        }

        // `xhigh`: every model except the fastest Anthropic one.
        assert!(supports(CLAUDE_FABLE, XHigh));
        assert!(supports(CLAUDE_OPUS, XHigh));
        assert!(supports(CLAUDE_SONNET, XHigh));
        assert!(supports(GPT_SOL, XHigh));
        assert!(supports(GPT_TERRA, XHigh));
        assert!(supports(GPT_LUNA, XHigh));
        assert!(supports(GPT_FLAGSHIP, XHigh));
        assert!(supports(GPT_MID_TIER, XHigh));
        assert!(supports(GPT_MINI, XHigh));
        assert!(supports(GPT_CODEX, XHigh));
        assert!(!supports(CLAUDE_HAIKU, XHigh));

        // `max`: Anthropic adaptive models plus the newest OpenAI
        // family; the older OpenAI models top out at `xhigh`.
        assert!(supports(CLAUDE_FABLE, Max));
        assert!(supports(CLAUDE_OPUS, Max));
        assert!(supports(CLAUDE_SONNET, Max));
        assert!(supports(GPT_SOL, Max));
        assert!(supports(GPT_TERRA, Max));
        assert!(supports(GPT_LUNA, Max));
        assert!(!supports(CLAUDE_HAIKU, Max));
        assert!(!supports(GPT_FLAGSHIP, Max));
        assert!(!supports(GPT_MINI, Max));
        assert!(!supports(GPT_CODEX, Max));
    }

    #[test]
    fn effort_support_error_lists_supported_levels_for_the_model() {
        let err = effort_support_error(GPT_FLAGSHIP, ReasoningEffort::Max)
            .expect("max on an OpenAI model should be rejected");
        assert!(err.contains(GPT_FLAGSHIP));
        assert!(err.contains("`max`"));
        let listed = err
            .split("Supported levels: ")
            .nth(1)
            .expect("error message lists supported levels");
        for label in ["off", "low", "medium", "high", "xhigh"] {
            assert!(listed.contains(label), "expected {label} in {listed}");
        }
        // This model tops out at `xhigh`, so the supported-list tail
        // must not mention `max`.
        assert!(!listed.contains("max"));

        let err = effort_support_error(CLAUDE_HAIKU, ReasoningEffort::XHigh)
            .expect("xhigh on the fastest Anthropic model should be rejected");
        assert!(err.contains(CLAUDE_HAIKU));
        assert!(err.contains("`xhigh`"));
        // The fastest Anthropic model stops at `high`, so neither of the
        // extra-capability rungs may appear in the supported list.
        let listed = err
            .split("Supported levels: ")
            .nth(1)
            .expect("error message lists supported levels");
        assert!(!listed.contains("xhigh"));
        assert!(!listed.contains("max"));

        // Supported combinations: no error.
        assert!(effort_support_error(CLAUDE_OPUS, ReasoningEffort::Max).is_none());
        assert!(effort_support_error(CLAUDE_SONNET, ReasoningEffort::XHigh).is_none());
        assert!(effort_support_error(GPT_FLAGSHIP, ReasoningEffort::XHigh).is_none());
        assert!(effort_support_error(CLAUDE_HAIKU, ReasoningEffort::High).is_none());
    }
}
