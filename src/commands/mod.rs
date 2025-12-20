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
    ThinkOn,
    ThinkOff,
    ThinkStatus,
    SafeMode,
    NormalMode,
}

impl Command {
    pub fn from_str(s: &str) -> Option<Self> {
        match s.to_lowercase().as_str() {
            "/exit" | "/quit" | "/q" => Some(Command::Exit),
            "/clear" => Some(Command::Clear),
            "/resume" => Some(Command::Resume),
            "/think on" => Some(Command::ThinkOn),
            "/think off" => Some(Command::ThinkOff),
            "/think" => Some(Command::ThinkStatus),
            "/s" => Some(Command::SafeMode),
            "/n" => Some(Command::NormalMode),
            _ => None,
        }
    }

    pub fn execute(&self, repl: &mut Repl) -> Result<CommandResult> {
        match self {
            Command::Exit => builtin::exit_command(repl),
            Command::Clear => builtin::clear_command(repl),
            Command::Resume => builtin::resume_command(repl),
            Command::ThinkOn => builtin::think_on_command(repl),
            Command::ThinkOff => builtin::think_off_command(repl),
            Command::ThinkStatus => builtin::think_status_command(repl),
            Command::SafeMode => builtin::safe_mode_command(repl),
            Command::NormalMode => builtin::normal_mode_command(repl),
        }
    }
}

/// All available commands as strings (for autocomplete)
pub static COMMANDS: &[&str] = &[
    "/exit",
    "/quit",
    "/q",
    "/clear",
    "/resume",
    "/think on",
    "/think off",
    "/think",
    "/s",
    "/n",
];
