mod api;
mod cli;
mod commands;
mod config;
mod conversation;
mod diff;
mod error;
mod error_ext;
mod history;
mod model_config;
mod prompt;
mod repl;
mod request_builder;
mod response_handler;
mod session_selector;
mod session_state;
mod syntax;
mod tools;
mod ui;

use api::{AnthropicClient, LlmClient, MorphClient, OpenAIClient};
use clap::Parser;
use cli::Cli;
use colored::Colorize;
use error::Result;
use history::HistoryManager;
use repl::{Repl, ReplConfig};
use std::env;

fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::from_default_env()
                .add_directive(tracing::Level::WARN.into()),
        )
        .init();

    let cli = Cli::parse();

    let is_openai_model = cli.model.starts_with("gpt-");

    let client = if is_openai_model {
        match cli.get_openai_api_key() {
            Ok(key) => match OpenAIClient::new(key) {
                Ok(c) => LlmClient::OpenAI(c),
                Err(e) => {
                    eprintln!("{} {}", "Error:".bright_red().bold(), e);
                    std::process::exit(1);
                }
            },
            Err(e) => {
                eprintln!("{} {}", "Error:".bright_red().bold(), e);
                eprintln!();
                eprintln!("Please set your OpenAI API key:");
                eprintln!("  export OPENAI_API_KEY='your-api-key'");
                eprintln!("Or use the --openai-api-key flag:");
                eprintln!("  sofos --openai-api-key 'your-api-key' --model gpt-5.1-codex-max");
                std::process::exit(1);
            }
        }
    } else {
        match cli.get_anthropic_api_key() {
            Ok(key) => match AnthropicClient::new(key) {
                Ok(c) => LlmClient::Anthropic(c),
                Err(e) => {
                    eprintln!("{} {}", "Error:".bright_red().bold(), e);
                    std::process::exit(1);
                }
            },
            Err(e) => {
                eprintln!("{} {}", "Error:".bright_red().bold(), e);
                eprintln!();
                eprintln!("Please set your Anthropic API key:");
                eprintln!("  export ANTHROPIC_API_KEY='your-api-key'");
                eprintln!("Or use the --api-key flag:");
                eprintln!("  sofos --api-key 'your-api-key'");
                std::process::exit(1);
            }
        }
    };

    if cli.check_connection {
        return check_api_connectivity(&client);
    }

    let workspace = env::current_dir().map_err(|e| {
        error::SofosError::Config(format!("Failed to get current directory: {}", e))
    })?;

    println!(
        "{} {}",
        "Workspace:".bright_cyan(),
        workspace.display().to_string().dimmed()
    );

    println!("{} {}", "Model:".bright_green(), cli.model);

    let morph_client = cli.morph_api_key.as_ref().and_then(|key| {
        match MorphClient::new(key.clone(), Some(cli.morph_model.clone())) {
            Ok(client) => {
                println!("{}", "Morph Fast Apply: Enabled".bright_green());
                Some(client)
            }
            Err(e) => {
                eprintln!(
                    "{} Failed to initialize Morph client: {}",
                    "Warning:".bright_yellow(),
                    e
                );
                None
            }
        }
    });

    println!();

    let config = ReplConfig::new(
        cli.model,
        cli.max_tokens,
        cli.enable_thinking,
        cli.thinking_budget,
        cli.safe_mode,
    );

    let mut repl = Repl::new(client, config, workspace.clone(), morph_client)?;

    if cli.resume {
        let history_manager = HistoryManager::new(workspace)?;
        let sessions = history_manager.list_sessions()?;

        if let Some(session_id) = session_selector::select_session(sessions)? {
            repl.load_session_by_id(&session_id)?;
            println!();
        }
    }

    if let Some(prompt) = cli.prompt {
        repl.process_single_prompt(&prompt)?;
    } else {
        repl.run()?;
    }

    Ok(())
}

fn check_api_connectivity(client: &LlmClient) -> Result<()> {
    let provider = client.provider_name();
    println!("Checking {} API connectivity...", provider.bright_cyan());

    let runtime = tokio::runtime::Runtime::new()
        .map_err(|e| error::SofosError::Config(format!("Failed to create async runtime: {}", e)))?;

    match runtime.block_on(client.check_connectivity()) {
        Ok(()) => {
            println!(
                "{} {} API is reachable",
                "✓".bright_green().bold(),
                provider
            );
            Ok(())
        }
        Err(e) => {
            eprintln!("{} {}", "✗".bright_red().bold(), e);
            std::process::exit(1);
        }
    }
}
