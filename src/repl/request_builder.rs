use crate::api::LlmClient::{Anthropic, OpenAI};
use crate::api::{CreateMessageRequest, LlmClient, ReasoningEffort, Tool};
use crate::repl::conversation::ConversationHistory;

pub struct RequestBuilder<'a> {
    client: &'a LlmClient,
    model: &'a str,
    max_tokens: u32,
    conversation: &'a ConversationHistory,
    tools: Vec<Tool>,
    reasoning_effort: ReasoningEffort,
    /// Stable per-session identifier sent as `prompt_cache_key` on the
    /// OpenAI Responses path. Anthropic ignores it.
    session_id: &'a str,
}

impl<'a> RequestBuilder<'a> {
    pub fn new(
        client: &'a LlmClient,
        model: &'a str,
        max_tokens: u32,
        conversation: &'a ConversationHistory,
        tools: Vec<Tool>,
        reasoning_effort: ReasoningEffort,
        session_id: &'a str,
    ) -> Self {
        Self {
            client,
            model,
            max_tokens,
            conversation,
            tools,
            reasoning_effort,
            session_id,
        }
    }

    pub fn build(self) -> CreateMessageRequest {
        let is_anthropic = matches!(self.client, Anthropic(_));
        let adaptive =
            is_anthropic && crate::api::anthropic::requires_adaptive_thinking(self.model);

        // Adaptive-thinking models (Opus 4.7, Sonnet 4.6) take
        // `thinking: adaptive` + `output_config.effort`; Opus 4.7
        // outright rejects the legacy `enabled` shape, and Sonnet 4.6
        // accepts it but Anthropic recommends adaptive. Haiku 4.5
        // still takes the manual `budget_tokens` shape. On adaptive models we *always* send `thinking:
        // adaptive` even when the user picked `Off` — dropping it
        // would 400 the next turn if the conversation history already
        // contains echoed thinking blocks from an earlier on turn
        // (Anthropic rejects requests carrying thinking blocks
        // without a matching top-level thinking config). The level
        // is expressed through `output_config.effort` instead, which
        // collapses `Off` to `low` for the same reason.
        let (thinking_config, output_config) = if is_anthropic && adaptive {
            let effort = crate::api::anthropic::effort_label(self.reasoning_effort);
            (
                Some(crate::api::Thinking::adaptive()),
                Some(crate::api::OutputConfig::with_effort(effort)),
            )
        } else if is_anthropic && self.reasoning_effort.is_enabled() {
            // Non-adaptive Anthropic models (Haiku 4.5) take the legacy
            // `{type: "enabled", budget_tokens}` shape.
            // The per-tier mapping lives in `crate::api::anthropic` so
            // the startup validation in `repl/mod.rs` can reference the
            // same `LEGACY_THINKING_BUDGET_HIGH` ceiling without
            // duplicating the values.
            let budget = crate::api::anthropic::legacy_thinking_budget(self.reasoning_effort);
            (Some(crate::api::Thinking::enabled(budget)), None)
        } else {
            (None, None)
        };

        let reasoning_config = if matches!(self.client, OpenAI(_)) {
            // `Max` is rejected upstream (startup validation + `/think`
            // gate) because OpenAI's wire schema doesn't accept it.
            // Clamping `Max` defensively to the highest accepted level
            // here keeps the request well-formed if validation is ever
            // bypassed; the user would see the request go through with
            // `xhigh` rather than a 400.
            Some(match self.reasoning_effort {
                ReasoningEffort::Off => crate::api::Reasoning::minimal(),
                ReasoningEffort::Low => crate::api::Reasoning::with_effort("low"),
                ReasoningEffort::Medium => crate::api::Reasoning::with_effort("medium"),
                ReasoningEffort::High => crate::api::Reasoning::with_effort("high"),
                ReasoningEffort::XHigh | ReasoningEffort::Max => {
                    crate::api::Reasoning::with_effort("xhigh")
                }
            })
        } else {
            None
        };

        // Send system prompt to both Anthropic and OpenAI; cache hints are handled per API
        let system_prompt = Some(self.conversation.system_prompt().clone());

        // Anthropic server-side compaction. Enabled only on models
        // that advertise it via `Model::supports_server_compaction`
        // (currently Opus 4.7 and Sonnet 4.6). The trigger value is
        // the same `auto_compact_at` number the rest of the crate
        // reads, so the per-model cost cap stays the single source
        // of truth. OpenAI has no server-side equivalent.
        let context_management = if matches!(self.client, Anthropic(_)) {
            let info = crate::api::model_info::lookup(self.model);
            if info.supports_server_compaction {
                Some(crate::api::ContextManagement {
                    edits: vec![crate::api::ContextEdit::Compact20260112 {
                        // Clamp to Anthropic's documented floor. No
                        // model in the registry today drops below it
                        // (Haiku 4.5 sits at 170K and doesn't carry
                        // `supports_server_compaction` anyway), but a
                        // future small-window addition would otherwise
                        // 400 the request.
                        trigger: Some(crate::api::CompactionTrigger::InputTokens {
                            value: info
                                .auto_compact_at()
                                .max(crate::api::anthropic::COMPACTION_TRIGGER_FLOOR),
                        }),
                        instructions: None,
                    }],
                })
            } else {
                None
            }
        } else {
            None
        };

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
            context_management,
        };

