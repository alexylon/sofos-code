//! Per-model metadata. The application only accepts the entries in
//! [`SUPPORTED_MODELS`]. `--model`, the `/model` picker, the request
//! builder, and the cost calculator all read the same table.
//!
//! Each model has a short name without a version, which users type and
//! see, and a versioned API id, which only the provider sees. To move a
//! model to a new version, change its `api_id` and add the old id to
//! [`RETIRED_MODELS`] so sessions and responses that name it still
//! resolve.
//!
//! Adding a model is one struct literal in [`SUPPORTED_MODELS`] (in
//! the order it should appear in the picker). Removing a model is
//! one deletion in the same array.

use crate::api::{ReasoningEffort, ReasoningMode};

/// Multiplier on the base input price for tokens written to a 5-minute
/// Anthropic cache breakpoint. OpenAI has no creation charge. The
/// 1-hour breakpoint bills at 2x, not 1.25x, so the estimate
/// under-reports that anchor.
const CACHE_CREATION_RATE: f64 = 1.25;

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

/// Prices in US dollars per million tokens.
#[derive(Debug, Clone, Copy)]
pub struct Prices {
    /// Input not served from the cache.
    pub input_per_m: f64,
    /// Input served from the cache. Listed per model because the
    /// discount varies.
    pub cache_read_per_m: f64,
    /// Output, including hidden reasoning tokens.
    pub output_per_m: f64,
}

/// Higher prices a model charges once one prompt exceeds
/// `above_tokens`.
#[derive(Debug, Clone, Copy)]
pub struct LongPromptPrices {
    pub above_tokens: u32,
    pub prices: Prices,
}

/// A model the application knows about, with everything needed to
/// talk to its provider and price its usage.
#[derive(Debug, Clone, Copy)]
pub struct Model {
    /// Short name the user types and sees, without a version.
    pub name: &'static str,
    /// Versioned id sent to the provider.
    pub api_id: &'static str,
    /// Short blurb rendered next to the name inside the `/model`
    /// picker.
    pub description: &'static str,
    /// LLM vendor that hosts this model. The picker greys out rows
    /// whose provider doesn't match the running session's client.
    pub provider: Provider,
    /// API context-window ceiling in tokens.
    pub context_window: u32,
    /// Largest `max_tokens` the model accepts on one response. Thinking
    /// counts against it alongside the answer, and a request above it is
    /// rejected by the provider, so startup and `/model` switching check
    /// the configured value against this first.
    pub max_output_tokens: u32,
    /// Cost-shaping override for the auto-compact trigger. When
    /// `Some`, auto-compaction fires at `min(override, 90% of
    /// context_window)`. When `None`, falls back to 90% of
    /// `context_window`.
    pub auto_compact_token_limit: Option<u32>,
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
    /// True for Anthropic models whose safety classifiers can decline a
    /// request. Those models take the server-side `fallbacks` parameter,
    /// which lets Anthropic answer a declined request on another model
    /// instead of returning the refusal.
    pub supports_refusal_fallback: bool,
    /// True for models that accept `reasoning.mode: "pro"`. Startup and
    /// `/mode` reject `pro` on every other model.
    pub supports_pro_mode: bool,
    pub prices: Prices,
    /// Prices for long prompts, on models that charge more for them.
    pub long_prompt: Option<LongPromptPrices>,
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

    /// USD cost of one response at this model's rates.
    ///
    /// `input_tokens` follows the provider's own meaning: OpenAI counts
    /// cached tokens inside it, Anthropic reports only the uncached
    /// ones, so the cached share is subtracted first on OpenAI to reach
    /// the tokens actually billed at the full rate.
    pub fn turn_cost(
        &self,
        input_tokens: u32,
        output_tokens: u32,
        cache_read_tokens: u32,
        cache_creation_tokens: u32,
    ) -> f64 {
        let prompt_tokens = match self.provider {
            Provider::OpenAI => input_tokens,
            Provider::Anthropic => input_tokens
                .saturating_add(cache_read_tokens)
                .saturating_add(cache_creation_tokens),
        };
        let prices = match self.long_prompt {
            Some(long) if prompt_tokens > long.above_tokens => long.prices,
            _ => self.prices,
        };
        self.cost_at(
            prices,
            input_tokens,
            output_tokens,
            cache_read_tokens,
            cache_creation_tokens,
        )
    }

