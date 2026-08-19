//! Thin Tauri command adapters over the shared `dscode-server` logic.
//!
//! The real behavior lives in `dscode-server`; this module only:
//! 1. Adapts `tauri::State<Arc<AppState>>` → `Arc<AppState>`.
//! 2. Bridges the shared `EventBus` into Tauri `emit` events.

use std::sync::Arc;

use dscode_server::app_state::AppState;
use dscode_server::commands::{chat, config, mcp, session};
use dscode_server::event_bus::ServerEvent;
use tauri::{Emitter, Manager};

/// Forward shared EventBus messages to the Tauri frontend.
pub fn spawn_event_bridge(app: &tauri::AppHandle) {
    let bus = {
        let state = app.state::<Arc<AppState>>();
        state.event_bus.clone()
    };
    let app = app.clone();
    let mut rx = bus.subscribe();

    tauri::async_runtime::spawn(async move {
        while let Ok(ev) = rx.recv().await {
            match ev {
                ServerEvent::Stream { session_id, event } => {
                    let _ = app.emit(
                        "stream-event",
                        serde_json::json!({ "session_id": session_id, "event": event }),
                    );
                }
                ServerEvent::SessionTitleUpdated { session_id, title } => {
                    let _ = app.emit(
                        "session-title-updated",
                        serde_json::json!({ "session_id": session_id, "title": title }),
                    );
                }
                ServerEvent::TaskNotification(notification) => {
                    let _ = app.emit("task-notification", &notification);
                }
            }
        }
    });
}

// ─────────────────────────── chat ───────────────────────────

#[tauri::command]
pub async fn send_message(
    state: tauri::State<'_, Arc<AppState>>,
    session_id: String,
    message: String,
    teams_mode: bool,
    attachments: Option<Vec<String>>,
) -> Result<(), String> {
    chat::send_message(state.inner().clone(), session_id, message, teams_mode, attachments).await
}

#[tauri::command]
pub async fn stage_upload(
    state: tauri::State<'_, Arc<AppState>>,
    session_id: String,
    name: String,
    base64_data: String,
) -> Result<String, String> {
    chat::stage_upload(state.inner().clone(), session_id, name, base64_data).await
}

#[tauri::command]
pub async fn approve_permission(
    state: tauri::State<'_, Arc<AppState>>,
    request_id: String,
) -> Result<(), String> {
    chat::approve_permission(state.inner().clone(), request_id).await
}

#[tauri::command]
pub async fn deny_permission(
    state: tauri::State<'_, Arc<AppState>>,
    request_id: String,
) -> Result<(), String> {
    chat::deny_permission(state.inner().clone(), request_id).await
}

#[tauri::command]
pub async fn abort(
    state: tauri::State<'_, Arc<AppState>>,
    session_id: String,
) -> Result<(), String> {
    chat::abort(state.inner().clone(), session_id).await
}

#[tauri::command]
pub async fn stop_team_agent(session_id: String, agent_id: String) -> Result<bool, String> {
    chat::stop_team_agent(session_id, agent_id).await
}

#[tauri::command]
pub async fn nudge_team_agent(
    session_id: String,
    agent_id: String,
    message: String,
) -> Result<bool, String> {
    chat::nudge_team_agent(session_id, agent_id, message).await
}

#[tauri::command]
pub async fn list_tools(
    state: tauri::State<'_, Arc<AppState>>,
) -> Result<Vec<chat::ToolInfo>, String> {
    chat::list_tools(state.inner().clone()).await
}

#[tauri::command]
pub async fn list_skills() -> Result<Vec<chat::SkillInfo>, String> {
    chat::list_skills().await
}

#[tauri::command]
pub async fn save_skill(
    name: String,
    description: String,
    body: String,
    triggers: Option<String>,
    files: Option<Vec<chat::SkillFileInput>>,
) -> Result<String, String> {
    chat::save_skill(name, description, body, triggers, files).await
}

#[tauri::command]
pub async fn write_skill_file(
    skill_name: String,
    relative_path: String,
    content: String,
) -> Result<String, String> {
    chat::write_skill_file(skill_name, relative_path, content).await
}

#[tauri::command]
pub async fn skills_dir() -> Result<String, String> {
    chat::skills_dir().await
}

#[tauri::command]
pub async fn install_skill_package(package: String) -> Result<String, String> {
    chat::install_skill_package(package).await
}

#[tauri::command]
pub async fn delete_skill(name: String, root: Option<String>) -> Result<String, String> {
    chat::delete_skill(name, root).await
}

#[tauri::command]
pub async fn subscribe_task_events(
    state: tauri::State<'_, Arc<AppState>>,
) -> Result<(), String> {
    chat::subscribe_task_events(state.inner().clone()).await
}

// ─────────────────────────── mcp ───────────────────────────

