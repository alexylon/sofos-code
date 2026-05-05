use thiserror::Error;

pub const DEFAULT_PARENT_DIR: &str = ".";

/// Substrings that, when found in a `ToolExecution` error message,
/// classify the error as an *expected* security block rather than an
/// actual failure. Both casings of "blocked" / "Blocked" are listed
/// because tool messages are constructed inline by call sites that
/// don't share a casing convention.
const BLOCKED_KEYWORDS: &[&str] = &[
    "blocked",
    "Blocked",
    "denied",
    "not allowed",
    "not explicitly allowed",
    "outside workspace",
    "absolute paths",
    "contains '..'",
    "tilde paths",
    "output redirection",
    "here-doc",
];

#[derive(Error, Debug)]
pub enum SofosError {
    #[error("API error: {0}")]
    Api(String),

    #[error("HTTP request failed: {0}")]
    Http(#[from] reqwest::Error),

    #[error("Network error: {0}")]
    NetworkError(String),

    #[error("JSON error: {0}")]
    Json(#[from] serde_json::Error),

    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),

    #[error("Path security violation: {0}")]
    PathViolation(String),

    #[error("Invalid path: {0}")]
    InvalidPath(String),

    #[error("File not found: {0}")]
    FileNotFound(String),

    #[error("Configuration error: {0}")]
    Config(String),

    #[error("Tool execution error: {0}")]
    ToolExecution(String),

    #[error("MCP error: {0}")]
    McpError(String),

    #[error("Task join error: {0}")]
    Join(String),

    #[error("Interrupted by user")]
    Interrupted,

    #[error("{message}")]
    Context {
        message: String,
        #[source]
        source: Box<SofosError>,
    },
}

impl SofosError {
    /// Returns true if this error represents a security block or permission denial
    /// (expected behavior) rather than an actual failure
    pub fn is_blocked(&self) -> bool {
        match self {
            Self::PathViolation(_) => true,
            Self::ToolExecution(msg) => BLOCKED_KEYWORDS.iter().any(|kw| msg.contains(kw)),
            Self::McpError(_) => false,
            Self::Join(_) => false,
            Self::Context { source, .. } => source.is_blocked(),
            _ => false,
        }
    }

