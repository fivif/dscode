//! MCP (Model Context Protocol) client — connects to external tool servers.
//!
//! The MCP client manages a subprocess-based connection to an MCP-compatible
//! server. It communicates via JSON-RPC 2.0 over stdin/stdout. Once connected,
//! it can list available tools and forward tool calls from the agent to the
//! external server.
//!
//! See <https://modelcontextprotocol.io> for the protocol specification.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::process::Stdio;
use std::sync::Arc;
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, Command};
use tokio::sync::Mutex;
use tokio::time::timeout;

// ---------------------------------------------------------------------------
// JSON-RPC 2.0 Types
// ---------------------------------------------------------------------------

/// A JSON-RPC 2.0 request.
#[derive(Debug, Clone, Serialize)]
struct JsonRpcRequest {
    jsonrpc: String,
    id: serde_json::Value,
    method: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    params: Option<serde_json::Value>,
}

/// A JSON-RPC 2.0 response (success or error).
#[derive(Debug, Clone, Deserialize)]
struct JsonRpcResponse {
    #[serde(default)]
    id: Option<serde_json::Value>,
    #[serde(default)]
    result: Option<serde_json::Value>,
    #[serde(default)]
    error: Option<JsonRpcError>,
    /// Present on server→client requests/notifications (skip when waiting for result).
    #[serde(default)]
    method: Option<String>,
}

/// A JSON-RPC 2.0 error payload.
#[derive(Debug, Clone, Deserialize)]
struct JsonRpcError {
    code: i64,
    message: String,
    #[serde(default)]
    data: Option<serde_json::Value>,
}

/// A tool definition returned by an MCP server.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpToolDef {
    /// The name of the tool.
    pub name: String,
    /// A description of what the tool does.
    #[serde(default)]
    pub description: String,
    /// JSON Schema for the tool's input parameters (MCP uses camelCase `inputSchema`).
    #[serde(default, alias = "inputSchema", rename = "inputSchema")]
    pub input_schema: serde_json::Value,
}

impl McpToolDef {
    /// Convert to a provider-agnostic ToolDef for the agent registry.
    pub fn to_tool_def(&self) -> crate::providers::trait_def::ToolDef {
        crate::providers::trait_def::ToolDef::new(
            &format!("mcp_{}", self.name),
            &self.description,
            self.input_schema.clone(),
        )
    }
}

// ---------------------------------------------------------------------------
// MCP Errors
// ---------------------------------------------------------------------------

