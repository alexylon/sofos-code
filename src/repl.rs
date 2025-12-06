use crate::api::{AnthropicClient, ContentBlock, CreateMessageRequest, MorphClient};
use crate::conversation::ConversationHistory;
use crate::error::{Result, SofosError};
use crate::tools::{add_code_search_tool, get_tools, get_tools_with_morph, ToolExecutor};
use colored::{Colorize};
use rustyline::error::ReadlineError;
use rustyline::DefaultEditor;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::Duration;
use std::io::{self, Write};

pub struct Repl {
    client: AnthropicClient,
    conversation: ConversationHistory,
    tool_executor: ToolExecutor,
    editor: DefaultEditor,
    model: String,
    max_tokens: u32,
    recursion_depth: u32,
}

impl Repl {
    pub fn new(
        api_key: String,
        model: String,
        max_tokens: u32,
        workspace: PathBuf,
        morph_client: Option<MorphClient>,
    ) -> Result<Self> {
        let client = AnthropicClient::new(api_key)?;
        let tool_executor = ToolExecutor::new(workspace, morph_client)?;
        
        let has_morph = tool_executor.has_morph();
        let has_code_search = tool_executor.has_code_search();
        let conversation = ConversationHistory::with_features(has_morph, has_code_search);
        
        let editor = DefaultEditor::new()
            .map_err(|e| SofosError::Config(format!("Failed to create editor: {}", e)))?;

        Ok(Self {
            client,
            conversation,
            tool_executor,
            editor,
            model,
            max_tokens,
            recursion_depth: 0,
        })
    }

