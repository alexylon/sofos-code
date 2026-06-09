//! Request transformation and per-model wire-format helpers for the
//! Anthropic Messages API. Strips OpenAI-only fields off the
//! [`CreateMessageRequest`] before serialisation, picks the right
//! `anthropic-beta` token for the target model, and exposes the
//! public reasoning-effort helpers that downstream code uses to
//! configure thinking budgets.

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
///
/// Only ships when the target model supports it — older models 400
/// on the unknown beta token.
/// Referenced by name only inside the cross-check test
/// `beta_with_compact_matches_components`; the production header is
/// served as the literal in `BETA_TOKEN_EFFICIENT_AND_COMPACT`.
#[allow(dead_code)]
pub(super) const BETA_COMPACT: &str = "compact-2026-01-12";

/// Compound beta token list used when the target model supports both
/// token-efficient tools and server-side compaction. Anthropic
/// accepts a comma-separated list in the single header.
pub(super) const BETA_TOKEN_EFFICIENT_AND_COMPACT: &str =
    "token-efficient-tools-2025-02-19,compact-2026-01-12";

/// Pick the `anthropic-beta` value for `model`. Compaction is gated
/// off the same `Model::supports_server_compaction` flag the
/// request builder uses to attach the `context_management` field, so
/// the beta header and the body field can never disagree about which
/// models speak server-side compaction.
pub(super) fn anthropic_beta_for(model: &str) -> &'static str {
    if crate::api::model_info::lookup(model).supports_server_compaction {
        BETA_TOKEN_EFFICIENT_AND_COMPACT
    } else {
        BETA_TOKEN_EFFICIENT
    }
}

/// Legacy `thinking.budget_tokens` values used by models that don't
/// accept the adaptive `output_config` reasoning request shape. The
/// four-tier mapping mirrors the reasoning-effort enum: `Off` → no
/// budget at all (no thinking block), `Low` / `Medium` / `High` →
/// these three constants.
pub const LEGACY_THINKING_BUDGET_LOW: u32 = 1024;
pub const LEGACY_THINKING_BUDGET_MEDIUM: u32 = 5120;
pub const LEGACY_THINKING_BUDGET_HIGH: u32 = 16384;

/// Default trigger floor for server-side compaction. Below this the
/// model probably hasn't earned compaction yet, and triggering early
/// would waste a compaction round-trip on a still-small history.
pub const COMPACTION_TRIGGER_FLOOR: u32 = 50_000;

/// Map a reasoning effort tier to its legacy `thinking.budget_tokens`
/// value. Returns 0 for `Off` so callers can branch on it to omit
/// the thinking block entirely. Used by request_builder when the
/// target model doesn't speak adaptive thinking.
pub fn legacy_thinking_budget(effort: ReasoningEffort) -> u32 {
    match effort {
        ReasoningEffort::Off | ReasoningEffort::Low => LEGACY_THINKING_BUDGET_LOW,
        ReasoningEffort::Medium => LEGACY_THINKING_BUDGET_MEDIUM,
        // Legacy-thinking models only expose budget tiers up to High.
        // `XHigh` and `Max` are adaptive-only rungs that upstream
        // validation refuses to pair with a legacy model, so this
        // branch is unreachable in practice; clamp defensively to the
        // highest legal budget.
        ReasoningEffort::High | ReasoningEffort::XHigh | ReasoningEffort::Max => {
            LEGACY_THINKING_BUDGET_HIGH
        }
    }
}

/// Maps a reasoning-effort level to the `output_config.effort` label
/// for adaptive-thinking requests. `Off` collapses to `"low"`: an
/// adaptive model rejects a request that carries signed thinking
/// blocks without an effort label, so there is no "no thinking" path
/// here. For no thinking at all, run a model whose
/// `requires_adaptive_thinking` is false with `--reasoning-effort off`.
pub fn effort_label(effort: ReasoningEffort) -> &'static str {
    match effort {
        ReasoningEffort::Off | ReasoningEffort::Low => "low",
        ReasoningEffort::Medium => "medium",
        ReasoningEffort::High => "high",
        ReasoningEffort::XHigh => "xhigh",
        ReasoningEffort::Max => "max",
    }
}

/// True for models that speak the adaptive `output_config.thinking`
/// reasoning request shape. Thin wrapper over the model-info table
/// so a new model's adaptive-thinking opt-in is one entry in
/// [`crate::api::model_info::lookup`] rather than a code change here.
pub fn requires_adaptive_thinking(model: &str) -> bool {
    crate::api::model_info::lookup(model).requires_adaptive_thinking
}

/// Strip OpenAI-only fields and tools off `request` before it goes
/// out on the wire to Anthropic, and run [`sanitize_messages_for_anthropic`]
/// over the message history so OpenAI Reasoning / Summary blocks
/// don't 400 the request. Used by both the streaming and
/// non-streaming call paths.
pub(super) fn prepare_request(mut request: CreateMessageRequest) -> CreateMessageRequest {
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
