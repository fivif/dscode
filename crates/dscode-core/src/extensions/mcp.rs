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

/// Max bytes buffered from a single stderr line; the rest of the line is
/// consumed and discarded so the stream stays line-framed.
const MAX_STDERR_LINE_BYTES: usize = 64 * 1024;

/// Bytes of stderr kept for diagnostics.
const STDERR_TAIL_BYTES: usize = 4096;

/// Max bytes accepted for one stdout protocol message. NDJSON has no framing,
/// so without a cap a server that writes a 2 GB line with no newline OOMs us.
const MAX_MCP_MESSAGE_BYTES: usize = 16 * 1024 * 1024;

/// Default timeout for a `tools/call` round trip. Build/test/database servers
/// legitimately run for minutes; the old hard-coded 60 s aborted them and then
/// silently discarded their eventual (already-performed) response as an
/// unmatched id. Overridable with `DSCODE_MCP_TOOL_TIMEOUT_SECS`.
const DEFAULT_TOOL_CALL_TIMEOUT_SECS: u64 = 600;

fn resolve_tool_call_timeout() -> Duration {
    let secs = std::env::var("DSCODE_MCP_TOOL_TIMEOUT_SECS")
        .ok()
        .and_then(|v| v.trim().parse::<u64>().ok())
        .filter(|v| *v > 0)
        .unwrap_or(DEFAULT_TOOL_CALL_TIMEOUT_SECS);
    Duration::from_secs(secs)
}

/// Read one line into `out`, buffering at most `max` bytes; anything past the
/// cap is consumed and dropped so the stream stays line-framed.
/// Returns the number of bytes consumed (0 = EOF).
async fn read_line_bounded<R>(
    reader: &mut R,
    out: &mut Vec<u8>,
    max: usize,
) -> std::io::Result<usize>
where
    R: tokio::io::AsyncBufRead + Unpin,
{
    out.clear();
    let mut total = 0usize;
    let mut full = false;
    loop {
        let (chunk_len, done) = {
            let available = reader.fill_buf().await?;
            if available.is_empty() {
                return Ok(total);
            }
            let (chunk_len, done) = match available.iter().position(|&b| b == b'\n') {
                Some(i) => (i + 1, true),
                None => (available.len(), false),
            };
            if !full {
                let room = max.saturating_sub(out.len());
                let take = chunk_len.min(room);
                out.extend_from_slice(&available[..take]);
                if take < chunk_len {
                    full = true;
                }
            }
            (chunk_len, done)
        };
        total += chunk_len;
        reader.consume(chunk_len);
        if done {
            return Ok(total);
        }
    }
}

/// Keep at most the last `max` bytes of `s`.
///
/// `String::drain` panics when the index is not a char boundary; the old
/// `g.drain(..g.len() - 4096)` therefore panicked as soon as an MCP server
/// logged >4 KB of multi-byte text (e.g. Chinese), killing the stderr drain
/// task and wedging the server forever once the 64 KB pipe filled.
fn truncate_front(s: &mut String, max: usize) {
    if s.len() <= max {
        return;
    }
    let mut n = s.len() - max;
    while n < s.len() && !s.is_char_boundary(n) {
        n += 1;
    }
    s.drain(..n);
}

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

