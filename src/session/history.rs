use crate::api::{Message, SystemPrompt};
use crate::error::{Result, SofosError};
use crate::error_ext::ResultExt;

use serde::{Deserialize, Serialize};
use std::fs::{self, File, OpenOptions};
use std::path::PathBuf;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

const SOFOS_DIR: &str = ".sofos";
const SESSIONS_DIR: &str = "sessions";
const INDEX_FILE: &str = "index.json";
/// Per-workspace lock file the session subsystem grabs exclusively for
/// the duration of every `save_session` / `delete_session` call.
/// Serialises the read-modify-write of `index.json` across concurrent
/// sofos processes working in the same directory — without it, two
/// instances racing each other can each read the stale index, append
/// their own entry, and then clobber the other's update with their
/// own `atomic_write`, silently losing session metadata.
const SAVE_LOCK_FILE: &str = ".save.lock";
const MAX_PREVIEW_LENGTH: usize = 120;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum DisplayMessage {
    UserMessage {
        content: String,
    },
    AssistantMessage {
        content: String,
    },
    ToolExecution {
        tool_name: String,
        tool_input: serde_json::Value,
        tool_output: String,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionMetadata {
    pub id: String,
    pub preview: String,
    pub created_at: u64,
    pub updated_at: u64,
    pub message_count: usize,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct Session {
    pub id: String,
    /// Messages in API format (for continuing the conversation with AI)
    pub api_messages: Vec<Message>,
    /// Messages in display format (for reconstructing the original UI)
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub display_messages: Vec<DisplayMessage>,
    pub system_prompt: Vec<SystemPrompt>,
    pub created_at: u64,
    pub updated_at: u64,
}

#[derive(Debug, Serialize, Deserialize)]
struct SessionIndex {
    sessions: Vec<SessionMetadata>,
}

pub struct HistoryManager {
    workspace: PathBuf,
}

impl HistoryManager {
    pub fn new(workspace: PathBuf) -> Result<Self> {
        let manager = Self { workspace };
        manager.ensure_directories()?;
        Ok(manager)
    }

    fn ensure_directories(&self) -> Result<()> {
        let sofos_dir = self.workspace.join(SOFOS_DIR);
        let sessions_dir = sofos_dir.join(SESSIONS_DIR);

        fs::create_dir_all(&sessions_dir).map_err(|e| {
            SofosError::Io(std::io::Error::new(
                e.kind(),
                format!("Failed to create .sofos directories: {}", e),
            ))
        })?;

        let index_path = sessions_dir.join(INDEX_FILE);
        if !index_path.exists() {
            let index = SessionIndex {
                sessions: Vec::new(),
            };
            let content = serde_json::to_string_pretty(&index)?;
            atomic_write(&index_path, &content)?;
        }

        Ok(())
    }

    fn sessions_dir(&self) -> PathBuf {
        self.workspace.join(SOFOS_DIR).join(SESSIONS_DIR)
    }

    fn index_path(&self) -> PathBuf {
        self.sessions_dir().join(INDEX_FILE)
    }

    fn save_lock_path(&self) -> PathBuf {
        self.sessions_dir().join(SAVE_LOCK_FILE)
    }

    /// Acquire an exclusive OS-level lock on the save-lock file for
    /// the lifetime of the returned `File`; the OS releases the lock
    /// when the handle drops, including on crash.
    fn acquire_save_lock(&self) -> Result<File> {
        let path = self.save_lock_path();
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(&path)
            .map_err(|e| {
                SofosError::Io(std::io::Error::new(
                    e.kind(),
                    format!("Failed to open session save-lock {:?}: {}", path, e),
                ))
            })?;
        file.lock().map_err(|e| {
            SofosError::Io(std::io::Error::new(
                e.kind(),
                format!("Failed to acquire session save-lock: {}", e),
            ))
        })?;
        Ok(file)
    }

    pub fn generate_session_id() -> String {
        let timestamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or(Duration::ZERO)
            .as_millis();
        format!("session_{}", timestamp)
    }

    fn extract_preview(messages: &[Message]) -> String {
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

    pub fn save_session(
        &self,
        session_id: &str,
        messages: &[Message],
        display_messages: &[DisplayMessage],
        system_prompt: &[SystemPrompt],
    ) -> Result<()> {
        let _lock = self.acquire_save_lock()?;

        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or(Duration::ZERO)
            .as_secs();

        let session_path = self.sessions_dir().join(format!("{}.json", session_id));

        // Preserve `created_at` from any prior save. If the old file is
        // unreadable or no longer parses (user edited it, disk
        // corruption, schema change), fall back to `now` rather than
        // propagating the error — losing the in-memory conversation to
        // save a `created_at` stamp would be an awful trade.
        let created_at = match fs::read_to_string(&session_path) {
            Ok(raw) => serde_json::from_str::<Session>(&raw)
                .map(|existing| existing.created_at)
                .unwrap_or(now),
            Err(_) => now,
        };
        let session = Session {
            id: session_id.to_string(),
            api_messages: messages.to_vec(),
            display_messages: display_messages.to_vec(),
            system_prompt: system_prompt.to_vec(),
            created_at,
            updated_at: now,
        };

        let content = serde_json::to_string_pretty(&session)?;
        atomic_write(&session_path, &content)?;

        self.update_index(&session)?;

        Ok(())
    }

    fn update_index(&self, session: &Session) -> Result<()> {
        let index_path = self.index_path();
        let mut index: SessionIndex = if index_path.exists() {
            serde_json::from_str(&fs::read_to_string(&index_path)?)?
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
            .sort_by(|a, b| b.updated_at.cmp(&a.updated_at));

        let content = serde_json::to_string_pretty(&index)?;
        atomic_write(&index_path, &content)?;

        Ok(())
    }

    pub fn load_session(&self, session_id: &str) -> Result<Session> {
        let session_path = self.sessions_dir().join(format!("{}.json", session_id));

        if !session_path.exists() {
            return Err(SofosError::Config(format!(
                "Session '{}' not found",
                session_id
            )));
        }

        let content = fs::read_to_string(session_path)?;
        let session: Session = serde_json::from_str(&content)?;

        Ok(session)
    }

    pub fn load_custom_instructions(&self) -> Result<Option<String>> {
        let project_rc = self.workspace.join("AGENTS.md");
        let personal_instructions = self.workspace.join(".sofos/instructions.md");

        let mut combined = String::new();

        if project_rc.exists() {
            let content = fs::read_to_string(&project_rc).with_context(|| {
                format!("Failed to read project instructions from {:?}", project_rc)
            })?;
            combined.push_str(&content);
        }

        if personal_instructions.exists() {
            if !combined.is_empty() {
                combined.push_str("\n\n");
            }
            let content = fs::read_to_string(&personal_instructions).with_context(|| {
                format!(
                    "Failed to read personal instructions from {:?}",
                    personal_instructions
                )
            })?;
            combined.push_str(&content);
        }

        if combined.is_empty() {
            Ok(None)
        } else {
            Ok(Some(combined))
        }
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

    #[allow(dead_code)]
    pub fn delete_session(&self, session_id: &str) -> Result<()> {
        let _lock = self.acquire_save_lock()?;

        let session_path = self.sessions_dir().join(format!("{}.json", session_id));

        if session_path.exists() {
            fs::remove_file(session_path)?;
        }

        let index_path = self.index_path();
        if index_path.exists() {
            let mut index: SessionIndex = serde_json::from_str(&fs::read_to_string(&index_path)?)?;
            index.sessions.retain(|s| s.id != session_id);

            let content = serde_json::to_string_pretty(&index)?;
            atomic_write(&index_path, &content)?;
        }

        Ok(())
    }
}

/// Write content to a file atomically by writing to a temp file first, then renaming.
/// This prevents corruption if the process crashes mid-write.
fn atomic_write(path: &PathBuf, content: &str) -> Result<()> {
    let tmp_path = path.with_extension("json.tmp");
    fs::write(&tmp_path, content)?;
    fs::rename(&tmp_path, path)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::SystemPrompt;
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
}
