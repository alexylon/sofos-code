//! Message-API methods on [`ConversationHistory`]: append new user,
//! assistant, and tool-result messages; restore an entire history;
//! pop the rolling message for error recovery; and the basic read
//! accessors for `messages` / `system_prompt`. Append paths run
//! [`Self::trim_if_needed`] so the budget is enforced on every write.

use crate::api::{Message, MessageContentBlock, SystemPrompt};
use crate::repl::conversation::ConversationHistory;

impl ConversationHistory {
    pub fn add_user_message(&mut self, content: String) {
        self.messages.push(Message::user(content));
        self.trim_if_needed();
    }

    pub fn add_user_with_blocks(&mut self, blocks: Vec<MessageContentBlock>) {
        self.messages.push(Message::user_with_blocks(blocks));
        self.trim_if_needed();
    }

    pub fn add_assistant_with_blocks(&mut self, blocks: Vec<MessageContentBlock>) {
        self.messages.push(Message::assistant_with_blocks(blocks));
        self.trim_if_needed();
    }

    pub fn add_tool_results(&mut self, results: Vec<MessageContentBlock>) {
        self.messages.push(Message::user_with_tool_results(results));
        self.trim_if_needed();
    }

    /// Append a plain-text block to the last user message when it already
    /// carries `Blocks` content (e.g. a user turn holding `ToolResult`
    /// blocks). Returns `true` if the append happened, `false` if there
    /// is no suitable user-role tail to extend — callers should fall
    /// back to [`Self::add_user_message`] in that case.
    ///
    /// Used by the post-tool interrupt path to avoid emitting two
    /// consecutive user messages (the tool-results turn plus an interrupt
    /// notice), which OpenAI's strict role-alternation validator rejects.
    pub fn append_text_to_last_user_blocks(&mut self, text: String) -> bool {
        if let Some(last) = self.messages.last_mut() {
            if last.role == "user" {
                if let crate::api::MessageContent::Blocks { content } = &mut last.content {
                    content.push(MessageContentBlock::Text {
                        text,
                        cache_control: None,
                    });
                    return true;
                }
            }
        }
        false
    }

    pub fn messages(&self) -> &[Message] {
        &self.messages
    }

    pub fn system_prompt(&self) -> &Vec<SystemPrompt> {
        &self.system_prompt
    }

    pub fn clear(&mut self) {
        self.messages.clear();
        self.invalidate_cache_anchor();
    }

    pub fn restore_messages(&mut self, messages: Vec<Message>) {
        // The new history has no relationship to the prior conversation;
        // any inherited anchor index is meaningless content-wise.
        self.invalidate_cache_anchor();
        self.messages = messages;
        self.trim_if_needed();
    }

    /// Remove the last message from the conversation (used for error recovery)
    pub fn remove_last_message(&mut self) {
        self.messages.pop();
        self.maintain_cache_anchor();
    }
}
