//! JSON API handlers and the snapshot WebSocket.

use crate::settings::{NodeProbe, SettingsDoc, SettingsError, SettingsPatch};
use crate::{AppState, Command, PoolSnapshot};
use alamo_store::{BlockRow, HashrateSample, ShareRow, StoreError};
use axum::body::Body;
use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{Path, Query, Request, State};
use axum::http::{header, StatusCode};
use axum::middleware::Next;
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

/// Stratum share units per unit of network difficulty, or 1 before the pool has
/// published a snapshot. The store keeps `share_diff` in stratum units; the API reports
/// network units, matching the best-share figures in the status document.
fn share_multiplier(state: &AppState) -> f64 {
    let m = state.snapshot().share_multiplier;
    if m > 0.0 {
        m
    } else {
        1.0
    }
}

/// Most recent shares, newest first. `share_diff` is in network units; `difficulty`
/// (the job's) stays in stratum units.
pub async fn shares(
    State(state): State<AppState>,
    Query(q): Query<LimitQuery>,
) -> Result<Json<Vec<ShareRow>>, ApiError> {
    let m = share_multiplier(&state);
    let mut rows = state.store().recent_shares(q.clamp(100, 1000)).await?;
    for r in &mut rows {
        r.share_diff /= m;
    }
    Ok(Json(rows))
}

/// `POST /api/stats/reset`: zero accepted/rejected share counts and best share for every
/// worker. The pool task applies it within one publish interval; the response only
/// confirms it was queued.
pub async fn reset_stats(State(state): State<AppState>) -> (StatusCode, Json<serde_json::Value>) {
    if state.send_command(Command::ResetStats) {
        (
            StatusCode::ACCEPTED,
            Json(serde_json::json!({ "status": "queued" })),
        )
    } else {
        (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(serde_json::json!({ "error": "pool is not accepting commands" })),
        )
    }
}

/// `DELETE /api/workers/{name}`: forget a worker that is not connected. Refused with 409
/// while it has a live session and 404 when the dashboard has never seen it. Like reset,
/// the pool task applies it within one publish interval.
pub async fn remove_worker(
    State(state): State<AppState>,
    Path(name): Path<String>,
) -> (StatusCode, Json<serde_json::Value>) {
    let snapshot = state.snapshot();
    let Some(worker) = snapshot.workers.iter().find(|w| w.name == name) else {
        return (
            StatusCode::NOT_FOUND,
            Json(serde_json::json!({ "error": "no such worker" })),
        );
    };
    if worker.connections > 0 {
        return (
            StatusCode::CONFLICT,
            Json(serde_json::json!({ "error": "worker is connected; disconnect it first" })),
        );
    }
    if state.send_command(Command::RemoveWorker(name)) {
        (
            StatusCode::ACCEPTED,
            Json(serde_json::json!({ "status": "queued" })),
        )
    } else {
        (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(serde_json::json!({ "error": "pool is not accepting commands" })),
        )
    }
}

/// Blocks found, newest first.
pub async fn blocks(
    State(state): State<AppState>,
    Query(q): Query<LimitQuery>,
) -> Result<Json<Vec<BlockRow>>, ApiError> {
    let m = share_multiplier(&state);
    let mut rows = state.store().recent_blocks(q.clamp(50, 500)).await?;
    for r in &mut rows {
        r.share_diff /= m;
    }
    Ok(Json(rows))
}

/// A refused request: a JSON `{"error": ...}` body with the given status.
#[derive(Debug)]
pub struct Refusal(StatusCode, String);

impl IntoResponse for Refusal {
    fn into_response(self) -> Response {
        (self.0, Json(serde_json::json!({ "error": self.1 }))).into_response()
    }
}

fn error(status: StatusCode, message: impl Into<String>) -> Refusal {
    Refusal(status, message.into())
}

/// Middleware on every mutating route: 403 while `[web] read_only` is set.
pub async fn refuse_when_read_only(
    State(state): State<AppState>,
    request: Request,
    next: Next,
) -> Response {
    if state.read_only() {
        return error(
            StatusCode::FORBIDDEN,
            "the dashboard is read-only ([web] read_only = true)",
        )
        .into_response();
    }
    next.run(request).await
}