    pub fn run(&mut self) -> Result<()> {
        println!("{}", "Sofos - AI Coding Assistant".bright_cyan().bold());
        println!("{}", "Type your message or 'exit' to quit.".dimmed());
        println!("{}", "Type 'clear' to clear conversation history.".dimmed());
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
                            println!("{}", "Goodbye!".bright_cyan());
                            break;
                        }
                        "clear" => {
                            self.conversation.clear();
                            println!("{}", "Conversation history cleared.".bright_yellow());
                            continue;
                        }
                        _ => {}
                    }

                    if let Err(e) = self.process_message(line) {
                        eprintln!("{} {}", "Error:".bright_red().bold(), e);
                    }

                    println!();
                }
                Err(ReadlineError::Interrupted) => {
                    println!("{}", "Use 'exit' to quit.".dimmed());
                }
                Err(ReadlineError::Eof) => {
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

        let request = CreateMessageRequest {
            model: self.model.clone(),
            max_tokens: self.max_tokens,
            messages: self.conversation.messages().to_vec(),
            system: Some(self.conversation.system_prompt().to_string()),
            tools: Some(self.get_available_tools()),
            stream: None,
        };

        let runtime = tokio::runtime::Runtime::new()
            .map_err(|e| SofosError::Config(format!("Failed to create async runtime: {}", e)))?;

        let response = runtime.block_on(self.client.create_message(request))?;

        self.recursion_depth = 0;
        self.handle_response(response.content, &runtime)?;

        Ok(())
    }

    fn handle_response(
        &mut self,
        content_blocks: Vec<ContentBlock>,
        runtime: &tokio::runtime::Runtime,
    ) -> Result<()> {
        const MAX_RECURSION_DEPTH: u32 = 15;

        if std::env::var("SOFOS_DEBUG").is_ok() {
            eprintln!("\n=== handle_response: recursion_depth={}, blocks={} ===", 
                self.recursion_depth, content_blocks.len());
        }

        if self.recursion_depth >= MAX_RECURSION_DEPTH {
            eprintln!(
                "\n{} Maximum recursion depth reached. Stopping to prevent infinite loop.",
                "Warning:".bright_yellow().bold()
            );
            println!("{}", "The assistant has made the maximum number of tool calls. Please rephrase your request or break it into smaller tasks.".bright_yellow());
            println!();
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
                ContentBlock::ToolUse { id, name, input } => {
                    tool_uses.push((id.clone(), name.clone(), input.clone()));
                }
            }
        }

        if !text_output.is_empty() {
            println!("{}", "Assistant:".bright_blue().bold());
            for text in &text_output {
                println!("{}", text);
            }
            println!();
        }

        // Store the full assistant response with content blocks
        // This includes both text and tool_use blocks so the API can match tool_results
        if !content_blocks.is_empty() {
            let message_blocks: Vec<crate::api::MessageContentBlock> = content_blocks
                .iter()
                .map(|block| crate::api::MessageContentBlock::from_content_block(block))
                .collect();
            self.conversation.add_assistant_with_blocks(message_blocks);
        }

        if !tool_uses.is_empty() {
            let mut user_cancelled = false;
            let mut tool_results = Vec::new();
            
            if std::env::var("SOFOS_DEBUG").is_ok() {
                eprintln!("\n=== Executing {} tools ===", tool_uses.len());
            }
            
            for (i, (tool_id, tool_name, tool_input)) in tool_uses.iter().enumerate() {
                if std::env::var("SOFOS_DEBUG").is_ok() {
                    eprintln!("=== Tool {}/{}: {} (id: {}) ===", i + 1, tool_uses.len(), tool_name, &tool_id[..20]);
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
                            eprintln!("=== Tool {} succeeded, output length: {} ===", i + 1, output.len());
                        }
                        
                        println!("{}", output.dimmed());
                        println!();

                        // Collect tool result instead of adding immediately
                        tool_results.push(crate::api::MessageContentBlock::ToolResult {
                            tool_use_id: tool_id.clone(),
                            content: output.clone(),
                        });

                        // If deletion was cancelled, stop executing remaining tools
                        // Check for the specific cancellation messages, not just substring
                        if output.starts_with("File deletion cancelled by user") 
                            || output.starts_with("Directory deletion cancelled by user") {
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

                        // Collect error as tool result
                        tool_results.push(crate::api::MessageContentBlock::ToolResult {
                            tool_use_id: tool_id.clone(),
                            content: error_msg,
                        });
                    }
                }
            }

            if std::env::var("SOFOS_DEBUG").is_ok() {
                eprintln!("=== All {} tools executed, collected {} results ===", tool_uses.len(), tool_results.len());
            }

            // Add all tool results together in one user message
            if !tool_results.is_empty() {
                if std::env::var("SOFOS_DEBUG").is_ok() {
                    eprintln!("=== Adding {} tool results to conversation ===", tool_results.len());
                }
                self.conversation.add_tool_results(tool_results);
            } else {
                if std::env::var("SOFOS_DEBUG").is_ok() {
                    eprintln!("=== WARNING: No tool results to add! ===");
                }
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

            // After executing tools, get another response from Claude
            // Start thinking animation
            let thinking = Arc::new(AtomicBool::new(true));
            let thinking_clone = Arc::clone(&thinking);
            
            let animation_handle = thread::spawn(move || {
                let frames = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];
                let mut frame_idx = 0;
                
                while thinking_clone.load(Ordering::Relaxed) {
                    print!("\r{} {}", frames[frame_idx].truecolor(0xFF, 0x99, 0x33), "Thinking...".truecolor(0xFF, 0x99, 0x33));
                    let _ = io::stdout().flush();
                    frame_idx = (frame_idx + 1) % frames.len();
                    thread::sleep(Duration::from_millis(80));
                }
                
                // Clear the line
                print!("\r{}\r", " ".repeat(20));
                let _ = io::stdout().flush();
            });

            // Debug: show conversation history
            if std::env::var("SOFOS_DEBUG").is_ok() {
                eprintln!("\n=== DEBUG: Conversation before API call ===");
                for (i, msg) in self.conversation.messages().iter().enumerate() {
                    let content_desc = match &msg.content {
                        crate::api::MessageContent::Text { content } => format!("text({})", content.len()),
                        crate::api::MessageContent::Blocks { content } => format!("blocks({})", content.len()),
                    };
                    eprintln!("Message {}: role={}, content={}", i, msg.role, content_desc);
                }
                eprintln!("===========================================\n");
            }
            
            let request = CreateMessageRequest {
                model: self.model.clone(),
                max_tokens: self.max_tokens,
                messages: self.conversation.messages().to_vec(),
                system: Some(self.conversation.system_prompt().to_string()),
                tools: Some(self.get_available_tools()),
                stream: None,
            };

            let response = match runtime.block_on(self.client.create_message(request)) {
                Ok(resp) => {
                    // Stop animation
                    thinking.store(false, Ordering::Relaxed);
                    if let Err(e) = animation_handle.join() {
                        eprintln!("{} Animation thread panicked: {:?}", "Warning:".bright_yellow().bold(), e);
                    }
                    resp
                },
                Err(e) => {
                    // Stop animation on error
                    thinking.store(false, Ordering::Relaxed);
                    if let Err(panic_err) = animation_handle.join() {
                        eprintln!("{} Animation thread panicked: {:?}", "Warning:".bright_yellow().bold(), panic_err);
                    }
                    eprintln!("{} Failed to get response after tool execution: {}", "Error:".bright_red().bold(), e);
                    return Err(e);
                }
            };

            if std::env::var("SOFOS_DEBUG").is_ok() {
                eprintln!("\n=== Response received: stop_reason={:?}, content_blocks={} ===", 
                    response.stop_reason, response.content.len());
                for (i, block) in response.content.iter().enumerate() {
                    match block {
                        ContentBlock::Text { text } => eprintln!("  Block {}: Text({})", i, text.len()),
                        ContentBlock::ToolUse { name, .. } => eprintln!("  Block {}: ToolUse({})", i, name),
                    }
                }
            }

            // Handle different stop reasons
            if let Some(ref stop_reason) = response.stop_reason {
                if stop_reason == "max_tokens" {
                    eprintln!("\n{} Response was cut off due to token limit.", "Warning:".bright_yellow().bold());
                    eprintln!("Consider using --max-tokens with a higher value (current: {})", self.max_tokens);
                    
                    // If we got some text before hitting the limit, show it
                    if !response.content.is_empty() {
                        let has_text = response.content.iter().any(|b| matches!(b, ContentBlock::Text { .. }));
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

            // Increment recursion depth before recursive call
            self.recursion_depth += 1;
            
            if std::env::var("SOFOS_DEBUG").is_ok() {
                eprintln!("=== Making recursive call to handle_response with depth={} ===", self.recursion_depth);
            }
            
            let result = self.handle_response(response.content, runtime);
            
            if std::env::var("SOFOS_DEBUG").is_ok() {
                eprintln!("=== Returned from recursive call, depth was {} ===", self.recursion_depth);
            }
            
            return result;
        }

        Ok(())
    }

    pub fn process_single_prompt(&mut self, prompt: &str) -> Result<()> {
        println!("{} {}", ">>>".bright_green(), prompt);
        println!();
        self.process_message(prompt)?;
        Ok(())
    }
}
