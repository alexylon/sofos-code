use crate::error::{Result, SofosError};
use crate::mcp::client::{
    MCP_REQUEST_TIMEOUT, create_call_tool_request, create_init_request, parse_call_tool_response,
    parse_list_tools_response,
};
use crate::mcp::config::McpServerConfig;
use crate::mcp::protocol::*;
use serde_json::Value;
use std::io::{BufRead, BufReader, Write};
use std::process::{Child, ChildStderr, ChildStdin, ChildStdout, Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

/// Take stdin/stdout/stderr from a freshly-spawned MCP child. Reaching
/// the `Err` branch is practically unreachable — `Command::spawn` with
/// `Stdio::piped()` on all three ends always populates the pipes — but
/// the type system doesn't enforce it. Owning the `Child` lets us reap
/// it internally on the impossible branch, so a `?` at the call site
/// can't accidentally leak a zombie via `Child::drop` (which doesn't
/// wait).
fn take_child_pipes(
    mut process: Child,
    server_name: &str,
) -> Result<(Child, ChildStdin, ChildStdout, ChildStderr)> {
    if let (Some(stdin), Some(stdout), Some(stderr)) = (
        process.stdin.take(),
        process.stdout.take(),
        process.stderr.take(),
    ) {
        return Ok((process, stdin, stdout, stderr));
    }
    let _ = process.kill();
    let _ = process.wait();
    Err(SofosError::McpError(format!(
        "Failed to acquire stdin/stdout/stderr for MCP server '{}'",
        server_name
    )))
}

/// Drain the child's stderr line-by-line on a blocking worker and route
/// each line through `tracing::debug!` tagged with the server name.
/// Servers reserve stdout for JSON-RPC and emit their own INFO/DEBUG
/// logs to stderr, so treating every line as a warning floods the
/// default-level (WARN) log with normal startup chatter. Real failures
/// still surface as WARN from the connect / list-tools paths in
/// `manager.rs`; raw stderr is opt-in via `RUST_LOG=debug`.
///
/// ANSI escapes are stripped before logging because tracing's default
/// formatter renders control bytes as `\x1b[…]` literals, which is what
/// the user actually sees on the terminal otherwise.
fn spawn_stderr_reader(server_name: String, stderr: ChildStderr) {
    tokio::task::spawn_blocking(move || {
        let reader = BufReader::new(stderr);
        for line in reader.lines() {
            match line {
                Ok(text) => {
                    let clean = strip_ansi_escapes(&text);
                    tracing::debug!(server = %server_name, "mcp stderr: {}", clean);
                }
                Err(e) => {
                    tracing::warn!(
                        server = %server_name,
                        "mcp stderr read failed: {}",
                        e
                    );
                    break;
                }
            }
        }
    });
}

/// Remove CSI sequences (`ESC [ … final-byte`) and the bare `ESC` so log
/// lines stay readable when the child wraps its output in ANSI styling.
/// Final bytes of a CSI run sit in `0x40..=0x7e`; we also tolerate a
/// stray `ESC` with no following bracket by skipping the next char.
fn strip_ansi_escapes(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        if c != '\x1b' {
            out.push(c);
            continue;
        }
        if let Some('[') = chars.next() {
            for cc in chars.by_ref() {
                if matches!(cc, '\x40'..='\x7e') {
                    break;
                }
            }
        }
    }
    out
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
///
/// Holds `request_lock` for the full write+read cycle: the previous
/// version locked stdin for the write, released, then locked stdout for
/// the read. If two requests overlapped, the read side could pick up
/// the *other* request's response — JSON-RPC ids let the caller catch
/// the mismatch, but only after deserialisation. Coupling the two
/// halves under one lock makes the contract straightforward: one
/// request fully completes before the next starts.
fn stdio_request_blocking(
    server_name: &str,
    request_lock: &Arc<Mutex<()>>,
    stdin: &Arc<Mutex<ChildStdin>>,
    stdout: &Arc<Mutex<BufReader<ChildStdout>>>,
    request_json: &str,
) -> Result<JsonRpcResponse> {
    let _request_guard = request_lock
        .lock()
        .map_err(|e| SofosError::McpError(format!("Failed to lock MCP request mutex: {}", e)))?;

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
    /// Serialises full request-response cycles. See `stdio_request_blocking`
    /// for why write+read must stay coupled.
    request_lock: Arc<Mutex<()>>,
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
            .stderr(Stdio::piped());

        for (key, value) in env_vars {
            cmd.env(key, value);
        }

        let process = cmd.spawn().map_err(|e| {
            SofosError::McpError(format!(
                "Failed to start MCP server '{}': {}",
                server_name, e
            ))
        })?;

        // `take_child_pipes` reaps the child if any pipe is missing,
        // so the `?` here can't leak a zombie. Once the child is
        // wrapped in `Self`, the `Drop` impl takes over.
        let (process, stdin, stdout, stderr) = take_child_pipes(process, &server_name)?;

        // Drain stderr into `tracing` so server diagnostics aren't
        // silently dropped on the floor.
        spawn_stderr_reader(server_name.clone(), stderr);

        let client = Self {
            server_name: server_name.clone(),
            process: Arc::new(Mutex::new(process)),
            stdin: Arc::new(Mutex::new(stdin)),
            stdout: Arc::new(Mutex::new(BufReader::new(stdout))),
            request_lock: Arc::new(Mutex::new(())),
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
        let request_lock = Arc::clone(&self.request_lock);
        let stdin = Arc::clone(&self.stdin);
        let stdout = Arc::clone(&self.stdout);
        let response = self
            .run_with_timeout("request", move || {
                stdio_request_blocking(&server_name, &request_lock, &stdin, &stdout, &request_json)
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

#[cfg(test)]
mod tests {
    use super::strip_ansi_escapes;

    #[test]
    fn strips_csi_color_run() {
        let input = "\x1b[2m2026-05-15T22:34:54.965614Z\x1b[0m \x1b[32m INFO\x1b[0m start";
        assert_eq!(
            strip_ansi_escapes(input),
            "2026-05-15T22:34:54.965614Z  INFO start"
        );
    }

    #[test]
    fn passes_plain_text_through() {
        assert_eq!(strip_ansi_escapes("no escapes here"), "no escapes here");
    }

    #[test]
    fn drops_bare_escape() {
        assert_eq!(strip_ansi_escapes("a\x1bXb"), "ab");
    }
}
