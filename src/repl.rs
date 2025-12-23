use crate::api::LlmClient::Anthropic;
use crate::api::{CreateMessageRequest, ImageSource, LlmClient, MessageContentBlock, MorphClient};
use crate::commands::{Command, CommandResult};
use crate::config::{NORMAL_MODE_MESSAGE, SAFE_MODE_MESSAGE};
use crate::conversation::ConversationHistory;
use crate::error::{Result, SofosError};
use crate::history::{DisplayMessage, HistoryManager};
use crate::model_config::ModelConfig;
use crate::prompt::ReplPrompt;
use crate::request_builder::RequestBuilder;
use crate::response_handler::ResponseHandler;
use crate::session_state::SessionState;
use crate::tools::image::{extract_image_references, ImageLoader, ImageReference};
use crate::tools::ToolExecutor;
use crate::ui::{set_safe_mode_cursor_style, UI};
use colored::Colorize;
use crossterm::event::{KeyCode, KeyModifiers};
use reedline::{
    default_emacs_keybindings, ColumnarMenu, DefaultCompleter, Emacs, MenuBuilder, Reedline,
    ReedlineEvent, ReedlineMenu, Signal,
};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use tokio::time::{sleep, Duration};

pub struct ReplConfig {
    pub model: String,
    pub max_tokens: u32,
    pub enable_thinking: bool,
    pub thinking_budget: u32,
    pub safe_mode: bool,
}

impl ReplConfig {
    pub fn new(
        model: String,
        max_tokens: u32,
        enable_thinking: bool,
        thinking_budget: u32,
        safe_mode: bool,
    ) -> Self {
        Self {
            model,
            max_tokens,
            enable_thinking,
            thinking_budget,
            safe_mode,
        }
    }
}

pub struct Repl {
    client: LlmClient,
    tool_executor: ToolExecutor,
    history_manager: HistoryManager,
    image_loader: ImageLoader,
    ui: UI,
    editor: Reedline,
    prompt: ReplPrompt,
    model_config: ModelConfig,
    session_state: SessionState,
    safe_mode: bool,
}

impl Repl {
    pub fn new(
        client: LlmClient,
        config: ReplConfig,
        workspace: PathBuf,
        morph_client: Option<MorphClient>,
    ) -> Result<Self> {
        let tool_executor = ToolExecutor::new(workspace.clone(), morph_client, config.safe_mode)?;

        let has_morph = tool_executor.has_morph();
        let has_code_search = tool_executor.has_code_search();

        let history_manager = HistoryManager::new(workspace.clone())?;
        let image_loader = ImageLoader::new(workspace.clone())?;

        // Load custom instructions
        let custom_instructions = history_manager.load_custom_instructions()?;

        if custom_instructions.is_some() {
            eprintln!("{}", "Loaded custom instructions".bright_green());
        }

        // Validate thinking budget
        if config.enable_thinking && config.thinking_budget >= config.max_tokens {
            return Err(SofosError::Config(format!(
                "thinking_budget ({}) must be less than max_tokens ({})",
                config.thinking_budget, config.max_tokens
            )));
        }

        let mut conversation =
            ConversationHistory::with_features(has_morph, has_code_search, custom_instructions);

        if config.safe_mode {
            conversation.add_user_message(SAFE_MODE_MESSAGE.to_string());
            set_safe_mode_cursor_style()?;
        }

        let commands: Vec<String> = crate::commands::COMMANDS
            .iter()
            .map(|s| s.to_string())
            .collect();

        let mut completer = DefaultCompleter::with_inclusions(&['/', '-', '_']);
        completer = completer.set_min_word_len(1);
        completer.insert(commands.clone());
        let completer = Box::new(completer);

        let completion_menu = ColumnarMenu::default().with_name("completion_menu");
        let completion_menu = ReedlineMenu::EngineCompleter(Box::new(completion_menu));

        let mut keybindings = default_emacs_keybindings();
        keybindings.add_binding(
            KeyModifiers::NONE,
            KeyCode::Tab,
            ReedlineEvent::UntilFound(vec![
                ReedlineEvent::Menu("completion_menu".into()),
                ReedlineEvent::MenuNext,
            ]),
        );
        keybindings.add_binding(
            KeyModifiers::SHIFT,
            KeyCode::BackTab,
            ReedlineEvent::MenuPrevious,
        );

        let edit_mode = Box::new(Emacs::new(keybindings));

        let editor = Reedline::create()
            .use_bracketed_paste(true)
            .with_completer(completer)
            .with_edit_mode(edit_mode)
            .with_menu(completion_menu);

        let prompt = ReplPrompt::new(config.safe_mode);

        let session_id = HistoryManager::generate_session_id();
        let session_state = SessionState::new(session_id, conversation);
        let model_config = ModelConfig::new(
            config.model,
            config.max_tokens,
            config.enable_thinking,
            config.thinking_budget,
        );

        let ui = UI::new();

        Ok(Self {
            client,
            tool_executor,
            history_manager,
            image_loader,
            ui,
            editor,
            prompt,
            model_config,
            session_state,
            safe_mode: config.safe_mode,
        })
    }

