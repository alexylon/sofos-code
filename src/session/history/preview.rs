//! Short preview string for the session index. Picks the first
//! non-empty user message and trims it to the per-entry display
//! budget — the index UI uses this as the row label when listing
//! saved sessions.

use crate::api::Message;
use crate::session::history::HistoryManager;

pub(super) const MAX_PREVIEW_LENGTH: usize = 120;

impl HistoryManager {
    pub(super) fn extract_preview(messages: &[Message]) -> String {
        for message in messages {
            if message.role == "user" {
                let text = match &message.content {
                    crate::api::MessageContent::Text { content } => content,
                    crate::api::MessageContent::Blocks { content } => content
                        .iter()
                        .find_map(|block| {
                            if let crate::api::MessageContentBlock::Text { text, .. } = block {
                                Some(text.as_str())
                            } else {
                                None
                            }
                        })
                        .unwrap_or(""),
                };

                let preview = text.trim();
                if preview.is_empty() {
                    continue;
                }

                return if preview.chars().count() > MAX_PREVIEW_LENGTH {
                    let truncate_at = preview
                        .char_indices()
                        .nth(MAX_PREVIEW_LENGTH)
                        .map(|(idx, _)| idx)
                        .unwrap_or(preview.len());
                    format!("{}...", &preview[..truncate_at])
                } else {
                    preview.to_string()
                };
            }
        }
        "Empty session".to_string()
    }
}
