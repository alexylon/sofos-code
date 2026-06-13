use crate::error::SofosError;
use clap::Parser;

/// Default for the deprecated `--thinking-budget` flag. Kept as a named
/// const so `main.rs` can warn when the user supplies a value that
/// differs from this — anything else means the user expected the flag
/// to do something it no longer does. Removable when the flag itself
/// goes.
pub const THINKING_BUDGET_DEFAULT: u32 = 5120;

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

    /// Maximum output tokens per API response. 8192 is too low for
    /// modern frontier models writing long files — a `write_file` call
    /// with multi-KB content hits this limit mid-stream and truncates
    /// the tool-call JSON, surfacing as "Missing 'path' parameter".
    /// Modern frontier models support 32k+; smaller models cap at
    /// their own limit so this is safe as a default.
    /// Must be > 16384 when reasoning effort is enabled (the legacy
    /// Anthropic thinking-budget ceiling); the default 32768 satisfies it.
    #[arg(long, default_value = "32768")]
    pub max_tokens: u32,

    /// Reasoning effort: off, low, medium, high, xhigh, max. Default
    /// `medium`. `Off` skips reasoning entirely on OpenAI (effort=
    /// minimal, summary suppressed) and disables Anthropic extended
    /// thinking on non-adaptive models. Anthropic adaptive models
    /// collapse `Off` to the lowest accepted level (`low`). `xhigh` is
    /// accepted by the larger Anthropic models and the OpenAI reasoning
    /// models only; `max` is accepted by Anthropic adaptive models
    /// only. Sofos refuses to start with an unsupported
    /// `(model, effort)` pair.
    //
    // Parsed as a raw `String` here, then validated and converted in
    // `main`, so the per-model rejection and the parse-failure
    // rejection both render in the same hand-rolled clap-style
    // format. `ValueEnum` would auto-emit a `[possible values: ...]`
    // line that always listed all six levels, which is misleading on
    // models that only accept a subset.
    #[arg(short = 'e', long, default_value = "medium")]
    pub reasoning_effort: String,

    /// Deprecated. The flag has no effect on any path: legacy Anthropic
    /// uses a fixed per-tier budget (Low=1024, Medium=5120, High=16384),
    /// adaptive Anthropic uses `output_config.effort`, and
    /// OpenAI uses `reasoning.effort`. The flag still parses so older
    /// scripts don't break; `main.rs` warns at startup when a non-default
    /// value is supplied. Hidden from `--help`. Will be removed in a
    /// future release. Use `--reasoning-effort` to control thinking depth.
    #[arg(long, default_value_t = THINKING_BUDGET_DEFAULT, hide = true)]
    pub thinking_budget: u32,

    /// Enable read-only mode
    #[arg(short, long)]
    pub safe_mode: bool,

    /// Run shell commands without operating-system confinement (full
    /// access). Overridden by `--safe-mode` when both are given.
    #[arg(long)]
    pub unrestricted: bool,
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
