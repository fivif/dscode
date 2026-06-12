//! Stream event types — the core communication protocol between agent and UI.

use serde::{Deserialize, Serialize};

/// The set of events emitted by the Forge agent loop during streaming execution.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum StreamEvent {
    /// Assistant thinking content (DeepSeek reasoning_content).
    Thinking {
        content: String,
        #[serde(default)]
        step: u32,
    },
    /// A markdown text token for streaming display.
    Token {
        content: String,
    },
    /// A tool call has started.
    ToolStart {
        id: String,
        name: String,
        #[serde(default)]
        description: String,
    },
    /// Streaming progress from a running tool.
    ToolProgress {
        id: String,
        chunk: String,
    },
    /// A tool call has completed.
    ToolEnd {
        id: String,
        #[serde(rename = "status")]
        status: ToolStatus,
        #[serde(default)]
        result: String,
    },
    /// A fact was extracted from the conversation.
    Fact {
        id: String,
        subject: String,
        predicate: String,
        object: String,
    },
    /// An error occurred.
    Error {
        content: String,
    },
    /// The agent has finished its turn.
    Complete {
        #[serde(default)]
        usage: Option<UsageInfo>,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolStatus {
    Success,
    Error,
    Running,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct UsageInfo {
    #[serde(default)]
    pub input_tokens: u64,
    #[serde(default)]
    pub output_tokens: u64,
    #[serde(default)]
    pub cache_read_tokens: u64,
    #[serde(default)]
    pub cache_write_tokens: u64,
}
