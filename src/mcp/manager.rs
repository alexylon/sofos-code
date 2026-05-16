use crate::error::{Result, SofosError};
use crate::mcp::client::McpClient;
use crate::mcp::config::{SafeModeAccess, load_mcp_config};
use crate::mcp::protocol::{CallToolResult, McpTool, ToolContent};
use colored::Colorize;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::Mutex;

/// Separator inserted between an MCP server name and a tool name to form
/// the prefixed identifier the model sees. Triple underscore is unlikely
/// to appear inside a tool or server name; the registration step rejects
/// any name that contains it, so the reverse lookup is unambiguous.
pub const MCP_NAME_SEPARATOR: &str = "___";

/// Structured tool result that can contain both text and image data
#[derive(Debug, Clone)]
pub struct ToolResult {
    pub text: String,
    pub images: Vec<ImageData>,
}

#[derive(Debug, Clone)]
pub struct ImageData {
    pub mime_type: String,
    pub base64_data: String,
}

/// Manages multiple MCP server connections and their tools.
///
/// `clients` wraps each `McpClient` in `Arc` so callers can clone the
/// handle out of the map and drop the outer lock before awaiting on a
/// server. The previous implementation held the outer lock across
/// `client.call_tool(...).await`, which serialised every tool call
/// across every server.
///
/// `tools_by_server` is a snapshot of each server's tool list taken at
/// construction time. The earlier `get_all_tools` re-called
/// `client.list_tools` on every invocation, which meant every TUI
/// refresh hit each remote MCP server with a fresh round-trip.
///
/// `tool_to_server` is also a snapshot — it never mutates after
/// construction, so it lives behind an `Arc<HashMap>` rather than a
/// `Mutex` and `is_mcp_tool` serves from it lock-free.
pub struct McpManager {
    clients: Arc<Mutex<HashMap<String, Arc<McpClient>>>>,
    tools_by_server: Arc<HashMap<String, Vec<McpTool>>>,
    tool_to_server: Arc<HashMap<String, String>>,
    safe_mode_by_server: Arc<HashMap<String, SafeModeAccess>>,
}

/// Reject server and tool names that contain the prefix separator or
/// that would produce an empty identifier. Names are otherwise accepted
/// as-is; provider-level character validation happens on the prefixed
/// name when the tool list is sent.
fn validate_mcp_name(kind: &str, name: &str) -> Result<()> {
    if name.is_empty() {
        return Err(SofosError::McpError(format!("MCP {} name is empty", kind)));
    }
    if name.contains(MCP_NAME_SEPARATOR) {
        return Err(SofosError::McpError(format!(
            "MCP {} name '{}' contains the reserved separator '{}'",
            kind, name, MCP_NAME_SEPARATOR
        )));
    }
    Ok(())
}

pub fn prefixed_tool_name(server: &str, tool: &str) -> String {
    format!("{}{}{}", server, MCP_NAME_SEPARATOR, tool)
}

