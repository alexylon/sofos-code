use crate::api::LlmClient::Anthropic;
use crate::api::{CreateMessageRequest, LlmClient, MorphClient};
use crate::conversation::ConversationHistory;
use crate::error::{Result, SofosError};
use crate::history::{DisplayMessage, HistoryManager};
use crate::request_builder::RequestBuilder;
use crate::response_handler::ResponseHandler;
use crate::tools::ToolExecutor;
use crate::ui::UI;
use colored::Colorize;
use rustyline::error::ReadlineError;
use rustyline::DefaultEditor;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

pub struct Repl {
    client: LlmClient,
    tool_executor: ToolExecutor,
    history_manager: HistoryManager,
    ui: UI,
    editor: DefaultEditor,
    model: String,
    max_tokens: u32,
    enable_thinking: bool,
    thinking_budget: u32,
    session_id: String,
    conversation: ConversationHistory,
    display_messages: Vec<DisplayMessage>,
    total_input_tokens: u32,
    total_output_tokens: u32,
    safe_mode: bool,
}

const SAFE_MODE_MESSAGE: &str = "[SYSTEM: Safe (read-only) mode has been enabled. \
                                No file modifications or bash commands are allowed.\
                                Available tools: list_directory, read_file and web_search.]";

impl Repl {
    pub fn new(
        client: LlmClient,
        model: String,
        max_tokens: u32,
        workspace: PathBuf,
        morph_client: Option<MorphClient>,
        enable_thinking: bool,
        thinking_budget: u32,
        safe_mode: bool,
    ) -> Result<Self> {
        let tool_executor = ToolExecutor::new(workspace.clone(), morph_client, safe_mode)?;

        let has_morph = tool_executor.has_morph();
        let has_code_search = tool_executor.has_code_search();

        let history_manager = HistoryManager::new(workspace.clone())?;

        // Load custom instructions
        let custom_instructions = history_manager.load_custom_instructions()?;

        if custom_instructions.is_some() {
            eprintln!("{}", "Loaded custom instructions".bright_green());
        }

        // Validate thinking budget
        if enable_thinking && thinking_budget >= max_tokens {
            return Err(SofosError::Config(format!(
                "thinking_budget ({}) must be less than max_tokens ({})",
                thinking_budget, max_tokens
            )));
        }

        let mut conversation =
            ConversationHistory::with_features(has_morph, has_code_search, custom_instructions);

        let display_messages = Vec::new();

        if safe_mode {
            conversation.add_user_message(SAFE_MODE_MESSAGE.to_string());
        }

        let editor = DefaultEditor::new()
            .map_err(|e| SofosError::Config(format!("Failed to create editor: {}", e)))?;

        let session_id = HistoryManager::generate_session_id();

        let ui = UI::new();

        Ok(Self {
            client,
            tool_executor,
            history_manager,
            ui,
            editor,
            model,
            max_tokens,
            enable_thinking,
            thinking_budget,
            session_id,
            conversation,
            display_messages,
            total_input_tokens: 0,
            total_output_tokens: 0,
            safe_mode,
        })
    }

    pub fn run(&mut self) -> Result<()> {
        UI::print_welcome();

        loop {
            let symbol = if self.safe_mode { "λ:" } else { "λ>" };
            let readline = self
                .editor
                .readline(&format!("{} ", symbol.bright_green().bold()));

            match readline {
                Ok(line) => {
                    let line = line.trim();

                    if line.is_empty() {
                        continue;
                    }

                    let _ = self.editor.add_history_entry(line);

                    match line.to_lowercase().as_str() {
                        "/exit" | "/quit" | "/q" => {
                            self.save_current_session()?;
                            UI::display_session_summary(
                                &self.model,
                                self.total_input_tokens,
                                self.total_output_tokens,
                            );
                            UI::print_goodbye();
                            break;
                        }
                        "/clear" => {
                            self.handle_clear();
                            self.conversation.add_user_message(
                                "The session history has been cleared".to_string(),
                            );
                            continue;
                        }
                        "/resume" => {
                            if let Err(e) = self.handle_resume() {
                                eprintln!("{} {}", "Error:".bright_red().bold(), e);
                            }
                            continue;
                        }
                        "/think on" => {
                            self.handle_think_on();
                            continue;
                        }
                        "/think off" => {
                            self.handle_think_off();
                            continue;
                        }
                        "/think" => {
                            self.handle_think_status();
                            continue;
                        }
                        "/s" => {
                            if !self.safe_mode {
                                self.safe_mode = true;
                                self.tool_executor.set_safe_mode(true);
                                self.conversation
                                    .add_user_message(SAFE_MODE_MESSAGE.to_string());
                            }
                            continue;
                        }
                        "/n" => {
                            if self.safe_mode {
                                self.safe_mode = false;
                                self.tool_executor.set_safe_mode(false);
                                self.conversation.add_user_message(
                                    "[SYSTEM: Normal (unrestricted) mode has been enabled. \
                                File modifications and bash commands are now allowed.\
                                All tools are available]"
                                        .to_string(),
                                );
                            }
                            continue;
                        }
                        _ => {}
                    }

                    if let Err(e) = self.process_message(line) {
                        eprintln!("{} {}", "Error:".bright_red().bold(), e);
                    } else if let Err(e) = self.save_current_session() {
                        eprintln!(
                            "{} Failed to save session: {}",
                            "Warning:".bright_yellow(),
                            e
                        );
                    }

                    println!();
                }
                Err(ReadlineError::Interrupted) => {
                    println!("{}", "Use 'exit' to quit.".dimmed());
                }
                Err(ReadlineError::Eof) => {
                    self.save_current_session()?;
                    UI::display_session_summary(
                        &self.model,
                        self.total_input_tokens,
                        self.total_output_tokens,
                    );
                    UI::print_goodbye();
                    break;
                }
                Err(e) => {
                    eprintln!("{} {}", "Error:".bright_red().bold(), e);
                    break;
                }
            }
        }

        Ok(())
    }

