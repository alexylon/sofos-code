use super::CommandResult;
use crate::error::Result;
use crate::repl::Repl;
use crate::ui::UI;

pub fn exit_command(repl: &mut Repl) -> Result<CommandResult> {
    // The TUI layer is responsible for printing the session summary and
    // goodbye banner after tearing down the alt screen — doing it here would
    // either duplicate the output or trap it inside the alternate screen.
    repl.save_current_session()?;
    Ok(CommandResult::Exit)
}

pub fn clear_command(repl: &mut Repl) -> Result<CommandResult> {
    repl.handle_clear_command()?;
    Ok(CommandResult::Continue)
}

pub fn resume_command(repl: &mut Repl) -> Result<CommandResult> {
    if let Err(e) = repl.handle_resume_command() {
        UI::print_error_with_hint(&e);
    }
    Ok(CommandResult::Continue)
}

pub fn think_on_command(repl: &mut Repl) -> Result<CommandResult> {
    repl.handle_think_on();
    Ok(CommandResult::Continue)
}

pub fn think_off_command(repl: &mut Repl) -> Result<CommandResult> {
    repl.handle_think_off();
    Ok(CommandResult::Continue)
}

pub fn think_status_command(repl: &mut Repl) -> Result<CommandResult> {
    repl.handle_think_status();
    Ok(CommandResult::Continue)
}

pub fn safe_mode_command(repl: &mut Repl) -> Result<CommandResult> {
    repl.enable_safe_mode();
    Ok(CommandResult::Continue)
}

pub fn normal_mode_command(repl: &mut Repl) -> Result<CommandResult> {
    repl.disable_safe_mode();
    Ok(CommandResult::Continue)
}

pub fn compact_command(repl: &mut Repl) -> Result<CommandResult> {
    repl.handle_compact_command()?;
    Ok(CommandResult::Continue)
}
