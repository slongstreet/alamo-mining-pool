//! JSON API handlers.

use crate::{AppState, PoolSnapshot};
use alamo_core::time::now_unix;
use alamo_store::{BlockRow, HashrateSample, ShareRow, StoreError};
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

/// Why a history request could not be answered.
#[derive(Debug, thiserror::Error)]
pub enum ApiError {
    /// The daemon runs without a database (tests only).
    #[error("history is not available")]
    NoStore,
    /// A query parameter was not understood.
    #[error("{0}")]
    BadRequest(String),
    /// The database failed.
    #[error("database error")]
    Store(#[from] StoreError),
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let status = match &self {
            ApiError::NoStore => StatusCode::SERVICE_UNAVAILABLE,
            ApiError::BadRequest(_) => StatusCode::BAD_REQUEST,
            ApiError::Store(err) => {
                tracing::warn!(%err, "history query failed");
                StatusCode::INTERNAL_SERVER_ERROR
            }
        };
        (
            status,
            Json(serde_json::json!({ "error": self.to_string() })),
        )
            .into_response()
    }
}

/// Query for `/api/hashrate`.
#[derive(Deserialize)]
pub struct HashrateQuery {
    /// One of `1h`, `6h`, `24h`, `7d`, `30d`. Defaults to `24h`.
    #[serde(default)]
    pub range: Option<String>,
    /// Worker name; omitted or empty for the pool total.
    #[serde(default)]
    pub worker: Option<String>,
}

/// Response body for `/api/hashrate`.
#[derive(Serialize)]
pub struct HashrateHistory {
    /// Range label as requested.
    pub range: String,
    /// Unix time the history starts at.
    pub since: i64,
    /// Unix time the history was read.
    pub now: i64,
    /// Worker the samples belong to; empty for the pool total.
    pub worker: String,
    /// Samples, oldest first.
    pub points: Vec<HashratePoint>,
}

/// One hashrate history point.
#[derive(Serialize)]
pub struct HashratePoint {
    /// Unix time.
    pub ts: i64,
    /// Hashes per second.
    pub hashrate: f64,
}

impl From<HashrateSample> for HashratePoint {
    fn from(s: HashrateSample) -> Self {
        Self {
            ts: s.ts,
            hashrate: s.hashrate,
        }
    }
}

fn range_seconds(range: &str) -> Option<i64> {
    Some(match range {
        "1h" => 3_600,
        "6h" => 6 * 3_600,
        "24h" => 24 * 3_600,
        "7d" => 7 * 24 * 3_600,
        "30d" => 30 * 24 * 3_600,
        _ => return None,
    })
}

/// Hashrate history for the pool or one worker.
pub async fn hashrate(
    State(state): State<AppState>,
    Query(q): Query<HashrateQuery>,
) -> Result<Json<HashrateHistory>, ApiError> {
    let store = state.store().ok_or(ApiError::NoStore)?;
    let range = q.range.unwrap_or_else(|| "24h".into());
    let seconds = range_seconds(&range)
        .ok_or_else(|| ApiError::BadRequest(format!("unknown range '{range}'")))?;
    let worker = q.worker.unwrap_or_default();
    let now = now_unix() as i64;
    let since = now - seconds;
    let points = store
        .hashrate_samples_since(Some(&worker), since)
        .await?
        .into_iter()
        .map(HashratePoint::from)
        .collect();
    Ok(Json(HashrateHistory {
        range,
        since,
        now,
        worker,
        points,
    }))
}

/// Query for list endpoints.
#[derive(Deserialize)]
pub struct LimitQuery {
    /// Maximum rows, capped server-side.
    #[serde(default)]
    pub limit: Option<i64>,
}

impl LimitQuery {
    fn limit(&self, default: i64, max: i64) -> i64 {
        self.limit.unwrap_or(default).clamp(1, max)
    }
}

/// Most recent shares, newest first.
pub async fn shares(
    State(state): State<AppState>,
    Query(q): Query<LimitQuery>,
) -> Result<Json<Vec<ShareRow>>, ApiError> {
    let store = state.store().ok_or(ApiError::NoStore)?;
    Ok(Json(store.recent_shares(q.limit(100, 1_000)).await?))
}

/// Blocks found, newest first.
pub async fn blocks(
    State(state): State<AppState>,
    Query(q): Query<LimitQuery>,
) -> Result<Json<Vec<BlockRow>>, ApiError> {
    let store = state.store().ok_or(ApiError::NoStore)?;
    Ok(Json(store.recent_blocks(q.limit(100, 1_000)).await?))
}

