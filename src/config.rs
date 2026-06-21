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

/// Opening marker of the injected `[SYSTEM: ...]` preambles below. They
/// ride in as `user` messages, so the session preview skips them to title
/// by the first real user message.
pub const SYSTEM_MESSAGE_PREFIX: &str = "[SYSTEM:";

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
     Switch access modes with /permissions.]"
        .to_string()
}

/// Sandbox-on preamble shown to the assistant. Names the three command
/// tiers, the structural rules that stay enforced, the per-operating-system
/// caveats around network closure, and — keyed off `policy` — the escalation
/// path the active sandboxed preset actually offers, so the three
/// `sandboxed-*` presets each describe their own behaviour.
#[cfg(any(target_os = "macos", target_os = "linux"))]
pub fn sandbox_on_message(policy: ApprovalPolicy) -> String {
    let preset = PermissionPreset::current(SandboxMode::Sandboxed, policy);
    let preset_intro = match policy {
        ApprovalPolicy::OnRequest => format!("{} preset, the default access mode", preset.label()),
        _ => format!("{} preset", preset.label()),
    };
    // The action a confined failure should trigger differs by preset:
    // sandboxed-ask honours an up-front require_escalated, sandboxed-retry
    // offers an unsandboxed retry instead (and refuses require_escalated), and
    // sandboxed-strict never lifts the sandbox at all.
    let escalation = match policy {
        ApprovalPolicy::OnRequest => {
            "Explain this likely cause to the user, try an alternative \
            that works under the sandbox when possible, and otherwise rerun the command with \
            sandbox_permissions set to \"require_escalated\" so the user can approve running that \
            one command outside the sandbox; suggest an unsandboxed preset (/permissions) only if \
            many commands need it."
        }
        ApprovalPolicy::OnFailure => {
            "Explain this likely cause to the user and try an alternative \
            that works under the sandbox when possible. Do not set sandbox_permissions to \
            \"require_escalated\" in this preset — up-front escalation requests are refused; \
            instead run the command normally, and when a confined command looks sandbox-blocked \
            the user is offered an unsandboxed retry of it. Suggest an unsandboxed preset \
            (/permissions) only if many commands need it."
        }
        ApprovalPolicy::Never => {
            "Explain this likely cause to the user and try an alternative \
            that works under the sandbox when possible. This preset never lifts the sandbox: \
            sandbox_permissions \"require_escalated\" requests are refused and failed commands are \
            not offered an unsandboxed retry, so a command that genuinely needs the network or to \
            write outside the workspace cannot run here — tell the user and suggest switching to \
            an unsandboxed preset with /permissions."
        }
    };
    format!(
        "[SYSTEM: The sandbox is on ({preset_intro}).\n\
         \n\
         Shell commands run through a three-tier model:\n\
         - Familiar commands (cargo, npm, go, ls, cat, grep, rg, git status, git log, \
         git diff, ...) run automatically.\n\
         - Destructive commands (rm, rmdir, chmod, chown, sudo, dd, mkfs, systemctl, \
         kill, destructive git operations) are always refused.\n\
         - Any other command runs without a prompt.\n\
         \n\
         Every command that runs, familiar or not, is confined by the operating-system \
         sandbox: writes are limited to the workspace and the temporary directories, and \
         the network is closed. This includes build and network tools such as cargo, npm, \
         and pip — with the sandbox on they cannot fetch over the network or write outside \
         the workspace. File redirection (echo hi > file) and here-documents also run \
         confined, so they succeed when targeting paths inside the workspace.\n\
         \n\
         The project's .sofos, .agents, .claude, and .codex stay read-only for confined \
         commands; edit them with the file tools, not the shell. The .git directory is \
         read-only too, except for a command that runs only git, so git checkout and local \
         git config still work.\n\
         \n\
         Always refused, even confined: parent traversal (..), hidden subcommands \
         ($(...), backticks, <(...), >(...)), and dangerous git operations.\n\
         \n\
         Confined-command failures: if a command fails with permission, network, socket, \
         mount, or container engine errors, assume the operating-system sandbox may be the \
         cause. Common examples include Docker and other container runtimes, tools that \
         need network access, local daemon sockets, or writes outside the workspace and \
         temporary directories. {escalation}\n\
         \n\
         Switch access modes with /permissions. All tools are available.]"
    )
}

