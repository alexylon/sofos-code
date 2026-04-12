mod clipboard_edit_mode;
pub mod conversation;
mod prompt;
mod request_builder;
mod response_handler;

pub use conversation::ConversationHistory;
pub use prompt::ReplPrompt;
pub use request_builder::RequestBuilder;
pub use response_handler::ResponseHandler;

use std::io::IsTerminal;

use crate::api::LlmClient::Anthropic;
use crate::api::{CreateMessageRequest, ImageSource, LlmClient, MessageContentBlock, MorphClient};
use crate::commands::{Command, CommandResult};
use crate::config::{ModelConfig, NORMAL_MODE_MESSAGE, SAFE_MODE_MESSAGE};
use crate::error::{Result, SofosError};
use crate::mcp::McpManager;
use crate::session::{DisplayMessage, HistoryManager, SessionState};
use crate::tools::ToolExecutor;
use crate::tools::image::{ImageLoader, ImageReference, extract_image_references};
use crate::ui::{UI, set_safe_mode_cursor_style};
use colored::Colorize;
use crossterm::event::{KeyCode, KeyModifiers};
use reedline::{
    ColumnarMenu, DefaultCompleter, Emacs, MenuBuilder, Reedline, ReedlineEvent, ReedlineMenu,
    Signal, default_emacs_keybindings,
};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use tokio::time::{Duration, sleep};

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
    available_tools: Vec<crate::api::Tool>,
    pasted_images: std::sync::Arc<std::sync::Mutex<Vec<crate::clipboard::PastedImage>>>,
    paste_counter: std::sync::Arc<std::sync::atomic::AtomicUsize>,
}

