pub mod filesystem;
pub mod search;
pub mod types;

use crate::error::{Result, SofosError};
use filesystem::FileSystemTool;
use search::WebSearchTool;
use serde_json::Value;

pub use types::get_tools;

/// ToolExecutor handles execution of tool calls from Claude
pub struct ToolExecutor {
    fs_tool: FileSystemTool,
    search_tool: WebSearchTool,
}

impl ToolExecutor {
    pub fn new(workspace: std::path::PathBuf) -> Result<Self> {
        Ok(Self {
            fs_tool: FileSystemTool::new(workspace)?,
            search_tool: WebSearchTool::new()?,
        })
    }

    pub async fn execute(&self, tool_name: &str, input: &Value) -> Result<String> {
        match tool_name {
            "read_file" => {
                let path = input["path"]
                    .as_str()
                    .ok_or_else(|| SofosError::ToolExecution("Missing 'path' parameter".to_string()))?;

                let content = self.fs_tool.read_file(path)?;
                Ok(format!("File content of '{}':\n\n{}", path, content))
            }
            "write_file" => {
                let path = input["path"]
                    .as_str()
                    .ok_or_else(|| SofosError::ToolExecution("Missing 'path' parameter".to_string()))?;
                let content = input["content"]
                    .as_str()
                    .ok_or_else(|| SofosError::ToolExecution("Missing 'content' parameter".to_string()))?;

                self.fs_tool.write_file(path, content)?;
                Ok(format!("Successfully wrote to file '{}'", path))
            }
            "list_directory" => {
                let path = input["path"]
                    .as_str()
                    .ok_or_else(|| SofosError::ToolExecution("Missing 'path' parameter".to_string()))?;

                let entries = self.fs_tool.list_directory(path)?;
                Ok(format!("Contents of '{}':\n{}", path, entries.join("\n")))
            }
            "create_directory" => {
                let path = input["path"]
                    .as_str()
                    .ok_or_else(|| SofosError::ToolExecution("Missing 'path' parameter".to_string()))?;

                self.fs_tool.create_directory(path)?;
                Ok(format!("Successfully created directory '{}'", path))
            }
            "web_search" => {
                let query = input["query"]
                    .as_str()
                    .ok_or_else(|| SofosError::ToolExecution("Missing 'query' parameter".to_string()))?;
                let max_results = input["max_results"]
                    .as_u64()
                    .unwrap_or(5) as usize;

                let results = self.search_tool.search(query, max_results).await?;

                if results.is_empty() {
                    Ok(format!("No search results found for '{}'", query))
                } else {
                    let formatted = results
                        .iter()
                        .enumerate()
                        .map(|(i, r)| {
                            format!("{}. {}\n   URL: {}\n   {}", i + 1, r.title, r.url, r.snippet)
                        })
                        .collect::<Vec<_>>()
                        .join("\n\n");
                    Ok(format!("Search results for '{}':\n\n{}", query, formatted))
                }
            }
            _ => Err(SofosError::ToolExecution(format!(
                "Unknown tool: {}",
                tool_name
            ))),
        }
    }
}
