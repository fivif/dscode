//! DS Code shared server logic.
//!
//! This crate contains the transport-agnostic backend used by both:
//! - `dscode-desktop` (Tauri shell — forwards events to Tauri emit)
//! - `dscode-web` (axum shell — forwards events over SSE)
//!
//! Command logic lives in [`commands`]; the shells only add thin transport
//! adapters, so the agent behavior is written **once** and shared.

pub mod app_state;
pub mod attachments;
pub mod commands;
pub mod event_bus;

pub use app_state::{ActiveForge, AppState};
pub use event_bus::{EventBus, ServerEvent};
