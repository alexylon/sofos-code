pub mod bashexec;
pub mod codesearch;
pub mod filesystem;
pub mod image;
pub mod permissions;
pub mod tool_name;
pub mod types;
mod utils;

use crate::api::MorphClient;
use crate::error::{Result, SofosError};
use crate::mcp::McpManager;
use crate::ui::diff;
use bashexec::BashExecutor;
use codesearch::CodeSearchTool;
use filesystem::FileSystemTool;
use permissions::PermissionManager;
use serde_json::Value;
use tool_name::ToolName;

use crate::tools::types::get_read_only_tools;
use crate::tools::utils::confirm_destructive;
pub use types::{add_code_search_tool, get_all_tools, get_all_tools_with_morph};

#[cfg(test)]
mod tests;

/// ToolExecutor handles execution of tool calls from AI
#[derive(Clone)]
pub struct ToolExecutor {
    fs_tool: FileSystemTool,
    code_search_tool: Option<CodeSearchTool>,
    bash_executor: BashExecutor,
    morph_client: Option<MorphClient>,
    mcp_manager: Option<McpManager>,
    safe_mode: bool,
}

impl ToolExecutor {
    pub fn new(
        workspace: std::path::PathBuf,
        morph_client: Option<MorphClient>,
        mcp_manager: Option<McpManager>,
        safe_mode: bool,
    ) -> Result<Self> {
        let code_search_tool = match CodeSearchTool::new(workspace.clone()) {
            Ok(tool) => Some(tool),
            Err(_) => {
                crate::ui::UI::print_warning("ripgrep not found. Code search will be unavailable.");
                None
            }
        };

        Ok(Self {
            fs_tool: FileSystemTool::new(workspace.clone())?,
            code_search_tool,
            bash_executor: BashExecutor::new(workspace)?,
            morph_client,
            mcp_manager,
            safe_mode,
        })
    }

    pub fn has_morph(&self) -> bool {
        self.morph_client.is_some()
    }

    pub fn has_code_search(&self) -> bool {
        self.code_search_tool.is_some()
    }

    pub fn set_safe_mode(&mut self, safe_mode: bool) {
        self.safe_mode = safe_mode;
    }

    pub async fn get_available_tools(&self) -> Vec<crate::api::Tool> {
        let mut tools = if self.safe_mode {
            get_read_only_tools()
        } else if self.has_morph() {
            get_all_tools_with_morph()
        } else {
            get_all_tools()
        };

        if self.has_code_search() {
            add_code_search_tool(&mut tools);
        }

        if let Some(mcp_manager) = &self.mcp_manager {
            if let Ok(mcp_tools) = mcp_manager.get_all_tools().await {
                tools.extend(mcp_tools);
            }
        }

        tools
    }