impl McpManager {
    /// Returns the manager and a pre-formatted startup block for the
    /// caller to fold into the TUI banner. The block is empty when no
    /// servers connected; otherwise it carries an `MCP servers:` header
    /// followed by one indented `✓ name (N tools)` bullet per server,
    /// matching the workspace/model section above it. Printing this
    /// block here would land on the raw terminal before `OutputCapture`
    /// is installed, and the inline viewport scrolls it off-screen when
    /// it anchors.
    pub async fn new(workspace: PathBuf) -> Result<(Self, String)> {
        let server_configs = load_mcp_config(&workspace);

        let mut clients: HashMap<String, Arc<McpClient>> = HashMap::new();
        let mut tools_by_server: HashMap<String, Vec<McpTool>> = HashMap::new();
        let mut tool_to_server: HashMap<String, String> = HashMap::new();
        let mut safe_mode_by_server: HashMap<String, SafeModeAccess> = HashMap::new();
        let mut bullets = String::new();

        for (server_name, config) in server_configs {
            if let Err(e) = validate_mcp_name("server", &server_name) {
                tracing::warn!(server = %server_name, error = %e, "skipping MCP server");
                continue;
            }
            let server_safe_mode = config.safe_mode;
            match McpClient::connect(server_name.clone(), config).await {
                Ok(client) => match client.list_tools().await {
                    Ok(tools) => {
                        let mut accepted: Vec<McpTool> = Vec::with_capacity(tools.len());
                        for tool in tools {
                            if let Err(e) = validate_mcp_name("tool", &tool.name) {
                                tracing::warn!(
                                    server = %server_name,
                                    tool = %tool.name,
                                    error = %e,
                                    "skipping MCP tool with reserved separator in its name"
                                );
                                continue;
                            }
                            let prefixed = prefixed_tool_name(&server_name, &tool.name);
                            if let Some(existing) = tool_to_server.get(&prefixed) {
                                tracing::warn!(
                                    new_server = %server_name,
                                    existing_server = %existing,
                                    tool = %tool.name,
                                    "MCP tool name collision; keeping the first registration"
                                );
                                continue;
                            }
                            tool_to_server.insert(prefixed, server_name.clone());
                            accepted.push(tool);
                        }
                        let tool_count = accepted.len();
                        tools_by_server.insert(server_name.clone(), accepted);
                        clients.insert(server_name.clone(), Arc::new(client));
                        safe_mode_by_server.insert(server_name.clone(), server_safe_mode);
                        bullets.push_str(&format!(
                            "  {} {} ({} tools)\n",
                            "✓".bright_green(),
                            server_name.bright_cyan(),
                            tool_count
                        ));
                    }
                    Err(e) => {
                        tracing::warn!(
                            server = %server_name,
                            error = %e,
                            "failed to list tools from MCP server"
                        );
                    }
                },
                Err(e) => {
                    tracing::warn!(
                        server = %server_name,
                        error = %e,
                        "failed to connect to MCP server"
                    );
                }
            }
        }

        let manager = Self {
            clients: Arc::new(Mutex::new(clients)),
            tools_by_server: Arc::new(tools_by_server),
            tool_to_server: Arc::new(tool_to_server),
            safe_mode_by_server: Arc::new(safe_mode_by_server),
        };
        let init_block = if bullets.is_empty() {
            String::new()
        } else {
            format!("{}\n{}", "MCP servers:".bright_green(), bullets)
        };
        Ok((manager, init_block))
    }

    /// Names of servers that have at least one tool registered. The
    /// caller can use these to compose a startup warning when safe
    /// mode is on but no server has opted in.
    pub fn server_names_for_safe_mode(&self, included: bool) -> Vec<String> {
        self.safe_mode_by_server
            .iter()
            .filter(|(_, access)| access.is_available_in_safe_mode() == included)
            .map(|(name, _)| name.clone())
            .collect()
    }

    pub fn is_server_available_in_safe_mode(&self, server: &str) -> bool {
        self.safe_mode_by_server
            .get(server)
            .copied()
            .unwrap_or_default()
            .is_available_in_safe_mode()
    }

    /// Get all available MCP tools from all connected servers.
    ///
    /// Served from the cache built at construction time — no remote
    /// round-trip per call. MCP server tool lists are stable for the
    /// lifetime of a session, so refreshing on every TUI tick is pure
    /// network noise.
    pub async fn get_all_tools(&self) -> Result<Vec<crate::api::Tool>> {
        Ok(self.collect_tools(false))
    }

    /// Get only the MCP tools whose servers opted into safe mode. Used
    /// by the tool executor to filter the tool list shown to the model
    /// when the user is in safe mode.
    pub async fn get_safe_mode_tools(&self) -> Result<Vec<crate::api::Tool>> {
        Ok(self.collect_tools(true))
    }

    fn collect_tools(&self, safe_mode: bool) -> Vec<crate::api::Tool> {
        let mut all_tools = Vec::new();
        for (server_name, tools) in self.tools_by_server.iter() {
            if safe_mode && !self.is_server_available_in_safe_mode(server_name) {
                continue;
            }
            for mcp_tool in tools {
                let prefixed_name = prefixed_tool_name(server_name, &mcp_tool.name);
                all_tools.push(crate::api::Tool::Regular {
                    name: prefixed_name,
                    description: format!("[MCP: {}] {}", server_name, mcp_tool.description),
                    input_schema: mcp_tool.input_schema.clone(),
                    cache_control: None,
                });
            }
        }
        all_tools
    }

