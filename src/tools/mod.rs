pub mod bashexec;
pub mod codesearch;
pub mod filesystem;
pub mod types;

use crate::api::MorphClient;
use crate::diff;
use crate::error::{Result, SofosError};
use bashexec::BashExecutor;
use codesearch::CodeSearchTool;
use filesystem::FileSystemTool;
use serde_json::Value;
use std::io::{self, Write};

pub use types::{add_code_search_tool, get_tools, get_tools_with_morph};

fn confirm_action(prompt: &str) -> Result<bool> {
    print!("{} (y/n): ", prompt);
    io::stdout().flush()?;

    let mut input = String::new();
    io::stdin().read_line(&mut input)?;

    let answer = input.trim().to_lowercase();
    Ok(answer == "y" || answer == "yes")
}

/// ToolExecutor handles execution of tool calls from AI
#[derive(Clone)]
pub struct ToolExecutor {
    fs_tool: FileSystemTool,
    code_search_tool: Option<CodeSearchTool>,
    bash_executor: BashExecutor,
    morph_client: Option<MorphClient>,
}

impl ToolExecutor {
    pub fn new(workspace: std::path::PathBuf, morph_client: Option<MorphClient>) -> Result<Self> {
        let code_search_tool = match CodeSearchTool::new(workspace.clone()) {
            Ok(tool) => Some(tool),
            Err(_) => {
                eprintln!("Warning: ripgrep not found. Code search will be unavailable.");
                None
            }
        };

        Ok(Self {
            fs_tool: FileSystemTool::new(workspace.clone())?,
            code_search_tool,
            bash_executor: BashExecutor::new(workspace)?,
            morph_client,
        })
    }

    pub fn has_morph(&self) -> bool {
        self.morph_client.is_some()
    }

    pub fn has_code_search(&self) -> bool {
        self.code_search_tool.is_some()
    }

    pub fn get_available_tools(&self) -> Vec<crate::api::Tool> {
        let mut tools = if self.has_morph() {
            get_tools_with_morph()
        } else {
            get_tools()
        };

        if self.has_code_search() {
            add_code_search_tool(&mut tools);
        }

        tools
    }

