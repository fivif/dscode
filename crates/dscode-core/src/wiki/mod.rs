//! Two-layer knowledge wiki — the agent's persistent memory across sessions.
//!
//! The wiki engine maintains dual SQLite databases:
//! - **Global** (`~/.dscode/wiki/global.db`) — shared knowledge across all sessions.
//! - **Session** (`~/.dscode/wiki/<session_id>/session.db`) — session-scoped knowledge.
//!
//! Each database stores [`KnowledgeNode`]s with FTS5 full-text search indexing.
//! A background [`Ingestor`] extracts entities and relations from conversation
//! turns, and a [`Graph`] builder constructs weighted edges between nodes for
//! traversal and visualization (sigma.js / Quartz export).

pub mod engine;
pub mod export;
pub mod graph;
pub mod inductive;
pub mod ingestor;
pub mod search;

pub use engine::{Engine, KnowledgeNode, NodeType};
pub use graph::{Edge, Graph};
pub use ingestor::Ingestor;
pub use search::SearchResult;
