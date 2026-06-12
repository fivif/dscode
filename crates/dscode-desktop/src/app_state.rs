use std::sync::Arc;
use tokio::sync::Mutex;

use dscode_core::config::settings::Config;
use dscode_core::tools::registry::ToolRegistry;
use dscode_core::wiki::Engine;

/// Shared application state managed by Tauri.
///
/// All fields are protected by [`Mutex`] or wrapped in [`Arc`] so they can be
/// accessed concurrently from command handlers and background tasks.
pub struct AppState {
    /// The user configuration (API keys, model settings, etc.).
    pub config: Mutex<Config>,

    /// The session manager (SQLite-backed chat history).
    /// Wrapped in `Option` so it can be lazily initialized on first use.
    pub session_manager: Mutex<Option<dscode_core::session::manager::SessionManager>>,

    /// Shared tool registry (bash, file ops, etc.).
    pub tool_registry: Arc<ToolRegistry>,

    /// Handle to the currently running forge task, if any.
    /// Allows the frontend to abort an in-progress agent turn.
    pub active_forge_handle: Mutex<Option<tokio::task::JoinHandle<()>>>,

    /// The wiki engine (global + per-session knowledge graphs).
    /// Lazily initialized on first wiki command.
    pub wiki_engine: Mutex<Option<Engine>>,
}

impl AppState {
    /// Create a new AppState with default config, empty session manager,
    /// a tool registry pre-populated with default tools, and no active forge.
    pub fn new() -> Self {
        let config = Config::load().unwrap_or_default();

        let mut tool_registry = ToolRegistry::new();
        tool_registry.register_default_tools();

        Self {
            config: Mutex::new(config),
            session_manager: Mutex::new(None),
            tool_registry: Arc::new(tool_registry),
            active_forge_handle: Mutex::new(None),
            wiki_engine: Mutex::new(None),
        }
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
    /// Returns a reference to the initialized manager via callback pattern
    /// because the Mutex guard must be held.
    pub async fn ensure_session_manager(&self) -> Result<(), String> {
        let mut guard = self.session_manager.lock().await;
        if guard.is_none() {
            let retention_days = {
                let cfg = self.config.lock().await;
                cfg.session.retention_days
            };
            let mgr = dscode_core::session::manager::SessionManager::new(retention_days)?;
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
