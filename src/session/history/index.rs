//! `index.json` shape and the read-modify-write paths that maintain it.
//! The save-lock acquired by [`HistoryManager::save_session`] serialises
//! the read-modify-write against concurrent sofos processes in the same
//! workspace — see the comment on `SAVE_LOCK_FILE`.

use crate::error::Result;
use crate::session::history::HistoryManager;
use crate::session::history::atomic_write;
use crate::session::history::model::{Session, SessionMetadata};
use serde::{Deserialize, Serialize};
use std::fs;

pub(super) const INDEX_FILE: &str = "index.json";

#[derive(Debug, Serialize, Deserialize)]
pub(super) struct SessionIndex {
    pub(super) sessions: Vec<SessionMetadata>,
}

impl HistoryManager {
    pub(super) fn update_index(&self, session: &Session) -> Result<()> {
        let index_path = self.index_path();
        let mut index: SessionIndex = if index_path.exists() {
            // Treat a parse failure as a missing index — we're about
            // to rewrite the file anyway, so a single corrupt entry
            // shouldn't poison every later save. The session JSON
            // itself already landed on disk above; rebuilding the
            // index from one entry is correct, just less complete
            // until the next pass walks the directory.
            match fs::read_to_string(&index_path)
                .ok()
                .and_then(|s| serde_json::from_str::<SessionIndex>(&s).ok())
            {
                Some(parsed) => parsed,
                None => {
                    tracing::warn!(
                        path = %index_path.display(),
                        "session index unreadable or malformed; rebuilding from this save"
                    );
                    SessionIndex {
                        sessions: Vec::new(),
                    }
                }
            }
        } else {
            SessionIndex {
                sessions: Vec::new(),
            }
        };

        let preview = Self::extract_preview(&session.api_messages);
        let metadata = SessionMetadata {
            id: session.id.clone(),
            preview,
            created_at: session.created_at,
            updated_at: session.updated_at,
            message_count: session.api_messages.len(),
        };

        if let Some(pos) = index.sessions.iter().position(|s| s.id == session.id) {
            index.sessions[pos] = metadata;
        } else {
            index.sessions.push(metadata);
        }

        index
            .sessions
            .sort_by_key(|b| std::cmp::Reverse(b.updated_at));

        let content = serde_json::to_string_pretty(&index)?;
        atomic_write(&index_path, &content)?;

        Ok(())
    }

    pub fn list_sessions(&self) -> Result<Vec<SessionMetadata>> {
        let index_path = self.index_path();

        if !index_path.exists() {
            return Ok(Vec::new());
        }

        let content = fs::read_to_string(index_path)?;
        let index: SessionIndex = serde_json::from_str(&content)?;

        Ok(index.sessions)
    }
}
