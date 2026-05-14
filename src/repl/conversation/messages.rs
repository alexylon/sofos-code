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

    /// Append `text` to the last message if it's a user turn. Returns
    /// `true` on success; `false` when there is no user-role tail to
    /// extend (caller should fall back to `add_user_message`).
    ///
    /// Used by the API-error and image-retry paths in `turn.rs` to
    /// attach a `[SYSTEM ERROR: ...]` note to the user turn that
    /// triggered the failure, instead of fabricating an assistant
    /// message (which would make the model think it wrote the note)
    /// or appending a second user message (which OpenAI's strict
    /// role-alternation validator rejects).
    pub fn append_text_to_last_user_blocks(&mut self, text: String) -> bool {
        let Some(last) = self.messages.last_mut() else {
            return false;
        };
        if last.role != "user" {
            return false;
        }
        match &mut last.content {
            crate::api::MessageContent::Blocks { content } => {
                content.push(MessageContentBlock::Text {
                    text,
                    cache_control: None,
                });
                true
            }
            crate::api::MessageContent::Text { content } => {
                if !content.is_empty() {
                    content.push_str("\n\n");
                }
                content.push_str(&text);
                true
            }
        }
    }

    pub fn messages(&self) -> &[Message] {
        &self.messages
    }

    pub fn system_prompt(&self) -> &Vec<SystemPrompt> {
        &self.system_prompt
    }

    /// Replace the system prompt wholesale (used when restoring a
    /// saved session so the resumed conversation keeps the same
    /// system context the assistant was answering against). The
    /// cache anchor is invalidated because the cached prefix bytes
    /// change with the system prompt.
    pub fn set_system_prompt(&mut self, system_prompt: Vec<SystemPrompt>) {
        self.system_prompt = system_prompt;
        self.invalidate_cache_anchor();
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
}
