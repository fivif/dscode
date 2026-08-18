//! DS Code Web — browser-based agent server (axum shell over `dscode-server`).

use std::path::{Path, PathBuf};
use std::sync::Arc;

use axum::routing::{get, post};
use axum::Router;
use tower_http::cors::CorsLayer;
use tower_http::services::ServeDir;

use dscode_server::app_state::AppState;

mod dispatch;
mod sse;

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt::init();

    let state = Arc::new(AppState::new());

    // Load MCP servers into the tool registry at startup.
    {
        let st = state.clone();
        tokio::spawn(async move {
            let (n, status) =
                dscode_core::tools::mcp_ops::register_mcp_tools(&st.tool_registry).await;
            for line in &status {
                tracing::info!(%line, "mcp");
            }
            tracing::info!(registered = n, "MCP tools ready for agent");
        });
    }

    // Static frontend directory (the desktop Vite build, reused as-is).
    let dist = resolve_dist_dir();

    let app = Router::new()
        .route("/api/events", get(sse::events_handler))
        .route("/api/invoke", post(dispatch::invoke_handler))
        .fallback_service(ServeDir::new(&dist))
        .with_state(state)
        .layer(CorsLayer::permissive());

    let addr = std::env::var("DSCODE_WEB_ADDR")
        .unwrap_or_else(|_| "127.0.0.1:8080".to_string());
    let listener = tokio::net::TcpListener::bind(&addr)
        .await
        .unwrap_or_else(|e| panic!("bind {addr}: {e}"));

    println!("🌐 DS Code Web → http://{addr}");
    println!("   frontend dir: {dist}");
    axum::serve(listener, app).await.expect("axum server failed");
}

/// Locate the built frontend. Tries, in order:
/// 1. `DSCODE_WEB_DIST` env var
/// 2. `<exe_dir>/dist` (installed layout)
/// 3. `<exe_dir>/../dist` (installed layout variant)
/// 4. workspace relative paths (dev)
fn resolve_dist_dir() -> String {
    if let Ok(d) = std::env::var("DSCODE_WEB_DIST") {
        if Path::new(&d).exists() {
            return d;
        }
    }

    let exe_dir = std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|d| d.to_path_buf()));

    let mut candidates: Vec<PathBuf> = Vec::new();
    if let Some(dir) = &exe_dir {
        candidates.push(dir.join("dist"));
        candidates.push(dir.join("../dist"));
    }
    candidates.push(PathBuf::from("../dscode-desktop/ui/dist"));
    candidates.push(PathBuf::from("crates/dscode-desktop/ui/dist"));

    for c in candidates {
        if c.exists() {
            return c.display().to_string();
        }
    }

    // Last resort: keep a relative path so the error message (if any) is clear.
    "../dscode-desktop/ui/dist".to_string()
}