        // Anthropic prompt caching is opt-in per content block. We mark
        // two breakpoints in addition to the system prompt (already
        // cached at construction): the last tool definition, and the
        // last block of the last message. The rolling breakpoint moves
        // forward each turn — paying the 2x write premium for a 1-hour
        // TTL on a position that gets superseded next turn would burn
        // cache writes for nothing, so the rolling stays at the 5m
        // default. The anchor and the last tool definition do NOT move
        // turn-to-turn (the anchor sticks until cumulative intermediate
        // distance exceeds ~18 blocks; tool definitions are static), so
        // they get the 1-hour TTL — paid once, survives any pause
        // shorter than an hour.
        if matches!(self.client, Anthropic(_)) {
            if let Some(tools) = request.tools.as_mut() {
                // `prepare_request` strips `OpenAIWebSearch` before
                // sending; stamp on the last *non-OpenAI* tool so the
                // breakpoint survives that filter. Without this skip,
                // when `OpenAIWebSearch` is the last entry in the
                // registered tool list, no Anthropic tool gets the
                // cache-control marker and the entire tools block
                // re-bills at full input rate every turn.
                let last_anthropic_tool = tools
                    .iter_mut()
                    .rev()
                    .find(|t| matches!(t, Tool::Regular { .. } | Tool::AnthropicWebSearch { .. }));
                if let Some(tool) = last_anthropic_tool {
                    match tool {
                        Tool::Regular { cache_control, .. }
                        | Tool::AnthropicWebSearch { cache_control, .. } => {
                            *cache_control = Some(crate::api::CacheControl::ephemeral_one_hour());
                        }
                        Tool::OpenAIWebSearch { .. } => unreachable!(),
                    }
                }
            }
            mark_rolling_cache_breakpoint(
                &mut request.messages,
                self.conversation.cache_anchor_message_idx(),
            );
        }

        request
    }
}

/// Stamp `cache_control: ephemeral` on the rolling breakpoint (last
/// block of the last message) and, if `anchor_idx` is provided, on the
/// last block of `messages[anchor_idx]`. The anchor is the secondary
/// breakpoint that protects against the 20-block lookback window in
/// wide multi-tool iterations — the rolling alone cold-misses when a
/// single turn jumps past 20 blocks. Rolling uses the 5m TTL because
/// it moves every turn; anchor uses 1h because it only advances when
/// cumulative intermediate distance exceeds ~18 blocks (i.e. once
/// every ~10 turns), so the 2x write premium is amortised.
fn mark_rolling_cache_breakpoint(messages: &mut [crate::api::Message], anchor_idx: Option<usize>) {
    if let Some(last_msg) = messages.last_mut() {
        stamp_last_block(last_msg, crate::api::CacheControl::ephemeral(None));
    }
    if let Some(idx) = anchor_idx {
        // The anchor must be a different message than the rolling, and
        // within bounds. `maintain_cache_anchor` already guarantees
        // both, but recheck so an out-of-sync caller can't panic the
        // request build.
        if idx + 1 < messages.len() {
            stamp_last_block(
                &mut messages[idx],
                crate::api::CacheControl::ephemeral_one_hour(),
            );
        }
    }
}