    fn cost_at(
        &self,
        prices: Prices,
        input_tokens: u32,
        output_tokens: u32,
        cache_read_tokens: u32,
        cache_creation_tokens: u32,
    ) -> f64 {
        let uncached = match self.provider {
            Provider::OpenAI => input_tokens.saturating_sub(cache_read_tokens),
            Provider::Anthropic => input_tokens,
        };
        let per_million = |tokens: u32, price: f64| (tokens as f64 / 1_000_000.0) * price;
        per_million(uncached, prices.input_per_m)
            + per_million(cache_read_tokens, prices.cache_read_per_m)
            + per_million(cache_creation_tokens, prices.input_per_m) * CACHE_CREATION_RATE
            + per_million(output_tokens, prices.output_per_m)
    }

    /// Error message if this model doesn't accept `effort`.
    pub fn effort_support_error(&self, effort: ReasoningEffort) -> Option<String> {
        if self.supported_efforts.contains(&effort) {
            return None;
        }
        Some(format!(
            "Model `{}` does not accept reasoning effort `{}`. Supported levels: {}.",
            self.name,
            effort.as_label(),
            self.supported_efforts_label(),
        ))
    }

    /// Comma-separated lowercase labels of every effort level this
    /// model accepts (`"low, medium, high, xhigh"` and so on).
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
    /// The application's default model, also returned by [`lookup`] for
    /// an unknown name.
    fn default() -> Self {
        SUPPORTED_MODELS[DEFAULT_MODEL_INDEX]
    }
}

/// Position of the application-wide default model inside
/// [`SUPPORTED_MODELS`]. Kept as a named index so the array order can
/// be reshuffled without losing track of which entry the rest of the
/// code treats as the default.
const DEFAULT_MODEL_INDEX: usize = 1;

/// Short name of the application's default model.
pub const DEFAULT_MODEL_NAME: &str = SUPPORTED_MODELS[DEFAULT_MODEL_INDEX].name;

/// Compile-time guard: the default-model index must stay inside the
/// array. A reorder that drops below the index (or a refactor that
/// renames the constant out of step with the array) trips here
/// instead of panicking at runtime when the first request hits.
const _: () = assert!(DEFAULT_MODEL_INDEX < SUPPORTED_MODELS.len());

/// Short model names. Code and tests refer to models through these, so
/// renaming one is a one-line change.
pub const CLAUDE_FABLE: &str = "claude-fable";
pub const CLAUDE_OPUS: &str = "claude-opus";
pub const CLAUDE_SONNET: &str = "claude-sonnet";
pub const CLAUDE_HAIKU: &str = "claude-haiku";
pub const GPT_ASTRA: &str = "gpt-astra";
pub const GPT_SOL: &str = "gpt-sol";
pub const GPT_TERRA: &str = "gpt-terra";
pub const GPT_LUNA: &str = "gpt-luna";

/// Names that pick a provider's default model, as in `--model claude`.
const PROVIDER_DEFAULTS: &[(&str, &str)] = &[("claude", CLAUDE_OPUS), ("gpt", GPT_SOL)];

