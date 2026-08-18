//! Unified event bus shared by the desktop (Tauri) and web (axum) shells.
//!
//! The shared command logic only ever pushes events into this bus. Each shell
//! subscribes and forwards the events to its own transport:
//! - Desktop: Tauri `emit("stream-event" | "session-title-updated" | "task-notification")`
//! - Web:     Server-Sent Events over HTTP

use dscode_core::agent::stream::StreamEvent;
use dscode_core::tools::background::TaskNotification;
use serde::Serialize;
use tokio::sync::broadcast;

/// One outbound event, tagged so the frontend can route it to the right session.
#[derive(Debug, Clone, Serialize)]
pub enum ServerEvent {
    Stream {
        session_id: String,
        event: StreamEvent,
    },
    SessionTitleUpdated {
        session_id: String,
        title: String,
    },
    TaskNotification(TaskNotification),
}

/// Fan-out bus. Cloning is cheap (it clones the underlying `broadcast::Sender`).
#[derive(Clone)]
pub struct EventBus {
    tx: broadcast::Sender<ServerEvent>,
}

impl EventBus {
    pub fn new() -> Self {
        let (tx, _) = broadcast::channel(8192);
        Self { tx }
    }

    pub fn subscribe(&self) -> broadcast::Receiver<ServerEvent> {
        self.tx.subscribe()
    }

    pub fn emit_stream(&self, session_id: &str, event: StreamEvent) {
        let _ = self.tx.send(ServerEvent::Stream {
            session_id: session_id.to_string(),
            event,
        });
    }

    pub fn emit_session_title(&self, session_id: &str, title: String) {
        let _ = self.tx.send(ServerEvent::SessionTitleUpdated {
            session_id: session_id.to_string(),
            title,
        });
    }

    pub fn emit_task_notification(&self, notification: TaskNotification) {
        let _ = self.tx.send(ServerEvent::TaskNotification(notification));
    }
}

impl Default for EventBus {
    fn default() -> Self {
        Self::new()
    }
}
