//! JSON API handlers.

use crate::AppState;
use axum::extract::State;
use axum::Json;
use serde::Serialize;

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

/// Response body for `/api/status`.
#[derive(Serialize)]
pub struct Status {
    /// Configured pool name.
    pub pool_name: String,
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

/// Pool status summary. Grows with each wave.
pub async fn status(State(state): State<AppState>) -> Json<Status> {
    Json(Status {
        pool_name: state.pool_name().to_string(),
        version: env!("CARGO_PKG_VERSION"),
        uptime_seconds: state.uptime_seconds(),
    })
}

#[cfg(test)]
mod tests {
    use crate::{router, AppState};
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use tower::ServiceExt;

    #[tokio::test]
    async fn health_returns_ok() {
        let app = router(AppState::new("test"));
        let res = app
            .oneshot(
                Request::builder()
                    .uri("/api/health")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        let bytes = axum::body::to_bytes(res.into_body(), 1024).await.unwrap();
        let json: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(json["status"], "ok");
    }

    #[tokio::test]
    async fn unknown_path_serves_dashboard_or_placeholder() {
        let app = router(AppState::new("test"));
        let res = app
            .oneshot(
                Request::builder()
                    .uri("/workers")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK);
    }
}
