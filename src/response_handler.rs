use crate::api::{ContentBlock, CreateMessageRequest, LlmClient};
use crate::conversation::ConversationHistory;
use crate::error::{Result, SofosError};
use crate::history::DisplayMessage;
use crate::request_builder::RequestBuilder;
use crate::tools::ToolExecutor;
use crate::ui::UI;
use colored::Colorize;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

/// Handles AI's responses and manages tool execution iteration
pub struct ResponseHandler {
    client: LlmClient,
    tool_executor: ToolExecutor,
    conversation: ConversationHistory,
    ui: UI,
    model: String,
    max_tokens: u32,
    enable_thinking: bool,
    thinking_budget: u32,
}

impl ResponseHandler {
    pub fn new(
        client: LlmClient,
        tool_executor: ToolExecutor,
        conversation: ConversationHistory,
        model: String,
        max_tokens: u32,
        enable_thinking: bool,
        thinking_budget: u32,
    ) -> Self {
        Self {
            client,
            tool_executor,
            conversation,
            ui: UI::new(),
            model,
            max_tokens,
            enable_thinking,
            thinking_budget,
        }
    }

    pub async fn handle_response(
        &mut self,
        mut content_blocks: Vec<ContentBlock>,
        display_messages: &mut Vec<DisplayMessage>,
        total_input_tokens: &mut u32,
        total_output_tokens: &mut u32,
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
                self.handle_max_iterations(
                    display_messages,
                    total_input_tokens,
                    total_output_tokens,
                )
                .await?;
                return Ok(());
            }

            let (text_output, tool_uses) = self.process_content_blocks(&content_blocks);

            // Display and store assistant's text response
            if !text_output.is_empty() {
                println!("{}", "Assistant:".bright_blue().bold());
                for text in &text_output {
                    self.ui.print_assistant_text(text);
                }
                println!();

                let combined_text = text_output.join("\n");
                display_messages.push(DisplayMessage::AssistantMessage {
                    content: combined_text,
                });
            }

            // Store assistant response in conversation
            if !content_blocks.is_empty() {
                let message_blocks: Vec<crate::api::MessageContentBlock> = content_blocks
                    .iter()
                    .filter_map(crate::api::MessageContentBlock::from_content_block_for_api)
                    .collect();
                if !message_blocks.is_empty() {
                    self.conversation.add_assistant_with_blocks(message_blocks);
                }
            }

            if tool_uses.is_empty() {
                break;
            }

            let (tool_results, user_cancelled) =
                self.execute_tools(&tool_uses, display_messages).await?;

            if !tool_results.is_empty() {
                if std::env::var("SOFOS_DEBUG").is_ok() {
                    eprintln!(
                        "=== Adding {} tool results to conversation ===",
                        tool_results.len()
                    );
                }
                self.conversation.add_tool_results(tool_results);
            }

            if user_cancelled {
                if std::env::var("SOFOS_DEBUG").is_ok() {
                    eprintln!("=== Returning early due to user cancellation ===");
                }
                return Ok(());
            }

            let response = self.get_next_response(&tool_uses, display_messages).await?;

            *total_input_tokens += response.usage.input_tokens;
            *total_output_tokens += response.usage.output_tokens;

