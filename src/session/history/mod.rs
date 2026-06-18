//! Session persistence: round-trip the in-memory conversation through
//! `.sofos/sessions/<id>.json` and maintain the `index.json` summary.
//! Submodules split the concerns —
//!
//! - [`manager`] — [`HistoryManager`] facade, disk-layout paths, and
//!   the save-lock that serialises concurrent writers.
//! - [`model`] — the persisted shapes ([`Session`], [`SessionMetadata`],
//!   [`SessionTokenCounters`], [`DisplayMessage`]).
//! - [`index`] — `index.json` load / save / update.
//! - [`preview`] — short user-facing preview string for the index UI.
//! - [`instructions`] — `AGENTS.md` + `.sofos/instructions.md` discovery.

pub mod index;
pub mod instructions;
pub mod manager;
pub mod model;
pub mod preview;

pub use manager::HistoryManager;
pub use model::{DisplayMessage, Session, SessionMetadata, SessionTokenCounters};

use crate::error::Result;
use std::fs;
use std::path::PathBuf;

/// Write content to a file atomically by writing to a temp file first, then renaming.
/// This prevents corruption if the process crashes mid-write.
pub(super) fn atomic_write(path: &PathBuf, content: &str) -> Result<()> {
    let tmp_path = path.with_extension("json.tmp");
    fs::write(&tmp_path, content)?;
    fs::rename(&tmp_path, path)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::{Message, SystemPrompt};
    use crate::session::history::manager::{SESSIONS_DIR, SOFOS_DIR};
    use crate::session::history::preview::MAX_PREVIEW_LENGTH;
    use tempfile::TempDir;

    #[test]
    fn test_history_manager_creation() {
        let temp_dir = TempDir::new().unwrap();
        let manager = HistoryManager::new(temp_dir.path().to_path_buf());
        assert!(manager.is_ok());

        let sofos_dir = temp_dir.path().join(SOFOS_DIR).join(SESSIONS_DIR);
        assert!(sofos_dir.exists());
    }

    #[test]
    fn generated_session_ids_are_unique_under_rapid_calls() {
        let mut seen = std::collections::HashSet::new();
        for _ in 0..256 {
            let id = HistoryManager::generate_session_id();
            assert!(id.starts_with("session_"));
            assert!(seen.insert(id), "duplicate session id within rapid burst");
        }
    }

    #[test]
    fn unique_session_id_avoids_existing_file() {
        let temp_dir = TempDir::new().unwrap();
        let manager = HistoryManager::new(temp_dir.path().to_path_buf()).unwrap();
        let id = manager.generate_unique_session_id();
        let path = temp_dir
            .path()
            .join(SOFOS_DIR)
            .join(SESSIONS_DIR)
            .join(format!("{}.json", id));
        assert!(!path.exists());
    }

    #[test]
    fn test_session_save_and_load() {
        let temp_dir = TempDir::new().unwrap();
        let manager = HistoryManager::new(temp_dir.path().to_path_buf()).unwrap();

        let session_id = HistoryManager::generate_session_id();
        let messages = vec![Message::user("Test message")];
        let system_prompt =
            SystemPrompt::new_cached_with_ttl("Test system prompt".to_string(), None);

        manager
            .save_session(
                &session_id,
                &messages,
                &[],
                std::slice::from_ref(&system_prompt),
                SessionTokenCounters::default(),
                "",
                false,
                None,
            )
            .unwrap();

        let loaded = manager.load_session(&session_id).unwrap();
        assert_eq!(loaded.id, session_id);
        assert_eq!(loaded.api_messages.len(), 1);
        assert_eq!(loaded.system_prompt, vec![system_prompt]);
    }

    #[test]
    fn test_list_sessions() {
        let temp_dir = TempDir::new().unwrap();
        let manager = HistoryManager::new(temp_dir.path().to_path_buf()).unwrap();

        let session_id1 = HistoryManager::generate_session_id();
        let system_prompt = SystemPrompt::new_cached_with_ttl("System".to_string(), None);

        manager
            .save_session(
                &session_id1,
                &[Message::user("First session")],
                &[],
                std::slice::from_ref(&system_prompt),
                SessionTokenCounters::default(),
                "",
                false,
                None,
            )
            .unwrap();

        std::thread::sleep(std::time::Duration::from_secs(1));

        let session_id2 = HistoryManager::generate_session_id();
        manager
            .save_session(
                &session_id2,
                &[Message::user("Second session")],
                &[],
                &[system_prompt],
                SessionTokenCounters::default(),
                "",
                false,
                None,
            )
            .unwrap();

        let sessions = manager.list_sessions().unwrap();
        assert_eq!(sessions.len(), 2);
        assert_eq!(sessions[0].preview, "Second session");
        assert_eq!(sessions[1].preview, "First session");
    }

    #[test]
    fn test_preview_extraction() {
        let messages = vec![Message::user("This is a test message")];
        let preview = HistoryManager::extract_preview(&messages);
        assert_eq!(preview, "This is a test message");

        let long_message = "a".repeat(150);
        let messages = vec![Message::user(long_message)];
        let preview = HistoryManager::extract_preview(&messages);
        assert_eq!(preview.len(), MAX_PREVIEW_LENGTH + 3);
        assert!(preview.ends_with("..."));

        // Test UTF-8 multi-byte characters (Cyrillic)
        let cyrillic_message = "създай текстов файл test-3.txt";
        let messages = vec![Message::user(cyrillic_message)];
        let preview = HistoryManager::extract_preview(&messages);
        // Should not panic and should truncate at character boundary
        assert!(preview.chars().count() <= MAX_PREVIEW_LENGTH + 3); // +3 for "..."
        if preview.ends_with("...") {
            assert!(preview.chars().count() <= MAX_PREVIEW_LENGTH + 3);
        }
    }

    /// Every persisted token counter must survive save/load. Without
    /// this, a `--resume` would reset the displayed cost (totals stay
    /// at 0 until the next API call replenishes them) and the cliff
    /// detector would forget that a premium-tier model had already crossed 272K.
    #[test]
    fn all_token_counters_survive_save_and_load() {
        let temp_dir = TempDir::new().unwrap();
        let manager = HistoryManager::new(temp_dir.path().to_path_buf()).unwrap();

        let session_id = HistoryManager::generate_session_id();
        let system_prompt = SystemPrompt::new_cached_with_ttl("sys".to_string(), None);
        let counters = SessionTokenCounters {
            total_input_tokens: 123_456,
            total_output_tokens: 7_890,
            total_cache_read_tokens: 65_000,
            total_cache_creation_tokens: 4_321,
            // > 272K — the premium-tier cliff.
            peak_single_turn_input_tokens: 300_000,
        };

        manager
            .save_session(
                &session_id,
                &[Message::user("crossed the cliff")],
                &[],
                std::slice::from_ref(&system_prompt),
                counters,
                "",
                false,
                None,
            )
            .unwrap();

        let loaded = manager.load_session(&session_id).unwrap();
        assert_eq!(loaded.token_counters, counters);
    }

    /// Older session files (written before persistence was added) have
    /// no token-counter fields at all. `#[serde(default)]` on each
    /// field of `SessionTokenCounters` must let them load with the
    /// whole struct defaulting to all zeros, otherwise older session
    /// files would fail to parse and the user would lose their saved
    /// history.
    #[test]
    fn old_session_files_without_counter_fields_load_with_zero() {
        let temp_dir = TempDir::new().unwrap();
        let manager = HistoryManager::new(temp_dir.path().to_path_buf()).unwrap();

        let session_id = "session_pre_persistence";
        let session_path = manager.sessions_dir().join(format!("{}.json", session_id));
        // Hand-rolled JSON missing every counter field — mirrors what
        // an older sofos would have written. Timestamps are 0 because
        // they don't matter for this test.
        let legacy_json = serde_json::json!({
            "id": session_id,
            "api_messages": [],
            "system_prompt": [],
            "created_at": 0,
            "updated_at": 0,
        });
        fs::write(&session_path, serde_json::to_string(&legacy_json).unwrap()).unwrap();

        let loaded = manager.load_session(session_id).unwrap();
        assert_eq!(loaded.token_counters, SessionTokenCounters::default());
    }

    /// If a session file on disk is corrupted (hand-edited, partial
    /// write from a prior crash, schema drift), `save_session` must
    /// still succeed rather than bubbling the parse error and losing
    /// the in-memory conversation.
    #[test]
    fn save_session_survives_corrupted_prior_file() {
        let temp_dir = TempDir::new().unwrap();
        let manager = HistoryManager::new(temp_dir.path().to_path_buf()).unwrap();

        let session_id = HistoryManager::generate_session_id();
        let session_path = manager.sessions_dir().join(format!("{}.json", session_id));
        fs::write(&session_path, "{not valid json at all").unwrap();

        let system_prompt = SystemPrompt::new_cached_with_ttl("System".to_string(), None);
        let save_result = manager.save_session(
            &session_id,
            &[Message::user("After corruption")],
            &[],
            std::slice::from_ref(&system_prompt),
            SessionTokenCounters::default(),
            "",
            false,
            None,
        );
        assert!(
            save_result.is_ok(),
            "save_session should recover: {save_result:?}"
        );

        let loaded = manager.load_session(&session_id).unwrap();
        assert_eq!(loaded.api_messages.len(), 1);
    }

    /// Two sofos processes sharing a workspace used to race each
    /// other's `update_index` (read-modify-write), occasionally
    /// dropping session metadata. The save-lock serialises those
    /// updates. Simulate the race with threads: 8 writers × 5 saves
    /// each, each writer using its own `HistoryManager` backed by
    /// the same on-disk directory (mirroring two processes), and
    /// assert the final index has all 8 session ids present.
    #[test]
    fn save_lock_serialises_concurrent_index_updates() {
        use std::sync::{Arc, Barrier};
        use std::thread;

        let temp_dir = TempDir::new().unwrap();
        // Ensure the sessions dir exists before any writer starts —
        // otherwise each writer races to `ensure_directories` too.
        HistoryManager::new(temp_dir.path().to_path_buf()).unwrap();

        let writer_count = 8;
        let saves_per_writer = 5;
        let barrier = Arc::new(Barrier::new(writer_count));
        let workspace = temp_dir.path().to_path_buf();

        let mut handles = Vec::new();
        for w in 0..writer_count {
            let barrier = Arc::clone(&barrier);
            let workspace = workspace.clone();
            handles.push(thread::spawn(move || {
                let manager = HistoryManager::new(workspace).unwrap();
                let system_prompt = SystemPrompt::new_cached_with_ttl("sys".to_string(), None);
                let session_id = format!("session_writer_{}", w);
                barrier.wait();
                for n in 0..saves_per_writer {
                    manager
                        .save_session(
                            &session_id,
                            &[Message::user(format!("writer {} save {}", w, n))],
                            &[],
                            std::slice::from_ref(&system_prompt),
                            SessionTokenCounters::default(),
                            "",
                            false,
                            None,
                        )
                        .unwrap();
                }
            }));
        }
        for h in handles {
            h.join().unwrap();
        }

        let sessions = HistoryManager::new(workspace)
            .unwrap()
            .list_sessions()
            .unwrap();
        assert_eq!(
            sessions.len(),
            writer_count,
            "all writers' ids should survive in the index: {sessions:?}"
        );
    }

    /// `model` and `readonly` must round-trip through save/load so the
    /// resumed session stays on the same provider and tool grant. The
    /// flatten on `token_counters` lives at the same JSON level, so this
    /// test also pins that there's no name collision.
    #[test]
    fn model_and_readonly_survive_save_and_load() {
        let temp_dir = TempDir::new().unwrap();
        let manager = HistoryManager::new(temp_dir.path().to_path_buf()).unwrap();
        let session_id = HistoryManager::generate_session_id();
        let system_prompt = SystemPrompt::new_cached_with_ttl("sys".to_string(), None);

        manager
            .save_session(
                &session_id,
                &[Message::user("hi")],
                &[],
                std::slice::from_ref(&system_prompt),
                SessionTokenCounters::default(),
                crate::api::model_info::CLAUDE_OPUS,
                true,
                Some("read-only"),
            )
            .unwrap();

        let loaded = manager.load_session(&session_id).unwrap();
        assert_eq!(
            loaded.model.as_deref(),
            Some(crate::api::model_info::CLAUDE_OPUS)
        );
        assert_eq!(loaded.readonly, Some(true));
        assert_eq!(loaded.permission_preset.as_deref(), Some("read-only"));
    }

    /// Older session files written before `model` and `readonly` existed
    /// must still load, with both fields defaulting to their empty values
    /// so the in-memory state on `--resume` falls back to whatever the
    /// CLI selected.
    #[test]
    fn legacy_session_without_model_or_readonly_loads_with_defaults() {
        let temp_dir = TempDir::new().unwrap();
        let manager = HistoryManager::new(temp_dir.path().to_path_buf()).unwrap();
        let session_id = "session_pre_model";
        let session_path = manager.sessions_dir().join(format!("{}.json", session_id));
        let legacy_json = serde_json::json!({
            "id": session_id,
            "api_messages": [],
            "system_prompt": [],
            "created_at": 0,
            "updated_at": 0,
        });
        fs::write(&session_path, serde_json::to_string(&legacy_json).unwrap()).unwrap();

        let loaded = manager.load_session(session_id).unwrap();
        assert!(loaded.model.is_none());
        assert!(loaded.readonly.is_none());
    }

    /// `save_session` / `load_session` must refuse session ids that would
    /// escape the sessions directory when interpolated into a path —
    /// `Repl::load_session_by_id` is `pub` and reachable from `--resume`
    /// with a user-controlled string. The generator only emits
    /// `session_<timestamp>`, so this is defensive against external
    /// callers, not the happy path.
    #[test]
    fn save_and_load_reject_traversing_session_ids() {
        let temp_dir = TempDir::new().unwrap();
        let manager = HistoryManager::new(temp_dir.path().to_path_buf()).unwrap();
        let system_prompt = SystemPrompt::new_cached_with_ttl("sys".to_string(), None);

        for bad in ["..", ".", "../escape", "a/b", "a\\b", ""] {
            let save_err = manager
                .save_session(
                    bad,
                    &[Message::user("x")],
                    &[],
                    std::slice::from_ref(&system_prompt),
                    SessionTokenCounters::default(),
                    "",
                    false,
                    None,
                )
                .err();
            assert!(save_err.is_some(), "save_session must reject '{}'", bad);

            let load_err = manager.load_session(bad).err();
            assert!(load_err.is_some(), "load_session must reject '{}'", bad);
        }
    }
}
