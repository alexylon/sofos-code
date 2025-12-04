use clap::Parser;

#[derive(Parser, Debug)]
#[command(
    name = "sofos",
    about = "An interactive AI coding assistant powered by Claude",
    long_about = "Sofos is an AI-powered coding assistant that can help you write code, edit files, and search the web. All file operations are sandboxed to the current working directory for security.",
    version
)]
pub struct Cli {
    #[arg(long, env = "ANTHROPIC_API_KEY")]
    pub api_key: Option<String>,

    /// Initial prompt to send (if not provided, starts interactive REPL)
    #[arg(short, long)]
    pub prompt: Option<String>,

    #[arg(long, default_value = "claude-sonnet-4-5")]
    pub model: String,

    #[arg(long, default_value = "4096")]
    pub max_tokens: u32,

    #[arg(short, long)]
    pub verbose: bool,
}

impl Cli {
    pub fn get_api_key(&self) -> Result<String, String> {
        self.api_key
            .clone()
            .ok_or_else(|| "ANTHROPIC_API_KEY not found. Please set it as an environment variable or use --api-key".to_string())
    }
}
