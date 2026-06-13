use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::Mutex;
use tokio_util::sync::CancellationToken;

use dscode_core::config::settings::Config;
use dscode_core::session::manager::SessionManager;
use dscode_core::tools::registry::ToolRegistry;
use dscode_core::tools::background::TaskManager;
use dscode_core::wiki::Engine;

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

    /// Handle to the currently running forge task, if any.
    /// Allows the frontend to abort an in-progress agent turn.
    pub active_forge_handle: Mutex<Option<ActiveForge>>,

    /// The wiki engine (global + per-session knowledge graphs).
    /// Lazily initialized on first wiki command.
    pub wiki_engine: Mutex<Option<Engine>>,

    /// Per-session mutexes to prevent concurrent sends to the same session.
    /// Each session gets its own Mutex<()> — the guard is held for the
    /// duration of send_message to serialize requests for that session.
    pub per_session_locks: Mutex<HashMap<String, Arc<Mutex<()>>>>,

    /// Background task manager for non-blocking command execution.
    pub task_manager: TaskManager,
}

impl AppState {
    /// Create a new AppState with default config, empty session manager,
    /// a tool registry pre-populated with default tools, and no active forge.
    pub fn new() -> Self {
        let config = Config::load().unwrap_or_default();

        let task_manager = TaskManager::new();
        let handle = task_manager.handle();

        let mut tool_registry = ToolRegistry::new();
        tool_registry.register_default_tools();
        tool_registry.register(dscode_core::tools::background::DoBackground::new(handle.clone()));
        tool_registry.register(dscode_core::tools::background::DoTaskStatus::new(handle));

        Self {
            config: Mutex::new(config),
            session_manager: Mutex::new(None),
            tool_registry: Arc::new(tool_registry),
            active_forge_handle: Mutex::new(None),
            wiki_engine: Mutex::new(None),
            per_session_locks: Mutex::new(HashMap::new()),
            task_manager,
        }
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
        let mut guard = self.session_manager.lock().await;
        if guard.is_none() {
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
        }
        Ok(())
    }

    /// Get or create the wiki engine.
    pub async fn ensure_wiki_engine(&self) -> Result<(), String> {
        let mut guard = self.wiki_engine.lock().await;
        if guard.is_none() {
            let engine = Engine::new()?;
            *guard = Some(engine);
        }
        Ok(())
    }
}

impl Default for AppState {
    fn default() -> Self {
        Self::new()
    }
}
