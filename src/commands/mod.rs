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
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Command {
    Exit,
    Clear,
    Resume,
    ThinkSet(crate::api::ReasoningEffort),
    ThinkStatus,
    SafeMode,
    NormalMode,
    Compact,
}

impl Command {
    pub fn from_str(s: &str) -> Option<Self> {
        let lower = s.to_lowercase();
        match lower.as_str() {
            "/exit" | "/quit" | "/q" => Some(Command::Exit),
            "/clear" => Some(Command::Clear),
            "/resume" => Some(Command::Resume),
            "/think" => Some(Command::ThinkStatus),
            "/s" => Some(Command::SafeMode),
            "/n" => Some(Command::NormalMode),
            "/compact" => Some(Command::Compact),
            _ => {
                if let Some(arg) = lower.strip_prefix("/think ") {
                    crate::api::ReasoningEffort::parse(arg).map(Command::ThinkSet)
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
            Command::ThinkSet(effort) => builtin::think_set_command(repl, *effort),
            Command::ThinkStatus => builtin::think_status_command(repl),
            Command::SafeMode => builtin::safe_mode_command(repl),
            Command::NormalMode => builtin::normal_mode_command(repl),
            Command::Compact => builtin::compact_command(repl),
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
        name: "/clear",
        description: "clear the conversation and start fresh",
    },
    CommandEntry {
        name: "/compact",
        description: "summarize the conversation to free up context",
    },
    CommandEntry {
        name: "/resume",
        description: "resume a previously saved session",
    },
    CommandEntry {
        name: "/think",
        description: "show the current reasoning effort",
    },
    CommandEntry {
        name: "/think off",
        description: "disable reasoning effort",
    },
    CommandEntry {
        name: "/think low",
        description: "set reasoning effort to low",
    },
    CommandEntry {
        name: "/think medium",
        description: "set reasoning effort to medium",
    },
    CommandEntry {
        name: "/think high",
        description: "set reasoning effort to high",
    },
    CommandEntry {
        name: "/think xhigh",
        description: "set reasoning effort to extra high",
    },
    CommandEntry {
        name: "/think max",
        description: "set reasoning effort to the maximum value",
    },
    CommandEntry {
        name: "/s",
        description: "enter safe mode (only read-only tools are allowed)",
    },
    CommandEntry {
        name: "/n",
        description: "leave safe mode and resume normal mode",
    },
    CommandEntry {
        name: "/exit",
        description: "save the session and quit",
    },
    CommandEntry {
        name: "/quit",
        description: "alias of /exit",
    },
    CommandEntry {
        name: "/q",
        description: "alias of /exit",
    },
];

#[cfg(test)]
mod tests {
    use super::*;

    /// Every catalog name must parse back into a known `Command`, either
    /// directly or via the `/think <effort>` argument form.
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
