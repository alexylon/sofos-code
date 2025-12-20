use crate::api::{Message, SystemPrompt};
use crate::error::{Result, SofosError};
use crate::error_ext::ResultExt;

use serde::{Deserialize, Serialize};
use std::fs;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

const SOFOS_DIR: &str = ".sofos";
const SESSIONS_DIR: &str = "sessions";
const INDEX_FILE: &str = "index.json";
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
            fs::write(index_path, content)?;
        }

        Ok(())
    }

    fn sessions_dir(&self) -> PathBuf {
        self.workspace.join(SOFOS_DIR).join(SESSIONS_DIR)
    }

    fn index_path(&self) -> PathBuf {
        self.sessions_dir().join(INDEX_FILE)
    }

    pub fn generate_session_id() -> String {
        let timestamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("System time is before UNIX epoch")
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

                return if preview.len() > MAX_PREVIEW_LENGTH {
                    format!("{}...", &preview[..MAX_PREVIEW_LENGTH])
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
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("System time is before UNIX epoch")
            .as_secs();

        let session_path = self.sessions_dir().join(format!("{}.json", session_id));

        let session = Session {
            id: session_id.to_string(),
            api_messages: messages.to_vec(),
            display_messages: display_messages.to_vec(),
            system_prompt: system_prompt.to_vec(),
            created_at: if session_path.exists() {
                let existing: Session = serde_json::from_str(&fs::read_to_string(&session_path)?)?;
                existing.created_at
            } else {
                now
            },
            updated_at: now,
        };

        let content = serde_json::to_string_pretty(&session)?;
        fs::write(&session_path, content)?;

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
        fs::write(index_path, content)?;

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
        let project_rc = self.workspace.join(".sofosrc");
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
        let session_path = self.sessions_dir().join(format!("{}.json", session_id));

        if session_path.exists() {
            fs::remove_file(session_path)?;
        }

        let index_path = self.index_path();
        if index_path.exists() {
            let mut index: SessionIndex = serde_json::from_str(&fs::read_to_string(&index_path)?)?;
            index.sessions.retain(|s| s.id != session_id);

            let content = serde_json::to_string_pretty(&index)?;
            fs::write(index_path, content)?;
        }

        Ok(())
    }
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
            .save_session(&session_id, &messages, &[], &[system_prompt.clone()])
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
                &[system_prompt.clone()],
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
    }
}
