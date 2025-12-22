use colored::Colorize;
use std::io;
use std::io::Write;

/// Confirmation dialog type determines styling and default behavior
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum ConfirmationType {
    /// Destructive action (delete, overwrite) - defaults to No
    Destructive,
    /// Permission request (allow command) - defaults to No
    Permission,
    /// Informational confirmation - defaults to No
    #[allow(dead_code)]
    Info,
}

impl ConfirmationType {
    fn icon(&self) -> &'static str {
        match self {
            Self::Destructive => "🗑️ ",
            Self::Permission => "🔐",
            Self::Info => "❓",
        }
    }

    fn prompt_style(&self) -> colored::ColoredString {
        match self {
            Self::Destructive => "Confirm".truecolor(0xFF, 0x99, 0x33).bold(), // Orange
            Self::Permission => "Permission".bright_yellow().bold(),
            Self::Info => "Confirm".bright_cyan().bold(),
        }
    }

    fn options(&self) -> &'static str {
        // All default to No for safety
        "[y/N]"
    }
}

pub fn confirm_action_enhanced(
    prompt: &str,
    confirmation_type: ConfirmationType,
) -> crate::error::Result<bool> {
    eprint!(
        "{} {}: {} {}: ",
        confirmation_type.icon(),
        confirmation_type.prompt_style(),
        prompt,
        confirmation_type.options().dimmed()
    );
    io::stderr().flush()?;

    let mut input = String::new();
    io::stdin().read_line(&mut input)?;

    let answer = input.trim().to_lowercase();
    
    // Default to No - only explicit "y" or "yes" confirms
    Ok(answer == "y" || answer == "yes")
}

pub fn confirm_action(prompt: &str) -> crate::error::Result<bool> {
    confirm_action_enhanced(prompt, ConfirmationType::Info)
}

pub fn confirm_destructive(prompt: &str) -> crate::error::Result<bool> {
    confirm_action_enhanced(prompt, ConfirmationType::Destructive)
}

pub fn confirm_permission(prompt: &str) -> crate::error::Result<bool> {
    confirm_action_enhanced(prompt, ConfirmationType::Permission)
}
