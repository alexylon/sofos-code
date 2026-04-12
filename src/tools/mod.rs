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
use colored::Colorize;
use filesystem::FileSystemTool;
use permissions::PermissionManager;
use serde_json::Value;
use std::time::Duration;
use tool_name::ToolName;

use crate::tools::types::get_read_only_tools;
use crate::tools::utils::confirm_destructive;
pub use types::{add_code_search_tool, get_all_tools, get_all_tools_with_morph};

use std::collections::HashSet;
use std::sync::{Arc, Mutex};

// Re-export MCP tool result types for use in response handler
pub use crate::mcp::manager::{ImageData, ToolResult as McpToolResult};

/// Result from tool execution that can contain text and/or images
#[derive(Debug, Clone)]
pub enum ToolExecutionResult {
    /// Simple text result (for most tools)
    Text(String),
    /// Structured result with optional images (for MCP tools)
    Structured(McpToolResult),
}

impl ToolExecutionResult {
    /// Get the text content
    pub fn text(&self) -> &str {
        match self {
            ToolExecutionResult::Text(s) => s,
            ToolExecutionResult::Structured(r) => &r.text,
        }
    }

    /// Check if this result has images
    #[allow(dead_code)]
    pub fn has_images(&self) -> bool {
        match self {
            ToolExecutionResult::Text(_) => false,
            ToolExecutionResult::Structured(r) => !r.images.is_empty(),
        }
    }

    /// Get images if any
    pub fn images(&self) -> &[ImageData] {
        match self {
            ToolExecutionResult::Text(_) => &[],
            ToolExecutionResult::Structured(r) => &r.images,
        }
    }
}

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
    /// Whether interactive prompts (stdin) are available (false in tests/pipes)
    interactive: bool,
    // Session-scoped path permissions for external directory access (not persisted)
    read_path_session_allowed: Arc<Mutex<HashSet<String>>>,
    read_path_session_denied: Arc<Mutex<HashSet<String>>>,
    write_path_session_allowed: Arc<Mutex<HashSet<String>>>,
    write_path_session_denied: Arc<Mutex<HashSet<String>>>,
}