    pub fn run(&mut self) -> Result<()> {
        UI::print_welcome();

        loop {
            match self.editor.read_line(&self.prompt) {
                Ok(Signal::Success(mut line)) => {
                    line = line.trim().to_string();

                    // Strip bracketed paste mode markers (ESC[200~ and ESC[201~)
                    // These can appear when pasting text in some terminals
                    line = line
                        .replace("\x1b[200~", "")
                        .replace("\x1b[201~", "")
                        .replace("^[[200~", "")
                        .replace("^[[201~", "");

                    if line.is_empty() {
                        continue;
                    }

                    // Check if this is a command
                    if let Some(command) = Command::from_str(&line) {
                        match command.execute(self) {
                            Ok(CommandResult::Continue) => continue,
                            Ok(CommandResult::Exit) => break,
                            Err(e) => {
                                if e.is_blocked() {
                                    UI::print_blocked_with_hint(&e);
                                } else {
                                    UI::print_error_with_hint(&e);
                                }
                                continue;
                            }
                        }
                    }

                    // Not a command, process as regular message
                    if let Err(e) = self.process_message(&line) {
                        if e.is_blocked() {
                            UI::print_blocked_with_hint(&e);
                        } else {
                            UI::print_error_with_hint(&e);
                        }
                    } else if let Err(e) = self.save_current_session() {
                        UI::print_warning(&format!("Failed to save session: {}", e));
                    }

                    println!();
                }
                Ok(Signal::CtrlC) | Ok(Signal::CtrlD) => {
                    println!("\nExiting...");
                    self.save_current_session()?;
                    UI::display_session_summary(
                        &self.model_config.model,
                        self.session_state.total_input_tokens,
                        self.session_state.total_output_tokens,
                    );
                    UI::print_goodbye();
                    break;
                }
                Err(err) => {
                    UI::print_error(&err.to_string());
                    break;
                }
            }
        }

        Ok(())
    }

