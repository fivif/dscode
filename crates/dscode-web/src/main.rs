//! DS Code Web — browser-based agent server (axum shell over `dscode-server`).

use std::path::{Path, PathBuf};
use std::sync::Arc;

use axum::extract::DefaultBodyLimit;
use axum::http::{header, Method};
use axum::middleware;
use axum::routing::{get, post};
use axum::Router;
use tower_http::cors::{AllowOrigin, CorsLayer};
use tower_http::services::ServeDir;

use dscode_server::app_state::AppState;

mod auth;
mod dispatch;
mod image;
mod sse;

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt::init();

    let addr =
        std::env::var("DSCODE_WEB_ADDR").unwrap_or_else(|_| "127.0.0.1:8080".to_string());
    let loopback = auth::is_loopback_addr(&addr);
    let (token, generated) = resolve_token(&addr, loopback);

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

    let app = api_router(state, token.clone(), &dist);

    let listener = tokio::net::TcpListener::bind(&addr)
        .await
        .unwrap_or_else(|e| panic!("bind {addr}: {e}"));

    println!("🌐 DS Code Web → http://{addr}");
    println!("   frontend dir: {dist}");
    if generated {
        // Generated token: the operator has no other way to learn it.
        println!("   API token: {token}");
        println!("   (open http://{addr}/?token={token} — /api/* requires this token)");
    } else {
        // Do not echo a secret the operator supplied via the environment.
        println!("   API token: from DSCODE_WEB_TOKEN");
    }
    println!("   note: the web frontend must send the token on /api/invoke and /api/events.");
    if loopback {
        println!("⚠  bound to loopback only, but other programs and browser pages on this");
        println!("   machine can still reach this port; the token is the only credential.");
    }
    axum::serve(listener, app).await.expect("axum server failed");
}

/// Build the router. `/api/*` sits behind the token middleware; the static
/// frontend does not (the browser has no token before it loads the page).
fn api_router(state: Arc<AppState>, token: String, dist: &str) -> Router {
    let web_auth = auth::WebAuth::new(token);

    // SECURITY: this layer is what keeps `/api/invoke` (arbitrary agent
    // commands) and `/api/events` (permission-request ids and streamed file
    // contents) from being a public, drive-by-reachable surface. Do not remove
    // it, and route any new API path through this router.
    let api = Router::new()
        .route("/api/events", get(sse::events_handler))
        .route("/api/invoke", post(dispatch::invoke_handler))
        // Must be registered *above* the `route_layer` below: `route_layer`
        // only wraps the routes that already exist, so a route added after it
        // would be served without the token. That would turn `/api/image` into
        // an unauthenticated arbitrary-file-read (`?path=…` is caller-supplied).
        // axum documents this on `Router::route_layer`; do not reorder.
        .route("/api/image", get(image::image_handler))
        .route_layer(middleware::from_fn_with_state(web_auth, auth::require_token));

    // Explicit CORS: loopback origins only (the Vite dev server / a local
    // page), and only the methods and headers the API actually uses.
    // `permissive()` used to let any origin read every response.
    let cors = CorsLayer::new()
        .allow_origin(AllowOrigin::predicate(|origin, _parts| {
            origin.to_str().map(is_loopback_origin).unwrap_or(false)
        }))
        .allow_methods([Method::GET, Method::POST])
        .allow_headers([header::AUTHORIZATION, header::CONTENT_TYPE]);

    Router::new()
        .merge(api)
        .fallback_service(ServeDir::new(dist))
        .layer(cors)
        // axum's `Json` default is 2 MiB, which silently 413s any upload over
        // ~1.5 MiB of file (base64 inflates by 4/3) against the advertised
        // 40 MiB attachment cap.
        .layer(DefaultBodyLimit::max(dispatch::MAX_BODY_BYTES))
        .with_state(state)
}

/// `http://127.0.0.1:5173`, `http://localhost:3000`, `http://[::1]:8080`, …
fn is_loopback_origin(origin: &str) -> bool {
    let rest = match origin.split_once("://") {
        Some((_, rest)) => rest,
        None => return false,
    };
    let host = rest.split(['/', '?']).next().unwrap_or(rest);
    let host = host.rsplit_once(':').map(|(h, _)| h).unwrap_or(host);
    let host = host.trim_start_matches('[').trim_end_matches(']');
    if host.eq_ignore_ascii_case("localhost") {
        return true;
    }
    host.parse::<std::net::IpAddr>()
        .map(|ip| ip.is_loopback())
        .unwrap_or(false)
}

/// Resolve the API token. Returns `(token, generated)`.
///
/// - `DSCODE_WEB_TOKEN` set → use it (and do not echo it).
/// - Otherwise, binding to a non-loopback address is refused: a generated token
///   the operator never sees is worse than failing loudly when the surface is
///   about to be exposed to the LAN/internet (fail closed).
/// - Otherwise generate a random token for the loopback-only server.
fn resolve_token(addr: &str, loopback: bool) -> (String, bool) {
    if let Ok(t) = std::env::var("DSCODE_WEB_TOKEN") {
        let t = t.trim().to_string();
        if !t.is_empty() {
            return (t, false);
        }
    }
    if !loopback {
        eprintln!("✖ refusing to start: DSCODE_WEB_ADDR={addr} is not a loopback address");
        eprintln!("  and DSCODE_WEB_TOKEN is not set. Exposing the agent API without an");
        eprintln!("  explicit token would let anyone who can reach this port run commands");
        eprintln!("  on this machine. Set DSCODE_WEB_TOKEN=<secret> to allow it.");
        std::process::exit(2);
    }
    (uuid::Uuid::new_v4().simple().to_string(), true)
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
