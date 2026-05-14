//! Saved-session lifecycle: listing past sessions, persisting the
//! current one to disk, and resuming a previous session into the live
//! [`Repl`] (`/resume` and direct `--session` loads). The actual disk
//! layout lives in [`crate::session::HistoryManager`]; this module is
//! the thin REPL-side adapter that translates between
//! [`crate::session::SessionState`] and the persisted form.

use crate::error::Result;
use crate::repl::Repl;
use crate::session::{SessionMetadata, SessionTokenCounters};
use colored::Colorize;

impl Repl {
    pub fn list_saved_sessions(&self) -> Result<Vec<SessionMetadata>> {
        self.history_manager.list_sessions()
    }

    pub fn save_current_session(&self) -> Result<()> {
        if self.session_state.conversation.messages().is_empty() {
            return Ok(());
        }

        self.history_manager.save_session(
            &self.session_state.session_id,
            self.session_state.conversation.messages(),
            &self.session_state.display_messages,
            self.session_state.conversation.system_prompt(),
            SessionTokenCounters {
                total_input_tokens: self.session_state.total_input_tokens,
                total_output_tokens: self.session_state.total_output_tokens,
                total_cache_read_tokens: self.session_state.total_cache_read_tokens,
                total_cache_creation_tokens: self.session_state.total_cache_creation_tokens,
                peak_single_turn_input_tokens: self.session_state.peak_single_turn_input_tokens,
            },
        )?;

        Ok(())
    }

    pub fn handle_resume_command(&mut self) -> Result<()> {
        let sessions = self.history_manager.list_sessions()?;

        if sessions.is_empty() {
            println!("{}", "No saved sessions found.".yellow());
            return Ok(());
        }

        let selected_id = crate::session::select_session(sessions)?;

        if let Some(session_id) = selected_id {
            self.load_session_by_id(&session_id)?;
            println!(
                "{} {}",
                "Session loaded:".bright_green(),
                "Continue your conversation below".dimmed()
            );
            println!();
        }

        Ok(())
    }

    pub fn load_session_by_id(&mut self, session_id: &str) -> Result<()> {
        let session = self.history_manager.load_session(session_id)?;

        self.session_state.session_id = session.id.clone();
        self.session_state.conversation.clear();
        self.session_state
            .conversation
            .restore_messages(session.api_messages.clone());
        self.session_state.display_messages = session.display_messages.clone();
        // Restore every persisted token counter so the cost summary
        // stays accurate across the resume. Older session files written
        // before persistence was added default the whole
        // `token_counters` struct to all-zero via `#[serde(default)]`
        // on each field, matching the pre-persistence behaviour for
        // those old files.
        self.session_state.total_input_tokens = session.token_counters.total_input_tokens;
        self.session_state.total_output_tokens = session.token_counters.total_output_tokens;
        self.session_state.total_cache_read_tokens = session.token_counters.total_cache_read_tokens;
        self.session_state.total_cache_creation_tokens =
            session.token_counters.total_cache_creation_tokens;
        self.session_state.peak_single_turn_input_tokens =
            session.token_counters.peak_single_turn_input_tokens;

        println!(
            "{} {} ({} messages)",
            "Loaded session:".bright_green(),
            session.id,
            session.api_messages.len()
        );
        println!();

        self.ui.display_session(&session)?;

        Ok(())
    }
}
