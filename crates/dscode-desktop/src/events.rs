//! Stream event serialization helpers for Tauri IPC.
//!
//! [`StreamEvent`] is the core communication protocol between the Forge agent
//! loop and the frontend UI. This module re-exports the event type and provides
//! a convenience helper for emitting events through a Tauri [`AppHandle`].

use tauri::Emitter;
use tracing::warn;

pub use dscode_core::agent::stream::{StreamEvent, ToolStatus, UsageInfo};

/// Emit a [`StreamEvent`] to the frontend via the Tauri event system.
///
/// Includes the `session_id` in the payload so the frontend can filter
/// events for the active session only.
pub fn emit_event(app_handle: &tauri::AppHandle, event: &StreamEvent, session_id: &str) {
    let payload = serde_json::json!({
        "session_id": session_id,
        "event": event,
    });
    // Log team events explicitly for debugging.
    if matches!(event, StreamEvent::TeamAgentStart { .. } | StreamEvent::TeamAgentOutput { .. } | StreamEvent::TeamAgentEnd { .. }) {
        tracing::info!(?event, "emit_event: team event");
    }
    if let Err(e) = app_handle.emit("stream-event", payload) {
        warn!(%e, "Failed to emit stream-event to frontend");
    }
}

/// Emit a [`StreamEvent`] to a specific Tauri window.
pub fn emit_event_to(
    window: &tauri::Window,
    event: &StreamEvent,
) {
    let _ = window.emit("stream-event", event);
}
