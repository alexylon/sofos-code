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
use crate::config::{
    ApprovalPolicy, ModelConfig, PermissionPreset, SandboxMode, readonly_mode_message,
    sandbox_off_message, sandbox_on_message,
};
use crate::error::{Result, SofosError};
use crate::mcp::McpManager;
use crate::session::{HistoryManager, SessionState};
use crate::tools::ToolExecutor;
use crate::ui::{UI, set_default_cursor_style, set_readonly_cursor_style};
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

/// Build the assistant-facing preamble for the active access `mode` and
/// escalation `policy`. Centralised so the startup path, slash-command path,
/// and `/clear` path all show the same text.
fn mode_preamble_for(mode: SandboxMode, policy: ApprovalPolicy) -> String {
    match mode {
        SandboxMode::ReadOnly => readonly_mode_message(),
        SandboxMode::Sandboxed => sandbox_on_message(policy),
        SandboxMode::Unsandboxed => sandbox_off_message(),
    }
}

/// One-line notice printed when a `/permissions` preset becomes active,
/// coloured by access mode. Only ever called with a preset available on
/// this host, since `apply_permission_preset` refuses the rest.
fn permission_preset_notice(preset: PermissionPreset) -> colored::ColoredString {
    let text = format!("Permissions: {} — {}", preset.label(), preset.description());
    match preset.mode() {
        SandboxMode::ReadOnly => text.bright_yellow(),
        SandboxMode::Unsandboxed => text.bright_red(),
        SandboxMode::Sandboxed => text.bright_green(),
    }
}

pub struct ReplConfig {
    pub model: String,
    pub max_tokens: u32,
    pub reasoning_effort: crate::api::ReasoningEffort,
    pub mode: SandboxMode,
    pub approval_policy: ApprovalPolicy,
}

impl ReplConfig {
    pub fn new(
        model: String,
        max_tokens: u32,
        reasoning_effort: crate::api::ReasoningEffort,
        mode: SandboxMode,
        approval_policy: ApprovalPolicy,
    ) -> Self {
        Self {
            model,
            max_tokens,
            reasoning_effort,
            mode,
            approval_policy,
        }
    }
}

pub struct Repl {
    pub(super) client: LlmClient,
    pub(super) tool_executor: ToolExecutor,
    pub(super) history_manager: HistoryManager,
    pub(super) ui: UI,
    pub(super) model_config: ModelConfig,
    pub(super) session_state: SessionState,
    pub(super) mode: SandboxMode,
    pub(super) approval_policy: ApprovalPolicy,
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
    /// once and reused for the lifetime of the `Repl`; the TUI worker
    /// runs on a plain `std::thread` so this is the only tokio context
    /// on that thread.
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

        let mut tool_executor = ToolExecutor::new(
            workspace.clone(),
            morph_client,
            mcp_manager,
            config.mode,
            std::io::stdin().is_terminal(),
        )?;
        tool_executor.set_approval_policy(config.approval_policy);

        let has_morph = tool_executor.has_morph();
        let has_code_search = tool_executor.has_code_search();

        let history_manager = HistoryManager::new(workspace.clone())?;

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
        // accept, e.g. `xhigh` on a model that tops out at `high`, or
        // `max` on any OpenAI model. Catching it here turns a runtime
        // 400 into a clear startup error.
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

        // Every mode gets a startup preamble so the assistant knows from
        // turn 1 which tier rules and platform caveats apply, not just
        // when the mode is switched mid-session.
        conversation.add_user_message(mode_preamble_for(config.mode, config.approval_policy));
        let readonly_mcp_note = if config.mode.is_readonly() {
            set_readonly_cursor_style()?;
            format_mcp_readonly_summary(
                &tool_executor.mcp_servers_excluded_from_readonly(),
                &tool_executor.mcp_servers_included_in_readonly(),
            )
        } else {
            String::new()
        };

        let mcp_init_lines = if readonly_mcp_note.is_empty() {
            mcp_init_lines
        } else if mcp_init_lines.is_empty() {
            readonly_mcp_note
        } else {
            format!("{}{}", mcp_init_lines, readonly_mcp_note)
        };

        let session_id = history_manager.generate_unique_session_id();
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
            ui,
            model_config,
            session_state,
            mode: config.mode,
            approval_policy: config.approval_policy,
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

