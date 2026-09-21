//! DS Code Desktop — Tauri GUI Application

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use std::sync::Arc;
use std::time::Duration;

use dscode_server::app_state::AppState;
use tauri::Manager;

/// Paint the Windows caption (title bar) in the app's own colour.
///
/// The system draws that bar, and on Windows 11 it is **Mica**: it samples the
/// desktop wallpaper rather than taking a colour from the app. On the machine
/// this was written on the bar came back `#1A2227` — a wallpaper-tinted teal —
/// sitting directly above a sidebar of `#1B1B1C`, so the app's most prominent
/// strip was the one surface it did not own and could not predict. Changing the
/// wallpaper changed the app's top edge.
///
/// `"theme": "Dark"` in `tauri.conf.json` is the other half and fixes
/// *legibility* — light title text and dark window controls — but it cannot pick
/// the colour. `DWMWA_CAPTION_COLOR` can.
///
/// The bar spans the full window width while only one colour can be set for it,
/// so it joins the **leftmost** plane: the sidebar. That is the choice VS Code,
/// Slack and Discord all make — title bar and navigation column read as a single
/// chrome surface, with the content area starting below and to the right. The
/// alternative (`main`) would put the seam on the left instead, where the eye
/// enters the window and where the app's identity sits.
#[cfg(windows)]
fn pin_caption_color(window: &tauri::WebviewWindow) {
    /// `DWMWA_CAPTION_COLOR`, Windows 11 build 22000 and later.
    const DWMWA_CAPTION_COLOR: u32 = 35;
    /// The `sidebar` token from `ui/tailwind.config.js`. COLORREF packs as
    /// `0x00BBGGRR`, so the byte order is reversed relative to the hex colour.
    const SIDEBAR: u32 = 0x001C_1B1B;

    #[link(name = "dwmapi")]
    extern "system" {
        fn DwmSetWindowAttribute(
            hwnd: isize,
            attribute: u32,
            value: *const core::ffi::c_void,
            size: u32,
        ) -> i32;
    }

    let Ok(hwnd) = window.hwnd() else { return };

    // SAFETY: a live HWND for the window Tauri created, a pointer to a `u32`
    // that outlives the call, and that value's true size.
    //
    // The result is deliberately dropped. On a Windows 10 host the attribute is
    // unknown and the call fails, which leaves the bar exactly as the window
    // theme made it — the behaviour before this function existed, not a
    // regression, and not worth a startup error.
    unsafe {
        let _ = DwmSetWindowAttribute(
            hwnd.0 as isize,
            DWMWA_CAPTION_COLOR,
            &SIDEBAR as *const u32 as *const core::ffi::c_void,
            4,
        );
    }
}

fn main() {
    tracing_subscriber::fmt::init();

    tauri::Builder::default()
        .plugin(tauri_plugin_shell::init())
        .plugin(tauri_plugin_dialog::init())
        // `capabilities/default.json` grants `notification:default`; without the
        // plugin registered every notification call fails at runtime.
        .plugin(tauri_plugin_notification::init())
        .manage(Arc::new(AppState::new()))
        .setup(|app| {
            let handle = app.handle().clone();

            // Every window, by iteration rather than by label: the config leaves
            // the default label in place, and a rename there should not silently
            // switch this off.
            #[cfg(windows)]
            for (_, window) in app.webview_windows() {
                pin_caption_color(&window);
            }

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
