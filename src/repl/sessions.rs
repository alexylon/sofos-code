//! Saved-session lifecycle: listing past sessions, persisting the
//! current one to disk, and resuming a previous session into the live
//! [`Repl`] (`/resume` and direct `--session` loads). The actual disk
//! layout lives in [`crate::session::HistoryManager`]; this module is
//! the thin REPL-side adapter that translates between
//! [`crate::session::SessionState`] and the persisted form.

use crate::config::SandboxMode;
use crate::error::{Result, SofosError};
use crate::repl::Repl;
use crate::session::{SessionMetadata, SessionTokenCounters};
use colored::Colorize;

/// Single-fact wrapper over the canonical provider lookup in
/// `api::model_info`. `load_session_by_id` calls this to detect a
/// cross-provider resume without spinning up the wrong API client.
fn provider_of(model: &str) -> &'static str {
    crate::api::model_info::provider_for(model).label()
}

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
            &self.model_config.model,
            self.mode.is_readonly(),
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

        // Refuse to resume across providers. The `LlmClient` was
        // constructed at startup from `cli.model` (Anthropic vs OpenAI
        // is decided there) and can't be swapped mid-process without
        // re-reading the API keys. If we let the load continue, the
        // saved Anthropic content blocks (`thinking`, `compaction`)
        // would either crash the OpenAI wire layer or get silently
        // dropped, and vice versa. Better to surface a clear error so
        // the user re-launches with the right `--model`.
        //
        // Validate `(saved_model, current_effort)` here too — the
        // saved model overrides the CLI model further down, and we
        // can't let an `xhigh`/`max` choice that the saved model
        // doesn't accept slip through. Both refusals happen before
        // any session-state mutation so a rejected resume leaves the
        // current Repl untouched.
        if let Some(saved_model) = session.model.as_deref() {
            if !saved_model.is_empty() {
                let saved_provider = provider_of(saved_model);
                let current_provider = self.client.provider_name();
                if saved_provider != current_provider {
                    return Err(SofosError::Config(format!(
                        "Session was saved under model '{}' ({}), but the current client is {}. \
                         Re-launch with `--model {}` to resume.",
                        saved_model, saved_provider, current_provider, saved_model
                    )));
                }
                // The saved model is about to override `--model` further
                // down, so refuse the resume if that override would land
                // on a slug the application no longer supports. The
                // resumed session would otherwise send an unrecognised
                // model id on the wire and fail at request time.
                if crate::api::model_info::canonical_model(saved_model).is_none() {
                    return Err(SofosError::Config(format!(
                        "Session was saved under model '{}', which is no longer supported. \
                         Supported models: {}.",
                        saved_model,
                        crate::api::model_info::supported_models_label()
                    )));
                }
                if saved_model != self.model_config.model {
                    if let Some(msg) = crate::api::model_info::effort_support_error(
                        saved_model,
                        self.model_config.reasoning_effort,
                    ) {
                        return Err(SofosError::Config(format!(
                            "{} Re-launch with `--reasoning-effort` set to a level the saved model accepts.",
                            msg
                        )));
                    }
                }
            }
        }

        self.session_state.session_id = session.id.clone();
        self.session_state.conversation.clear();
        self.session_state
            .conversation
            .restore_messages(session.api_messages.clone());
        // Restore the persisted system prompt so the resumed conversation
        // sees the same system context the assistant was answering
        // against at save time. Older session files always carry a
        // non-empty prompt because the field has been in the schema since
        // v1; the guard here is purely defensive.
        if !session.system_prompt.is_empty() {
            self.session_state
                .conversation
                .set_system_prompt(session.system_prompt.clone());
        }
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

        // Restore the model the session was running under so streaming
        // continuity holds across providers. `None` means the file was
        // written before this field existed — keep whatever the CLI
        // selected in that case so old sessions don't lose the user's
        // current `--model` choice. Saved slugs land in canonical form
        // so a mixed-case file still compares equal to the picker rows
        // and the wire payload uses the spelling the provider expects.
        if let Some(saved_model) = session.model.as_deref() {
            if !saved_model.is_empty() {
                let canonical = crate::api::model_info::canonical_model(saved_model)
                    .map(|c| c.name.to_string())
                    .unwrap_or_else(|| saved_model.to_string());
                if canonical != self.model_config.model {
                    println!(
                        "{} session was saved under model '{}'; continuing with that instead of '{}'",
                        "Note:".dimmed(),
                        canonical,
                        self.model_config.model
                    );
                    self.model_config.model = canonical;
                }
            }
        }

        // Restore read-only silently when the file records it. The
        // saved conversation already reflects whichever tool grant was
        // active, so we just sync the in-memory flag and the tool
        // executor's allow-list. Older files with no `readonly` field
        // (`None`) keep the CLI value, so a `--readonly --resume`
        // against a pre-persistence file still honours the flag.
        if let Some(saved_readonly) = session.readonly {
            if saved_readonly != self.mode.is_readonly() {
                self.mode = if saved_readonly {
                    SandboxMode::ReadOnly
                } else {
                    // Leaving read-only returns to the host default. Honour
                    // sandbox availability the way startup does, so a host
                    // without a usable sandbox lands in Unsandboxed rather
                    // than a Sandboxed mode that cannot confine anything.
                    SandboxMode::from_flags(
                        false,
                        false,
                        crate::tools::bash::sandbox::is_available(),
                    )
                };
                self.tool_executor.set_mode(self.mode);
                self.refresh_available_tools();
            }
        }

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

#[cfg(test)]
mod tests {
    use super::provider_of;

    #[test]
    fn provider_of_matches_build_llm_client_routing() {
        // Every model in the supported whitelist must route to the
        // provider its record names — `build_llm_client` in `main.rs`
        // picks the API client off the same field, and the
        // cross-provider resume check in `load_session_by_id` compares
        // these strings against `LlmClient::provider_name` directly.
        for m in crate::api::model_info::SUPPORTED_MODELS {
            assert_eq!(
                provider_of(m.name),
                m.provider.label(),
                "{} should route to {}",
                m.name,
                m.provider.label()
            );
        }
        // Unsupported slugs fall through to the default model's
        // provider (Anthropic, because the default is an Anthropic
        // model); `build_llm_client` mirrors that fallback.
        assert_eq!(provider_of("unknown-model"), "Anthropic");
    }
}
