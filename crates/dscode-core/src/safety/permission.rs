//! Interactive permission hub — tools wait for UI approve/deny.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use tokio::sync::{oneshot, Mutex};
use tokio::time::timeout;

use crate::agent::stream::StreamEvent;

const DEFAULT_TIMEOUT_SECS: u64 = 120;

/// Shared gate: emit `PermissionRequest`, wait for `resolve`.
#[derive(Debug, Default)]
pub struct PermissionHub {
    pending: Mutex<HashMap<String, oneshot::Sender<bool>>>,
}

impl PermissionHub {
    pub fn new() -> Self {
        Self {
            pending: Mutex::new(HashMap::new()),
        }
    }

    pub fn shared() -> Arc<Self> {
        Arc::new(Self::new())
    }

    /// Ask the UI to confirm a command. Returns `true` only if the user approves
    /// within `timeout_secs`. Missing UI / timeout / deny → `false`.
    pub async fn request_confirm(
        &self,
        event_tx: &tokio::sync::mpsc::UnboundedSender<StreamEvent>,
        tool_call_id: &str,
        command: &str,
        reason: &str,
        timeout_secs: u64,
    ) -> bool {
        let request_id = format!(
            "perm_{}",
            uuid::Uuid::new_v4()
                .to_string()
                .chars()
                .take(12)
                .collect::<String>()
        );
        let (tx, rx) = oneshot::channel();
        {
            let mut g = self.pending.lock().await;
            g.insert(request_id.clone(), tx);
        }

        let _ = event_tx.send(StreamEvent::PermissionRequest {
            id: request_id.clone(),
            tool_call_id: tool_call_id.to_string(),
            command: command.to_string(),
            reason: reason.to_string(),
            timeout_secs: if timeout_secs == 0 {
                DEFAULT_TIMEOUT_SECS
            } else {
                timeout_secs
            },
        });

        let secs = if timeout_secs == 0 {
            DEFAULT_TIMEOUT_SECS
        } else {
            timeout_secs
        };

        match timeout(Duration::from_secs(secs), rx).await {
            Ok(Ok(true)) => true,
            Ok(Ok(false)) => false,
            Ok(Err(_)) => false, // sender dropped
            Err(_) => {
                // timeout — drop pending
                let mut g = self.pending.lock().await;
                g.remove(&request_id);
                false
            }
        }
    }

    /// Resolve a pending request (called from Tauri IPC).
    pub async fn resolve(&self, request_id: &str, allow: bool) -> Result<(), String> {
        let mut g = self.pending.lock().await;
        match g.remove(request_id) {
            Some(tx) => {
                let _ = tx.send(allow);
                Ok(())
            }
            None => Err(format!("Permission request '{request_id}' not found or expired")),
        }
    }
}
