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
    /// Stable per-session identifier sent as `prompt_cache_key` on the
    /// OpenAI Responses path. Anthropic ignores it.
    session_id: &'a str,
}

impl<'a> RequestBuilder<'a> {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        client: &'a LlmClient,
        model: &'a str,
        max_tokens: u32,
        conversation: &'a ConversationHistory,
        tools: Vec<Tool>,
        enable_thinking: bool,
        thinking_budget: u32,
        session_id: &'a str,
    ) -> Self {
        Self {
            client,
            model,
            max_tokens,
            conversation,
            tools,
            enable_thinking,
            thinking_budget,
            session_id,
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
            prompt_cache_key: Some(self.session_id.to_string()),
        };

        // Anthropic prompt caching is opt-in per content block. We mark
        // two breakpoints in addition to the system prompt (already
        // cached at construction): the last tool definition, and the
        // last block of the last message. The latter rolls forward as
        // the conversation grows so each turn extends the cached prefix
        // instead of restarting.
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
            mark_rolling_cache_breakpoint(&mut request.messages);
        }

        request
    }
}

/// Stamp `cache_control: ephemeral` on the final content block of the
/// last message so the cached Anthropic prefix grows with the
/// conversation instead of restarting on each turn.
fn mark_rolling_cache_breakpoint(messages: &mut [crate::api::Message]) {
    use crate::api::{CacheControl, MessageContent, MessageContentBlock};

    let Some(last_msg) = messages.last_mut() else {
        return;
    };
    // Plain-text user messages don't carry per-block cache_control;
    // the breakpoint lands on the next turn that uses blocks.
    let MessageContent::Blocks { content } = &mut last_msg.content else {
        return;
    };
    let Some(last_block) = content.last_mut() else {
        return;
    };
    let cc = match last_block {
        MessageContentBlock::Text { cache_control, .. }
        | MessageContentBlock::Thinking { cache_control, .. }
        | MessageContentBlock::Summary { cache_control, .. }
        | MessageContentBlock::ToolUse { cache_control, .. }
        | MessageContentBlock::ToolResult { cache_control, .. }
        | MessageContentBlock::ServerToolUse { cache_control, .. }
        | MessageContentBlock::WebSearchToolResult { cache_control, .. }
        | MessageContentBlock::Image { cache_control, .. } => cache_control,
    };
    *cc = Some(CacheControl::ephemeral(None));
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::{AnthropicClient, Message, MessageContentBlock, OpenAIClient, Tool};

    fn anthropic_client() -> LlmClient {
        LlmClient::Anthropic(AnthropicClient::new("test-key".to_string()).unwrap())
    }

    fn openai_client() -> LlmClient {
        LlmClient::OpenAI(OpenAIClient::new("test-key".to_string()).unwrap())
    }

    fn one_regular_tool() -> Vec<Tool> {
        vec![Tool::Regular {
            name: "read_file".to_string(),
            description: "read a file".to_string(),
            input_schema: serde_json::json!({"type": "object"}),
            cache_control: None,
        }]
    }

    #[test]
    fn build_sets_prompt_cache_key_to_session_id() {
        let conv = ConversationHistory::new();
        let request = RequestBuilder::new(
            &openai_client(),
            "gpt-5.3",
            8192,
            &conv,
            one_regular_tool(),
            false,
            0,
            "session-abc",
        )
        .build();

        assert_eq!(request.prompt_cache_key.as_deref(), Some("session-abc"));
    }

    #[test]
    fn build_marks_rolling_cache_breakpoint_on_anthropic_only() {
        let mut conv = ConversationHistory::new();
        conv.add_user_with_blocks(vec![MessageContentBlock::Text {
            text: "hello".to_string(),
            cache_control: None,
        }]);

        let anth = RequestBuilder::new(
            &anthropic_client(),
            "claude-sonnet-4-6",
            8192,
            &conv,
            one_regular_tool(),
            false,
            0,
            "s1",
        )
        .build();

        let last_block = match &anth.messages.last().unwrap().content {
            crate::api::MessageContent::Blocks { content } => content.last().unwrap(),
            _ => panic!("expected Blocks content"),
        };
        let cc = match last_block {
            MessageContentBlock::Text { cache_control, .. } => cache_control.as_ref(),
            _ => panic!("expected Text block"),
        };
        assert!(cc.is_some(), "Anthropic should stamp rolling breakpoint");

        let oai = RequestBuilder::new(
            &openai_client(),
            "gpt-5.3",
            8192,
            &conv,
            one_regular_tool(),
            false,
            0,
            "s1",
        )
        .build();

        let last_block = match &oai.messages.last().unwrap().content {
            crate::api::MessageContent::Blocks { content } => content.last().unwrap(),
            _ => panic!("expected Blocks content"),
        };
        let cc = match last_block {
            MessageContentBlock::Text { cache_control, .. } => cache_control.as_ref(),
            _ => panic!("expected Text block"),
        };
        assert!(cc.is_none(), "OpenAI must not stamp Anthropic markers");
    }

    #[test]
    fn rolling_breakpoint_is_noop_on_plain_text_user_message() {
        let mut messages = vec![Message::user("just text")];
        mark_rolling_cache_breakpoint(&mut messages);
        // No panic, and no Blocks content was synthesized.
        assert!(matches!(
            &messages[0].content,
            crate::api::MessageContent::Text { .. }
        ));
    }

    #[test]
    fn rolling_breakpoint_is_noop_on_empty_messages() {
        let mut messages: Vec<Message> = Vec::new();
        mark_rolling_cache_breakpoint(&mut messages);
        assert!(messages.is_empty());
    }

    #[test]
    fn rolling_breakpoint_targets_only_the_final_block() {
        let mut messages = vec![Message::user_with_blocks(vec![
            MessageContentBlock::Text {
                text: "first".to_string(),
                cache_control: None,
            },
            MessageContentBlock::Text {
                text: "second".to_string(),
                cache_control: None,
            },
        ])];
        mark_rolling_cache_breakpoint(&mut messages);

        let blocks = match &messages[0].content {
            crate::api::MessageContent::Blocks { content } => content,
            _ => panic!("expected Blocks"),
        };
        assert!(matches!(
            &blocks[0],
            MessageContentBlock::Text {
                cache_control: None,
                ..
            }
        ));
        assert!(matches!(
            &blocks[1],
            MessageContentBlock::Text {
                cache_control: Some(_),
                ..
            }
        ));
    }
}
