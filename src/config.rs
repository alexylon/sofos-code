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

/// Read-only mode preamble shown to the assistant. Must stay in sync
/// with the tool set returned by `tools::get_read_only_tools()` (+ the
/// optional `search_code` tool wired in when ripgrep is on PATH).
pub fn readonly_mode_message() -> String {
    "[SYSTEM: Read-only mode is active.\n\
     \n\
     Shell commands: not available in this mode.\n\
     File edits: blocked (write_file, edit_file, delete_file, create_directory, \
     move_file, copy_file all unavailable).\n\
     External paths: not reachable.\n\
     \n\
     Available native tools: list_directory, read_file, glob_files, search_code \
     (when ripgrep is installed), update_plan, web_fetch, web_search. MCP tools \
     are filtered out unless their server is marked readonly = \"read_only\" or \
     \"allow\" in the configuration.\n\
     \n\
     Switch with /workspace (default) or /unrestricted.]"
        .to_string()
}

/// Workspace mode preamble shown to the assistant. Names the three
/// command tiers, the structural rules that stay enforced, and the
/// per-operating-system caveats around network closure.
#[cfg(any(target_os = "macos", target_os = "linux"))]
pub fn workspace_mode_message() -> String {
    String::from(
        "[SYSTEM: Workspace mode is active (the default).\n\
         \n\
         Shell commands run through a three-tier model:\n\
         - Familiar commands (cargo, npm, go, ls, cat, grep, rg, git status, git log, \
         git diff, ...) run automatically.\n\
         - Destructive commands (rm, rmdir, chmod, chown, sudo, dd, mkfs, systemctl, \
         kill, destructive git operations) are always refused.\n\
         - Any other command runs confined by the operating-system sandbox without a \
         prompt: writes are limited to the workspace and the temporary directories. \
         File redirection (echo hi > file) and here-documents also run confined, so \
         they succeed when targeting paths inside the workspace.\n\
         \n\
         The project's .git, .sofos, .agents, .claude, and .codex stay read-only even \
         for confined commands; edit them with the file tools, not the shell.\n\
         \n\
         Always refused, even confined: parent traversal (..), hidden subcommands \
         ($(...), backticks, <(...), >(...)), and dangerous git operations.\n\
         \n\
         Network: closed for confined commands.\n\
         \n\
         Switch with /readonly or /unrestricted. All tools are available.]",
    )
}

/// Workspace mode preamble on Windows. The restricted-token backend is
/// not engaged on this platform (the default Git for Windows `sh.exe`
/// cannot start under it), so workspace mode currently behaves like
/// unrestricted mode for shell commands. The message tells the
/// assistant the truth so it does not assume confinement is in effect.
#[cfg(target_os = "windows")]
pub fn workspace_mode_message() -> String {
    "[SYSTEM: Workspace mode is active (the default).\n\
     \n\
     Shell commands run through a three-tier model:\n\
     - Familiar commands (cargo, npm, go, ls, cat, grep, rg, git status, git log, \
     git diff, ...) run automatically.\n\
     - Destructive commands (rm, rmdir, chmod, chown, sudo, dd, mkfs, systemctl, \
     kill, destructive git operations) are always refused.\n\
     - Any other command prompts the user for approval before running.\n\
     \n\
     Always refused: parent traversal (..), hidden subcommands ($(...), backticks, \
     <(...), >(...)), file redirection (>, >>) and here-documents (use write_file or \
     edit_file instead; 2>&1 is allowed), and dangerous git operations.\n\
     \n\
     Operating-system confinement is NOT engaged on Windows in this release: the \
     default shell cannot start under the restricted access token. Workspace mode \
     therefore behaves the same as unrestricted mode on this platform; the network \
     is not closed for shell commands. The destructive-command blocklist, the \
     read-deny rules, and the external-path prompts still apply.\n\
     \n\
     Switch with /readonly or /unrestricted. All tools are available.]"
        .to_string()
}