    fn process_message(&mut self, user_input: &str) -> Result<()> {
        let (remaining_text, image_refs) = extract_image_references(user_input);

        if !image_refs.is_empty() {
            println!(
                "{} Detected {} image reference(s)",
                "🔍".bright_cyan(),
                image_refs.len()
            );
        }

        let content_blocks = if !image_refs.is_empty() {
            let mut blocks: Vec<MessageContentBlock> = Vec::new();
            let mut failed_images: Vec<String> = Vec::new();

            // Load images first (Claude recommends images before text)
            for img_ref in &image_refs {
                match self.image_loader.load_image(img_ref) {
                    Ok(source) => {
                        let api_source = match source {
                            crate::tools::image::ImageSource::Base64 { media_type, data } => {
                                ImageSource::Base64 { media_type, data }
                            }
                            crate::tools::image::ImageSource::Url { url } => {
                                ImageSource::Url { url }
                            }
                        };
                        blocks.push(MessageContentBlock::Image {
                            source: api_source,
                            cache_control: None,
                        });

                        let path_str = match img_ref {
                            ImageReference::LocalPath(p) => format!("local: {}", p),
                            ImageReference::WebUrl(u) => format!("url: {}", u),
                        };
                        println!("{} {}", "📷 Image loaded:".bright_cyan(), path_str.dimmed());
                    }
                    Err(e) => {
                        let path_str = match img_ref {
                            ImageReference::LocalPath(p) => p.clone(),
                            ImageReference::WebUrl(u) => u.clone(),
                        };
                        let error_msg = format!("[Failed to load image '{}': {}]", path_str, e);
                        failed_images.push(error_msg);
                        println!(
                            "\n{} {}\n",
                            "⚠️  Failed to load image:".bright_yellow().bold(),
                            e
                        );
                    }
                }
            }

            let mut text_parts: Vec<String> = Vec::new();

            if !remaining_text.trim().is_empty() {
                text_parts.push(remaining_text.clone());
            }

            if !failed_images.is_empty() {
                text_parts.extend(failed_images);
            }

            if !text_parts.is_empty() {
                blocks.push(MessageContentBlock::Text {
                    text: text_parts.join("\n\n"),
                    cache_control: None,
                });
            } else if blocks.is_empty() {
                return Err(SofosError::ToolExecution(
                    "No valid images or text in message".to_string(),
                ));
            }

            Some(blocks)
        } else {
            None
        };

        if let Some(blocks) = content_blocks {
            self.session_state.conversation.add_user_with_blocks(blocks);
        } else {
            self.session_state
                .conversation
                .add_user_message(user_input.to_string());
        }

        self.session_state
            .display_messages
            .push(DisplayMessage::UserMessage {
                content: user_input.to_string(),
            });

        let initial_request = self.build_initial_request();

        let runtime = tokio::runtime::Runtime::new()
            .map_err(|e| SofosError::Config(format!("Failed to create async runtime: {}", e)))?;

        // Start interruptible animation and API call
        let running = Arc::new(AtomicBool::new(true));
        let interrupted = Arc::new(AtomicBool::new(false));

        let running_clone = Arc::clone(&running);
        let interrupted_clone = Arc::clone(&interrupted);
        let ui_handle = std::thread::spawn(move || {
            UI::run_animation_with_interrupt(
                "Awaiting response...".to_string(),
                "(Press ESC to interrupt) ".to_string(),
                running_clone,
                interrupted_clone,
            )
        });

        let client = self.client.clone();
        let req = initial_request;
        let mut request_handle = runtime.spawn(async move { client.create_message(req).await });

        let response_result = runtime.block_on(async {
            tokio::select! {
                res = &mut request_handle => {
                    match res {
                        Ok(inner) => inner,
                        Err(e) => Err(SofosError::Join(format!("{}", e)))
                    }
                }
                _ = Self::wait_for_interrupt(Arc::clone(&interrupted)) => {
                    request_handle.abort();
                    Err(SofosError::Interrupted)
                }
            }
        });

        running.store(false, Ordering::Relaxed);
        let _ = ui_handle.join();

        if interrupted.load(Ordering::Relaxed) {
            self.handle_initial_interrupt();
            return Ok(());
        }

        let response = response_result?;

        self.session_state
            .add_tokens(response.usage.input_tokens, response.usage.output_tokens);

        let mut handler = ResponseHandler::new(
            self.client.clone(),
            self.tool_executor.clone(),
            self.session_state.conversation.clone(),
            self.model_config.model.clone(),
            self.model_config.max_tokens,
            self.model_config.enable_thinking,
            self.model_config.thinking_budget,
        );

        match runtime.block_on(handler.handle_response(
            response.content,
            &mut self.session_state.display_messages,
            &mut self.session_state.total_input_tokens,
            &mut self.session_state.total_output_tokens,
        )) {
            Ok(_) => {
                // Update conversation from handler
                self.session_state.conversation = handler.conversation().clone();
                Ok(())
            }
            Err(SofosError::Interrupted) => {
                // Update conversation even on interrupt
                self.session_state.conversation = handler.conversation().clone();
                Ok(())
            }
            Err(e) => Err(e),
        }
    }

    /// Build initial request for user message
    fn build_initial_request(&self) -> CreateMessageRequest {
        RequestBuilder::new(
            &self.client,
            &self.model_config.model,
            self.model_config.max_tokens,
            &self.session_state.conversation,
            self.get_available_tools(),
            self.model_config.enable_thinking,
            self.model_config.thinking_budget,
        )
        .build()
    }

    pub fn process_single_prompt(&mut self, prompt: &str) -> Result<()> {
        let symbol = if self.safe_mode { "λ:" } else { "λ>" };
        println!("{} {}", symbol.bright_green().bold(), prompt);
        println!();
        self.process_message(prompt)?;
        self.save_current_session()?;
        UI::display_session_summary(
            &self.model_config.model,
            self.session_state.total_input_tokens,
            self.session_state.total_output_tokens,
        );

        Ok(())
    }

    // Public methods for command implementations

    pub fn save_current_session(&self) -> Result<()> {
        if self.session_state.conversation.messages().is_empty() {
            return Ok(());
        }

        self.history_manager.save_session(
            &self.session_state.session_id,
            self.session_state.conversation.messages(),
            &self.session_state.display_messages,
            self.session_state.conversation.system_prompt(),
        )?;

        Ok(())
    }

