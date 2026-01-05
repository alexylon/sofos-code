use crate::error::{Result, SofosError};
use crate::mcp::client::McpClient;
use crate::mcp::config::load_mcp_config;
use crate::mcp::protocol::{CallToolResult, ToolContent};
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

impl ToolResult {
    #[allow(dead_code)]
    pub fn text_only(text: String) -> Self {
        Self {
            text,
            images: Vec::new(),
        }
    }

    #[allow(dead_code)]
    pub fn has_images(&self) -> bool {
        !self.images.is_empty()
    }
}

/// Manages multiple MCP server connections and their tools
pub struct McpManager {
    clients: Arc<Mutex<HashMap<String, McpClient>>>,
    tool_to_server: Arc<Mutex<HashMap<String, String>>>,
}

impl McpManager {
    pub async fn new(workspace: PathBuf) -> Result<Self> {
        let server_configs = load_mcp_config(&workspace);

        let mut clients = HashMap::new();
        let mut tool_to_server = HashMap::new();

        for (server_name, config) in server_configs {
            match McpClient::connect(server_name.clone(), config).await {
                Ok(client) => {
                    // Get tools from this server
                    match client.list_tools().await {
                        Ok(tools) => {
                            let tool_count = tools.len();
                            for tool in tools {
                                // Prefix tool name with server name to avoid conflicts
                                let prefixed_name = format!("{}_{}", server_name, tool.name);
                                tool_to_server.insert(prefixed_name, server_name.to_string());
                            }
                            clients.insert(server_name.clone(), client);
                            println!(
                                "{} MCP server '{}' initialized ({} tools)",
                                "✓".bright_green(),
                                server_name.bright_cyan(),
                                tool_count
                            );
                        }
                        Err(e) => {
                            eprintln!(
                                "Warning: Failed to list tools from MCP server '{}': {}",
                                server_name, e
                            );
                        }
                    }
                }
                Err(e) => {
                    eprintln!(
                        "Warning: Failed to connect to MCP server '{}': {}",
                        server_name, e
                    );
                }
            }
        }

        Ok(Self {
            clients: Arc::new(Mutex::new(clients)),
            tool_to_server: Arc::new(Mutex::new(tool_to_server)),
        })
    }

    /// Get all available MCP tools from all connected servers
    pub async fn get_all_tools(&self) -> Result<Vec<crate::api::Tool>> {
        let clients = self.clients.lock().await;
        let mut all_tools = Vec::new();

        for (server_name, client) in clients.iter() {
            match client.list_tools().await {
                Ok(tools) => {
                    for mcp_tool in tools {
                        // Prefix tool name with server name
                        let prefixed_name = format!("{}_{}", server_name, mcp_tool.name);

                        all_tools.push(crate::api::Tool::Regular {
                            name: prefixed_name,
                            description: format!("[MCP: {}] {}", server_name, mcp_tool.description),
                            input_schema: mcp_tool.input_schema,
                            cache_control: None,
                        });
                    }
                }
                Err(e) => {
                    eprintln!(
                        "Warning: Failed to list tools from MCP server '{}': {}",
                        server_name, e
                    );
                }
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
        let tool_to_server = self.tool_to_server.lock().await;
        let clients = self.clients.lock().await;

        // Find which server owns this tool
        let server_name = tool_to_server
            .get(tool_name)
            .ok_or_else(|| SofosError::McpError(format!("Unknown MCP tool: {}", tool_name)))?;

        let client = clients.get(server_name).ok_or_else(|| {
            SofosError::McpError(format!("MCP server '{}' not connected", server_name))
        })?;

        let original_tool_name = tool_name
            .strip_prefix(&format!("{}_", server_name))
            .unwrap_or(tool_name);

        let result = client
            .call_tool(original_tool_name, Some(input.clone()))
            .await?;

        Ok(format_tool_result(result))
    }

    /// Check if a tool name belongs to an MCP server
    pub async fn is_mcp_tool(&self, tool_name: &str) -> bool {
        let tool_to_server = self.tool_to_server.lock().await;
        tool_to_server.contains_key(tool_name)
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
            tool_to_server: Arc::clone(&self.tool_to_server),
        }
    }
}
