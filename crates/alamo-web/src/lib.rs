//! HTTP API and embedded dashboard.

#![forbid(unsafe_code)]

pub mod api;
pub mod assets;
pub mod config;
pub mod snapshot;
pub mod ws;

use alamo_store::Store;
use axum::extract::ws::Utf8Bytes;
use axum::routing::get;
use axum::Router;
use serde::Serialize;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Instant;
use tokio::sync::broadcast;
use tokio_util::sync::CancellationToken;
use tower_http::trace::TraceLayer;

pub use config::WebConfig;
pub use snapshot::{
    AuxPayoutStatus, CoinStatus, PoolSnapshot, RoundStatus, ShareEvent, WorkerStatus,
};

/// Messages queued per WebSocket client before it is considered lagging and skipped ahead.
const PUSH_CAPACITY: usize = 256;

/// State shared with request handlers.
#[derive(Clone)]
pub struct AppState {
    inner: Arc<Inner>,
}

struct Inner {
    pool_name: String,
    started_at: Instant,
    store: Option<Store>,
    snapshot: parking_lot::RwLock<PoolSnapshot>,
    pushes: broadcast::Sender<Utf8Bytes>,
}

/// Envelope for every WebSocket message: `{"type": ..., "data": ...}`.
#[derive(Serialize)]
struct Push<'a, T: Serialize> {
    r#type: &'static str,
    data: &'a T,
}

impl AppState {
    /// Create the application state without database-backed endpoints.
    pub fn new(pool_name: impl Into<String>) -> Self {
        Self {
            inner: Arc::new(Inner {
                pool_name: pool_name.into(),
                started_at: Instant::now(),
                store: None,
                snapshot: parking_lot::RwLock::new(PoolSnapshot::default()),
                pushes: broadcast::channel(PUSH_CAPACITY).0,
            }),
        }
    }

    /// Create the application state with history endpoints backed by `store`.
    pub fn with_store(pool_name: impl Into<String>, store: Store) -> Self {
        let mut state = Self::new(pool_name);
        Arc::get_mut(&mut state.inner)
            .expect("freshly created state is unshared")
            .store = Some(store);
        state
    }

    /// Configured pool name.
    pub fn pool_name(&self) -> &str {
        &self.inner.pool_name
    }

    /// Seconds since the daemon started.
    pub fn uptime_seconds(&self) -> u64 {
        self.inner.started_at.elapsed().as_secs()
    }

    /// Database handle, if history endpoints are enabled.
    pub fn store(&self) -> Option<&Store> {
        self.inner.store.as_ref()
    }

    /// Replace the published status document and push it to WebSocket clients.
    pub fn publish(&self, mut snapshot: PoolSnapshot) {
        self.stamp(&mut snapshot);
        self.push("status", &snapshot);
        *self.inner.snapshot.write() = snapshot;
    }

    /// Push a share to WebSocket clients' live log. Cheap when nobody is listening.
    pub fn push_share(&self, share: &ShareEvent) {
        if self.inner.pushes.receiver_count() > 0 {
            self.push("share", share);
        }
    }

    /// The current status document, with identity and uptime stamped on.
    pub fn snapshot(&self) -> PoolSnapshot {
        let mut s = self.inner.snapshot.read().clone();
        self.stamp(&mut s);
        s
    }

    fn stamp(&self, s: &mut PoolSnapshot) {
        s.pool_name = self.inner.pool_name.clone();
        s.version = env!("CARGO_PKG_VERSION").to_string();
        s.uptime_seconds = self.uptime_seconds();
    }

    fn push<T: Serialize>(&self, kind: &'static str, data: &T) {
        match serde_json::to_string(&Push { r#type: kind, data }) {
            // A send error only means there are no subscribers right now.
            Ok(text) => drop(self.inner.pushes.send(Utf8Bytes::from(text))),
            Err(err) => tracing::warn!(kind, %err, "could not serialize push"),
        }
    }

    fn subscribe(&self) -> broadcast::Receiver<Utf8Bytes> {
        self.inner.pushes.subscribe()
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
        .route("/api/hashrate", get(api::hashrate))
        .route("/api/shares", get(api::shares))
        .route("/api/blocks", get(api::blocks))
        .route("/api/ws", get(ws::upgrade))
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
