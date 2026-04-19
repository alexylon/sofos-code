use crate::error::SofosError;
use clap::Parser;

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

    #[arg(long, default_value = "claude-sonnet-4-6")]
    pub model: String,

    #[arg(long, default_value = "morph-v3-fast")]
    pub morph_model: String,

    /// Maximum output tokens per API response. 8192 is too low for
    /// modern frontier models writing long files — a `write_file` call
    /// with multi-KB content hits this limit mid-stream and truncates
    /// the tool-call JSON, surfacing as "Missing 'path' parameter".
    /// Claude Sonnet 4 and GPT-4.1 both support 32k+; smaller models
    /// cap at their own limit so this is safe as a default.
    #[arg(long, default_value = "32768")]
    pub max_tokens: u32,

    /// Enable extended thinking for complex reasoning tasks
    #[arg(short = 't', long)]
    pub enable_thinking: bool,

    /// Token budget for thinking (must be less than max_tokens)
    #[arg(long, default_value = "5120")]
    pub thinking_budget: u32,

    #[arg(short, long)]
    pub verbose: bool,

    /// Enable read-only mode
    #[arg(short, long)]
    pub safe_mode: bool,
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
