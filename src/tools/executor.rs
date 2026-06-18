use crate::api::MorphClient;
use crate::config::SandboxMode;
use crate::error::{DEFAULT_PARENT_DIR, Result, SofosError};
use crate::mcp::McpManager;
use crate::mcp::manager::{ImageData, ToolResult as McpToolResult};
use crate::tools::ToolName;
use crate::tools::bash::BashExecutor;
use crate::tools::codesearch::CodeSearchTool;
use crate::tools::filesystem::FileSystemTool;
use crate::tools::image::ImageLoader;
use crate::tools::morph_validate;
use crate::tools::permissions::{self, PermissionManager};
use crate::tools::plan;
use crate::tools::resolve::ResolvedPath;
use crate::tools::types::{
    add_code_search_tool, get_all_tools, get_all_tools_with_morph, get_read_only_tools,
};
use crate::tools::utils::{
    MAX_DIFF_TOKENS, MAX_FILE_READ_TOKENS, MAX_MCP_IMAGE_BYTES, MAX_MCP_IMAGE_COUNT,
    MAX_MCP_OUTPUT_TOKENS, MAX_PATH_LIST_TOKENS, TruncationKind, base64_approx_decoded_kb,
    confirm_destructive, is_http_url, truncate_for_context,
};
use crate::ui::diff;
use colored::Colorize;
use serde_json::Value;
use std::collections::HashSet;
use std::sync::{Arc, Mutex};
use std::time::Duration;

const SOFOS_USER_AGENT: &str = concat!("Sofos/", env!("CARGO_PKG_VERSION"));

/// Hard cap on the raw HTTP body `web_fetch` will accept. The
/// post-fetch pipeline runs HTML stripping and truncates to ~64 KB of
/// text before anything reaches the model, so the cap only exists to
/// bound how much memory a single tool call can pull in. Eight
/// megabytes is enough for almost every real article and small enough
/// that the streaming HTML pass stays cheap.
const MAX_WEB_FETCH_BODY_BYTES: usize = 8 * 1024 * 1024;

/// Byte cap on the text produced by `html_to_text` for `web_fetch`.
/// Sized for a small headroom over the eventual model truncation
/// budget so we never copy bytes that would be dropped anyway.
const WEB_FETCH_TEXT_BUDGET_BYTES: usize = 128 * 1024;

/// Result from tool execution that can contain text and/or images
#[derive(Debug, Clone)]
pub enum ToolExecutionResult {
    /// Simple text result (for most tools)
    Text(String),
    /// Separate text for the model vs. the on-screen display. The model
    /// gets a short summary; the user sees the full rendered output
    /// (e.g. a colored diff). Keeps the tool-result payload small in
    /// conversation history without losing the visual diff in the TUI.
    TextWithDisplay { text: String, display: String },
    /// Structured result with optional images (for MCP tools)
    Structured(McpToolResult),
}

impl ToolExecutionResult {
    /// Text shipped back to the model as the tool result.
    pub fn text(&self) -> &str {
        match self {
            ToolExecutionResult::Text(s) => s,
            ToolExecutionResult::TextWithDisplay { text, .. } => text,
            ToolExecutionResult::Structured(r) => &r.text,
        }
    }

    /// Text rendered to the user in the TUI / session replay. Falls back
    /// to the model-facing text for tools that don't draw a distinction.
    pub fn display_text(&self) -> &str {
        match self {
            ToolExecutionResult::Text(s) => s,
            ToolExecutionResult::TextWithDisplay { display, .. } => display,
            ToolExecutionResult::Structured(r) => &r.text,
        }
    }

    /// Get images if any
    pub fn images(&self) -> &[ImageData] {
        match self {
            ToolExecutionResult::Text(_) => &[],
            ToolExecutionResult::TextWithDisplay { .. } => &[],
            ToolExecutionResult::Structured(r) => &r.images,
        }
    }
}

fn resolve_view_image_candidate(workspace: &std::path::Path, path: &str) -> std::path::PathBuf {
    if crate::tools::utils::is_absolute_or_tilde(path) {
        std::path::PathBuf::from(PermissionManager::expand_tilde_pub(path))
    } else {
        workspace.join(path)
    }
}

impl From<crate::tools::image::ImageSource> for ImageData {
    fn from(source: crate::tools::image::ImageSource) -> Self {
        match source {
            crate::tools::image::ImageSource::Base64 { media_type, data } => ImageData::Base64 {
                mime_type: media_type,
                data,
            },
            crate::tools::image::ImageSource::Url { url } => ImageData::Url { url },
        }
    }
}

/// Header line of the model-facing summary every file-modification tool
/// emits. Mirrors a unified-diff "files changed" preamble: a fixed first
/// line followed by per-file lines tagged `A` (added), `M` (modified),
/// or `D` (deleted).
const FILE_MUTATION_SUMMARY_HEADER: &str = "Success. Updated the following files:";

/// Build a [`ToolExecutionResult`] for a file-modification tool that
/// wants to keep the user's colored diff while shipping a constant-size
/// summary to the model. The colored diff carries syntax-highlighting
/// ANSI that roughly multiplies the byte count per line, and every tool
/// result stays in conversation history for the rest of the session —
/// echoing the diff back to the model dominates the cost of repeated
/// edits. Returning only `M <path>` to the model keeps that cost flat
/// regardless of edit size; the model can `read_file` a range if it
/// needs to inspect the post-edit state.
fn file_modification_result(
    path: &str,
    original: &str,
    modified: &str,
    success_prefix: &str,
) -> ToolExecutionResult {
    let diff_output = diff::generate_compact_diff(original, modified, path);
    let display_body = format!("{} '{}'\n\nChanges:\n{}", success_prefix, path, diff_output);
    let display = truncate_for_context(&display_body, MAX_DIFF_TOKENS, TruncationKind::DiffOutput);
    let summary = format!("{FILE_MUTATION_SUMMARY_HEADER}\nM {path}");
    ToolExecutionResult::TextWithDisplay {
        text: summary,
        display,
    }
}