impl ToolExecutor {
    pub fn new(
        workspace: std::path::PathBuf,
        morph_client: Option<MorphClient>,
        mcp_manager: Option<McpManager>,
        safe_mode: bool,
        interactive: bool,
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
            bash_executor: BashExecutor::new(workspace, interactive)?,
            morph_client,
            mcp_manager,
            safe_mode,
            interactive,
            read_path_session_allowed: Arc::new(Mutex::new(HashSet::new())),
            read_path_session_denied: Arc::new(Mutex::new(HashSet::new())),
            write_path_session_allowed: Arc::new(Mutex::new(HashSet::new())),
            write_path_session_denied: Arc::new(Mutex::new(HashSet::new())),
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

    /// Check read permissions on a path (both original and canonical forms),
    /// and verify external access. Returns Ok if allowed, Err if denied.
    fn check_read_access(
        &self,
        path: &str,
        canonical: &std::path::Path,
        canonical_str: &str,
        is_inside_workspace: bool,
    ) -> Result<()> {
        let permission_manager = PermissionManager::new(self.fs_tool._workspace().to_path_buf())?;

        let (perm_original, matched_rule_original) =
            permission_manager.check_read_permission_with_source(path);
        let (perm_canonical, matched_rule_canonical) =
            permission_manager.check_read_permission_with_source(canonical_str);

        let (final_perm, matched_rule) = if perm_original == permissions::CommandPermission::Denied
        {
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

        if !is_inside_workspace {
            // Use ONLY the canonical (symlink-resolved) path for permission checks
            // to prevent symlink escape attacks
            let is_explicit_allow = permission_manager.is_read_explicit_allow(canonical_str);
            if !is_explicit_allow {
                let dir_to_grant = if canonical.is_dir() {
                    canonical_str.to_string()
                } else {
                    canonical
                        .parent()
                        .and_then(|p| p.to_str())
                        .unwrap_or(canonical_str)
                        .to_string()
                };
                self.check_external_path_access(
                    "Read",
                    canonical_str,
                    &dir_to_grant,
                    &self.read_path_session_allowed,
                    &self.read_path_session_denied,
                )?;
            }
        }

        Ok(())
    }

    /// Check Write permissions for an external path: enforce deny rules, then check allow/ask.
    fn check_write_access(
        &self,
        path: &str,
        canonical_str: &str,
        canonical: &std::path::Path,
    ) -> Result<()> {
        let permission_manager = PermissionManager::new(self.fs_tool._workspace().to_path_buf())?;

        // Enforce Write deny rules first
        if permission_manager.check_write_permission(canonical_str)
            == permissions::CommandPermission::Denied
        {
            return Err(SofosError::ToolExecution(format!(
                "Write access denied for path '{}'\n\
                 Hint: Blocked by deny rule in .sofos/config.local.toml or ~/.sofos/config.toml",
                path
            )));
        }

        // Check explicit allow (canonical only, for symlink safety)
        let is_explicit_allow = permission_manager.is_write_explicit_allow(canonical_str);
        if !is_explicit_allow {
            let dir_to_grant = canonical
                .parent()
                .and_then(|p| p.to_str())
                .unwrap_or(canonical_str);
            self.check_external_path_access(
                "Write",
                canonical_str,
                dir_to_grant,
                &self.write_path_session_allowed,
                &self.write_path_session_denied,
            )?;
        }

        Ok(())
    }

    /// Check if an external path is allowed for the given scope, asking the user if needed.
    /// Returns Ok(()) if access is granted, Err if denied.
    fn check_external_path_access(
        &self,
        scope: &str,
        canonical_path: &str,
        dir_to_grant: &str,
        session_allowed: &Arc<Mutex<HashSet<String>>>,
        session_denied: &Arc<Mutex<HashSet<String>>>,
    ) -> Result<()> {
        let path_obj = std::path::Path::new(canonical_path);

        // Check session allowed
        if let Ok(allowed_dirs) = session_allowed.lock() {
            for dir in allowed_dirs.iter() {
                if path_obj.starts_with(std::path::Path::new(dir)) {
                    return Ok(());
                }
            }
        }

        // Check session denied
        if let Ok(denied_dirs) = session_denied.lock() {
            for dir in denied_dirs.iter() {
                if path_obj.starts_with(std::path::Path::new(dir)) {
                    return Err(SofosError::ToolExecution(format!(
                        "{} access denied for '{}' (denied earlier this session)",
                        scope, canonical_path
                    )));
                }
            }
        }

        // Non-interactive mode (tests, piped input): deny with a config hint
        if !self.interactive {
            return Err(SofosError::ToolExecution(format!(
                "Path '{}' is outside workspace and not explicitly allowed\n\
                 Hint: Add {}({}/**) to 'allow' list in .sofos/config.local.toml",
                canonical_path, scope, dir_to_grant
            )));
        }

        // Ask user interactively
        let mut pm = PermissionManager::new(self.fs_tool._workspace().to_path_buf())?;
        let (allowed, remember) = pm.ask_user_path_permission(scope, dir_to_grant)?;

        if allowed {
            if !remember {
                if let Ok(mut dirs) = session_allowed.lock() {
                    dirs.insert(dir_to_grant.to_string());
                }
            }
            Ok(())
        } else {
            if !remember {
                if let Ok(mut dirs) = session_denied.lock() {
                    dirs.insert(dir_to_grant.to_string());
                }
            }
            Err(SofosError::ToolExecution(format!(
                "{} access denied by user for '{}'",
                scope, canonical_path
            )))
        }
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

    pub async fn execute(&self, tool_name: &str, input: &Value) -> Result<ToolExecutionResult> {
        // Check if this is an MCP tool first
        if let Some(mcp_manager) = &self.mcp_manager {
            if mcp_manager.is_mcp_tool(tool_name).await {
                let result = mcp_manager.execute_tool(tool_name, input).await?;
                return Ok(ToolExecutionResult::Structured(result));
            }
        }

        let tool = ToolName::from_str(tool_name)?;

        let text_result = match tool {
            ToolName::ReadFile => {
                let path = input["path"].as_str().ok_or_else(|| {
                    SofosError::ToolExecution("Missing 'path' parameter".to_string())
                })?;

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

                self.check_read_access(path, &canonical, canonical_str, is_inside_workspace)?;

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

                // Check if path is outside workspace
                let full_path = if path.starts_with('/') || path.starts_with('~') {
                    std::path::PathBuf::from(permissions::PermissionManager::expand_tilde_pub(path))
                } else {
                    self.fs_tool._workspace().join(path)
                };

                // For new files, canonicalize parent; for existing files, canonicalize directly
                let (is_inside_workspace, canonical_str_owned) = if full_path.exists() {
                    let canonical = std::fs::canonicalize(&full_path).map_err(|e| {
                        SofosError::ToolExecution(format!("Failed to resolve path: {}", e))
                    })?;
                    let inside = canonical.starts_with(self.fs_tool._workspace());
                    let cs = canonical.to_string_lossy().to_string();
                    (inside, cs)
                } else if let Some(parent) = full_path.parent() {
                    if parent.exists() {
                        let canonical_parent = std::fs::canonicalize(parent).map_err(|e| {
                            SofosError::ToolExecution(format!("Failed to resolve path: {}", e))
                        })?;
                        let inside = canonical_parent.starts_with(self.fs_tool._workspace());
                        let filename = full_path
                            .file_name()
                            .map(|f| f.to_string_lossy().to_string())
                            .unwrap_or_default();
                        let cs = format!("{}/{}", canonical_parent.display(), filename);
                        (inside, cs)
                    } else {
                        // Parent doesn't exist — assume outside if path is absolute
                        let inside = !path.starts_with('/') && !path.starts_with('~');
                        (inside, full_path.to_string_lossy().to_string())
                    }
                } else {
                    (true, full_path.to_string_lossy().to_string())
                };

                if !is_inside_workspace {
                    let canonical_path = std::path::Path::new(&canonical_str_owned);
                    self.check_write_access(path, &canonical_str_owned, canonical_path)?;
                }

                let original_content = if is_inside_workspace {
                    self.fs_tool.read_file(path).ok()
                } else {
                    self.fs_tool
                        .read_file_with_outside_access(&canonical_str_owned)
                        .ok()
                };

                if is_inside_workspace {
                    self.fs_tool.write_file(path, content)?;
                } else {
                    self.fs_tool
                        .write_file_with_outside_access(&canonical_str_owned, content)?;
                }

                if let Some(original) = original_content {
                    let diff_output = diff::generate_compact_diff(&original, content, path);
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

                self.check_read_access(path, &canonical, canonical_str, is_inside_workspace)?;

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
            ToolName::GlobFiles => {
                let pattern = input["pattern"].as_str().ok_or_else(|| {
                    SofosError::ToolExecution("Missing 'pattern' parameter".to_string())
                })?;
                let base = input["path"].as_str().unwrap_or(".");

                let search_dir = self.fs_tool._workspace().join(base);
                if !search_dir.exists() {
                    return Err(SofosError::FileNotFound(base.to_string()));
                }

                let glob = globset::GlobBuilder::new(pattern)
                    .literal_separator(false)
                    .build()
                    .map_err(|e| SofosError::ToolExecution(format!("Invalid glob pattern: {}", e)))?
                    .compile_matcher();

                let mut matches = Vec::new();
                let mut stack = vec![search_dir.clone()];

                while let Some(dir) = stack.pop() {
                    let entries = match std::fs::read_dir(&dir) {
                        Ok(e) => e,
                        Err(_) => continue,
                    };
                    for entry in entries.flatten() {
                        let path = entry.path();
                        if let Ok(rel) = path.strip_prefix(&search_dir) {
                            let rel_str = rel.to_string_lossy();
                            if path.is_dir() {
                                stack.push(path);
                            } else if glob.is_match(rel_str.as_ref()) {
                                matches.push(rel_str.to_string());
                            }
                        }
                    }
                }

                matches.sort();

                if matches.is_empty() {
                    Ok(format!("No files matching '{}' in '{}'", pattern, base))
                } else {
                    Ok(format!(
                        "Found {} file(s) matching '{}':\n{}",
                        matches.len(),
                        pattern,
                        matches.join("\n")
                    ))
                }
            }
            ToolName::EditFile => {
                let path = input["path"].as_str().ok_or_else(|| {
                    SofosError::ToolExecution("Missing 'path' parameter".to_string())
                })?;
                let old_string = input["old_string"].as_str().ok_or_else(|| {
                    SofosError::ToolExecution("Missing 'old_string' parameter".to_string())
                })?;
                let new_string = input["new_string"].as_str().ok_or_else(|| {
                    SofosError::ToolExecution("Missing 'new_string' parameter".to_string())
                })?;
                let replace_all = input["replace_all"].as_bool().unwrap_or(false);

                // Guard against truncation markers from conversation history compaction
                let truncation_markers = [
                    "...[truncated",
                    "// ... existing code ...",
                    "/* ... existing code ... */",
                    "# ... existing code ...",
                ];
                for marker in &truncation_markers {
                    if old_string.contains(marker) {
                        return Err(SofosError::ToolExecution(format!(
                            "old_string contains a truncation marker '{}'. This is not real file content. \
                             Use read_file to get the actual current content of '{}' before editing.",
                            marker, path
                        )));
                    }
                    if new_string.contains(marker) {
                        return Err(SofosError::ToolExecution(format!(
                            "new_string contains a truncation marker '{}'. You must provide the complete \
                             replacement text, not abbreviated content. Use read_file to get the current \
                             content of '{}' if needed.",
                            marker, path
                        )));
                    }
                }

                // Check if path is outside workspace
                let full_path = if path.starts_with('/') || path.starts_with('~') {
                    std::path::PathBuf::from(permissions::PermissionManager::expand_tilde_pub(path))
                } else {
                    self.fs_tool._workspace().join(path)
                };

                let canonical = std::fs::canonicalize(&full_path).map_err(|_| {
                    SofosError::ToolExecution(format!(
                        "File not found: '{}'. The file must exist to edit it.",
                        path
                    ))
                })?;
                let is_inside_workspace = canonical.starts_with(self.fs_tool._workspace());
                let canonical_str = canonical.to_string_lossy().to_string();

                if !is_inside_workspace {
                    self.check_write_access(path, &canonical_str, &canonical)?;
                }

                let original = if is_inside_workspace {
                    self.fs_tool.read_file(path)?
                } else {
                    self.fs_tool.read_file_with_outside_access(&canonical_str)?
                };

                if !original.contains(old_string) {
                    return Err(SofosError::ToolExecution(format!(
                        "old_string not found in '{}'. Make sure it matches the file content exactly, \
                         including whitespace and indentation. Use read_file first to see the current content.",
                        path
                    )));
                }

                let modified = if replace_all {
                    original.replace(old_string, new_string)
                } else {
                    original.replacen(old_string, new_string, 1)
                };

                if is_inside_workspace {
                    self.fs_tool.write_file(path, &modified)?;
                } else {
                    self.fs_tool
                        .write_file_with_outside_access(&canonical_str, &modified)?;
                }

                let diff_output = diff::generate_compact_diff(&original, &modified, path);
                Ok(format!(
                    "Successfully edited '{}'\n\nChanges:\n{}",
                    path, diff_output
                ))
            }
            ToolName::MorphEditFile => {
                let morph = self.morph_client.as_ref().ok_or_else(|| {
                    SofosError::ToolExecution(
                        "Morph client not available. Set MORPH_API_KEY to use morph_edit_file"
                            .to_string(),
                    )
                })?;

                // Canonical schema (Morph docs) is `target_filepath` /
                // `instructions` / `code_edit`. Accept legacy `path` /
                // `instruction` (and a few common typos) so older
                // conversation history and models that diverge keep working.
                let path = input["target_filepath"]
                    .as_str()
                    .or_else(|| input["path"].as_str())
                    .or_else(|| input["file_path"].as_str())
                    .or_else(|| input["file"].as_str())
                    .ok_or_else(|| {
                        SofosError::ToolExecution(format!(
                            "Missing 'target_filepath' parameter. Got keys: {:?}. \
                             Please retry with the 'target_filepath' parameter set to the file path.",
                            input
                                .as_object()
                                .map(|o| o.keys().collect::<Vec<_>>())
                                .unwrap_or_default()
                        ))
                    })?;
                let instruction = input["instructions"]
                    .as_str()
                    .or_else(|| input["instruction"].as_str())
                    .ok_or_else(|| {
                        SofosError::ToolExecution("Missing 'instructions' parameter".to_string())
                    })?;
                let code_edit = input["code_edit"].as_str().ok_or_else(|| {
                    SofosError::ToolExecution("Missing 'code_edit' parameter".to_string())
                })?;

                // Guard against truncation markers from conversation history compaction
                if code_edit.contains("...[truncated") {
                    return Err(SofosError::ToolExecution(format!(
                        "code_edit contains a truncation marker '...[truncated'. This is not real code. \
                         Use read_file to get the actual current content of '{}' before editing.",
                        path
                    )));
                }

                // Check if path is outside workspace for external access
                let full_path = if path.starts_with('/') || path.starts_with('~') {
                    std::path::PathBuf::from(permissions::PermissionManager::expand_tilde_pub(path))
                } else {
                    self.fs_tool._workspace().join(path)
                };
                let canonical = std::fs::canonicalize(&full_path).map_err(|_| {
                    SofosError::ToolExecution(format!(
                        "File not found: '{}'. The file must exist for morph_edit_file.",
                        path
                    ))
                })?;
                let is_inside_workspace = canonical.starts_with(self.fs_tool._workspace());
                let canonical_str = canonical.to_string_lossy().to_string();

                if !is_inside_workspace {
                    self.check_write_access(path, &canonical_str, &canonical)?;
                }

                let original_code = if is_inside_workspace {
                    self.fs_tool.read_file(path)?
                } else {
                    self.fs_tool.read_file_with_outside_access(&canonical_str)?
                };

                // Wrap morph API call with a timeout; fall back to edit_file on timeout/network errors
                let morph_timeout = Duration::from_secs(30);
                let merged_code = match tokio::time::timeout(
                    morph_timeout,
                    morph.apply_edit(instruction, &original_code, code_edit),
                )
                .await
                {
                    Ok(Ok(code)) => code,
                    Ok(Err(SofosError::NetworkError(msg))) => {
                        eprintln!(
                            "  {} Morph API failed ({}), use edit_file instead",
                            "⚠".bright_yellow(),
                            msg
                        );
                        return Ok(ToolExecutionResult::Text(format!(
                            "morph_edit_file failed ({}). The file '{}' was NOT modified. \
                             Please use read_file to get the current file content, then use edit_file \
                             with exact old_string/new_string to make this change.",
                            msg, path
                        )));
                    }
                    Err(_elapsed) => {
                        eprintln!(
                            "  {} Morph API timed out after {}s, use edit_file instead",
                            "⚠".bright_yellow(),
                            morph_timeout.as_secs()
                        );
                        return Ok(ToolExecutionResult::Text(format!(
                            "morph_edit_file timed out after {}s. The file '{}' was NOT modified. \
                             Please use read_file to get the current file content, then use edit_file \
                             with exact old_string/new_string to make this change.",
                            morph_timeout.as_secs(),
                            path
                        )));
                    }
                    Ok(Err(e)) => return Err(e),
                };

                if is_inside_workspace {
                    self.fs_tool.write_file(path, &merged_code)?;
                } else {
                    self.fs_tool
                        .write_file_with_outside_access(&canonical_str, &merged_code)?;
                }

                // Generate diff for display
                let diff_output = diff::generate_compact_diff(&original_code, &merged_code, path);

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
                    return Ok(ToolExecutionResult::Text(format!(
                        "File deletion cancelled by user. The file '{}' was not deleted.",
                        path
                    )));
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
                    return Ok(ToolExecutionResult::Text(format!(
                        "Directory deletion cancelled by user. The directory '{}' and its contents were not deleted. What would you like to do instead?",
                        path
                    )));
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
            ToolName::WebFetch => {
                let url = input["url"].as_str().ok_or_else(|| {
                    SofosError::ToolExecution("Missing 'url' parameter".to_string())
                })?;

                if !url.starts_with("http://") && !url.starts_with("https://") {
                    return Err(SofosError::ToolExecution(
                        "URL must start with http:// or https://".to_string(),
                    ));
                }

                let client = reqwest::Client::builder()
                    .timeout(std::time::Duration::from_secs(30))
                    .build()
                    .map_err(|e| SofosError::ToolExecution(format!("HTTP client error: {}", e)))?;

                let response = client
                    .get(url)
                    .header("User-Agent", "Sofos/1.0")
                    .send()
                    .await
                    .map_err(|e| SofosError::ToolExecution(format!("Fetch failed: {}", e)))?;

                let status = response.status();
                if !status.is_success() {
                    return Err(SofosError::ToolExecution(format!(
                        "HTTP {} for {}",
                        status, url
                    )));
                }

                let body = response
                    .text()
                    .await
                    .map_err(|e| SofosError::ToolExecution(format!("Read body failed: {}", e)))?;

                let text = utils::html_to_text(&body);

                let max_bytes = 64_000;
                let truncated = if text.len() > max_bytes {
                    let end = crate::api::utils::truncate_at_char_boundary(&text, max_bytes);
                    format!(
                        "{}\n\n[TRUNCATED: showing first ~{} chars of {}]",
                        &text[..end],
                        max_bytes,
                        text.len()
                    )
                } else {
                    text
                };

                Ok(format!("Content from {}:\n\n{}", url, truncated))
            }
            ToolName::WebSearch => Err(SofosError::ToolExecution(
                "web_search is handled server-side by the API and should not be executed locally"
                    .to_string(),
            )),
        };

        Ok(ToolExecutionResult::Text(text_result?))
    }
}
