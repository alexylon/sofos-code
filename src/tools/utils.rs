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

/// Strip HTML tags and convert common entities to produce readable plain text
pub fn html_to_text(html: &str) -> String {
    let mut out = String::with_capacity(html.len() / 2);
    let mut in_tag = false;
    let mut in_script = false;
    let mut in_style = false;
    let mut last_was_whitespace = false;

    let lower = html.to_lowercase();
    let chars: Vec<char> = html.chars().collect();
    let lower_chars: Vec<char> = lower.chars().collect();
    let len = chars.len();
    let mut i = 0;

    while i < len {
        if in_tag {
            if chars[i] == '>' {
                in_tag = false;
            }
            i += 1;
            continue;
        }

        if chars[i] == '<' {
            // Check for block-level tags that should insert newlines
            let rest = &lower[lower.char_indices().nth(i).map_or(0, |(idx, _)| idx)..];
            if rest.starts_with("<script") {
                in_script = true;
            } else if rest.starts_with("</script") {
                in_script = false;
            } else if rest.starts_with("<style") {
                in_style = true;
            } else if rest.starts_with("</style") {
                in_style = false;
            }

            let is_block = rest.starts_with("<br")
                || rest.starts_with("<p")
                || rest.starts_with("</p")
                || rest.starts_with("<div")
                || rest.starts_with("</div")
                || rest.starts_with("<li")
                || rest.starts_with("<h1")
                || rest.starts_with("<h2")
                || rest.starts_with("<h3")
                || rest.starts_with("<h4")
                || rest.starts_with("<tr")
                || rest.starts_with("</tr");

            if is_block && !out.ends_with('\n') {
                out.push('\n');
                last_was_whitespace = true;
            }

            in_tag = true;
            i += 1;
            continue;
        }

        if in_script || in_style {
            i += 1;
            continue;
        }

        // Handle HTML entities
        if chars[i] == '&' {
            let rest: String = lower_chars[i..].iter().take(10).collect();
            if rest.starts_with("&amp;") {
                out.push('&');
                i += 5;
            } else if rest.starts_with("&lt;") {
                out.push('<');
                i += 4;
            } else if rest.starts_with("&gt;") {
                out.push('>');
                i += 4;
            } else if rest.starts_with("&quot;") {
                out.push('"');
                i += 6;
            } else if rest.starts_with("&#39;") || rest.starts_with("&apos;") {
                out.push('\'');
                i += if rest.starts_with("&#39;") { 5 } else { 6 };
            } else if rest.starts_with("&nbsp;") {
                out.push(' ');
                i += 6;
            } else {
                out.push('&');
                i += 1;
            }
            last_was_whitespace = false;
            continue;
        }

        let ch = chars[i];
        if ch.is_whitespace() {
            if !last_was_whitespace {
                out.push(if ch == '\n' { '\n' } else { ' ' });
                last_was_whitespace = true;
            }
        } else {
            out.push(ch);
            last_was_whitespace = false;
        }
        i += 1;
    }

    // Collapse runs of 3+ newlines into 2
    let mut result = String::new();
    let mut consecutive_newlines = 0;
    for ch in out.chars() {
        if ch == '\n' {
            consecutive_newlines += 1;
            if consecutive_newlines <= 2 {
                result.push(ch);
            }
        } else {
            consecutive_newlines = 0;
            result.push(ch);
        }
    }

    result.trim().to_string()
}