/// The settings hook, or the 503 to answer with while the pool is still connecting.
fn operator(state: &AppState) -> Result<std::sync::Arc<dyn crate::Operator>, Refusal> {
    state.operator().ok_or_else(|| {
        error(
            StatusCode::SERVICE_UNAVAILABLE,
            "the pool is still connecting to its nodes",
        )
    })
}

fn with_read_only(state: &AppState, mut doc: SettingsDoc) -> SettingsDoc {
    doc.read_only = state.read_only();
    doc
}

/// `GET /api/settings`: the settings document.
pub async fn get_settings(State(state): State<AppState>) -> Result<Json<SettingsDoc>, Refusal> {
    let op = operator(&state)?;
    Ok(Json(with_read_only(&state, op.settings())))
}

/// `PUT /api/settings`: apply a patch and return the new document. Invalid values are
/// refused with 400 and a reason; nothing is stored unless the whole patch validates.
pub async fn put_settings(
    State(state): State<AppState>,
    Json(patch): Json<SettingsPatch>,
) -> Result<Json<SettingsDoc>, Refusal> {
    let op = operator(&state)?;
    match op.apply(patch).await {
        Ok(doc) => Ok(Json(with_read_only(&state, doc))),
        Err(SettingsError::Invalid(reason)) => Err(error(StatusCode::BAD_REQUEST, reason)),
        Err(err @ SettingsError::Store(_)) => {
            tracing::warn!(%err, "settings change failed");
            Err(error(StatusCode::INTERNAL_SERVER_ERROR, err.to_string()))
        }
    }
}

/// `POST /api/nodes/{key}/test`: ask a coin's node who it is. A node that does not
/// answer is a 502 with the reason.
pub async fn test_node(
    State(state): State<AppState>,
    Path(key): Path<String>,
) -> Result<Json<NodeProbe>, Refusal> {
    let op = operator(&state)?;
    op.probe_node(&key)
        .await
        .map(Json)
        .map_err(|reason| error(StatusCode::BAD_GATEWAY, reason))
}

fn attachment(name: &str, mime: &'static str) -> [(header::HeaderName, String); 3] {
    [
        (header::CONTENT_TYPE, mime.to_string()),
        (
            header::CONTENT_DISPOSITION,
            format!("attachment; filename=\"{name}\""),
        ),
        (header::CACHE_CONTROL, "no-store".to_string()),
    ]
}

fn stamp() -> String {
    chrono::Utc::now().format("%Y%m%d-%H%M%S").to_string()
}

/// `GET /api/logs`: the recent log as a text file.
pub async fn download_logs(State(state): State<AppState>) -> Response {
    let mut text = format!(
        "# alamo {} log, downloaded {}\n",
        env!("CARGO_PKG_VERSION"),
        chrono::Utc::now().format("%Y-%m-%dT%H:%M:%SZ")
    );
    text.push_str(&state.logs().contents());
    (
        attachment(
            &format!("alamo-{}.log", stamp()),
            "text/plain; charset=utf-8",
        ),
        text,
    )
        .into_response()
}