    pub fn hint(&self) -> Option<String> {
        match self {
            Self::FileNotFound(path) => {
                let parent = std::path::Path::new(path)
                    .parent()
                    .and_then(|p| p.to_str())
                    .unwrap_or(DEFAULT_PARENT_DIR);
                Some(format!(
                    "Use list_directory on '{}' to see available files",
                    parent
                ))
            }

            Self::PathViolation(msg) => {
                if msg.contains("Absolute paths") {
                    Some("Use relative paths from the workspace root".to_string())
                } else if msg.contains("..") || msg.contains("Parent directory") {
                    Some("Stay within the workspace directory".to_string())
                } else if msg.contains("escapes workspace") {
                    Some("All file operations must stay within the current workspace".to_string())
                } else {
                    None
                }
            }

            Self::InvalidPath(msg) => {
                if msg.contains("not a directory") {
                    Some("Check that the path points to a directory, not a file".to_string())
                } else if msg.contains("not a file") {
                    Some("Check that the path points to a file, not a directory".to_string())
                } else {
                    None
                }
            }

            Self::Api(msg) => {
                if msg.contains("401")
                    || msg.contains("authentication")
                    || msg.contains("unauthorized")
                {
                    Some("Check that your API key is valid and has not expired".to_string())
                } else if msg.contains("429") || msg.contains("rate limit") {
                    Some("Wait a moment and try again, or reduce request frequency".to_string())
                } else if msg.contains("500") || msg.contains("server error") {
                    Some("The API server may be experiencing issues. Try again later".to_string())
                } else {
                    None
                }
            }

            Self::NetworkError(msg) => {
                if msg.contains("timeout") {
                    Some("Check your internet connection or try again".to_string())
                } else if msg.contains("connection refused") || msg.contains("connect") {
                    Some("Check your internet connection and firewall settings".to_string())
                } else {
                    Some("Check your internet connection".to_string())
                }
            }

            Self::Http(e) => {
                let msg = e.to_string().to_lowercase();
                if msg.contains("timeout") {
                    Some("Request timed out. Check your internet connection".to_string())
                } else if msg.contains("connection") {
                    Some("Connection failed. Check your internet connection".to_string())
                } else {
                    None
                }
            }

            Self::Config(msg) => {
                if msg.contains("API key")
                    || msg.contains("api key")
                    || msg.contains("ANTHROPIC_API_KEY")
                {
                    Some(
                        "Set ANTHROPIC_API_KEY environment variable or use --api-key flag"
                            .to_string(),
                    )
                } else if msg.contains("OPENAI_API_KEY") {
                    Some(
                        "Set OPENAI_API_KEY environment variable or use --openai-api-key flag"
                            .to_string(),
                    )
                } else if msg.contains("max_tokens") && msg.contains("thinking-budget ceiling") {
                    // Matches the validation message in `Repl::new`. The
                    // suggestion no longer mentions `--thinking-budget`
                    // because that flag is inert — the legacy budget is
                    // picked per-effort tier in `request_builder`, not
                    // from the flag value.
                    Some(format!(
                        "Increase --max-tokens above {} or set --reasoning-effort off",
                        crate::api::anthropic::LEGACY_THINKING_BUDGET_HIGH
                    ))
                } else {
                    None
                }
            }

            Self::Json(e) => {
                let msg = e.to_string();
                if msg.contains("expected") {
                    Some(
                        "The API response format was unexpected. This may be a temporary issue"
                            .to_string(),
                    )
                } else {
                    None
                }
            }

            Self::Io(e) => {
                use std::io::ErrorKind;
                match e.kind() {
                    ErrorKind::PermissionDenied => {
                        Some("Check file permissions or run from a different directory".to_string())
                    }
                    ErrorKind::NotFound => Some("The file or directory does not exist".to_string()),
                    ErrorKind::AlreadyExists => {
                        Some("A file or directory with this name already exists".to_string())
                    }
                    _ => None,
                }
            }

            Self::ToolExecution(msg) => {
                if msg.contains("Hint:") {
                    None
                } else if msg.contains("Missing") && msg.contains("parameter") {
                    Some("Ensure all required parameters are provided".to_string())
                } else if msg.contains("too large") {
                    Some("Try processing smaller files or limiting output".to_string())
                } else if msg.contains("ripgrep") {
                    Some(
                        "Install ripgrep: https://github.com/BurntSushi/ripgrep#installation"
                            .to_string(),
                    )
                } else if msg.contains("MORPH_API_KEY") {
                    Some(
                        "Set MORPH_API_KEY environment variable to enable fast editing".to_string(),
                    )
                } else {
                    None
                }
            }
            Self::McpError(msg) => {
                if msg.contains("Failed to connect") || msg.contains("Failed to start") {
                    Some(
                        "Check that the MCP server command/URL is correct and accessible"
                            .to_string(),
                    )
                } else if msg.contains("Failed to list tools") || msg.contains("Failed to parse") {
                    Some("The MCP server may be incompatible or misconfigured".to_string())
                } else {
                    None
                }
            }
            Self::Join(_) => None,

            Self::Context { source, .. } => source.hint(),
            Self::Interrupted => None,
        }
    }

    #[allow(dead_code)]
    pub fn with_hint(&self) -> String {
        match self.hint() {
            Some(hint) => format!("{}\nHint: {}", self, hint),
            None => self.to_string(),
        }
    }
}

pub type Result<T> = std::result::Result<T, SofosError>;

#[cfg(test)]
mod tests {
    use super::*;

    /// Regression: the validation message in `Repl::new` was rewritten
    /// (`thinking_budget >= max_tokens` → `max_tokens <= legacy thinking-
    /// budget ceiling`) when `/think` started picking budgets per-effort
    /// instead of from the inert flag. The classifier here must still
    /// recognise the new wording and surface a useful hint.
    #[test]
    fn config_hint_fires_on_new_max_tokens_validation_message() {
        // Mirror the exact format string used at the validation site so
        // a future rewording on either side breaks this test loudly.
        let err = SofosError::Config(format!(
            "max_tokens ({}) must exceed the legacy thinking-budget ceiling ({}). \
             Use a higher --max-tokens or set --reasoning-effort off.",
            crate::api::anthropic::LEGACY_THINKING_BUDGET_HIGH,
            crate::api::anthropic::LEGACY_THINKING_BUDGET_HIGH
        ));
        let hint = err.hint().expect("hint must fire on the new message");
        assert!(
            hint.contains("Increase --max-tokens"),
            "hint should mention --max-tokens, got: {hint}"
        );
        assert!(
            hint.contains(&crate::api::anthropic::LEGACY_THINKING_BUDGET_HIGH.to_string()),
            "hint should embed the actual ceiling, got: {hint}"
        );
        // `--thinking-budget` is inert; the suggestion must not point
        // users at a flag that no longer does anything.
        assert!(
            !hint.contains("--thinking-budget"),
            "suggestion must not advise tweaking the inert --thinking-budget flag, got: {hint}"
        );
    }
}