impl Repl {
    pub fn new(
        client: LlmClient,
        config: ReplConfig,
        workspace: PathBuf,
        morph_client: Option<MorphClient>,
    ) -> Result<Self> {
        // Initialize MCP manager
        let runtime = tokio::runtime::Runtime::new()
            .map_err(|e| SofosError::Config(format!("Failed to create async runtime: {}", e)))?;

        let mcp_manager = runtime.block_on(async {
            match McpManager::new(workspace.clone()).await {
                Ok(manager) => Some(manager),
                Err(e) => {
                    eprintln!("Warning: Failed to initialize MCP manager: {}", e);
                    None
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
        let emacs = Emacs::new(keybindings);
        let clipboard_mode = clipboard_edit_mode::ClipboardEditMode::new(emacs);
        let pasted_images = clipboard_mode.images_handle();
        let paste_counter = clipboard_mode.counter_handle();
        let edit_mode: Box<dyn reedline::EditMode> = Box::new(clipboard_mode);

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

        let mut repl = Self {
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
            available_tools: Vec::new(),
            pasted_images,
            paste_counter,
        };

        // Initialize available tools (needs async)
        let available_tools =
            runtime.block_on(async { repl.tool_executor.get_available_tools().await });
        repl.available_tools = available_tools;

        Ok(repl)
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

                    let (stripped, image_indices) = crate::clipboard::strip_paste_markers(&line);
                    let pasted_images: Vec<crate::clipboard::PastedImage> = if !image_indices
                        .is_empty()
                    {
                        let mut imgs = self.pasted_images.lock().unwrap_or_else(|e| e.into_inner());
                        let result: Vec<_> = image_indices
                            .iter()
                            .filter_map(|&idx| imgs.get(idx).cloned())
                            .collect();
                        println!(
                            "{} Pasted {} image(s) from clipboard",
                            "📋".bright_cyan(),
                            result.len()
                        );
                        imgs.clear();
                        self.paste_counter
                            .store(0, std::sync::atomic::Ordering::SeqCst);
                        result
                    } else {
                        self.pasted_images
                            .lock()
                            .unwrap_or_else(|e| e.into_inner())
                            .clear();
                        self.paste_counter
                            .store(0, std::sync::atomic::Ordering::SeqCst);
                        vec![]
                    };
                    line = stripped;

                    if line.is_empty() {
                        continue;
                    }

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
                    if let Err(e) = self.process_message(&line, pasted_images) {
                        if e.is_blocked() {
                            UI::print_blocked_with_hint(&e);
                        } else {
                            UI::print_error_with_hint(&e);
                        }
                    }
                    // Always save session (even after errors) to preserve conversation context
                    if let Err(e) = self.save_current_session() {
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

    fn process_message(
        &mut self,
        user_input: &str,
        pasted_images: Vec<crate::clipboard::PastedImage>,
    ) -> Result<()> {
        let (remaining_text, image_refs) = extract_image_references(user_input);

        let has_images = !image_refs.is_empty() || !pasted_images.is_empty();

        if !image_refs.is_empty() {
            println!(
                "{} Detected {} image reference(s)",
                "🔍".bright_cyan(),
                image_refs.len()
            );
        }

        let content_blocks = if has_images {
            let mut blocks: Vec<MessageContentBlock> = Vec::new();

            for pasted in &pasted_images {
                blocks.push(MessageContentBlock::Image {
                    source: ImageSource::Base64 {
                        media_type: pasted.media_type.clone(),
                        data: pasted.base64_data.clone(),
                    },
                    cache_control: None,
                });
            }
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

        if self.session_state.conversation.needs_compaction() {
            let _ = self.compact_conversation(false);
        }

        let initial_request = self.build_initial_request();

        let runtime = tokio::runtime::Runtime::new()
            .map_err(|e| SofosError::Config(format!("Failed to create async runtime: {}", e)))?;

        let use_streaming = false;
        let client_for_retry = self.client.clone();

        let response_result: Result<_> = if use_streaming {
            let printer = Arc::new(crate::ui::StreamPrinter::new());
            let p_text = printer.clone();
            let p_think = printer.clone();
            let interrupt = Arc::new(AtomicBool::new(false));

            let client = self.client.clone();
            let req = initial_request;
            let result = runtime.block_on(async move {
                client
                    .create_message_streaming(
                        req,
                        move |t| p_text.on_text_delta(t),
                        move |t| p_think.on_thinking_delta(t),
                        interrupt,
                    )
                    .await
            });

            printer.finish();
            result
        } else {
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

            let result = runtime.block_on(async {
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

            result
        };

        // Handle API errors, especially those related to invalid images
        let response = match response_result {
            Ok(resp) => resp,
            Err(e) => {
                // Check if this is an image-related API error
                if let SofosError::Api(ref msg) = e {
                    let is_400_error = msg.contains("400");
                    let is_image_error = msg.contains("Unable to download")
                        || msg.contains("invalid_request_error")
                        || msg.contains("verify the URL");

                    // Check if current message OR conversation has images
                    let current_has_images = !image_refs.is_empty();
                    let conversation_has_images = self
                        .session_state
                        .conversation
                        .messages()
                        .iter()
                        .any(|msg| {
                            use crate::api::{MessageContent, MessageContentBlock};
                            if let MessageContent::Blocks { content } = &msg.content {
                                content
                                    .iter()
                                    .any(|block| matches!(block, MessageContentBlock::Image { .. }))
                            } else {
                                false
                            }
                        });

                    let has_images = current_has_images || conversation_has_images;

                    if is_400_error && is_image_error && has_images {
                        println!(
                            "\n{} One or more image URLs in the conversation could not be loaded by the API\n",
                            "⚠️  Image loading error:".bright_yellow().bold()
                        );

                        // Backup conversation before mutating, in case the retry also fails
                        let conversation_backup =
                            self.session_state.conversation.messages().to_vec();

                        self.session_state.conversation.remove_last_message();

                        // Remove ALL images from conversation
                        let messages = self.session_state.conversation.messages();
                        let mut cleaned_messages = Vec::new();

                        for msg in messages {
                            use crate::api::{Message, MessageContent, MessageContentBlock};
                            let cleaned_msg = match &msg.content {
                                MessageContent::Blocks { content } => {
                                    let filtered_blocks: Vec<MessageContentBlock> = content
                                        .iter()
                                        .filter(|block| {
                                            !matches!(block, MessageContentBlock::Image { .. })
                                        })
                                        .cloned()
                                        .collect();

                                    if filtered_blocks.is_empty() {
                                        continue;
                                    } else {
                                        Message {
                                            role: msg.role.clone(),
                                            content: MessageContent::Blocks {
                                                content: filtered_blocks,
                                            },
                                        }
                                    }
                                }
                                _ => msg.clone(),
                            };
                            cleaned_messages.push(cleaned_msg);
                        }

                        self.session_state.conversation.clear();
                        self.session_state
                            .conversation
                            .restore_messages(cleaned_messages);

                        let error_message = if !image_refs.is_empty() {
                            "[SYSTEM ERROR: Image URLs in your message could not be loaded and have been removed from the conversation.]"
                        } else {
                            "[SYSTEM ERROR: Image URLs from a previous message could not be loaded and have been removed from the conversation. You can continue normally.]"
                        }.to_string();

                        self.session_state
                            .conversation
                            .add_user_message(error_message);
                        let new_request = self.build_initial_request();

                        println!("{}", "Retrying request without images...".dimmed());
                        println!();

                        match runtime
                            .block_on(async { client_for_retry.create_message(new_request).await })
                        {
                            Ok(resp) => resp,
                            Err(retry_err) => {
                                // Restore original conversation on retry failure
                                self.session_state.conversation.clear();
                                self.session_state
                                    .conversation
                                    .restore_messages(conversation_backup);
                                // Add error context instead of removing user message
                                self.session_state
                                    .conversation
                                    .add_assistant_with_blocks(vec![
                                        crate::api::MessageContentBlock::Text {
                                            text: format!(
                                                "[Image loading failed and retry also failed: {}. \
                                             Your message is preserved above.]",
                                                retry_err
                                            ),
                                            cache_control: None,
                                        },
                                    ]);
                                return Err(retry_err);
                            }
                        }
                    } else {
                        // Add error context so the AI knows what happened on next turn
                        self.session_state
                            .conversation
                            .add_assistant_with_blocks(vec![
                                crate::api::MessageContentBlock::Text {
                                    text: format!(
                                        "[API error: {}. I was unable to process your request.]",
                                        msg
                                    ),
                                    cache_control: None,
                                },
                            ]);
                        return Err(e);
                    }
                } else {
                    // Add error context so the AI knows what happened on next turn
                    self.session_state
                        .conversation
                        .add_assistant_with_blocks(vec![crate::api::MessageContentBlock::Text {
                            text: format!(
                                "[System error: {}. I was unable to process your request.]",
                                e
                            ),
                            cache_control: None,
                        }]);
                    return Err(e);
                }
            }
        };

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
            self.available_tools.clone(),
            use_streaming,
        );

        let result = runtime.block_on(handler.handle_response(
            response.content,
            &mut self.session_state.display_messages,
            &mut self.session_state.total_input_tokens,
            &mut self.session_state.total_output_tokens,
        ));

        // Always preserve conversation state so the AI retains context on retry
        self.session_state.conversation = handler.conversation().clone();

        match result {
            Ok(_) => Ok(()),
            Err(SofosError::Interrupted) => Ok(()),
            Err(e) => {
                // Add error context so the AI knows what happened on next turn.
                // Check last message role to maintain proper alternation —
                // the conversation could end on either role depending on where
                // the error occurred (e.g. after assistant reasoning vs after tool results).
                let error_text = format!(
                    "[System error during processing: {}. Previous actions are preserved above.]",
                    e
                );
                let last_role = self
                    .session_state
                    .conversation
                    .messages()
                    .last()
                    .map(|m| m.role.as_str());
                if last_role == Some("assistant") {
                    // Last message is assistant — add user error context
                    self.session_state.conversation.add_user_message(error_text);
                } else {
                    // Last message is user (tool results) or empty — add assistant error context
                    self.session_state
                        .conversation
                        .add_assistant_with_blocks(vec![crate::api::MessageContentBlock::Text {
                            text: error_text,
                            cache_control: None,
                        }]);
                }
                Err(e)
            }
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
        self.process_message(prompt, vec![])?;
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

        let selected_id = crate::session::select_session(sessions)?;

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
            self.refresh_available_tools();

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
            self.refresh_available_tools();

            self.session_state
                .conversation
                .add_user_message(NORMAL_MODE_MESSAGE.to_string());
            self.prompt.set_safe_mode(false);
        }
    }

    fn refresh_available_tools(&mut self) {
        if let Ok(runtime) = tokio::runtime::Runtime::new() {
            self.available_tools =
                runtime.block_on(async { self.tool_executor.get_available_tools().await });
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
        self.available_tools.clone()
    }

    /// Compact the conversation by truncating tool results and summarizing older messages.
    /// Returns Ok(true) if compaction was performed, Ok(false) if skipped.
    pub fn compact_conversation(&mut self, force: bool) -> Result<bool> {
        if !force && !self.session_state.conversation.needs_compaction() {
            return Ok(false);
        }

        let tokens_before = self.session_state.conversation.estimate_total_tokens();

        // Phase 1: Truncate large tool results in older messages
        let split_point = self.session_state.conversation.compaction_split_point();
        if split_point == 0 {
            if force {
                println!("\n{}\n", "Not enough messages to compact.".bright_yellow());
            }
            return Ok(false);
        }

        self.session_state
            .conversation
            .truncate_tool_results(split_point);

        if !force && !self.session_state.conversation.needs_compaction() {
            let tokens_after = self.session_state.conversation.estimate_total_tokens();
            println!(
                "\n{} {} -> {} tokens (tool results truncated)\n",
                "Compacted:".bright_green(),
                tokens_before,
                tokens_after
            );
            return Ok(true);
        }

        // Phase 2: Summarize older messages via the LLM
        let older_messages: Vec<_> =
            self.session_state.conversation.messages()[..split_point].to_vec();
        let serialized = ConversationHistory::serialize_messages_for_summary(&older_messages);

        let summary_system = vec![crate::api::SystemPrompt::new_cached_with_ttl(
            "You are a conversation summarizer. Produce a detailed but concise summary of the following \
             coding assistant conversation. Preserve:\n\
             1. All file paths mentioned or modified\n\
             2. Key decisions made and their rationale\n\
             3. Current state of any ongoing task\n\
             4. Any errors encountered and how they were resolved\n\n\
             Format as structured sections. Do NOT include raw file contents or verbose tool output — \
             just what was done and decided."
                .to_string(),
            None,
        )];

        let summary_request = CreateMessageRequest {
            model: self.model_config.model.clone(),
            max_tokens: 4096,
            messages: vec![crate::api::Message::user(serialized)],
            system: Some(summary_system),
            tools: None,
            stream: None,
            thinking: None,
            reasoning: None,
        };

        let runtime = tokio::runtime::Runtime::new()
            .map_err(|e| SofosError::Config(format!("Failed to create async runtime: {}", e)))?;

        let running = Arc::new(AtomicBool::new(true));
        let interrupted = Arc::new(AtomicBool::new(false));
        let running_clone = Arc::clone(&running);
        let interrupted_clone = Arc::clone(&interrupted);

        let ui_handle = std::thread::spawn(move || {
            UI::run_animation_with_interrupt(
                "Compacting conversation...".to_string(),
                "(Press ESC to cancel) ".to_string(),
                running_clone,
                interrupted_clone,
            )
        });

        let client = self.client.clone();
        let mut request_handle =
            runtime.spawn(async move { client.create_message(summary_request).await });

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

        match response_result {
            Ok(response) => {
                let summary_text: String = response
                    .content
                    .iter()
                    .filter_map(|block| {
                        if let crate::api::ContentBlock::Text { text } = block {
                            Some(text.as_str())
                        } else {
                            None
                        }
                    })
                    .collect::<Vec<_>>()
                    .join("\n");

                if summary_text.len() < 50 {
                    UI::print_warning(
                        "Compaction produced an insufficient summary. Falling back to trimming.",
                    );
                    self.session_state.conversation.fallback_trim();
                    return Ok(false);
                }

                self.session_state
                    .conversation
                    .replace_with_summary(summary_text, split_point);

                self.session_state
                    .add_tokens(response.usage.input_tokens, response.usage.output_tokens);

                let tokens_after = self.session_state.conversation.estimate_total_tokens();
                println!(
                    "{} {} -> {} tokens (saved {}%)",
                    "Compacted:".bright_green(),
                    tokens_before,
                    tokens_after,
                    if tokens_before > 0 {
                        100 - (tokens_after * 100 / tokens_before)
                    } else {
                        0
                    }
                );

                Ok(true)
            }
            Err(SofosError::Interrupted) => {
                UI::print_warning("Compaction interrupted. Falling back to trimming.");
                self.session_state.conversation.fallback_trim();
                Ok(false)
            }
            Err(e) => {
                UI::print_warning(&format!(
                    "Compaction failed: {}. Falling back to trimming.",
                    e
                ));
                self.session_state.conversation.fallback_trim();
                Ok(false)
            }
        }
    }

    pub fn handle_compact_command(&mut self) -> Result<()> {
        self.compact_conversation(true)?;
        Ok(())
    }

    /// Await the interrupt flag in an async-friendly loop (50ms poll).
    async fn wait_for_interrupt(flag: Arc<AtomicBool>) {
        while !flag.load(Ordering::Relaxed) {
            sleep(Duration::from_millis(50)).await;
        }
    }
}
