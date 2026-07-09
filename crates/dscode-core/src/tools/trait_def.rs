//! Tool trait definition — standard interface for all tools in the registry.

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::sync::Arc;

use crate::agent::stream::StreamEvent;
use crate::providers::trait_def::ToolDef;
use crate::safety::guard::SafetyGuard;
use crate::safety::permission::PermissionHub;

/// Context passed to every tool invocation, providing the session environment
/// and a channel to emit streaming events back to the agent loop.
#[derive(Clone)]
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
    /// Optional interactive permission hub (desktop GUI). Absent in CLI/tests.
    pub permission_hub: Option<Arc<PermissionHub>>,
    /// Seconds to wait for user permission (Safe mode).
    pub permission_timeout_secs: u64,
    /// Teams v2: sub-agent id for path ownership (optional).
    pub team_agent_id: Option<String>,
    /// Shared file ownership map (optional).
    pub file_ownership: Option<Arc<tokio::sync::Mutex<crate::teams::ownership::FileOwnership>>>,
    /// When true, enforce owned_paths (Denied on violation unless soft).
    pub ownership_enforced: bool,
    /// When true with enforced, only warn (do not block write).
    pub ownership_soft_log_only: bool,
    /// Paths already read this agent turn (read-before-edit).
    pub read_paths: Option<Arc<tokio::sync::Mutex<std::collections::HashSet<String>>>>,
    /// Enforce read-before-edit for write/edit tools.
    pub read_before_edit: bool,
}

impl std::fmt::Debug for ToolContext {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ToolContext")
            .field("working_dir", &self.working_dir)
            .field("session_id", &self.session_id)
            .field("tool_call_id", &self.tool_call_id)
            .field("absolute_trust", &self.safety_guard.absolute_trust)
            .field("has_permission_hub", &self.permission_hub.is_some())
            .field("team_agent_id", &self.team_agent_id)
            .field("ownership_enforced", &self.ownership_enforced)
            .field("read_before_edit", &self.read_before_edit)
            .finish()
    }
}

impl ToolContext {
    /// Minimal context for tests / simple callers.
    pub fn simple(
        working_dir: PathBuf,
        session_id: impl Into<String>,
        tool_call_id: impl Into<String>,
        sender: tokio::sync::mpsc::UnboundedSender<StreamEvent>,
        safety_guard: Arc<SafetyGuard>,
    ) -> Self {
        Self {
            working_dir,
            session_id: session_id.into(),
            tool_call_id: tool_call_id.into(),
            sender,
            safety_guard,
            permission_hub: None,
            permission_timeout_secs: 120,
            team_agent_id: None,
            file_ownership: None,
            ownership_enforced: false,
            ownership_soft_log_only: true,
            read_paths: None,
            read_before_edit: false,
        }
    }
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