/// Sandbox-on preamble on Windows, where the restricted-token backend is
/// not engaged (the default Git for Windows `sh.exe` cannot start under
/// it), so shell commands run unconfined even with the sandbox on. The
/// message tells the assistant the truth so it does not assume confinement
/// is in effect. The escalation policy is moot here because no confinement
/// is engaged, so the policy argument is accepted only to match the signature.
#[cfg(target_os = "windows")]
pub fn sandbox_on_message(_policy: ApprovalPolicy) -> String {
    "[SYSTEM: The sandbox is on.\n\
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
     default shell cannot start under the restricted access token, so shell commands \
     run unconfined on this platform even with the sandbox on; the network is not \
     closed for shell commands. The destructive-command blocklist, the read-deny \
     rules, and the external-path prompts still apply.\n\
     \n\
     Switch access modes with /permissions. All tools are available.]"
        .to_string()
}

/// Sandbox-on preamble on platforms without a sandbox backend.
/// Treated like the Windows path until a backend is added. The escalation
/// policy is moot without confinement, so the argument is accepted only to
/// match the signature.
#[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
pub fn sandbox_on_message(_policy: ApprovalPolicy) -> String {
    "[SYSTEM: The sandbox is on.\n\
     \n\
     Shell commands run through a three-tier model:\n\
     - Familiar commands run automatically.\n\
     - Destructive commands are always refused.\n\
     - Any other command prompts the user for approval before running.\n\
     \n\
     Operating-system confinement is not available on this platform, so shell \
     commands run unconfined even with the sandbox on.\n\
     \n\
     Switch access modes with /permissions. All tools are available.]"
        .to_string()
}

/// Sandbox-off preamble shown to the assistant. Names the same three-tier
/// model and the structural rules, and points out that no operating-system
/// confinement is applied.
pub fn sandbox_off_message() -> String {
    "[SYSTEM: The sandbox is off.\n\
     \n\
     Shell commands run through the same three-tier model as with the sandbox on, but \
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
     Switch access modes with /permissions. All tools are available.]"
        .to_string()
}

/// How much access the assistant has to the workspace and the shell.
///
/// Chosen at startup from the command line (`--readonly`,
/// `--no-sandbox`, or neither) and switchable during a session through the
/// `/permissions` presets.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SandboxMode {
    /// Only read-only tools are offered. No file writes and no shell
    /// commands.
    ReadOnly,
    /// Read and write, with shell commands confined to the project
    /// directory by the operating-system sandbox. The default when a
    /// sandbox is available and neither switch is given.
    Sandboxed,
    /// Read and write, with shell commands run without operating-system
    /// confinement. Intended for trusted environments only.
    Unsandboxed,
}

impl SandboxMode {
    /// Resolve the startup mode. `--readonly` wins over `--no-sandbox` when
    /// both are given. With neither flag the default is sandbox on, except
    /// where no sandbox can run (`sandbox_available` is false — Windows, or
    /// a Linux host without Bubblewrap), where the default is sandbox off so
    /// the mode does not claim a confinement that cannot take effect.
    pub fn from_flags(readonly: bool, no_sandbox: bool, sandbox_available: bool) -> Self {
        if readonly {
            Self::ReadOnly
        } else if no_sandbox || !sandbox_available {
            Self::Unsandboxed
        } else {
            Self::Sandboxed
        }
    }

    /// Whether only read-only tools are offered. Drives tool selection
    /// and the read-only banner.
    pub fn is_readonly(self) -> bool {
        matches!(self, Self::ReadOnly)
    }

    /// Whether shell commands should be confined to the workspace by the
    /// operating-system sandbox. True only for [`SandboxMode::Sandboxed`].
    pub fn is_sandboxed(self) -> bool {
        matches!(self, Self::Sandboxed)
    }

    /// Whether `self` restricts less than `other`. Read-only is the most
    /// restrictive, then sandboxed, then unsandboxed. Used so a resume that
    /// lands on a less restrictive mode than the one in effect can be
    /// surfaced rather than applied silently.
    pub fn is_more_permissive_than(self, other: SandboxMode) -> bool {
        self.restriction_rank() < other.restriction_rank()
    }

    /// Relative restrictiveness, higher meaning more locked down.
    fn restriction_rank(self) -> u8 {
        match self {
            Self::ReadOnly => 2,
            Self::Sandboxed => 1,
            Self::Unsandboxed => 0,
        }
    }
}

