use crate::error::Result;
use crate::repl::Repl;

pub mod builtin;

/// Result of command execution
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CommandResult {
    /// Continue REPL loop
    Continue,
    /// Exit REPL loop
    Exit,
}

/// Enum representing all available REPL commands
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Command {
    Exit,
    Clear,
    Resume,
    /// `/effort` — open the reasoning-effort picker.
    EffortPicker,
    /// `/effort <level>` — set the level directly. Per-model
    /// validation matches `--reasoning-effort`.
    EffortSet(crate::api::ReasoningEffort),
    ReadOnlyMode,
    WorkspaceMode,
    UnrestrictedMode,
    /// `/approval` — open the approval-policy picker.
    ApprovalPicker,
    /// `/approval <policy>` — set when the user is asked before a command
    /// runs outside the sandbox (on-failure/on-request/never).
    ApprovalSet(crate::config::ApprovalPolicy),
    Compact,
    /// `/model` with no argument — open the model picker.
    ModelPicker,
    /// `/model <name>` — switch directly to a named model without
    /// going through the picker. Validation happens in the command
    /// handler so the rejection message matches the one the CLI
    /// surfaces for `--model`.
    ModelSet(String),
}

/// Slash-command names, defined once so the parser and the catalog (and
/// any future rename) share a single source of truth.
const CMD_EXIT: &str = "/exit";
const CMD_QUIT: &str = "/quit";
const CMD_QUIT_SHORT: &str = "/q";
const CMD_CLEAR: &str = "/clear";
const CMD_RESUME: &str = "/resume";
const CMD_EFFORT: &str = "/effort";
const CMD_MODEL: &str = "/model";
const CMD_COMPACT: &str = "/compact";
const CMD_READONLY: &str = "/readonly";
const CMD_WORKSPACE: &str = "/workspace";
const CMD_UNRESTRICTED: &str = "/unrestricted";
const CMD_APPROVAL: &str = "/approval";

impl Command {
    pub fn from_str(s: &str) -> Option<Self> {
        let lower = s.to_lowercase();
        match lower.as_str() {
            CMD_EXIT | CMD_QUIT | CMD_QUIT_SHORT => Some(Command::Exit),
            CMD_CLEAR => Some(Command::Clear),
            CMD_RESUME => Some(Command::Resume),
            CMD_EFFORT => Some(Command::EffortPicker),
            CMD_READONLY => Some(Command::ReadOnlyMode),
            CMD_WORKSPACE => Some(Command::WorkspaceMode),
            CMD_UNRESTRICTED => Some(Command::UnrestrictedMode),
            CMD_APPROVAL => Some(Command::ApprovalPicker),
            CMD_COMPACT => Some(Command::Compact),
            CMD_MODEL => Some(Command::ModelPicker),
            _ => {
                if let Some(arg) = lower.strip_prefix("/effort ") {
                    let trimmed = arg.trim();
                    if trimmed.is_empty() {
                        Some(Command::EffortPicker)
                    } else {
                        crate::api::ReasoningEffort::parse(trimmed).map(Command::EffortSet)
                    }
                } else if let Some(arg) = lower.strip_prefix("/model ") {
                    let trimmed = arg.trim();
                    if trimmed.is_empty() {
                        Some(Command::ModelPicker)
                    } else {
                        Some(Command::ModelSet(trimmed.to_string()))
                    }
                } else if let Some(arg) = lower.strip_prefix("/approval ") {
                    let trimmed = arg.trim();
                    if trimmed.is_empty() {
                        Some(Command::ApprovalPicker)
                    } else {
                        crate::config::ApprovalPolicy::parse(trimmed).map(Command::ApprovalSet)
                    }
                } else {
                    None
                }
            }
        }
    }

    pub fn execute(&self, repl: &mut Repl) -> Result<CommandResult> {
        match self {
            Command::Exit => builtin::exit_command(repl),
            Command::Clear => builtin::clear_command(repl),
            Command::Resume => builtin::resume_command(repl),
            Command::EffortPicker => builtin::effort_picker_command(repl),
            Command::EffortSet(effort) => builtin::effort_set_command(repl, *effort),
            Command::ReadOnlyMode => builtin::readonly_mode_command(repl),
            Command::WorkspaceMode => builtin::workspace_mode_command(repl),
            Command::UnrestrictedMode => builtin::unrestricted_mode_command(repl),
            Command::ApprovalPicker => builtin::approval_picker_command(repl),
            Command::ApprovalSet(policy) => builtin::approval_set_command(repl, *policy),
            Command::Compact => builtin::compact_command(repl),
            Command::ModelPicker => builtin::model_picker_command(repl),
            Command::ModelSet(name) => builtin::model_set_command(repl, name),
        }
    }
}

/// Static catalog entry for one slash command. Used by both the popup
/// (for rendering names and descriptions) and by the unknown-command
/// error message (which lists every available name).
#[derive(Debug, Clone, Copy)]
pub struct CommandEntry {
    /// What the user types, including the leading `/`.
    pub name: &'static str,
    /// Short one-line description shown in the popup next to the name.
    pub description: &'static str,
}