    pub async fn execute(&self, tool_name: &str, input: &Value) -> Result<String> {
        // Check if this is an MCP tool first
        if let Some(mcp_manager) = &self.mcp_manager {
            if mcp_manager.is_mcp_tool(tool_name).await {
                return mcp_manager.execute_tool(tool_name, input).await;
            }
        }

        let tool = ToolName::from_str(tool_name)?;

        match tool {
            ToolName::ReadFile => {
                let path = input["path"].as_str().ok_or_else(|| {
                    SofosError::ToolExecution("Missing 'path' parameter".to_string())
                })?;

                let permission_manager =
                    PermissionManager::new(self.fs_tool._workspace().to_path_buf())?;

                // Canonicalize to resolve symlinks and normalize relative paths
                let full_path = if path.starts_with('/') || path.starts_with('~') {
                    std::path::PathBuf::from(permissions::PermissionManager::expand_tilde_pub(path))
                } else {
                    self.fs_tool._workspace().join(path)
                };

                let canonical = match std::fs::canonicalize(&full_path) {
                    Ok(p) => p,
                    Err(_) => {
                        let parent_dir = std::path::Path::new(path)
                            .parent()
                            .and_then(|p| p.to_str())
                            .unwrap_or(".");
                        return Err(SofosError::ToolExecution(format!(
                            "File not found: '{}'. Suggestion: Use list_directory with path '{}' to see available files.",
                            path, parent_dir
                        )));
                    }
                };

                let is_inside_workspace = canonical.starts_with(self.fs_tool._workspace());
                let canonical_str = canonical.to_str().unwrap_or(path);

                // Check permissions on both original and canonical forms
                let (perm_original, matched_rule_original) =
                    permission_manager.check_read_permission_with_source(path);
                let (perm_canonical, matched_rule_canonical) =
                    permission_manager.check_read_permission_with_source(canonical_str);

                // Use the denied result if either check failed
                let (final_perm, matched_rule) =
                    if perm_original == permissions::CommandPermission::Denied {
                        (perm_original, matched_rule_original)
                    } else if perm_canonical == permissions::CommandPermission::Denied {
                        (perm_canonical, matched_rule_canonical)
                    } else if perm_original == permissions::CommandPermission::Ask {
                        (perm_original, None)
                    } else if perm_canonical == permissions::CommandPermission::Ask {
                        (perm_canonical, None)
                    } else {
                        (permissions::CommandPermission::Allowed, None)
                    };

                match final_perm {
                    permissions::CommandPermission::Denied => {
                        let config_source = if let Some(ref rule) = matched_rule {
                            permission_manager.get_rule_source(rule)
                        } else {
                            ".sofos/config.local.toml or ~/.sofos/config.toml".to_string()
                        };
                        return Err(SofosError::ToolExecution(format!(
                            "Read access denied for path '{}'\n\
                             Hint: Blocked by deny rule in {}",
                            path, config_source
                        )));
                    }
                    permissions::CommandPermission::Ask => {
                        return Err(SofosError::ToolExecution(format!(
                            "Path '{}' is in 'ask' list\n\
                             Hint: 'ask' only works for Bash commands. Use 'allow' or 'deny' for Read permissions.",
                            path
                        )));
                    }
                    permissions::CommandPermission::Allowed => {}
                }

                let is_explicit_allow =
                    permission_manager.is_read_explicit_allow_both_forms(path, canonical_str);

                if !is_inside_workspace && !is_explicit_allow {
                    return Err(SofosError::ToolExecution(format!(
                        "Path '{}' is outside workspace and not explicitly allowed\n\
                         Hint: Add Read({}) to 'allow' list in .sofos/config.local.toml",
                        path, path
                    )));
                }

                if is_inside_workspace {
                    match self.fs_tool.read_file(path) {
                        Ok(content) => Ok(format!("File content of '{}':\n\n{}", path, content)),
                        Err(e) => Err(e),
                    }
                } else {
                    match self.fs_tool.read_file_with_outside_access(canonical_str) {
                        Ok(content) => Ok(format!("File content of '{}':\n\n{}", path, content)),
                        Err(e) => Err(e),
                    }
                }
            }
            ToolName::WriteFile => {
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
            ToolName::ListDirectory => {
                let path = input["path"].as_str().ok_or_else(|| {
                    SofosError::ToolExecution("Missing 'path' parameter".to_string())
                })?;

                let permission_manager =
                    PermissionManager::new(self.fs_tool._workspace().to_path_buf())?;

                // Expand tilde and canonicalize
                let full_path = if path.starts_with('/') || path.starts_with('~') {
                    std::path::PathBuf::from(permissions::PermissionManager::expand_tilde_pub(path))
                } else {
                    self.fs_tool._workspace().join(path)
                };

                let canonical = match std::fs::canonicalize(&full_path) {
                    Ok(p) => p,
                    Err(_) => {
                        return Err(SofosError::FileNotFound(path.to_string()));
                    }
                };

                let is_inside_workspace = canonical.starts_with(self.fs_tool._workspace());
                let canonical_str = canonical.to_str().unwrap_or(path);

                // Check permissions
                let (perm_original, matched_rule_original) =
                    permission_manager.check_read_permission_with_source(path);
                let (perm_canonical, matched_rule_canonical) =
                    permission_manager.check_read_permission_with_source(canonical_str);

                let (final_perm, matched_rule) =
                    if perm_original == permissions::CommandPermission::Denied {
                        (perm_original, matched_rule_original)
                    } else if perm_canonical == permissions::CommandPermission::Denied {
                        (perm_canonical, matched_rule_canonical)
                    } else if perm_original == permissions::CommandPermission::Ask {
                        (perm_original, None)
                    } else if perm_canonical == permissions::CommandPermission::Ask {
                        (perm_canonical, None)
                    } else {
                        (permissions::CommandPermission::Allowed, None)
                    };

                match final_perm {
                    permissions::CommandPermission::Denied => {
                        let config_source = if let Some(ref rule) = matched_rule {
                            permission_manager.get_rule_source(rule)
                        } else {
                            ".sofos/config.local.toml or ~/.sofos/config.toml".to_string()
                        };
                        return Err(SofosError::ToolExecution(format!(
                            "Read access denied for path '{}'\n\
                             Hint: Blocked by deny rule in {}",
                            path, config_source
                        )));
                    }
                    permissions::CommandPermission::Ask => {
                        return Err(SofosError::ToolExecution(format!(
                            "Path '{}' is in 'ask' list\n\
                             Hint: 'ask' only works for Bash commands. Use 'allow' or 'deny' for Read permissions.",
                            path
                        )));
                    }
                    permissions::CommandPermission::Allowed => {}
                }

                let is_explicit_allow =
                    permission_manager.is_read_explicit_allow_both_forms(path, canonical_str);

                if !is_inside_workspace && !is_explicit_allow {
                    return Err(SofosError::ToolExecution(format!(
                        "Path '{}' is outside workspace and not explicitly allowed\n\
                         Hint: Add Read({}) to 'allow' list in .sofos/config.local.toml",
                        path, path
                    )));
                }

                // Use canonical path for the actual operation
                let entries = if is_inside_workspace {
                    self.fs_tool.list_directory(path)?
                } else {
                    // List using canonical path for outside workspace
                    let canonical_entries = std::fs::read_dir(&canonical)?;
                    let mut entries = Vec::new();
                    for entry in canonical_entries {
                        let entry = entry?;
                        let name = entry.file_name().to_string_lossy().to_string();
                        let is_dir = entry.file_type()?.is_dir();
                        entries.push(if is_dir { format!("{}/", name) } else { name });
                    }
                    entries.sort();
                    entries
                };

                Ok(format!("Contents of '{}':\n{}", path, entries.join("\n")))
            }
            ToolName::CreateDirectory => {
                let path = input["path"].as_str().ok_or_else(|| {
                    SofosError::ToolExecution("Missing 'path' parameter".to_string())
                })?;

                self.fs_tool.create_directory(path)?;
                Ok(format!("Successfully created directory '{}'", path))
            }
            ToolName::SearchCode => {
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
            ToolName::MorphEditFile => {
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
            ToolName::DeleteFile => {
                let path = input["path"].as_str().ok_or_else(|| {
                    SofosError::ToolExecution("Missing 'path' parameter".to_string())
                })?;

                let confirmed = confirm_destructive(&format!("Delete file '{}'?", path))?;

                if !confirmed {
                    return Ok(format!(
                        "File deletion cancelled by user. The file '{}' was not deleted.",
                        path
                    ));
                }

                self.fs_tool.delete_file(path)?;
                Ok(format!("Successfully deleted file '{}'", path))
            }
            ToolName::DeleteDirectory => {
                let path = input["path"].as_str().ok_or_else(|| {
                    SofosError::ToolExecution("Missing 'path' parameter".to_string())
                })?;

                let confirmed = confirm_destructive(&format!(
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
            ToolName::MoveFile => {
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
            ToolName::CopyFile => {
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
            ToolName::ExecuteBash => {
                let command = input["command"].as_str().ok_or_else(|| {
                    SofosError::ToolExecution("Missing 'command' parameter".to_string())
                })?;

                let result = self.bash_executor.execute(command)?;
                Ok(result)
            }
            ToolName::WebSearch => Err(SofosError::ToolExecution(
                "web_search is handled server-side by the API and should not be executed locally"
                    .to_string(),
            )),
        }
    }
}
