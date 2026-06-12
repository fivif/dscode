//! Multi-agent team dispatch — split complex tasks across sub-agents.
//!
//! The Teams system takes a high-level task description, uses an LLM to
//! decompose it into subtasks, spawns independent Tokio tasks for each
//! sub-agent (each running its own [`crate::agent::forge::Forge`] instance),
//! monitors their progress, and merges results with conflict resolution.

pub mod dispatcher;
pub mod monitor;
pub mod orchestrator;

pub use dispatcher::{Dispatcher, SubTask, TaskAssignments};
pub use monitor::Monitor;
pub use orchestrator::Orchestrator;
