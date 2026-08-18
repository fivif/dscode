//! Server-Sent Events endpoint — relays the shared EventBus to the browser.

use std::convert::Infallible;
use std::sync::Arc;

use axum::extract::State;
use axum::response::sse::{Event, KeepAlive, Sse};
use futures::stream::{self, Stream, StreamExt};

use dscode_server::app_state::AppState;
use dscode_server::ServerEvent;

pub async fn events_handler(
    State(state): State<Arc<AppState>>,
) -> Sse<impl Stream<Item = Result<Event, Infallible>>> {
    let rx = state.event_bus.subscribe();

    let stream = stream::unfold(rx, |mut rx| async move {
        match rx.recv().await {
            Ok(ev) => Some((ev, rx)),
            Err(_) => None, // sender dropped
        }
    })
    .map(|ev: ServerEvent| {
        let json = serde_json::to_string(&ev).unwrap_or_else(|_| "{}".to_string());
        Ok::<_, Infallible>(Event::default().event("server-event").data(json))
    });

    Sse::new(stream).keep_alive(KeepAlive::default())
}
