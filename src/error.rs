use thiserror::Error;

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
            Self::ToolExecution(msg) => {
                msg.contains("blocked")
                    || msg.contains("Blocked")
                    || msg.contains("denied")
                    || msg.contains("not allowed")
                    || msg.contains("not explicitly allowed")
                    || msg.contains("outside workspace")
            }
            Self::Context { source, .. } => source.is_blocked(),
            _ => false,
        }
    }
}

pub type Result<T> = std::result::Result<T, SofosError>;