/// ToolExecutor handles execution of tool calls from AI
#[derive(Clone)]
pub struct ToolExecutor {
    pub(super) fs_tool: FileSystemTool,
    code_search_tool: Option<CodeSearchTool>,
    bash_executor: BashExecutor,
    morph_client: Option<MorphClient>,
    mcp_manager: Option<McpManager>,
    image_loader: Arc<ImageLoader>,
    mode: SandboxMode,
    /// Whether interactive prompts (stdin) are available (false in tests/pipes)
    interactive: bool,
    // Not persisted across sessions.
    read_path_session_allowed: Arc<Mutex<HashSet<String>>>,
    read_path_session_denied: Arc<Mutex<HashSet<String>>>,
    write_path_session_allowed: Arc<Mutex<HashSet<String>>>,
    write_path_session_denied: Arc<Mutex<HashSet<String>>>,
}

/// Apply both MCP-response caps (image count/bytes and text tokens) in
/// the order the dispatcher needs. The drop note for images has to land
/// AFTER text truncation so the model always sees it — otherwise a
/// ~1 MB text response could push the note out past the truncation
/// boundary and the model would silently lose the "images dropped"
/// signal. The overage this adds to the text field is ~100 bytes,
/// immaterial compared with the 10 MB API ceiling this cap is really
/// protecting against. Factored out from the `execute` dispatcher so
/// the ordering can be pinned in unit tests.
pub(super) fn cap_mcp_response(result: &mut McpToolResult) {
    let dropped = cap_mcp_images(result);
    result.text = truncate_for_context(
        &result.text,
        MAX_MCP_OUTPUT_TOKENS,
        TruncationKind::McpOutput,
    );
    if dropped > 0 {
        result.text.push_str(&format!(
            "\n\n[{} image attachment(s) dropped: MCP image cap is {} images or ~{} MB total]",
            dropped,
            MAX_MCP_IMAGE_COUNT,
            MAX_MCP_IMAGE_BYTES / (1024 * 1024)
        ));
    }
}

/// Drop image attachments from an MCP result until both the per-call
/// count cap and the total-bytes cap are satisfied. Returns the number
/// of images that were dropped. Walks the list in order and keeps each
/// image that still fits under both caps — so a single oversized image
/// in the middle of the response is skipped without blocking smaller
/// images that come after it. Kept images retain their original order.
pub(super) fn cap_mcp_images(result: &mut McpToolResult) -> usize {
    let original = result.images.len();
    let mut kept = Vec::with_capacity(result.images.len().min(MAX_MCP_IMAGE_COUNT));
    let mut total_bytes: usize = 0;
    for img in std::mem::take(&mut result.images) {
        let size = img.outbound_size();
        if kept.len() >= MAX_MCP_IMAGE_COUNT
            || total_bytes.saturating_add(size) > MAX_MCP_IMAGE_BYTES
        {
            continue;
        }
        total_bytes += size;
        kept.push(img);
    }
    result.images = kept;
    original - result.images.len()
}

/// Ensure `path`'s parent directory exists, creating it (and any missing
/// intermediates) if not. Used by move/copy when the destination is
/// outside the workspace — inside-workspace writes go through
/// `FileSystemTool::move_file` / `copy_file`, which already handle this.
fn ensure_parent_dir(path: &std::path::Path) -> Result<()> {
    if let Some(parent) = path.parent() {
        if !parent.exists() {
            std::fs::create_dir_all(parent).map_err(|e| {
                SofosError::ToolExecution(format!(
                    "Failed to create destination parent '{}': {}",
                    parent.display(),
                    e
                ))
            })?;
        }
    }
    Ok(())
}

/// Rename `src` → `dst` handling the four inside/outside combinations.
/// When both paths are inside the workspace we delegate to the
/// workspace-sandboxed `FileSystemTool::move_file`; any other combination
/// (inside→outside, outside→inside, outside→outside) uses `std::fs::rename`
/// on the canonical paths after the dispatcher has already verified the
/// required Read / Write grants.
fn move_between(
    source: &str,
    destination: &str,
    src: &ResolvedPath,
    dst: &ResolvedPath,
    fs_tool: &FileSystemTool,
) -> Result<()> {
    if src.is_inside_workspace && dst.is_inside_workspace {
        return fs_tool.move_file(source, destination);
    }
    ensure_parent_dir(&dst.canonical)?;
    crate::tools::filesystem::rename_with_cross_device_fallback(&src.canonical, &dst.canonical)
        .map_err(|e| {
            SofosError::ToolExecution(format!(
                "Failed to move '{}' to '{}': {}",
                source, destination, e
            ))
        })
}

/// Copy-file counterpart to [`move_between`]. Same inside/outside matrix;
/// uses `FileSystemTool::copy_file` when both are inside, `std::fs::copy`
/// otherwise.
fn copy_between(
    source: &str,
    destination: &str,
    src: &ResolvedPath,
    dst: &ResolvedPath,
    fs_tool: &FileSystemTool,
) -> Result<()> {
    if src.is_inside_workspace && dst.is_inside_workspace {
        return fs_tool.copy_file(source, destination);
    }
    ensure_parent_dir(&dst.canonical)?;
    std::fs::copy(&src.canonical, &dst.canonical)
        .map(|_| ())
        .map_err(|e| {
            SofosError::ToolExecution(format!(
                "Failed to copy '{}' to '{}': {}",
                source, destination, e
            ))
        })
}

