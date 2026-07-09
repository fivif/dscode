//! Tool trait definition — standard interface for all tools in the registry.

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::sync::Arc;

use crate::agent::stream::StreamEvent;
use crate::providers::trait_def::ToolDef;
use crate::safety::guard::SafetyGuard;

/// Context passed to every tool invocation, providing the session environment
/// and a channel to emit streaming events back to the agent loop.
#[derive(Debug, Clone)]
pub struct ToolContext {
    /// The working directory for resolving relative paths.
    pub working_dir: PathBuf,
    /// The current session identifier.
    pub session_id: String,
    /// The unique ID for this tool call (used for ToolStart/ToolProgress/ToolEnd events).
    pub tool_call_id: String,
    /// Channel to emit StreamEvents (e.g. ToolProgress, ToolEnd).
    pub sender: tokio::sync::mpsc::UnboundedSender<StreamEvent>,
    /// Safety guard for command and path validation during tool execution.
    pub safety_guard: Arc<SafetyGuard>,
}

/// The result of a single tool invocation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolResult {
    /// Whether the tool completed successfully.
    pub success: bool,
    /// Human-readable output from the tool (stdout, file contents, etc.).
    pub output: String,
    /// Error message if the tool failed.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

impl ToolResult {
    /// Create a successful result.
    pub fn ok(output: impl Into<String>) -> Self {
        Self {
            success: true,
            output: output.into(),
            error: None,
        }
    }

    /// Create a failure result with an error message.
    pub fn err(output: impl Into<String>, error: impl Into<String>) -> Self {
        Self {
            success: false,
            output: output.into(),
            error: Some(error.into()),
        }
    }
}

/// The core Tool trait — every tool in the agent's registry implements this.
///
/// Tools are async, must be `Send + Sync` (registry stores them behind `Arc`),
/// and expose their schema so the LLM can see them via `to_openai_tool()`.
#[async_trait]
pub trait Tool: Send + Sync {
    /// The unique name of this tool (e.g. "do_bash").
    fn name(&self) -> &str;

    /// A human-readable description of what the tool does.
    fn description(&self) -> &str;

    /// JSON Schema describing the tool's input parameters.
    fn parameters(&self) -> serde_json::Value;

    /// Execute the tool with the given JSON arguments.
    ///
    /// `args` is the parsed JSON object containing the tool's expected fields.
    /// `ctx` provides session context and the event sender for progress updates.
    async fn execute(
        &self,
        args: serde_json::Value,
        ctx: &ToolContext,
    ) -> Result<ToolResult, ToolError>;

    /// Convert this tool into an OpenAI-compatible `ToolDef` for the LLM API.
    fn to_openai_tool(&self) -> ToolDef {
        ToolDef::new(self.name(), self.description(), self.parameters())
    }
}

/// Errors that can occur during tool execution.
#[derive(Debug, thiserror::Error)]
pub enum ToolError {
    #[error("Tool not found: {0}")]
    NotFound(String),

    #[error("Missing required parameter: {0}")]
    MissingParameter(String),

    #[error("Invalid parameter value for '{name}': {reason}")]
    InvalidParameter { name: String, reason: String },

    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),

    #[error("Command timed out after {0}s")]
    Timeout(u64),

    #[error("Path outside working directory: {0}")]
    PathEscape(String),

    #[error("File not found: {0}")]
    FileNotFound(String),

    #[error("Edit failed: {0}")]
    EditError(String),

    #[error("Internal error: {0}")]
    Internal(String),
}