/// Every model the application accepts, in `/model` picker order,
/// strongest first within each provider.
pub const SUPPORTED_MODELS: &[Model] = &[
    Model {
        name: CLAUDE_FABLE,
        api_id: "claude-fable-5-1",
        description: "Anthropic's most capable model - demanding reasoning, 1M context",
        provider: Provider::Anthropic,
        context_window: 1_000_000,
        max_output_tokens: 128_000,
        auto_compact_token_limit: Some(250_000),
        supports_server_compaction: true,
        supports_refusal_fallback: true,
        supported_efforts: &[
            ReasoningEffort::Low,
            ReasoningEffort::Medium,
            ReasoningEffort::High,
            ReasoningEffort::XHigh,
            ReasoningEffort::Max,
        ],
        supports_pro_mode: false,
        prices: Prices {
            input_per_m: 10.0,
            cache_read_per_m: 0.25,
            output_per_m: 50.0,
        },
        long_prompt: None,
    },
    Model {
        name: CLAUDE_OPUS,
        api_id: "claude-opus-5-5",
        description: "Powerful Anthropic reasoning model, 1M context",
        provider: Provider::Anthropic,
        context_window: 1_000_000,
        max_output_tokens: 128_000,
        auto_compact_token_limit: Some(250_000),
        supports_server_compaction: true,
        supports_refusal_fallback: true,
        supported_efforts: &[
            ReasoningEffort::Low,
            ReasoningEffort::Medium,
            ReasoningEffort::High,
            ReasoningEffort::XHigh,
            ReasoningEffort::Max,
        ],
        supports_pro_mode: false,
        prices: Prices {
            input_per_m: 4.0,
            cache_read_per_m: 0.20,
            output_per_m: 20.0,
        },
        long_prompt: None,
    },
    Model {
        name: CLAUDE_SONNET,
        api_id: "claude-sonnet-5-5",
        description: "Balanced Anthropic model for day-to-day coding",
        provider: Provider::Anthropic,
        context_window: 1_000_000,
        max_output_tokens: 128_000,
        auto_compact_token_limit: Some(250_000),
        supports_server_compaction: true,
        supports_refusal_fallback: true,
        supported_efforts: &[
            ReasoningEffort::Low,
            ReasoningEffort::Medium,
            ReasoningEffort::High,
            ReasoningEffort::XHigh,
            ReasoningEffort::Max,
        ],
        supports_pro_mode: false,
        prices: Prices {
            input_per_m: 2.0,
            cache_read_per_m: 0.10,
            output_per_m: 10.0,
        },
        long_prompt: None,
    },
    Model {
        name: CLAUDE_HAIKU,
        api_id: "claude-haiku-5-5",
        description: "Fastest, cheapest Anthropic model - 1M context",
        provider: Provider::Anthropic,
        context_window: 1_000_000,
        max_output_tokens: 128_000,
        // Compacting below 100K keeps most prompts off the long-prompt
        // prices.
        auto_compact_token_limit: Some(80_000),
        supports_server_compaction: true,
        supports_refusal_fallback: false,
        supported_efforts: &[
            ReasoningEffort::Low,
            ReasoningEffort::Medium,
            ReasoningEffort::High,
            ReasoningEffort::XHigh,
            ReasoningEffort::Max,
        ],
        supports_pro_mode: false,
        prices: Prices {
            input_per_m: 0.10,
            cache_read_per_m: 0.01,
            output_per_m: 0.50,
        },
        long_prompt: Some(LongPromptPrices {
            above_tokens: 100_000,
            prices: Prices {
                input_per_m: 0.50,
                cache_read_per_m: 0.05,
                output_per_m: 2.50,
            },
        }),
    },
    Model {
        name: GPT_ASTRA,
        api_id: "gpt-6-astra",
        description: "OpenAI frontier model for complex professional work",
        provider: Provider::OpenAI,
        context_window: 1_050_000,
        max_output_tokens: 128_000,
        auto_compact_token_limit: Some(250_000),
        supports_server_compaction: false,
        supports_refusal_fallback: false,
        supported_efforts: &[
            ReasoningEffort::Low,
            ReasoningEffort::Medium,
            ReasoningEffort::High,
            ReasoningEffort::XHigh,
            ReasoningEffort::Max,
        ],
        supports_pro_mode: true,
        prices: Prices {
            input_per_m: 10.0,
            cache_read_per_m: 1.0,
            output_per_m: 50.0,
        },
        long_prompt: Some(LongPromptPrices {
            above_tokens: 272_000,
            prices: Prices {
                input_per_m: 20.0,
                cache_read_per_m: 2.0,
                output_per_m: 75.0,
            },
        }),
    },
    Model {
        name: GPT_SOL,
        api_id: "gpt-6.1-sol",
        description: "Balanced OpenAI model - intelligence at mid-tier cost",
        provider: Provider::OpenAI,
        context_window: 1_050_000,
        max_output_tokens: 128_000,
        auto_compact_token_limit: Some(250_000),
        supports_server_compaction: false,
        supports_refusal_fallback: false,
        supported_efforts: &[
            ReasoningEffort::Low,
            ReasoningEffort::Medium,
            ReasoningEffort::High,
            ReasoningEffort::XHigh,
            ReasoningEffort::Max,
        ],
        supports_pro_mode: true,
        prices: Prices {
            input_per_m: 2.0,
            cache_read_per_m: 0.10,
            output_per_m: 10.0,
        },
        long_prompt: Some(LongPromptPrices {
            above_tokens: 272_000,
            prices: Prices {
                input_per_m: 4.0,
                cache_read_per_m: 0.20,
                output_per_m: 15.0,
            },
        }),
    },
    Model {
        name: GPT_TERRA,
        api_id: "gpt-5.6-terra",
        description: "Previous-generation mid-tier OpenAI model",
        provider: Provider::OpenAI,
        context_window: 1_050_000,
        max_output_tokens: 128_000,
        auto_compact_token_limit: Some(250_000),
        supports_server_compaction: false,
        supports_refusal_fallback: false,
        supported_efforts: &[
            ReasoningEffort::Low,
            ReasoningEffort::Medium,
            ReasoningEffort::High,
            ReasoningEffort::XHigh,
            ReasoningEffort::Max,
        ],
        supports_pro_mode: true,
        prices: Prices {
            input_per_m: 2.0,
            cache_read_per_m: 0.20,
            output_per_m: 12.0,
        },
        long_prompt: Some(LongPromptPrices {
            above_tokens: 272_000,
            prices: Prices {
                input_per_m: 4.0,
                cache_read_per_m: 0.40,
                output_per_m: 18.0,
            },
        }),
    },
    Model {
        name: GPT_LUNA,
        api_id: "gpt-6-luna",
        description: "OpenAI model optimised for cost-sensitive workloads",
        provider: Provider::OpenAI,
        context_window: 1_050_000,
        max_output_tokens: 128_000,
        auto_compact_token_limit: Some(250_000),
        supports_server_compaction: false,
        supports_refusal_fallback: false,
        supported_efforts: &[
            ReasoningEffort::Low,
            ReasoningEffort::Medium,
            ReasoningEffort::High,
            ReasoningEffort::XHigh,
            ReasoningEffort::Max,
        ],
        supports_pro_mode: true,
        prices: Prices {
            input_per_m: 0.10,
            cache_read_per_m: 0.01,
            output_per_m: 0.50,
        },
        long_prompt: Some(LongPromptPrices {
            above_tokens: 272_000,
            prices: Prices {
                input_per_m: 0.20,
                cache_read_per_m: 0.02,
                output_per_m: 0.75,
            },
        }),
    },
];

