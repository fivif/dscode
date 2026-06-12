//! DS Code — Universal Code Agent Core Library
//!
//! This crate provides the complete agent engine:
//! - ReAct agent loop (Forge)
//! - 3-tier memory system (Scribe)
//! - Multi-provider LLM adapters
//! - Tool registry with sandboxed execution
//! - MAGI three-brain auto-spiral scheduler
//! - Two-layer knowledge wiki
//! - Multi-agent team dispatch
//! - Plan interview engine
//! - MCP + SKILLS extension system

pub mod agent;
pub mod auto;
pub mod config;
pub mod extensions;
pub mod magi;
pub mod memory;
pub mod plan;
pub mod providers;
pub mod safety;
pub mod session;
pub mod teams;
pub mod tools;
pub mod wiki;

pub mod prelude {
    pub use crate::agent::forge::Forge;
    pub use crate::agent::stream::StreamEvent;
    pub use crate::config::settings::Config;
    pub use crate::memory::scribe::Scribe;
    pub use crate::providers::openai::OpenAiProvider;
    pub use crate::providers::trait_def::LlmProvider;
    pub use crate::session::manager::SessionManager;
    pub use crate::tools::registry::ToolRegistry;
    pub use crate::tools::trait_def::Tool;
}