    pub fn current_reasoning_effort(&self) -> crate::api::ReasoningEffort {
        self.model_config.reasoning_effort
    }

    /// Snapshot of the user-facing state displayed in the TUI status line.
    pub fn status_snapshot(&self) -> tui::event::StatusSnapshot {
        let effort = self.model_config.reasoning_effort;
        let reasoning = if self.uses_adaptive_thinking() {
            // Adaptive models pick their own budget; showing a fixed
            // token count would be misleading, so render the
            // `output_config.effort` value we actually send instead.
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
            mode: self.mode,
            approval: self.approval_policy,
            reasoning,
            input_tokens: self.session_state.total_input_tokens,
            output_tokens: self.session_state.total_output_tokens,
            cache_read_tokens: self.session_state.total_cache_read_tokens,
            cache_creation_tokens: self.session_state.total_cache_creation_tokens,
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
        let symbol = match self.mode {
            SandboxMode::ReadOnly => ":",
            SandboxMode::Sandboxed => ">",
            SandboxMode::Unsandboxed => "#",
        };
        println!("{} {}", symbol.bright_green().bold(), prompt);
        println!();
        // Capture the turn result so we can persist the session even
        // when the turn errored out — without this the user can't
        // `--resume` after any failed -p invocation. Save failures are
        // logged as warnings rather than overriding the original error.
        let turn_result = self.process_message(prompt, vec![]);
        if let Err(e) = self.save_current_session() {
            // Mirror the interactive worker: surface save failures
            // through `UI::print_warning` so they appear in the
            // user's terminal output even without `RUST_LOG` set,
            // which most one-shot users don't configure.
            UI::print_warning(&format!(
                "failed to save session after non-interactive turn: {}",
                e
            ));
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
            panicked: false,
        }
    }

    pub fn handle_clear_command(&mut self) -> Result<()> {
        let new_session_id = self.history_manager.generate_unique_session_id();
        self.session_state.conversation.clear();
        self.session_state.clear(new_session_id);
        // The active mode survives `/clear`, so the preamble has to ride
        // along too — otherwise the model proposes blocked tools (in
        // readonly mode) or assumes a different policy than is in effect.
        self.session_state
            .conversation
            .add_user_message(mode_preamble_for(self.mode, self.approval_policy));
        self.session_state
            .conversation
            .add_user_message("The session history has been cleared".to_string());
        println!("\n{}\n", "Conversation history cleared.".bright_yellow());
        Ok(())
    }

    /// True when the active model uses adaptive thinking.
    fn uses_adaptive_thinking(&self) -> bool {
        matches!(self.client, Anthropic(_))
            && crate::api::anthropic::requires_adaptive_thinking(&self.model_config.model)
    }

    /// Print the reasoning-state line shared by the `/effort` paths.
    /// Three flavours: adaptive effort, Anthropic manual budget, OpenAI
    /// reasoning effort.
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

    pub fn handle_effort_set(&mut self, effort: crate::api::ReasoningEffort) {
        if let Some(msg) =
            crate::api::model_info::effort_support_error(&self.model_config.model, effort)
        {
            println!();
            UI::print_error(&msg);
            println!();
            return;
        }
        // Mirror the startup gate: legacy Anthropic thinking needs
        // max_tokens above the budget ceiling.
        if matches!(self.client, Anthropic(_))
            && !self.uses_adaptive_thinking()
            && effort.is_enabled()
            && self.model_config.max_tokens <= crate::api::anthropic::LEGACY_THINKING_BUDGET_HIGH
        {
            println!();
            UI::print_error(&format!(
                "Cannot enable extended thinking on the legacy budget — max_tokens \
                 ({}) must exceed {}. Relaunch with a higher --max-tokens or pick a \
                 lower effort.",
                self.model_config.max_tokens,
                crate::api::anthropic::LEGACY_THINKING_BUDGET_HIGH
            ));
            println!();
            return;
        }
        self.model_config.set_reasoning_effort(effort);
        self.print_reasoning_state();
    }

    /// Non-interactive fallback for `/effort`. The TUI opens the
    /// picker; this path lists supported levels in `--prompt` mode.
    pub fn handle_effort_picker_fallback(&self) {
        let info = crate::api::model_info::lookup(&self.model_config.model);
        let current = self.model_config.reasoning_effort;
        println!();
        println!(
            "{} {}",
            "Current effort:".bright_green(),
            current.as_label().bright_white()
        );
        println!("{}", "Supported levels:".bright_cyan());
        for effort in info.supported_efforts {
            let marker = if *effort == current { "❯" } else { " " };
            println!(
                "  {} {}",
                marker.bright_green(),
                effort.as_label().bright_white()
            );
        }
        println!();
        println!(
            "{}",
            "Use `/effort <level>` to switch, or open an interactive session for the picker."
                .dimmed()
        );
        println!();
    }

    /// Switch the active model to `name`. Refuses unsupported slugs,
    /// cross-provider switches (we can't swap the constructed
    /// [`LlmClient`] mid-session without re-reading API keys), and
    /// switches that would leave the current reasoning effort
    /// orphaned on a model that doesn't accept it. On success the
    /// per-model context-window and auto-compact thresholds are
    /// refreshed so the new ceilings take effect immediately.
    pub fn handle_model_set(&mut self, name: &str) {
        let Some(choice) = crate::api::model_info::canonical_model(name) else {
            println!();
            UI::print_error(
                &crate::api::model_info::model_support_error(name)
                    .unwrap_or_else(|| format!("Model `{}` is not supported.", name)),
            );
            println!();
            return;
        };

        if choice.name == self.model_config.model {
            println!(
                "\n{} already active: {}\n",
                "Model:".dimmed(),
                choice.name.bright_green()
            );
            return;
        }

        let current_provider = crate::api::model_info::provider_for(&self.model_config.model);
        if choice.provider != current_provider {
            println!();
            UI::print_error(&format!(
                "Cannot switch to `{}` ({}) from the current {} session. \
                 Re-launch with `--model {}` to use it.",
                choice.name,
                choice.provider.label(),
                current_provider.label(),
                choice.name
            ));
            println!();
            return;
        }

        if let Some(msg) = crate::api::model_info::effort_support_error(
            choice.name,
            self.model_config.reasoning_effort,
        ) {
            println!();
            UI::print_error(&format!(
                "{} Run `/effort <level>` to pick a supported level before switching.",
                msg
            ));
            println!();
            return;
        }

        self.model_config.model = choice.name.to_string();
        self.session_state
            .conversation
            .set_max_context_tokens(crate::config::max_context_tokens_for(choice.name));
        self.session_state
            .conversation
            .set_auto_compact_token_limit(crate::config::auto_compact_token_limit_for(choice.name));
        println!(
            "\n{} {}\n",
            "Model:".bright_green(),
            choice.name.bright_white()
        );
    }

    /// Non-interactive fallback for `/model` (no argument). The TUI
    /// intercepts the command and opens the picker; this path only
    /// runs in `--prompt` mode and other non-interactive contexts,
    /// where listing the choices is the best we can do.
    pub fn handle_model_picker_fallback(&self) {
        println!();
        println!(
            "{} {}",
            "Current model:".bright_green(),
            self.model_config.model.bright_white()
        );
        println!("{}", "Available models:".bright_cyan());
        for choice in crate::api::model_info::SUPPORTED_MODELS {
            let marker = if choice.name == self.model_config.model {
                "❯"
            } else {
                " "
            };
            println!(
                "  {} {:<20} {}",
                marker.bright_green(),
                choice.name.bright_white(),
                choice.description.dimmed()
            );
        }
        println!();
        println!(
            "{}",
            "Use `/model <name>` to switch, or open an interactive session for the picker."
                .dimmed()
        );
        println!();
    }

    /// Apply a `/permissions` preset: set the sandbox mode and, for the
    /// sandboxed presets, the escalation policy in one step with a single
    /// notice. Syncs the offered tools, the cursor shape, and the mode
    /// preamble the assistant sees. A no-op (dimmed notice) when the preset
    /// is already active.
    pub fn apply_permission_preset(&mut self, preset: PermissionPreset) {
        // The sandboxed presets need an operating-system sandbox. Where none
        // can run, the picker greys them out; the typed `/permissions
        // <preset>` path refuses them here for the same reason, so the mode
        // never claims a confinement that cannot take effect.
        if !preset.is_available(self.sandbox_available()) {
            let current = PermissionPreset::current(self.mode, self.approval_policy);
            println!(
                "\n{}\n",
                format!(
                    "{} is unavailable here: this platform has no operating-system \
                     sandbox. Staying in {}.",
                    preset.label(),
                    current.label()
                )
                .bright_yellow()
            );
            return;
        }
        let mode = preset.mode();
        let target_policy = preset.escalation();
        let mode_changed = self.mode != mode;
        let policy_changed = target_policy.is_some_and(|p| p != self.approval_policy);
        if !mode_changed && !policy_changed {
            println!(
                "\n{}\n",
                format!("Already set to {}", preset.label()).dimmed()
            );
            return;
        }
        if mode_changed {
            self.mode = mode;
            self.tool_executor.set_mode(mode);
            self.refresh_available_tools();
            // Match the terminal cursor shape to the mode. Best-effort: a
            // failed SGR write here is purely cosmetic.
            let _ = if mode.is_readonly() {
                set_readonly_cursor_style()
            } else {
                set_default_cursor_style()
            };
        }
        if let Some(policy) = target_policy {
            self.approval_policy = policy;
            self.tool_executor.set_approval_policy(policy);
        }
        // Re-state the active permissions to the assistant on every change.
        // Switching among the sandboxed presets changes only the escalation
        // policy, not the mode, so this must fire on a policy-only change too —
        // otherwise the model keeps the previous preset's escalation guidance.
        self.session_state
            .conversation
            .add_user_message(mode_preamble_for(self.mode, self.approval_policy));
        println!("\n{}\n", permission_preset_notice(preset));
        if mode.is_readonly() {
            self.print_mcp_readonly_summary();
        }
    }

    /// Whether the operating-system sandbox can actually run on this
    /// machine. False on Windows and on a Linux host without a usable
    /// Bubblewrap, where the sandboxed mode runs commands unconfined.
    pub fn sandbox_available(&self) -> bool {
        crate::tools::bash::sandbox::is_available()
    }

    /// Non-interactive fallback for `/permissions`. The TUI opens the
    /// picker; this path lists the presets in `--prompt` mode and marks the
    /// active one.
    pub fn handle_permissions_picker_fallback(&self) {
        let current = PermissionPreset::current(self.mode, self.approval_policy);
        let available = self.sandbox_available();
        println!();
        println!(
            "{} {}",
            "Current permissions:".bright_green(),
            current.label().bright_white()
        );
        println!("{}", "Available presets:".bright_cyan());
        for preset in crate::config::PERMISSION_PRESETS {
            let marker = if preset == current { "❯" } else { " " };
            let note = if preset.is_available(available) {
                ""
            } else {
                "  (unavailable on this platform)"
            };
            println!(
                "  {} {}{}",
                marker.bright_green(),
                preset.label().bright_white(),
                note.dimmed()
            );
        }
        println!();
        println!(
            "{}",
            "Use `/permissions <preset>` to switch, or open an interactive session for the picker."
                .dimmed()
        );
    }

    fn print_mcp_readonly_summary(&self) {
        let summary = format_mcp_readonly_summary(
            &self.tool_executor.mcp_servers_excluded_from_readonly(),
            &self.tool_executor.mcp_servers_included_in_readonly(),
        );
        if !summary.is_empty() {
            print!("{}", summary);
            println!();
        }
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

/// Format the per-server read-only summary for the startup banner. The
/// terminal renders this right after the `MCP servers:` block so the
/// user can immediately see which servers were filtered out and which
/// were opted in.
fn format_mcp_readonly_summary(excluded: &[String], included: &[String]) -> String {
    if excluded.is_empty() && included.is_empty() {
        return String::new();
    }
    let mut out = String::new();
    if !excluded.is_empty() {
        out.push_str(&format!(
            "  {} Read-only mode hides MCP servers: {}\n",
            "•".bright_yellow(),
            excluded.join(", ")
        ));
        out.push_str(
            "    Set `readonly = \"read_only\"` on a server to make its tools available.\n",
        );
    }
    if !included.is_empty() {
        out.push_str(&format!(
            "  {} Read-only mode allows MCP servers: {}\n",
            "•".bright_green(),
            included.join(", ")
        ));
    }
    out
}