            if std::env::var("SOFOS_DEBUG").is_ok() {
                eprintln!(
                    "\n=== Response received: stop_reason={:?}, content_blocks={} ===",
                    response.stop_reason,
                    response.content.len()
                );
            }

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
                }
            }

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

    /// Process content blocks into text output and tool uses
    fn process_content_blocks(
        &self,
        content_blocks: &[ContentBlock],
    ) -> (Vec<String>, Vec<(String, String, serde_json::Value)>) {
        let mut text_output = Vec::new();
        let mut tool_uses = Vec::new();

        for block in content_blocks {
            match block {
                ContentBlock::Text { text } => {
                    if !text.trim().is_empty() {
                        text_output.push(text.clone());
                    }
                }
                ContentBlock::Thinking { thinking, .. } => {
                    self.ui.print_thinking(thinking);
                }
                ContentBlock::Summary { summary } => {
                    self.ui.print_thinking(summary);
                }
                ContentBlock::ToolUse { id, name, input } => {
                    tool_uses.push((id.clone(), name.clone(), input.clone()));
                }
                ContentBlock::ServerToolUse { name, input, .. } => {
                    if std::env::var("SOFOS_DEBUG").is_ok() {
                        eprintln!("Server tool use: {} with input: {:?}", name, input);
                    }
                }
                ContentBlock::WebSearchToolResult { content, .. } => {
                    if !content.is_empty() {
                        text_output
                            .push(format!("\n[Web search returned {} results]", content.len()));
                    }
                }
            }
        }

        (text_output, tool_uses)
    }

    async fn execute_tools(
        &self,
        tool_uses: &[(String, String, serde_json::Value)],
        display_messages: &mut Vec<DisplayMessage>,
    ) -> Result<(Vec<crate::api::MessageContentBlock>, bool)> {
        let mut tool_results = Vec::new();
        let mut user_cancelled = false;

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
                    &tool_id[..20.min(tool_id.len())]
                );
            }

            let command = if tool_name == "execute_bash" {
                tool_input.get("command").and_then(|v| v.as_str())
            } else {
                None
            };
            self.ui.print_tool_header(tool_name, command);

            let result = self.tool_executor.execute(tool_name, tool_input).await;

            match result {
                Ok(output) => {
                    if std::env::var("SOFOS_DEBUG").is_ok() {
                        eprintln!(
                            "=== Tool {} succeeded, output length: {} ===",
                            i + 1,
                            output.len()
                        );
                    }

                    let display_output =
                        UI::create_tool_display_message(tool_name, tool_input, &output);

                    if !display_output.is_empty() {
                        println!("{}", display_output.dimmed());
                        println!();
                    }

                    display_messages.push(DisplayMessage::ToolExecution {
                        tool_name: tool_name.clone(),
                        tool_input: tool_input.clone(),
                        tool_output: display_output.clone(),
                    });

                    // Collect tool result for API
                    tool_results.push(crate::api::MessageContentBlock::ToolResult {
                        tool_use_id: tool_id.clone(),
                        content: output.clone(),
                    });

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

                    display_messages.push(DisplayMessage::ToolExecution {
                        tool_name: tool_name.clone(),
                        tool_input: tool_input.clone(),
                        tool_output: error_msg.clone(),
                    });

                    tool_results.push(crate::api::MessageContentBlock::ToolResult {
                        tool_use_id: tool_id.clone(),
                        content: error_msg,
                    });
                }
            }
        }

        Ok((tool_results, user_cancelled))
    }

    async fn get_next_response(
        &mut self,
        tool_uses: &[(String, String, serde_json::Value)],
        display_messages: &mut Vec<DisplayMessage>,
    ) -> Result<crate::api::CreateMessageResponse> {
        if std::env::var("SOFOS_DEBUG").is_ok() {
            eprintln!("=== About to generate response ===");
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

        let processing = Arc::new(AtomicBool::new(true));
        let processing_interrupted = Arc::new(AtomicBool::new(false));
        let processing_clone = Arc::clone(&processing);
        let processing_interrupted_clone = Arc::clone(&processing_interrupted);
        let ui_handle = std::thread::spawn(move || {
            UI::run_animation_with_interrupt(
                "Processing...".to_string(),
                "(Press ESC to interrupt)".to_string(),
                processing_clone,
                processing_interrupted_clone,
            )
        });

        let request = self.build_request();
        let response_result = self.client.create_message(request).await;

        // Stop animation
        processing.store(false, Ordering::Relaxed);
        let _ = ui_handle.join();

        if processing_interrupted.load(Ordering::Relaxed) {
            println!(
                "\n{}",
                "Processing interrupted by user. You can now provide additional guidance."
                    .bright_yellow()
            );
            println!();

            let tools_executed: Vec<String> =
                tool_uses.iter().map(|(_, name, _)| name.clone()).collect();

            let interrupt_msg = format!(
                "INTERRUPT: The user pressed ESC while waiting for your response after tool execution. \
                 Tools that were executed: {}. The user wants to provide additional guidance before you continue. \
                 Wait for their next message.",
                tools_executed.join(", ")
            );

            self.conversation.add_user_message(interrupt_msg);

            display_messages.push(DisplayMessage::UserMessage {
                content: format!(
                    "[Interrupted after executing: {}]",
                    tools_executed.join(", ")
                ),
            });

            return Err(SofosError::Interrupted);
        }

        response_result
    }

    async fn handle_max_iterations(
        &mut self,
        display_messages: &mut Vec<DisplayMessage>,
        total_input_tokens: &mut u32,
        total_output_tokens: &mut u32,
    ) -> Result<()> {
        const MAX_TOOL_ITERATIONS: u32 = 200;

        eprintln!(
            "\n{} Maximum tool iterations reached. Stopping to prevent infinite loop.",
            "Warning:".bright_yellow().bold()
        );

        let interruption_msg = format!(
            "SYSTEM INTERRUPTION: You have reached the maximum number of tool iterations ({}). \
            This limit prevents infinite loops. Please provide a summary of what you've accomplished \
            so far and suggest how the user should proceed. Consider breaking down the task into \
            smaller steps or asking the user for clarification.",
            MAX_TOOL_ITERATIONS
        );

        self.conversation.add_user_message(interruption_msg.clone());

        display_messages.push(DisplayMessage::UserMessage {
            content: "[System: Maximum tool iterations reached]".to_string(),
        });

        // Let AI respond to the interruption
        let request = self.build_request();

        match self.client.create_message(request).await {
            Ok(response) => {
                *total_input_tokens += response.usage.input_tokens;
                *total_output_tokens += response.usage.output_tokens;

                for block in &response.content {
                    if let ContentBlock::Text { text } = block {
                        if !text.trim().is_empty() {
                            println!("{}", "Assistant:".bright_blue().bold());
                            self.ui.print_assistant_text(text);
                            println!();

                            display_messages.push(DisplayMessage::AssistantMessage {
                                content: text.clone(),
                            });
                        }
                    }
                }

                let message_blocks: Vec<crate::api::MessageContentBlock> = response
                    .content
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

        Ok(())
    }

    fn get_available_tools(&self) -> Vec<crate::api::Tool> {
        self.tool_executor.get_available_tools()
    }

    fn build_request(&self) -> CreateMessageRequest {
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

    pub fn conversation(&self) -> &ConversationHistory {
        &self.conversation
    }
}
