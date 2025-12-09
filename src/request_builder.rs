use crate::api::{CreateMessageRequest, Tool};
use crate::conversation::ConversationHistory;

pub struct RequestBuilder<'a> {
    model: &'a str,
    max_tokens: u32,
    conversation: &'a ConversationHistory,
    tools: Vec<Tool>,
    enable_thinking: bool,
    thinking_budget: u32,
}

impl<'a> RequestBuilder<'a> {
    pub fn new(
        model: &'a str,
        max_tokens: u32,
        conversation: &'a ConversationHistory,
        tools: Vec<Tool>,
        enable_thinking: bool,
        thinking_budget: u32,
    ) -> Self {
        Self {
            model,
            max_tokens,
            conversation,
            tools,
            enable_thinking,
            thinking_budget,
        }
    }

    pub fn build(self) -> CreateMessageRequest {
        let thinking_config = if self.enable_thinking {
            Some(crate::api::Thinking::enabled(self.thinking_budget))
        } else {
            None
        };

        CreateMessageRequest {
            model: self.model.to_string(),
            max_tokens: self.max_tokens,
            messages: self.conversation.messages().to_vec(),
            system: Some(self.conversation.system_prompt().to_string()),
            tools: Some(self.tools),
            stream: None,
            thinking: thinking_config,
        }
    }
}
