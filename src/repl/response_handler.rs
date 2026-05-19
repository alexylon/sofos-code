use crate::api::{ContentBlock, CreateMessageRequest, LlmClient};
use crate::config::SofosConfig;
use crate::error::{Result, SofosError};
use crate::repl::SteerBuffer;
use crate::repl::conversation::ConversationHistory;
use crate::repl::request_builder::RequestBuilder;
use crate::session::DisplayMessage;
use crate::tools::ToolExecutor;
use crate::ui::UI;
use colored::Colorize;
use std::io::Write;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use tokio::time::{Duration, sleep};

/// Handles AI's responses and manages tool execution iteration
pub struct ResponseHandler {
    client: LlmClient,
    tool_executor: ToolExecutor,
    conversation: ConversationHistory,
    ui: UI,
    model: String,
    max_tokens: u32,
    reasoning_effort: crate::api::ReasoningEffort,
    config: SofosConfig,
    available_tools: Vec<crate::api::Tool>,
    interrupt_flag: Arc<AtomicBool>,
    steer_buffer: SteerBuffer,
    session_id: String,
}

impl ResponseHandler {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        client: LlmClient,
        tool_executor: ToolExecutor,
        conversation: ConversationHistory,
        model: String,
        max_tokens: u32,
        reasoning_effort: crate::api::ReasoningEffort,
        available_tools: Vec<crate::api::Tool>,
        interrupt_flag: Arc<AtomicBool>,
        steer_buffer: SteerBuffer,
        session_id: String,
    ) -> Self {
        Self {
            client,
            tool_executor,
            conversation,
            ui: UI::new(),
            model,
            max_tokens,
            reasoning_effort,
            config: SofosConfig::default(),
            available_tools,
            interrupt_flag,
            steer_buffer,
            session_id,
        }
    }

    fn accumulate_usage(
        usage: &crate::api::Usage,
        total_input: &mut u32,
        total_output: &mut u32,
        total_cache_read: &mut u32,
        total_cache_creation: &mut u32,
        peak_single_turn_input: &mut u32,
    ) {
        *total_input += usage.input_tokens;
        *total_output += usage.output_tokens;
        *total_cache_read += usage.cache_read_input_tokens.unwrap_or(0);
        *total_cache_creation += usage.cache_creation_input_tokens.unwrap_or(0);
        if usage.input_tokens > *peak_single_turn_input {
            *peak_single_turn_input = usage.input_tokens;
        }
    }

    /// Atomically drain all pending steer messages the user typed while
    /// this turn was running. Returns `None` if the queue is empty, or
    /// `Some(text)` with the messages joined by blank lines (preserving
    /// the order they were submitted in). Poisoned locks are recovered
    /// via `into_inner` so a panic in another thread never silently
    /// swallows the user's mid-turn message.
    fn drain_steer_messages(&self) -> Option<String> {
        let mut queue = self
            .steer_buffer
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if queue.is_empty() {
            return None;
        }
        let messages: Vec<String> = std::mem::take(&mut *queue);
        Some(messages.join("\n\n"))
    }

    #[allow(clippy::too_many_arguments)]
    pub async fn handle_response(
        &mut self,
        mut content_blocks: Vec<ContentBlock>,
        mut stop_reason: Option<String>,
        display_messages: &mut Vec<DisplayMessage>,
        total_input_tokens: &mut u32,
        total_output_tokens: &mut u32,
        total_cache_read_tokens: &mut u32,
        total_cache_creation_tokens: &mut u32,
        peak_single_turn_input_tokens: &mut u32,
    ) -> Result<()> {
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

            if iteration > self.config.max_tool_iterations {
                self.handle_max_iterations(
                    display_messages,
                    total_input_tokens,
                    total_output_tokens,
                    total_cache_read_tokens,
                    total_cache_creation_tokens,
                    peak_single_turn_input_tokens,
                )
                .await?;
                return Ok(());
            }

            let truncated_by_max_tokens = matches!(stop_reason.as_deref(), Some("max_tokens"));

            let (text_output, tool_uses, had_reasoning) =
                self.process_content_blocks(&content_blocks);

            if !text_output.is_empty() {
                let combined_text = text_output.join("\n");
                display_messages.push(DisplayMessage::AssistantMessage {
                    content: combined_text,
                });
            }

            if !content_blocks.is_empty() {
                let mut message_blocks: Vec<crate::api::MessageContentBlock> = content_blocks
                    .iter()
                    .map(crate::api::MessageContentBlock::from_content_block_for_api)
                    .collect();
                if truncated_by_max_tokens {
                    // Drop every tool-related block from a truncated
                    // response. Their arguments may be half-formed
                    // JSON, and leaving a `tool_use` without the
                    // matching `tool_result` (or a server tool result
                    // without its `server_tool_use`) puts the next
                    // request in a shape the provider will reject.
                    message_blocks.retain(|block| {
                        !matches!(
                            block,
                            crate::api::MessageContentBlock::ToolUse { .. }
                                | crate::api::MessageContentBlock::ServerToolUse { .. }
                                | crate::api::MessageContentBlock::WebSearchToolResult { .. }
                        )
                    });
                    if message_blocks.is_empty() {
                        // The truncated response was tool-use only. Record
                        // a short placeholder so the conversation keeps
                        // alternating user / assistant — without it the
                        // next user turn would land directly after the
                        // previous one and the provider would reject the
                        // request.
                        message_blocks.push(crate::api::MessageContentBlock::Text {
                            text: "[Response cut off by token limit before any visible content.]"
                                .to_string(),
                            cache_control: None,
                        });
                    }
                }
                if !message_blocks.is_empty() {
                    self.conversation.add_assistant_with_blocks(message_blocks);
                }
            }

            if truncated_by_max_tokens {
                UI::print_warning("Response was cut off due to token limit.");
                eprintln!(
                    "Consider using --max-tokens with a higher value (current: {})",
                    self.max_tokens
                );
                return Ok(());
            }

            // OpenAI can return reasoning/summary-only blocks; auto-continue once to get real text
            if tool_uses.is_empty()
                && text_output.is_empty()
                && had_reasoning
                && matches!(self.client, LlmClient::OpenAI(_))
            {
                let response = self.get_next_response().await?;

                Self::accumulate_usage(
                    &response.usage,
                    total_input_tokens,
                    total_output_tokens,
                    total_cache_read_tokens,
                    total_cache_creation_tokens,
                    peak_single_turn_input_tokens,
                );

                if response.content.is_empty() {
                    println!(
                        "{}",
                        "Assistant returned reasoning but no visible response.".dimmed()
                    );
                    println!();
                    return Ok(());
                }

                stop_reason = response.stop_reason;
                content_blocks = response.content;
                continue;
            }

            if tool_uses.is_empty() {
                if text_output.is_empty() && !had_reasoning {
                    println!("{}", "Assistant returned an empty response.".dimmed());
                    println!();
                }
                return Ok(());
            }

            let (tool_results, user_cancelled) =
                self.execute_tools(&tool_uses, display_messages).await;

            if !tool_results.is_empty() {
                if std::env::var("SOFOS_DEBUG").is_ok() {
                    eprintln!(
                        "=== Adding {} tool results to conversation ===",
                        tool_results.len()
                    );
                }
                // Drain any messages the user typed while this turn was
                // running and fold them into the same user turn that
                // carries the tool results. The model sees the combined
                // turn before the next API call and can course-correct
                // without having to be interrupted.
                if let Some(steer_text) = self.drain_steer_messages() {
                    println!(
                        "{} {}",
                        "↑".bright_magenta().bold(),
                        "mid-turn message delivered to the model".bright_magenta()
                    );
                    let mut blocks = tool_results;
                    blocks.push(crate::api::MessageContentBlock::Text {
                        text: format!(
                            "[User sent this message while you were working on the current task. \
                             Take it into account and adjust your plan if needed]:\n{}",
                            steer_text
                        ),
                        cache_control: None,
                    });
                    self.conversation.add_user_with_blocks(blocks);
                } else {
                    self.conversation.add_tool_results(tool_results);
                }
            }

            if user_cancelled {
                if std::env::var("SOFOS_DEBUG").is_ok() {
                    eprintln!("=== Returning early due to user cancellation ===");
                }
                return Ok(());
            }

            let response = self.get_next_response().await?;

            Self::accumulate_usage(
                &response.usage,
                total_input_tokens,
                total_output_tokens,
                total_cache_read_tokens,
                total_cache_creation_tokens,
                peak_single_turn_input_tokens,
            );

            if std::env::var("SOFOS_DEBUG").is_ok() {
                eprintln!(
                    "\n=== Response received: stop_reason={:?}, content_blocks={} ===",
                    response.stop_reason,
                    response.content.len()
                );
            }

            if response.content.is_empty()
                && !matches!(response.stop_reason.as_deref(), Some("max_tokens"))
            {
                println!("{}", "Assistant:".bright_blue().bold());
                println!("{}", "I've completed the tool operations but didn't generate a response. Please let me know if you need any clarification.".dimmed());
                println!();
                return Ok(());
            }

            // Continue loop with new content blocks; the top-of-loop
            // check picks up `max_tokens` truncation uniformly for both
            // the initial response and any follow-up.
            stop_reason = response.stop_reason;
            content_blocks = response.content;
        }
    }

    /// Process content blocks into text output and tool uses
    fn process_content_blocks(
        &self,
        content_blocks: &[ContentBlock],
    ) -> (Vec<String>, Vec<(String, String, serde_json::Value)>, bool) {
        let mut text_output = Vec::new();
        let mut tool_uses = Vec::new();
        let mut had_reasoning = false;

        for block in content_blocks {
            match block {
                ContentBlock::Text { text } => {
                    if !text.trim().is_empty() {
                        text_output.push(text.clone());
                    }
                }
                ContentBlock::Thinking { .. } => {
                    had_reasoning = true;
                }
                ContentBlock::Summary { .. } => {
                    had_reasoning = true;
                }
                ContentBlock::Compaction { .. } => {
                    // Server-side compaction summary already streamed
                    // live. Counts as reasoning so a Compaction-only
                    // response (rare, but possible right after the
                    // server folds older turns) doesn't fall into the
                    // "Assistant returned an empty response" branch
                    // and print a misleading warning. The OpenAI
                    // auto-continue is gated on the OpenAI client so
                    // setting this on Anthropic Compaction is a no-op
                    // there.
                    had_reasoning = true;
                }
                ContentBlock::Reasoning { .. } => {
                    had_reasoning = true;
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

        (text_output, tool_uses, had_reasoning)
    }

    /// Run every tool in the assistant's `tool_use` batch and return
    /// a `ToolResult` for each one — including a synthesised note for
    /// any tool we skip after a cancellation. Returns `(results,
    /// user_cancelled)` directly, without a `Result`, so a caller
    /// cannot accidentally short-circuit with `?` and leave a
    /// half-batch on the wire: every `ToolUse` MUST be paired with a
    /// matching `ToolResult` on the immediate next user turn or the
    /// provider 400s, and the saved session loads dead.
    async fn execute_tools(
        &self,
        tool_uses: &[(String, String, serde_json::Value)],
        display_messages: &mut Vec<DisplayMessage>,
    ) -> (Vec<crate::api::MessageContentBlock>, bool) {
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

            let command = if tool_name == crate::tools::ToolName::ExecuteBash.as_str() {
                tool_input.get("command").and_then(|v| v.as_str())
            } else {
                None
            };
            self.ui.print_tool_header(tool_name, command);

            // Hide cursor during bash execution
            if tool_name == crate::tools::ToolName::ExecuteBash.as_str() {
                print!("\x1B[?25l");
                let _ = std::io::stdout().flush();
            }

            let result = self.tool_executor.execute(tool_name, tool_input).await;

            // Show cursor and add newline after bash execution completes
            if tool_name == crate::tools::ToolName::ExecuteBash.as_str() {
                print!("\x1B[?25h");
                println!();
            }

            match result {
                Ok(output) => {
                    if std::env::var("SOFOS_DEBUG").is_ok() {
                        eprintln!(
                            "=== Tool {} succeeded, output length: {} ===",
                            i + 1,
                            output.text().len()
                        );
                    }

                    let display_output = UI::create_tool_display_message(
                        tool_name,
                        tool_input,
                        output.display_text(),
                    );

                    if !display_output.is_empty() {
                        UI::shared().print_tool_output(&display_output);
                    }

                    display_messages.push(DisplayMessage::ToolExecution {
                        tool_name: tool_name.clone(),
                        tool_input: tool_input.clone(),
                        tool_output: display_output.clone(),
                    });

                    tool_results.push(crate::api::MessageContentBlock::ToolResult {
                        tool_use_id: tool_id.clone(),
                        content: output.text().to_string(),
                        cache_control: None,
                    });

                    for image in output.images() {
                        let source = match image {
                            crate::mcp::manager::ImageData::Base64 { mime_type, data } => {
                                crate::api::ImageSource::Base64 {
                                    media_type: mime_type.clone(),
                                    data: data.clone(),
                                }
                            }
                            crate::mcp::manager::ImageData::Url { url } => {
                                crate::api::ImageSource::Url { url: url.clone() }
                            }
                        };
                        tool_results.push(crate::api::MessageContentBlock::Image {
                            source,
                            cache_control: None,
                        });
                    }

                    if output.text().starts_with("File deletion cancelled by user")
                        || output
                            .text()
                            .starts_with("Directory deletion cancelled by user")
                    {
                        user_cancelled = true;
                        // Synthesize cancellation results for every
                        // tool that hasn't run yet. Every assistant
                        // `ToolUse` block must be paired with a
                        // matching `ToolResult` on the very next user
                        // turn — Anthropic returns 400 on the next
                        // request otherwise. Each skipped tool gets a
                        // short note so the model sees why nothing
                        // happened the next time it looks at this turn.
                        for (skipped_id, _, _) in &tool_uses[i + 1..] {
                            tool_results.push(crate::api::MessageContentBlock::ToolResult {
                                tool_use_id: skipped_id.clone(),
                                content: "Tool execution skipped: an earlier deletion in this batch was cancelled by the user.".to_string(),
                                cache_control: None,
                            });
                        }
                        break;
                    }
                }
                Err(e) => {
                    if std::env::var("SOFOS_DEBUG").is_ok() {
                        eprintln!("=== Tool {} failed: {} ===", i + 1, e);
                    }

                    let error_msg = format!("{}", e);

                    if e.is_blocked() {
                        UI::print_blocked_with_hint(&e);
                    } else {
                        UI::print_error_with_hint(&e);
                    }
                    println!();

                    display_messages.push(DisplayMessage::ToolExecution {
                        tool_name: tool_name.clone(),
                        tool_input: tool_input.clone(),
                        tool_output: error_msg.clone(),
                    });

                    tool_results.push(crate::api::MessageContentBlock::ToolResult {
                        tool_use_id: tool_id.clone(),
                        content: error_msg,
                        cache_control: None,
                    });
                }
            }
        }

        (tool_results, user_cancelled)
    }

    async fn get_next_response(&mut self) -> Result<crate::api::CreateMessageResponse> {
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

        let printer = Arc::new(crate::ui::StreamPrinter::new());
        let p_text = printer.clone();
        let p_think = printer.clone();
        let interrupt = Arc::clone(&self.interrupt_flag);

        let request = self.build_request();
        let response_result = self
            .client
            .create_message_streaming(
                request,
                move |t| p_text.on_text_delta(t),
                move |t| p_think.on_thinking_delta(t),
                interrupt,
            )
            .await;

        printer.finish();
        response_result
    }

    /// Await the interrupt flag in an async-friendly loop (50ms poll).
    async fn wait_for_interrupt(flag: Arc<AtomicBool>) {
        while !flag.load(Ordering::Relaxed) {
            sleep(Duration::from_millis(50)).await;
        }
    }

    /// Race `fut` against the shared interrupt flag. Spawns the future
    /// on the current runtime (so it keeps running even while we're
    /// `.await`ing the select), and if ESC fires before the future
    /// completes the spawned task is aborted and the function returns
    /// `Err(SofosError::Interrupted)`. A tokio `JoinError` is wrapped
    /// as `SofosError::Join` — `tokio::spawn` requires
    /// `Send + 'static`, so the future's captures must be owned (or
    /// `Arc`d).
    ///
    /// Used by `handle_max_iterations` so ESC can abort the in-flight
    /// summary request rather than blocking the user until the server
    /// responds.
    async fn run_interruptible<T>(
        &self,
        fut: impl std::future::Future<Output = Result<T>> + Send + 'static,
    ) -> Result<T>
    where
        T: Send + 'static,
    {
        let mut handle = tokio::spawn(fut);
        let interrupt_flag = Arc::clone(&self.interrupt_flag);
        tokio::select! {
            res = &mut handle => match res {
                Ok(inner) => inner,
                Err(e) => Err(SofosError::Join(format!("{}", e))),
            },
            _ = Self::wait_for_interrupt(interrupt_flag) => {
                handle.abort();
                Err(SofosError::Interrupted)
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    async fn handle_max_iterations(
        &mut self,
        display_messages: &mut Vec<DisplayMessage>,
        total_input_tokens: &mut u32,
        total_output_tokens: &mut u32,
        total_cache_read_tokens: &mut u32,
        total_cache_creation_tokens: &mut u32,
        peak_single_turn_input_tokens: &mut u32,
    ) -> Result<()> {
        UI::print_warning("Maximum tool iterations reached. Stopping to prevent infinite loop.");

        let interruption_msg = format!(
            "SYSTEM INTERRUPTION: You have reached the maximum number of tool iterations ({}). \
            This limit prevents infinite loops. Please provide a summary of what you've accomplished \
            so far and suggest how the user should proceed. Consider breaking down the task into \
            smaller steps or asking the user for clarification.",
            self.config.max_tool_iterations
        );

        self.conversation.add_user_message(interruption_msg.clone());

        display_messages.push(DisplayMessage::UserMessage {
            content: "[System: Maximum tool iterations reached]".to_string(),
        });

        // Let the assistant respond to the interruption. Use
        // `run_interruptible` so ESC during this final summary cancels
        // the HTTP call instead of blocking on the server.
        //
        // The recovery request is built without tools. Sending the
        // tools array would let the assistant come back with another
        // `tool_use` block; persisting that block without the matching
        // `tool_result` puts the session in a shape the provider
        // rejects on the next request and breaks `--resume`.
        let mut request = self.build_request();
        request.tools = None;
        let client = self.client.clone();
        let response_result = self
            .run_interruptible(async move { client.create_message(request).await })
            .await;

        match response_result {
            Ok(response) => {
                Self::accumulate_usage(
                    &response.usage,
                    total_input_tokens,
                    total_output_tokens,
                    total_cache_read_tokens,
                    total_cache_creation_tokens,
                    peak_single_turn_input_tokens,
                );

                for block in &response.content {
                    if let ContentBlock::Text { text } = block {
                        if !text.trim().is_empty() {
                            println!("{}", "Assistant:".bright_blue().bold());
                            self.ui.print_assistant_text(text)?;

                            display_messages.push(DisplayMessage::AssistantMessage {
                                content: text.clone(),
                            });
                        }
                    }
                }

                // Even though the recovery request carried no tools,
                // strip any tool-related blocks defensively before
                // appending. A provider that ignores `tools = None`
                // (or returns a cached tool_use from a previous turn)
                // would otherwise leave an orphan in the saved session.
                let message_blocks: Vec<crate::api::MessageContentBlock> = response
                    .content
                    .iter()
                    .map(crate::api::MessageContentBlock::from_content_block_for_api)
                    .filter(|block| {
                        !matches!(
                            block,
                            crate::api::MessageContentBlock::ToolUse { .. }
                                | crate::api::MessageContentBlock::ServerToolUse { .. }
                                | crate::api::MessageContentBlock::WebSearchToolResult { .. }
                        )
                    })
                    .collect();
                if !message_blocks.is_empty() {
                    self.conversation.add_assistant_with_blocks(message_blocks);
                }
            }
            Err(e) => {
                UI::print_error(&format!("Failed to get summary after interruption: {}", e));
                return Err(e);
            }
        }

        Ok(())
    }

    fn get_available_tools(&self) -> Vec<crate::api::Tool> {
        self.available_tools.clone()
    }

    fn build_request(&self) -> CreateMessageRequest {
        RequestBuilder::new(
            &self.client,
            &self.model,
            self.max_tokens,
            &self.conversation,
            self.get_available_tools(),
            self.reasoning_effort,
            &self.session_id,
        )
        .build()
    }

    pub fn conversation(&self) -> &ConversationHistory {
        &self.conversation
    }
}

#[cfg(test)]
mod truncation_tests {
    use super::*;
    use crate::api::{
        AnthropicClient, ContentBlock, MessageContent, MessageContentBlock, ReasoningEffort,
    };
    use crate::tools::ToolExecutor;
    use serde_json::json;
    use tempfile::TempDir;

    fn build_handler() -> (TempDir, ResponseHandler) {
        let workspace = TempDir::new().expect("temp workspace");
        let client = LlmClient::Anthropic(
            AnthropicClient::new("test-key".to_string()).expect("anthropic client"),
        );
        let tool_executor =
            ToolExecutor::new(workspace.path().to_path_buf(), None, None, false, false)
                .expect("tool executor");
        let conversation = ConversationHistory::new();
        let interrupt = Arc::new(AtomicBool::new(false));
        let steer = Arc::new(std::sync::Mutex::new(Vec::new()));
        let handler = ResponseHandler::new(
            client,
            tool_executor,
            conversation,
            "claude-sonnet-4-6".to_string(),
            8_192,
            ReasoningEffort::Off,
            Vec::new(),
            interrupt,
            steer,
            "test-session".to_string(),
        );
        (workspace, handler)
    }

    fn assistant_blocks(handler: &ResponseHandler) -> Vec<MessageContentBlock> {
        let last = handler
            .conversation()
            .messages()
            .last()
            .cloned()
            .expect("conversation has at least one message");
        assert_eq!(
            last.role, "assistant",
            "expected the last turn to be assistant"
        );
        match last.content {
            MessageContent::Blocks { content } => content,
            MessageContent::Text { content } => vec![MessageContentBlock::Text {
                text: content,
                cache_control: None,
            }],
        }
    }

    fn block_kinds(blocks: &[MessageContentBlock]) -> Vec<&'static str> {
        blocks
            .iter()
            .map(|b| match b {
                MessageContentBlock::Text { .. } => "text",
                MessageContentBlock::Image { .. } => "image",
                MessageContentBlock::Thinking { .. } => "thinking",
                MessageContentBlock::Summary { .. } => "summary",
                MessageContentBlock::Compaction { .. } => "compaction",
                MessageContentBlock::Reasoning { .. } => "reasoning",
                MessageContentBlock::ToolUse { .. } => "tool_use",
                MessageContentBlock::ServerToolUse { .. } => "server_tool_use",
                MessageContentBlock::ToolResult { .. } => "tool_result",
                MessageContentBlock::WebSearchToolResult { .. } => "web_search_tool_result",
            })
            .collect()
    }

    fn call_handler(handler: &mut ResponseHandler, blocks: Vec<ContentBlock>, stop: Option<&str>) {
        let mut display = Vec::new();
        let (mut a, mut b, mut c, mut d, mut e) = (0, 0, 0, 0, 0);
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("test runtime");
        rt.block_on(handler.handle_response(
            blocks,
            stop.map(str::to_string),
            &mut display,
            &mut a,
            &mut b,
            &mut c,
            &mut d,
            &mut e,
        ))
        .expect("handle_response should not error on the truncation early-return paths");
    }

    /// A truncated response that contains text plus a partial `tool_use`
    /// must keep the text in the conversation and drop the `tool_use`,
    /// because storing a tool call without the matching tool result
    /// puts the next request into a shape the provider will reject.
    #[test]
    fn truncated_with_text_and_tool_use_drops_only_the_tool_use() {
        let (_ws, mut handler) = build_handler();
        let blocks = vec![
            ContentBlock::Text {
                text: "Looking at the file...".to_string(),
            },
            ContentBlock::ToolUse {
                id: "tool_001".to_string(),
                name: "read_file".to_string(),
                input: json!({ "path": "src/main.rs" }),
            },
        ];

        call_handler(&mut handler, blocks, Some("max_tokens"));

        let kinds = block_kinds(&assistant_blocks(&handler));
        assert_eq!(kinds, vec!["text"], "tool_use must not survive truncation");
    }

    /// When the only block in a truncated response is a `tool_use`,
    /// stripping it would leave the assistant turn empty and the next
    /// user message would land directly after the prior user message,
    /// which the provider rejects. A placeholder text block keeps the
    /// conversation alternating.
    #[test]
    fn truncated_with_only_tool_use_inserts_placeholder_text() {
        let (_ws, mut handler) = build_handler();
        let blocks = vec![ContentBlock::ToolUse {
            id: "tool_002".to_string(),
            name: "read_file".to_string(),
            input: json!({ "path": "src/lib.rs" }),
        }];

        call_handler(&mut handler, blocks, Some("max_tokens"));

        let assistant = assistant_blocks(&handler);
        assert_eq!(block_kinds(&assistant), vec!["text"]);
        match &assistant[0] {
            MessageContentBlock::Text { text, .. } => {
                assert!(
                    text.contains("cut off"),
                    "placeholder should mention truncation, got: {text}"
                );
            }
            other => panic!("expected text placeholder, got {other:?}"),
        }
    }

    /// A truncated response that also carries a `WebSearchToolResult`
    /// must drop the result alongside the matching `server_tool_use`.
    /// Keeping the result without its use orphans the pair and the
    /// next request fails.
    #[test]
    fn truncated_drops_server_tool_use_and_web_search_result_together() {
        let (_ws, mut handler) = build_handler();
        let blocks = vec![
            ContentBlock::Text {
                text: "Here is what I found:".to_string(),
            },
            ContentBlock::ServerToolUse {
                id: "srv_001".to_string(),
                name: "web_search".to_string(),
                input: json!({ "query": "rust async" }),
            },
            ContentBlock::WebSearchToolResult {
                tool_use_id: "srv_001".to_string(),
                content: Vec::new(),
            },
        ];

        call_handler(&mut handler, blocks, Some("max_tokens"));

        let kinds = block_kinds(&assistant_blocks(&handler));
        assert_eq!(
            kinds,
            vec!["text"],
            "server_tool_use and its web_search_tool_result must be dropped together"
        );
    }

    /// A non-truncated response that contains only text and ends the
    /// turn (no tool calls) should land unchanged in the conversation,
    /// without the truncation filter or placeholder being applied.
    #[test]
    fn non_truncated_text_only_response_is_passed_through() {
        let (_ws, mut handler) = build_handler();
        let blocks = vec![ContentBlock::Text {
            text: "All done.".to_string(),
        }];

        call_handler(&mut handler, blocks, Some("end_turn"));

        let assistant = assistant_blocks(&handler);
        assert_eq!(block_kinds(&assistant), vec!["text"]);
        match &assistant[0] {
            MessageContentBlock::Text { text, .. } => {
                assert_eq!(text, "All done.");
            }
            other => panic!("expected the original text block, got {other:?}"),
        }
    }

    /// Reasoning and thinking blocks must survive truncation — they
    /// have no pairing requirement with a later message, so dropping
    /// them would lose useful context for the next turn.
    #[test]
    fn truncated_keeps_thinking_and_reasoning_blocks() {
        let (_ws, mut handler) = build_handler();
        let blocks = vec![
            ContentBlock::Thinking {
                thinking: "Need to read the file first.".to_string(),
                signature: "sig".to_string(),
            },
            ContentBlock::Text {
                text: "Let me look.".to_string(),
            },
            ContentBlock::ToolUse {
                id: "tool_003".to_string(),
                name: "read_file".to_string(),
                input: json!({ "path": "x" }),
            },
        ];

        call_handler(&mut handler, blocks, Some("max_tokens"));

        let kinds = block_kinds(&assistant_blocks(&handler));
        assert_eq!(kinds, vec!["thinking", "text"]);
    }
}