    pub async fn execute(&self, tool_name: &str, input: &Value) -> Result<String> {
        match tool_name {
            "read_file" => {
                let path = input["path"].as_str().ok_or_else(|| {
                    SofosError::ToolExecution("Missing 'path' parameter".to_string())
                })?;

                match self.fs_tool.read_file(path) {
                    Ok(content) => Ok(format!("File content of '{}':\n\n{}", path, content)),
                    Err(e) => {
                        if matches!(e, SofosError::FileNotFound(_)) {
                            let parent_dir = std::path::Path::new(path)
                                .parent()
                                .and_then(|p| p.to_str())
                                .unwrap_or(".");
                            Err(SofosError::ToolExecution(format!(
                                "File not found: '{}'. Suggestion: Use list_directory with path '{}' to see available files.",
                                path, parent_dir
                            )))
                        } else {
                            Err(e)
                        }
                    }
                }
            }
            "write_file" => {
                let path = input["path"].as_str().ok_or_else(|| {
                    SofosError::ToolExecution("Missing 'path' parameter".to_string())
                })?;
                let content = input["content"].as_str().ok_or_else(|| {
                    SofosError::ToolExecution("Missing 'content' parameter".to_string())
                })?;

                // Check if file exists and read original content for diff
                let original_content = self.fs_tool.read_file(path).ok();

                self.fs_tool.write_file(path, content)?;

                // If file existed before, show diff
                if let Some(original) = original_content {
                    let diff_output = diff::generate_compact_diff(&original, content);
                    Ok(format!(
                        "Successfully wrote to file '{}'\n\nChanges:\n{}",
                        path, diff_output
                    ))
                } else {
                    Ok(format!("Successfully created file '{}'", path))
                }
            }
            "list_directory" => {
                let path = input["path"].as_str().ok_or_else(|| {
                    SofosError::ToolExecution("Missing 'path' parameter".to_string())
                })?;

                let entries = self.fs_tool.list_directory(path)?;
                Ok(format!("Contents of '{}':\n{}", path, entries.join("\n")))
            }
            "create_directory" => {
                let path = input["path"].as_str().ok_or_else(|| {
                    SofosError::ToolExecution("Missing 'path' parameter".to_string())
                })?;

                self.fs_tool.create_directory(path)?;
                Ok(format!("Successfully created directory '{}'", path))
            }
            "search_code" => {
                let code_search = self.code_search_tool.as_ref()
                    .ok_or_else(|| SofosError::ToolExecution(
                        "Code search not available. Please install ripgrep: https://github.com/BurntSushi/ripgrep".to_string()
                    ))?;

                let pattern = input["pattern"].as_str().ok_or_else(|| {
                    SofosError::ToolExecution("Missing 'pattern' parameter".to_string())
                })?;

                let file_type = input["file_type"].as_str();
                let max_results = input["max_results"].as_u64().map(|n| n as usize);

                let results = code_search.search(pattern, file_type, max_results)?;
                Ok(format!("Code search results:\n\n{}", results))
            }
            "morph_edit_file" => {
                let morph = self.morph_client.as_ref().ok_or_else(|| {
                    SofosError::ToolExecution(
                        "Morph client not available. Set MORPH_API_KEY to use morph_edit_file"
                            .to_string(),
                    )
                })?;

                let path = input["path"].as_str().ok_or_else(|| {
                    SofosError::ToolExecution("Missing 'path' parameter".to_string())
                })?;
                let instruction = input["instruction"].as_str().ok_or_else(|| {
                    SofosError::ToolExecution("Missing 'instruction' parameter".to_string())
                })?;
                let code_edit = input["code_edit"].as_str().ok_or_else(|| {
                    SofosError::ToolExecution("Missing 'code_edit' parameter".to_string())
                })?;

                let original_code = self.fs_tool.read_file(path)?;

                let merged_code = morph
                    .apply_edit(instruction, &original_code, code_edit)
                    .await?;

                self.fs_tool.write_file(path, &merged_code)?;

                // Generate diff for display
                let diff_output = diff::generate_compact_diff(&original_code, &merged_code);

                Ok(format!(
                    "Successfully applied Morph edit to '{}'\n\nChanges:\n{}",
                    path, diff_output
                ))
            }
            "delete_file" => {
                let path = input["path"].as_str().ok_or_else(|| {
                    SofosError::ToolExecution("Missing 'path' parameter".to_string())
                })?;

                let confirmed = confirm_action(&format!("Delete file '{}'?", path))?;

                if !confirmed {
                    return Ok(format!(
                        "File deletion cancelled by user. The file '{}' was not deleted.",
                        path
                    ));
                }

                self.fs_tool.delete_file(path)?;
                Ok(format!("Successfully deleted file '{}'", path))
            }
            "delete_directory" => {
                let path = input["path"].as_str().ok_or_else(|| {
                    SofosError::ToolExecution("Missing 'path' parameter".to_string())
                })?;

                let confirmed = confirm_action(&format!(
                    "Delete directory '{}' and all its contents?",
                    path
                ))?;

                if !confirmed {
                    return Ok(format!(
                        "Directory deletion cancelled by user. The directory '{}' and its contents were not deleted. What would you like to do instead?",
                        path
                    ));
                }

                self.fs_tool.delete_directory(path)?;
                Ok(format!("Successfully deleted directory '{}'", path))
            }
            "move_file" => {
                let source = input["source"].as_str().ok_or_else(|| {
                    SofosError::ToolExecution("Missing 'source' parameter".to_string())
                })?;
                let destination = input["destination"].as_str().ok_or_else(|| {
                    SofosError::ToolExecution("Missing 'destination' parameter".to_string())
                })?;

                self.fs_tool.move_file(source, destination)?;
                Ok(format!(
                    "Successfully moved '{}' to '{}'",
                    source, destination
                ))
            }
            "copy_file" => {
                let source = input["source"].as_str().ok_or_else(|| {
                    SofosError::ToolExecution("Missing 'source' parameter".to_string())
                })?;
                let destination = input["destination"].as_str().ok_or_else(|| {
                    SofosError::ToolExecution("Missing 'destination' parameter".to_string())
                })?;

                self.fs_tool.copy_file(source, destination)?;
                Ok(format!(
                    "Successfully copied '{}' to '{}'",
                    source, destination
                ))
            }
            "execute_bash" => {
                let command = input["command"].as_str().ok_or_else(|| {
                    SofosError::ToolExecution("Missing 'command' parameter".to_string())
                })?;

                let result = self.bash_executor.execute(command)?;
                Ok(result)
            }
            // web_search is now handled server-side by Claude API, not by ToolExecutor
            _ => Err(SofosError::ToolExecution(format!(
                "Unknown tool: {}",
                tool_name
            ))),
        }
    }
}