/// Errors that can occur during MCP client operation.
#[derive(Debug, thiserror::Error)]
pub enum McpError {
    #[error("Failed to spawn MCP server process: {0}")]
    SpawnError(#[from] std::io::Error),

    #[error("MCP server process exited unexpectedly: {0}")]
    ProcessExited(String),

    #[error("JSON-RPC protocol error: {0}")]
    Protocol(String),

    #[error("MCP server returned an error: code={code}, message={message}")]
    ServerError { code: i64, message: String },

    #[error("JSON serialization error: {0}")]
    Json(#[from] serde_json::Error),

    #[error("Timeout waiting for MCP server response")]
    Timeout,
}

// ---------------------------------------------------------------------------
// MCP Client
// ---------------------------------------------------------------------------

/// Configuration for an MCP server connection.
///
/// This mirrors the [`McpServerConfig`] from the settings system.
#[derive(Debug, Clone)]
pub struct McpClient {
    /// Human-readable name for this MCP server.
    pub server_name: String,

    /// The command to execute (e.g., "npx", "node", "python").
    pub command: String,

    /// Arguments to pass to the command.
    pub args: Vec<String>,

    /// Environment variables to set for the child process.
    pub env: HashMap<String, String>,
}

impl McpClient {
    /// Create a new MCP client configuration.
    pub fn new(
        server_name: impl Into<String>,
        command: impl Into<String>,
    ) -> Self {
        Self {
            server_name: server_name.into(),
            command: command.into(),
            args: Vec::new(),
            env: HashMap::new(),
        }
    }

    /// Add arguments to the command.
    pub fn with_args(mut self, args: Vec<String>) -> Self {
        self.args = args;
        self
    }

    /// Add an environment variable.
    pub fn with_env(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.env.insert(key.into(), value.into());
        self
    }

    /// Replace the env map entirely.
    pub fn with_env_map(mut self, env: HashMap<String, String>) -> Self {
        self.env = env;
        self
    }

    /// Connect to the MCP server, spawning the subprocess, performing the
    /// initialize handshake, and sending the required initialized notification.
    pub async fn connect(&self) -> Result<McpConnection, McpError> {
        self.connect_with_proxy(None).await
    }

    /// Connect with optional HTTP(S) proxy env for the child process (npx/node).
    pub async fn connect_with_proxy(
        &self,
        proxy_url: Option<&str>,
    ) -> Result<McpConnection, McpError> {
        let mut cmd = Command::new(&self.command);
        cmd.args(&self.args);
        cmd.stdin(Stdio::piped());
        cmd.stdout(Stdio::piped());
        cmd.stderr(Stdio::piped());
        cmd.kill_on_drop(true);
        // Inherit parent env (PATH/HOME/etc.) so GUI-launched npx/node works.
        // Ensure a usable PATH even when app is launched without shell profile.
        if let Ok(path) = std::env::var("PATH") {
            let extras = [
                "/usr/local/bin",
                "/opt/homebrew/bin",
                "/usr/bin",
                "/bin",
            ];
            let mut parts: Vec<String> = path.split(':').map(|s| s.to_string()).collect();
            for e in extras {
                if !parts.iter().any(|p| p == e) {
                    parts.push(e.into());
                }
            }
            cmd.env("PATH", parts.join(":"));
        }
        for (key, value) in &self.env {
            cmd.env(key, value);
        }
        crate::config::settings::apply_proxy_env_tokio(&mut cmd, proxy_url);
        if let Some(url) = proxy_url.map(str::trim).filter(|u| !u.is_empty()) {
            // npm / npx / node fetch
            cmd.env("npm_config_proxy", url);
            cmd.env("npm_config_https_proxy", url);
            cmd.env("NODE_USE_ENV_PROXY", "1");
        }

        let mut child = cmd.spawn().map_err(|e| {
            McpError::SpawnError(std::io::Error::new(
                e.kind(),
                format!(
                    "无法启动 MCP 命令 `{} {}`: {e}（请确认 PATH 中有 npx/node）",
                    self.command,
                    self.args.join(" ")
                ),
            ))
        })?;

        let stdin = child.stdin.take();
        let stdout = child.stdout.take();
        let stderr = child.stderr.take();

        // Drain stderr so the process never blocks on a full pipe; keep a tail for errors
        let stderr_tail = Arc::new(Mutex::new(String::new()));
        if let Some(err) = stderr {
            let tail = Arc::clone(&stderr_tail);
            tokio::spawn(async move {
                let mut reader = BufReader::new(err);
                let mut line = String::new();
                loop {
                    line.clear();
                    match reader.read_line(&mut line).await {
                        Ok(0) => break,
                        Ok(_) => {
                            let mut g = tail.lock().await;
                            g.push_str(&line);
                            // Keep last ~4KB
                            if g.len() > 4096 {
                                let drop_n = g.len() - 4096;
                                g.drain(..drop_n);
                            }
                        }
                        Err(_) => break,
                    }
                }
            });
        }

        let stdin = stdin.ok_or_else(|| McpError::Protocol("Failed to capture stdin".into()))?;
        let stdout = stdout.ok_or_else(|| McpError::Protocol("Failed to capture stdout".into()))?;

        let stdin_writer = tokio::io::BufWriter::new(stdin);

        let mut conn = McpConnection {
            server_name: self.server_name.clone(),
            child,
            stdin: Some(stdin_writer),
            reader: BufReader::new(stdout),
            next_id: 1,
            stderr_tail,
            proxy_used: proxy_url.map(|s| s.to_string()),
        };

        // First handshake can be slow (npx download through proxy)
        let init_timeout = Duration::from_secs(120);
        match timeout(init_timeout, conn.initialize()).await {
            Ok(Ok(_)) => {}
            Ok(Err(e)) => {
                let hint = conn.error_hint();
                return Err(McpError::Protocol(format!("{e}{hint}")));
            }
            Err(_) => {
                let hint = conn.error_hint();
                return Err(McpError::Protocol(format!(
                    "initialize 超时（{init_timeout:?}）。npx 首次下载或代理不通时常见。{hint}"
                )));
            }
        }

        // MCP spec requires sending notifications/initialized after initialize
        conn.send_notification("notifications/initialized", None)
            .await
            .map_err(|e| {
                let hint = conn.error_hint();
                McpError::Protocol(format!("notifications/initialized failed: {e}{hint}"))
            })?;

        Ok(conn)
    }

    /// Connect and immediately list available tools.
    ///
    /// This is the most common pattern: connect to an MCP server and discover
    /// what tools it provides.
    pub async fn list_tools(&self) -> Result<Vec<McpToolDef>, McpError> {
        let mut conn = self.connect().await?;
        conn.list_tools().await
    }
}

// ---------------------------------------------------------------------------
// MCP Connection
// ---------------------------------------------------------------------------

/// An active, initialized connection to an MCP server.
///
/// This holds the child process and the stdin/stdout pipes. The connection
/// is initialized with a handshake (`initialize` request) once connected.
///
/// When dropped, the child process is killed.
pub struct McpConnection {
    /// Human-readable server name.
    pub server_name: String,

    /// The spawned child process.
    child: Child,

    /// Buffered writer for stdin (None after the connection is closed).
    stdin: Option<tokio::io::BufWriter<tokio::process::ChildStdin>>,

    /// Buffered reader for stdout.
    reader: BufReader<tokio::process::ChildStdout>,

    /// Monotonically increasing JSON-RPC request ID.
    next_id: u64,

    /// Tailed stderr from the child (for diagnostics).
    stderr_tail: Arc<Mutex<String>>,

    /// Proxy URL used for this connection, if any.
    proxy_used: Option<String>,
}

impl McpConnection {
    fn error_hint(&self) -> String {
        // Try non-blocking peek of stderr — best-effort in async context
        let mut parts = Vec::new();
        if let Some(ref p) = self.proxy_used {
            parts.push(format!(" proxy={p}"));
        }
        // Can't easily lock here from sync without block_on — use try_lock
        if let Ok(g) = self.stderr_tail.try_lock() {
            let t = g.trim();
            if !t.is_empty() {
                let tail: String = t.chars().rev().take(600).collect::<String>().chars().rev().collect();
                parts.push(format!(" stderr: {tail}"));
            }
        }
        if parts.is_empty() {
            String::new()
        } else {
            parts.join("")
        }
    }

    async fn error_hint_async(&self) -> String {
        let mut parts = Vec::new();
        if let Some(ref p) = self.proxy_used {
            parts.push(format!(" proxy={p}"));
        }
        let g = self.stderr_tail.lock().await;
        let t = g.trim();
        if !t.is_empty() {
            let tail: String = t.chars().rev().take(800).collect::<String>().chars().rev().collect();
            parts.push(format!("\n--- MCP stderr ---\n{tail}"));
        }
        if parts.is_empty() {
            String::new()
        } else {
            parts.join("")
        }
    }

    /// Send the MCP `initialize` request and receive the server's capabilities.
    pub async fn initialize(&mut self) -> Result<serde_json::Value, McpError> {
        let params = serde_json::json!({
            "protocolVersion": "2024-11-05",
            "capabilities": {
                "tools": {}
            },
            "clientInfo": {
                "name": "dscode",
                "version": "0.1.0"
            }
        });

        self.send_request_timeout("initialize", Some(params), Duration::from_secs(120))
            .await
    }

    /// List all tools provided by this MCP server.
    ///
    /// Assumes the handshake has already been performed (connect() does this).
    pub async fn list_tools(&mut self) -> Result<Vec<McpToolDef>, McpError> {
        // Empty object params — some servers reject omitted params
        let response = self
            .send_request("tools/list", Some(serde_json::json!({})))
            .await?;

        let tools_array = response
            .get("tools")
            .and_then(|v| v.as_array())
            .ok_or_else(|| McpError::Protocol("tools/list response missing 'tools' array".into()))?;

        let tools: Result<Vec<McpToolDef>, _> = tools_array
            .iter()
            .map(|v| serde_json::from_value(v.clone()).map_err(McpError::Json))
            .collect();

        tools
    }

    /// Call a specific tool on the MCP server.
    ///
    /// Sends a `tools/call` request with the tool name and arguments.
    pub async fn call_tool(
        &mut self,
        name: &str,
        arguments: serde_json::Value,
    ) -> Result<serde_json::Value, McpError> {
        let params = serde_json::json!({
            "name": name,
            "arguments": arguments
        });
        self.send_request("tools/call", Some(params)).await
    }

    /// Check if the child process is still running.
    pub fn is_alive(&mut self) -> bool {
        match self.child.try_wait() {
            Ok(None) => true,
            _ => false,
        }
    }

    // ------------------------------------------------------------------
    // Internal wire protocol
    // ------------------------------------------------------------------

    /// Send a JSON-RPC 2.0 notification (no id field, no response expected).
    async fn send_notification(
        &mut self,
        method: &str,
        params: Option<serde_json::Value>,
    ) -> Result<(), McpError> {
        let mut notification = serde_json::json!({
            "jsonrpc": "2.0",
            "method": method,
        });
        if let Some(p) = params {
            notification["params"] = p;
        } else {
            notification["params"] = serde_json::json!({});
        }

        let json = serde_json::to_string(&notification)?;
        self.write_message(&json).await
    }

    async fn send_request(
        &mut self,
        method: &str,
        params: Option<serde_json::Value>,
    ) -> Result<serde_json::Value, McpError> {
        self.send_request_timeout(method, params, Duration::from_secs(60))
            .await
    }

    /// Send a JSON-RPC request and wait for the **matching id** response
    /// (skipping server notifications / unrelated messages).
    async fn send_request_timeout(
        &mut self,
        method: &str,
        params: Option<serde_json::Value>,
        wait: Duration,
    ) -> Result<serde_json::Value, McpError> {
        let id_num = self.next_id;
        self.next_id += 1;
        let id = serde_json::json!(id_num);

        let request = JsonRpcRequest {
            jsonrpc: "2.0".into(),
            id: id.clone(),
            method: method.to_string(),
            params,
        };

        let json = serde_json::to_string(&request)?;
        self.write_message(&json).await?;

        let deadline = tokio::time::Instant::now() + wait;
        loop {
            let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
            if remaining.is_zero() {
                let hint = self.error_hint_async().await;
                return Err(McpError::Protocol(format!(
                    "Timeout waiting for MCP response to `{method}` (id={id_num}){hint}"
                )));
            }

            let response_text = match timeout(remaining, self.read_message()).await {
                Ok(Ok(t)) => t,
                Ok(Err(e)) => {
                    let hint = self.error_hint_async().await;
                    return Err(McpError::Protocol(format!("{e}{hint}")));
                }
                Err(_) => {
                    let hint = self.error_hint_async().await;
                    return Err(McpError::Protocol(format!(
                        "Timeout waiting for MCP response to `{method}` (id={id_num}){hint}"
                    )));
                }
            };

            let response: JsonRpcResponse = match serde_json::from_str(&response_text) {
                Ok(r) => r,
                Err(e) => {
                    // Non-JSON noise slipped through — keep waiting
                    tracing::debug!(%e, body = %response_text.chars().take(120).collect::<String>(), "skip non-json mcp line");
                    continue;
                }
            };

            // Server→client request or notification: ignore while waiting for our result
            if response.method.is_some() && response.result.is_none() && response.error.is_none() {
                tracing::debug!(method = ?response.method, "skip mcp server message");
                continue;
            }

            // Match id (number or string form of same number)
            if let Some(ref rid) = response.id {
                let matches = rid == &id
                    || rid.as_i64() == Some(id_num as i64)
                    || rid.as_u64() == Some(id_num)
                    || rid.as_str() == Some(&id_num.to_string());
                if !matches {
                    tracing::debug!(?rid, expected = id_num, "skip mcp message with other id");
                    continue;
                }
            } else if response.result.is_none() && response.error.is_none() {
                continue;
            }

            if let Some(err) = response.error {
                let hint = self.error_hint_async().await;
                return Err(McpError::ServerError {
                    code: err.code,
                    message: format!("{}{hint}", err.message),
                });
            }

            if let Some(result) = response.result {
                return Ok(result);
            }

            // id matched but empty — keep reading
        }
    }

    /// Write a JSON-RPC message on stdio.
    ///
    /// Modern `@modelcontextprotocol/sdk` (v1.29+) uses **newline-delimited JSON**
    /// (`JSON.stringify(msg) + "\n"`), NOT LSP-style Content-Length framing.
    /// Content-Length is kept only as a legacy write path we no longer use by default.
    async fn write_message(&mut self, json: &str) -> Result<(), McpError> {
        // NDJSON: one JSON object per line (current MCP TS/JS SDK stdio transport)
        let framed = format!("{json}\n");

        if let Some(ref mut stdin) = self.stdin {
            stdin.write_all(framed.as_bytes()).await?;
            stdin.flush().await?;
            Ok(())
        } else {
            Err(McpError::Protocol("stdin is closed".into()))
        }
    }

    /// Read one MCP message.
    ///
    /// Primary: newline-delimited JSON (current SDK).
    /// Legacy: Content-Length frames (older servers).
    async fn read_message(&mut self) -> Result<String, McpError> {
        let mut skipped = 0u32;
        loop {
            let mut line = String::new();
            match self.reader.read_line(&mut line).await {
                Ok(0) => {
                    let status = self.child.wait().await;
                    let hint = self.error_hint_async().await;
                    return Err(McpError::ProcessExited(format!(
                        "MCP server '{}' exited with {:?}{hint}",
                        self.server_name, status
                    )));
                }
                Ok(_) => {}
                Err(e) => return Err(McpError::SpawnError(e)),
            }

            let trimmed = line.trim().trim_start_matches('\u{feff}');
            if trimmed.is_empty() {
                continue;
            }

            // NDJSON JSON-RPC (primary)
            if trimmed.starts_with('{') {
                return Ok(trimmed.to_string());
            }

            // Legacy Content-Length framing
            let lower = trimmed.to_ascii_lowercase();
            if lower.starts_with("content-length:") {
                let length_str = trimmed.split(':').nth(1).unwrap_or("").trim();
                let length: usize = length_str.parse().map_err(|_| {
                    McpError::Protocol(format!("Invalid Content-Length: {length_str}"))
                })?;

                loop {
                    let mut hdr = String::new();
                    let n = self.reader.read_line(&mut hdr).await?;
                    if n == 0 || hdr.trim().is_empty() {
                        break;
                    }
                }

                let mut body = vec![0u8; length];
                self.reader.read_exact(&mut body).await.map_err(|e| {
                    McpError::Protocol(format!("failed reading {length}-byte MCP body: {e}"))
                })?;
                return String::from_utf8(body)
                    .map_err(|e| McpError::Protocol(format!("Invalid UTF-8: {e}")));
            }

            skipped += 1;
            if skipped > 200 {
                let hint = self.error_hint_async().await;
                return Err(McpError::Protocol(format!(
                    "Expected JSON-RPC line after {skipped} lines; last={trimmed}{hint}"
                )));
            }
        }
    }
}

impl Drop for McpConnection {
    fn drop(&mut self) {
        // Close stdin first so the process can terminate cleanly
        drop(self.stdin.take());
        // The child will be killed on drop thanks to kill_on_drop(true)
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_mcp_client_config() {
        let client = McpClient::new("test-server", "echo")
            .with_args(vec!["hello".into()])
            .with_env("TEST", "value");

        assert_eq!(client.server_name, "test-server");
        assert_eq!(client.command, "echo");
        assert_eq!(client.args, vec!["hello"]);
        assert_eq!(client.env.get("TEST").unwrap(), "value");
    }

    #[test]
    fn test_mcp_tool_def_conversion() {
        let mcp_tool = McpToolDef {
            name: "search".into(),
            description: "Search the web".into(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "query": { "type": "string" }
                },
                "required": ["query"]
            }),
        };

        let tool_def = mcp_tool.to_tool_def();
        assert_eq!(tool_def.function.name, "mcp_search");
        assert_eq!(tool_def.function.description, Some("Search the web".into()));
    }

    #[test]
    fn test_mcp_error_display() {
        let err = McpError::ServerError {
            code: -32600,
            message: "Invalid Request".into(),
        };
        assert!(err.to_string().contains("-32600"));
        assert!(err.to_string().contains("Invalid Request"));
    }

    #[test]
    fn test_json_rpc_request_serialization() {
        let req = JsonRpcRequest {
            jsonrpc: "2.0".into(),
            id: serde_json::json!(1),
            method: "tools/list".into(),
            params: None,
        };
        let json = serde_json::to_string(&req).unwrap();
        assert!(json.contains("\"jsonrpc\":\"2.0\""));
        assert!(json.contains("\"method\":\"tools/list\""));
        assert!(json.contains("\"id\":1"));
    }

    #[test]
    fn test_json_rpc_response_deserialization() {
        let json = r#"{"jsonrpc":"2.0","id":1,"result":{"tools":[]}}"#;
        let resp: JsonRpcResponse = serde_json::from_str(json).unwrap();
        assert_eq!(resp.id, Some(serde_json::json!(1)));
        assert!(resp.result.is_some());
        assert!(resp.error.is_none());
    }

    #[test]
    fn test_json_rpc_error_response() {
        let json = r#"{"jsonrpc":"2.0","id":1,"error":{"code":-32600,"message":"Invalid Request"}}"#;
        let resp: JsonRpcResponse = serde_json::from_str(json).unwrap();
        assert!(resp.error.is_some());
        assert_eq!(resp.error.as_ref().unwrap().code, -32600);
    }
}
