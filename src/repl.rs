use crate::api::{AnthropicClient, ContentBlock, CreateMessageRequest, MorphClient};
use crate::conversation::ConversationHistory;
use crate::error::{Result, SofosError};
use crate::history::HistoryManager;
use crate::syntax::SyntaxHighlighter;
use crate::tools::{add_code_search_tool, get_tools, get_tools_with_morph, ToolExecutor};
use colored::Colorize;
use rustyline::error::ReadlineError;
use rustyline::DefaultEditor;
use std::io::{self, Write};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::Duration;

pub struct Repl {
    client: AnthropicClient,
    conversation: ConversationHistory,
    tool_executor: ToolExecutor,
    history_manager: HistoryManager,
    highlighter: SyntaxHighlighter,
    editor: DefaultEditor,
    model: String,
    max_tokens: u32,
    enable_thinking: bool,
    thinking_budget: u32,
    session_id: String,
    display_messages: Vec<crate::history::DisplayMessage>,
}

impl Repl {
    pub fn new(
        api_key: String,
        model: String,
        max_tokens: u32,
        workspace: PathBuf,
        morph_client: Option<MorphClient>,
        enable_thinking: bool,
        thinking_budget: u32,
    ) -> Result<Self> {
        let client = AnthropicClient::new(api_key)?;
        let tool_executor = ToolExecutor::new(workspace.clone(), morph_client)?;

        let has_morph = tool_executor.has_morph();
        let has_code_search = tool_executor.has_code_search();
        
        let history_manager = HistoryManager::new(workspace.clone())?;
        
        // Load custom instructions
        let custom_instructions = history_manager.load_custom_instructions()?;
        
        // Show message if custom instructions are loaded
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
        
        let conversation = ConversationHistory::with_features(has_morph, has_code_search, custom_instructions);

        let editor = DefaultEditor::new()
            .map_err(|e| SofosError::Config(format!("Failed to create editor: {}", e)))?;

        let session_id = HistoryManager::generate_session_id();

        let highlighter = SyntaxHighlighter::new();

        Ok(Self {
            client,
            conversation,
            tool_executor,
            history_manager,
            highlighter,
            editor,
            model,
            max_tokens,
            enable_thinking,
            thinking_budget,
            session_id,
            display_messages: Vec::new(),
        })
    }

