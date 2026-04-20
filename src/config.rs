/// Central configuration for Sofos. The actual file-size and bash-output
/// caps live next to the code that enforces them — `MAX_FILE_SIZE` in
/// `src/tools/filesystem.rs` (50 MB) and `MAX_OUTPUT_SIZE` in
/// `src/tools/bashexec.rs` (10 MB) — not here, so this struct only
/// carries config values that the rest of the crate actually reads.
#[derive(Debug, Clone)]
pub struct SofosConfig {
    pub max_messages: usize,
    pub max_context_tokens: usize,
    pub max_tool_iterations: u32,
    /// Auto-compact when token usage exceeds this ratio of max_context_tokens
    pub compaction_trigger_ratio: f64,
    /// Number of recent messages to preserve during compaction
    pub compaction_preserve_recent: usize,
    /// Truncate tool results longer than this (chars) during compaction
    pub tool_result_truncate_threshold: usize,
}

impl Default for SofosConfig {
    fn default() -> Self {
        Self {
            max_messages: 500,
            max_context_tokens: 165_000,
            max_tool_iterations: 200,
            compaction_trigger_ratio: 0.80,
            compaction_preserve_recent: 20,
            tool_result_truncate_threshold: 2000,
        }
    }
}

/// Configuration for the language model
#[derive(Clone)]
pub struct ModelConfig {
    pub model: String,
    pub max_tokens: u32,
    pub enable_thinking: bool,
    pub thinking_budget: u32,
}

impl ModelConfig {
    pub fn new(
        model: String,
        max_tokens: u32,
        enable_thinking: bool,
        thinking_budget: u32,
    ) -> Self {
        Self {
            model,
            max_tokens,
            enable_thinking,
            thinking_budget,
        }
    }

    pub fn set_thinking(&mut self, enabled: bool) {
        self.enable_thinking = enabled;
    }
}

impl SofosConfig {
    // No need for new() since Default::default() is the idiomatic way
}

/// Safe mode message shown to user and AI
pub const SAFE_MODE_MESSAGE: &str = "[SYSTEM: Safe (read-only) mode has been enabled. \
                                     No file modifications or bash commands are allowed.\
                                     Available tools: list_directory, read_file and web_search.]";

/// Normal mode message shown when switching from safe mode
pub const NORMAL_MODE_MESSAGE: &str = "[SYSTEM: Normal (unrestricted) mode has been enabled. \
                                       File modifications and bash commands are now allowed.\
                                       All tools are available]";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_default_config() {
        let config = SofosConfig::default();
        assert_eq!(config.max_messages, 500);
        assert_eq!(config.max_context_tokens, 165_000);
        assert_eq!(config.max_tool_iterations, 200);
        assert!((config.compaction_trigger_ratio - 0.80).abs() < f64::EPSILON);
        assert_eq!(config.compaction_preserve_recent, 20);
        assert_eq!(config.tool_result_truncate_threshold, 2000);
    }
}