/// Comma-separated names `--model` and `/model` accept, in picker
/// order, followed by the provider shortcuts. Shown in their error
/// messages.
pub fn supported_models_label() -> String {
    let names: Vec<&str> = SUPPORTED_MODELS
        .iter()
        .map(|m| m.name)
        .chain(PROVIDER_DEFAULTS.iter().map(|(alias, _)| *alias))
        .collect();
    names.join(", ")
}

/// Versioned API id for a model name, or `name` unchanged when it isn't
/// a supported model.
pub fn api_id_for(name: &str) -> &str {
    canonical_model(name).map_or(name, |m| m.api_id)
}

/// Short name for a model name or API id, or `name` unchanged when it
/// isn't a supported model.
pub fn short_name_for(name: &str) -> &str {
    canonical_model(name).map_or(name, |m| m.name)
}

/// A model name or API id Sofos no longer uses. When a model is
/// dropped, list both its short name and its last API id.
struct Retired {
    id: &'static str,
    /// The model that replaced it. Saved sessions resume on this one.
    successor: &'static str,
    /// Prices Sofos used for it while it was supported.
    prices: Prices,
}

const RETIRED_MODELS: &[Retired] = &[
    Retired {
        id: "claude-fable-5",
        successor: CLAUDE_FABLE,
        prices: Prices {
            input_per_m: 10.0,
            cache_read_per_m: 1.0,
            output_per_m: 50.0,
        },
    },
    Retired {
        id: "claude-opus-5",
        successor: CLAUDE_OPUS,
        prices: Prices {
            input_per_m: 5.0,
            cache_read_per_m: 0.50,
            output_per_m: 25.0,
        },
    },
    Retired {
        id: "claude-sonnet-5",
        successor: CLAUDE_SONNET,
        prices: Prices {
            input_per_m: 3.0,
            cache_read_per_m: 0.30,
            output_per_m: 15.0,
        },
    },
    Retired {
        id: "claude-haiku-4-5",
        successor: CLAUDE_HAIKU,
        prices: Prices {
            input_per_m: 1.0,
            cache_read_per_m: 0.10,
            output_per_m: 5.0,
        },
    },
    Retired {
        id: "gpt-5.6-sol",
        successor: GPT_ASTRA,
        prices: Prices {
            input_per_m: 5.0,
            cache_read_per_m: 0.50,
            output_per_m: 30.0,
        },
    },
    Retired {
        id: "gpt-5.6-luna",
        successor: GPT_LUNA,
        prices: Prices {
            input_per_m: 1.0,
            cache_read_per_m: 0.10,
            output_per_m: 6.0,
        },
    },
];

/// Model a saved session resumes on: the saved model if it is still
/// supported, otherwise its replacement.
pub fn resume_model(name: &str) -> Option<&'static Model> {
    canonical_model(name).or_else(|| successor_of(name))
}

fn retired(name: &str) -> Option<&'static Retired> {
    RETIRED_MODELS
        .iter()
        .find(|r| r.id.eq_ignore_ascii_case(name))
}

/// Replacement for a retired model name.
fn successor_of(name: &str) -> Option<&'static Model> {
    retired(name).and_then(|r| canonical_model(r.successor))
}

