/// Central configuration for Sofos. The actual file-size and bash-output
/// caps live next to the code that enforces them — `MAX_FILE_SIZE` in
/// `src/tools/filesystem.rs` (50 MB) and `MAX_BASH_OUTPUT_BYTES` in
/// `src/tools/bashexec.rs` (10 MB) — not here, so this struct only
/// carries config values that the rest of the crate actually reads.
///
/// Per-model knowledge (context window, auto-compact trigger,
/// pricing, adaptive-thinking flag) lives in
/// [`crate::api::model_info::lookup`] instead of in this struct.
/// `set_max_context_tokens` and `set_auto_compact_token_limit` on
/// [`crate::repl::conversation::ConversationHistory`] populate the
/// runtime values from the model lookup at REPL startup.
#[derive(Debug, Clone)]
pub struct SofosConfig {
    pub max_messages: usize,
    /// Hard drop-trim floor in tokens. Above this, older messages are
    /// dropped without summary as a last resort. Populated from
    /// `Model::effective_window()` at startup.
    pub max_context_tokens: usize,
    pub max_tool_iterations: u32,
    /// Auto-compaction trigger in tokens. Compaction runs an LLM
    /// summary step that preserves context, so this fires well below
    /// `max_context_tokens`. Populated from `Model::auto_compact_at()`
    /// at startup.
    pub auto_compact_token_limit: usize,
    /// Number of recent messages to preserve during compaction
    pub compaction_preserve_recent: usize,
    /// Truncate tool results longer than this (chars) during compaction
    pub tool_result_truncate_threshold: usize,
}

impl Default for SofosConfig {
    fn default() -> Self {
        // Defaults track the application-default model (see
        // `crate::api::Model::default`) so the numbers visible before
        // the user has picked a model match what the default produces.
        // These get overwritten by model-specific values at REPL
        // startup.
        let info = crate::api::Model::default();
        Self {
            max_messages: 500,
            max_context_tokens: info.effective_window() as usize,
            max_tool_iterations: 200,
            auto_compact_token_limit: info.auto_compact_at() as usize,
            compaction_preserve_recent: 20,
            tool_result_truncate_threshold: 2000,
        }
    }
}

/// Configuration for the language model
#[derive(Clone)]
pub struct ModelConfig {
    pub model: String,
    pub max_tokens: u32,
    pub reasoning_effort: crate::api::ReasoningEffort,
}

impl ModelConfig {
    pub fn new(
        model: String,
        max_tokens: u32,
        reasoning_effort: crate::api::ReasoningEffort,
    ) -> Self {
        Self {
            model,
            max_tokens,
            reasoning_effort,
        }
    }

    pub fn set_reasoning_effort(&mut self, effort: crate::api::ReasoningEffort) {
        self.reasoning_effort = effort;
    }
}

/// Per-model trim-safety floor. Above this value the conversation
/// trim drops older messages without summary as a last resort —
/// auto-compaction (which preserves context) runs much earlier at
/// [`crate::api::Model::auto_compact_at`]. Both numbers come
/// from the same per-model lookup so a single
/// [`crate::api::model_info::lookup`] call is the source of truth.
pub fn max_context_tokens_for(model: &str) -> usize {
    crate::api::model_info::lookup(model).effective_window() as usize
}

/// Auto-compaction trigger for `model`. Keeps the cost-shaping cap and
/// the API ceiling as separate concepts: this is where the LLM-summary
/// phase fires, while [`max_context_tokens_for`] is where the hard
/// drop-trim kicks in.
pub fn auto_compact_token_limit_for(model: &str) -> usize {
    crate::api::model_info::lookup(model).auto_compact_at() as usize
}

/// Safe mode message shown to user and AI. Must stay in sync with the
/// tool set returned by `tools::get_read_only_tools()` (+ the optional
/// `search_code` tool wired in when ripgrep is on PATH).
pub const SAFE_MODE_MESSAGE: &str = "[SYSTEM: Safe (read-only) mode has been enabled. \
                                     No file modifications or bash commands are allowed. \
                                     Available native tools: list_directory, read_file, glob_files, \
                                     search_code (when ripgrep is installed), update_plan, \
                                     web_fetch, web_search. MCP tools are filtered out unless \
                                     their server is marked safe_mode = \"read_only\" or \"allow\" \
                                     in the configuration.]";

/// Workspace mode message shown to the assistant when switching out of
/// safe mode.
pub const WORKSPACE_MODE_MESSAGE: &str = "[SYSTEM: Workspace mode enabled. \
                                          File edits and shell commands are allowed. \
                                          On supported systems, shell commands run confined \
                                          to the project: writes cannot leave the workspace \
                                          and there is no network access. All tools are available.]";

/// How much access the assistant has to the workspace and the shell.
///
/// Chosen at startup from the command line (`--safe-mode`,
/// `--unrestricted`, or neither) and switchable during a session with
/// the `/safe` and `/workspace` commands.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SandboxMode {
    /// Only read-only tools are offered. No file writes and no shell
    /// commands.
    ReadOnly,
    /// Read and write within the workspace, with shell commands confined
    /// to the workspace directory by the operating system. This is the
    /// default when neither switch is given.
    Workspace,
    /// Full access: shell commands run without operating-system
    /// confinement. Intended for trusted environments only.
    Full,
}

impl SandboxMode {
    /// Resolve the mode from the two command-line switches. `--safe-mode`
    /// wins over `--unrestricted` when both are given, so the most
    /// restrictive choice always takes effect.
    pub fn from_flags(safe: bool, unrestricted: bool) -> Self {
        if safe {
            Self::ReadOnly
        } else if unrestricted {
            Self::Full
        } else {
            Self::Workspace
        }
    }

    /// Whether only read-only tools are offered. Drives tool selection
    /// and the read-only banner.
    pub fn is_read_only(self) -> bool {
        matches!(self, Self::ReadOnly)
    }

    /// Whether shell commands should be confined to the workspace by the
    /// operating-system sandbox. True only for [`SandboxMode::Workspace`].
    pub fn is_sandboxed(self) -> bool {
        matches!(self, Self::Workspace)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sandbox_mode_from_flags_prefers_safe_then_unrestricted() {
        assert_eq!(
            SandboxMode::from_flags(false, false),
            SandboxMode::Workspace
        );
        assert_eq!(SandboxMode::from_flags(false, true), SandboxMode::Full);
        assert_eq!(SandboxMode::from_flags(true, false), SandboxMode::ReadOnly);
        assert_eq!(SandboxMode::from_flags(true, true), SandboxMode::ReadOnly);
    }

    #[test]
    fn test_default_config_matches_fallback_model_info() {
        let config = SofosConfig::default();
        let info = crate::api::Model::default();
        assert_eq!(config.max_messages, 500);
        assert_eq!(config.max_context_tokens, info.effective_window() as usize);
        assert_eq!(config.max_tool_iterations, 200);
        assert_eq!(
            config.auto_compact_token_limit,
            info.auto_compact_at() as usize
        );
        assert_eq!(config.compaction_preserve_recent, 20);
        assert_eq!(config.tool_result_truncate_threshold, 2000);
    }
}
