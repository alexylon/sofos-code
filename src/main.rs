mod api;
mod cli;
mod conversation;
mod diff;
mod error;
mod history;
mod repl;
mod session_selector;
mod syntax;
mod tools;

use api::MorphClient;
use clap::Parser;
use cli::Cli;
use colored::Colorize;
use error::Result;
use history::HistoryManager;
use repl::Repl;
use std::env;

fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::from_default_env()
                .add_directive(tracing::Level::WARN.into()),
        )
        .init();

    let cli = Cli::parse();

    let api_key = match cli.get_api_key() {
        Ok(key) => key,
        Err(e) => {
            eprintln!("{} {}", "Error:".bright_red().bold(), e);
            eprintln!();
            eprintln!("Please set your Anthropic API key:");
            eprintln!("  export ANTHROPIC_API_KEY='your-api-key'");
            eprintln!("Or use the --api-key flag:");
            eprintln!("  sofos --api-key 'your-api-key'");
            std::process::exit(1);
        }
    };

    let workspace = env::current_dir().map_err(|e| {
        error::SofosError::Config(format!("Failed to get current directory: {}", e))
    })?;

    println!(
        "{} {}",
        "Workspace:".bright_cyan(),
        workspace.display().to_string().dimmed()
    );

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

    let mut repl = Repl::new(api_key, cli.model, cli.max_tokens, workspace.clone(), morph_client)?;

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
