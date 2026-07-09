//! LLM Provider trait — standard interface for all model backends.

use async_trait::async_trait;
use futures::stream::Stream;
use serde::{Deserialize, Serialize};
use std::pin::Pin;

/// Standard message format (OpenAI/Claude compatible).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Message {
    pub role: Role,
    pub content: MessageContent,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_calls: Option<Vec<ToolCall>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
    /// DeepSeek reasoning_content (thinking mode)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reasoning_content: Option<String>,
    /// Unix timestamp (set when loaded from DB).
    #[serde(default)]
    pub created_at: i64,
}

impl Default for Message {
    fn default() -> Self {
        Self {
            role: Role::User, content: MessageContent::Text(String::new()),
            name: None, tool_calls: None, tool_call_id: None,
            reasoning_content: None, created_at: 0,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Role {
    System,
    User,
    Assistant,
    Tool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(untagged)]
pub enum MessageContent {
    Text(String),
    Parts(Vec<ContentPart>),
}

impl MessageContent {
    pub fn as_text(&self) -> Option<&str> {
        match self {
            MessageContent::Text(s) => Some(s),
            MessageContent::Parts(parts) => {
                for p in parts {
                    if let ContentPart::Text { text } = p {
                        return Some(text);
                    }
                }
                None
            }
        }
    }

    pub fn is_empty(&self) -> bool {
        self.as_text().map(|s| s.is_empty()).unwrap_or(true)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ContentPart {
    Text { text: String },
    ToolUse { id: String, name: String, input: serde_json::Value },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ToolCall {
    pub id: String,
    #[serde(rename = "type", default = "tool_type_default")]
    pub call_type: String,
    pub function: FunctionCall,
}

fn tool_type_default() -> String { "function".into() }

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct FunctionCall {
    pub name: String,
    pub arguments: String,
}

/// Tool definition (OpenAI format).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolDef {
    #[serde(rename = "type")]
    pub tool_type: String,
    pub function: FunctionDef,
}

fn tool_def_type() -> String { "function".into() }

impl ToolDef {
    pub fn new(name: &str, description: &str, parameters: serde_json::Value) -> Self {
        Self {
            tool_type: tool_def_type(),
            function: FunctionDef {
                name: name.to_string(),
                description: Some(description.to_string()),
                parameters: Some(parameters),
            },
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FunctionDef {
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parameters: Option<serde_json::Value>,
}

/// Streaming chunk from any LLM provider.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StreamChunk {
    /// Text delta token
    pub content: Option<String>,
    /// Tool call delta (for streaming tool calls)
    pub tool_calls: Option<Vec<ToolCallDelta>>,
    /// DeepSeek reasoning content
    pub reasoning_content: Option<String>,
    /// Is this the final chunk?
    pub finish_reason: Option<String>,
    /// Usage info (usually on final chunk)
    pub usage: Option<crate::agent::stream::UsageInfo>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolCallDelta {
    pub index: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub function: Option<FunctionDelta>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FunctionDelta {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub arguments: Option<String>,
}

/// Non-streaming chat response.
#[derive(Debug, Clone)]
pub struct ChatResponse {
    pub content: String,
    pub tool_calls: Vec<ToolCall>,
    pub usage: Option<crate::agent::stream::UsageInfo>,
    pub reasoning_content: Option<String>,
}

/// The core LLM Provider trait.
#[async_trait]
pub trait LlmProvider: Send + Sync {
    /// Send a chat completion request (non-streaming).
    async fn chat(
        &self,
        messages: Vec<Message>,
        tools: Vec<ToolDef>,
    ) -> Result<ChatResponse, ProviderError>;

    /// Send a chat completion request, returning a stream of chunks.
    async fn chat_stream(
        &self,
        messages: Vec<Message>,
        tools: Vec<ToolDef>,
    ) -> Result<Pin<Box<dyn Stream<Item = Result<StreamChunk, ProviderError>> + Send>>, ProviderError>;

    /// Clone this provider into a new boxed instance.
    /// Required so Forge can spawn sub-agents that each own their provider.
    fn clone_box(&self) -> Box<dyn LlmProvider>;
}

#[derive(Debug, Clone, thiserror::Error)]
pub enum ProviderError {
    #[error("HTTP error: {0}")]
    Http(String),
    #[error("API error: {status} - {message}")]
    Api { status: u16, message: String },
    #[error("Parse error: {0}")]
    Parse(String),
    #[error("No API key configured for this provider")]
    NoApiKey,
}

impl From<reqwest::Error> for ProviderError {
    fn from(e: reqwest::Error) -> Self {
        ProviderError::Http(e.to_string())
    }
}
