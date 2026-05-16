pub mod compaction;
pub mod conversation;
mod request_builder;
mod response_handler;
pub mod sessions;
pub mod tui;
pub mod turn;

pub use conversation::ConversationHistory;
pub use request_builder::RequestBuilder;
pub use response_handler::ResponseHandler;

use std::io::IsTerminal;

use crate::api::LlmClient::Anthropic;
use crate::api::{CreateMessageRequest, LlmClient, MorphClient};
use crate::config::{ModelConfig, NORMAL_MODE_MESSAGE, SAFE_MODE_MESSAGE};
use crate::error::{Result, SofosError};
use crate::mcp::McpManager;
use crate::session::{HistoryManager, SessionState};
use crate::tools::ToolExecutor;
use crate::tools::image::ImageLoader;
use crate::ui::{UI, set_safe_mode_cursor_style};
use colored::Colorize;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::time::sleep;

/// Shared buffer used by the TUI to inject user messages mid-turn. The UI
/// thread pushes text onto this vec when the worker is busy; the tool loop
/// in [`ResponseHandler`] drains it between tool-call iterations and merges
/// the accumulated text into the user turn that carries the tool results.
pub type SteerBuffer = Arc<Mutex<Vec<String>>>;

pub struct ReplConfig {
    pub model: String,
    pub max_tokens: u32,
    pub reasoning_effort: crate::api::ReasoningEffort,
    pub safe_mode: bool,
}

impl ReplConfig {
    pub fn new(
        model: String,
        max_tokens: u32,
        reasoning_effort: crate::api::ReasoningEffort,
        safe_mode: bool,
    ) -> Self {
        Self {
            model,
            max_tokens,
            reasoning_effort,
            safe_mode,
        }
    }
}

pub struct Repl {
    pub(super) client: LlmClient,
    pub(super) tool_executor: ToolExecutor,
    pub(super) history_manager: HistoryManager,
    pub(super) image_loader: ImageLoader,
    pub(super) ui: UI,
    pub(super) model_config: ModelConfig,
    pub(super) session_state: SessionState,
    pub(super) safe_mode: bool,
    pub(super) available_tools: Vec<crate::api::Tool>,
    /// Interrupt flag shared with the TUI. Set to `true` when the user presses
    /// ESC/Ctrl+C during an AI turn; checked by the API request loop.
    pub(super) interrupt_flag: Arc<AtomicBool>,
    /// Shared buffer of pending steering messages the user typed while a
    /// turn was already running. Drained by the tool loop between
    /// iterations so the user can redirect in-flight work without having
    /// to interrupt it.
    pub(super) steer_buffer: SteerBuffer,
    /// Queued through the TUI's captured-stdout pipe so the banner
    /// survives terminals whose cursor-position DSR doesn't answer
    /// (e.g. Ghostty), where the fallback origin would let the viewport
    /// overwrite the lines.
    pub(super) startup_banner: String,
    /// Per-server "✓ MCP server 'X' initialized (N tools)" lines built
    /// during [`Self::new`]. Held separately from `startup_banner` so
    /// `main.rs` can splice them in after its own workspace/model
    /// header before handing the combined banner back for the TUI to
    /// replay through its capture pipe.
    pub(super) mcp_init_lines: String,
    /// Shared tokio runtime driving every `block_on` in the REPL
    /// (initial request, compaction summary, tool-list refresh). Built
    /// once in [`Self::new`] and reused for the lifetime of the `Repl`.
    /// Previously each call site constructed a fresh
    /// `Runtime::new()` and dropped it on return — expensive per-turn
    /// (thread-pool spin-up + epoll registration) and fd-exhaustion-
    /// prone under sustained load. Works because the TUI worker runs
    /// on a plain `std::thread` (see `tui/worker.rs`), so the REPL's
    /// owned runtime is the only tokio context on that thread.
    pub(super) runtime: tokio::runtime::Runtime,
}

