use crate::api::LlmClient::{Anthropic, OpenAI};
use crate::api::{CreateMessageRequest, LlmClient, Tool};
use crate::repl::conversation::ConversationHistory;

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
        let is_anthropic = matches!(self.client, Anthropic(_));
        let adaptive =
            is_anthropic && crate::api::anthropic::requires_adaptive_thinking(self.model);

        // Opus 4.7 rejects `thinking.type = "enabled"` and requires
        // `adaptive` + `output_config.effort`. Older Anthropic models
        // still take the manual `budget_tokens` shape. On adaptive
        // models we *always* send `thinking: adaptive` even when the
        // user toggled `/think off` — dropping it would 400 the next
        // turn if the conversation history already contains echoed
        // thinking blocks from an earlier adaptive-on turn (Anthropic
        // rejects requests carrying thinking blocks without a matching
        // top-level thinking config). The on/off knob is expressed
        // through `output_config.effort` instead.
        let (thinking_config, output_config) = if is_anthropic && adaptive {
            let effort = crate::api::anthropic::effort_label(self.enable_thinking);
            (
                Some(crate::api::Thinking::adaptive()),
                Some(crate::api::OutputConfig::with_effort(effort)),
            )
        } else if is_anthropic && self.enable_thinking {
            (
                Some(crate::api::Thinking::enabled(self.thinking_budget)),
                None,
            )
        } else {
            (None, None)
        };

        let reasoning_config = if self.enable_thinking && matches!(self.client, OpenAI(_)) {
            Some(crate::api::Reasoning::enabled())
        } else if matches!(self.client, OpenAI(_)) {
            Some(crate::api::Reasoning::disabled())
        } else {
            None
        };

        // Send system prompt to both Anthropic and OpenAI; cache hints are handled per API
        let system_prompt = Some(self.conversation.system_prompt().clone());

        let mut request = CreateMessageRequest {
            model: self.model.to_string(),
            max_tokens: self.max_tokens,
            messages: self.conversation.messages().to_vec(),
            system: system_prompt,
            tools: Some(self.tools),
            stream: None,
            thinking: thinking_config,
            output_config,
            reasoning: reasoning_config,
        };

        // For Anthropic, enable cache on last tool to mark cache breakpoint
        if matches!(self.client, Anthropic(_)) {
            if let Some(tools) = request.tools.as_mut() {
                if let Some(last_tool) = tools.last_mut() {
                    match last_tool {
                        Tool::Regular { cache_control, .. }
                        | Tool::AnthropicWebSearch { cache_control, .. } => {
                            *cache_control = Some(crate::api::CacheControl::ephemeral(None));
                        }
                        Tool::OpenAIWebSearch { .. } => {}
                    }
                }
            }
        }

        request
    }
}
