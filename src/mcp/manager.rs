use crate::error::{Result, SofosError};
use crate::mcp::client::McpClient;
use crate::mcp::config::load_mcp_config;
use crate::mcp::protocol::{CallToolResult, McpTool, ToolContent};
use colored::Colorize;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::Mutex;

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
        let mut bullets = String::new();

        for (server_name, config) in server_configs {
            match McpClient::connect(server_name.clone(), config).await {
                Ok(client) => {
                    // Get tools from this server
                    match client.list_tools().await {
                        Ok(tools) => {
                            let tool_count = tools.len();
                            for tool in &tools {
                                // Prefix tool name with server name to avoid conflicts
                                let prefixed_name = format!("{}_{}", server_name, tool.name);
                                tool_to_server.insert(prefixed_name, server_name.to_string());
                            }
                            tools_by_server.insert(server_name.clone(), tools);
                            clients.insert(server_name.clone(), Arc::new(client));
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
                    }
                }
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
        };
        let init_block = if bullets.is_empty() {
            String::new()
        } else {
            format!("{}\n{}", "MCP servers:".bright_green(), bullets)
        };
        Ok((manager, init_block))
    }

    /// Get all available MCP tools from all connected servers.
    ///
    /// Served from the cache built at construction time — no remote
    /// round-trip per call. MCP server tool lists are stable for the
    /// lifetime of a session, so refreshing on every TUI tick is pure
    /// network noise.
    pub async fn get_all_tools(&self) -> Result<Vec<crate::api::Tool>> {
        let mut all_tools = Vec::new();
        for (server_name, tools) in self.tools_by_server.iter() {
            for mcp_tool in tools {
                let prefixed_name = format!("{}_{}", server_name, mcp_tool.name);
                all_tools.push(crate::api::Tool::Regular {
                    name: prefixed_name,
                    description: format!("[MCP: {}] {}", server_name, mcp_tool.description),
                    input_schema: mcp_tool.input_schema.clone(),
                    cache_control: None,
                });
            }
        }
        Ok(all_tools)
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
        // before awaiting. The earlier version held the outer lock
        // across `.await`, which serialised every tool call across
        // every server.
        let client = {
            let clients = self.clients.lock().await;
            clients.get(server_name).cloned().ok_or_else(|| {
                SofosError::McpError(format!("MCP server '{}' not connected", server_name))
            })?
        };

        let original_tool_name = tool_name
            .strip_prefix(&format!("{}_", server_name))
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
        }
    }
}
