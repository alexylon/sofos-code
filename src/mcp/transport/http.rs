use crate::error::{Result, SofosError};
use crate::mcp::client::{
    MCP_INIT_TIMEOUT, MCP_REQUEST_TIMEOUT, create_call_tool_request, create_init_request,
    parse_call_tool_response, parse_list_tools_response,
};
use crate::mcp::config::McpServerConfig;
use crate::mcp::protocol::*;
use futures::StreamExt;
use serde_json::Value;
use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

/// Bound on the TCP/TLS connect phase for the HTTP MCP transport.
/// Without this, a network outage waits the full request timeout
/// (`MCP_REQUEST_TIMEOUT`, currently 120 s) before failing — confusing
/// when the user just wants a quick "server unreachable" signal.
const HTTP_CONNECT_TIMEOUT: Duration = Duration::from_secs(10);

/// Cap on a single MCP HTTP response body. A hostile or buggy server
/// can otherwise stream multi-GB JSON for a `tools/list` reply and
/// OOM the host long before the request timeout fires.
const MCP_HTTP_BODY_CAP: usize = 32 * 1024 * 1024;

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
        // every call-site inherits it without extra threading. The
        // connect timeout is shorter than the overall ceiling so an
        // unreachable host fails fast instead of holding the full
        // request budget on DNS / TCP / TLS.
        //
        // Redirects are disabled outright. The default reqwest policy
        // would follow up to 10 hops and forward custom headers like
        // `Authorization: Bearer ...` across origins, leaking the
        // bearer token to whatever host the server pointed at. A 3xx
        // here surfaces as an explicit error instead.
        let client = reqwest::Client::builder()
            .timeout(MCP_REQUEST_TIMEOUT)
            .connect_timeout(HTTP_CONNECT_TIMEOUT)
            .redirect(reqwest::redirect::Policy::none())
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
        // Handshake under a tighter ceiling than tool calls so a
        // frozen server can't hold session startup hostage.
        let handshake = async {
            let response = self
                .send_request(
                    "initialize",
                    Some(serde_json::to_value(create_init_request())?),
                )
                .await?;
            let _init_result: InitializeResult = serde_json::from_value(response)?;
            self.send_notification_initialized().await?;
            Ok::<(), SofosError>(())
        };
        tokio::time::timeout(MCP_INIT_TIMEOUT, handshake)
            .await
            .map_err(|_| {
                SofosError::McpError(format!(
                    "MCP server '{}' initialize timed out after {}s",
                    self.server_name,
                    MCP_INIT_TIMEOUT.as_secs()
                ))
            })?
    }

    async fn send_notification_initialized(&self) -> Result<()> {
        // JSON-RPC notification: no `id`, no response expected.
        let notification = serde_json::json!({
            "jsonrpc": "2.0",
            "method": "notifications/initialized",
        });

        let mut req = self.client.post(&self.url).json(&notification);

        for (key, value) in &self.headers {
            req = req.header(key, value);
        }

        req.send().await.map_err(|e| {
            SofosError::McpError(format!(
                "Failed to send `notifications/initialized` to MCP server '{}': {}",
                self.server_name, e
            ))
        })?;

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

        let status = response.status();
        if status.is_redirection() {
            return Err(SofosError::McpError(format!(
                "MCP server '{}' returned redirect status {}; refused to follow \
                 to keep the configured bearer token from leaking cross-origin",
                self.server_name, status
            )));
        }

        if let Some(announced) = response.content_length() {
            if announced as usize > MCP_HTTP_BODY_CAP {
                return Err(SofosError::McpError(format!(
                    "MCP server '{}' announced a {} byte response, exceeds {} MB cap",
                    self.server_name,
                    announced,
                    MCP_HTTP_BODY_CAP / (1024 * 1024)
                )));
            }
        }

        let mut buffered: Vec<u8> = Vec::with_capacity(8 * 1024);
        let mut stream = response.bytes_stream();
        while let Some(chunk) = stream.next().await {
            let chunk = chunk.map_err(|e| {
                SofosError::McpError(format!(
                    "Failed to read response body from MCP server '{}': {}",
                    self.server_name, e
                ))
            })?;
            if buffered.len().saturating_add(chunk.len()) > MCP_HTTP_BODY_CAP {
                return Err(SofosError::McpError(format!(
                    "MCP server '{}' exceeded the {} MB response cap mid-stream",
                    self.server_name,
                    MCP_HTTP_BODY_CAP / (1024 * 1024)
                )));
            }
            buffered.extend_from_slice(&chunk);
        }

        let raw: Value = serde_json::from_slice(&buffered).map_err(|e| {
            SofosError::McpError(format!(
                "Failed to parse response from MCP server '{}': {}",
                self.server_name, e
            ))
        })?;
        if raw.get("method").is_some() {
            return Err(SofosError::McpError(format!(
                "MCP server '{}' sent a server-initiated message; sofos does not \
                 implement the server-to-client side of the spec",
                self.server_name
            )));
        }

        let response_json: JsonRpcResponse = serde_json::from_value(raw).map_err(|e| {
            SofosError::McpError(format!(
                "Failed to parse response envelope from MCP server '{}': {}",
                self.server_name, e
            ))
        })?;
        if !response_json.id.matches_outgoing(id) {
            return Err(SofosError::McpError(format!(
                "MCP server '{}' returned response with id {:?}, expected {}",
                self.server_name, response_json.id, id
            )));
        }

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
