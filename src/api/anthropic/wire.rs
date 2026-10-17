//! Request transformation and per-model wire-format helpers for the
//! Anthropic Messages API. Strips OpenAI-only fields off the
//! [`CreateMessageRequest`] before serialisation, picks the right
//! `anthropic-beta` token for the target model.

use crate::api::types::*;

/// Compact `anthropic-beta` set sent on every Anthropic request.
/// `prompt-caching-2024-07-31` is implicit for current Claude models,
/// so we only have to ship token-efficient tools and the compaction
/// beta where it applies.
pub(super) const BETA_HEADER_NAME: &str = "anthropic-beta";

/// Token-efficient tools beta — opt-in to the smaller tool-result
/// schema. Cuts ~30% off tool-result tokens on the models that
/// support it by streaming results in the compact wire shape. Models
/// that don't recognise the beta just ignore the header.
pub(super) const BETA_TOKEN_EFFICIENT: &str = "token-efficient-tools-2025-02-19";

/// Server-side compaction beta — Anthropic prunes earlier turns when
/// the request input grows past the model's per-request threshold,
/// returning a `compaction` content block in the next assistant
/// response that we round-trip on subsequent calls.
pub(super) const BETA_COMPACT: &str = "compact-2026-01-12";

/// Server-side fallback beta — lets Anthropic answer a request its
/// safety classifiers decline on a model of its choosing instead of
/// returning the refusal. Pairs with the `fallbacks` request field;
/// this header is the one that accepts the `"default"` form.
pub(super) const BETA_SERVER_SIDE_FALLBACK: &str = "server-side-fallback-2026-07-01";

/// Build the `anthropic-beta` value for `model`: token-efficient tools
/// on every request, plus one token per capability the model
/// advertises. Each capability reads the same `Model` flag the request
/// builder uses to populate its body field, so the header and the body
/// can never disagree about which features are in play. Models that
/// support nothing extra get the single universal token rather than
/// relying on Anthropic ignoring unknown ones.
pub(super) fn anthropic_beta_for(model: &str) -> String {
    beta_header_for(crate::api::model_info::lookup(model))
}

pub(super) fn beta_header_for(info: &crate::api::model_info::Model) -> String {
    let mut tokens = vec![BETA_TOKEN_EFFICIENT];
    if info.supports_server_compaction {
        tokens.push(BETA_COMPACT);
    }
    if info.supports_refusal_fallback {
        tokens.push(BETA_SERVER_SIDE_FALLBACK);
    }
    tokens.join(",")
}

/// Default trigger floor for server-side compaction. Below this the
/// model probably hasn't earned compaction yet, and triggering early
/// would waste a compaction round-trip on a still-small history.
pub const COMPACTION_TRIGGER_FLOOR: u32 = 50_000;

/// Strip OpenAI-only fields and tools off `request` before it goes
/// out on the wire to Anthropic, and run [`sanitize_messages_for_anthropic`]
/// over the message history so OpenAI Reasoning / Summary blocks
/// don't 400 the request. Used by both the streaming and
/// non-streaming call paths.
pub(super) fn prepare_request(mut request: CreateMessageRequest) -> CreateMessageRequest {
    request.model = crate::api::model_info::api_id_for(&request.model).to_string();
    request.messages = sanitize_messages_for_anthropic(request.messages);

    // OpenAI-only; drop before serializing for Anthropic.
    request.prompt_cache_key = None;
    // `reasoning` is the OpenAI Responses-style sibling of Anthropic's
    // `thinking` field. The request builder never sets it on the
    // Anthropic path today, but clear it here defensively so a future
    // caller that constructs a request directly doesn't accidentally
    // send it and trigger a 400.
    request.reasoning = None;

    if let Some(tools) = request.tools.take() {
        let filtered: Vec<Tool> = tools
            .into_iter()
            .filter(|t| !matches!(t, Tool::OpenAIWebSearch { tool_type: _ }))
            .collect();

        if !filtered.is_empty() {
            request.tools = Some(filtered);
        }
    }

    request
}

/// Drop OpenAI-only content blocks (`Summary`, `Reasoning`) from
/// every message before sending to Anthropic. A session that
/// switched providers mid-stream still carries the OpenAI blocks in
/// memory; without this strip, the next Anthropic call 400s on the
/// unknown content-block types.
pub(crate) fn sanitize_messages_for_anthropic(messages: Vec<Message>) -> Vec<Message> {
    messages
        .into_iter()
        .map(|mut msg| {
            if let MessageContent::Blocks { content } = msg.content {
                let filtered_content = content
                    .into_iter()
                    .filter_map(|block| match block {
                        // OpenAI reasoning summary block — not part of
                        // Anthropic's content-block schema; the server
                        // would reject the unknown type.
                        MessageContentBlock::Summary { .. } => None,
                        // OpenAI Responses API reasoning item, packed
                        // with `id` + `encrypted_content`. Carries no
                        // meaning to Anthropic and uses a `type`
                        // string the server doesn't recognise. Drop
                        // before sending so a session that switched
                        // providers doesn't 400 on the next turn.
                        MessageContentBlock::Reasoning { .. } => None,
                        other => Some(other),
                    })
                    .collect();

                msg.content = MessageContent::Blocks {
                    content: filtered_content,
                };
            }
            msg
        })
        .collect()
}
