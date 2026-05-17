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

pub fn effort_picker_command(repl: &mut Repl) -> Result<CommandResult> {
    // The TUI worker intercepts this and opens the inline picker;
    // this fallback only runs in non-interactive mode.
    repl.handle_effort_picker_fallback();
    Ok(CommandResult::Continue)
}

pub fn effort_set_command(
    repl: &mut Repl,
    effort: crate::api::ReasoningEffort,
) -> Result<CommandResult> {
    repl.handle_effort_set(effort);
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

pub fn model_picker_command(repl: &mut Repl) -> Result<CommandResult> {
    // The TUI worker intercepts `Command::ModelPicker` before this
    // path is hit and opens the inline picker overlay instead, so
    // executing the command directly only happens in non-interactive
    // mode (for example a unit test, or `--prompt` followed by a
    // typed slash command). Fall back to a printed list there so
    // the user still gets useful information.
    repl.handle_model_picker_fallback();
    Ok(CommandResult::Continue)
}

pub fn model_set_command(repl: &mut Repl, name: &str) -> Result<CommandResult> {
    repl.handle_model_set(name);
    Ok(CommandResult::Continue)
}
