use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::Mutex;
use tokio_util::sync::CancellationToken;

use crate::event_bus::EventBus;

use dscode_core::config::settings::Config;
use dscode_core::safety::permission::PermissionHub;
use dscode_core::session::manager::SessionManager;
use dscode_core::tools::registry::ToolRegistry;
use dscode_core::tools::background::TaskManager;

/// How long a cancelled turn gets to persist its partial answer before the
/// task is hard-aborted. The cancel branch in `commands::chat` writes the
/// accumulated assistant/thinking content, so aborting immediately (as the
/// old code did) reliably lost it.
const ABORT_GRACE: Duration = Duration::from_secs(2);

/// Handle to an in-progress forge task with cancellation support.
pub struct ActiveForge {
    /// Cancels the forge and event-loop tasks when triggered.
    pub cancel: CancellationToken,
    /// JoinHandle for the outer spawned task (event loop + forge).
    pub handle: tokio::task::JoinHandle<()>,
}

/// Shared application state managed by Tauri.
///
/// All fields are protected by [`Mutex`] or wrapped in [`Arc`] so they can be
/// accessed concurrently from command handlers and background tasks.
pub struct AppState {
    /// The user configuration (API keys, model settings, etc.).
    pub config: Mutex<Config>,

    /// The session manager (SQLite-backed chat history).
    /// Wrapped in `Option` so it can be lazily initialized on first use.
    pub session_manager: Mutex<Option<SessionManager>>,

    /// Shared tool registry (bash, file ops, etc.).
    pub tool_registry: Arc<ToolRegistry>,

    /// In-flight forge tasks keyed by session_id — multiple sessions can run
    /// agent turns concurrently; each session still serializes its own sends.
    pub active_forges: Mutex<HashMap<String, ActiveForge>>,

    /// Per-session mutexes to prevent concurrent sends to the same session.
    /// Each session gets its own Mutex<()> — the guard is held for the
    /// duration of send_message to serialize requests for that session.
    pub per_session_locks: Mutex<HashMap<String, Arc<Mutex<()>>>>,

    /// Background task manager for non-blocking command execution.
    pub task_manager: TaskManager,

    /// Interactive permission hub (Safe mode confirmations).
    pub permission_hub: Arc<PermissionHub>,

    /// Whether /teams multi-agent mode is active.
    pub teams_mode: AtomicBool,

    /// Unified event bus (stream / title / task notifications) relayed by the shell.
    pub event_bus: EventBus,

    /// The single background task-notification forwarder, so
    /// `subscribe_task_events` is idempotent instead of spawning one
    /// never-cancelled task per call.
    pub(crate) task_forwarder: Mutex<Option<tokio::task::JoinHandle<()>>>,

    /// Set when `session.retention_days` changed while a turn was in flight;
    /// applied by [`AppState::ensure_session_manager`] once nothing is running.
    pub(crate) retention_reset_pending: AtomicBool,
}

impl AppState {
    /// Create a new AppState with default config, empty session manager,
    /// a tool registry pre-populated with default tools, and no active forge.
    pub fn new() -> Self {
        let config = Config::load().unwrap_or_default();

        let task_manager = TaskManager::new();
        let handle = task_manager.handle();
        let notify_tx = task_manager.notify_tx();
        let permission_hub = PermissionHub::shared();

        let mut tool_registry = ToolRegistry::new();
        // Honour `[generation] image_enabled`: disabled means the tool is not
        // registered at all, so it costs no definition tokens and the model
        // cannot call it.
        tool_registry.register_default_tools_with_image(config.generation.image_enabled);
        let live = task_manager.live_handle();
        tool_registry.register(dscode_core::tools::background::DoBackground::new(
            handle.clone(),
            live.clone(),
            notify_tx.clone(),
        ));
        tool_registry.register(dscode_core::tools::background::DoTaskStatus::new(handle.clone()));
        tool_registry.register(dscode_core::tools::background::DoTaskKill::new(
            handle,
            live,
            notify_tx,
        ));

        Self {
            config: Mutex::new(config),
            session_manager: Mutex::new(None),
            tool_registry: Arc::new(tool_registry),
            active_forges: Mutex::new(HashMap::new()),
            per_session_locks: Mutex::new(HashMap::new()),
            task_manager,
            permission_hub,
            teams_mode: AtomicBool::new(false),
            event_bus: EventBus::new(),
            task_forwarder: Mutex::new(None),
            retention_reset_pending: AtomicBool::new(false),
        }
    }

