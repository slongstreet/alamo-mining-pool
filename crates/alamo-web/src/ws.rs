//! Live updates over `/api/ws`.
//!
//! Every message is `{"type": "status" | "share", "data": ...}`. A client gets the current
//! status document on connect, then every published snapshot and, between snapshots,
//! each share as it is processed. Slow clients skip ahead rather than back up the daemon.

use crate::AppState;
use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::State;
use axum::response::Response;
use tokio::sync::broadcast::error::RecvError;

/// Upgrade handler for `/api/ws`.
pub async fn upgrade(ws: WebSocketUpgrade, State(state): State<AppState>) -> Response {
    ws.on_upgrade(move |socket| run(socket, state))
}

async fn run(mut socket: WebSocket, state: AppState) {
    let mut pushes = state.subscribe();
    let hello = match serde_json::to_string(&serde_json::json!({
        "type": "status",
        "data": state.snapshot(),
    })) {
        Ok(text) => text,
        Err(err) => {
            tracing::warn!(%err, "could not serialize status");
            return;
        }
    };
    if socket.send(Message::Text(hello.into())).await.is_err() {
        return;
    }
    loop {
        tokio::select! {
            push = pushes.recv() => match push {
                Ok(text) => {
                    if socket.send(Message::Text(text)).await.is_err() {
                        return;
                    }
                }
                Err(RecvError::Lagged(skipped)) => {
                    tracing::debug!(skipped, "websocket client lagged");
                }
                Err(RecvError::Closed) => return,
            },
            incoming = socket.recv() => match incoming {
                // Pings are answered by axum; the dashboard never sends anything else.
                Some(Ok(Message::Close(_))) | Some(Err(_)) | None => return,
                Some(Ok(_)) => {}
            },
        }
    }
}
