//! High-level [`HistoryManager`] facade and the on-disk layout it
//! enforces. Owns the per-workspace `.sofos/sessions/` directory plus
//! the save-lock that serialises concurrent writers, and is the public
//! entry point every caller uses to persist or reload a session.

use crate::api::{Message, SystemPrompt};
use crate::error::{Result, SofosError};
use crate::session::history::atomic_write;
use crate::session::history::index::{INDEX_FILE, SessionIndex};
use crate::session::history::model::{DisplayMessage, Session, SessionTokenCounters};
use std::fs::{self, File, OpenOptions};
use std::path::PathBuf;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

pub(super) const SOFOS_DIR: &str = ".sofos";
pub(super) const SESSIONS_DIR: &str = "sessions";

/// Number of regenerations attempted by `generate_unique_session_id`
/// before falling back to a non-uniqueness-checked id. The random
/// suffix already makes the chance of a collision astronomically
/// small; the retry loop only matters if two processes happen to
/// generate the same byte stream in the same millisecond, which is
/// itself effectively impossible.
const SESSION_ID_UNIQUE_RETRIES: usize = 8;
/// Per-workspace lock file the session subsystem grabs exclusively for
/// the duration of every `save_session` / `delete_session` call.
/// Serialises the read-modify-write of `index.json` across concurrent
/// sofos processes working in the same directory — without it, two
/// instances racing each other can each read the stale index, append
/// their own entry, and then clobber the other's update with their
/// own `atomic_write`, silently losing session metadata.
const SAVE_LOCK_FILE: &str = ".save.lock";

pub struct HistoryManager {
    pub(super) workspace: PathBuf,
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

    pub(super) fn sessions_dir(&self) -> PathBuf {
        self.workspace.join(SOFOS_DIR).join(SESSIONS_DIR)
    }

    pub(super) fn index_path(&self) -> PathBuf {
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
        let suffix = random_session_suffix();
        format!("session_{}_{}", timestamp, suffix)
    }

    /// Generate a session id that does not clash with any existing file
    /// in this workspace's sessions directory. Two Sofos processes
    /// started in the same millisecond previously produced identical
    /// ids and clobbered each other's session file on save; the random
    /// suffix makes the chance of overlap vanishingly small, and on
    /// the off chance one slips through we regenerate.
    pub fn generate_unique_session_id(&self) -> String {
        for _ in 0..SESSION_ID_UNIQUE_RETRIES {
            let id = Self::generate_session_id();
            let path = self.sessions_dir().join(format!("{}.json", id));
            if !path.exists() {
                return id;
            }
        }
        Self::generate_session_id()
    }

    /// Reject session ids that could escape the sessions directory
    /// when interpolated into a path. The generator only produces
    /// `session_<timestamp>_<random>` strings, so anything containing a
    /// path separator or `..` came from an external caller (e.g.
    /// `--resume <id>`) and must not be trusted.
    fn validate_session_id(session_id: &str) -> Result<()> {
        if session_id.is_empty()
            || session_id == "."
            || session_id == ".."
            || session_id.contains(['/', '\\'])
        {
            return Err(SofosError::Config(format!(
                "Invalid session id '{}'",
                session_id
            )));
        }
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    pub fn save_session(
        &self,
        session_id: &str,
        messages: &[Message],
        display_messages: &[DisplayMessage],
        system_prompt: &[SystemPrompt],
        token_counters: SessionTokenCounters,
        model: &str,
        readonly: bool,
        permission_preset: Option<&str>,
    ) -> Result<()> {
        Self::validate_session_id(session_id)?;
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
            Ok(raw) => match serde_json::from_str::<Session>(&raw) {
                Ok(existing) => existing.created_at,
                Err(e) => {
                    tracing::warn!(
                        session_id = %session_id,
                        error = %e,
                        "failed to parse prior session save; resetting created_at to now"
                    );
                    now
                }
            },
            Err(_) => now,
        };
        let session = Session {
            id: session_id.to_string(),
            api_messages: messages.to_vec(),
            display_messages: display_messages.to_vec(),
            system_prompt: system_prompt.to_vec(),
            created_at,
            updated_at: now,
            token_counters,
            model: Some(model.to_string()),
            readonly: Some(readonly),
            permission_preset: permission_preset.map(str::to_string),
        };

        let content = serde_json::to_string_pretty(&session)?;
        atomic_write(&session_path, &content)?;

        self.update_index(&session)?;

        Ok(())
    }

    pub fn load_session(&self, session_id: &str) -> Result<Session> {
        Self::validate_session_id(session_id)?;
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

    #[allow(dead_code)]
    pub fn delete_session(&self, session_id: &str) -> Result<()> {
        Self::validate_session_id(session_id)?;
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

/// Eight-character hex suffix from the system random source. Used by
/// `generate_session_id` so two processes started in the same
/// millisecond do not collide on the resulting session id.
fn random_session_suffix() -> String {
    use rand::RngExt;
    let n = rand::rng().random_range(0..=u32::MAX);
    format!("{:08x}", n)
}
