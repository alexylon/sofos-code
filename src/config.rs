/// Central configuration for Sofos
#[derive(Debug, Clone)]
pub struct SofosConfig {
    /// Maximum number of messages to keep in conversation history
    pub max_messages: usize,
    /// Maximum number of tool execution iterations to prevent infinite loops
    pub max_tool_iterations: u32,
    /// Maximum file size for read operations (in bytes)
    #[allow(dead_code)]
    pub max_file_size: usize,
    /// Maximum bash command output size (in bytes)
    #[allow(dead_code)]
    pub max_bash_output: usize,
}

impl Default for SofosConfig {
    fn default() -> Self {
        Self {
            max_messages: 500,
            max_tool_iterations: 200,
            max_file_size: 10 * 1024 * 1024,   // 10MB
            max_bash_output: 50 * 1024 * 1024, // 50MB
        }
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
        assert_eq!(config.max_tool_iterations, 200);
        assert_eq!(config.max_file_size, 10 * 1024 * 1024);
        assert_eq!(config.max_bash_output, 50 * 1024 * 1024);
    }
}