    fn process_message(&mut self, user_input: &str) -> Result<()> {
        self.conversation.add_user_message(user_input.to_string());

        self.display_messages.push(DisplayMessage::UserMessage {
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
                "(Press ESC to interrupt)".to_string(),
                running_clone,
                interrupted_clone,
            )
        });

        let response_result = runtime.block_on(self.client.create_message(initial_request));

        running.store(false, Ordering::Relaxed);
        let _ = ui_handle.join();

        if interrupted.load(Ordering::Relaxed) {
            self.handle_initial_interrupt();
            return Ok(());
        }

        let response = response_result?;

        self.total_input_tokens += response.usage.input_tokens;
        self.total_output_tokens += response.usage.output_tokens;

        let mut handler = ResponseHandler::new(
            self.client.clone(),
            self.tool_executor.clone(),
            self.conversation.clone(),
            self.model.clone(),
            self.max_tokens,
            self.enable_thinking,
            self.thinking_budget,
        );

        match runtime.block_on(handler.handle_response(
            response.content,
            &mut self.display_messages,
            &mut self.total_input_tokens,
            &mut self.total_output_tokens,
        )) {
            Ok(_) => {
                // Update conversation from handler
                self.conversation = handler.conversation().clone();
                Ok(())
            }
            Err(SofosError::Interrupted) => {
                // Update conversation even on interrupt
                self.conversation = handler.conversation().clone();
                Ok(())
            }
            Err(e) => Err(e),
        }
    }

    /// Build initial request for user message
    fn build_initial_request(&self) -> CreateMessageRequest {
        RequestBuilder::new(
            &self.client,
            &self.model,
            self.max_tokens,
            &self.conversation,
            self.get_available_tools(),
            self.enable_thinking,
            self.thinking_budget,
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
            &self.model,
            self.total_input_tokens,
            self.total_output_tokens,
        );

        Ok(())
    }

    fn save_current_session(&self) -> Result<()> {
        if self.conversation.messages().is_empty() {
            return Ok(());
        }

        self.history_manager.save_session(
            &self.session_id,
            self.conversation.messages(),
            &self.display_messages,
            self.conversation.system_prompt(),
        )?;

        Ok(())
    }

    fn handle_resume(&mut self) -> Result<()> {
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

    pub fn load_session_by_id(&mut self, session_id: &str) -> Result<()> {
        let session = self.history_manager.load_session(session_id)?;

        self.session_id = session.id.clone();
        self.conversation.clear();
        self.conversation
            .restore_messages(session.api_messages.clone());
        self.display_messages = session.display_messages.clone();

        println!(
            "{} {} ({} messages)",
            "Loaded session:".bright_green(),
            session.id,
            session.api_messages.len()
        );
        println!();

        self.ui.display_session(&session);

        Ok(())
    }

    fn handle_clear(&mut self) {
        self.conversation.clear();
        self.display_messages.clear();
        self.session_id = HistoryManager::generate_session_id();
        println!("\n{}\n", "Conversation history cleared.".bright_yellow());
    }

    fn handle_think_on(&mut self) {
        self.enable_thinking = true;

        if matches!(self.client, Anthropic(_)) {
            println!(
                "\n{} (budget: {} tokens)\n",
                "Extended thinking enabled.".bright_green(),
                self.thinking_budget
            );
        } else {
            let reasoning = Some(crate::api::Reasoning::enabled());
            let effort: Option<&str> = reasoning.as_ref().map(|r| r.effort.as_str());

            if let Some(e) = effort {
                println!("\n{} {}\n", "Reasoning effort:".bright_green(), e);
            }
        }
    }

    fn handle_think_off(&mut self) {
        self.enable_thinking = false;

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

    fn handle_think_status(&self) {
        if self.enable_thinking {
            println!(
                "\n{} (budget: {} tokens)\n",
                "Extended thinking is enabled".bright_green(),
                self.thinking_budget
            );
        } else {
            println!("\n{}\n", "Extended thinking is disabled".bright_yellow());
        }
    }

    fn handle_initial_interrupt(&mut self) {
        println!(
            "\n{}",
            "Interrupted by user. You can now provide additional guidance.".bright_yellow()
        );
        println!();

        let interrupt_msg = "INTERRUPT: The user pressed ESC to interrupt the request before receiving a response. \
                             They want to provide additional guidance or clarification. Wait for their next message.";
        self.conversation
            .add_user_message(interrupt_msg.to_string());

        self.display_messages.push(DisplayMessage::UserMessage {
            content: "[Interrupted - no response received]".to_string(),
        });
    }

    fn get_available_tools(&self) -> Vec<crate::api::Tool> {
        self.tool_executor.get_available_tools()
    }
}