/// When the user is asked before a command runs outside the
/// operating-system sandbox. Independent of [`SandboxMode`]: the mode
/// decides what the sandbox confines, this decides when an escalation out
/// of that confinement is offered.
///
/// Two escalation paths are gated by different policies, so at most one is
/// active at a time:
/// - The model can mark a command as needing to run unsandboxed
///   (`require_escalated`); honored only under [`ApprovalPolicy::OnRequest`].
/// - A confined command that fails in a way that looks sandbox-caused is
///   offered for an unsandboxed retry; only under
///   [`ApprovalPolicy::OnFailure`].
///
/// Selected through the `/permissions` presets: the three `sandboxed-*`
/// choices fold this policy into the access mode, so there is no separate
/// approval surface to keep in sync.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ApprovalPolicy {
    /// No up-front model escalation requests, but a confined command that
    /// looks blocked by the sandbox is offered for an unsandboxed retry.
    OnFailure,
    /// Default. The model may request that a command run outside the
    /// sandbox; the user is asked before it runs. Failures are returned to
    /// the model without a retry prompt.
    #[default]
    OnRequest,
    /// Never offer to leave the sandbox: failures are returned as-is and
    /// model escalation requests are refused.
    Never,
}

impl ApprovalPolicy {
    /// Short label used in the escalation-rejection message the model sees
    /// when it requests a sandbox lift under a policy that forbids it.
    pub fn label(self) -> &'static str {
        match self {
            Self::OnFailure => "on-failure",
            Self::OnRequest => "on-request",
            Self::Never => "never",
        }
    }

    /// Whether a confined command that looks blocked by the sandbox should
    /// prompt the user to retry it unsandboxed (the reactive escalation
    /// path). True only for [`ApprovalPolicy::OnFailure`].
    pub fn wants_no_sandbox_approval(self) -> bool {
        matches!(self, Self::OnFailure)
    }

    /// Whether the model may ask for a command to run outside the sandbox
    /// up front (the proactive escalation path). True only for
    /// [`ApprovalPolicy::OnRequest`].
    pub fn allows_model_escalation_request(self) -> bool {
        matches!(self, Self::OnRequest)
    }
}

/// A `/permissions` preset: a [`SandboxMode`] paired with its escalation
/// policy, offered to the user as one choice. The single source of truth
/// for the picker rows, the `/permissions <label>` parser, and the status
/// label.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PermissionPreset {
    /// Inspection tools only; no edits or shell commands.
    ReadOnly,
    /// Sandboxed with [`ApprovalPolicy::OnRequest`] — the default.
    SandboxedAsk,
    /// Sandboxed with [`ApprovalPolicy::OnFailure`].
    SandboxedRetry,
    /// Sandboxed with [`ApprovalPolicy::Never`].
    SandboxedStrict,
    /// Edits and shell commands with no operating-system confinement.
    Unsandboxed,
}

/// Every preset in picker-display order, least to most permissive. The
/// picker, the parser, and the status line all read from this array.
pub const PERMISSION_PRESETS: [PermissionPreset; 5] = [
    PermissionPreset::ReadOnly,
    PermissionPreset::SandboxedAsk,
    PermissionPreset::SandboxedRetry,
    PermissionPreset::SandboxedStrict,
    PermissionPreset::Unsandboxed,
];

