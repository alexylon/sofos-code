use crate::error::{Result, SofosError};
use crate::mcp::config::McpServerConfig;
use crate::mcp::protocol::*;
use serde_json::Value;
use std::collections::HashMap;
use std::io::{BufRead, BufReader, Write};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

fn create_init_request() -> InitializeRequest {
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

fn parse_list_tools_response(result: Value) -> Result<Vec<McpTool>> {
    let list_result: ListToolsResult = serde_json::from_value(result)?;
    Ok(list_result.tools)
}

fn parse_call_tool_response(result: Value) -> Result<CallToolResult> {
    let call_result: CallToolResult = serde_json::from_value(result)?;
    Ok(call_result)
}

fn create_call_tool_request(name: &str, arguments: Option<Value>) -> CallToolRequest {
    CallToolRequest {
        name: name.to_string(),
        arguments,
    }
}

macro_rules! impl_list_tools {
    ($self:ident) => {{
        let result = $self.send_request("tools/list", None).await?;
        parse_list_tools_response(result)
    }};
}

macro_rules! impl_call_tool {
    ($self:ident, $name:ident, $arguments:ident) => {{
        let result = $self
            .send_request(
                "tools/call",
                Some(serde_json::to_value(create_call_tool_request(
                    $name, $arguments,
                ))?),
            )
            .await?;
        parse_call_tool_response(result)
    }};
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

pub struct StdioClient {
    server_name: String,
    _process: Arc<Mutex<Child>>,
    stdin: Arc<Mutex<ChildStdin>>,
    stdout: Arc<Mutex<BufReader<ChildStdout>>>,
    next_id: Arc<AtomicU64>,
}

impl StdioClient {
    pub async fn new(server_name: String, config: McpServerConfig) -> Result<Self> {
        let command = config
            .command
            .ok_or_else(|| SofosError::McpError("Missing command for stdio server".to_string()))?;

        let args = config.args.unwrap_or_default();
        let env_vars = config.env.unwrap_or_default();

        let mut cmd = Command::new(&command);
        cmd.args(&args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit());

        for (key, value) in env_vars {
            cmd.env(key, value);
        }

        let mut process = cmd.spawn().map_err(|e| {
            SofosError::McpError(format!(
                "Failed to start MCP server '{}': {}",
                server_name, e
            ))
        })?;

        let stdin = process.stdin.take().ok_or_else(|| {
            SofosError::McpError(format!(
                "Failed to get stdin for MCP server '{}'",
                server_name
            ))
        })?;

        let stdout = process.stdout.take().ok_or_else(|| {
            SofosError::McpError(format!(
                "Failed to get stdout for MCP server '{}'",
                server_name
            ))
        })?;

        let client = Self {
            server_name: server_name.clone(),
            _process: Arc::new(Mutex::new(process)),
            stdin: Arc::new(Mutex::new(stdin)),
            stdout: Arc::new(Mutex::new(BufReader::new(stdout))),
            next_id: Arc::new(AtomicU64::new(1)),
        };

        client.initialize().await?;

        Ok(client)
    }

    async fn initialize(&self) -> Result<()> {
        let response = self
            .send_request(
                "initialize",
                Some(serde_json::to_value(create_init_request())?),
            )
            .await?;

        let _init_result: InitializeResult = serde_json::from_value(response)?;

        self.send_notification("notifications/initialized", None)
            .await?;

        Ok(())
    }

    async fn send_request(&self, method: &str, params: Option<Value>) -> Result<Value> {
        let id = self.next_id.fetch_add(1, Ordering::SeqCst);
        let request = JsonRpcRequest::new(id, method.to_string(), params);

        let request_json = serde_json::to_string(&request)?;

        {
            let mut stdin = self
                .stdin
                .lock()
                .map_err(|e| SofosError::McpError(format!("Failed to lock stdin: {}", e)))?;
            writeln!(stdin, "{}", request_json).map_err(|e| {
                SofosError::McpError(format!(
                    "Failed to write to MCP server '{}': {}",
                    self.server_name, e
                ))
            })?;
            stdin.flush().map_err(|e| {
                SofosError::McpError(format!(
                    "Failed to flush stdin for MCP server '{}': {}",
                    self.server_name, e
                ))
            })?;
        }

        let mut stdout = self
            .stdout
            .lock()
            .map_err(|e| SofosError::McpError(format!("Failed to lock stdout: {}", e)))?;

        let mut response_line = String::new();
        stdout.read_line(&mut response_line).map_err(|e| {
            SofosError::McpError(format!(
                "Failed to read from MCP server '{}': {}",
                self.server_name, e
            ))
        })?;

        let response: JsonRpcResponse = serde_json::from_str(&response_line).map_err(|e| {
            SofosError::McpError(format!(
                "Failed to parse response from MCP server '{}': {}",
                self.server_name, e
            ))
        })?;

        if let Some(error) = response.error {
            return Err(SofosError::McpError(format!(
                "MCP server '{}' returned error: {}",
                self.server_name, error.message
            )));
        }

        response.result.ok_or_else(|| {
            SofosError::McpError(format!(
                "MCP server '{}' returned no result",
                self.server_name
            ))
        })
    }

    async fn send_notification(&self, method: &str, _params: Option<Value>) -> Result<()> {
        let notification = serde_json::json!({
            "jsonrpc": "2.0",
            "method": method,
        });

        let notification_json = serde_json::to_string(&notification)?;

        let mut stdin = self
            .stdin
            .lock()
            .map_err(|e| SofosError::McpError(format!("Failed to lock stdin: {}", e)))?;
        writeln!(stdin, "{}", notification_json).map_err(|e| {
            SofosError::McpError(format!(
                "Failed to write notification to MCP server '{}': {}",
                self.server_name, e
            ))
        })?;
        stdin.flush().map_err(|e| {
            SofosError::McpError(format!(
                "Failed to flush stdin for MCP server '{}': {}",
                self.server_name, e
            ))
        })?;

        Ok(())
    }

    pub async fn list_tools(&self) -> Result<Vec<McpTool>> {
        impl_list_tools!(self)
    }

    pub async fn call_tool(&self, name: &str, arguments: Option<Value>) -> Result<CallToolResult> {
        impl_call_tool!(self, name, arguments)
    }
}

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

        let client = reqwest::Client::new();

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
        impl_list_tools!(self)
    }

    pub async fn call_tool(&self, name: &str, arguments: Option<Value>) -> Result<CallToolResult> {
        impl_call_tool!(self, name, arguments)
    }
}
