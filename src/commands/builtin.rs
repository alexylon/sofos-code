use super::CommandResult;
use crate::error::Result;
use crate::repl::Repl;
use crate::ui::UI;

pub fn exit_command(repl: &mut Repl) -> Result<CommandResult> {
    repl.save_current_session()?;
    let (model, input_tokens, output_tokens) = repl.get_session_summary();
    UI::display_session_summary(&model, input_tokens, output_tokens);
    UI::print_goodbye();
    Ok(CommandResult::Exit)
}

pub fn clear_command(repl: &mut Repl) -> Result<CommandResult> {
    repl.handle_clear_command()?;
    Ok(CommandResult::Continue)
}

pub fn resume_command(repl: &mut Repl) -> Result<CommandResult> {
    if let Err(e) = repl.handle_resume_command() {
        UI::print_error(&e.to_string());
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
