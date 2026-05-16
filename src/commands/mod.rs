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

/// All available commands as strings (for Tab autocomplete in the TUI)
pub static COMMANDS: &[&str] = &[
    "/exit",
    "/quit",
    "/q",
    "/clear",
    "/resume",
    "/think off",
    "/think low",
    "/think medium",
    "/think high",
    "/think xhigh",
    "/think max",
    "/think",
    "/s",
    "/n",
    "/compact",
];
