use crate::api::LlmClient::{Anthropic, OpenAI};
use crate::api::{CreateMessageRequest, LlmClient, Tool};
use crate::conversation::ConversationHistory;

pub struct RequestBuilder<'a> {
    client: &'a LlmClient,
    model: &'a str,
    max_tokens: u32,
    conversation: &'a ConversationHistory,
    tools: Vec<Tool>,
    enable_thinking: bool,
    thinking_budget: u32,
}

impl<'a> RequestBuilder<'a> {
    pub fn new(
        client: &'a LlmClient,
        model: &'a str,
        max_tokens: u32,
        conversation: &'a ConversationHistory,
        tools: Vec<Tool>,
        enable_thinking: bool,
        thinking_budget: u32,
    ) -> Self {
        Self {
            client,
            model,
            max_tokens,
            conversation,
            tools,
            enable_thinking,
            thinking_budget,
        }
    }

    pub fn build(self) -> CreateMessageRequest {
        let thinking_config = if self.enable_thinking && matches!(self.client, Anthropic(_)) {
            Some(crate::api::Thinking::enabled(self.thinking_budget))
        } else {
            None
        };

        let reasoning_config = if self.enable_thinking && matches!(self.client, OpenAI(_)) {
            Some(crate::api::Reasoning::enabled())
        } else if matches!(self.client, OpenAI(_)) {
            Some(crate::api::Reasoning::disabled())
        } else {
            None
        };

        let system_prompt = if matches!(self.client, Anthropic(_)) {
            Some(self.conversation.system_prompt().clone())
        } else {
            None
        };

        CreateMessageRequest {
            model: self.model.to_string(),
            max_tokens: self.max_tokens,
            messages: self.conversation.messages().to_vec(),
            system: system_prompt,
            tools: Some(self.tools),
            stream: None,
            thinking: thinking_config,
            reasoning: reasoning_config,
        }
    }
}
