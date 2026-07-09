//! DS Code Desktop — Tauri GUI backend library.
//!
//! Provides:
//! - AppState management (config, sessions, tools, forge handle).
//! - Tauri IPC commands (chat, session, config).
//! - Stream event helpers for relaying agent events to the frontend.

pub mod app_state;
pub mod attachments;
pub mod commands;
pub mod events;
