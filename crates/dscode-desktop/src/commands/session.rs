//! Session commands — CRUD for chat sessions.
//!
//! Sessions are persisted in `~/.dscode/sessions.db` via [`SessionManager`].

use dscode_core::session::manager::Session;
use tracing::info;

use crate::app_state::AppState;

/// List sessions, most-recently-updated first.
/// Messages are NOT loaded (use [`get_session`] for full details).
///
/// # Parameters
/// - `limit`: maximum number of sessions to return (default 100, max 500).
/// - `offset`: number of sessions to skip before returning results (default 0).
#[tauri::command]
pub async fn list_sessions(
    state: tauri::State<'_, AppState>,
    limit: Option<usize>,
    offset: Option<usize>,
) -> Result<Vec<Session>, String> {
    let limit = limit.unwrap_or(100).min(500);
    let offset = offset.unwrap_or(0);

    state.ensure_session_manager().await?;

    let sm_guard = state.session_manager.lock().await;
    let sm = sm_guard
        .as_ref()
        .ok_or_else(|| "Session manager not initialized".to_string())?;

    let mut sessions = sm.list_sessions()?;
    // Apply pagination in-memory (the full list is already sorted by updated_at DESC).
    let total = sessions.len();
    if offset < total {
        let end = (offset + limit).min(total);
        sessions = sessions[offset..end].to_vec();
    } else {
        sessions.clear();
    }

    Ok(sessions)
}

/// Get a session by id, including all messages.
#[tauri::command]
pub async fn get_session(
    state: tauri::State<'_, AppState>,
    id: String,
) -> Result<Session, String> {
    state.ensure_session_manager().await?;

    let sm_guard = state.session_manager.lock().await;
    let sm = sm_guard
        .as_ref()
        .ok_or_else(|| "Session manager not initialized".to_string())?;

    sm.get_session(&id)?
        .ok_or_else(|| format!("Session '{}' not found", id))
}

/// Create a new session with the given title.
/// Returns the session with an empty message list.
#[tauri::command]
pub async fn create_session(
    state: tauri::State<'_, AppState>,
    title: String,
    workspace: String,
) -> Result<Session, String> {
    info!(%title, %workspace, "session: creating");

    state.ensure_session_manager().await?;

    let sm_guard = state.session_manager.lock().await;
    let sm = sm_guard
        .as_ref()
        .ok_or_else(|| "Session manager not initialized".to_string())?;

    sm.create_session(&title, &workspace)
}

/// Get the most recently used session.
#[tauri::command]
pub async fn get_last_session(
    state: tauri::State<'_, AppState>,
) -> Result<Option<Session>, String> {
    state.ensure_session_manager().await?;
    let sm_guard = state.session_manager.lock().await;
    let sm = sm_guard.as_ref().ok_or_else(|| "Session manager not initialized".to_string())?;
    sm.get_last_session()
}

/// Update the workspace directory for a session.
#[tauri::command]
pub async fn update_session_workspace(
    state: tauri::State<'_, AppState>,
    session_id: String,
    workspace: String,
) -> Result<(), String> {
    state.ensure_session_manager().await?;
    let sm_guard = state.session_manager.lock().await;
    let sm = sm_guard.as_ref().ok_or_else(|| "Session manager not initialized".to_string())?;
    sm.update_workspace(&session_id, &workspace)
}

/// Rename a session (manual rename from sidebar).
#[tauri::command]
pub async fn update_session_title(
    state: tauri::State<'_, AppState>,
    session_id: String,
    title: String,
) -> Result<(), String> {
    info!(%session_id, %title, "session: rename");
    state.ensure_session_manager().await?;
    let sm_guard = state.session_manager.lock().await;
    let sm = sm_guard
        .as_ref()
        .ok_or_else(|| "Session manager not initialized".to_string())?;
    sm.update_title(&session_id, &title)
}

/// Delete a session and all its messages.
#[tauri::command]
pub async fn delete_session(
    state: tauri::State<'_, AppState>,
    id: String,
) -> Result<(), String> {
    info!(%id, "session: deleting");

    state.ensure_session_manager().await?;

    let sm_guard = state.session_manager.lock().await;
    let sm = sm_guard
        .as_ref()
        .ok_or_else(|| "Session manager not initialized".to_string())?;

    sm.delete_session(&id)
}
