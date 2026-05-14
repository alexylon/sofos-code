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

    /// Remove the last message from the conversation (used for error recovery)
    pub fn remove_last_message(&mut self) {
        self.messages.pop();
        self.maintain_cache_anchor();
    }
}
