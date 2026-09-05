//! JSON API handlers and the snapshot WebSocket.

use crate::{AppState, PoolSnapshot};
use alamo_store::{BlockRow, HashrateSample, ShareRow, StoreError};
use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde::{Deserialize, Serialize};

/// Response body for `/api/health`.
#[derive(Serialize)]
pub struct Health {
    /// Always `"ok"` when the daemon answers.
    pub status: &'static str,
    /// Daemon version.
    pub version: &'static str,
    /// Seconds since start.
    pub uptime_seconds: u64,
}

/// Liveness probe.
pub async fn health(State(state): State<AppState>) -> Json<Health> {
    Json(Health {
        status: "ok",
        version: env!("CARGO_PKG_VERSION"),
        uptime_seconds: state.uptime_seconds(),
    })
}

/// Full pool status document.
pub async fn status(State(state): State<AppState>) -> Json<PoolSnapshot> {
    Json(state.snapshot())
}

/// Live status: the current snapshot on connect, then every new one as JSON text frames.
pub async fn ws(State(state): State<AppState>, upgrade: WebSocketUpgrade) -> Response {
    upgrade.on_upgrade(move |socket| stream_snapshots(socket, state))
}

async fn stream_snapshots(mut socket: WebSocket, state: AppState) {
    let mut updates = state.subscribe();
    loop {
        let text = match serde_json::to_string(&state.snapshot()) {
            Ok(text) => text,
            Err(err) => {
                tracing::error!(%err, "could not serialize snapshot");
                return;
            }
        };
        if socket.send(Message::Text(text.into())).await.is_err() {
            return;
        }
        // Wait for the next snapshot while draining anything the client sends. Pings are
        // answered by the protocol layer; a close frame or a dropped socket ends the loop.
        loop {
            tokio::select! {
                changed = updates.changed() => {
                    if changed.is_err() {
                        return;
                    }
                    break;
                }
                incoming = socket.recv() => match incoming {
                    None | Some(Err(_)) | Some(Ok(Message::Close(_))) => return,
                    Some(Ok(_)) => {}
                },
            }
        }
    }
}

/// A store failure surfaced as a JSON 500.
pub struct ApiError(StoreError);

impl From<StoreError> for ApiError {
    fn from(err: StoreError) -> Self {
        Self(err)
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        tracing::warn!(err = %self.0, "api query failed");
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({ "error": self.0.to_string() })),
        )
            .into_response()
    }
}

/// Longest history the hashrate endpoint serves: matches retention.
const MAX_HASHRATE_SPAN_SECS: u64 = 30 * 24 * 3600;
const DEFAULT_HASHRATE_SPAN_SECS: u64 = 24 * 3600;

/// Query for `/api/hashrate`.
#[derive(Debug, Default, Deserialize)]
pub struct HashrateQuery {
    /// Worker name; omitted or empty means the pool total.
    #[serde(default)]
    pub worker: String,
    /// How far back to look, in seconds (default one day, capped at 30 days).
    pub span: Option<u64>,
}

/// Hashrate samples for the pool or one worker, oldest first.
pub async fn hashrate(
    State(state): State<AppState>,
    Query(q): Query<HashrateQuery>,
) -> Result<Json<Vec<HashrateSample>>, ApiError> {
    let span = q
        .span
        .unwrap_or(DEFAULT_HASHRATE_SPAN_SECS)
        .min(MAX_HASHRATE_SPAN_SECS);
    let since = alamo_core::time::now_unix().saturating_sub(span) as i64;
    let samples = state
        .store()
        .hashrate_samples_since(Some(q.worker.as_str()), since)
        .await?;
    Ok(Json(samples))
}

/// Query for the list endpoints.
#[derive(Debug, Default, Deserialize)]
pub struct LimitQuery {
    /// Maximum rows to return.
    pub limit: Option<i64>,
}

impl LimitQuery {
    fn clamp(&self, default: i64, max: i64) -> i64 {
        self.limit.unwrap_or(default).clamp(1, max)
    }
}

/// Most recent shares, newest first.
pub async fn shares(
    State(state): State<AppState>,
    Query(q): Query<LimitQuery>,
) -> Result<Json<Vec<ShareRow>>, ApiError> {
    Ok(Json(state.store().recent_shares(q.clamp(100, 1000)).await?))
}

/// Blocks found, newest first.
pub async fn blocks(
    State(state): State<AppState>,
    Query(q): Query<LimitQuery>,
) -> Result<Json<Vec<BlockRow>>, ApiError> {
    Ok(Json(state.store().recent_blocks(q.clamp(50, 500)).await?))
}

