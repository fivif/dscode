//! DS Code Desktop — Tauri GUI Application

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use std::time::Duration;

use dscode_desktop::app_state::AppState;
use tauri::Manager;

fn main() {
    tracing_subscriber::fmt::init();

    tauri::Builder::default()
        .plugin(tauri_plugin_shell::init())
        .plugin(tauri_plugin_dialog::init())
        .manage(AppState::new())
        .setup(|app| {
            let handle = app.handle().clone();
            // Load MCP servers into the tool registry at startup
            let mcp_handle = handle.clone();
            tauri::async_runtime::spawn(async move {
                let state = mcp_handle.state::<AppState>();
                let (n, status) =
                    dscode_core::tools::mcp_ops::register_mcp_tools(&state.tool_registry).await;
                for line in &status {
                    tracing::info!(%line, "mcp");
                }
                tracing::info!(registered = n, "MCP tools ready for agent");
            });
            tauri::async_runtime::spawn(async move {
                loop {
                    tokio::time::sleep(Duration::from_secs(6 * 3600)).await;
                    let state = handle.state::<AppState>();
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
            dscode_desktop::commands::chat::send_message,
            dscode_desktop::commands::chat::stage_upload,
            dscode_desktop::commands::chat::approve_permission,
            dscode_desktop::commands::chat::deny_permission,
            dscode_desktop::commands::chat::abort,
            dscode_desktop::commands::chat::stop_team_agent,
            dscode_desktop::commands::chat::nudge_team_agent,
            dscode_desktop::commands::chat::list_tools,
            dscode_desktop::commands::mcp::list_mcp_servers,
            dscode_desktop::commands::mcp::add_mcp_server,
            dscode_desktop::commands::mcp::update_mcp_server,
            dscode_desktop::commands::mcp::remove_mcp_server,
            dscode_desktop::commands::mcp::reload_mcp,
            dscode_desktop::commands::chat::list_skills,
            dscode_desktop::commands::chat::save_skill,
            dscode_desktop::commands::chat::write_skill_file,
            dscode_desktop::commands::chat::skills_dir,
            dscode_desktop::commands::chat::install_skill_package,
            dscode_desktop::commands::chat::delete_skill,
            dscode_desktop::commands::session::list_sessions,
            dscode_desktop::commands::session::get_session,
            dscode_desktop::commands::session::create_session,
            dscode_desktop::commands::session::get_last_session,
            dscode_desktop::commands::session::update_session_workspace,
            dscode_desktop::commands::session::update_session_title,
            dscode_desktop::commands::session::delete_session,
            dscode_desktop::commands::config::get_config,
            dscode_desktop::commands::config::update_config,
            dscode_desktop::commands::config::get_global_prompt,
            dscode_desktop::commands::config::set_global_prompt,
            dscode_desktop::commands::config::fetch_models,
            dscode_desktop::commands::chat::subscribe_task_events,
        ])
        .run(tauri::generate_context!())
        .expect("error while running DS Code Desktop");
}
