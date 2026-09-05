//! HTTP API and embedded dashboard.

#![forbid(unsafe_code)]

pub mod api;
pub mod assets;
pub mod config;
pub mod snapshot;

use axum::routing::get;
use axum::Router;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Instant;
use tokio_util::sync::CancellationToken;
use tower_http::trace::TraceLayer;

pub use config::WebConfig;
pub use snapshot::{AuxPayoutStatus, CoinStatus, PoolSnapshot, WorkerStatus};

/// State shared with request handlers.
#[derive(Clone)]
pub struct AppState {
    inner: Arc<Inner>,
}

struct Inner {
    pool_name: String,
    started_at: Instant,
    snapshot: parking_lot::RwLock<PoolSnapshot>,
}

impl AppState {
    /// Create the application state.
    pub fn new(pool_name: impl Into<String>) -> Self {
        Self {
            inner: Arc::new(Inner {
                pool_name: pool_name.into(),
                started_at: Instant::now(),
                snapshot: parking_lot::RwLock::new(PoolSnapshot::default()),
            }),
        }
    }

    /// Configured pool name.
    pub fn pool_name(&self) -> &str {
        &self.inner.pool_name
    }

    /// Seconds since the daemon started.
    pub fn uptime_seconds(&self) -> u64 {
        self.inner.started_at.elapsed().as_secs()
    }

    /// Replace the published status document.
    pub fn publish(&self, snapshot: PoolSnapshot) {
        *self.inner.snapshot.write() = snapshot;
    }

    /// The current status document, with identity and uptime stamped on.
    pub fn snapshot(&self) -> PoolSnapshot {
        let mut s = self.inner.snapshot.read().clone();
        s.pool_name = self.inner.pool_name.clone();
        s.version = env!("CARGO_PKG_VERSION").to_string();
        s.uptime_seconds = self.uptime_seconds();
        s
    }
}

/// Errors from the HTTP server.
#[derive(Debug, thiserror::Error)]
pub enum ServeError {
    /// Could not bind the listen address.
    #[error("failed to bind {addr}: {source}")]
    Bind {
        /// The configured address.
        addr: SocketAddr,
        /// Underlying error.
        source: std::io::Error,
    },
    /// The server loop failed.
    #[error("http server error: {0}")]
    Serve(#[source] std::io::Error),
}

/// Build the router: `/api/*` handlers and the embedded dashboard for everything else.
pub fn router(state: AppState) -> Router {
    Router::new()
        .route("/api/health", get(api::health))
        .route("/api/status", get(api::status))
        .fallback(assets::serve)
        .layer(TraceLayer::new_for_http())
        .with_state(state)
}

/// Run the HTTP server until `shutdown` is cancelled.
pub async fn serve(
    config: WebConfig,
    state: AppState,
    shutdown: CancellationToken,
) -> Result<(), ServeError> {
    let listener = tokio::net::TcpListener::bind(config.listen)
        .await
        .map_err(|source| ServeError::Bind {
            addr: config.listen,
            source,
        })?;
    tracing::info!(addr = %config.listen, "dashboard listening");
    axum::serve(listener, router(state))
        .with_graceful_shutdown(async move { shutdown.cancelled().await })
        .await
        .map_err(ServeError::Serve)
}