#[cfg(test)]
mod tests {
    use crate::{router, AppState, PoolSnapshot};
    use alamo_store::{HashrateSample, NewShare, Store};
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use futures::StreamExt;
    use std::path::PathBuf;
    use tower::ServiceExt;

    fn temp_db(tag: &str) -> PathBuf {
        std::env::temp_dir()
            .join(format!(
                "alamo-web-{tag}-{}-{}",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_nanos())
                    .unwrap_or(0)
            ))
            .join("pool.db")
    }

    async fn state(tag: &str) -> (AppState, PathBuf) {
        let path = temp_db(tag);
        let store = Store::open(&path).await.unwrap();
        (AppState::new("test", store), path)
    }

    async fn get_json(state: &AppState, uri: &str) -> (StatusCode, serde_json::Value) {
        let res = router(state.clone())
            .oneshot(Request::builder().uri(uri).body(Body::empty()).unwrap())
            .await
            .unwrap();
        let status = res.status();
        let bytes = axum::body::to_bytes(res.into_body(), 1 << 20)
            .await
            .unwrap();
        (status, serde_json::from_slice(&bytes).unwrap())
    }

    #[tokio::test]
    async fn health_returns_ok() {
        let (state, path) = state("health").await;
        let (status, json) = get_json(&state, "/api/health").await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(json["status"], "ok");
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[tokio::test]
    async fn unknown_path_serves_dashboard_or_placeholder() {
        let (state, path) = state("spa").await;
        let res = router(state)
            .oneshot(
                Request::builder()
                    .uri("/workers")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[tokio::test]
    async fn history_endpoints_read_the_store_and_clamp_limits() {
        let (state, path) = state("history").await;
        let now = alamo_core::time::now_unix() as i64;
        state
            .store()
            .persist_batch(
                &[],
                &(0..5)
                    .map(|i| NewShare {
                        ts: now - i,
                        worker: "rig".into(),
                        difficulty: 8.0,
                        share_diff: 9.0,
                        accepted: i != 2,
                        reject_reason: (i == 2).then(|| "stale_job".into()),
                    })
                    .collect::<Vec<_>>(),
            )
            .await
            .unwrap();
        state
            .store()
            .insert_hashrate_samples(&[
                HashrateSample {
                    ts: now - 60,
                    worker: String::new(),
                    hashrate: 5.0,
                },
                HashrateSample {
                    ts: now - 60,
                    worker: "rig".into(),
                    hashrate: 5.0,
                },
                HashrateSample {
                    ts: now - 3 * 24 * 3600,
                    worker: String::new(),
                    hashrate: 1.0,
                },
            ])
            .await
            .unwrap();

        let (status, json) = get_json(&state, "/api/shares?limit=2").await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(json.as_array().unwrap().len(), 2);
        assert_eq!(json[0]["worker"], "rig");
        assert_eq!(json[0]["accepted"], true);

        let (_, json) = get_json(&state, "/api/shares?limit=0").await;
        assert_eq!(
            json.as_array().unwrap().len(),
            1,
            "limit clamps to at least 1"
        );

        let (_, json) = get_json(&state, "/api/hashrate").await;
        assert_eq!(json.as_array().unwrap().len(), 1, "default span is one day");
        assert_eq!(json[0]["worker"], "");
        let (_, json) = get_json(&state, "/api/hashrate?span=999999999").await;
        assert_eq!(json.as_array().unwrap().len(), 2, "span caps at 30 days");
        let (_, json) = get_json(&state, "/api/hashrate?worker=rig").await;
        assert_eq!(json[0]["worker"], "rig");

        let (status, json) = get_json(&state, "/api/blocks").await;
        assert_eq!(status, StatusCode::OK);
        assert!(json.as_array().unwrap().is_empty());
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[tokio::test]
    async fn websocket_sends_the_snapshot_now_and_on_every_publish() {
        let (state, path) = state("ws").await;
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let app = router(state.clone());
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

        let (mut socket, _) = tokio_tungstenite::connect_async(format!("ws://{addr}/api/ws"))
            .await
            .unwrap();
        let first = socket.next().await.unwrap().unwrap().into_text().unwrap();
        let first: serde_json::Value = serde_json::from_str(&first).unwrap();
        assert_eq!(first["pool_name"], "test");
        assert_eq!(first["shares_accepted"], 0);

        state.publish(PoolSnapshot {
            shares_accepted: 7,
            ..Default::default()
        });
        let second = socket.next().await.unwrap().unwrap().into_text().unwrap();
        let second: serde_json::Value = serde_json::from_str(&second).unwrap();
        assert_eq!(second["shares_accepted"], 7);

        socket.close(None).await.unwrap();
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }
}
