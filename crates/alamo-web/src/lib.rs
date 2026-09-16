//! HTTP API and embedded dashboard.

#![forbid(unsafe_code)]

pub mod api;
pub mod assets;
pub mod config;
pub mod logs;
pub mod metrics;
pub mod settings;
pub mod snapshot;

use alamo_store::Store;
use axum::routing::{delete, get, post, put};
use axum::{middleware, Router};
use std::net::SocketAddr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock, RwLock};
use std::time::Instant;
use tokio::sync::{mpsc, watch};
use tokio_util::sync::CancellationToken;
use tower_http::trace::TraceLayer;

pub use config::WebConfig;
pub use logs::LogBuffer;
pub use settings::{
    CoinSettings, LogControl, NodeProbe, Operator, Setting, SettingsDoc, SettingsError,
    SettingsPatch, VardiffPatch, VardiffSettings,
};
pub use snapshot::{
    AuxPayoutStatus, CoinStatus, NodeStatus, PoolSnapshot, RoundStatus, WorkerStatus,
};

/// An operator action requested through the API, carried out by the pool task that
/// owns the live statistics.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Command {
    /// Zero every worker's accepted and rejected share counts and best share.
    ResetStats,
    /// Forget a worker that has no live session: its row, share log, and hashrate
    /// history. Its accepted work stays in the pool total so rounds do not move.
    RemoveWorker(String),
}

/// State shared with request handlers.
#[derive(Clone)]
pub struct AppState {
    inner: Arc<Inner>,
}

struct Inner {
    pool_name: RwLock<String>,
    stratum_port: u16,
    /// `[web] read_only`: every mutating endpoint answers 403.
    read_only: AtomicBool,
    /// Recent log lines for download; empty until the binary installs its buffer.
    logs: OnceLock<LogBuffer>,
    /// Settings hook; absent until the pool has connected to its nodes.
    operator: OnceLock<Arc<dyn Operator>>,
    started_at: Instant,
    /// The latest status document. WebSocket clients subscribe to it.
    snapshot: watch::Sender<PoolSnapshot>,
    /// History (shares, samples, blocks) is read straight from the store.
    store: Store,
    /// Operator commands for the pool task; the receiver is taken once by that task.
    commands: mpsc::Sender<Command>,
    commands_rx: Mutex<Option<mpsc::Receiver<Command>>>,
}

impl AppState {
    /// Create the application state.
    pub fn new(pool_name: impl Into<String>, stratum_port: u16, store: Store) -> Self {
        let (commands, commands_rx) = mpsc::channel(8);
        Self {
            inner: Arc::new(Inner {
                pool_name: RwLock::new(pool_name.into()),
                stratum_port,
                read_only: AtomicBool::new(false),
                logs: OnceLock::new(),
                operator: OnceLock::new(),
                started_at: Instant::now(),
                snapshot: watch::Sender::new(PoolSnapshot::default()),
                store,
                commands,
                commands_rx: Mutex::new(Some(commands_rx)),
            }),
        }
    }

    /// The receiving end of operator commands. Only the first caller gets it.
    pub fn take_commands(&self) -> Option<mpsc::Receiver<Command>> {
        self.inner.commands_rx.lock().ok()?.take()
    }

    /// Queue an operator command. `false` when nobody is listening or the queue is full.
    pub fn send_command(&self, command: Command) -> bool {
        self.inner.commands.try_send(command).is_ok()
    }

    /// The database behind the history endpoints.
    pub fn store(&self) -> &Store {
        &self.inner.store
    }

    /// Subscribe to snapshot updates.
    pub fn subscribe(&self) -> watch::Receiver<PoolSnapshot> {
        self.inner.snapshot.subscribe()
    }

    /// Pool name shown on the dashboard.
    pub fn pool_name(&self) -> String {
        self.inner
            .pool_name
            .read()
            .map(|n| n.clone())
            .unwrap_or_default()
    }

    /// Rename the pool; the next snapshot carries the new name.
    pub fn set_pool_name(&self, name: &str) {
        if let Ok(mut current) = self.inner.pool_name.write() {
            *current = name.to_string();
        }
        self.inner.snapshot.send_modify(|_| {});
    }

    /// Refuse every mutating request from now on (or accept them again).
    pub fn set_read_only(&self, read_only: bool) {
        self.inner.read_only.store(read_only, Ordering::Relaxed);
    }

    /// Whether mutating requests are refused.
    pub fn read_only(&self) -> bool {
        self.inner.read_only.load(Ordering::Relaxed)
    }

    /// Hand over the log buffer the tracing subscriber writes to. Only the first call
    /// takes effect.
    pub fn install_logs(&self, logs: LogBuffer) {
        let _ = self.inner.logs.set(logs);
    }

    /// The log buffer, empty when none was installed.
    pub fn logs(&self) -> LogBuffer {
        self.inner.logs.get().cloned().unwrap_or_default()
    }

    /// Install the settings hook. Only the first call takes effect.
    pub fn install_operator(&self, operator: Arc<dyn Operator>) {
        let _ = self.inner.operator.set(operator);
    }

    /// The settings hook, once the pool has installed it.
    pub fn operator(&self) -> Option<Arc<dyn Operator>> {
        self.inner.operator.get().cloned()
    }

    /// Seconds since the daemon started.
    pub fn uptime_seconds(&self) -> u64 {
        self.inner.started_at.elapsed().as_secs()
    }

    /// Replace the published status document and wake WebSocket clients.
    pub fn publish(&self, mut snapshot: PoolSnapshot) {
        snapshot.pool_name = self.pool_name();
        snapshot.stratum_port = self.inner.stratum_port;
        snapshot.version = env!("CARGO_PKG_VERSION").to_string();
        snapshot.uptime_seconds = self.uptime_seconds();
        self.inner.snapshot.send_replace(snapshot);
    }

    /// The current status document.
    pub fn snapshot(&self) -> PoolSnapshot {
        let mut s = self.inner.snapshot.borrow().clone();
        s.pool_name = self.pool_name();
        s.stratum_port = self.inner.stratum_port;
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
    // Everything that changes state goes through the read-only guard.
    let writes = Router::new()
        .route("/api/stats/reset", post(api::reset_stats))
        .route("/api/workers/{name}", delete(api::remove_worker))
        .route("/api/settings", put(api::put_settings))
        .route_layer(middleware::from_fn_with_state(
            state.clone(),
            api::refuse_when_read_only,
        ));
    Router::new()
        .route("/api/health", get(api::health))
        .route("/api/status", get(api::status))
        .route("/api/ws", get(api::ws))
        .route("/api/hashrate", get(api::hashrate))
        .route("/api/shares", get(api::shares))
        .route("/api/blocks", get(api::blocks))
        .route("/api/settings", get(api::get_settings))
        .route("/api/nodes/{key}/test", post(api::test_node))
        .route("/api/logs", get(api::download_logs))
        .route("/api/backup", get(api::download_backup))
        .merge(writes)
        .route("/metrics", get(metrics::metrics))
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
