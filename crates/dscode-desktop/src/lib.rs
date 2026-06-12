//! DS Code Desktop — Tauri GUI backend library.
//!
//! Provides:
//! - AppState management (config, sessions, tools, forge handle).
//! - Tauri IPC commands (chat, session, config, wiki).
//! - Stream event helpers for relaying agent events to the frontend.

pub mod app_state;
pub mod commands;
pub mod events;
