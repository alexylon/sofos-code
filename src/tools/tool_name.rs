use crate::error::{Result, SofosError};
use colored::Colorize;
use serde_json::Value;

/// Type-safe tool names to prevent typos and enable better refactoring
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ToolName {
    ReadFile,
    WriteFile,
    ListDirectory,
    CreateDirectory,
    DeleteFile,
    DeleteDirectory,
    MoveFile,
    CopyFile,
    ExecuteBash,
    SearchCode,
    EditFile,
    GlobFiles,
    MorphEditFile,
    WebFetch,
    WebSearch,
}

impl ToolName {
    pub fn as_str(&self) -> &'static str {
        match self {
            ToolName::ReadFile => "read_file",
            ToolName::WriteFile => "write_file",
            ToolName::ListDirectory => "list_directory",
            ToolName::CreateDirectory => "create_directory",
            ToolName::DeleteFile => "delete_file",
            ToolName::DeleteDirectory => "delete_directory",
            ToolName::MoveFile => "move_file",
            ToolName::CopyFile => "copy_file",
            ToolName::ExecuteBash => "execute_bash",
            ToolName::SearchCode => "search_code",
            ToolName::EditFile => "edit_file",
            ToolName::GlobFiles => "glob_files",
            ToolName::MorphEditFile => "morph_edit_file",
            ToolName::WebFetch => "web_fetch",
            ToolName::WebSearch => "web_search",
        }
    }

    pub fn from_str(s: &str) -> Result<Self> {
        match s {
            "read_file" => Ok(ToolName::ReadFile),
            "write_file" => Ok(ToolName::WriteFile),
            "list_directory" => Ok(ToolName::ListDirectory),
            "create_directory" => Ok(ToolName::CreateDirectory),
            "delete_file" => Ok(ToolName::DeleteFile),
            "delete_directory" => Ok(ToolName::DeleteDirectory),
            "move_file" => Ok(ToolName::MoveFile),
            "copy_file" => Ok(ToolName::CopyFile),
            "execute_bash" => Ok(ToolName::ExecuteBash),
            "search_code" => Ok(ToolName::SearchCode),
            "edit_file" => Ok(ToolName::EditFile),
            "glob_files" => Ok(ToolName::GlobFiles),
            "morph_edit_file" => Ok(ToolName::MorphEditFile),
            "web_fetch" => Ok(ToolName::WebFetch),
            "web_search" => Ok(ToolName::WebSearch),
            _ => Err(SofosError::ToolExecution(format!("Unknown tool: {}", s))),
        }
    }

    /// Render a one-line human summary of a completed tool call for the
    /// transcript UI. The four custom-shaped variants (read_file,
    /// list_directory, search_code, web_fetch) extract counts/paths from
    /// `tool_input` + `output`; everything else falls through to the raw
    /// tool output. MCP tools never reach here — `from_str` rejects them
    /// at the caller and they get the raw-output fallback there.
    pub fn display_summary(&self, tool_input: &Value, output: &str) -> String {
        match self {
            ToolName::ReadFile => {
                let file_path = tool_input
                    .get("path")
                    .and_then(|v| v.as_str())
                    .unwrap_or("");

                let offset = tool_input
                    .get("offset")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(1);

                let line_count = output.lines().count() as u64;

                if line_count == 0 {
                    if file_path.is_empty() {
                        "Read file (empty or not found)".to_string()
                    } else {
                        format!(
                            "Read file from {} - empty or not found",
                            file_path.bright_cyan()
                        )
                    }
                } else {
                    let end_line = offset + line_count - 1;
                    if file_path.is_empty() {
                        format!("Read lines {}-{}", offset, end_line)
                    } else {
                        format!(
                            "Read lines {}-{} from {}",
                            offset,
                            end_line,
                            file_path.bright_cyan()
                        )
                    }
                }
            }
            ToolName::ListDirectory => {
                let path = tool_input
                    .get("path")
                    .and_then(|v| v.as_str())
                    .unwrap_or(".");

                let item_count = output
                    .lines()
                    .filter(|line| !line.trim().is_empty() && !line.starts_with("Contents of"))
                    .count();

                if item_count == 0 {
                    format!("Found 0 items in {}", path.bright_cyan())
                } else if item_count == 1 {
                    format!("Found 1 item in {}", path.bright_cyan())
                } else {
                    format!("Found {} items in {}", item_count, path.bright_cyan())
                }
            }
            ToolName::WebFetch => {
                let url = tool_input.get("url").and_then(|v| v.as_str()).unwrap_or("");
                let char_count = output.len();
                format!("Fetched {} ({} chars)", url.bright_cyan(), char_count)
            }
            ToolName::SearchCode => {
                let pattern = tool_input
                    .get("pattern")
                    .and_then(|v| v.as_str())
                    .unwrap_or("");

                let body = output
                    .strip_prefix(crate::tools::codesearch::SEARCH_RESULTS_PREFIX)
                    .unwrap_or(output);

                // ripgrep --heading output groups matches under file headings
                // separated by blank lines. Lines starting with `<digits>:` are
                // matches; non-empty lines without that prefix are file
                // headings.
                let mut files = 0usize;
                let mut matches = 0usize;
                for line in body.lines() {
                    if line.is_empty() {
                        continue;
                    }
                    if line.starts_with("No matches found") {
                        continue;
                    }
                    let is_match_line = line.split_once(':').is_some_and(|(prefix, _)| {
                        !prefix.is_empty() && prefix.chars().all(|c| c.is_ascii_digit())
                    });
                    if is_match_line {
                        matches += 1;
                    } else {
                        files += 1;
                    }
                }

                if matches == 0 {
                    format!("No matches for {}", pattern.bright_cyan())
                } else {
                    format!(
                        "Found {} match{} in {} file{} for {}",
                        matches,
                        if matches == 1 { "" } else { "es" },
                        files,
                        if files == 1 { "" } else { "s" },
                        pattern.bright_cyan()
                    )
                }
            }
            _ => output.to_string(),
        }
    }
}

impl std::fmt::Display for ToolName {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.as_str())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_tool_name_roundtrip() {
        let tools = [
            ToolName::ReadFile,
            ToolName::WriteFile,
            ToolName::ExecuteBash,
            ToolName::MorphEditFile,
        ];

        for tool in &tools {
            let s = tool.as_str();
            let parsed = ToolName::from_str(s).unwrap();
            assert_eq!(*tool, parsed);
        }
    }

    #[test]
    fn test_unknown_tool() {
        assert!(ToolName::from_str("unknown_tool").is_err());
    }
}
