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
            dscode_desktop::commands::chat::abort,
            dscode_desktop::commands::chat::list_tools,
            dscode_desktop::commands::chat::list_skills,
            dscode_desktop::commands::chat::save_skill,
            dscode_desktop::commands::chat::delete_skill,
            dscode_desktop::commands::session::list_sessions,
            dscode_desktop::commands::session::get_session,
            dscode_desktop::commands::session::create_session,
            dscode_desktop::commands::session::get_last_session,
            dscode_desktop::commands::session::update_session_workspace,
            dscode_desktop::commands::session::delete_session,
            dscode_desktop::commands::config::get_config,
            dscode_desktop::commands::config::update_config,
            dscode_desktop::commands::config::fetch_models,
            dscode_desktop::commands::wiki::wiki_search,
            dscode_desktop::commands::wiki::wiki_graph,
        ])
        .run(tauri::generate_context!())
        .expect("error while running DS Code Desktop");
}