impl ToolExecutor {
    pub fn new(
        workspace: std::path::PathBuf,
        morph_client: Option<MorphClient>,
        mcp_manager: Option<McpManager>,
        mode: SandboxMode,
        interactive: bool,
    ) -> Result<Self> {
        let code_search_tool = match CodeSearchTool::new(workspace.clone()) {
            Ok(tool) => Some(tool),
            Err(_) => {
                crate::ui::UI::print_warning("ripgrep not found. Code search will be unavailable.");
                None
            }
        };

        let has_morph = morph_client.is_some();
        let read_path_session_allowed = Arc::new(Mutex::new(HashSet::new()));
        let read_path_session_denied = Arc::new(Mutex::new(HashSet::new()));

        // Share read-session caches so a Read grant covers view_image too.
        let mut image_loader = ImageLoader::new(workspace.clone())?;
        image_loader.install_read_path_session(
            interactive,
            Arc::clone(&read_path_session_allowed),
            Arc::clone(&read_path_session_denied),
        );

        let mut bash_executor = BashExecutor::new(workspace.clone(), interactive, has_morph)?;
        bash_executor.set_sandbox_mode(mode);

        Ok(Self {
            fs_tool: FileSystemTool::new(workspace.clone())?,
            code_search_tool,
            bash_executor,
            morph_client,
            mcp_manager,
            image_loader: Arc::new(image_loader),
            mode,
            interactive,
            read_path_session_allowed,
            read_path_session_denied,
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

    pub fn set_mode(&mut self, mode: SandboxMode) {
        self.mode = mode;
        // The bash executor keeps its own copy of the mode to drive
        // sandbox confinement, so a runtime `/permissions` switch must
        // reach it too.
        self.bash_executor.set_sandbox_mode(mode);
    }

    /// Push the approval policy down to the bash executor, which owns the
    /// escalation behaviour. The REPL calls this at startup and whenever a
    /// `/permissions` sandboxed preset changes the policy mid-session.
    pub fn set_approval_policy(&mut self, policy: crate::config::ApprovalPolicy) {
        self.bash_executor.set_approval_policy(policy);
    }

    /// Names of MCP servers whose tools would be filtered out when
    /// read-only mode is on. Returned regardless of the current mode
    /// so the REPL can decide what to print at startup.
    pub fn mcp_servers_excluded_from_readonly(&self) -> Vec<String> {
        self.mcp_manager
            .as_ref()
            .map(|m| m.server_names_for_readonly(false))
            .unwrap_or_default()
    }

    /// Names of MCP servers that opted into read-only mode through their
    /// configuration.
    pub fn mcp_servers_included_in_readonly(&self) -> Vec<String> {
        self.mcp_manager
            .as_ref()
            .map(|m| m.server_names_for_readonly(true))
            .unwrap_or_default()
    }

    /// Share the REPL's interrupt flag with the bash executor so that
    /// pressing ESC or Ctrl+C during a turn terminates a running
    /// shell command instead of waiting for it to exit on its own.
    pub fn install_interrupt_flag(&mut self, flag: Arc<std::sync::atomic::AtomicBool>) {
        self.bash_executor.install_interrupt_flag(flag);
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
        let permission_manager = PermissionManager::new(self.fs_tool.workspace().to_path_buf())?;

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
                    permissions::grant_dir_for_path(canonical).to_string()
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
        let permission_manager = PermissionManager::new(self.fs_tool.workspace().to_path_buf())?;

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
            let dir_to_grant = permissions::grant_dir_for_path(canonical);
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

    /// Check if an external path is allowed for the given scope, asking
    /// the user if needed. Thin wrapper that forwards to the shared
    /// `permissions::check_external_path_session_access` so the same
    /// allow / deny sets cover file reads, file writes, bash arguments,
    /// and image loading.
    fn check_external_path_access(
        &self,
        scope: &str,
        canonical_path: &str,
        dir_to_grant: &str,
        session_allowed: &Arc<Mutex<HashSet<String>>>,
        session_denied: &Arc<Mutex<HashSet<String>>>,
    ) -> Result<()> {
        permissions::check_external_path_session_access(
            self.fs_tool.workspace(),
            scope,
            canonical_path,
            dir_to_grant,
            self.interactive,
            session_allowed,
            session_denied,
        )
    }

    pub async fn get_available_tools(&self) -> Vec<crate::api::Tool> {
        let mut tools = if self.mode.is_readonly() {
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
            let mcp_tools = if self.mode.is_readonly() {
                mcp_manager.get_readonly_tools().await
            } else {
                mcp_manager.get_all_tools().await
            };
            if let Ok(mcp_tools) = mcp_tools {
                tools.extend(mcp_tools);
            }
        }

        tools
    }

    pub async fn execute(&self, tool_name: &str, input: &Value) -> Result<ToolExecutionResult> {
        // Check if this is an MCP tool first
        if let Some(mcp_manager) = &self.mcp_manager {
            if mcp_manager.is_mcp_tool(tool_name) {
                if self.mode.is_readonly() {
                    if let Some(server) = mcp_manager.server_for_tool(tool_name) {
                        if !mcp_manager.is_server_available_in_readonly(server) {
                            return Err(SofosError::ToolExecution(format!(
                                "MCP tool '{}' is filtered out in read-only mode because its server is not marked for read-only access.",
                                tool_name
                            )));
                        }
                    }
                }
                let mut result = mcp_manager.execute_tool(tool_name, input).await?;
                cap_mcp_response(&mut result);
                return Ok(ToolExecutionResult::Structured(result));
            }
        }

        let tool = ToolName::from_str(tool_name)?;

        let text_result = match tool {
            ToolName::ReadFile => {
                let path = input["path"].as_str().ok_or_else(|| {
                    SofosError::ToolExecution("Missing 'path' parameter".to_string())
                })?;

                let resolved = self.resolve_existing(path).map_err(|_| {
                    let parent_dir = std::path::Path::new(path)
                        .parent()
                        .and_then(|p| p.to_str())
                        .unwrap_or(DEFAULT_PARENT_DIR);
                    SofosError::ToolExecution(format!(
                        "File not found: '{}'. Suggestion: Use list_directory with path '{}' to see available files.",
                        path, parent_dir
                    ))
                })?;

                self.check_read_access(
                    path,
                    &resolved.canonical,
                    &resolved.canonical_str,
                    resolved.is_inside_workspace,
                )?;

                // Read raw file contents, then apply the model-facing
                // truncation cap here at the dispatcher. Truncation lives
                // in this layer (not inside `fs_tool.read_file`) so that
                // `edit_file` / `morph_edit_file` / the `write_file` diff
                // path — which all call the same fs_tool method — get the
                // full file and don't silently drop the tail past ~64 KB.
                let raw = if resolved.is_inside_workspace {
                    self.fs_tool.read_file(path)?
                } else {
                    self.fs_tool
                        .read_file_with_outside_access(&resolved.canonical_str)?
                };
                let content =
                    truncate_for_context(&raw, MAX_FILE_READ_TOKENS, TruncationKind::File);
                Ok(crate::tools::format_read_file_output(path, &content))
            }
            ToolName::WriteFile => {
                // Accept common parameter-name variations. OpenAI
                // models occasionally emit `file_path` / `file` /
                // `filename` (especially when the tool-argument JSON
                // gets repaired from a truncated payload), and
                // `text` / `body` / `data` in place of `content`.
                // Failing the call with a bare "missing parameter"
                // message forces the model to re-plan from scratch;
                // accepting the aliases lets the call proceed, and
                // when nothing matches we echo the keys that WERE
                // supplied so the model can self-correct.
                let path = input["path"]
                    .as_str()
                    .or_else(|| input["file_path"].as_str())
                    .or_else(|| input["file"].as_str())
                    .or_else(|| input["filepath"].as_str())
                    .or_else(|| input["filename"].as_str())
                    .ok_or_else(|| {
                        let keys: Vec<&String> = input
                            .as_object()
                            .map(|o| o.keys().collect())
                            .unwrap_or_default();
                        // If `content` is the only populated field, the
                        // model's previous response almost certainly got
                        // cut off mid-tool-call by `max_output_tokens`
                        // before it could emit `path`. Include a
                        // concrete, actionable recovery hint so the
                        // model doesn't just retry the same oversized
                        // write and hit the same truncation again.
                        let content_only = keys.len() == 1
                            && keys.first().map(|s| s.as_str()) == Some("content");
                        let hint = if content_only {
                            " Your previous response was likely truncated mid-call (content was emitted but the tool-call JSON was cut off before `path`). Split the file into smaller pieces, or use `edit_file` to append in chunks, rather than writing the full body in one call."
                        } else {
                            ""
                        };
                        SofosError::ToolExecution(format!(
                            "Missing 'path' parameter. Got keys: {:?}. \
                             Please retry with 'path' set to the destination file path.{}",
                            keys, hint
                        ))
                    })?;
                let content = input["content"]
                    .as_str()
                    .or_else(|| input["text"].as_str())
                    .or_else(|| input["body"].as_str())
                    .or_else(|| input["data"].as_str())
                    .ok_or_else(|| {
                        SofosError::ToolExecution(format!(
                            "Missing 'content' parameter. Got keys: {:?}. \
                             Please retry with 'content' set to the file body.",
                            input
                                .as_object()
                                .map(|o| o.keys().collect::<Vec<_>>())
                                .unwrap_or_default()
                        ))
                    })?;

                let resolved = self.resolve_for_write(path)?;

                if !resolved.is_inside_workspace {
                    self.check_write_access(path, &resolved.canonical_str, &resolved.canonical)?;
                }

                // Append mode lets the model write a file larger than a
                // single `max_output_tokens` response in multiple calls:
                // first call (append=false or omitted) creates/
                // overwrites, later calls append to the growing file.
                // Default is false so `write_file` keeps its usual
                // "create or overwrite" semantics for non-chunked
                // writes.
                let append = input["append"].as_bool().unwrap_or(false);

                let original_content = if append {
                    // In append mode we don't compute a diff: the
                    // interesting delta is just the new chunk, which
                    // the model already has in front of it. Reading
                    // the whole file back for each chunk would scale
                    // quadratically with the number of chunks.
                    None
                } else if resolved.is_inside_workspace {
                    self.fs_tool.read_file(path).ok()
                } else {
                    self.fs_tool
                        .read_file_with_outside_access(&resolved.canonical_str)
                        .ok()
                };

                match (append, resolved.is_inside_workspace) {
                    (true, true) => self.fs_tool.append_file(path, content)?,
                    (true, false) => self
                        .fs_tool
                        .append_file_with_outside_access(&resolved.canonical_str, content)?,
                    (false, true) => self.fs_tool.write_file(path, content)?,
                    (false, false) => self
                        .fs_tool
                        .write_file_with_outside_access(&resolved.canonical_str, content)?,
                }

                if append {
                    Ok(format!(
                        "Successfully appended {} bytes to '{}'",
                        content.len(),
                        path
                    ))
                } else if let Some(original) = original_content {
                    return Ok(file_modification_result(
                        path,
                        &original,
                        content,
                        "Successfully wrote to file",
                    ));
                } else {
                    Ok(format!("Successfully created file '{}'", path))
                }
            }
            ToolName::ListDirectory => {
                let path = input["path"].as_str().ok_or_else(|| {
                    SofosError::ToolExecution("Missing 'path' parameter".to_string())
                })?;

                let resolved = self.resolve_existing(path)?;
                self.check_read_access(
                    path,
                    &resolved.canonical,
                    &resolved.canonical_str,
                    resolved.is_inside_workspace,
                )?;

                let entries = if resolved.is_inside_workspace {
                    self.fs_tool.list_directory(path)?
                } else {
                    let canonical_entries = std::fs::read_dir(&resolved.canonical)?;
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

                let body = format!("Contents of '{}':\n{}", path, entries.join("\n"));
                Ok(truncate_for_context(
                    &body,
                    MAX_PATH_LIST_TOKENS,
                    TruncationKind::PathList,
                ))
            }
            ToolName::CreateDirectory => {
                let path = input["path"].as_str().ok_or_else(|| {
                    SofosError::ToolExecution("Missing 'path' parameter".to_string())
                })?;

                // Symmetry with `write_file` / `edit_file`: accept
                // absolute and `~/` paths gated by a Write grant, so a
                // user who's granted Write to an external directory can
                // create subfolders there without dropping to bash.
                let resolved = self.resolve_for_write(path)?;
                if !resolved.is_inside_workspace {
                    self.check_write_access(path, &resolved.canonical_str, &resolved.canonical)?;
                }

                if resolved.is_inside_workspace {
                    self.fs_tool.create_directory(path)?;
                } else {
                    std::fs::create_dir_all(&resolved.canonical).map_err(|e| {
                        SofosError::ToolExecution(format!(
                            "Failed to create directory '{}': {}",
                            path, e
                        ))
                    })?;
                }
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
                let include_ignored = input["include_ignored"].as_bool().unwrap_or(false);

                let results =
                    code_search.search(pattern, file_type, max_results, include_ignored)?;
                Ok(format!(
                    "{}{}",
                    crate::tools::codesearch::SEARCH_RESULTS_PREFIX,
                    results
                ))
            }
            ToolName::GlobFiles => {
                let pattern = input["pattern"].as_str().ok_or_else(|| {
                    SofosError::ToolExecution("Missing 'pattern' parameter".to_string())
                })?;
                let base = input["path"].as_str().unwrap_or(".");
                let include_ignored = input["include_ignored"].as_bool().unwrap_or(false);
                // Matches ripgrep's default: symlinks are not followed
                // unless the caller opts in. Prevents a workspace-internal
                // symlink pointing outside the workspace from leaking
                // filenames under the target directory. Set
                // `follow_symlinks: true` to walk them (equivalent to
                // `rg -L`).
                let follow_symlinks = input["follow_symlinks"].as_bool().unwrap_or(false);

                // Same shape as `list_directory` / `read_file`: resolve
                // the base path (tilde/abs/rel) and route through
                // `check_read_access`. External paths with a Read grant
                // proceed; unauthorised `base=".."` / `base="/etc"` hit
                // the permission gate.
                let resolved = self.resolve_existing(base)?;
                self.check_read_access(
                    base,
                    &resolved.canonical,
                    &resolved.canonical_str,
                    resolved.is_inside_workspace,
                )?;
                let search_dir = resolved.canonical;

                // `literal_separator(true)` keeps `*` from crossing `/`,
                // so `*.rs` matches only top-level Rust files and not
                // every Rust file in every subdirectory. Recursive matches
                // still work via `**`. The previous default let a casual
                // `*.rs` quietly walk arbitrary depths, which surprised
                // users and broadened any pattern based on the file name
                // alone into a workspace-wide hit.
                let glob = globset::GlobBuilder::new(pattern)
                    .literal_separator(true)
                    .build()
                    .map_err(|e| SofosError::ToolExecution(format!("Invalid glob pattern: {}", e)))?
                    .compile_matcher();

                // Skip descent into build / vendor directories by basename.
                // Matches the `search_code` policy and prevents a broad
                // pattern like `**/*` from walking a 2.5 GB `target/` tree.
                // `include_ignored=true` disables this and walks everything.
                let excluded_basenames: std::collections::HashSet<&str> = if include_ignored {
                    std::collections::HashSet::new()
                } else {
                    crate::tools::codesearch::DEFAULT_EXCLUDE_DIRS
                        .iter()
                        .copied()
                        .collect()
                };

                // Stop accumulating beyond this many hits — bounds
                // memory on pathological patterns like `**` over a
                // huge tree.
                const GLOB_MAX_MATCHES: usize = 50_000;

                let mut matches = Vec::new();
                let mut stack = vec![search_dir.clone()];
                // Canonical dirs we've descended into — breaks symlink
                // cycles when `follow_symlinks=true`.
                let mut visited: std::collections::HashSet<std::path::PathBuf> =
                    std::collections::HashSet::new();
                if follow_symlinks {
                    if let Ok(canon) = std::fs::canonicalize(&search_dir) {
                        visited.insert(canon);
                    }
                }
                let mut hit_cap = false;

                'walk: while let Some(dir) = stack.pop() {
                    let entries = match std::fs::read_dir(&dir) {
                        Ok(e) => e,
                        Err(_) => continue,
                    };
                    for entry in entries.flatten() {
                        // `file_type()` returns symlink info without
                        // following the link, so we can distinguish a
                        // real directory from a symlink-to-directory.
                        let file_type = match entry.file_type() {
                            Ok(ft) => ft,
                            Err(_) => continue,
                        };
                        if file_type.is_symlink() && !follow_symlinks {
                            continue;
                        }
                        let path = entry.path();
                        if path.is_dir() {
                            let dir_name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
                            if excluded_basenames.contains(dir_name) {
                                continue;
                            }
                            if follow_symlinks {
                                // Skip dirs we already descended under
                                // their canonical form.
                                match std::fs::canonicalize(&path) {
                                    Ok(canon) => {
                                        if !visited.insert(canon) {
                                            continue;
                                        }
                                    }
                                    Err(_) => continue,
                                }
                            }
                            stack.push(path);
                        } else if let Ok(rel) = path.strip_prefix(&search_dir) {
                            let rel_str = rel.to_string_lossy();
                            if glob.is_match(rel_str.as_ref()) {
                                matches.push(rel_str.to_string());
                                if matches.len() >= GLOB_MAX_MATCHES {
                                    hit_cap = true;
                                    break 'walk;
                                }
                            }
                        }
                    }
                }

                matches.sort();

                let body = if matches.is_empty() {
                    format!("No files matching '{}' in '{}'", pattern, base)
                } else if hit_cap {
                    format!(
                        "Found {}+ file(s) matching '{}' (capped at {}; narrow the pattern to see the rest):\n{}",
                        matches.len(),
                        pattern,
                        GLOB_MAX_MATCHES,
                        matches.join("\n")
                    )
                } else {
                    format!(
                        "Found {} file(s) matching '{}':\n{}",
                        matches.len(),
                        pattern,
                        matches.join("\n")
                    )
                };

                Ok(truncate_for_context(
                    &body,
                    MAX_PATH_LIST_TOKENS,
                    TruncationKind::PathList,
                ))
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

                if old_string.is_empty() {
                    return Err(SofosError::ToolExecution(format!(
                        "old_string cannot be empty for '{}'. Use read_file to copy the exact current text you want to replace.",
                        path
                    )));
                }

                // Guard against truncation markers from conversation history compaction.
                // Catches every comment style we've seen the model emit:
                // C-family line/block, shell/Python, HTML/XML, and the
                // square-bracket variant some templates use.
                let truncation_markers = [
                    "...[truncated",
                    "// ... existing code ...",
                    "/* ... existing code ... */",
                    "# ... existing code ...",
                    "<!-- ... existing code ... -->",
                    "[... existing code ...]",
                    "{# ... existing code ... #}",
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

                let resolved = self.resolve_existing(path).map_err(|_| {
                    SofosError::ToolExecution(format!(
                        "File not found: '{}'. The file must exist to edit it.",
                        path
                    ))
                })?;

                // External paths require BOTH a Read grant (we read the
                // file to compute the modified content and the diff) and
                // a Write grant (we write it back). Previously only
                // Write was checked, which silently granted Read as a
                // side effect — defensible ergonomically, but wrong if
                // the user explicitly shaped the permission model to
                // allow writes and block reads. Check both so the scopes
                // hold independently.
                if !resolved.is_inside_workspace {
                    self.check_read_access(
                        path,
                        &resolved.canonical,
                        &resolved.canonical_str,
                        resolved.is_inside_workspace,
                    )?;
                    self.check_write_access(path, &resolved.canonical_str, &resolved.canonical)?;
                }

                // Snapshot mtime + len so the post-modify re-stat can
                // detect a concurrent writer (auto-save, cargo-watch,
                // another editor) before we clobber their change.
                let pre_meta = std::fs::metadata(&resolved.canonical).ok();

                let original = if resolved.is_inside_workspace {
                    self.fs_tool.read_file(path)?
                } else {
                    self.fs_tool
                        .read_file_with_outside_access(&resolved.canonical_str)?
                };

                let match_count = original.matches(old_string).count();
                if match_count == 0 {
                    return Err(SofosError::ToolExecution(format!(
                        "old_string not found in '{}'. Make sure it matches the file content exactly, \
                         including whitespace and indentation. Use read_file first to see the current content.",
                        path
                    )));
                }
                if !replace_all && match_count > 1 {
                    return Err(SofosError::ToolExecution(format!(
                        "old_string appears {} times in '{}'. Non-global edit_file calls require a unique match so the wrong occurrence is not changed. Include more surrounding context in old_string, or set replace_all to true if every occurrence should change.",
                        match_count, path
                    )));
                }

                let modified = if replace_all {
                    original.replace(old_string, new_string)
                } else {
                    original.replacen(old_string, new_string, 1)
                };

                // Re-stat: any mtime/length drift means another writer
                // touched the file mid-edit. Best-effort.
                if let Some(pre) = pre_meta.as_ref() {
                    if let Ok(post) = std::fs::metadata(&resolved.canonical) {
                        let mtime_changed = match (pre.modified(), post.modified()) {
                            (Ok(a), Ok(b)) => a != b,
                            _ => false,
                        };
                        if mtime_changed || pre.len() != post.len() {
                            return Err(SofosError::ToolExecution(format!(
                                "File '{}' changed on disk between the read and the write \
                                 (concurrent edit?). Re-run read_file to see the current \
                                 content and retry.",
                                path
                            )));
                        }
                    }
                }

                if resolved.is_inside_workspace {
                    self.fs_tool.write_file(path, &modified)?;
                } else {
                    self.fs_tool
                        .write_file_with_outside_access(&resolved.canonical_str, &modified)?;
                }

                return Ok(file_modification_result(
                    path,
                    &original,
                    &modified,
                    "Successfully edited",
                ));
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

                let resolved = self.resolve_existing(path).map_err(|_| {
                    SofosError::ToolExecution(format!(
                        "File not found: '{}'. The file must exist for morph_edit_file.",
                        path
                    ))
                })?;

                // External paths require BOTH Read (we send the file to
                // Morph as context) and Write (we write the merged
                // result back). Same rationale as `edit_file`.
                if !resolved.is_inside_workspace {
                    self.check_read_access(
                        path,
                        &resolved.canonical,
                        &resolved.canonical_str,
                        resolved.is_inside_workspace,
                    )?;
                    self.check_write_access(path, &resolved.canonical_str, &resolved.canonical)?;
                }

                let original_code = if resolved.is_inside_workspace {
                    self.fs_tool.read_file(path)?
                } else {
                    self.fs_tool
                        .read_file_with_outside_access(&resolved.canonical_str)?
                };

                // Any Morph failure (timeout, transport, 4xx, 5xx) falls back to
                // a prompt-level `edit_file` hint rather than propagating and
                // stalling the tool loop.
                let morph_timeout = Duration::from_secs(600);
                let merged_code = match tokio::time::timeout(
                    morph_timeout,
                    morph.apply_edit(instruction, &original_code, code_edit),
                )
                .await
                {
                    Ok(Ok(code)) => code,
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
                    Ok(Err(e)) => {
                        // Match only variants Morph produces; propagate anything
                        // else (Interrupted, Io, etc.) so it isn't silently masked.
                        let msg = match e {
                            SofosError::Api(m) | SofosError::NetworkError(m) => m,
                            SofosError::Http(err) => err.to_string(),
                            other => return Err(other),
                        };
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
                };

                // Sanity-check the Morph output before committing it to
                // disk. Morph has occasionally returned a valid-JSON
                // response whose `content` string was silently truncated
                // (the model stopped short without raising `finish_reason
                // = length`), which then got written as a corrupted file.
                // Reject the result instead — the caller still has the
                // original on disk and can retry with `edit_file`.
                if let Err(reason) =
                    morph_validate::validate_morph_output(&original_code, &merged_code)
                {
                    eprintln!(
                        "  {} Morph output rejected ({}), use edit_file instead",
                        "⚠".bright_yellow(),
                        reason
                    );
                    return Ok(ToolExecutionResult::Text(format!(
                        "morph_edit_file rejected Morph's response ({}). The file '{}' was NOT modified. \
                         Please use read_file to get the current file content, then use edit_file \
                         with exact old_string/new_string to make this change.",
                        reason, path
                    )));
                }

                if resolved.is_inside_workspace {
                    self.fs_tool.write_file(path, &merged_code)?;
                } else {
                    self.fs_tool
                        .write_file_with_outside_access(&resolved.canonical_str, &merged_code)?;
                }

                return Ok(file_modification_result(
                    path,
                    &original_code,
                    &merged_code,
                    "Successfully applied Morph edit to",
                ));
            }
            ToolName::DeleteFile => {
                let path = input["path"].as_str().ok_or_else(|| {
                    SofosError::ToolExecution("Missing 'path' parameter".to_string())
                })?;

                // Resolve before prompting so we can surface "file not
                // found" without first asking for confirmation, and so
                // the Write-access check on external paths happens
                // BEFORE the user types y/n (consistent with
                // `write_file` / `edit_file`).
                let resolved = self.resolve_existing(path)?;

                if !resolved.is_inside_workspace {
                    self.check_write_access(path, &resolved.canonical_str, &resolved.canonical)?;
                }

                let confirmed = confirm_destructive(&format!("Delete file '{}'?", path))?;

                if !confirmed {
                    return Ok(ToolExecutionResult::Text(format!(
                        "File deletion cancelled by user. The file '{}' was not deleted.",
                        path
                    )));
                }

                if resolved.is_inside_workspace {
                    self.fs_tool.delete_file(path)?;
                } else {
                    self.fs_tool
                        .delete_file_with_outside_access(&resolved.canonical_str)?;
                }
                Ok(format!("Successfully deleted file '{}'", path))
            }
            ToolName::DeleteDirectory => {
                let path = input["path"].as_str().ok_or_else(|| {
                    SofosError::ToolExecution("Missing 'path' parameter".to_string())
                })?;

                let resolved = self.resolve_existing(path)?;

                if !resolved.is_inside_workspace {
                    self.check_write_access(path, &resolved.canonical_str, &resolved.canonical)?;
                }

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

                if resolved.is_inside_workspace {
                    self.fs_tool.delete_directory(path)?;
                } else {
                    self.fs_tool
                        .delete_directory_with_outside_access(&resolved.canonical_str)?;
                }
                Ok(format!("Successfully deleted directory '{}'", path))
            }
            ToolName::MoveFile => {
                let source = input["source"].as_str().ok_or_else(|| {
                    SofosError::ToolExecution("Missing 'source' parameter".to_string())
                })?;
                let destination = input["destination"].as_str().ok_or_else(|| {
                    SofosError::ToolExecution("Missing 'destination' parameter".to_string())
                })?;

                // Moving a file removes it from its source location, so
                // external sources need a Write grant (not just Read).
                // External destinations need Write as usual.
                let src_resolved = self.resolve_existing(source)?;
                let dst_resolved = self.resolve_for_write(destination)?;

                if !src_resolved.is_inside_workspace {
                    self.check_write_access(
                        source,
                        &src_resolved.canonical_str,
                        &src_resolved.canonical,
                    )?;
                }
                if !dst_resolved.is_inside_workspace {
                    self.check_write_access(
                        destination,
                        &dst_resolved.canonical_str,
                        &dst_resolved.canonical,
                    )?;
                }

                move_between(
                    source,
                    destination,
                    &src_resolved,
                    &dst_resolved,
                    &self.fs_tool,
                )?;
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

                // Copy leaves the source untouched, so external sources
                // only need a Read grant. External destinations still
                // need Write.
                let src_resolved = self.resolve_existing(source)?;
                let dst_resolved = self.resolve_for_write(destination)?;

                if !src_resolved.is_inside_workspace {
                    self.check_read_access(
                        source,
                        &src_resolved.canonical,
                        &src_resolved.canonical_str,
                        src_resolved.is_inside_workspace,
                    )?;
                }
                if !dst_resolved.is_inside_workspace {
                    self.check_write_access(
                        destination,
                        &dst_resolved.canonical_str,
                        &dst_resolved.canonical,
                    )?;
                }

                copy_between(
                    source,
                    destination,
                    &src_resolved,
                    &dst_resolved,
                    &self.fs_tool,
                )?;
                Ok(format!(
                    "Successfully copied '{}' to '{}'",
                    source, destination
                ))
            }
            ToolName::ExecuteBash => {
                let command = input["command"].as_str().ok_or_else(|| {
                    SofosError::ToolExecution("Missing 'command' parameter".to_string())
                })?;

                // The model can ask to run a command outside the sandbox via
                // `sandbox_permissions: "require_escalated"`. A bare bool on
                // either `sandbox_permissions` or `require_escalated` is also
                // accepted, with an optional `justification` shown in the
                // approval prompt.
                let wants_escalation = input["sandbox_permissions"]
                    .as_str()
                    .map(|s| {
                        s.eq_ignore_ascii_case("require_escalated")
                            || s.eq_ignore_ascii_case("escalate")
                    })
                    .or_else(|| input["sandbox_permissions"].as_bool())
                    .or_else(|| input["require_escalated"].as_bool())
                    .unwrap_or(false);
                let result = if wants_escalation {
                    let escalation = crate::tools::bash::EscalationRequest {
                        justification: input["justification"].as_str().map(|s| s.to_string()),
                    };
                    self.bash_executor
                        .execute_with_escalation(command, Some(escalation))?
                } else {
                    self.bash_executor.execute(command)?
                };
                Ok(result)
            }
            ToolName::UpdatePlan => {
                let update = plan::parse_plan_update(input)?;
                return Ok(ToolExecutionResult::TextWithDisplay {
                    text: plan::model_summary(&update),
                    display: plan::render_plan(&update),
                });
            }
            ToolName::ViewImage => {
                let path = input["path"].as_str().ok_or_else(|| {
                    SofosError::ToolExecution("Missing 'path' parameter".to_string())
                })?;

                let trimmed = path.trim();
                if trimmed.is_empty() {
                    return Err(SofosError::ToolExecution(
                        "'path' cannot be empty. Pass a local image file path or an http(s):// URL."
                            .to_string(),
                    ));
                }
                // Reject `data:` URLs up front; otherwise they fall
                // into the file branch and fail with a confusing
                // "Image not found".
                if trimmed.starts_with("data:") {
                    return Err(SofosError::ToolExecution(
                        "view_image does not accept `data:` URLs. Save the image to a file \
                         (workspace-relative, absolute, or ~/) or expose it over http(s):// \
                         and call view_image with that path."
                            .to_string(),
                    ));
                }

                let source = if is_http_url(trimmed) {
                    self.image_loader.prepare_web_image(trimmed)?
                } else {
                    let candidate = resolve_view_image_candidate(self.fs_tool.workspace(), trimmed);
                    if std::fs::metadata(&candidate).is_ok_and(|m| m.is_dir()) {
                        return Err(SofosError::ToolExecution(format!(
                            "'{}' is a directory; view_image only opens files. \
                             Call list_directory on this path to find image files, \
                             then call view_image on each file you want to see.",
                            path
                        )));
                    }
                    self.image_loader.load_local_image(trimmed)?
                };

                let image = ImageData::from(source);
                let display_text = match &image {
                    ImageData::Url { url } => format!("Loaded image from {}", url),
                    ImageData::Base64 { mime_type, data } => {
                        format!(
                            "Loaded image '{}' ({}, ~{} KB)",
                            path,
                            mime_type,
                            base64_approx_decoded_kb(data.len())
                        )
                    }
                };

                return Ok(ToolExecutionResult::Structured(McpToolResult {
                    text: display_text,
                    images: vec![image],
                }));
            }
            ToolName::WebFetch => {
                use futures::StreamExt;

                let url = input["url"].as_str().ok_or_else(|| {
                    SofosError::ToolExecution("Missing 'url' parameter".to_string())
                })?;

                if !is_http_url(url) {
                    return Err(SofosError::ToolExecution(
                        "URL must start with http:// or https://".to_string(),
                    ));
                }

                // Limit redirects to three hops and only http(s); the
                // default policy follows ten hops across any scheme.
                let client = reqwest::Client::builder()
                    .timeout(std::time::Duration::from_secs(30))
                    .redirect(reqwest::redirect::Policy::custom(|attempt| {
                        if attempt.previous().len() >= 3 {
                            return attempt.error("web_fetch follows at most 3 redirects");
                        }
                        match attempt.url().scheme() {
                            "http" | "https" => attempt.follow(),
                            _ => {
                                attempt.error("web_fetch refuses to follow a non-http(s) redirect")
                            }
                        }
                    }))
                    .build()
                    .map_err(|e| SofosError::ToolExecution(format!("HTTP client error: {}", e)))?;

                let response = client
                    .get(url)
                    .header("User-Agent", SOFOS_USER_AGENT)
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

                // Reject the request before downloading anything if the
                // server already announces an oversized body. The
                // streaming cap below catches the unreported case too.
                if let Some(announced) = response.content_length() {
                    if announced > MAX_WEB_FETCH_BODY_BYTES as u64 {
                        return Err(SofosError::ToolExecution(format!(
                            "Response from {} announces {} bytes, which exceeds the {} MB web_fetch cap; aborted before downloading.",
                            url,
                            announced,
                            MAX_WEB_FETCH_BODY_BYTES / (1024 * 1024)
                        )));
                    }
                }

                // Stream the body chunk-by-chunk and abort as soon as the
                // running total crosses the cap. Reading through
                // `response.text()` would buffer the whole body into
                // RAM first, so a pathological URL serving gigabytes
                // could OOM the process even though the trailing
                // truncation below would have shown only the first
                // ~64 KB of characters anyway.
                let mut stream = response.bytes_stream();
                let mut raw: Vec<u8> = Vec::new();
                while let Some(chunk) = stream.next().await {
                    let chunk = chunk.map_err(|e| {
                        SofosError::ToolExecution(format!("Read body failed: {}", e))
                    })?;
                    if raw.len().saturating_add(chunk.len()) > MAX_WEB_FETCH_BODY_BYTES {
                        return Err(SofosError::ToolExecution(format!(
                            "Response from {} exceeded the {} MB web_fetch cap mid-stream; aborted.",
                            url,
                            MAX_WEB_FETCH_BODY_BYTES / (1024 * 1024)
                        )));
                    }
                    raw.extend_from_slice(&chunk);
                }
                // The cap protects RAM; the downstream `html_to_text` +
                // truncation handles the eventual model-visible size.
                // Non-UTF-8 charsets fall through to lossy decoding,
                // which matches what the post-fetch `html_to_text`
                // pipeline expects.
                let body = String::from_utf8_lossy(&raw).into_owned();

                let text =
                    crate::tools::utils::html_to_text_capped(&body, WEB_FETCH_TEXT_BUDGET_BYTES);

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
