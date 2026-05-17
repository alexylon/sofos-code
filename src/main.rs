mod api;
mod cli;
mod clipboard;
mod commands;
mod config;
mod error;
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

    if cli.thinking_budget != cli::THINKING_BUDGET_DEFAULT {
        tracing::warn!(
            "--thinking-budget is deprecated and has no effect on any provider path. \
             Use --reasoning-effort to control thinking depth. The flag will be removed in a future release."
        );
    }

    // Reject `--model` values outside the supported whitelist before
    // anything else looks at the slug, and normalise the case to the
    // canonical form so internal state and the provider wire payload
    // never carry a `Claude-Opus-4-7`-style mixed-case spelling.
    let mut cli = cli;
    match crate::api::model_info::canonical_model(&cli.model) {
        Some(choice) => cli.model = choice.name.to_string(),
        None => {
            let reason = crate::api::model_info::model_support_error(&cli.model)
                .unwrap_or_else(|| format!("Model `{}` is not supported.", cli.model));
            eprintln!("{} {}", "error:".bright_red().bold(), reason);
            eprintln!(
                "  [supported models: {}]",
                crate::api::model_info::supported_models_label()
            );
            std::process::exit(2);
        }
    }

    // Parse `--reasoning-effort` ourselves so the parse failure and
    // the per-model rejection both render in the same clap-style
    // envelope. The supported-values list is per-model, which clap's
    // `ValueEnum` derive can't produce.
    let model_info = crate::api::model_info::lookup(&cli.model);
    let bail = |reason: String| -> ! {
        eprintln!("{} {}", "error:".bright_red().bold(), reason);
        eprintln!(
            "  [supported values: {}]",
            model_info.supported_efforts_label()
        );
        std::process::exit(2);
    };
    let reasoning_effort = match crate::api::ReasoningEffort::parse(&cli.reasoning_effort) {
        Some(e) if model_info.supported_efforts.contains(&e) => e,
        Some(_) => bail(format!(
            "reasoning effort '{}' is not supported on model '{}'",
            cli.reasoning_effort, cli.model
        )),
        None => bail(format!(
            "invalid reasoning effort '{}' for model '{}'",
            cli.reasoning_effort, cli.model
        )),
    };

    // Historically the logo printed here, up front. It's now deferred:
    // in interactive mode the banner text is collected into
    // `startup_banner` below and replayed through the TUI's capture
    // pipe after `OutputCapture` is installed, so the inline viewport
    // can't paint over it on terminals whose cursor-position DSR
    // doesn't answer (e.g. Ghostty). One-shot prompt mode and
    // connectivity checks skip the banner entirely to keep piped
    // output clean — matching the original behaviour.

    let client = build_llm_client(&cli);

    if cli.check_connection {
        // `--prompt` is silently ignored when paired with
        // `--check-connection` because the connectivity check exits
        // before any prompt processing runs. Surface that explicitly
        // so scripts that combine the two don't quietly drop their
        // input.
        if cli.prompt.is_some() {
            tracing::warn!(
                "--prompt is ignored when --check-connection is set; \
                 only the connectivity check will run."
            );
        }
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
            reasoning_effort.as_label()
        ));
    } else if crate::api::anthropic::requires_adaptive_thinking(&cli.model) {
        // Adaptive-thinking models (Opus 4.7, Sonnet 4.6) pick
        // their own budget; advertising a token count would be a lie.
        // Surface the `output_config.effort` we actually send.
        startup_banner.push_str(&format!(
            "{} {}\n",
            "Adaptive thinking effort:".bright_green(),
            crate::api::anthropic::effort_label(reasoning_effort)
        ));
    } else if reasoning_effort.is_enabled() {
        // Show the per-effort tier budget so the startup banner matches
        // what hits the API.
        let budget = crate::api::anthropic::legacy_thinking_budget(reasoning_effort);
        startup_banner.push_str(&format!(
            "{} (budget: {} tokens)\n",
            "Extended thinking: enabled".bright_green(),
            budget
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

    if !interactive_mode {
        print!("{}", startup_banner);
    }

    let config = ReplConfig::new(cli.model, cli.max_tokens, reasoning_effort, cli.safe_mode);

    let mut repl = Repl::new(client, config, workspace.clone(), morph_client).unwrap_or_else(|e| {
        UI::print_error_with_hint(&e);
        std::process::exit(1)
    });
    // MCP block sits flush below the workspace/model labels (no blank
    // line in between), then a single trailing newline separates the
    // banner from the welcome (interactive) or the next CLI output
    // (one-shot). When there are no servers the trailing `\n` alone
    // gives the same blank-line separator the old banner had.
    let mcp_section = format!("{}\n", repl.take_mcp_init_lines());
    if interactive_mode {
        startup_banner.push_str(&mcp_section);
        repl.set_startup_banner(startup_banner);
    } else {
        print!("{}", mcp_section);
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

/// Construct the LLM client matching `cli.model`. Both the API-key
/// fetch and the client constructor exit the process via
/// `UI::print_error_with_hint` on failure — funnelled through one
/// `unwrap_or_else` at the bottom so the startup-error UX stays in
/// sync across all four failure modes.
fn build_llm_client(cli: &Cli) -> LlmClient {
    fn try_build(cli: &Cli) -> Result<LlmClient> {
        match crate::api::model_info::provider_for(&cli.model) {
            crate::api::model_info::Provider::OpenAI => {
                let key = cli.get_openai_api_key()?;
                Ok(LlmClient::OpenAI(OpenAIClient::new(key)?))
            }
            crate::api::model_info::Provider::Anthropic => {
                let key = cli.get_anthropic_api_key()?;
                Ok(LlmClient::Anthropic(AnthropicClient::new(key)?))
            }
        }
    }
    try_build(cli).unwrap_or_else(|e| {
        UI::print_error_with_hint(&e);
        std::process::exit(1)
    })
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