impl Repl {
    pub fn new(
        client: LlmClient,
        config: ReplConfig,
        workspace: PathBuf,
        morph_client: Option<MorphClient>,
    ) -> Result<Self> {
        // One runtime for the whole REPL lifetime — reused by every
        // in-REPL `block_on` below (initial request, tool-list refresh,
        // compaction summary). See the `runtime` field doc on `Repl`.
        let runtime = tokio::runtime::Runtime::new()
            .map_err(|e| SofosError::Config(format!("Failed to create async runtime: {}", e)))?;

        let (mcp_manager, mcp_init_lines) = runtime.block_on(async {
            match McpManager::new(workspace.clone()).await {
                Ok((manager, block)) => (Some(manager), block),
                Err(e) => {
                    tracing::warn!(error = %e, "failed to initialize MCP manager");
                    (None, String::new())
                }
            }
        });

        let tool_executor = ToolExecutor::new(
            workspace.clone(),
            morph_client,
            mcp_manager,
            config.safe_mode,
            std::io::stdin().is_terminal(),
        )?;

        let has_morph = tool_executor.has_morph();
        let has_code_search = tool_executor.has_code_search();

        let history_manager = HistoryManager::new(workspace.clone())?;
        let mut image_loader = ImageLoader::new(workspace.clone())?;
        image_loader.install_read_path_session(
            std::io::stdin().is_terminal(),
            tool_executor.read_path_session_allowed(),
            tool_executor.read_path_session_denied(),
        );

        // Load custom instructions
        let custom_instructions = history_manager.load_custom_instructions()?;

        if custom_instructions.is_some() {
            tracing::info!("loaded custom instructions");
        }

        // Validate that `max_tokens` leaves room for the largest legacy
        // thinking budget we might send. The budget is picked per-effort
        // in `request_builder` (Low=1024, Medium=5120, High=16384), so
        // the invariant we need is `max_tokens > HIGH`. We check
        // unconditionally on enabled-thinking sessions rather than
        // probing the model id, because the model can be swapped mid-
        // session via `/model` and we don't want a runtime 400.
        if config.reasoning_effort.is_enabled()
            && config.max_tokens <= crate::api::anthropic::LEGACY_THINKING_BUDGET_HIGH
        {
            return Err(SofosError::Config(format!(
                "max_tokens ({}) must exceed the legacy thinking-budget ceiling ({}). \
                 Use a higher --max-tokens or set --reasoning-effort off.",
                config.max_tokens,
                crate::api::anthropic::LEGACY_THINKING_BUDGET_HIGH
            )));
        }

        // Reject `(model, effort)` pairs the active provider won't
        // accept, e.g. `xhigh` on Opus 4.6 or `max` on any OpenAI
        // model. Catching it here turns a runtime 400 into a clear
        // startup error.
        if let Some(msg) =
            crate::api::model_info::effort_support_error(&config.model, config.reasoning_effort)
        {
            return Err(SofosError::Config(msg));
        }

        let mut conversation =
            ConversationHistory::with_features(has_morph, has_code_search, custom_instructions);
        conversation.set_max_context_tokens(crate::config::max_context_tokens_for(&config.model));
        conversation.set_auto_compact_token_limit(crate::config::auto_compact_token_limit_for(
            &config.model,
        ));

        let safe_mode_mcp_note = if config.safe_mode {
            conversation.add_user_message(SAFE_MODE_MESSAGE.to_string());
            set_safe_mode_cursor_style()?;
            format_mcp_safe_mode_summary(
                &tool_executor.mcp_servers_excluded_from_safe_mode(),
                &tool_executor.mcp_servers_included_in_safe_mode(),
            )
        } else {
            String::new()
        };

        let mcp_init_lines = if safe_mode_mcp_note.is_empty() {
            mcp_init_lines
        } else if mcp_init_lines.is_empty() {
            safe_mode_mcp_note
        } else {
            format!("{}{}", mcp_init_lines, safe_mode_mcp_note)
        };

        let session_id = HistoryManager::generate_session_id();
        let session_state = SessionState::new(session_id, conversation);
        let model_config =
            ModelConfig::new(config.model, config.max_tokens, config.reasoning_effort);

        let ui = UI::new();

        // Initialize available tools (needs async) — uses the runtime
        // before it's moved into the struct so the async block can
        // borrow `tool_executor` without conflicting with the struct's
        // own borrow rules.
        let available_tools = runtime.block_on(async { tool_executor.get_available_tools().await });

        Ok(Self {
            client,
            tool_executor,
            history_manager,
            image_loader,
            ui,
            model_config,
            session_state,
            safe_mode: config.safe_mode,
            available_tools,
            interrupt_flag: Arc::new(AtomicBool::new(false)),
            steer_buffer: Arc::new(Mutex::new(Vec::new())),
            startup_banner: String::new(),
            mcp_init_lines,
            runtime,
        })
    }

    pub fn run(self) -> Result<()> {
        tui::run(self)
    }

    /// Hand the TUI the logo + workspace/model/morph lines that `main.rs`
    /// used to `println!` straight to stdout. The TUI replays them through
    /// its capture pipe after the alternate-output redirection is live so
    /// they land above the viewport instead of being overdrawn by it.
    pub fn set_startup_banner(&mut self, text: String) {
        self.startup_banner = text;
    }

    pub(crate) fn take_startup_banner(&mut self) -> String {
        std::mem::take(&mut self.startup_banner)
    }

    /// Drain the "MCP server '…' initialized" lines collected during
    /// [`Self::new`] so the caller can splice them into the startup
    /// banner.
    pub(crate) fn take_mcp_init_lines(&mut self) -> String {
        std::mem::take(&mut self.mcp_init_lines)
    }

