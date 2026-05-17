//! Single-turn driver: takes one user message (plus any clipboard-
//! pasted images), kicks off the initial API request, and hands the
//! response off to [`crate::repl::ResponseHandler`] for the tool loop.
//! Also owns the image-error retry path that strips images from the
//! conversation and retries once before surfacing the failure.

use crate::api::{ImageSource, MessageContentBlock};
use crate::error::{Result, SofosError};
use crate::repl::{Repl, ResponseHandler};
use crate::session::DisplayMessage;
use crate::ui::UI;
use colored::Colorize;
use std::sync::Arc;
use std::time::Instant;

impl Repl {
    pub fn process_message(
        &mut self,
        user_input: &str,
        pasted_images: Vec<crate::clipboard::PastedImage>,
    ) -> Result<()> {
        // Record turn start so we can show "Finished in Xs" when the
        // model is fully done (after every text reply, tool call, and
        // continuation). Steer messages typed mid-turn don't reset
        // this — they're folded into the same turn via `SteerBuffer` and
        // the same `process_message` call keeps running until the
        // agent loop exits.
        let turn_start = Instant::now();

        let has_pasted_images = !pasted_images.is_empty();

        let content_blocks = if has_pasted_images {
            let mut blocks: Vec<MessageContentBlock> = Vec::with_capacity(pasted_images.len() + 1);

            // Image-before-text ordering matches the recommendation in
            // the Anthropic and OpenAI image-input docs.
            for pasted in &pasted_images {
                blocks.push(MessageContentBlock::Image {
                    source: ImageSource::Base64 {
                        media_type: pasted.media_type.clone(),
                        data: pasted.base64_data.clone(),
                    },
                    cache_control: None,
                });
            }

            if !user_input.trim().is_empty() {
                blocks.push(MessageContentBlock::Text {
                    text: user_input.to_string(),
                    cache_control: None,
                });
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
            // Inner failure paths already surface a warning through
            // `UI::print_warning` and fall back to `fallback_trim`,
            // so the user is never left without compaction. The
            // outer `Err` arm is only reachable from future failure
            // modes added to the helper, but log it through tracing
            // rather than swallowing it silently.
            if let Err(e) = self.compact_conversation(false) {
                tracing::warn!(error = %e, "auto-compaction returned an error");
            }
        }

        let initial_request = self.build_initial_request();

        let runtime = &self.runtime;

        let client_for_retry = self.client.clone();

        let response_result: Result<_> = {
            let printer = Arc::new(crate::ui::StreamPrinter::new());
            let p_text = printer.clone();
            let p_think = printer.clone();
            let interrupt = Arc::clone(&self.interrupt_flag);

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
        };

        // Handle API errors, especially those related to invalid images
        let response = match response_result {
            Ok(resp) => resp,
            Err(e) => {
                // Try to recover from an image-loading 400 by stripping every
                // image block from the conversation and retrying once.
                if let SofosError::Api(ref msg) = e {
                    let is_400_error = msg.contains("400");
                    let is_image_error = msg.contains("Unable to download")
                        || msg.contains("invalid_request_error")
                        || msg.contains("verify the URL");

                    let conversation_has_images =
                        self.session_state.conversation.messages().iter().any(|m| {
                            use crate::api::{MessageContent, MessageContentBlock};
                            if let MessageContent::Blocks { content } = &m.content {
                                content
                                    .iter()
                                    .any(|b| matches!(b, MessageContentBlock::Image { .. }))
                            } else {
                                false
                            }
                        });
                    let has_images = has_pasted_images || conversation_has_images;

                    if is_400_error && is_image_error && has_images {
                        println!(
                            "\n{} One or more image URLs in the conversation could not be loaded by the API\n",
                            "⚠️  Image loading error:".bright_yellow().bold()
                        );

                        // Strip every Image block in place. Surrounding text
                        // survives so the user's actual prompt isn't lost on
                        // the retry. A message that was image-only gets
                        // dropped entirely.
                        let mut cleaned_messages: Vec<crate::api::Message> = Vec::new();
                        for m in self.session_state.conversation.messages() {
                            use crate::api::{Message, MessageContent, MessageContentBlock};
                            let cleaned = match &m.content {
                                MessageContent::Blocks { content } => {
                                    let filtered: Vec<MessageContentBlock> = content
                                        .iter()
                                        .filter(|b| !matches!(b, MessageContentBlock::Image { .. }))
                                        .cloned()
                                        .collect();
                                    if filtered.is_empty() {
                                        continue;
                                    }
                                    Message {
                                        role: m.role.clone(),
                                        content: MessageContent::Blocks { content: filtered },
                                    }
                                }
                                _ => m.clone(),
                            };
                            cleaned_messages.push(cleaned);
                        }

                        self.session_state.conversation.clear();
                        self.session_state
                            .conversation
                            .restore_messages(cleaned_messages);

                        let system_note = if has_pasted_images {
                            "[SYSTEM ERROR: An image attached to your message could not be loaded and has been removed from the conversation.]"
                        } else {
                            "[SYSTEM ERROR: An image from a previous message could not be loaded and has been removed from the conversation. You can continue normally.]"
                        };
                        if !self
                            .session_state
                            .conversation
                            .append_text_to_last_user_blocks(system_note.to_string())
                        {
                            self.session_state
                                .conversation
                                .add_user_message(system_note.to_string());
                        }

                        // Backup the cleaned state so a retry failure
                        // restores the image-free conversation rather than
                        // the image-laden one that caused the 400.
                        let conversation_backup =
                            self.session_state.conversation.messages().to_vec();

                        let new_request = self.build_initial_request();

                        println!("{}", "Retrying request without images...".dimmed());
                        println!();

                        // Stream the retry with the same interrupt support
                        // as the initial request so ESC works during the
                        // second attempt.
                        let printer = Arc::new(crate::ui::StreamPrinter::new());
                        let p_text = printer.clone();
                        let p_think = printer.clone();
                        let interrupt = Arc::clone(&self.interrupt_flag);
                        let client = client_for_retry.clone();
                        let req = new_request;
                        let retry_result = runtime.block_on(async move {
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

                        match retry_result {
                            Ok(resp) => resp,
                            Err(retry_err) => {
                                self.session_state.conversation.clear();
                                self.session_state
                                    .conversation
                                    .restore_messages(conversation_backup);
                                let failure_note = format!(
                                    "[SYSTEM ERROR: Image loading failed and the retry also failed: {}.]",
                                    retry_err
                                );
                                if !self
                                    .session_state
                                    .conversation
                                    .append_text_to_last_user_blocks(failure_note.clone())
                                {
                                    self.session_state
                                        .conversation
                                        .add_user_message(failure_note);
                                }
                                return Err(retry_err);
                            }
                        }
                    } else {
                        // Non-image API error. Append a system note to the
                        // user turn that triggered the failure rather than
                        // fabricating an assistant turn (which would make
                        // the model think it wrote the error string on the
                        // next turn).
                        let note = format!(
                            "[SYSTEM ERROR: API error: {}. The request did not produce a response.]",
                            msg
                        );
                        if !self
                            .session_state
                            .conversation
                            .append_text_to_last_user_blocks(note.clone())
                        {
                            self.session_state.conversation.add_user_message(note);
                        }
                        return Err(e);
                    }
                } else {
                    // Non-API error (transport, IO, ...). Same approach as
                    // the non-image API branch.
                    let note = format!(
                        "[SYSTEM ERROR: {}. The request did not produce a response.]",
                        e
                    );
                    if !self
                        .session_state
                        .conversation
                        .append_text_to_last_user_blocks(note.clone())
                    {
                        self.session_state.conversation.add_user_message(note);
                    }
                    return Err(e);
                }
            }
        };

        self.session_state.add_usage(&response.usage);

        let mut handler = ResponseHandler::new(
            self.client.clone(),
            self.tool_executor.clone(),
            self.session_state.conversation.clone(),
            self.model_config.model.clone(),
            self.model_config.max_tokens,
            self.model_config.reasoning_effort,
            self.available_tools.clone(),
            Arc::clone(&self.interrupt_flag),
            Arc::clone(&self.steer_buffer),
            self.session_state.session_id.clone(),
        );

        let result = runtime.block_on(handler.handle_response(
            response.content,
            response.stop_reason,
            &mut self.session_state.display_messages,
            &mut self.session_state.total_input_tokens,
            &mut self.session_state.total_output_tokens,
            &mut self.session_state.total_cache_read_tokens,
            &mut self.session_state.total_cache_creation_tokens,
            &mut self.session_state.peak_single_turn_input_tokens,
        ));

        // Always preserve conversation state so the AI retains context on retry
        self.session_state.conversation = handler.conversation().clone();

        match result {
            Ok(_) => {
                println!(
                    "{}",
                    UI::format_turn_finished(turn_start.elapsed()).dimmed()
                );
                Ok(())
            }
            Err(SofosError::Interrupted) => Ok(()),
            Err(e) => {
                // Record the system error against the conversation so the
                // model sees what happened on the next turn, without
                // attributing the note to the assistant.
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
                match last_role {
                    Some("assistant") => {
                        self.session_state.conversation.add_user_message(error_text);
                    }
                    Some("user") => {
                        // Append to the existing user turn (typically the
                        // tool-results message) so we keep the user/
                        // assistant alternation the providers require and
                        // never fabricate assistant content.
                        if !self
                            .session_state
                            .conversation
                            .append_text_to_last_user_blocks(error_text.clone())
                        {
                            self.session_state.conversation.add_user_message(error_text);
                        }
                    }
                    _ => {
                        self.session_state.conversation.add_user_message(error_text);
                    }
                }
                Err(e)
            }
        }
    }
}