/// Workspace mode preamble on platforms without a sandbox backend.
/// Treated like the Windows path until a backend is added.
#[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
pub fn workspace_mode_message() -> String {
    "[SYSTEM: Workspace mode is active (the default).\n\
     \n\
     Shell commands run through a three-tier model:\n\
     - Familiar commands run automatically.\n\
     - Destructive commands are always refused.\n\
     - Any other command prompts the user for approval before running.\n\
     \n\
     Operating-system confinement is not available on this platform. Workspace mode \
     therefore behaves the same as unrestricted mode for shell commands.\n\
     \n\
     Switch with /readonly or /unrestricted. All tools are available.]"
        .to_string()
}

/// Unrestricted mode preamble shown to the assistant. Names the same
/// three-tier model and the structural rules, and points out that no
/// operating-system confinement is applied.
pub fn unrestricted_mode_message() -> String {
    "[SYSTEM: Unrestricted mode is active.\n\
     \n\
     Shell commands run through the same three-tier model as workspace mode, but \
     without operating-system confinement:\n\
     - Familiar commands run automatically.\n\
     - Destructive commands (rm, rmdir, chmod, chown, sudo, dd, mkfs, systemctl, \
     kill, destructive git operations) are always refused.\n\
     - Any other command prompts the user for approval before running.\n\
     \n\
     Structural rules still apply: parent traversal (..), hidden subcommands \
     ($(...), backticks, <(...), >(...)), file redirection (>, >>) and here-documents \
     (use write_file or edit_file instead; 2>&1 is allowed), and dangerous git \
     operations are refused outright.\n\
     \n\
     No operating-system confinement is applied; intended for trusted environments only.\n\
     \n\
     Switch with /readonly or /workspace. All tools are available.]"
        .to_string()
}

/// How much access the assistant has to the workspace and the shell.
///
/// Chosen at startup from the command line (`--readonly`,
/// `--unrestricted`, or neither) and switchable during a session with
/// the `/readonly` and `/workspace` commands.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SandboxMode {
    /// Only read-only tools are offered. No file writes and no shell
    /// commands.
    ReadOnly,
    /// Read and write within the workspace, with shell commands confined
    /// to the workspace directory by the operating system. This is the
    /// default when neither switch is given.
    Workspace,
    /// Unrestricted: shell commands run without operating-system
    /// confinement. Intended for trusted environments only.
    Unrestricted,
}

impl SandboxMode {
    /// Resolve the mode from the two command-line switches. `--readonly`
    /// wins over `--unrestricted` when both are given, so the most
    /// restrictive choice always takes effect.
    pub fn from_flags(readonly: bool, unrestricted: bool) -> Self {
        if readonly {
            Self::ReadOnly
        } else if unrestricted {
            Self::Unrestricted
        } else {
            Self::Workspace
        }
    }

    /// Whether only read-only tools are offered. Drives tool selection
    /// and the read-only banner.
    pub fn is_readonly(self) -> bool {
        matches!(self, Self::ReadOnly)
    }

    /// Whether shell commands should be confined to the workspace by the
    /// operating-system sandbox. True only for [`SandboxMode::Workspace`].
    pub fn is_sandboxed(self) -> bool {
        matches!(self, Self::Workspace)
    }

    /// Short label shown in the status line. On Windows the workspace
    /// label is suffixed with "(no sandbox)" because operating-system
    /// confinement is not engaged on that platform in this release.
    pub fn label(self) -> &'static str {
        match self {
            Self::ReadOnly => "readonly",
            Self::Workspace => {
                #[cfg(target_os = "windows")]
                {
                    "workspace (no sandbox)"
                }
                #[cfg(not(target_os = "windows"))]
                {
                    "workspace"
                }
            }
            Self::Unrestricted => "unrestricted",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sandbox_mode_from_flags_prefers_readonly_then_unrestricted() {
        assert_eq!(
            SandboxMode::from_flags(false, false),
            SandboxMode::Workspace
        );
        assert_eq!(
            SandboxMode::from_flags(false, true),
            SandboxMode::Unrestricted
        );
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