    /// Execute an MCP tool call
    pub async fn execute_tool(
        &self,
        tool_name: &str,
        input: &serde_json::Value,
    ) -> Result<ToolResult> {
        // Find which server owns this tool — `tool_to_server` is
        // immutable so the lookup is lock-free.
        let server_name = self
            .tool_to_server
            .get(tool_name)
            .ok_or_else(|| SofosError::McpError(format!("Unknown MCP tool: {}", tool_name)))?;

        // Clone the client `Arc` out under the lock, then drop the lock
        // before awaiting. See the struct-level doc on `clients` for
        // why holding the outer lock across `.await` is unsafe.
        let client = {
            let clients = self.clients.lock().await;
            clients.get(server_name).cloned().ok_or_else(|| {
                SofosError::McpError(format!("MCP server '{}' not connected", server_name))
            })?
        };

        let original_tool_name = tool_name
            .strip_prefix(&format!("{}{}", server_name, MCP_NAME_SEPARATOR))
            .unwrap_or(tool_name);

        let result = client
            .call_tool(original_tool_name, Some(input.clone()))
            .await?;

        Ok(format_tool_result(result))
    }

    /// Check if a tool name belongs to an MCP server. The lookup is
    /// lock-free because `tool_to_server` is immutable after
    /// construction.
    pub fn is_mcp_tool(&self, tool_name: &str) -> bool {
        self.tool_to_server.contains_key(tool_name)
    }

    pub fn server_for_tool(&self, tool_name: &str) -> Option<&str> {
        self.tool_to_server.get(tool_name).map(String::as_str)
    }
}

fn format_tool_result(result: CallToolResult) -> ToolResult {
    let mut text_output = String::new();
    let mut images = Vec::new();

    if result.is_error == Some(true) {
        text_output.push_str("Error from MCP tool:\n");
    }

    for content in result.content {
        match content {
            ToolContent::Text { text } => {
                text_output.push_str(&text);
                text_output.push('\n');
            }
            ToolContent::Image { data, mime_type } => {
                let size_kb = (data.len() * 3 / 4) / 1024;
                text_output.push_str(&format!("[Image: {} ({} KB)]\n", mime_type, size_kb));
                images.push(ImageData {
                    mime_type,
                    base64_data: data,
                });
            }
            ToolContent::Resource {
                uri,
                mime_type,
                text,
            } => {
                text_output.push_str(&format!("[Resource: {}]\n", uri));
                if let Some(mime) = mime_type {
                    text_output.push_str(&format!("MIME type: {}\n", mime));
                }
                if let Some(content) = text {
                    text_output.push_str(&content);
                    text_output.push('\n');
                }
            }
        }
    }

    ToolResult {
        text: text_output,
        images,
    }
}

impl Clone for McpManager {
    fn clone(&self) -> Self {
        Self {
            clients: Arc::clone(&self.clients),
            tools_by_server: Arc::clone(&self.tools_by_server),
            tool_to_server: Arc::clone(&self.tool_to_server),
            safe_mode_by_server: Arc::clone(&self.safe_mode_by_server),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prefixed_name_uses_triple_underscore_separator() {
        assert_eq!(prefixed_tool_name("docs", "read"), "docs___read");
        assert_eq!(
            prefixed_tool_name("github", "create_issue"),
            "github___create_issue"
        );
    }

    #[test]
    fn validate_rejects_reserved_separator_in_names() {
        assert!(validate_mcp_name("server", "good").is_ok());
        assert!(validate_mcp_name("server", "with___sep").is_err());
        assert!(validate_mcp_name("tool", "").is_err());
    }

    /// Two `(server, tool)` pairs that would collide under the old
    /// single-underscore separator no longer collide under the triple
    /// underscore. Names that contain the reserved separator are
    /// rejected before they can hit the registration map at all.
    #[test]
    fn collisions_from_underscores_no_longer_overlap() {
        let a = prefixed_tool_name("a", "b_c");
        let b = prefixed_tool_name("a_b", "c");
        assert_ne!(a, b);
        assert_eq!(a, "a___b_c");
        assert_eq!(b, "a_b___c");
    }
}