impl JsonRpcError {
    /// `message`, plus the `data` field when the server supplied one.
    ///
    /// MCP servers routinely put the actionable detail (`{ "trace": … }`, the
    /// offending argument, a stack) in `data` and leave `message` generic;
    /// parsing the field and then never rendering it threw that away.
    fn render(&self) -> String {
        match self.data.as_ref() {
            Some(d) if !d.is_null() => format!("{} — data: {d}", self.message),
            _ => self.message.clone(),
        }
    }
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
        // Hide the console window when the desktop app spawns an MCP server on Windows.
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            cmd.as_std_mut()
                .creation_flags(crate::tools::bash::CREATE_NO_WINDOW);
        }
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
                loop {
                    let mut raw: Vec<u8> = Vec::new();
                    match read_line_bounded(&mut reader, &mut raw, MAX_STDERR_LINE_BYTES).await {
                        Ok(0) => break,
                        Ok(_) => {
                            let text = String::from_utf8_lossy(&raw);
                            let mut g = tail.lock().await;
                            g.push_str(&text);
                            // Keep last ~4KB, on a char boundary.
                            truncate_front(&mut g, STDERR_TAIL_BYTES);
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
            tool_call_timeout: resolve_tool_call_timeout(),
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

    /// Timeout for a single `tools/call` round trip (see
    /// [`DEFAULT_TOOL_CALL_TIMEOUT_SECS`]).
    tool_call_timeout: Duration,
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
    /// Long-running tools are allowed up to [`Self::tool_call_timeout`] (10
    /// minutes by default, `DSCODE_MCP_TOOL_TIMEOUT_SECS` to override).
    pub async fn call_tool(
        &mut self,
        name: &str,
        arguments: serde_json::Value,
    ) -> Result<serde_json::Value, McpError> {
        let params = serde_json::json!({
            "name": name,
            "arguments": arguments
        });
        let wait = self.tool_call_timeout;
        self.send_request_timeout("tools/call", Some(params), wait)
            .await
    }

    /// Timeout applied to each `tools/call`.
    pub fn tool_call_timeout(&self) -> Duration {
        self.tool_call_timeout
    }

    /// Override the per-`tools/call` timeout for this connection.
    pub fn set_tool_call_timeout(&mut self, wait: Duration) {
        self.tool_call_timeout = wait;
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

            // A JSON-RPC error reply with `id: null` (parse error / invalid
            // request) can never match our request id. The old code fell
            // through to the id check, logged "other id" and kept waiting until
            // the full timeout, so the caller burned 60 s for an answer it had
            // already received.
            let id_is_null = matches!(&response.id, None | Some(serde_json::Value::Null));
            if id_is_null {
                if let Some(err) = response.error {
                    let hint = self.error_hint_async().await;
                    return Err(McpError::ServerError {
                        code: err.code,
                        message: format!("[response with id=null] {}{hint}", err.render()),
                    });
                }
                tracing::debug!("skip mcp message without id");
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
            }

            if let Some(err) = response.error {
                let hint = self.error_hint_async().await;
                return Err(McpError::ServerError {
                    code: err.code,
                    message: format!("{}{hint}", err.render()),
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
            let mut raw: Vec<u8> = Vec::new();
            let n = read_line_bounded(&mut self.reader, &mut raw, MAX_MCP_MESSAGE_BYTES)
                .await
                .map_err(McpError::SpawnError)?;
            if n == 0 {
                let status = self.child.wait().await;
                let hint = self.error_hint_async().await;
                return Err(McpError::ProcessExited(format!(
                    "MCP server '{}' exited with {:?}{hint}",
                    self.server_name, status
                )));
            }
            if n > MAX_MCP_MESSAGE_BYTES {
                let hint = self.error_hint_async().await;
                return Err(McpError::Protocol(format!(
                    "MCP message exceeded {MAX_MCP_MESSAGE_BYTES} bytes without a newline; \
                     refusing to buffer more (server may be stuck){hint}"
                )));
            }

            let line = String::from_utf8_lossy(&raw);
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
                if length > MAX_MCP_MESSAGE_BYTES {
                    return Err(McpError::Protocol(format!(
                        "Content-Length {length} exceeds the {MAX_MCP_MESSAGE_BYTES}-byte limit"
                    )));
                }

                loop {
                    let mut hdr: Vec<u8> = Vec::new();
                    let n = read_line_bounded(&mut self.reader, &mut hdr, 8192)
                        .await
                        .map_err(McpError::SpawnError)?;
                    if n == 0 || hdr.iter().all(|b| b.is_ascii_whitespace()) {
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
        // `kill_on_drop(true)` only signals the *direct* child: `npx -y server`
        // spawns npx → node, and killing npx leaves node holding ports/files for
        // the rest of the session (repeated config reloads stack orphans). Kill
        // the whole tree: taskkill /T on Windows, process group on Unix.
        if let Some(pid) = self.child.id() {
            crate::tools::bash::kill_process_tree(Some(pid));
        }
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

    #[test]
    fn test_truncate_front_does_not_split_codepoints() {
        // 3000 × 3 bytes = 9000 bytes of multi-byte text; the old
        // `drain(..len - 4096)` panicked here (4096 is not a char boundary).
        let mut s = "中".repeat(3000);
        truncate_front(&mut s, STDERR_TAIL_BYTES);
        assert!(s.len() <= STDERR_TAIL_BYTES + 3);
        assert!(s.chars().all(|c| c == '中'));
    }

    #[tokio::test]
    async fn test_read_line_bounded_caps_and_keeps_framing() {
        let data = format!("{}\nnext\n", "あ".repeat(10)).into_bytes();
        let mut reader = BufReader::new(&data[..]);
        let mut out: Vec<u8> = Vec::new();
        let n = read_line_bounded(&mut reader, &mut out, 8).await.unwrap();
        assert_eq!(n, 31, "whole over-long line must be consumed");
        assert!(out.len() <= 8);

        let mut out2: Vec<u8> = Vec::new();
        let n2 = read_line_bounded(&mut reader, &mut out2, 1024).await.unwrap();
        assert_eq!(n2, 5);
        assert_eq!(String::from_utf8(out2).unwrap(), "next\n");
    }
}
