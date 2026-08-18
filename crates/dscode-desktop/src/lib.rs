//! DS Code Desktop — Tauri GUI shell over the shared `dscode-server` backend.
//!
//! All agent/config/session behavior lives in `dscode-server`. This crate only
//! provides the thin Tauri command adapters (see [`shell`]) and the event
//! bridge from the shared EventBus into Tauri `emit`.

pub mod shell;