    /// Register a forge for a session, cancelling only a previous run on the
    /// **same** session (other sessions keep running).
    pub async fn set_active_forge(&self, session_id: String, forge: ActiveForge) {
        let old = {
            let mut map = self.active_forges.lock().await;
            let old = map.remove(&session_id);
            map.insert(session_id, forge);
            old
        };
        // Retire the replaced run outside the lock: cancel it, then let it
        // persist its partial answer in the background so the new turn starts
        // without waiting (the old code hard-aborted immediately, which made
        // the cancel-branch persistence dead code and lost the partial answer).
        if let Some(old) = old {
            tokio::spawn(Self::retire(old));
        }
    }

    /// Abort one session's forge (or all if `session_id` is None — unused).
    pub async fn abort_forge(&self, session_id: &str) -> bool {
        let active = {
            let mut map = self.active_forges.lock().await;
            map.remove(session_id)
        };
        match active {
            Some(active) => {
                Self::retire(active).await;
                true
            }
            None => false,
        }
    }

    /// Cancel a forge and wait — bounded — for its event loop to persist the
    /// accumulated partial answer, then hard-abort if it is still running.
    /// Never called while holding `active_forges`.
    async fn retire(active: ActiveForge) {
        active.cancel.cancel();
        let mut handle = active.handle;
        if tokio::time::timeout(ABORT_GRACE, &mut handle).await.is_err() {
            tracing::warn!(
                grace_ms = ABORT_GRACE.as_millis() as u64,
                "forge did not stop after cancel; aborting task"
            );
            handle.abort();
        }
    }

    /// Drop finished forges so the map does not grow forever.
    pub async fn prune_finished_forges(&self) {
        let mut map = self.active_forges.lock().await;
        map.retain(|_, f| !f.handle.is_finished());
    }

    /// Acquire a per-session lock to prevent concurrent request processing
    /// for the same session. Returns the guard that should be held for the
    /// entire send_message call.
    pub async fn acquire_session_lock(&self, session_id: &str) -> tokio::sync::OwnedMutexGuard<()> {
        let mut locks = self.per_session_locks.lock().await;
        let entry = locks
            .entry(session_id.to_string())
            .or_insert_with(|| Arc::new(Mutex::new(())));
        let arc = Arc::clone(entry);
        drop(locks);
        arc.lock_owned().await
    }

    /// The SessionManager owns a SQLite connection and cannot be cloned.
    /// Commands must lock `session_manager`, create it if needed, then use
    /// it directly within the lock scope. Use [`ensure_session_manager`]
    /// to lazily initialize.
    pub async fn get_or_create_session_manager(
        &self,
    ) -> Result<(), String> {
        self.ensure_session_manager().await
    }

    /// Ensure the session manager exists, creating it if needed.
    /// Uses `spawn_blocking` to avoid blocking the async runtime on
    /// synchronous SQLite I/O during initialization.
    pub async fn ensure_session_manager(&self) -> Result<(), String> {
        // Apply a retention change that was deferred while a turn was running
        // (see `commands::config::update_config`). Re-initialize in place
        // rather than dropping to `None`: a concurrent persist that acquires
        // the lock afterwards then still writes through a working connection
        // instead of silently discarding the row.
        let no_active_turns = self.active_forges.lock().await.is_empty();
        let force = self.retention_reset_pending.load(Ordering::SeqCst) && no_active_turns;

        let mut guard = self.session_manager.lock().await;
        if force || guard.is_none() {
            let retention_days = {
                let cfg = self.config.lock().await;
                cfg.session.retention_days
            };
            let mgr = tokio::task::spawn_blocking(move || {
                SessionManager::new(retention_days)
            })
            .await
            .map_err(|e| format!("spawn_blocking panicked: {}", e))?
            .map_err(|e| format!("SessionManager init failed: {}", e))?;
            *guard = Some(mgr);
            if force {
                self.retention_reset_pending.store(false, Ordering::SeqCst);
                tracing::info!("session manager re-initialized for new retention setting");
            }
        }
        Ok(())
    }
}

impl Default for AppState {
    fn default() -> Self {
        Self::new()
    }
}
