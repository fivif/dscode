//! Stream event serialization helpers for Tauri IPC.
//!
//! [`StreamEvent`] is the core communication protocol between the Forge agent
//! loop and the frontend UI. This module re-exports the event type and provides
//! a convenience helper for emitting events through a Tauri [`AppHandle`].

use tauri::Emitter;

pub use dscode_core::agent::stream::{StreamEvent, ToolStatus, UsageInfo};

/// Emit a [`StreamEvent`] to the frontend via the Tauri event system.
///
/// The event name is `"stream-event"` and the payload is the serialized
/// [`StreamEvent`]. Failures are silently ignored (the frontend may have
/// disconnected).
pub fn emit_event(app_handle: &tauri::AppHandle, event: &StreamEvent) {
    let _ = app_handle.emit("stream-event", event);
}

/// Emit a [`StreamEvent`] to a specific Tauri window.
pub fn emit_event_to(
    window: &tauri::Window,
    event: &StreamEvent,
) {
    let _ = window.emit("stream-event", event);
}