/// Old prices for a retired model name, paired with its replacement,
/// which tells the cost code how that provider counts tokens.
fn retired_rates(name: &str) -> Option<(&'static Model, Prices)> {
    let r = retired(name)?;
    Some((canonical_model(r.successor)?, r.prices))
}

/// Cost in US dollars of one response. `served_by` differs from
/// `configured` when Anthropic answers a declined request on another
/// model. A retired model uses its old prices. An unknown model is
/// priced as the configured one, so that turn's estimate is approximate.
pub fn response_cost(
    served_by: &str,
    configured: &str,
    input_tokens: u32,
    output_tokens: u32,
    cache_read_tokens: u32,
    cache_creation_tokens: u32,
) -> f64 {
    if let Some(model) = canonical_model(served_by) {
        return model.turn_cost(
            input_tokens,
            output_tokens,
            cache_read_tokens,
            cache_creation_tokens,
        );
    }
    if let Some((model, prices)) = retired_rates(served_by) {
        return model.cost_at(
            prices,
            input_tokens,
            output_tokens,
            cache_read_tokens,
            cache_creation_tokens,
        );
    }
    lookup(configured).turn_cost(
        input_tokens,
        output_tokens,
        cache_read_tokens,
        cache_creation_tokens,
    )
}

/// Cost of a whole session's token totals under `name`. The totals
/// span many prompts, so long-prompt prices never apply. A retired
/// model uses its old prices.
pub fn session_totals_cost(
    name: &str,
    input_tokens: u32,
    output_tokens: u32,
    cache_read_tokens: u32,
    cache_creation_tokens: u32,
) -> f64 {
    let (model, prices) = retired_rates(name).unwrap_or_else(|| {
        let model = lookup(name);
        (model, model.prices)
    });
    model.cost_at(
        prices,
        input_tokens,
        output_tokens,
        cache_read_tokens,
        cache_creation_tokens,
    )
}

/// Supported model matching `name` by short name or API id, ignoring
/// case. Used internally, where API ids come back from the provider and
/// from saved sessions.
pub fn canonical_model(name: &str) -> Option<&'static Model> {
    SUPPORTED_MODELS
        .iter()
        .find(|m| m.name.eq_ignore_ascii_case(name) || m.api_id.eq_ignore_ascii_case(name))
}

/// Supported model matching a name the user typed: a short name, or
/// `claude` / `gpt` for that provider's default model.
pub fn selectable_model(name: &str) -> Option<&'static Model> {
    SUPPORTED_MODELS
        .iter()
        .find(|m| m.name.eq_ignore_ascii_case(name))
        .or_else(|| {
            PROVIDER_DEFAULTS
                .iter()
                .find(|(alias, _)| alias.eq_ignore_ascii_case(name))
                .map(|(_, model)| lookup(model))
        })
}

/// Error message for a model name the user typed, or `None` when it is
/// accepted. Shared by `--model` and `/model`.
pub fn model_support_error(name: &str) -> Option<String> {
    if selectable_model(name).is_some() {
        return None;
    }
    let reason = match canonical_model(name).or_else(|| successor_of(name)) {
        Some(model) => format!("is not accepted; use `{}`", model.name),
        None => "is not supported".to_string(),
    };
    Some(format!(
        "Model `{}` {}. Available models: {}.",
        name,
        reason,
        supported_models_label()
    ))
}

/// Provider of the model named `name`. An unknown name gets the default
/// model's provider.
pub fn provider_for(name: &str) -> Provider {
    lookup(name).provider
}

/// Model matching `name` by short name or API id, ignoring case. An
/// unknown name gets the default model; user input is checked before it
/// gets here, so only internal callers can hit that case.
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
    lookup(name).effort_support_error(effort)
}

/// Human-readable rejection message when `max_tokens` asks for more
/// output than the model can produce, or `None` when the value fits.
/// Surfaced from the startup validator and from `/model` switching, so
/// an over-large ceiling is reported before it reaches the provider.
pub fn max_tokens_support_error(name: &str, max_tokens: u32) -> Option<String> {
    let info = lookup(name);
    if max_tokens <= info.max_output_tokens {
        return None;
    }
    Some(format!(
        "Model `{}` produces at most {} output tokens, but `--max-tokens` is {}.",
        name, info.max_output_tokens, max_tokens
    ))
}

/// Comma-separated list of every model that accepts `reasoning.mode:
/// "pro"`, in catalog order. Surfaced in [`mode_support_error`] so the
/// user sees which models to switch to.
pub fn pro_capable_models_label() -> String {
    SUPPORTED_MODELS
        .iter()
        .filter(|m| m.supports_pro_mode)
        .map(|m| m.name)
        .collect::<Vec<_>>()
        .join(", ")
}

