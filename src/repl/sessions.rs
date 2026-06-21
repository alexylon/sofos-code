//! Saved-session lifecycle: listing past sessions, persisting the
//! current one to disk, and resuming a previous session into the live
//! [`Repl`] (`/resume` and direct `--session` loads). The actual disk
//! layout lives in [`crate::session::HistoryManager`]; this module is
//! the thin REPL-side adapter that translates between
//! [`crate::session::SessionState`] and the persisted form.

use crate::config::{ApprovalPolicy, PermissionPreset, SandboxMode};
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

/// Decide which permissions preset a resumed session should restore to.
/// Prefers the saved preset label; falls back to the legacy `readonly`
/// bool for files written before the preset was persisted; returns `None`
/// when neither field is present, so the startup choice stands. A saved
/// sandboxed preset that cannot run on this host is coerced to the
/// availability-aware default, so the restored mode never claims a
/// confinement that cannot take effect.
fn preset_to_restore(
    saved_preset: Option<&str>,
    saved_readonly: Option<bool>,
    sandbox_available: bool,
) -> Option<PermissionPreset> {
    let host_default = || {
        PermissionPreset::current(
            SandboxMode::from_flags(false, false, sandbox_available),
            ApprovalPolicy::default(),
        )
    };
    let preset = match saved_preset.and_then(PermissionPreset::parse) {
        Some(preset) => preset,
        // Older file: only read-only-ness was recorded. Read-only restores
        // read-only; a non-read-only file returns to the host default, the
        // same behaviour as before the preset was persisted.
        None => match saved_readonly {
            Some(true) => PermissionPreset::ReadOnly,
            Some(false) => host_default(),
            None => return None,
        },
    };
    Some(if preset.is_available(sandbox_available) {
        preset
    } else {
        host_default()
    })
}

impl Repl {
    pub fn list_saved_sessions(&self) -> Result<Vec<SessionMetadata>> {
        self.history_manager.list_sessions()
    }

    pub fn save_current_session(&self) -> Result<()> {
        if self.session_state.conversation.messages().is_empty() {
            return Ok(());
        }

        let preset = PermissionPreset::current(self.mode, self.approval_policy);
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
            Some(preset.label()),
        )?;

        Ok(())
    }

    /// Apply a permissions preset while resuming, without the notice or mode
    /// preamble that `apply_permission_preset` adds: the resumed conversation
    /// already reflects the saved state. The terminal cursor is still synced
    /// to the restored mode, because the cursor shows the live access mode
    /// rather than the conversation, so leaving it would strand the glyph on
    /// the pre-resume mode.
    fn restore_permission_preset(&mut self, preset: PermissionPreset) {
        let mode = preset.mode();
        if self.mode != mode {
            self.mode = mode;
            self.tool_executor.set_mode(mode);
            self.refresh_available_tools();
            // Best-effort: a failed SGR write here is purely cosmetic.
            let _ = if mode.is_readonly() {
                crate::ui::set_readonly_cursor_style()
            } else {
                crate::ui::set_default_cursor_style()
            };
        }
        if let Some(policy) = preset.escalation() {
            self.approval_policy = policy;
            self.tool_executor.set_approval_policy(policy);
        }
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

        // Restore the permissions preset silently when the file records
        // one. The saved conversation already reflects whichever access
        // mode and escalation policy were active, so we sync the in-memory
        // state and the tool executor without re-announcing the mode or
        // re-adding a preamble. A saved sandboxed preset that cannot run on
        // this host is coerced to the availability-aware default. Older
        // files carry only `readonly` (or neither field, keeping the CLI
        // value), which `preset_to_restore` folds in.
        if let Some(preset) = preset_to_restore(
            session.permission_preset.as_deref(),
            session.readonly,
            crate::tools::bash::sandbox::is_available(),
        ) {
            // When a saved sandboxed preset is coerced to unsandboxed because
            // this host has no operating-system sandbox, the restored
            // conversation still describes confinement that is not in effect.
            // Re-announce the live mode so the model is not left assuming a
            // sandbox that cannot run here.
            let saved_mode = session
                .permission_preset
                .as_deref()
                .and_then(PermissionPreset::parse)
                .map(PermissionPreset::mode);
            let preset_before = PermissionPreset::current(self.mode, self.approval_policy);
            self.restore_permission_preset(preset);
            if saved_mode.is_some_and(|saved| saved != preset.mode()) {
                self.session_state
                    .conversation
                    .add_user_message(super::mode_preamble_for(self.mode, self.approval_policy));
            }
            // A relaxed saved preset must not silently undo stricter settings
            // that were in effect: surface a loosening of either the access
            // mode or, within sandboxed mode, the escalation policy, so the
            // user knows the resumed session runs with fewer restrictions.
            let preset_after = PermissionPreset::current(self.mode, self.approval_policy);
            if preset_after.is_more_permissive_than(preset_before) {
                println!(
                    "{} resuming this session relaxed permissions from {} to {}. \
                     Commands now run with fewer restrictions; use /permissions to change it.",
                    "Note:".yellow(),
                    preset_before.label(),
                    preset_after.label(),
                );
                println!();
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
    use super::{preset_to_restore, provider_of};
    use crate::config::PermissionPreset;

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

    /// The core of L-g: a saved sandboxed preset resumes as itself instead
    /// of collapsing to the default `sandboxed-ask`.
    #[test]
    fn preset_to_restore_keeps_a_saved_sandboxed_preset_when_it_can_run() {
        assert_eq!(
            preset_to_restore(
                Some(PermissionPreset::SandboxedStrict.label()),
                Some(false),
                true
            ),
            Some(PermissionPreset::SandboxedStrict)
        );
        assert_eq!(
            preset_to_restore(
                Some(PermissionPreset::SandboxedRetry.label()),
                Some(false),
                true
            ),
            Some(PermissionPreset::SandboxedRetry)
        );
    }

    /// A saved sandboxed preset cannot run where no sandbox exists, so it
    /// is coerced to the availability-aware default. Read-only and
    /// unsandboxed always apply.
    #[test]
    fn preset_to_restore_coerces_a_sandboxed_preset_where_no_sandbox_runs() {
        assert_eq!(
            preset_to_restore(
                Some(PermissionPreset::SandboxedStrict.label()),
                Some(false),
                false
            ),
            Some(PermissionPreset::Unsandboxed)
        );
        assert_eq!(
            preset_to_restore(Some(PermissionPreset::ReadOnly.label()), None, false),
            Some(PermissionPreset::ReadOnly)
        );
    }

    /// Older files carry only the `readonly` bool; it still drives the
    /// restored mode, with a non-read-only file returning to the host
    /// default.
    #[test]
    fn preset_to_restore_falls_back_to_the_legacy_readonly_flag() {
        assert_eq!(
            preset_to_restore(None, Some(true), true),
            Some(PermissionPreset::ReadOnly)
        );
        assert_eq!(
            preset_to_restore(None, Some(false), true),
            Some(PermissionPreset::SandboxedAsk)
        );
        assert_eq!(
            preset_to_restore(None, Some(false), false),
            Some(PermissionPreset::Unsandboxed)
        );
    }

    /// A file written before either field existed, or one with an
    /// unparseable label, keeps whatever the startup chose.
    #[test]
    fn preset_to_restore_keeps_startup_choice_for_pre_persistence_files() {
        assert_eq!(preset_to_restore(None, None, true), None);
        assert_eq!(preset_to_restore(Some("bogus"), None, true), None);
    }
}