impl PermissionPreset {
    /// User-facing name, typed as `/permissions <label>` and shown in the
    /// picker and status line.
    pub fn label(self) -> &'static str {
        match self {
            Self::ReadOnly => "read-only",
            Self::SandboxedAsk => "sandboxed-ask",
            Self::SandboxedRetry => "sandboxed-retry",
            Self::SandboxedStrict => "sandboxed-strict",
            Self::Unsandboxed => "unsandboxed",
        }
    }

    /// One-line picker description. "Project-limited" is shorthand for
    /// "writes and the network are confined to the project; reads stay
    /// open"; "sandbox lift" means running a single command unconfined.
    pub fn description(self) -> &'static str {
        match self {
            Self::ReadOnly => "Inspect only; no edits or shell commands.",
            Self::SandboxedAsk => "Project-limited edits and commands; may request a sandbox lift.",
            Self::SandboxedRetry => {
                "Project-limited edits and commands; offers an unsandboxed retry if blocked."
            }
            Self::SandboxedStrict => "Project-limited edits and commands; never lifts the sandbox.",
            Self::Unsandboxed => "Edit and run commands without sandbox limits.",
        }
    }

    /// The access mode this preset selects.
    pub fn mode(self) -> SandboxMode {
        match self {
            Self::ReadOnly => SandboxMode::ReadOnly,
            Self::SandboxedAsk | Self::SandboxedRetry | Self::SandboxedStrict => {
                SandboxMode::Sandboxed
            }
            Self::Unsandboxed => SandboxMode::Unsandboxed,
        }
    }

    /// Whether this preset can be selected given whether a working OS
    /// sandbox exists. The sandboxed presets are unavailable where none can
    /// run (Windows, or a Linux host without Bubblewrap); the others always
    /// apply.
    pub fn is_available(self, sandbox_available: bool) -> bool {
        self.mode() != SandboxMode::Sandboxed || sandbox_available
    }

    /// Escalation policy to apply, or `None` where escalation is moot —
    /// read-only runs nothing and unsandboxed has no sandbox to lift.
    pub fn escalation(self) -> Option<ApprovalPolicy> {
        match self {
            Self::ReadOnly | Self::Unsandboxed => None,
            Self::SandboxedAsk => Some(ApprovalPolicy::OnRequest),
            Self::SandboxedRetry => Some(ApprovalPolicy::OnFailure),
            Self::SandboxedStrict => Some(ApprovalPolicy::Never),
        }
    }

    /// Resolve from a `/permissions <label>` argument, lenient on case and
    /// underscores-for-hyphens.
    pub fn parse(s: &str) -> Option<Self> {
        let normalized = s.trim().to_ascii_lowercase().replace('_', "-");
        PERMISSION_PRESETS
            .into_iter()
            .find(|preset| preset.label() == normalized)
    }

    /// The preset matching the live state. Read-only and unsandboxed are
    /// decided by the mode alone; the sandboxed presets are distinguished
    /// by the active escalation policy.
    pub fn current(mode: SandboxMode, policy: ApprovalPolicy) -> Self {
        match mode {
            SandboxMode::ReadOnly => Self::ReadOnly,
            SandboxMode::Unsandboxed => Self::Unsandboxed,
            SandboxMode::Sandboxed => match policy {
                ApprovalPolicy::OnRequest => Self::SandboxedAsk,
                ApprovalPolicy::OnFailure => Self::SandboxedRetry,
                ApprovalPolicy::Never => Self::SandboxedStrict,
            },
        }
    }

    /// Whether resuming into `self` loosens access compared with `other`. The
    /// access mode is compared first (read-only is tightest, then sandboxed,
    /// then unsandboxed); when both are sandboxed, the escalation policy
    /// decides, so `sandboxed-ask` over `sandboxed-strict` is a loosening even
    /// though the mode is unchanged. Lets a resume that relaxes either one be
    /// surfaced instead of applied silently.
    pub fn is_more_permissive_than(self, other: Self) -> bool {
        if self.mode() != other.mode() {
            return self.mode().is_more_permissive_than(other.mode());
        }
        self.escalation_rank() < other.escalation_rank()
    }

    /// Escalation restrictiveness, higher meaning more locked down:
    /// `sandboxed-strict` never lifts the sandbox, so it outranks the two
    /// presets that can lift it. Those two rank equal — ask lifts proactively
    /// and retry only after a sandbox-looking failure, but both lift only with
    /// the user's approval, so switching between them neither loosens nor
    /// tightens access. Read-only and unsandboxed reach this only when
    /// compared with themselves.
    fn escalation_rank(self) -> u8 {
        match self.escalation() {
            Some(ApprovalPolicy::Never) => 1,
            _ => 0,
        }
    }
}

/// Workspace-local config file, relative to the workspace root. Holds
/// project-specific permission rules and MCP servers, and overrides the
/// global config on conflict.
pub(crate) const LOCAL_CONFIG_FILE: &str = ".sofos/config.local.toml";

/// Global config file, relative to the user's home directory. Holds
/// defaults shared across every workspace.
pub(crate) const GLOBAL_CONFIG_FILE: &str = ".sofos/config.toml";