    /// Install the interrupt flag used by the TUI to signal ESC/Ctrl+C during
    /// an AI turn. Called once before the worker thread takes ownership.
    pub fn install_interrupt_flag(&mut self, flag: Arc<AtomicBool>) {
        self.interrupt_flag = Arc::clone(&flag);
        self.tool_executor.install_interrupt_flag(flag);
    }

    /// Install the shared steer buffer used by the TUI to inject mid-turn
    /// user messages. Called once before the worker thread takes ownership
    /// so UI and worker share the same buffer.
    pub fn install_steer_buffer(&mut self, buffer: SteerBuffer) {
        self.steer_buffer = buffer;
    }

    pub fn model_label(&self) -> String {
        self.model_config.model.clone()
    }

    /// Snapshot of the user-facing state displayed in the TUI status line.
    pub fn status_snapshot(&self) -> tui::event::StatusSnapshot {
        let effort = self.model_config.reasoning_effort;
        let reasoning = if self.uses_adaptive_thinking() {
            // Opus 4.7 picks its own budget; showing a fixed token count
            // would be misleading, so render the `output_config.effort`
            // value we actually send instead.
            format!("effort: {}", crate::api::anthropic::effort_label(effort))
        } else if matches!(self.client, Anthropic(_)) {
            if effort.is_enabled() {
                // The legacy non-adaptive shape's `budget_tokens` comes
                // from the effort tier (mapping in `request_builder`).
                // Show the value we actually send so the status line
                // matches reality.
                let budget = crate::api::anthropic::legacy_thinking_budget(effort);
                format!("thinking: {} tok", budget)
            } else {
                "thinking: off".to_string()
            }
        } else {
            format!("effort: {}", effort.as_label())
        };

        tui::event::StatusSnapshot {
            model: self.model_config.model.clone(),
            mode: if self.safe_mode {
                tui::event::Mode::Safe
            } else {
                tui::event::Mode::Normal
            },
            reasoning,
            input_tokens: self.session_state.total_input_tokens,
            output_tokens: self.session_state.total_output_tokens,
        }
    }

    /// Build initial request for user message
    pub(super) fn build_initial_request(&self) -> CreateMessageRequest {
        RequestBuilder::new(
            &self.client,
            &self.model_config.model,
            self.model_config.max_tokens,
            &self.session_state.conversation,
            self.get_available_tools(),
            self.model_config.reasoning_effort,
            &self.session_state.session_id,
        )
        .build()
    }

    pub fn process_single_prompt(&mut self, prompt: &str) -> Result<()> {
        let symbol = if self.safe_mode { ":" } else { ">" };
        println!("{} {}", symbol.bright_green().bold(), prompt);
        println!();
        // Capture the turn result so we can persist the session even
        // when the turn errored out — without this the user can't
        // `--resume` after any failed -p invocation. Save failures are
        // logged as warnings rather than overriding the original error.
        let turn_result = self.process_message(prompt, vec![]);
        if let Err(e) = self.save_current_session() {
            tracing::warn!(error = %e, "failed to save session after non-interactive turn");
        }
        turn_result?;
        UI::display_session_summary(
            &self.model_config.model,
            self.session_state.total_input_tokens,
            self.session_state.total_output_tokens,
            self.session_state.total_cache_read_tokens,
            self.session_state.total_cache_creation_tokens,
            self.session_state.peak_single_turn_input_tokens,
        );

        Ok(())
    }

    pub fn get_session_summary(&self) -> tui::event::ExitSummary {
        tui::event::ExitSummary {
            model: self.model_config.model.clone(),
            input_tokens: self.session_state.total_input_tokens,
            output_tokens: self.session_state.total_output_tokens,
            cache_read_tokens: self.session_state.total_cache_read_tokens,
            cache_creation_tokens: self.session_state.total_cache_creation_tokens,
            peak_single_turn_input_tokens: self.session_state.peak_single_turn_input_tokens,
        }
    }

    pub fn handle_clear_command(&mut self) -> Result<()> {
        let new_session_id = HistoryManager::generate_session_id();
        self.session_state.conversation.clear();
        self.session_state.clear(new_session_id);
        self.session_state
            .conversation
            .add_user_message("The session history has been cleared".to_string());
        println!("\n{}\n", "Conversation history cleared.".bright_yellow());
        Ok(())
    }

    /// True when the active model uses adaptive thinking (Opus 4.7,
    /// Opus 4.6, Sonnet 4.6). Shared by the three `/think` handlers
    /// and the status line so they don't drift apart.
    fn uses_adaptive_thinking(&self) -> bool {
        matches!(self.client, Anthropic(_))
            && crate::api::anthropic::requires_adaptive_thinking(&self.model_config.model)
    }

