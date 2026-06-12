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
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, Command};
use tokio::time::timeout;

// ---------------------------------------------------------------------------
// JSON-RPC 2.0 Types
// ---------------------------------------------------------------------------

/// A JSON-RPC 2.0 request.
#[derive(Debug, Clone, Serialize)]
struct JsonRpcRequest {
    jsonrpc: String,
    id: u64,
    method: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    params: Option<serde_json::Value>,
}

/// A JSON-RPC 2.0 response (success or error).
#[derive(Debug, Clone, Deserialize)]
struct JsonRpcResponse {
    #[serde(default)]
    id: Option<u64>,
    #[serde(default)]
    result: Option<serde_json::Value>,
    #[serde(default)]
    error: Option<JsonRpcError>,
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
    /// JSON Schema for the tool's input parameters.
    #[serde(default)]
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

    /// Connect to the MCP server, spawning the subprocess, performing the
    /// initialize handshake, and sending the required initialized notification.
    pub async fn connect(&self) -> Result<McpConnection, McpError> {
        let mut cmd = Command::new(&self.command);
        cmd.args(&self.args);
        cmd.stdin(Stdio::piped());
        cmd.stdout(Stdio::piped());
        cmd.stderr(Stdio::piped());
        cmd.kill_on_drop(true);

        // Set environment variables
        for (key, value) in &self.env {
            cmd.env(key, value);
        }

        let mut child = cmd.spawn()?;

        let stdin = child.stdin.take();
        let stdout = child.stdout.take();

        let stdin = stdin.ok_or_else(|| McpError::Protocol("Failed to capture stdin".into()))?;
        let stdout = stdout.ok_or_else(|| McpError::Protocol("Failed to capture stdout".into()))?;

        let stdin_writer = tokio::io::BufWriter::new(stdin);

        let mut conn = McpConnection {
            server_name: self.server_name.clone(),
            child,
            stdin: Some(stdin_writer),
            reader: BufReader::new(stdout),
            next_id: 1,
        };

        // Perform the initialize handshake automatically on connect
        conn.initialize().await?;

        // MCP spec requires sending notifications/initialized after initialize
        // handshake completes (E1)
        conn.send_notification("notifications/initialized", None).await?;

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
#[derive(Debug)]
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
}

impl McpConnection {
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

        self.send_request("initialize", Some(params)).await
    }

    /// List all tools provided by this MCP server.
    ///
    /// Assumes the handshake has already been performed (connect() does this).
    pub async fn list_tools(&mut self) -> Result<Vec<McpToolDef>, McpError> {
        // Request tool list
        let response = self.send_request("tools/list", None).await?;

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
        let notification = serde_json::json!({
            "jsonrpc": "2.0",
            "method": method,
            "params": params
        });

        let json = serde_json::to_string(&notification)?;
        self.write_message(&json).await
    }

    /// Send a JSON-RPC request and wait for the response.
    ///
    /// Uses MCP stdio framing: writes a Content-Length header then the JSON
    /// body, and reads the response using the same framing. Includes a timeout
    /// to prevent hanging on unresponsive servers.
    async fn send_request(
        &mut self,
        method: &str,
        params: Option<serde_json::Value>,
    ) -> Result<serde_json::Value, McpError> {
        let id = self.next_id;
        self.next_id += 1;

        let request = JsonRpcRequest {
            jsonrpc: "2.0".into(),
            id,
            method: method.to_string(),
            params,
        };

        let json = serde_json::to_string(&request)?;
        self.write_message(&json).await?;

        // Read response with timeout (E2)
        let response_text = timeout(Duration::from_secs(30), self.read_message()).await
            .map_err(|_| McpError::Timeout)?
            .map_err(|e| e)?;

        let response: JsonRpcResponse = serde_json::from_str(&response_text)?;

        if let Some(err) = response.error {
            return Err(McpError::ServerError {
                code: err.code,
                message: err.message,
            });
        }

        response
            .result
            .ok_or_else(|| McpError::Protocol("Response missing result".into()))
    }

    /// Write a JSON message using MCP stdio framing (Content-Length header).
    async fn write_message(&mut self, json: &str) -> Result<(), McpError> {
        let framed = format!("Content-Length: {}\r\n\r\n{}", json.len(), json);

        if let Some(ref mut stdin) = self.stdin {
            stdin.write_all(framed.as_bytes()).await?;
            stdin.flush().await?;
            Ok(())
        } else {
            Err(McpError::Protocol("stdin is closed".into()))
        }
    }

    /// Read a JSON message using MCP stdio framing.
    ///
    /// Reads a Content-Length header line, then reads exactly that many bytes
    /// of JSON body (E3 — supports multi-line JSON responses).
    async fn read_message(&mut self) -> Result<String, McpError> {
        // Read the Content-Length header line
        let mut header_line = String::new();
        match self.reader.read_line(&mut header_line).await {
            Ok(0) => {
                let status = self.child.wait().await;
                return Err(McpError::ProcessExited(format!(
                    "MCP server '{}' exited with {:?}",
                    self.server_name, status
                )));
            }
            Ok(_) => {}
            Err(e) => return Err(McpError::SpawnError(e)),
        }

        let header = header_line.trim();
        if !header.starts_with("Content-Length:") {
            return Err(McpError::Protocol(format!(
                "Expected Content-Length header, got: {}",
                header
            )));
        }

        let length_str = header["Content-Length:".len()..].trim();
        let length: usize = length_str
            .parse()
            .map_err(|_| McpError::Protocol(format!("Invalid Content-Length: {}", length_str)))?;

        // Read the empty line separator (\r\n)
        let mut separator = String::new();
        self.reader.read_line(&mut separator).await?;

        // Read exactly `length` bytes of JSON body
        let mut body = vec![0u8; length];
        self.reader.read_exact(&mut body).await?;

        String::from_utf8(body).map_err(|e| McpError::Protocol(format!("Invalid UTF-8: {}", e)))
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
            id: 1,
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
        assert_eq!(resp.id, Some(1));
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