/// The user's home directory: `HOME` on Unix, `USERPROFILE` on Windows.
/// Returns `None` when the variable is unset. Reading the platform
/// variable directly — rather than `std::env::home_dir`, which was only
/// re-stabilised with a correct Windows implementation in Rust 1.85 —
/// keeps older toolchains working and makes the per-platform choice
/// explicit.
pub(crate) fn home_dir() -> Option<std::path::PathBuf> {
    #[cfg(windows)]
    {
        std::env::var_os("USERPROFILE").map(std::path::PathBuf::from)
    }
    #[cfg(not(windows))]
    {
        std::env::var_os("HOME").map(std::path::PathBuf::from)
    }
}

/// Absolute path to the global config file, or `None` when the home
/// directory is unknown. Both the permission system and the MCP loader
/// resolve the global config through this so the two never diverge.
pub(crate) fn global_config_path() -> Option<std::path::PathBuf> {
    home_dir().map(|home| home.join(GLOBAL_CONFIG_FILE))
}

/// The two config file locations as written in user-facing hint messages:
/// `.sofos/config.local.toml or ~/.sofos/config.toml`. The global file is
/// shown with the `~/` shorthand the user types.
pub(crate) fn config_files_hint() -> String {
    format!("{} or ~/{}", LOCAL_CONFIG_FILE, GLOBAL_CONFIG_FILE)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn is_more_permissive_than_orders_modes_by_restrictiveness() {
        use SandboxMode::*;
        // Unsandboxed is the least restrictive, read-only the most.
        assert!(Unsandboxed.is_more_permissive_than(Sandboxed));
        assert!(Unsandboxed.is_more_permissive_than(ReadOnly));
        assert!(Sandboxed.is_more_permissive_than(ReadOnly));
        // Tightening or keeping the mode is not "more permissive".
        assert!(!ReadOnly.is_more_permissive_than(Unsandboxed));
        assert!(!Sandboxed.is_more_permissive_than(Unsandboxed));
        assert!(!Sandboxed.is_more_permissive_than(Sandboxed));
    }

    #[test]
    fn preset_is_more_permissive_than_covers_both_axes() {
        use PermissionPreset::*;
        // Access mode: read-only is tightest, unsandboxed is loosest.
        assert!(Unsandboxed.is_more_permissive_than(SandboxedAsk));
        assert!(SandboxedAsk.is_more_permissive_than(ReadOnly));
        assert!(!ReadOnly.is_more_permissive_than(Unsandboxed));

        // Escalation policy within sandboxed mode: strict never lifts the
        // sandbox, so resuming ask or retry over it loosens access.
        assert!(SandboxedAsk.is_more_permissive_than(SandboxedStrict));
        assert!(SandboxedRetry.is_more_permissive_than(SandboxedStrict));
        // Tightening onto strict, switching between ask and retry, or the
        // same preset is not a loosening.
        assert!(!SandboxedStrict.is_more_permissive_than(SandboxedAsk));
        assert!(!SandboxedAsk.is_more_permissive_than(SandboxedRetry));
        assert!(!SandboxedRetry.is_more_permissive_than(SandboxedAsk));
        assert!(!SandboxedAsk.is_more_permissive_than(SandboxedAsk));
    }

    #[test]
    fn sandbox_mode_from_flags_prefers_readonly_then_no_sandbox() {
        // With a sandbox available, no flags defaults to sandbox on.
        assert_eq!(
            SandboxMode::from_flags(false, false, true),
            SandboxMode::Sandboxed
        );
        // --no-sandbox forces sandbox off even where one is available.
        assert_eq!(
            SandboxMode::from_flags(false, true, true),
            SandboxMode::Unsandboxed
        );
        // No usable sandbox (Windows, or Linux without Bubblewrap) defaults
        // to off rather than a confinement that cannot run.
        assert_eq!(
            SandboxMode::from_flags(false, false, false),
            SandboxMode::Unsandboxed
        );
        // --readonly always wins.
        assert_eq!(
            SandboxMode::from_flags(true, false, true),
            SandboxMode::ReadOnly
        );
        assert_eq!(
            SandboxMode::from_flags(true, true, false),
            SandboxMode::ReadOnly
        );
    }

    #[cfg(any(target_os = "macos", target_os = "linux"))]
    #[test]
    fn sandbox_on_message_explains_confined_command_failures() {
        let message = sandbox_on_message(ApprovalPolicy::OnRequest);
        assert!(message.contains("Confined-command failures"));
        assert!(message.contains("Docker"));
        assert!(message.contains("require_escalated"));
        assert!(message.contains("/permissions"));
    }

    /// Each sandboxed preset must describe its own escalation path: ask
    /// points at require_escalated, retry points at the unsandboxed retry and
    /// refuses require_escalated, and strict says the sandbox never lifts.
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    #[test]
    fn sandbox_on_message_describes_the_active_escalation_policy() {
        let ask = sandbox_on_message(ApprovalPolicy::OnRequest);
        assert!(ask.contains(PermissionPreset::SandboxedAsk.label()));
        assert!(ask.contains("require_escalated"));

        let retry = sandbox_on_message(ApprovalPolicy::OnFailure);
        assert!(retry.contains(PermissionPreset::SandboxedRetry.label()));
        assert!(retry.contains("unsandboxed retry"));
        assert!(retry.contains("Do not set sandbox_permissions"));

        let strict = sandbox_on_message(ApprovalPolicy::Never);
        assert!(strict.contains(PermissionPreset::SandboxedStrict.label()));
        assert!(strict.contains("never lifts the sandbox"));
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

    #[test]
    fn approval_policy_default_is_on_request() {
        assert_eq!(ApprovalPolicy::default(), ApprovalPolicy::OnRequest);
    }

    #[test]
    fn approval_policy_gates_each_escalation_path() {
        // Reactive retry-on-failure path.
        assert!(ApprovalPolicy::OnFailure.wants_no_sandbox_approval());
        assert!(!ApprovalPolicy::OnRequest.wants_no_sandbox_approval());
        assert!(!ApprovalPolicy::Never.wants_no_sandbox_approval());
        // Proactive model-requested path.
        assert!(ApprovalPolicy::OnRequest.allows_model_escalation_request());
        assert!(!ApprovalPolicy::OnFailure.allows_model_escalation_request());
        assert!(!ApprovalPolicy::Never.allows_model_escalation_request());
    }

    #[test]
    fn permission_preset_parse_round_trips_every_label() {
        for preset in PERMISSION_PRESETS {
            assert_eq!(PermissionPreset::parse(preset.label()), Some(preset));
        }
        // Lenient on case and underscores.
        assert_eq!(
            PermissionPreset::parse("Sandboxed_Ask"),
            Some(PermissionPreset::SandboxedAsk)
        );
        assert_eq!(PermissionPreset::parse("bogus"), None);
    }

    #[test]
    fn permission_preset_current_matches_mode_and_policy() {
        // Read-only and unsandboxed are decided by the mode alone.
        assert_eq!(
            PermissionPreset::current(SandboxMode::ReadOnly, ApprovalPolicy::OnRequest),
            PermissionPreset::ReadOnly
        );
        assert_eq!(
            PermissionPreset::current(SandboxMode::Unsandboxed, ApprovalPolicy::Never),
            PermissionPreset::Unsandboxed
        );
        // The sandboxed presets are distinguished by the escalation policy.
        assert_eq!(
            PermissionPreset::current(SandboxMode::Sandboxed, ApprovalPolicy::OnRequest),
            PermissionPreset::SandboxedAsk
        );
        assert_eq!(
            PermissionPreset::current(SandboxMode::Sandboxed, ApprovalPolicy::OnFailure),
            PermissionPreset::SandboxedRetry
        );
        assert_eq!(
            PermissionPreset::current(SandboxMode::Sandboxed, ApprovalPolicy::Never),
            PermissionPreset::SandboxedStrict
        );
    }

    #[test]
    fn permission_preset_availability_tracks_the_sandbox() {
        // With a sandbox, every preset is selectable.
        for preset in PERMISSION_PRESETS {
            assert!(preset.is_available(true));
        }
        // Without one, only the sandboxed presets become unavailable.
        for preset in PERMISSION_PRESETS {
            let expected = preset.mode() != SandboxMode::Sandboxed;
            assert_eq!(preset.is_available(false), expected, "{:?}", preset);
        }
    }

    #[test]
    fn permission_preset_mode_and_escalation_agree_with_current() {
        // Each preset's (mode, escalation) round-trips back to itself.
        for preset in PERMISSION_PRESETS {
            let policy = preset.escalation().unwrap_or_default();
            assert_eq!(PermissionPreset::current(preset.mode(), policy), preset);
        }
    }
}
