//! Server-Sent Events endpoint — relays the shared EventBus to the browser.
//!
//! The bus is process-global: every subscriber receives every session's events
//! (see the session-scoping finding in the shells review). The token middleware
//! in `main.rs` is what keeps this from being a public feed; per-session
//! filtering by an authenticated owner is still a follow-up.

use std::convert::Infallible;
use std::sync::Arc;

use axum::extract::State;
use axum::response::sse::{Event, KeepAlive, Sse};
use futures::stream::{self, Stream};
use serde_json::json;
use tokio::sync::broadcast::error::RecvError;

use dscode_server::app_state::AppState;
use dscode_server::ServerEvent;

fn server_event(ev: &ServerEvent) -> Event {
    let data = serde_json::to_string(ev).unwrap_or_else(|_| "{}".to_string());
    Event::default().event("server-event").data(data)
}

/// Tells the client it fell behind and events were dropped. A dedicated SSE
/// event name (not a `ServerEvent`) so clients that only listen for
/// `server-event` ignore it instead of mis-rendering it.
fn resync_event(skipped: u64) -> Event {
    Event::default()
        .event("resync")
        .data(json!({ "skipped": skipped }).to_string())
}

pub async fn events_handler(
    State(state): State<Arc<AppState>>,
) -> Sse<impl Stream<Item = Result<Event, Infallible>>> {
    let rx = state.event_bus.subscribe();

    let stream = stream::unfold(rx, |mut rx| async move {
        loop {
            match rx.recv().await {
                Ok(ev) => return Some((Ok::<_, Infallible>(server_event(&ev)), rx)),
                // The old `while let Ok(..)` treated a lagged receiver as
                // "sender dropped" and ended the forwarder: the connection
                // then received nothing, forever, with no signal. Emit a
                // resync notice and keep going — `Closed` is the only real end.
                Err(RecvError::Lagged(skipped)) => {
                    tracing::warn!(skipped, "web sse: subscriber lagged, events dropped");
                    return Some((Ok::<_, Infallible>(resync_event(skipped)), rx));
                }
                Err(RecvError::Closed) => return None,
            }
        }
    });

    Sse::new(stream).keep_alive(KeepAlive::default())
}
