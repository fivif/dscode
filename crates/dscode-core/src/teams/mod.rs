//! Multi-agent team orchestration (Teams v2).
//!
//! Production path: [`runtime::TeamRuntime`] (pure `/teams` chat mode).
//! `/auto` and `/auto`+TEAM stay on [`crate::auto::runner::AutoRunner`] (never TeamRuntime).
//!
//! [`dispatcher`] / [`orchestrator`] kept for unit tests and heuristic utilities only
//! (not on the production chat path).

pub mod board;
pub mod config;
pub mod control;
pub mod dispatcher;
pub mod merge;
pub mod monitor;
pub mod orchestrator;
pub mod ownership;
pub mod phase;
pub mod role;
pub mod runtime;
pub mod schema;

pub use board::{TaskBoard, TaskSpec, TaskStatus, WaveKind};
pub use config::TeamsConfig;
pub use control::{global_control_planes, TeamControlPlane};
pub use dispatcher::{Dispatcher, SubTask, TaskAssignments};
pub use monitor::Monitor;
pub use orchestrator::Orchestrator;
pub use role::AgentRole;
pub use runtime::TeamRuntime;