    /// Print the reasoning-state line shared by the `/think` handlers.
    /// Three flavours: adaptive effort, Anthropic manual budget, OpenAI
    /// reasoning effort. Writing this once keeps the wording identical
    /// across the commands.
    fn print_reasoning_state(&self) {
        let effort = self.model_config.reasoning_effort;
        if self.uses_adaptive_thinking() {
            println!(
                "\n{} {}\n",
                "Adaptive thinking effort:".bright_green(),
                crate::api::anthropic::effort_label(effort)
            );
        } else if matches!(self.client, Anthropic(_)) {
            if effort.is_enabled() {
                // Show the per-effort tier budget so the `/think`
                // output matches what hits the API.
                let budget = crate::api::anthropic::legacy_thinking_budget(effort);
                println!(
                    "\n{} (budget: {} tokens)\n",
                    "Extended thinking: enabled".bright_green(),
                    budget
                );
            } else {
                println!("\n{}\n", "Extended thinking: disabled".bright_yellow());
            }
        } else {
            println!(
                "\n{} {}\n",
                "Reasoning effort:".bright_green(),
                effort.as_label()
            );
        }
    }

    pub fn handle_think_set(&mut self, effort: crate::api::ReasoningEffort) {
        if let Some(msg) =
            crate::api::model_info::effort_support_error(&self.model_config.model, effort)
        {
            println!();
            UI::print_error(&msg);
            println!();
            return;
        }
        self.model_config.set_reasoning_effort(effort);
        self.print_reasoning_state();
    }

    pub fn handle_think_status(&self) {
        self.print_reasoning_state();
    }

    pub fn enable_safe_mode(&mut self) {
        if self.safe_mode {
            println!("\n{}\n", "Safe mode: already enabled".dimmed());
            return;
        }
        self.safe_mode = true;
        self.tool_executor.set_safe_mode(true);
        self.refresh_available_tools();

        self.session_state
            .conversation
            .add_user_message(SAFE_MODE_MESSAGE.to_string());
        println!(
            "\n{} read-only native tools; no writes or bash\n",
            "Safe mode: enabled".bright_yellow()
        );
        self.print_mcp_safe_mode_summary();
    }

    fn print_mcp_safe_mode_summary(&self) {
        let summary = format_mcp_safe_mode_summary(
            &self.tool_executor.mcp_servers_excluded_from_safe_mode(),
            &self.tool_executor.mcp_servers_included_in_safe_mode(),
        );
        if !summary.is_empty() {
            print!("{}", summary);
            println!();
        }
    }

    pub fn disable_safe_mode(&mut self) {
        if !self.safe_mode {
            println!("\n{}\n", "Safe mode: already disabled".dimmed());
            return;
        }
        self.safe_mode = false;
        self.tool_executor.set_safe_mode(false);
        self.refresh_available_tools();

        self.session_state
            .conversation
            .add_user_message(NORMAL_MODE_MESSAGE.to_string());
        println!(
            "\n{} all tools available\n",
            "Safe mode: disabled".bright_green()
        );
    }

    fn refresh_available_tools(&mut self) {
        // Disjoint field borrows: `self.runtime` and `self.tool_executor`
        // are different fields, so the async block's borrow of
        // `tool_executor` doesn't conflict with the runtime's `&self`
        // receiver on `block_on`.
        let tools = self
            .runtime
            .block_on(async { self.tool_executor.get_available_tools().await });
        self.available_tools = tools;
    }

    fn get_available_tools(&self) -> Vec<crate::api::Tool> {
        self.available_tools.clone()
    }

    /// Await the interrupt flag in an async-friendly loop (50ms poll).
    pub(super) async fn wait_for_interrupt(flag: Arc<AtomicBool>) {
        while !flag.load(Ordering::Relaxed) {
            sleep(Duration::from_millis(50)).await;
        }
    }
}

/// Format the per-server safe-mode summary for the startup banner. The
/// terminal renders this right after the `MCP servers:` block so the
/// user can immediately see which servers were filtered out and which
/// were opted in.
fn format_mcp_safe_mode_summary(excluded: &[String], included: &[String]) -> String {
    if excluded.is_empty() && included.is_empty() {
        return String::new();
    }
    let mut out = String::new();
    if !excluded.is_empty() {
        out.push_str(&format!(
            "  {} Safe mode hides MCP servers: {}\n",
            "•".bright_yellow(),
            excluded.join(", ")
        ));
        out.push_str(
            "    Set `safe_mode = \"read_only\"` on a server to make its tools available.\n",
        );
    }
    if !included.is_empty() {
        out.push_str(&format!(
            "  {} Safe mode allows MCP servers: {}\n",
            "•".bright_green(),
            included.join(", ")
        ));
    }
    out
}
