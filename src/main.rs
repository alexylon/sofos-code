mod api;
mod cli;
mod clipboard;
mod commands;
mod config;
mod error;
mod error_ext;
mod mcp;
mod repl;
mod session;
mod tools;
mod ui;

use api::{AnthropicClient, LlmClient, MorphClient, OpenAIClient};
use clap::Parser;
use cli::Cli;
use colored::Colorize;
use error::Result;
use repl::{Repl, ReplConfig};
use session::HistoryManager;
use std::env;
use ui::UI;

fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::from_default_env()
                .add_directive(tracing::Level::WARN.into()),
        )
        .without_time()
        .init();

    let cli = Cli::parse();

    // Historically the logo printed here, up front. It's now deferred:
    // in interactive mode the banner text is collected into
    // `startup_banner` below and replayed through the TUI's capture
    // pipe after `OutputCapture` is installed, so the inline viewport
    // can't paint over it on terminals whose cursor-position DSR
    // doesn't answer (e.g. Ghostty). One-shot prompt mode and
    // connectivity checks skip the banner entirely to keep piped
    // output clean — matching the original behaviour.

    let is_openai_model = cli.model.starts_with("gpt-");

    let client = if is_openai_model {
        match cli.get_openai_api_key() {
            Ok(key) => match OpenAIClient::new(key) {
                Ok(c) => LlmClient::OpenAI(c),
                Err(e) => {
                    UI::print_error_with_hint(&e);
                    std::process::exit(1);
                }
            },
            Err(e) => {
                UI::print_error_with_hint(&e);
                std::process::exit(1);
            }
        }
    } else {
        match cli.get_anthropic_api_key() {
            Ok(key) => match AnthropicClient::new(key) {
                Ok(c) => LlmClient::Anthropic(c),
                Err(e) => {
                    UI::print_error_with_hint(&e);
                    std::process::exit(1);
                }
            },
            Err(e) => {
                UI::print_error_with_hint(&e);
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

    // Collect the startup lines (logo + workspace/model/reasoning/morph)
    // into one string rather than `println!`-ing them. In interactive
    // mode we hand the buffer to `Repl` so the TUI can replay it above
    // its inline viewport; in one-shot mode we still print it directly.
    // The indirection is what keeps the banner visible on terminals
    // whose cursor-position DSR doesn't answer (Ghostty) — otherwise
    // our `(0, 0)` fallback placed the viewport on top of the banner.
    let interactive_mode = cli.prompt.is_none() && !cli.check_connection;
    let mut startup_banner = String::new();
    if interactive_mode {
        startup_banner.push_str(&UI::banner_text());
    }
    startup_banner.push_str(&format!(
        "{} {}\n",
        "Workspace:".bright_cyan(),
        workspace.display().to_string().dimmed()
    ));
    startup_banner.push_str(&format!("{} {}\n", "Model:".bright_green(), cli.model));

    if matches!(client, LlmClient::OpenAI(_)) {
        startup_banner.push_str(&format!(
            "{} {}\n",
            "Reasoning effort:".bright_green(),
            crate::api::anthropic::effort_label(cli.enable_thinking)
        ));
    } else if crate::api::anthropic::requires_adaptive_thinking(&cli.model) {
        // Opus 4.7 picks its own budget; advertising a token count would be
        // a lie. Surface the `output_config.effort` we actually send.
        startup_banner.push_str(&format!(
            "{} {}\n",
            "Adaptive thinking effort:".bright_green(),
            crate::api::anthropic::effort_label(cli.enable_thinking)
        ));
    } else if cli.enable_thinking {
        startup_banner.push_str(&format!(
            "{} (budget: {} tokens)\n",
            "Extended thinking: enabled".bright_green(),
            cli.thinking_budget
        ));
    }

    let morph_client = cli.morph_api_key.as_ref().and_then(|key| {
        match MorphClient::new(key.clone(), Some(cli.morph_model.clone())) {
            Ok(client) => {
                startup_banner
                    .push_str(&format!("{}\n", "Morph Fast Apply: Enabled".bright_green()));
                Some(client)
            }
            Err(e) => {
                UI::print_warning(&format!("Failed to initialize Morph client: {}", e));
                None
            }
        }
    });

    startup_banner.push('\n');

    if !interactive_mode {
        print!("{}", startup_banner);
    }

    let config = ReplConfig::new(
        cli.model,
        cli.max_tokens,
        cli.enable_thinking,
        cli.thinking_budget,
        cli.safe_mode,
    );

    let mut repl = Repl::new(client, config, workspace.clone(), morph_client)?;
    if interactive_mode {
        repl.set_startup_banner(startup_banner);
    }

    if cli.resume {
        let history_manager = HistoryManager::new(workspace)?;
        let sessions = history_manager.list_sessions()?;

        if let Some(session_id) = session::select_session(sessions)? {
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
            UI::print_error_with_hint(&e);
            std::process::exit(1);
        }
    }
}
