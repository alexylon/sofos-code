use crate::error::{Result, SofosError};
use crate::mcp::config::McpServerConfig;
use crate::mcp::protocol::*;
use serde_json::Value;
use std::collections::HashMap;
use std::io::{BufRead, BufReader, Write};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// Ceiling on a single MCP request (stdio read + HTTP round-trip). A
/// misbehaving MCP server used to freeze every subsequent MCP call
/// because `BufRead::read_line` blocks indefinitely and the stdout
/// mutex serialises all requests. Two minutes is generous enough for
/// slow remote search backends (PubMed / ClinicalTrials.gov) while
/// still bounding a frozen server's blast radius to a single turn.
const MCP_REQUEST_TIMEOUT: Duration = Duration::from_secs(120);

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

/// Take the stdin and (buffered) stdout out of a freshly-spawned child.
/// Returning `Err` here is essentially unreachable because
/// `Command::spawn` with `Stdio::piped()` on both ends always populates
/// `Child::stdin` / `Child::stdout`, but we still handle it — leaking
/// a live `Child` out to `Drop` would fail to reap it (the std type
/// doesn't wait).
fn take_child_pipes(process: &mut Child, server_name: &str) -> Result<(ChildStdin, ChildStdout)> {
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
    Ok((stdin, stdout))
}

/// Write a single stdio message to the server (JSON-RPC request *or*
/// notification). Shared by `send_request` and `send_notification` so
/// the lock/write/flush sequence lives in one place.
fn stdio_write_blocking(
    server_name: &str,
    stdin: &Arc<Mutex<ChildStdin>>,
    payload: &str,
) -> Result<()> {
    let mut stdin_guard = stdin
        .lock()
        .map_err(|e| SofosError::McpError(format!("Failed to lock stdin: {}", e)))?;
    writeln!(stdin_guard, "{}", payload).map_err(|e| {
        SofosError::McpError(format!(
            "Failed to write to MCP server '{}': {}",
            server_name, e
        ))
    })?;
    stdin_guard.flush().map_err(|e| {
        SofosError::McpError(format!(
            "Failed to flush stdin for MCP server '{}': {}",
            server_name, e
        ))
    })?;
    Ok(())
}

/// Run one stdio MCP request on a worker thread. Owns no `self` so it
/// can be freely moved into `tokio::task::spawn_blocking`. Returns the
/// parsed `JsonRpcResponse` envelope (not the `result` payload) so the
/// caller can distinguish a server-reported error from a transport
/// failure.
fn stdio_request_blocking(
    server_name: &str,
    stdin: &Arc<Mutex<ChildStdin>>,
    stdout: &Arc<Mutex<BufReader<ChildStdout>>>,
    request_json: &str,
) -> Result<JsonRpcResponse> {
    stdio_write_blocking(server_name, stdin, request_json)?;

    let mut stdout_guard = stdout
        .lock()
        .map_err(|e| SofosError::McpError(format!("Failed to lock stdout: {}", e)))?;

    let mut response_line = String::new();
    let bytes_read = stdout_guard.read_line(&mut response_line).map_err(|e| {
        SofosError::McpError(format!(
            "Failed to read from MCP server '{}': {}",
            server_name, e
        ))
    })?;
    // Zero bytes from `read_line` means the server closed stdout
    // cleanly — typically a crash or exit between requests.
    // Surface that plainly so the user isn't chasing a bogus
    // "parse error" message for what's really a dead server.
    if bytes_read == 0 {
        return Err(SofosError::McpError(format!(
            "MCP server '{}' closed stdout unexpectedly (server crashed or exited?)",
            server_name
        )));
    }

    serde_json::from_str(&response_line).map_err(|e| {
        SofosError::McpError(format!(
            "Failed to parse response from MCP server '{}': {}",
            server_name, e
        ))
    })
}

pub struct StdioClient {
    server_name: String,
    process: Arc<Mutex<Child>>,
    stdin: Arc<Mutex<ChildStdin>>,
    stdout: Arc<Mutex<BufReader<ChildStdout>>>,
    next_id: Arc<AtomicU64>,
}

