use colored::Colorize;
use std::io;
use std::io::Write;
use std::sync::OnceLock;

/// Maximum tokens (≈ chars / 4, ≈ 56 KB) a single tool call is allowed
/// to return before [`truncate_for_context`] clips it with an informational
/// suffix. Keeps a single large bash output or file read from monopolising
/// the model's context window.
pub const MAX_TOOL_OUTPUT_TOKENS: usize = 16_000;

/// Which "kind" of tool output we're truncating — drives the suffix copy
/// so the model sees a hint tuned to the actual recovery path (re-run
/// with redirection for bash; request a range for file reads).
#[derive(Copy, Clone, Debug)]
pub enum TruncationKind {
    /// `read_file` / `read_file_with_outside_access` — suffix suggests
    /// `search_code` or a narrower line range.
    File,
    /// `execute_bash` stdout / stderr — suffix suggests redirecting the
    /// full output to a file.
    BashOutput,
}

impl TruncationKind {
    fn suffix(&self, estimated_tokens: usize, shown_tokens: usize) -> String {
        match self {
            Self::File => format!(
                "[TRUNCATED: File has ~{} tokens, showing first ~{} tokens. Use search_code or request specific line ranges if you need more.]",
                estimated_tokens, shown_tokens
            ),
            Self::BashOutput => format!(
                "[TRUNCATED: Output has ~{} tokens, showing first ~{} tokens. Re-run with output redirection if you need the full output.]",
                estimated_tokens, shown_tokens
            ),
        }
    }
}

/// Truncate `content` to at most `max_tokens` token-equivalents
/// (4 chars ≈ 1 token) and append an informational suffix describing the
/// drop. The cut point is snapped to the nearest UTF-8 char boundary so
/// multi-byte scalars (Cyrillic, CJK, emoji) never get split — raw byte
/// slicing would panic on those. Consolidated from two near-identical
/// copies that previously lived in `filesystem.rs` and `bashexec.rs`.
pub fn truncate_for_context(content: &str, max_tokens: usize, kind: TruncationKind) -> String {
    let estimated_tokens = content.len() / 4;
    if estimated_tokens > max_tokens {
        let truncate_at = crate::api::utils::truncate_at_char_boundary(content, max_tokens * 4);
        let truncated_content = &content[..truncate_at];
        format!(
            "{}...\n\n{}",
            truncated_content,
            kind.suffix(estimated_tokens, max_tokens)
        )
    } else {
        content.to_string()
    }
}

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

/// Callback type for routing confirmation prompts through a UI instead of
/// reading from stdin. Installed once at startup by front ends that own the
/// terminal (the TUI) so prompts don't try to read from a raw-mode stdin
/// the user can't reach.
///
/// Arguments: prompt text, list of choice labels, default index used when
/// the user cancels (Esc), and a typed category for styling. Returns the
/// selected choice index; must always be `< choices.len()`.
pub type ConfirmHandler =
    Box<dyn Fn(&str, &[String], usize, ConfirmationType) -> usize + Send + Sync>;

static CONFIRM_HANDLER: OnceLock<ConfirmHandler> = OnceLock::new();

/// Install a process-global confirmation handler. Can only be set once —
/// subsequent calls are silently ignored. Returns `true` if the handler
/// was installed, `false` if one was already registered.
pub fn set_confirm_handler(handler: ConfirmHandler) -> bool {
    CONFIRM_HANDLER.set(handler).is_ok()
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
}

/// Ask the user to pick one of `choices`. Returns the 0-based index of the
/// selected choice. `default_index` is used when the user cancels (Esc /
/// Ctrl+C in the TUI, empty line in stdin mode) and should point at the
/// "safe" option — e.g. "No" for destructive actions.
///
/// In TUI mode the call routes through the registered `CONFIRM_HANDLER`
/// and blocks the caller thread until the user answers. In non-TUI mode
/// (one-shot `-p` runs, tests) it falls back to a numbered stdin prompt.
pub fn confirm_multi_choice(
    prompt: &str,
    choices: &[&str],
    default_index: usize,
    confirmation_type: ConfirmationType,
) -> crate::error::Result<usize> {
    if choices.is_empty() {
        return Err(crate::error::SofosError::Config(
            "confirm_multi_choice requires at least one choice".to_string(),
        ));
    }
    let default_index = default_index.min(choices.len() - 1);

    if let Some(handler) = CONFIRM_HANDLER.get() {
        let choices_owned: Vec<String> = choices.iter().map(|s| s.to_string()).collect();
        let selected = handler(prompt, &choices_owned, default_index, confirmation_type);
        return Ok(selected.min(choices.len() - 1));
    }

    eprintln!();
    eprintln!(
        "{} {}: {}",
        confirmation_type.icon(),
        confirmation_type.prompt_style(),
        prompt
    );
    for (i, choice) in choices.iter().enumerate() {
        let marker = if i == default_index { "*" } else { " " };
        eprintln!("  {} [{}] {}", marker.dimmed(), i + 1, choice);
    }
    eprint!(
        "  {} ",
        format!(
            "Choose 1–{} (default {}): ",
            choices.len(),
            default_index + 1
        )
        .dimmed()
    );
    io::stderr().flush()?;

    let mut input = String::new();
    io::stdin().read_line(&mut input)?;
    let trimmed = input.trim();
    if trimmed.is_empty() {
        return Ok(default_index);
    }
    match trimmed.parse::<usize>() {
        Ok(n) if n >= 1 && n <= choices.len() => Ok(n - 1),
        _ => Ok(default_index),
    }
}

/// Yes/No convenience over `confirm_multi_choice` for destructive actions
/// (delete file, delete directory). Returns `true` when the user picks
/// the first choice ("Yes"). `No` is the default / fail-safe on cancel.
pub fn confirm_destructive(prompt: &str) -> crate::error::Result<bool> {
    let idx = confirm_multi_choice(prompt, &["Yes", "No"], 1, ConfirmationType::Destructive)?;
    Ok(idx == 0)
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn truncate_for_context_preserves_short_content() {
        let short = "tiny";
        assert_eq!(
            truncate_for_context(short, 16_000, TruncationKind::File),
            short
        );
        assert_eq!(
            truncate_for_context(short, 16_000, TruncationKind::BashOutput),
            short
        );
    }

    #[test]
    fn truncate_for_context_file_variant_hints_at_range_read() {
        let big = "x".repeat(20_000);
        let out = truncate_for_context(&big, 4, TruncationKind::File);
        assert!(out.contains("[TRUNCATED: File has"));
        assert!(out.contains("search_code or request specific line ranges"));
        assert!(!out.contains("output redirection"));
    }

    #[test]
    fn truncate_for_context_bash_variant_hints_at_redirection() {
        let big = "y".repeat(20_000);
        let out = truncate_for_context(&big, 4, TruncationKind::BashOutput);
        assert!(out.contains("[TRUNCATED: Output has"));
        assert!(out.contains("output redirection"));
        assert!(!out.contains("search_code"));
    }
}