#[cfg(test)]
mod tests {
    use crate::{router, AppState, PoolSnapshot, ShareEvent};
    use alamo_store::{HashrateSample, Store};
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use tower::ServiceExt;

    async fn get(app: axum::Router, uri: &str) -> (StatusCode, serde_json::Value) {
        let res = app
            .oneshot(Request::builder().uri(uri).body(Body::empty()).unwrap())
            .await
            .unwrap();
        let status = res.status();
        let bytes = axum::body::to_bytes(res.into_body(), 1 << 20)
            .await
            .unwrap();
        let json = serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null);
        (status, json)
    }

    fn temp_db(tag: &str) -> std::path::PathBuf {
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

    #[tokio::test]
    async fn health_returns_ok() {
        let (status, json) = get(router(AppState::new("test")), "/api/health").await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(json["status"], "ok");
    }

    #[tokio::test]
    async fn unknown_path_serves_dashboard_or_placeholder() {
        let (status, _) = get(router(AppState::new("test")), "/workers").await;
        assert_eq!(status, StatusCode::OK);
    }

    #[tokio::test]
    async fn history_without_store_is_unavailable() {
        let (status, json) = get(router(AppState::new("test")), "/api/hashrate").await;
        assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
        assert!(json["error"].is_string());
    }

    #[tokio::test]
    async fn history_endpoints_read_the_store() {
        let path = temp_db("history");
        let store = Store::open(&path).await.unwrap();
        let now = alamo_core::time::now_unix() as i64;
        store
            .insert_hashrate_samples(&[
                HashrateSample {
                    ts: now - 120,
                    worker: String::new(),
                    hashrate: 5.0,
                },
                HashrateSample {
                    ts: now - 60,
                    worker: "rig".into(),
                    hashrate: 3.0,
                },
                HashrateSample {
                    ts: now - 2 * 24 * 3_600,
                    worker: String::new(),
                    hashrate: 1.0,
                },
            ])
            .await
            .unwrap();
        let state = AppState::with_store("test", store);

        let (status, json) = get(router(state.clone()), "/api/hashrate?range=1h").await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(json["worker"], "");
        assert_eq!(json["points"].as_array().unwrap().len(), 1);
        assert_eq!(json["points"][0]["hashrate"], 5.0);

        let (_, json) = get(router(state.clone()), "/api/hashrate?range=7d&worker=rig").await;
        assert_eq!(json["points"].as_array().unwrap().len(), 1);
        assert_eq!(json["points"][0]["hashrate"], 3.0);

        let (status, _) = get(router(state.clone()), "/api/hashrate?range=2y").await;
        assert_eq!(status, StatusCode::BAD_REQUEST);

        let (status, json) = get(router(state.clone()), "/api/shares?limit=5").await;
        assert_eq!(status, StatusCode::OK);
        assert!(json.as_array().unwrap().is_empty());
        let (status, json) = get(router(state), "/api/blocks").await;
        assert_eq!(status, StatusCode::OK);
        assert!(json.as_array().unwrap().is_empty());
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[tokio::test]
    async fn publish_pushes_stamped_status_and_shares_to_subscribers() {
        let state = AppState::new("Pushy");
        // Nobody listening: publishing and share pushes must not fail.
        state.publish(PoolSnapshot::default());
        let share = ShareEvent {
            ts: 1,
            worker: "w".into(),
            coin: "LTC".into(),
            difficulty: 1.0,
            share_diff: 2.0,
            accepted: true,
            reject_reason: None,
        };
        state.push_share(&share);

        let mut rx = state.subscribe();
        state.publish(PoolSnapshot {
            hashrate: 42.0,
            ..Default::default()
        });
        state.push_share(&share);
        let status: serde_json::Value =
            serde_json::from_str(rx.recv().await.unwrap().as_str()).unwrap();
        assert_eq!(status["type"], "status");
        assert_eq!(status["data"]["pool_name"], "Pushy");
        assert_eq!(status["data"]["hashrate"], 42.0);
        let pushed: serde_json::Value =
            serde_json::from_str(rx.recv().await.unwrap().as_str()).unwrap();
        assert_eq!(pushed["type"], "share");
        assert_eq!(pushed["data"]["worker"], "w");
        assert_eq!(state.snapshot().hashrate, 42.0);
    }
}
