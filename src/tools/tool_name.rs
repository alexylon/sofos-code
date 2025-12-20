use crate::error::{Result, SofosError};

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
    MorphEditFile,
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
            ToolName::MorphEditFile => "morph_edit_file",
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
            "morph_edit_file" => Ok(ToolName::MorphEditFile),
            "web_search" => Ok(ToolName::WebSearch),
            _ => Err(SofosError::ToolExecution(format!("Unknown tool: {}", s))),
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
