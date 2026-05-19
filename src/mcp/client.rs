use crate::error::{Result, SofosError};
use crate::mcp::config::McpServerConfig;
use crate::mcp::protocol::*;
use crate::mcp::transport::{HttpClient, StdioClient};
use serde_json::Value;
use std::time::Duration;

/// Ceiling on a single MCP request (stdio read + HTTP round-trip). A
/// misbehaving MCP server used to freeze every subsequent MCP call
/// because `BufRead::read_line` blocks indefinitely and the stdout
/// mutex serialises all requests. Two minutes is generous enough for
/// slow remote search backends (PubMed / ClinicalTrials.gov) while
/// still bounding a frozen server's blast radius to a single turn.
pub(crate) const MCP_REQUEST_TIMEOUT: Duration = Duration::from_secs(120);

/// Shorter ceiling for the MCP `initialize` handshake. A frozen server
/// holds startup hostage for the full request timeout otherwise (two
/// minutes per server). 15 s is long enough for a slow stdio child to
/// finish booting and short enough that a misconfigured server gives
/// up quickly so the user can fix the config.
pub(crate) const MCP_INIT_TIMEOUT: Duration = Duration::from_secs(15);

pub(crate) fn create_init_request() -> InitializeRequest {
    InitializeRequest {
        protocol_version: "2024-11-05".to_string(),
        capabilities: ClientCapabilities {
            roots: Some(RootsCapability { list_changed: true }),
            sampling: None,
        },
        client_info: ClientInfo {
            name: "sofos".to_string(),
            version: env!("CARGO_PKG_VERSION").to_string(),
        },
    }
}

pub(crate) fn parse_list_tools_response(result: Value) -> Result<Vec<McpTool>> {
    let list_result: ListToolsResult = serde_json::from_value(result)?;
    Ok(list_result.tools)
}

pub(crate) fn parse_call_tool_response(result: Value) -> Result<CallToolResult> {
    let call_result: CallToolResult = serde_json::from_value(result)?;
    Ok(call_result)
}

pub(crate) fn create_call_tool_request(name: &str, arguments: Option<Value>) -> CallToolRequest {
    CallToolRequest {
        name: name.to_string(),
        arguments,
    }
}

pub enum McpClient {
    Stdio(StdioClient),
    Http(HttpClient),
}

impl McpClient {
    pub async fn connect(name: String, config: McpServerConfig) -> Result<Self> {
        if config.is_stdio() {
            let client = StdioClient::new(name, config).await?;
            Ok(McpClient::Stdio(client))
        } else if config.is_http() {
            let client = HttpClient::new(name, config).await?;
            Ok(McpClient::Http(client))
        } else {
            Err(SofosError::McpError(
                "Invalid MCP server configuration".to_string(),
            ))
        }
    }

    pub async fn list_tools(&self) -> Result<Vec<McpTool>> {
        match self {
            McpClient::Stdio(client) => client.list_tools().await,
            McpClient::Http(client) => client.list_tools().await,
        }
    }

    pub async fn call_tool(&self, name: &str, arguments: Option<Value>) -> Result<CallToolResult> {
        match self {
            McpClient::Stdio(client) => client.call_tool(name, arguments).await,
            McpClient::Http(client) => client.call_tool(name, arguments).await,
        }
    }
}