/// Ordered list of every typeable command. Order here is the order shown
/// in the popup, so put the most useful entries first.
pub static COMMAND_CATALOG: &[CommandEntry] = &[
    CommandEntry {
        name: CMD_COMPACT,
        description: "summarize the conversation to free up context",
    },
    CommandEntry {
        name: CMD_CLEAR,
        description: "clear the conversation and start fresh",
    },
    CommandEntry {
        name: CMD_MODEL,
        description: "switch the active model (opens a picker)",
    },
    CommandEntry {
        name: CMD_EFFORT,
        description: "switch the reasoning effort (opens a picker)",
    },
    CommandEntry {
        name: CMD_RESUME,
        description: "resume a previously saved session",
    },
    CommandEntry {
        name: CMD_READONLY,
        description: "switch to read-only mode (no writes or shell)",
    },
    CommandEntry {
        name: CMD_WORKSPACE,
        description: "switch to workspace mode (read/write, shell confined to the project)",
    },
    CommandEntry {
        name: CMD_UNRESTRICTED,
        description: "switch to unrestricted mode (shell without sandbox confinement)",
    },
    CommandEntry {
        name: CMD_APPROVAL,
        description: "set when to run a command outside the sandbox",
    },
    CommandEntry {
        name: CMD_EXIT,
        description: "save the session and quit",
    },
    CommandEntry {
        name: CMD_QUIT,
        description: "alias of /exit",
    },
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bare_slash_model_opens_picker() {
        assert_eq!(Command::from_str("/model"), Some(Command::ModelPicker));
    }

    #[test]
    fn slash_model_with_trailing_space_opens_picker() {
        assert_eq!(Command::from_str("/model   "), Some(Command::ModelPicker));
    }

    #[test]
    fn slash_model_with_name_parses_to_model_set() {
        match Command::from_str(&format!("/model {}", crate::api::model_info::CLAUDE_OPUS)) {
            Some(Command::ModelSet(name)) => assert_eq!(name, crate::api::model_info::CLAUDE_OPUS),
            other => panic!("expected ModelSet, got {other:?}"),
        }
    }

    #[test]
    fn slash_model_lowercases_input_before_parsing() {
        // Matching the rest of `from_str`, the argument is lowercased
        // before reaching the handler. Validation against the
        // whitelist still happens in `handle_model_set` so the user
        // sees the same error there as on `--model`.
        match Command::from_str(&format!(
            "/model {}",
            crate::api::model_info::CLAUDE_OPUS.to_uppercase()
        )) {
            Some(Command::ModelSet(name)) => assert_eq!(name, crate::api::model_info::CLAUDE_OPUS),
            other => panic!("expected ModelSet, got {other:?}"),
        }
    }

    #[test]
    fn slash_model_accepts_unknown_arg_for_handler_to_reject() {
        // Validation lives in the executor so the user sees the
        // supported-list message there; `from_str` doesn't gate it.
        match Command::from_str("/model totally-made-up") {
            Some(Command::ModelSet(name)) => assert_eq!(name, "totally-made-up"),
            other => panic!("expected ModelSet, got {other:?}"),
        }
    }

    #[test]
    fn bare_slash_effort_opens_picker() {
        assert_eq!(Command::from_str("/effort"), Some(Command::EffortPicker));
    }

    #[test]
    fn slash_effort_with_trailing_space_opens_picker() {
        assert_eq!(Command::from_str("/effort  "), Some(Command::EffortPicker));
    }

    #[test]
    fn slash_effort_with_level_parses_to_effort_set() {
        match Command::from_str("/effort high") {
            Some(Command::EffortSet(e)) => assert_eq!(e, crate::api::ReasoningEffort::High),
            other => panic!("expected EffortSet, got {other:?}"),
        }
    }

    #[test]
    fn slash_effort_with_unknown_level_returns_none() {
        // Unlike `/model <name>`, the effort argument has a fixed
        // alphabet (off/low/medium/high/xhigh/max); anything else
        // can't be turned into a `ReasoningEffort` so we surface the
        // generic "unknown command" message instead of guessing.
        assert!(Command::from_str("/effort turbo").is_none());
    }

    #[test]
    fn bare_slash_approval_opens_picker() {
        assert_eq!(
            Command::from_str("/approval"),
            Some(Command::ApprovalPicker)
        );
        assert_eq!(
            Command::from_str("/approval   "),
            Some(Command::ApprovalPicker)
        );
    }

    #[test]
    fn slash_approval_with_policy_parses_to_set() {
        assert_eq!(
            Command::from_str("/approval on-failure"),
            Some(Command::ApprovalSet(
                crate::config::ApprovalPolicy::OnFailure
            ))
        );
        assert_eq!(
            Command::from_str("/approval never"),
            Some(Command::ApprovalSet(crate::config::ApprovalPolicy::Never))
        );
    }

    #[test]
    fn slash_approval_with_unknown_policy_returns_none() {
        // Like `/effort`, the argument has a fixed alphabet; anything
        // else surfaces the generic unknown-command message.
        assert!(Command::from_str("/approval turbo").is_none());
    }

    /// Every catalog name must parse back into a known `Command`.
    #[test]
    fn every_catalog_entry_parses() {
        for entry in COMMAND_CATALOG {
            assert!(
                Command::from_str(entry.name).is_some(),
                "command `{}` is in the catalog but does not parse",
                entry.name
            );
        }
    }
}