/// `GET /api/backup`: a consistent copy of the database, made with `VACUUM INTO` and
/// streamed from a temporary file beside it that is unlinked as soon as it is open.
pub async fn download_backup(State(state): State<AppState>) -> Response {
    let store = state.store();
    let temp = store.path().with_file_name(format!(
        "alamo-backup-{}-{}.db",
        std::process::id(),
        stamp()
    ));
    if let Err(err) = store.backup_to(&temp).await {
        tracing::warn!(%err, "database backup failed");
        return error(
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("backup failed: {err}"),
        )
        .into_response();
    }
    let file = match tokio::fs::File::open(&temp).await {
        Ok(file) => file,
        Err(err) => {
            let _ = tokio::fs::remove_file(&temp).await;
            return error(
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("backup failed: {err}"),
            )
            .into_response();
        }
    };
    let size = file.metadata().await.ok().map(|m| m.len());
    // The open handle keeps the data readable after the name is gone.
    if let Err(err) = tokio::fs::remove_file(&temp).await {
        tracing::warn!(path = %temp.display(), %err, "could not remove backup file");
    }
    let mut response = (
        attachment(&format!("alamo-{}.db", stamp()), "application/vnd.sqlite3"),
        Body::from_stream(tokio_util::io::ReaderStream::new(file)),
    )
        .into_response();
    if let Some(size) = size {
        response
            .headers_mut()
            .insert(header::CONTENT_LENGTH, size.into());
    }
    response
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
        (AppState::new("test", 3333, store), path)
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

    /// An operator that records patches and answers with a fixed document.
    struct FakeOperator {
        patches: std::sync::Mutex<Vec<crate::SettingsPatch>>,
    }

    impl crate::Operator for FakeOperator {
        fn settings(&self) -> crate::SettingsDoc {
            crate::SettingsDoc {
                pool_name: crate::Setting::from_file("Fake".to_string()),
                coinbase_tag_max_bytes: 41,
                ..Default::default()
            }
        }

        fn apply(
            &self,
            patch: crate::SettingsPatch,
        ) -> crate::settings::BoxFuture<'_, Result<crate::SettingsDoc, crate::SettingsError>>
        {
            Box::pin(async move {
                if patch.pool_name == Some(Some(String::new())) {
                    return Err(crate::SettingsError::Invalid(
                        "pool name must not be empty".into(),
                    ));
                }
                let mut doc = self.settings();
                if let Some(Some(name)) = &patch.pool_name {
                    doc.pool_name = crate::Setting::overridden("Fake".into(), name.clone());
                }
                self.patches.lock().unwrap().push(patch);
                Ok(doc)
            })
        }

        fn probe_node(
            &self,
            key: &str,
        ) -> crate::settings::BoxFuture<'_, Result<crate::NodeProbe, String>> {
            let key = key.to_string();
            Box::pin(async move {
                if key == "ltc" {
                    Ok(crate::NodeProbe {
                        key,
                        subversion: "/LitecoinCore:0.21.4/".into(),
                        ..Default::default()
                    })
                } else {
                    Err(format!("unknown coin {key}"))
                }
            })
        }
    }

    async fn send(
        state: &AppState,
        method: &str,
        uri: &str,
        body: Option<serde_json::Value>,
    ) -> (StatusCode, axum::http::HeaderMap, Vec<u8>) {
        let mut req = Request::builder().method(method).uri(uri);
        let body = match body {
            Some(json) => {
                req = req.header("content-type", "application/json");
                Body::from(json.to_string())
            }
            None => Body::empty(),
        };
        let res = router(state.clone())
            .oneshot(req.body(body).unwrap())
            .await
            .unwrap();
        let status = res.status();
        let headers = res.headers().clone();
        let bytes = axum::body::to_bytes(res.into_body(), 1 << 24)
            .await
            .unwrap();
        (status, headers, bytes.to_vec())
    }

    #[tokio::test]
    async fn settings_wait_for_the_operator_then_pass_patches_through() {
        let (state, path) = state("settings").await;
        let (status, _, body) = send(&state, "GET", "/api/settings", None).await;
        assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE, "{body:?}");

        let op = std::sync::Arc::new(FakeOperator {
            patches: Default::default(),
        });
        state.install_operator(op.clone());
        let (status, _, body) = send(&state, "GET", "/api/settings", None).await;
        assert_eq!(status, StatusCode::OK);
        let doc: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(doc["pool_name"]["value"], "Fake");
        assert_eq!(doc["read_only"], false);

        let patch =
            serde_json::json!({ "pool_name": "Renamed", "vardiff": { "min_difficulty": null } });
        let (status, _, body) = send(&state, "PUT", "/api/settings", Some(patch)).await;
        assert_eq!(status, StatusCode::OK, "{}", String::from_utf8_lossy(&body));
        let doc: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(doc["pool_name"]["value"], "Renamed");
        assert_eq!(doc["pool_name"]["overridden"], true);
        {
            let recorded = op.patches.lock().unwrap();
            assert_eq!(recorded.len(), 1);
            assert_eq!(recorded[0].vardiff.min_difficulty, Some(None));
        }

        let (status, _, body) = send(
            &state,
            "PUT",
            "/api/settings",
            Some(serde_json::json!({ "pool_name": "" })),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        let err: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(err["error"], "pool name must not be empty");

        let (status, _, body) = send(&state, "POST", "/api/nodes/ltc/test", None).await;
        assert_eq!(status, StatusCode::OK);
        let probe: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(probe["subversion"], "/LitecoinCore:0.21.4/");
        let (status, _, _) = send(&state, "POST", "/api/nodes/btc/test", None).await;
        assert_eq!(status, StatusCode::BAD_GATEWAY);
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[tokio::test]
    async fn read_only_refuses_every_write_but_still_serves_reads() {
        let (state, path) = state("readonly").await;
        state.install_operator(std::sync::Arc::new(FakeOperator {
            patches: Default::default(),
        }));
        state.set_read_only(true);
        let _rx = state.take_commands().unwrap();
        for (method, uri) in [
            ("POST", "/api/stats/reset"),
            ("DELETE", "/api/workers/x"),
            ("PUT", "/api/settings"),
        ] {
            let body = (method == "PUT").then(|| serde_json::json!({}));
            let (status, _, bytes) = send(&state, method, uri, body).await;
            assert_eq!(status, StatusCode::FORBIDDEN, "{method} {uri}");
            let err: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
            assert!(err["error"].as_str().unwrap().contains("read-only"));
        }
        let (status, _, body) = send(&state, "GET", "/api/settings", None).await;
        assert_eq!(status, StatusCode::OK);
        let doc: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(doc["read_only"], true);
        let (status, _, _) = send(&state, "GET", "/api/logs", None).await;
        assert_eq!(status, StatusCode::OK);
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[tokio::test]
    async fn logs_and_backup_download_as_attachments() {
        let (state, path) = state("downloads").await;
        let logs = crate::LogBuffer::default();
        logs.push("2026-09-16T00:00:00.000Z  INFO hello".into());
        state.install_logs(logs);
        let (status, headers, body) = send(&state, "GET", "/api/logs", None).await;
        assert_eq!(status, StatusCode::OK);
        let disposition = headers["content-disposition"].to_str().unwrap();
        assert!(
            disposition.starts_with("attachment; filename=\"alamo-"),
            "{disposition}"
        );
        assert!(disposition.ends_with(".log\""), "{disposition}");
        let text = String::from_utf8(body).unwrap();
        assert!(text.starts_with("# alamo "), "{text}");
        assert!(text.ends_with("INFO hello\n"), "{text}");

        state
            .store()
            .set_setting("pool.name", "kept", 1)
            .await
            .unwrap();
        let (status, headers, body) = send(&state, "GET", "/api/backup", None).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(headers["content-type"], "application/vnd.sqlite3");
        assert_eq!(
            headers["content-length"].to_str().unwrap(),
            body.len().to_string()
        );
        assert!(body.starts_with(b"SQLite format 3\0"));
        let restored_path = path.with_file_name("restored.db");
        std::fs::write(&restored_path, &body).unwrap();
        let restored = Store::open(&restored_path).await.unwrap();
        assert_eq!(restored.load_settings().await.unwrap()["pool.name"], "kept");
        // The temporary file was unlinked once open.
        let leftovers: Vec<_> = std::fs::read_dir(path.parent().unwrap())
            .unwrap()
            .filter_map(|e| e.ok())
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|n| n.contains("backup"))
            .collect();
        assert!(leftovers.is_empty(), "{leftovers:?}");
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[tokio::test]
    async fn reset_stats_is_queued_for_the_pool_task() {
        use crate::Command;
        let (state, path) = state("reset").await;
        let mut rx = state.take_commands().unwrap();
        let post = || {
            Request::builder()
                .method("POST")
                .uri("/api/stats/reset")
                .body(Body::empty())
                .unwrap()
        };
        let res = router(state.clone()).oneshot(post()).await.unwrap();
        assert_eq!(res.status(), StatusCode::ACCEPTED);
        assert_eq!(rx.try_recv().unwrap(), Command::ResetStats);
        // GET is not allowed: the reset must be a deliberate POST.
        let res = router(state.clone())
            .oneshot(
                Request::builder()
                    .uri("/api/stats/reset")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::METHOD_NOT_ALLOWED);
        drop(rx);
        let res = router(state).oneshot(post()).await.unwrap();
        assert_eq!(res.status(), StatusCode::SERVICE_UNAVAILABLE);
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[tokio::test]
    async fn remove_worker_is_queued_only_for_known_offline_workers() {
        use crate::{Command, WorkerStatus};
        let (state, path) = state("remove").await;
        let mut rx = state.take_commands().unwrap();
        let worker = |name: &str, connections: usize| WorkerStatus {
            name: name.into(),
            address: "addr".into(),
            fallback: false,
            aux_payouts: Vec::new(),
            connections,
            difficulty: 1.0,
            hashrate: 0.0,
            shares_accepted: 0,
            shares_rejected: 0,
            best_difficulty: 0.0,
            work_accepted: 0.0,
            last_share_seconds: None,
        };
        state.publish(PoolSnapshot {
            workers: vec![worker("live", 1), worker("gone", 0)],
            ..PoolSnapshot::default()
        });
        let delete = |name: &str| {
            Request::builder()
                .method("DELETE")
                .uri(format!("/api/workers/{name}"))
                .body(Body::empty())
                .unwrap()
        };
        let res = router(state.clone()).oneshot(delete("gone")).await.unwrap();
        assert_eq!(res.status(), StatusCode::ACCEPTED);
        assert_eq!(rx.try_recv().unwrap(), Command::RemoveWorker("gone".into()));
        let res = router(state.clone()).oneshot(delete("live")).await.unwrap();
        assert_eq!(res.status(), StatusCode::CONFLICT);
        let res = router(state.clone())
            .oneshot(delete("nobody"))
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::NOT_FOUND);
        assert!(rx.try_recv().is_err(), "only the offline worker was queued");
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
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
    async fn metrics_serves_prometheus_text() {
        let (state, path) = state("metrics").await;
        state.publish(PoolSnapshot {
            shares_accepted: 3,
            ..Default::default()
        });
        let res = router(state)
            .oneshot(
                Request::builder()
                    .uri("/metrics")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        assert_eq!(
            res.headers()["content-type"],
            crate::metrics::CONTENT_TYPE_TEXT
        );
        let bytes = axum::body::to_bytes(res.into_body(), 1 << 20)
            .await
            .unwrap();
        let text = String::from_utf8(bytes.to_vec()).unwrap();
        assert!(text.contains("alamo_shares_total{result=\"accepted\"} 3\n"));
        assert!(text.contains("alamo_info{version=\""));
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
        assert_eq!(
            json[0]["share_diff"], 9.0,
            "stratum units until a snapshot says otherwise"
        );

        // Once the pool reports its share multiplier, share_diff comes back in network
        // units while the job difficulty stays as the miner saw it.
        state.publish(PoolSnapshot {
            share_multiplier: 4.0,
            ..Default::default()
        });
        state
            .store()
            .insert_block(&alamo_store::NewBlock {
                coin: "LTC".into(),
                height: 1,
                hash: "00ab".into(),
                worker: "rig".into(),
                difficulty: 3.0,
                share_diff: 12.0,
                reward_sats: None,
                found_at: now as u64,
                status: alamo_store::BlockStatus::Accepted,
            })
            .await
            .unwrap();
        let (_, json) = get_json(&state, "/api/shares?limit=1").await;
        assert_eq!(json[0]["share_diff"], 2.25);
        assert_eq!(json[0]["difficulty"], 8.0);
        let (_, json) = get_json(&state, "/api/blocks").await;
        assert_eq!(json[0]["share_diff"], 3.0);
        assert_eq!(json[0]["difficulty"], 3.0);

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
        assert_eq!(json.as_array().unwrap().len(), 1);
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
        assert_eq!(first["stratum_port"], 3333);
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