    pub fn get_session_summary(&self) -> (String, u32, u32) {
        (
            self.model_config.model.clone(),
            self.session_state.total_input_tokens,
            self.session_state.total_output_tokens,
        )
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

    pub fn handle_resume_command(&mut self) -> Result<()> {
        let sessions = self.history_manager.list_sessions()?;

        if sessions.is_empty() {
            println!("{}", "No saved sessions found.".yellow());
            return Ok(());
        }

        let selected_id = crate::session_selector::select_session(sessions)?;

        if let Some(session_id) = selected_id {
            self.load_session_by_id(&session_id)?;
            println!(
                "{} {}",
                "Session loaded:".bright_green(),
                "Continue your conversation below".dimmed()
            );
            println!();
        }

        Ok(())
    }

    pub fn handle_think_on(&mut self) {
        self.model_config.set_thinking(true);

        if matches!(self.client, Anthropic(_)) {
            println!(
                "\n{} (budget: {} tokens)\n",
                "Extended thinking enabled.".bright_green(),
                self.model_config.thinking_budget
            );
        } else {
            let reasoning = Some(crate::api::Reasoning::enabled());
            let effort: Option<&str> = reasoning.as_ref().map(|r| r.effort.as_str());

            if let Some(e) = effort {
                println!("\n{} {}\n", "Reasoning effort:".bright_green(), e);
            }
        }
    }

    pub fn handle_think_off(&mut self) {
        self.model_config.set_thinking(false);

        if matches!(self.client, Anthropic(_)) {
            println!("\n{}\n", "Extended thinking disabled.".bright_yellow());
        } else {
            let reasoning = Some(crate::api::Reasoning::disabled());
            let effort: Option<&str> = reasoning.as_ref().map(|r| r.effort.as_str());

            if let Some(e) = effort {
                println!("\n{} {}\n", "Reasoning effort:".bright_green(), e);
            }
        }
    }

    pub fn handle_think_status(&self) {
        if self.model_config.enable_thinking {
            println!(
                "\n{} (budget: {} tokens)\n",
                "Extended thinking is enabled".bright_green(),
                self.model_config.thinking_budget
            );
        } else {
            println!("\n{}\n", "Extended thinking is disabled".bright_yellow());
        }
    }

    pub fn enable_safe_mode(&mut self) {
        if !self.safe_mode {
            self.safe_mode = true;
            self.tool_executor.set_safe_mode(true);
            self.session_state
                .conversation
                .add_user_message(SAFE_MODE_MESSAGE.to_string());
            self.prompt.set_safe_mode(true);
        }
    }

    pub fn disable_safe_mode(&mut self) {
        if self.safe_mode {
            self.safe_mode = false;
            self.tool_executor.set_safe_mode(false);
            self.session_state
                .conversation
                .add_user_message(NORMAL_MODE_MESSAGE.to_string());
            self.prompt.set_safe_mode(false);
        }
    }

    pub fn load_session_by_id(&mut self, session_id: &str) -> Result<()> {
        let session = self.history_manager.load_session(session_id)?;

        self.session_state.session_id = session.id.clone();
        self.session_state.conversation.clear();
        self.session_state
            .conversation
            .restore_messages(session.api_messages.clone());
        self.session_state.display_messages = session.display_messages.clone();

        println!(
            "{} {} ({} messages)",
            "Loaded session:".bright_green(),
            session.id,
            session.api_messages.len()
        );
        println!();

        self.ui.display_session(&session)?;

        Ok(())
    }

    fn handle_initial_interrupt(&mut self) {
        println!(
            "\n{}",
            "Interrupted by user. You can now provide additional guidance.".bright_yellow()
        );
        println!();

        let interrupt_msg = "INTERRUPT: The user pressed ESC to interrupt the request before receiving a response. \
                             They want to provide additional guidance or clarification. Wait for their next message.";
        self.session_state
            .conversation
            .add_user_message(interrupt_msg.to_string());

        self.session_state
            .display_messages
            .push(DisplayMessage::UserMessage {
                content: "[Interrupted - no response received]".to_string(),
            });
    }

    fn get_available_tools(&self) -> Vec<crate::api::Tool> {
        self.tool_executor.get_available_tools()
    }

    /// Await the interrupt flag in an async-friendly loop (50ms poll).
    async fn wait_for_interrupt(flag: Arc<AtomicBool>) {
        while !flag.load(Ordering::Relaxed) {
            sleep(Duration::from_millis(50)).await;
        }
    }
}
