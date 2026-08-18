//! DS Code Desktop — Tauri GUI Application

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use std::sync::Arc;
use std::time::Duration;

use dscode_server::app_state::AppState;
use tauri::Manager;

fn main() {
    tracing_subscriber::fmt::init();

    tauri::Builder::default()
        .plugin(tauri_plugin_shell::init())
        .plugin(tauri_plugin_dialog::init())
        .manage(Arc::new(AppState::new()))
        .setup(|app| {
            let handle = app.handle().clone();

            // Bridge shared EventBus → Tauri events for the webview.
            dscode_desktop::shell::spawn_event_bridge(&handle);

            // Load MCP servers into the tool registry at startup.
            let mcp_handle = handle.clone();
            tauri::async_runtime::spawn(async move {
                let state = mcp_handle.state::<Arc<AppState>>();
                let (n, status) =
                    dscode_core::tools::mcp_ops::register_mcp_tools(&state.tool_registry).await;
                for line in &status {
                    tracing::info!(%line, "mcp");
                }
                tracing::info!(registered = n, "MCP tools ready for agent");
            });

            // Periodic session auto-cleanup.
            tauri::async_runtime::spawn(async move {
                loop {
                    tokio::time::sleep(Duration::from_secs(6 * 3600)).await;
                    let state = handle.state::<Arc<AppState>>();
                    let guard = state.session_manager.lock().await;
                    if let Some(ref mgr) = *guard {
                        if let Err(e) = mgr.purge_now() {
                            tracing::warn!("Session auto-cleanup failed: {}", e);
                        } else {
                            tracing::info!("Session auto-cleanup completed");
                        }
                    }
                }
            });

            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            dscode_desktop::shell::send_message,
            dscode_desktop::shell::stage_upload,
            dscode_desktop::shell::approve_permission,
            dscode_desktop::shell::deny_permission,
            dscode_desktop::shell::abort,
            dscode_desktop::shell::stop_team_agent,
            dscode_desktop::shell::nudge_team_agent,
            dscode_desktop::shell::list_tools,
            dscode_desktop::shell::list_mcp_servers,
            dscode_desktop::shell::add_mcp_server,
            dscode_desktop::shell::update_mcp_server,
            dscode_desktop::shell::remove_mcp_server,
            dscode_desktop::shell::reload_mcp,
            dscode_desktop::shell::list_skills,
            dscode_desktop::shell::save_skill,
            dscode_desktop::shell::write_skill_file,
            dscode_desktop::shell::skills_dir,
            dscode_desktop::shell::install_skill_package,
            dscode_desktop::shell::delete_skill,
            dscode_desktop::shell::list_sessions,
            dscode_desktop::shell::get_session,
            dscode_desktop::shell::create_session,
            dscode_desktop::shell::get_last_session,
            dscode_desktop::shell::update_session_workspace,
            dscode_desktop::shell::update_session_title,
            dscode_desktop::shell::update_session_model,
            dscode_desktop::shell::delete_session,
            dscode_desktop::shell::get_config,
            dscode_desktop::shell::update_config,
            dscode_desktop::shell::get_global_prompt,
            dscode_desktop::shell::set_global_prompt,
            dscode_desktop::shell::fetch_models,
            dscode_desktop::shell::subscribe_task_events,
        ])
        .run(tauri::generate_context!())
        .expect("error while running DS Code Desktop");
}
