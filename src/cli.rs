use crate::error::SofosError;
use clap::Parser;

/// Default for the deprecated `--thinking-budget` flag. Kept as a named
/// const so `main.rs` can warn when the user supplies a value that
/// differs from this — anything else means the user expected the flag
/// to do something it no longer does. Removable when the flag itself
/// goes.
pub const THINKING_BUDGET_DEFAULT: u32 = 5120;

/// Default output-token limit. It serves every model, so it must stay
/// at or below the smallest model limit; a test enforces this.
pub const DEFAULT_MAX_TOKENS: u32 = 64_000;

/// Smallest `--max-tokens` accepted. It is OpenAI's minimum, applied to
/// both providers so the flag behaves the same everywhere.
pub const MIN_MAX_TOKENS: u32 = 16;

#[derive(Parser, Debug)]
#[command(
    name = "sofos",
    about = "An interactive AI coding assistant powered by Claude or OpenAI",
    long_about = "Sofos is an AI-powered coding assistant (Claude / OpenAI) that can help you write code, edit files, and search the web.",
    version
)]
pub struct Cli {
    #[arg(long, env = "ANTHROPIC_API_KEY")]
    pub api_key: Option<String>,

    #[arg(long, env = "OPENAI_API_KEY")]
    pub openai_api_key: Option<String>,

    #[arg(long, env = "MORPH_API_KEY")]
    pub morph_api_key: Option<String>,

    /// Initial prompt to send (if not provided, starts interactive REPL)
    #[arg(short, long)]
    pub prompt: Option<String>,

    /// Resume a previous conversation session
    #[arg(short, long)]
    pub resume: bool,

    /// Check API connectivity and exit
    #[arg(long)]
    pub check_connection: bool,

    #[arg(long, default_value = crate::api::model_info::DEFAULT_MODEL_NAME)]
    pub model: String,

    #[arg(long, default_value = "morph-v3-fast")]
    pub morph_model: String,

    /// Maximum output tokens per response. Thinking counts toward this
    /// limit alongside the answer. Must be at least [`MIN_MAX_TOKENS`]
    /// and within the chosen model's own limit.
    #[arg(
        long,
        default_value_t = DEFAULT_MAX_TOKENS,
        value_parser = clap::value_parser!(u32).range(MIN_MAX_TOKENS as i64..)
    )]
    pub max_tokens: u32,

    /// Reasoning effort: low, medium, high, xhigh, max. Default
    /// `medium`. Sofos refuses to start if the model doesn't accept the
    /// chosen level.
    //
    // Parsed as a raw `String` here, then validated and converted in
    // `main`, so the per-model rejection and the parse-failure
    // rejection both render in the same hand-rolled clap-style
    // format. `ValueEnum` would auto-emit a `[possible values: ...]`
    // line that always listed all five levels, which is misleading on
    // models that only accept a subset.
    #[arg(short = 'e', long, default_value = "medium")]
    pub reasoning_effort: String,

    /// Reasoning mode: standard or pro. Default `standard`. `pro` asks
    /// the OpenAI models to do extra work before answering, for better
    /// quality at higher latency and cost. Other models reject it at
    /// startup. Independent of `--reasoning-effort`.
    //
    // Parsed as a raw `String` here, then validated and converted in
    // `main`, mirroring `--reasoning-effort`.
    #[arg(long, default_value = "standard")]
    pub reasoning_mode: String,

    /// Deprecated and has no effect; use `--reasoning-effort` instead.
    /// Still parsed so older scripts don't break, with a warning when a
    /// non-default value is passed. Hidden from `--help`.
    #[arg(long, default_value_t = THINKING_BUDGET_DEFAULT, hide = true)]
    pub thinking_budget: u32,

    /// Start in read-only mode: only inspection tools, no writes or shell.
    #[arg(long)]
    pub readonly: bool,

    /// Start with the operating-system sandbox off: shell commands run
    /// without confinement. Overridden by `--readonly` when both are given.
    #[arg(long)]
    pub no_sandbox: bool,
}

impl Cli {
    pub fn get_anthropic_api_key(&self) -> Result<String, SofosError> {
        self.api_key
            .clone()
            .ok_or_else(|| SofosError::Config("ANTHROPIC_API_KEY not found".to_string()))
    }

    pub fn get_openai_api_key(&self) -> Result<String, SofosError> {
        self.openai_api_key
            .clone()
            .ok_or_else(|| SofosError::Config("OPENAI_API_KEY not found".to_string()))
    }
}
