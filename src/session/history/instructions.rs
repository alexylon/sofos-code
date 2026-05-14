//! Custom-instruction discovery: stitches together the project-level
//! `AGENTS.md` and the personal `.sofos/instructions.md`, returning
//! the concatenation that gets appended to the system prompt at REPL
//! startup. Either file is optional; returns `None` when neither
//! exists.

use crate::error::{Result, ResultExt};
use crate::session::history::HistoryManager;
use std::fs;

impl HistoryManager {
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
}