    pub fn run(&mut self) -> Result<()> {
        println!("{}", "Sofos - AI Coding Assistant".bright_cyan().bold());
        println!("{}", "Type your message or 'exit' to quit.".dimmed());
        println!("{}", "Type 'clear' to clear conversation history.".dimmed());
        println!("{}", "Type 'resume' to load a previous session.".dimmed());
        println!();

        loop {
            let readline = self.editor.readline(&format!("{} ", ">>>".bright_green()));

            match readline {
                Ok(line) => {
                    let line = line.trim();

                    if line.is_empty() {
                        continue;
                    }

                    let _ = self.editor.add_history_entry(line);

                    match line.to_lowercase().as_str() {
                        "exit" | "quit" => {
                            self.save_current_session()?;
                            println!("\n{}", "Goodbye!".bright_cyan());
                            break;
                        }
                        "clear" => {
                            self.conversation.clear();
                            self.display_messages.clear();
                            self.session_id = HistoryManager::generate_session_id();
                            println!("\n{}\n", "Conversation history cleared.".bright_yellow());
                            continue;
                        }
                        "resume" => {
                            if let Err(e) = self.handle_resume() {
                                eprintln!("{} {}", "Error:".bright_red().bold(), e);
                            }
                            continue;
                        }
                        _ => {}
                    }

                    if let Err(e) = self.process_message(line) {
                        eprintln!("{} {}", "Error:".bright_red().bold(), e);
                    } else {
                        if let Err(e) = self.save_current_session() {
                            eprintln!("{} Failed to save session: {}", "Warning:".bright_yellow(), e);
                        }
                    }

                    println!();
                }
                Err(ReadlineError::Interrupted) => {
                    println!("{}", "Use 'exit' to quit.".dimmed());
                }
                Err(ReadlineError::Eof) => {
                    self.save_current_session()?;
                    println!("{}", "Goodbye!".bright_cyan());
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

    fn get_available_tools(&self) -> Vec<crate::api::Tool> {
        let mut tools = if self.tool_executor.has_morph() {
            get_tools_with_morph()
        } else {
            get_tools()
        };

        if self.tool_executor.has_code_search() {
            add_code_search_tool(&mut tools);
        }

        tools
    }

    fn process_message(&mut self, user_input: &str) -> Result<()> {
        self.conversation.add_user_message(user_input.to_string());
        
        // Track display message
        self.display_messages.push(crate::history::DisplayMessage::UserMessage {
            content: user_input.to_string(),
        });

        let thinking_config = if self.enable_thinking {
            Some(crate::api::Thinking::enabled(self.thinking_budget))
        } else {
            None
        };

        let request = CreateMessageRequest {
            model: self.model.clone(),
            max_tokens: self.max_tokens,
            messages: self.conversation.messages().to_vec(),
            system: Some(self.conversation.system_prompt().to_string()),
            tools: Some(self.get_available_tools()),
            stream: None,
            thinking: thinking_config,
        };

        let runtime = tokio::runtime::Runtime::new()
            .map_err(|e| SofosError::Config(format!("Failed to create async runtime: {}", e)))?;

        let awaiting = Arc::new(AtomicBool::new(true));
        let awaiting_clone = Arc::clone(&awaiting);

        let animation_handle = thread::spawn(move || {
            let frames = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];
            let mut frame_idx = 0;

            // Hide cursor
            print!("\x1B[?25l");
            let _ = io::stdout().flush();

            while awaiting_clone.load(Ordering::Relaxed) {
                print!(
                    "\r{} {}",
                    frames[frame_idx].truecolor(0xFF, 0x99, 0x33),
                    "Awaiting response...".truecolor(0xFF, 0x99, 0x33)
                );
                let _ = io::stdout().flush();
                frame_idx = (frame_idx + 1) % frames.len();
                thread::sleep(Duration::from_millis(80));
            }

            // Clear the line and show cursor
            print!("\r{}\r", " ".repeat(30));
            print!("\x1B[?25h");
            let _ = io::stdout().flush();
        });

        let response = runtime.block_on(self.client.create_message(request));

        // Stop animation
        awaiting.store(false, Ordering::Relaxed);
        if let Err(e) = animation_handle.join() {
            eprintln!(
                "{} Animation thread panicked: {:?}",
                "Warning:".bright_yellow().bold(),
                e
            );
        }

        let response = response?;

        self.handle_response(response.content, &runtime)?;

        Ok(())
    }

    fn handle_response(
        &mut self,
        mut content_blocks: Vec<ContentBlock>,
        runtime: &tokio::runtime::Runtime,
    ) -> Result<()> {
        const MAX_TOOL_ITERATIONS: u32 = 200;
        let mut iteration = 0;

        loop {
            iteration += 1;

            if std::env::var("SOFOS_DEBUG").is_ok() {
                eprintln!(
                    "\n=== handle_response: iteration={}, blocks={} ===",
                    iteration,
                    content_blocks.len()
                );
            }

            if iteration > MAX_TOOL_ITERATIONS {
                eprintln!(
                    "\n{} Maximum tool iterations reached. Stopping to prevent infinite loop.",
                    "Warning:".bright_yellow().bold()
                );
                
                // Inform Claude about the interruption so it can respond appropriately
                let interruption_msg = format!(
                    "SYSTEM INTERRUPTION: You have reached the maximum number of tool iterations ({}). \
                    This limit prevents infinite loops. Please provide a summary of what you've accomplished \
                    so far and suggest how the user should proceed. Consider breaking down the task into \
                    smaller steps or asking the user for clarification.",
                    MAX_TOOL_ITERATIONS
                );
                
                self.conversation.add_user_message(interruption_msg.clone());
                
                // Track this as a system message in display
                self.display_messages.push(crate::history::DisplayMessage::UserMessage {
                    content: format!("[System: Maximum tool iterations reached]"),
                });
                
                // Let Claude respond to the interruption
                let thinking_config = if self.enable_thinking {
                    Some(crate::api::Thinking::enabled(self.thinking_budget))
                } else {
                    None
                };

                let request = CreateMessageRequest {
                    model: self.model.clone(),
                    max_tokens: self.max_tokens,
                    messages: self.conversation.messages().to_vec(),
                    system: Some(self.conversation.system_prompt().to_string()),
                    tools: Some(self.get_available_tools()),
                    stream: None,
                    thinking: thinking_config,
                };
                
                match runtime.block_on(self.client.create_message(request)) {
                    Ok(response) => {
                        // Display Claude's response to the interruption
                        for block in &response.content {
                            if let ContentBlock::Text { text } = block {
                                if !text.trim().is_empty() {
                                    println!("{}", "Assistant:".bright_blue().bold());
                                    let highlighted = self.highlighter.highlight_text(text);
                                    println!("{}", highlighted);
                                    println!();
                                    
                                    self.display_messages.push(crate::history::DisplayMessage::AssistantMessage {
                                        content: text.clone(),
                                    });
                                }
                            }
                        }
                        
                        // Store the response
                        let message_blocks: Vec<crate::api::MessageContentBlock> = response.content
                            .iter()
                            .filter_map(crate::api::MessageContentBlock::from_content_block_for_api)
                            .collect();
                        if !message_blocks.is_empty() {
                            self.conversation.add_assistant_with_blocks(message_blocks);
                        }
                    }
                    Err(e) => {
                        eprintln!(
                            "{} Failed to get response after interruption: {}",
                            "Error:".bright_red().bold(),
                            e
                        );
                    }
                }
                
                return Ok(());
            }

            let mut text_output = Vec::new();
            let mut tool_uses = Vec::new();

            for block in &content_blocks {
                match block {
                    ContentBlock::Text { text } => {
                        if !text.trim().is_empty() {
                            text_output.push(text.clone());
                        }
                    }
                    ContentBlock::Thinking { thinking, .. } => {
                        if !thinking.trim().is_empty() {
                            println!("{}", "Thinking:".truecolor(0xFF, 0x99, 0x33).bold());
                            println!("{}", thinking.dimmed());
                            println!();
                        }
                    }
                    ContentBlock::ToolUse { id, name, input } => {
                        tool_uses.push((id.clone(), name.clone(), input.clone()));
                    }
                    ContentBlock::ServerToolUse { name, input, .. } => {
                        // Server-side tools (like web_search) are executed by Claude API
                        if std::env::var("SOFOS_DEBUG").is_ok() {
                            eprintln!("Server tool use: {} with input: {:?}", name, input);
                        }
                    }
                    ContentBlock::WebSearchToolResult { content, .. } => {
                        if !content.is_empty() {
                            text_output.push(format!("\n[Web search returned {} results]", content.len()));
                        }
                    }
                }
            }

            if !text_output.is_empty() {
                println!("{}", "Assistant:".bright_blue().bold());
                for text in &text_output {
                    let highlighted = self.highlighter.highlight_text(text);
                    println!("{}", highlighted);
                }
                println!();
                
                // Track assistant display message
                let combined_text = text_output.join("\n");
                self.display_messages.push(crate::history::DisplayMessage::AssistantMessage {
                    content: combined_text,
                });
            }

            // Store the full assistant response with content blocks
            // This includes both text and tool_use blocks so the API can match tool_results
            // Note: Thinking blocks are redacted (empty string) to save tokens
            if !content_blocks.is_empty() {
                let message_blocks: Vec<crate::api::MessageContentBlock> = content_blocks
                    .iter()
                    .filter_map(crate::api::MessageContentBlock::from_content_block_for_api)
                    .collect();
                if !message_blocks.is_empty() {
                    self.conversation.add_assistant_with_blocks(message_blocks);
                }
            }

            // If no tools to execute, we're done
            if tool_uses.is_empty() {
                break;
            }

            // Execute tools
            let mut user_cancelled = false;
            let mut tool_results = Vec::new();

            if std::env::var("SOFOS_DEBUG").is_ok() {
                eprintln!("\n=== Executing {} tools ===", tool_uses.len());
            }

            for (i, (tool_id, tool_name, tool_input)) in tool_uses.iter().enumerate() {
                if std::env::var("SOFOS_DEBUG").is_ok() {
                    eprintln!(
                        "=== Tool {}/{}: {} (id: {}) ===",
                        i + 1,
                        tool_uses.len(),
                        tool_name,
                        &tool_id[..20]
                    );
                }

                if tool_name == "execute_bash" {
                    if let Some(command) = tool_input.get("command").and_then(|v| v.as_str()) {
                        println!(
                            "{} {}",
                            "Executing:".bright_green().bold(),
                            command.bright_cyan()
                        );
                    }
                } else {
                    println!(
                        "{} {}",
                        "Using tool:".bright_yellow().bold(),
                        tool_name.bright_yellow()
                    );
                }

                let result = runtime.block_on(self.tool_executor.execute(tool_name, tool_input));

                match result {
                    Ok(output) => {
                        if std::env::var("SOFOS_DEBUG").is_ok() {
                            eprintln!(
                                "=== Tool {} succeeded, output length: {} ===",
                                i + 1,
                                output.len()
                            );
                        }

                        // Create display message based on tool type
                        let display_output = self.create_tool_display_message(tool_name, tool_input, &output);
                        
                        // Only print if there's a display message
                        if !display_output.is_empty() {
                            println!("{}", display_output.dimmed());
                            println!();
                        }
                        
                        // Track tool execution in display_messages with summary for quiet tools
                        self.display_messages.push(crate::history::DisplayMessage::ToolExecution {
                            tool_name: tool_name.clone(),
                            tool_input: tool_input.clone(),
                            tool_output: display_output.clone(),
                        });

                        // Collect tool result (full output for Claude)
                        tool_results.push(crate::api::MessageContentBlock::ToolResult {
                            tool_use_id: tool_id.clone(),
                            content: output.clone(),
                        });

                        // If deletion was cancelled, stop executing remaining tools
                        // Check for the specific cancellation messages, not just substring
                        if output.starts_with("File deletion cancelled by user")
                            || output.starts_with("Directory deletion cancelled by user")
                        {
                            user_cancelled = true;
                            break;
                        }
                    }
                    Err(e) => {
                        if std::env::var("SOFOS_DEBUG").is_ok() {
                            eprintln!("=== Tool {} failed: {} ===", i + 1, e);
                        }

                        let error_msg = format!("Tool execution failed: {}", e);
                        eprintln!("{} {}", "Error:".bright_red().bold(), error_msg);
                        println!();
                        
                        // Track tool execution error in display_messages
                        self.display_messages.push(crate::history::DisplayMessage::ToolExecution {
                            tool_name: tool_name.clone(),
                            tool_input: tool_input.clone(),
                            tool_output: error_msg.clone(),
                        });

                        // Collect error as tool result
                        tool_results.push(crate::api::MessageContentBlock::ToolResult {
                            tool_use_id: tool_id.clone(),
                            content: error_msg,
                        });
                    }
                }
            }

            if std::env::var("SOFOS_DEBUG").is_ok() {
                eprintln!(
                    "=== All {} tools executed, collected {} results ===",
                    tool_uses.len(),
                    tool_results.len()
                );
            }

            // Add all tool results together in one user message
            if !tool_results.is_empty() {
                if std::env::var("SOFOS_DEBUG").is_ok() {
                    eprintln!(
                        "=== Adding {} tool results to conversation ===",
                        tool_results.len()
                    );
                }
                self.conversation.add_tool_results(tool_results);
            } else if std::env::var("SOFOS_DEBUG").is_ok() {
                eprintln!("=== WARNING: No tool results to add! ===");
            }

            if std::env::var("SOFOS_DEBUG").is_ok() {
                eprintln!("=== user_cancelled={} ===", user_cancelled);
            }

            // If user cancelled deletion, don't make another API request - let them respond
            if user_cancelled {
                if std::env::var("SOFOS_DEBUG").is_ok() {
                    eprintln!("=== Returning early due to user cancellation ===");
                }
                return Ok(());
            }

            if std::env::var("SOFOS_DEBUG").is_ok() {
                eprintln!("=== About to generate response ===");
            }

            // Save session before making API call (in case of network failure)
            if let Err(e) = self.save_current_session() {
                eprintln!(
                    "{} Failed to save session before API call: {}",
                    "Warning:".bright_yellow().bold(),
                    e
                );
            }

            // After executing tools, get another response from Claude
            // Start thinking animation
            let thinking = Arc::new(AtomicBool::new(true));
            let thinking_clone = Arc::clone(&thinking);

            let animation_handle = thread::spawn(move || {
                let frames = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];
                let mut frame_idx = 0;

                // Hide cursor
                print!("\x1B[?25l");
                let _ = io::stdout().flush();

                while thinking_clone.load(Ordering::Relaxed) {
                    print!(
                        "\r{} {}",
                        frames[frame_idx].truecolor(0xFF, 0x99, 0x33),
                        "Processing...".truecolor(0xFF, 0x99, 0x33)
                    );
                    let _ = io::stdout().flush();
                    frame_idx = (frame_idx + 1) % frames.len();
                    thread::sleep(Duration::from_millis(80));
                }

                // Clear the line and show cursor
                print!("\r{}\r", " ".repeat(30));
                print!("\x1B[?25h");
                let _ = io::stdout().flush();
            });

            // Debug: show conversation history
            if std::env::var("SOFOS_DEBUG").is_ok() {
                eprintln!("\n=== DEBUG: Conversation before API call ===");
                for (i, msg) in self.conversation.messages().iter().enumerate() {
                    let content_desc = match &msg.content {
                        crate::api::MessageContent::Text { content } => {
                            format!("text({})", content.len())
                        }
                        crate::api::MessageContent::Blocks { content } => {
                            format!("blocks({})", content.len())
                        }
                    };
                    eprintln!("Message {}: role={}, content={}", i, msg.role, content_desc);
                }
                eprintln!("===========================================\n");
            }

            let thinking_config = if self.enable_thinking {
                Some(crate::api::Thinking::enabled(self.thinking_budget))
            } else {
                None
            };

            let request = CreateMessageRequest {
                model: self.model.clone(),
                max_tokens: self.max_tokens,
                messages: self.conversation.messages().to_vec(),
                system: Some(self.conversation.system_prompt().to_string()),
                tools: Some(self.get_available_tools()),
                stream: None,
                thinking: thinking_config,
            };

            let response = match runtime.block_on(self.client.create_message(request)) {
                Ok(resp) => {
                    // Stop animation
                    thinking.store(false, Ordering::Relaxed);
                    if let Err(e) = animation_handle.join() {
                        eprintln!(
                            "{} Animation thread panicked: {:?}",
                            "Warning:".bright_yellow().bold(),
                            e
                        );
                    }
                    resp
                }
                Err(e) => {
                    // Stop animation on error
                    thinking.store(false, Ordering::Relaxed);
                    if let Err(panic_err) = animation_handle.join() {
                        eprintln!(
                            "{} Animation thread panicked: {:?}",
                            "Warning:".bright_yellow().bold(),
                            panic_err
                        );
                    }
                    eprintln!(
                        "{} Failed to get response after tool execution: {}",
                        "Error:".bright_red().bold(),
                        e
                    );
                    return Err(e);
                }
            };

            if std::env::var("SOFOS_DEBUG").is_ok() {
                eprintln!(
                    "\n=== Response received: stop_reason={:?}, content_blocks={} ===",
                    response.stop_reason,
                    response.content.len()
                );
                for (i, block) in response.content.iter().enumerate() {
                    match block {
                        ContentBlock::Text { text } => {
                            eprintln!("  Block {}: Text({})", i, text.len())
                        }
                        ContentBlock::Thinking { thinking, .. } => {
                            eprintln!("  Block {}: Thinking({})", i, thinking.len())
                        }
                        ContentBlock::ToolUse { name, .. } => {
                            eprintln!("  Block {}: ToolUse({})", i, name)
                        }
                        ContentBlock::ServerToolUse { name, .. } => {
                            eprintln!("  Block {}: ServerToolUse({})", i, name)
                        }
                        ContentBlock::WebSearchToolResult { content, .. } => {
                            eprintln!("  Block {}: WebSearchToolResult({} results)", i, content.len())
                        }
                    }
                }
            }

            // Handle different stop reasons
            if let Some(ref stop_reason) = response.stop_reason {
                if stop_reason == "max_tokens" {
                    eprintln!(
                        "\n{} Response was cut off due to token limit.",
                        "Warning:".bright_yellow().bold()
                    );
                    eprintln!(
                        "Consider using --max-tokens with a higher value (current: {})",
                        self.max_tokens
                    );

                    // If we got some text before hitting the limit, show it
                    if !response.content.is_empty() {
                        let has_text = response
                            .content
                            .iter()
                            .any(|b| matches!(b, ContentBlock::Text { .. }));
                        if has_text {
                            eprintln!("Showing partial response:\n");
                        }
                    }
                }
            }

            // Check if response is empty
            if response.content.is_empty() {
                println!("{}", "Assistant:".bright_blue().bold());
                println!("{}", "I've completed the tool operations but didn't generate a response. Please let me know if you need any clarification.".dimmed());
                println!();
                return Ok(());
            }

            // Continue loop with new content blocks
            content_blocks = response.content;
        }

        Ok(())
    }

    pub fn process_single_prompt(&mut self, prompt: &str) -> Result<()> {
        println!("{} {}", ">>>".bright_green(), prompt);
        println!();
        self.process_message(prompt)?;
        self.save_current_session()?;
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
        self.conversation.restore_messages(session.api_messages.clone());
        self.display_messages = session.display_messages.clone();

        println!(
            "{} {} ({} messages)",
            "Loaded session:".bright_green(),
            session.id,
            session.api_messages.len()
        );
        println!();
        
        // Display the original conversation
        self.display_session(&session);

        Ok(())
    }
    
    fn display_session(&self, session: &crate::history::Session) {
        if session.display_messages.is_empty() {
            println!("{}", "Note: No display history available for this session.".dimmed());
            println!();
            return;
        }
        
        println!("{}", "═".repeat(80).bright_cyan());
        println!("{}", "Previous Conversation:".bright_cyan().bold());
        println!("{}", "═".repeat(80).bright_cyan());
        println!();
        
        for display_msg in &session.display_messages {
            match display_msg {
                crate::history::DisplayMessage::UserMessage { content } => {
                    println!("{} {}", ">>>".bright_green(), content);
                    println!();
                }
                crate::history::DisplayMessage::AssistantMessage { content } => {
                    println!("{}", "Assistant:".bright_blue().bold());
                    let highlighted = self.highlighter.highlight_text(content);
                    println!("{}", highlighted);
                    println!();
                }
                crate::history::DisplayMessage::ToolExecution { tool_name, tool_input: _, tool_output } => {
                    if tool_name == "execute_bash" {
                        if let Ok(input_val) = serde_json::from_value::<serde_json::Value>(
                            serde_json::to_value(&tool_output).unwrap_or_default()
                        ) {
                            if let Some(command) = input_val.get("command").and_then(|v| v.as_str()) {
                                println!(
                                    "{} {}",
                                    "Executing:".bright_green().bold(),
                                    command.bright_cyan()
                                );
                            }
                        }
                    } else {
                        println!(
                            "{} {}",
                            "Using tool:".bright_yellow().bold(),
                            tool_name.bright_yellow()
                        );
                    }
                    println!("{}", tool_output.dimmed());
                    println!();
                }
            }
        }
        
        println!("{}", "═".repeat(80).bright_cyan());
        println!();
    }

    fn create_tool_display_message(
        &self,
        tool_name: &str,
        tool_input: &serde_json::Value,
        output: &str,
    ) -> String {
        match tool_name {
            "read_file" => {
                let file_path = tool_input.get("path")
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                
                let offset = tool_input.get("offset")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(1);
                
                // Count actual lines in output
                let line_count = output.lines().count() as u64;
                
                if line_count == 0 {
                    if file_path.is_empty() {
                        format!("Read file (empty or not found)")
                    } else {
                        format!("Read file from {} - empty or not found", file_path.bright_cyan())
                    }
                } else {
                    let end_line = offset + line_count - 1;
                    if file_path.is_empty() {
                        format!("Read lines {}-{}", offset, end_line)
                    } else {
                        format!("Read lines {}-{} from {}", offset, end_line, file_path.bright_cyan())
                    }
                }
            }
            "list_directory" => {
                let path = tool_input.get("path")
                    .and_then(|v| v.as_str())
                    .unwrap_or(".");
                
                // Count number of items listed
                let item_count = output.lines()
                    .filter(|line| !line.trim().is_empty() && !line.starts_with("Contents of"))
                    .count();
                
                if item_count == 0 {
                    format!("Found 0 items in {}", path.bright_cyan())
                } else if item_count == 1 {
                    format!("Found 1 item in {}", path.bright_cyan())
                } else {
                    format!("Found {} items in {}", item_count, path.bright_cyan())
                }
            }
            "morph_edit_file" => {
                // For morph edits, show the full output including the diff
                output.to_string()
            }
            _ => {
                // For all other tools, return the full output
                output.to_string()
            }
        }
    }
}