/// No-op when the message has plain `Text` content rather than
/// per-block `Blocks` — those messages have no `cache_control` field
/// to stamp, so the breakpoint lands on the next turn that uses
/// blocks.
fn stamp_last_block(msg: &mut crate::api::Message, control: crate::api::CacheControl) {
    use crate::api::{MessageContent, MessageContentBlock};

    let MessageContent::Blocks { content } = &mut msg.content else {
        return;
    };
    let Some(last_block) = content.last_mut() else {
        return;
    };
    let cc = match last_block {
        MessageContentBlock::Text { cache_control, .. }
        | MessageContentBlock::Thinking { cache_control, .. }
        | MessageContentBlock::Summary { cache_control, .. }
        | MessageContentBlock::Compaction { cache_control, .. }
        | MessageContentBlock::Reasoning { cache_control, .. }
        | MessageContentBlock::ToolUse { cache_control, .. }
        | MessageContentBlock::ToolResult { cache_control, .. }
        | MessageContentBlock::ServerToolUse { cache_control, .. }
        | MessageContentBlock::WebSearchToolResult { cache_control, .. }
        | MessageContentBlock::Image { cache_control, .. } => cache_control,
    };
    *cc = Some(control);
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
            "gpt-5.5",
            8192,
            &conv,
            one_regular_tool(),
            ReasoningEffort::Off,
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
            ReasoningEffort::Off,
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
            "gpt-5.5",
            8192,
            &conv,
            one_regular_tool(),
            ReasoningEffort::Off,
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
        mark_rolling_cache_breakpoint(&mut messages, None);
        // No panic, and no Blocks content was synthesized.
        assert!(matches!(
            &messages[0].content,
            crate::api::MessageContent::Text { .. }
        ));
    }

    #[test]
    fn rolling_breakpoint_is_noop_on_empty_messages() {
        let mut messages: Vec<Message> = Vec::new();
        mark_rolling_cache_breakpoint(&mut messages, None);
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
        mark_rolling_cache_breakpoint(&mut messages, None);

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

    fn one_text_block_user(text: &str) -> Message {
        Message::user_with_blocks(vec![MessageContentBlock::Text {
            text: text.to_string(),
            cache_control: None,
        }])
    }

    fn block_cache_control(msg: &Message) -> Option<&crate::api::CacheControl> {
        match &msg.content {
            crate::api::MessageContent::Blocks { content } => match content.last() {
                Some(MessageContentBlock::Text { cache_control, .. }) => cache_control.as_ref(),
                _ => None,
            },
            _ => None,
        }
    }

    #[test]
    fn anchor_breakpoint_stamps_when_index_provided() {
        let mut messages = vec![
            one_text_block_user("a"),
            one_text_block_user("b"),
            one_text_block_user("c"),
        ];
        mark_rolling_cache_breakpoint(&mut messages, Some(0));

        assert!(
            block_cache_control(&messages[0]).is_some(),
            "anchor stamped"
        );
        assert!(
            block_cache_control(&messages[1]).is_none(),
            "middle untouched"
        );
        assert!(
            block_cache_control(&messages[2]).is_some(),
            "rolling stamped"
        );
    }

    #[test]
    fn anchor_breakpoint_skipped_when_index_is_rolling() {
        let mut messages = vec![one_text_block_user("a"), one_text_block_user("b")];
        // Anchor index 1 == rolling index → defensively skip rather than double-stamp.
        mark_rolling_cache_breakpoint(&mut messages, Some(1));

        assert!(block_cache_control(&messages[0]).is_none());
        assert!(
            block_cache_control(&messages[1]).is_some(),
            "rolling still stamped"
        );
    }

    #[test]
    fn anchor_breakpoint_skipped_when_index_out_of_bounds() {
        let mut messages = vec![one_text_block_user("a"), one_text_block_user("b")];
        mark_rolling_cache_breakpoint(&mut messages, Some(99));
        // Out-of-bounds anchor must not panic; rolling still stamped.
        assert!(block_cache_control(&messages[1]).is_some());
    }

    #[test]
    fn anthropic_build_stamps_both_anchor_and_rolling_when_conversation_has_anchor() {
        // End-to-end: build a long-enough conversation that the
        // ConversationHistory's `cache_anchor_message_idx` is set, then
        // verify the Anthropic build path stamps cache_control on both
        // the rolling (last message) AND the anchor (earlier message).
        let mut conv = ConversationHistory::new();
        for i in 0..16 {
            conv.add_user_with_blocks(vec![MessageContentBlock::Text {
                text: format!("msg-{}", i),
                cache_control: None,
            }]);
        }
        let anchor_idx = conv
            .cache_anchor_message_idx()
            .expect("anchor must be set with 16 blocks of history");

        let req = RequestBuilder::new(
            &anthropic_client(),
            "claude-sonnet-4-6",
            8192,
            &conv,
            one_regular_tool(),
            ReasoningEffort::Off,
            "s1",
        )
        .build();

        // Rolling stamp on the last message.
        let last_cc = block_cache_control(req.messages.last().unwrap());
        assert!(last_cc.is_some(), "rolling breakpoint missing");

        // Anchor stamp on the recorded anchor message.
        let anchor_cc = block_cache_control(&req.messages[anchor_idx]);
        assert!(
            anchor_cc.is_some(),
            "anchor breakpoint missing at idx {}",
            anchor_idx
        );

        // No spurious stamps in between.
        for (i, msg) in req.messages.iter().enumerate() {
            if i == anchor_idx || i == req.messages.len() - 1 {
                continue;
            }
            assert!(
                block_cache_control(msg).is_none(),
                "unexpected cache_control at idx {} (only rolling + anchor should be stamped)",
                i
            );
        }
    }

    #[test]
    fn anthropic_1m_models_get_server_compaction_config() {
        let conv = ConversationHistory::new();
        let req = RequestBuilder::new(
            &anthropic_client(),
            "claude-opus-4-7",
            8192,
            &conv,
            one_regular_tool(),
            ReasoningEffort::Off,
            "s1",
        )
        .build();
        let cm = req
            .context_management
            .expect("Opus 4.7 should carry context_management");
        let json = serde_json::to_value(&cm).unwrap();
        // Wire shape matches Anthropic's docs: edits[].type ==
        // "compact_20260112" with an input_tokens trigger.
        assert_eq!(json["edits"][0]["type"], "compact_20260112");
        assert_eq!(json["edits"][0]["trigger"]["type"], "input_tokens");
        let trigger_value = json["edits"][0]["trigger"]["value"].as_u64().unwrap();
        assert!(
            trigger_value >= 50_000,
            "Anthropic rejects triggers below 50K"
        );
    }

    #[test]
    fn legacy_anthropic_thinking_budget_scales_with_effort() {
        // On non-adaptive Anthropic models (Haiku 4.5),
        // `/think low|medium|high` used to all collapse to the same
        // `thinking_budget`. Verify each tier now produces a strictly
        // larger budget so the slider has a visible effect.
        let conv = ConversationHistory::new();
        let budget_for = |effort| {
            let req = RequestBuilder::new(
                &anthropic_client(),
                "claude-haiku-4-5",
                65_536,
                &conv,
                one_regular_tool(),
                effort,
                "s1",
            )
            .build();
            let thinking = req.thinking.expect("legacy Anthropic enables thinking");
            thinking
                .budget_tokens
                .expect("legacy thinking carries budget_tokens")
        };

        let low = budget_for(ReasoningEffort::Low);
        let medium = budget_for(ReasoningEffort::Medium);
        let high = budget_for(ReasoningEffort::High);

        assert!(low < medium, "Medium ({medium}) must exceed Low ({low})");
        assert!(medium < high, "High ({high}) must exceed Medium ({medium})");
    }

    #[test]
    fn unsupported_models_skip_server_compaction() {
        let conv = ConversationHistory::new();
        let haiku_req = RequestBuilder::new(
            &anthropic_client(),
            "claude-haiku-4-5",
            8192,
            &conv,
            one_regular_tool(),
            ReasoningEffort::Off,
            "s1",
        )
        .build();
        assert!(
            haiku_req.context_management.is_none(),
            "Haiku 4.5 isn't on Anthropic's compaction-supported list"
        );

        let openai_req = RequestBuilder::new(
            &openai_client(),
            "gpt-5.5",
            8192,
            &conv,
            one_regular_tool(),
            ReasoningEffort::Off,
            "s1",
        )
        .build();
        assert!(
            openai_req.context_management.is_none(),
            "OpenAI never receives Anthropic's context_management"
        );
    }
}
