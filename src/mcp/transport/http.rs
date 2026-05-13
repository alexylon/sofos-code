use crate::error::{Result, SofosError};
use crate::mcp::client::{
    MCP_REQUEST_TIMEOUT, create_call_tool_request, create_init_request, parse_call_tool_response,
    parse_list_tools_response,
};
use crate::mcp::config::McpServerConfig;
use crate::mcp::protocol::*;
use serde_json::Value;
use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

pub struct HttpClient {
    server_name: String,
    url: String,
    headers: HashMap<String, String>,
    client: reqwest::Client,
    next_id: Arc<AtomicU64>,
}

impl HttpClient {
    pub async fn new(server_name: String, config: McpServerConfig) -> Result<Self> {
        let url = config
            .url
            .ok_or_else(|| SofosError::McpError("Missing URL for HTTP server".to_string()))?;

        let headers = config.headers.unwrap_or_default();

        // A bare `reqwest::Client::new()` uses no request timeout at
        // all, so a slow remote MCP server could stall a turn forever.
        // Set the shared MCP ceiling at client-construction time so
        // every call-site inherits it without extra threading.
        let client = reqwest::Client::builder()
            .timeout(MCP_REQUEST_TIMEOUT)
            .build()
            .map_err(|e| SofosError::McpError(format!("Failed to build MCP HTTP client: {}", e)))?;

        let http_client = Self {
            server_name: server_name.clone(),
            url,
            headers,
            client,
            next_id: Arc::new(AtomicU64::new(1)),
        };

        http_client.initialize().await?;

        Ok(http_client)
    }

    async fn initialize(&self) -> Result<()> {
        let response = self
            .send_request(
                "initialize",
                Some(serde_json::to_value(create_init_request())?),
            )
            .await?;

        let _init_result: InitializeResult = serde_json::from_value(response)?;

        Ok(())
    }

    async fn send_request(&self, method: &str, params: Option<Value>) -> Result<Value> {
        let id = self.next_id.fetch_add(1, Ordering::SeqCst);
        let request = JsonRpcRequest::new(id, method.to_string(), params);

        let mut req = self.client.post(&self.url).json(&request);

        for (key, value) in &self.headers {
            req = req.header(key, value);
        }

        let response = req.send().await.map_err(|e| {
            SofosError::McpError(format!(
                "Failed to send request to MCP server '{}': {}",
                self.server_name, e
            ))
        })?;

        let response_json: JsonRpcResponse = response.json().await.map_err(|e| {
            SofosError::McpError(format!(
                "Failed to parse response from MCP server '{}': {}",
                self.server_name, e
            ))
        })?;

        if let Some(error) = response_json.error {
            return Err(SofosError::McpError(format!(
                "MCP server '{}' returned error: {}",
                self.server_name, error.message
            )));
        }

        response_json.result.ok_or_else(|| {
            SofosError::McpError(format!(
                "MCP server '{}' returned no result",
                self.server_name
            ))
        })
    }

    pub async fn list_tools(&self) -> Result<Vec<McpTool>> {
        let result = self.send_request("tools/list", None).await?;
        parse_list_tools_response(result)
    }

    pub async fn call_tool(&self, name: &str, arguments: Option<Value>) -> Result<CallToolResult> {
        let result = self
            .send_request(
                "tools/call",
                Some(serde_json::to_value(create_call_tool_request(
                    name, arguments,
                ))?),
            )
            .await?;
        parse_call_tool_response(result)
    }
}
