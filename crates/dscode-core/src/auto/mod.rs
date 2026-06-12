//! Auto runner — self-directed task execution loop.
//!
//! The auto runner takes a high-level PRD or task description, decomposes it
//! into subtasks, runs the MAGI three-brain spiral on each subtask, detects
//! stalls, and re-decomposes when progress plateaus. It runs autonomously
//! until the task is complete or the user interrupts.

pub mod runner;
pub mod decomposer;
pub mod stall;

pub use runner::{AutoError, AutoRunResult, AutoRunner, Subtask, SubtaskStatus};
pub use decomposer::decompose_task;
pub use stall::StallDetector;