impl Drop for StdioClient {
    fn drop(&mut self) {
        // `Child::drop` does NOT wait on the subprocess, so without
        // this the MCP server lingers as a zombie until sofos itself
        // exits. A detached reap task spawned by `kill_child_detached`
        // may also be running when Drop fires — the mutex serialises
        // them, and the kill/wait pair is idempotent: a second `kill`
        // after the child already exited returns `InvalidInput`, and
        // a second `wait` after the child was already reaped returns
        // a harmless error. Both are discarded.
        if let Ok(mut child) = self.process.lock() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
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
            .stderr(Stdio::null());

        for (key, value) in env_vars {
            cmd.env(key, value);
        }

        let mut process = cmd.spawn().map_err(|e| {
            SofosError::McpError(format!(
                "Failed to start MCP server '{}': {}",
                server_name, e
            ))
        })?;

        // Take the pipes through a helper that reaps the child on
        // error, so a (practically-impossible but not-type-proven)
        // `None` from `take()` doesn't leak a zombie into the OS
        // process table. Once the child is wrapped in `Self`, the
        // `Drop` impl takes over.
        let (stdin, stdout) = match take_child_pipes(&mut process, &server_name) {
            Ok(pair) => pair,
            Err(e) => {
                let _ = process.kill();
                let _ = process.wait();
                return Err(e);
            }
        };

        let client = Self {
            server_name: server_name.clone(),
            process: Arc::new(Mutex::new(process)),
            stdin: Arc::new(Mutex::new(stdin)),
            stdout: Arc::new(Mutex::new(BufReader::new(stdout))),
            next_id: Arc::new(AtomicU64::new(1)),
        };

        client.initialize().await?;

        Ok(client)
    }

    /// Run a blocking closure with the shared MCP timeout ceiling. On
    /// timeout the child is killed off-thread so the async caller
    /// doesn't pause the executor waiting for the OS to reap it. Used
    /// by both `send_request` and `send_notification` so they share
    /// the same lock/panic/timeout error vocabulary.
    async fn run_with_timeout<T, F>(&self, label: &str, blocking: F) -> Result<T>
    where
        F: FnOnce() -> Result<T> + Send + 'static,
        T: Send + 'static,
    {
        let task = tokio::task::spawn_blocking(blocking);
        match tokio::time::timeout(MCP_REQUEST_TIMEOUT, task).await {
            Ok(Ok(Ok(value))) => Ok(value),
            Ok(Ok(Err(e))) => Err(e),
            Ok(Err(join_err)) => Err(SofosError::McpError(format!(
                "MCP worker panicked for server '{}' during {}: {}",
                self.server_name, label, join_err
            ))),
            Err(_) => {
                self.kill_child_detached();
                Err(SofosError::McpError(format!(
                    "MCP server '{}' {} timed out after {}s",
                    self.server_name,
                    label,
                    MCP_REQUEST_TIMEOUT.as_secs()
                )))
            }
        }
    }

    /// Kill + reap the child on a blocking thread without waiting for
    /// it to finish here. Used from async timeout handlers so a slow
    /// `Child::wait` (milliseconds in practice, but the call is
    /// synchronous) never pauses the tokio executor. Firing and
    /// forgetting is safe because tokio drains blocking tasks on
    /// runtime shutdown, and the `Drop` impl is idempotent against
    /// an already-reaped child.
    fn kill_child_detached(&self) {
        let process = Arc::clone(&self.process);
        tokio::task::spawn_blocking(move || {
            if let Ok(mut child) = process.lock() {
                let _ = child.kill();
                let _ = child.wait();
            }
        });
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

        let server_name = self.server_name.clone();
        let stdin = Arc::clone(&self.stdin);
        let stdout = Arc::clone(&self.stdout);
        let response = self
            .run_with_timeout("request", move || {
                stdio_request_blocking(&server_name, &stdin, &stdout, &request_json)
            })
            .await?;

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

        // Notifications are one-way (no response to read), but the
        // write itself can still block forever if the child has
        // wedged its read side. Same timeout path as `send_request`.
        let server_name = self.server_name.clone();
        let stdin = Arc::clone(&self.stdin);
        self.run_with_timeout("notification", move || {
            stdio_write_blocking(&server_name, &stdin, &notification_json)
        })
        .await
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
        impl_list_tools!(self)
    }

    pub async fn call_tool(&self, name: &str, arguments: Option<Value>) -> Result<CallToolResult> {
        impl_call_tool!(self, name, arguments)
    }
}