/// Human-readable rejection message for an unsupported `(model, mode)`
/// pair, or `None` if the pair is supported. `Standard` is accepted by
/// every model; `Pro` only by the models whose `supports_pro_mode` is
/// set. Surfaced from the startup validator, the `/mode` handler, and
/// `/model` switching so the failure mode is the same everywhere.
pub fn mode_support_error(name: &str, mode: ReasoningMode) -> Option<String> {
    if !mode.is_pro() || lookup(name).supports_pro_mode {
        return None;
    }
    Some(format!(
        "Model `{}` does not support pro mode. Pro mode is available on: {}.",
        name,
        pro_capable_models_label(),
    ))
}

/// Whether `name` accepts `reasoning.mode: "pro"`. Thin wrapper over the
/// model-info table so callers do not compare model slugs by hand.
pub fn supports_pro_mode(name: &str) -> bool {
    lookup(name).supports_pro_mode
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Price a response the way the session does: resolve the slug
    /// through the table, then apply that model's rates.
    fn cost_of(model: &str, input: u32, output: u32, cache_read: u32, cache_creation: u32) -> f64 {
        lookup(model).turn_cost(input, output, cache_read, cache_creation)
    }

    fn approx(a: f64, b: f64) {
        assert!(
            (a - b).abs() < 1e-9,
            "expected ≈{}, got {} (delta {})",
            b,
            a,
            (a - b).abs()
        );
    }

    #[test]
    fn openai_cost_uses_full_rate_when_no_cache() {
        // 100k input @ $2/M, 5k output @ $10/M, no cache.
        let cost = cost_of(crate::api::model_info::GPT_SOL, 100_000, 5_000, 0, 0);
        approx(cost, 100_000.0 / 1e6 * 2.0 + 5_000.0 / 1e6 * 10.0);
    }

    #[test]
    fn openai_cost_bills_cache_reads_at_the_cached_rate() {
        let cost = cost_of(crate::api::model_info::GPT_SOL, 100_000, 5_000, 75_000, 0);
        approx(
            cost,
            25_000.0 / 1e6 * 2.0 + 75_000.0 / 1e6 * 0.10 + 5_000.0 / 1e6 * 10.0,
        );
    }

    #[test]
    fn anthropic_cost_input_tokens_already_excludes_cache() {
        let cost = cost_of(
            crate::api::model_info::CLAUDE_OPUS,
            25_000,
            5_000,
            75_000,
            0,
        );
        approx(
            cost,
            25_000.0 / 1e6 * 4.0 + 75_000.0 / 1e6 * 0.20 + 5_000.0 / 1e6 * 20.0,
        );
    }

    #[test]
    fn anthropic_cost_charges_creation_at_125pct() {
        let cost = cost_of(crate::api::model_info::CLAUDE_OPUS, 0, 0, 0, 50_000);
        approx(cost, 50_000.0 / 1e6 * 4.0 * 1.25);
    }

    #[test]
    fn cache_hit_does_not_underflow_when_read_exceeds_input() {
        let cost = cost_of(crate::api::model_info::GPT_SOL, 50_000, 0, 100_000, 0);
        approx(cost, 100_000.0 / 1e6 * 0.10);
    }

    #[test]
    fn fable_bills_cache_reads_at_its_own_rate() {
        let cost = cost_of(crate::api::model_info::CLAUDE_FABLE, 0, 0, 100_000, 0);
        approx(cost, 100_000.0 / 1e6 * 0.25);
    }

    #[test]
    fn anthropic_long_prompt_counts_cached_tokens_toward_the_threshold() {
        // 20k uncached + 90k cached = 110k, past Haiku's 100k cut-off.
        let cost = cost_of(
            crate::api::model_info::CLAUDE_HAIKU,
            20_000,
            1_000,
            90_000,
            0,
        );
        approx(
            cost,
            20_000.0 / 1e6 * 0.50 + 90_000.0 / 1e6 * 0.05 + 1_000.0 / 1e6 * 2.50,
        );

        // Exactly at the cut-off stays on standard prices.
        let cost = cost_of(
            crate::api::model_info::CLAUDE_HAIKU,
            10_000,
            1_000,
            90_000,
            0,
        );
        approx(
            cost,
            10_000.0 / 1e6 * 0.10 + 90_000.0 / 1e6 * 0.01 + 1_000.0 / 1e6 * 0.50,
        );
    }

    #[test]
    fn openai_long_prompt_bills_at_the_higher_rate_card() {
        let cost = cost_of(crate::api::model_info::GPT_SOL, 300_000, 1_000, 0, 0);
        approx(cost, 300_000.0 / 1e6 * 4.0 + 1_000.0 / 1e6 * 15.0);
    }

    #[test]
    fn unknown_model_falls_back_without_panic() {
        // Falls back to the default model's prices ($4 / $20).
        let cost = cost_of("some-future-model", 1_000, 1_000, 0, 0);
        approx(cost, 1_000.0 / 1e6 * 4.0 + 1_000.0 / 1e6 * 20.0);
    }

    #[test]
    fn provider_routes_supported_models_correctly() {
        assert_eq!(provider_for(CLAUDE_FABLE), Provider::Anthropic);
        assert_eq!(provider_for(CLAUDE_OPUS), Provider::Anthropic);
        assert_eq!(provider_for(CLAUDE_SONNET), Provider::Anthropic);
        assert_eq!(provider_for(CLAUDE_HAIKU), Provider::Anthropic);
        assert_eq!(provider_for(GPT_ASTRA), Provider::OpenAI);
        assert_eq!(provider_for(GPT_SOL), Provider::OpenAI);
        assert_eq!(provider_for(GPT_TERRA), Provider::OpenAI);
        assert_eq!(provider_for(GPT_LUNA), Provider::OpenAI);
        // Case insensitivity covers an upper-cased `--model` argument.
        assert_eq!(
            provider_for(&CLAUDE_OPUS.to_uppercase()),
            Provider::Anthropic
        );
        // An unknown name gets the default model's provider.
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
                GPT_ASTRA,
                GPT_SOL,
                GPT_TERRA,
                GPT_LUNA,
            ]
        );
    }

    #[test]
    fn typed_names_accept_short_names_and_provider_defaults_only() {
        assert_eq!(
            selectable_model("Claude-Opus").map(|m| m.name),
            Some(CLAUDE_OPUS)
        );
        assert_eq!(
            selectable_model("claude").map(|m| m.name),
            Some(CLAUDE_OPUS)
        );
        assert_eq!(selectable_model("GPT").map(|m| m.name), Some(GPT_SOL));

        // A versioned API id resolves internally but can't be typed.
        let api_id = lookup(CLAUDE_OPUS).api_id;
        assert_eq!(canonical_model(api_id).map(|m| m.name), Some(CLAUDE_OPUS));
        assert!(selectable_model(api_id).is_none());
        let err = model_support_error(api_id).expect("a versioned id is rejected");
        assert!(err.contains(&format!("use `{CLAUDE_OPUS}`")));
    }

    #[test]
    fn default_model_is_the_cli_default() {
        assert_eq!(Model::default().name, CLAUDE_OPUS);
        assert_eq!(SUPPORTED_MODELS[DEFAULT_MODEL_INDEX].name, CLAUDE_OPUS);
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
        assert!(info.supports_server_compaction);
    }

    #[test]
    fn unknown_model_resolves_to_the_default_model() {
        // Slugs outside the whitelist fall back to the default model
        // entry — `--model` rejects them up front, but internal
        // helpers that pass arbitrary strings should still see real
        // model values rather than a phantom fallback shape.
        let info = lookup("some-future-model-2099");
        assert_eq!(info.name, Model::default().name);
        assert_eq!(info.prices.input_per_m, Model::default().prices.input_per_m);
    }

    #[test]
    fn max_tokens_support_error_accepts_up_to_the_model_ceiling() {
        for m in SUPPORTED_MODELS {
            assert!(
                max_tokens_support_error(m.name, m.max_output_tokens).is_none(),
                "{} should accept its own ceiling",
                m.name
            );
            let err = max_tokens_support_error(m.name, m.max_output_tokens + 1)
                .expect("one token past the ceiling is rejected");
            assert!(err.contains(m.name));
        }
    }

    #[test]
    fn default_max_tokens_fits_every_supported_model() {
        // The CLI ships one default for every model, so the smallest
        // ceiling in the table is what that default has to clear.
        let smallest = SUPPORTED_MODELS
            .iter()
            .map(|m| m.max_output_tokens)
            .min()
            .expect("the table is never empty");
        assert!(
            crate::cli::DEFAULT_MAX_TOKENS <= smallest,
            "default --max-tokens ({}) exceeds the smallest model ceiling ({smallest})",
            crate::cli::DEFAULT_MAX_TOKENS
        );
    }

    #[test]
    fn openai_tiers_are_priced_cheapest_last() {
        let astra = lookup(GPT_ASTRA);
        let sol = lookup(GPT_SOL);
        let terra = lookup(GPT_TERRA);
        let luna = lookup(GPT_LUNA);
        assert!(luna.prices.input_per_m < sol.prices.input_per_m);
        assert!(luna.prices.input_per_m < terra.prices.input_per_m);
        assert!(sol.prices.input_per_m < astra.prices.input_per_m);
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
    fn anthropic_adaptive_models_advertise_server_compaction() {
        for slug in [CLAUDE_FABLE, CLAUDE_OPUS, CLAUDE_SONNET, CLAUDE_HAIKU] {
            assert!(
                lookup(slug).supports_server_compaction,
                "{slug} should opt into server-side compaction"
            );
        }
    }

    #[test]
    fn effort_support_matches_provider_matrix() {
        use ReasoningEffort::*;

        for m in SUPPORTED_MODELS {
            for e in [Low, Medium, High, XHigh, Max] {
                assert!(
                    effort_support_error(m.name, e).is_none(),
                    "{} should accept {e:?}",
                    m.name
                );
            }
        }
    }

    #[test]
    fn response_cost_prices_the_model_that_answered() {
        // A retired model uses its old prices.
        approx(
            response_cost("claude-opus-5", CLAUDE_SONNET, 1_000_000, 0, 0, 0),
            5.0,
        );
        // The provider reports the API id, not the short name.
        approx(
            response_cost(
                lookup(CLAUDE_OPUS).api_id,
                CLAUDE_SONNET,
                1_000_000,
                0,
                0,
                0,
            ),
            4.0,
        );
        // An unknown model is priced as the configured one.
        approx(
            response_cost("claude-unknown", CLAUDE_SONNET, 1_000_000, 0, 0, 0),
            2.0,
        );
        approx(
            session_totals_cost("claude-haiku-4-5", 1_000_000, 0, 0, 0),
            1.0,
        );
    }

    #[test]
    fn resume_model_maps_retired_slugs_to_their_successors() {
        assert_eq!(resume_model(CLAUDE_OPUS).map(|m| m.name), Some(CLAUDE_OPUS));
        assert!(resume_model("claude-opus-4-8").is_none());
        for Retired {
            id: retired,
            successor,
            ..
        } in RETIRED_MODELS
        {
            assert_eq!(
                resume_model(&retired.to_uppercase()).map(|m| m.name),
                Some(*successor)
            );
            let err = model_support_error(retired).expect("a retired id is not selectable");
            assert!(err.contains(&format!("use `{successor}`")));
            assert!(
                canonical_model(retired).is_none(),
                "{retired} is still supported"
            );
            assert!(
                canonical_model(successor).is_some(),
                "{retired} must map to a supported model"
            );
        }
    }

    #[test]
    fn effort_support_error_lists_supported_levels_for_the_model() {
        let capped = Model {
            supported_efforts: &[
                ReasoningEffort::Low,
                ReasoningEffort::Medium,
                ReasoningEffort::High,
            ],
            ..*lookup(CLAUDE_HAIKU)
        };
        let err = capped
            .effort_support_error(ReasoningEffort::Max)
            .expect("effort above the model's ceiling should be rejected");
        assert!(err.contains(CLAUDE_HAIKU));
        assert!(err.contains("`max`"));
        assert!(err.ends_with("Supported levels: low, medium, high."));
        assert!(capped.effort_support_error(ReasoningEffort::High).is_none());
    }

    #[test]
    fn pro_mode_only_supported_on_openai_models() {
        use ReasoningMode::{Pro, Standard};
        // Standard is accepted by every model.
        for m in SUPPORTED_MODELS {
            assert!(mode_support_error(m.name, Standard).is_none());
        }
        // Pro on the OpenAI models: accepted.
        assert!(mode_support_error(GPT_ASTRA, Pro).is_none());
        assert!(mode_support_error(GPT_SOL, Pro).is_none());
        assert!(mode_support_error(GPT_TERRA, Pro).is_none());
        assert!(mode_support_error(GPT_LUNA, Pro).is_none());
        // Pro elsewhere: rejected, and the message names the capable models.
        let err = mode_support_error(CLAUDE_OPUS, Pro)
            .expect("pro on an Anthropic model should be rejected");
        assert!(err.contains(CLAUDE_OPUS));
        assert!(err.contains(GPT_SOL));
        // The capability helper agrees with the table.
        assert!(supports_pro_mode(GPT_SOL));
        assert!(!supports_pro_mode(CLAUDE_OPUS));
    }
}
