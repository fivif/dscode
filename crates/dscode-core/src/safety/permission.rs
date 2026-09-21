//! Interactive permission hub — tools wait for UI approve/deny.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use tokio::sync::{oneshot, Mutex};
use tokio::time::timeout;

use crate::agent::stream::StreamEvent;

const DEFAULT_TIMEOUT_SECS: u64 = 120;

type PendingMap = Arc<Mutex<HashMap<String, oneshot::Sender<bool>>>>;

/// Shared gate: emit `PermissionRequest`, wait for `resolve`.
#[derive(Debug, Default)]
pub struct PermissionHub {
    pending: PendingMap,
}

/// Removes its request id from the hub when dropped.
///
/// `request_confirm` can be cancelled at any await point: the caller's task is
/// aborted, the tool future is dropped, or the process tears the session down
/// mid-prompt. `resolve` is the only other place that removes an entry, so
/// without this guard a cancelled request would sit in `pending` forever.
#[derive(Debug)]
struct PendingGuard {
    pending: PendingMap,
    request_id: String,
}

impl Drop for PendingGuard {
    fn drop(&mut self) {
        // `drop` is synchronous, so the async mutex cannot be awaited here.
        match self.pending.try_lock() {
            Ok(mut g) => {
                g.remove(&self.request_id);
            }
            Err(_) => {
                // Someone holds the lock right now (an insert or a resolve —
                // both are short and never await while holding it). Hand the
                // cleanup to the runtime rather than leaking the entry; with no
                // runtime we are shutting down and the map goes with us.
                if let Ok(handle) = tokio::runtime::Handle::try_current() {
                    let pending = Arc::clone(&self.pending);
                    let id = std::mem::take(&mut self.request_id);
                    handle.spawn(async move {
                        pending.lock().await.remove(&id);
                    });
                }
            }
        }
    }
}

impl PermissionHub {
    pub fn new() -> Self {
        Self {
            pending: Arc::new(Mutex::new(HashMap::new())),
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
        // The guard owns a clone of the map handle, not a borrow of `self`, so
        // it stays valid however this future is dropped.
        let _pending_guard = {
            let mut g = self.pending.lock().await;
            g.insert(request_id.clone(), tx);
            PendingGuard {
                pending: Arc::clone(&self.pending),
                request_id: request_id.clone(),
            }
        };

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
            Err(_) => false,     // timeout — dropped guard removes the entry
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

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::sync::mpsc;

    #[tokio::test]
    async fn cancelled_request_is_removed_from_pending() {
        let hub = PermissionHub::shared();
        let (tx, mut rx) = mpsc::unbounded_channel::<StreamEvent>();
        let task_hub = Arc::clone(&hub);
        let task = tokio::spawn(async move {
            task_hub
                .request_confirm(&tx, "call-1", "rm -rf /", "test", 60)
                .await
        });

        let id = match rx.recv().await {
            Some(StreamEvent::PermissionRequest { id, .. }) => id,
            _ => panic!("expected a permission request event"),
        };

        task.abort();
        let _ = task.await; // the future (and its PendingGuard) is dropped here

        assert!(
            hub.resolve(&id, true).await.is_err(),
            "a cancelled request must not stay pending"
        );
    }

    #[tokio::test]
    async fn approved_request_resolves_once() {
        let hub = PermissionHub::shared();
        let (tx, mut rx) = mpsc::unbounded_channel::<StreamEvent>();
        let task_hub = Arc::clone(&hub);
        let task = tokio::spawn(async move {
            task_hub
                .request_confirm(&tx, "call-1", "sudo ls", "test", 60)
                .await
        });

        let id = match rx.recv().await {
            Some(StreamEvent::PermissionRequest { id, .. }) => id,
            _ => panic!("expected a permission request event"),
        };
        hub.resolve(&id, true).await.unwrap();
        assert!(task.await.unwrap());
        // One-shot: the same id cannot be replayed.
        assert!(hub.resolve(&id, true).await.is_err());
    }

    #[tokio::test]
    async fn timeout_fails_closed_and_clears_pending() {
        let hub = PermissionHub::shared();
        let (tx, mut rx) = mpsc::unbounded_channel::<StreamEvent>();
        let task_hub = Arc::clone(&hub);
        let task = tokio::spawn(async move {
            // Never answered: must fail closed on the timeout, not default to allow.
            task_hub
                .request_confirm(&tx, "call-1", "sudo ls", "test", 1)
                .await
        });

        let id = match rx.recv().await {
            Some(StreamEvent::PermissionRequest { id, .. }) => id,
            _ => panic!("expected a permission request event"),
        };
        assert!(!task.await.unwrap());
        assert!(
            hub.resolve(&id, true).await.is_err(),
            "a timed-out request must not stay pending"
        );
    }
}