#[tauri::command]
pub async fn list_mcp_servers(
    state: tauri::State<'_, Arc<AppState>>,
) -> Result<Vec<mcp::McpServerInfo>, String> {
    mcp::list_mcp_servers(state.inner().clone()).await
}

#[tauri::command]
pub async fn add_mcp_server(
    state: tauri::State<'_, Arc<AppState>>,
    name: String,
    command: String,
    args: String,
) -> Result<mcp::McpReloadResult, String> {
    mcp::add_mcp_server(state.inner().clone(), name, command, args).await
}

#[tauri::command]
pub async fn update_mcp_server(
    state: tauri::State<'_, Arc<AppState>>,
    original_name: String,
    name: String,
    command: String,
    args: String,
) -> Result<mcp::McpReloadResult, String> {
    mcp::update_mcp_server(state.inner().clone(), original_name, name, command, args).await
}

#[tauri::command]
pub async fn remove_mcp_server(
    state: tauri::State<'_, Arc<AppState>>,
    name: String,
) -> Result<mcp::McpReloadResult, String> {
    mcp::remove_mcp_server(state.inner().clone(), name).await
}

#[tauri::command]
pub async fn reload_mcp(
    state: tauri::State<'_, Arc<AppState>>,
) -> Result<mcp::McpReloadResult, String> {
    mcp::reload_mcp(state.inner().clone()).await
}

// ─────────────────────────── session ───────────────────────────

#[tauri::command]
pub async fn list_sessions(
    state: tauri::State<'_, Arc<AppState>>,
    limit: Option<usize>,
    offset: Option<usize>,
) -> Result<Vec<dscode_core::session::manager::Session>, String> {
    session::list_sessions(state.inner().clone(), limit, offset).await
}

#[tauri::command]
pub async fn get_session(
    state: tauri::State<'_, Arc<AppState>>,
    id: String,
) -> Result<dscode_core::session::manager::Session, String> {
    session::get_session(state.inner().clone(), id).await
}

#[tauri::command]
pub async fn create_session(
    state: tauri::State<'_, Arc<AppState>>,
    title: String,
    workspace: String,
) -> Result<dscode_core::session::manager::Session, String> {
    session::create_session(state.inner().clone(), title, workspace).await
}

#[tauri::command]
pub async fn get_last_session(
    state: tauri::State<'_, Arc<AppState>>,
) -> Result<Option<dscode_core::session::manager::Session>, String> {
    session::get_last_session(state.inner().clone()).await
}

#[tauri::command]
pub async fn update_session_workspace(
    state: tauri::State<'_, Arc<AppState>>,
    session_id: String,
    workspace: String,
) -> Result<(), String> {
    session::update_session_workspace(state.inner().clone(), session_id, workspace).await
}

#[tauri::command]
pub async fn update_session_title(
    state: tauri::State<'_, Arc<AppState>>,
    session_id: String,
    title: String,
) -> Result<(), String> {
    session::update_session_title(state.inner().clone(), session_id, title).await
}

#[tauri::command]
pub async fn update_session_model(
    state: tauri::State<'_, Arc<AppState>>,
    session_id: String,
    model: String,
) -> Result<(), String> {
    session::update_session_model(state.inner().clone(), session_id, model).await
}

#[tauri::command]
pub async fn delete_session(
    state: tauri::State<'_, Arc<AppState>>,
    id: String,
) -> Result<(), String> {
    session::delete_session(state.inner().clone(), id).await
}

// ─────────────────────────── config ───────────────────────────

#[tauri::command]
pub async fn get_config(
    state: tauri::State<'_, Arc<AppState>>,
) -> Result<dscode_core::config::settings::Config, String> {
    config::get_config(state.inner().clone()).await
}

#[tauri::command]
pub async fn update_config(
    state: tauri::State<'_, Arc<AppState>>,
    config: dscode_core::config::settings::Config,
) -> Result<(), String> {
    config::update_config(state.inner().clone(), config).await
}

#[tauri::command]
pub async fn get_global_prompt(
    state: tauri::State<'_, Arc<AppState>>,
) -> Result<config::GlobalPromptInfo, String> {
    config::get_global_prompt(state.inner().clone()).await
}

#[tauri::command]
pub async fn set_global_prompt(
    state: tauri::State<'_, Arc<AppState>>,
    global_prompt: String,
    replace_system_prompt: bool,
) -> Result<config::GlobalPromptInfo, String> {
    config::set_global_prompt(state.inner().clone(), global_prompt, replace_system_prompt).await
}

#[tauri::command]
pub async fn fetch_models(
    state: tauri::State<'_, Arc<AppState>>,
    provider_key: String,
) -> Result<Vec<String>, String> {
    config::fetch_models(state.inner().clone(), provider_key).await
}
