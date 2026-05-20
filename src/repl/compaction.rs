//! Conversation compaction: truncates large tool-result blocks in older
//! messages, and (when truncation alone is not enough) summarises the
//! pre-split prefix through a one-shot LLM call. Drives both the
//! automatic threshold check from [`Repl::process_message`] and the
//! explicit `/compact` command.

use crate::api::CreateMessageRequest;
use crate::error::{Result, SofosError};
use crate::repl::{ConversationHistory, Repl};
use crate::ui::UI;
use colored::Colorize;
use std::sync::Arc;

impl Repl {
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

        // Truncate a clone first: commit the truncation only when it
        // alone frees enough tokens, so a failed or interrupted phase 2
        // never persists half-shortened tool results.
        if !force {
            let mut truncated = self.session_state.conversation.clone();
            truncated.truncate_tool_results(split_point);
            if !truncated.needs_compaction() {
                let tokens_after = truncated.estimate_total_tokens();
                self.session_state.conversation = truncated;
                println!(
                    "\n{} {} -> {} tokens (tool results truncated)\n",
                    "Compacted:".bright_green(),
                    tokens_before,
                    tokens_after
                );
                return Ok(true);
            }
        }

        // Phase 2: Summarize older messages via the LLM, leaving the
        // history un-truncated so a failed summary trims whole messages.
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
            output_config: None,
            reasoning: None,
            // Use a distinct cache key for the summary call. The
            // summarization system prompt and serialized-history user
            // turn share nothing with regular turns, so reusing the
            // session id would just thrash the OpenAI prompt-cache
            // shard between the two prefixes.
            prompt_cache_key: Some(format!("{}-summary", self.session_state.session_id)),
            // The summarization call is itself a one-shot request, not
            // a long-running conversation, so server-side compaction
            // would be a no-op even on supported models.
            context_management: None,
        };

        let interrupt_flag = Arc::clone(&self.interrupt_flag);
        let client = self.client.clone();
        let mut request_handle = self
            .runtime
            .spawn(async move { client.create_message(summary_request).await });

        let response_result = self.runtime.block_on(async {
            tokio::select! {
                res = &mut request_handle => {
                    match res {
                        Ok(inner) => inner,
                        Err(e) => Err(SofosError::Join(format!("{}", e)))
                    }
                }
                _ = Self::wait_for_interrupt(Arc::clone(&interrupt_flag)) => {
                    request_handle.abort();
                    Err(SofosError::Interrupted)
                }
            }
        });

        match response_result {
            Ok(response) => {
                // Bill the summary call before the length gate; the
                // tokens are spent regardless of whether the summary
                // ends up being used or discarded by the fallback.
                self.session_state.add_usage(&response.usage);

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

                let tokens_after = self.session_state.conversation.estimate_total_tokens();
                let saved_percent = if tokens_before > 0 {
                    // The summary can be longer than what it replaced
                    // on short histories. Saturate so the "saved" line
                    // reports 0% instead of underflowing the subtraction.
                    let shrunk = tokens_before.saturating_sub(tokens_after);
                    shrunk * 100 / tokens_before
                } else {
                    0
                };
                println!(
                    "{} {} -> {} tokens (saved {}%)",
                    "Compacted:".bright_green(),
                    tokens_before,
                    tokens_after,
                    saved_percent
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
}
