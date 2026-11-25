//! Outbound webhook notifications: a block was found, a worker went quiet or came back, a
//! node became unreachable or recovered, a lane gave up starting.
//!
//! Producers hand an [`Event`] to a [`Notifier`], which only ever `try_send`s into a small
//! bounded queue: nothing on the share path or in the publisher loop waits for a webhook.
//! One sender task drains the queue and POSTs each event as JSON. Worker, node, and lane
//! transitions are derived by a [`Watcher`] from the status snapshot the publisher already
//! publishes, so no state is added to the statistics.
//!
//! The webhook URL is a secret (Discord, Slack, and ntfy URLs carry their token): it is
//! never logged, never part of an error, and redacted wherever the config is shown.

use alamo_core::time::now_unix;
use alamo_web::{LaneState, PoolSnapshot};
use anyhow::bail;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::{mpsc, watch};
use tokio_util::sync::CancellationToken;

/// Default for `worker_offline_minutes`.
pub const DEFAULT_WORKER_OFFLINE_MINUTES: u64 = 5;
/// Longest `worker_offline_minutes` accepted: a week.
const MAX_WORKER_OFFLINE_MINUTES: u64 = 7 * 24 * 60;
/// How long a node must stay unreachable before `node_down` fires, so one failed poll
/// does not notify.
pub const NODE_DOWN_AFTER: Duration = Duration::from_secs(30);
/// Notifications waiting for delivery; more are dropped with a warning.
const QUEUE_CAPACITY: usize = 64;
/// Per-attempt HTTP timeout.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(10);
/// Pauses between the three delivery attempts.
const RETRY_DELAYS: [Duration; 2] = [Duration::from_secs(2), Duration::from_secs(5)];
/// Discord refuses `content` over 2000 characters; stay below it.
const MAX_TEXT_CHARS: usize = 1900;
/// Placeholder for the URL in the redacted config.
const REDACTED: &str = "<redacted>";

/// What a notification is about, as named in `[notify] events` and in the payload.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EventKind {
    /// A block candidate was submitted and the node gave its verdict.
    BlockFound,
    /// A worker that was connected has had no connection for `worker_offline_minutes`.
    WorkerOffline,
    /// A worker reported offline connected again.
    WorkerOnline,
    /// A node has been unreachable (or its template withdrawn) for about 30 s.
    NodeDown,
    /// A node reported down is reachable again.
    NodeUp,
    /// A lane gave up starting.
    LaneFailed,
}

impl EventKind {
    /// Every kind, the default for `events`.
    pub const ALL: [EventKind; 6] = [
        EventKind::BlockFound,
        EventKind::WorkerOffline,
        EventKind::WorkerOnline,
        EventKind::NodeDown,
        EventKind::NodeUp,
        EventKind::LaneFailed,
    ];

    /// The name used in the config and the payload.
    pub fn as_str(self) -> &'static str {
        match self {
            EventKind::BlockFound => "block_found",
            EventKind::WorkerOffline => "worker_offline",
            EventKind::WorkerOnline => "worker_online",
            EventKind::NodeDown => "node_down",
            EventKind::NodeUp => "node_up",
            EventKind::LaneFailed => "lane_failed",
        }
    }

    fn bit(self) -> u8 {
        1 << (self as u8)
    }
}

fn all_events() -> Vec<EventKind> {
    EventKind::ALL.to_vec()
}

fn default_offline_minutes() -> u64 {
    DEFAULT_WORKER_OFFLINE_MINUTES
}

/// The `[notify]` section. File-only: the Settings page cannot change it.
#[derive(Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NotifyConfig {
    /// Where to POST notifications. Absent or empty: notifications are off. A secret.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub webhook_url: Option<String>,
    /// Which events to send. Default: all.
    #[serde(default = "all_events")]
    pub events: Vec<EventKind>,
    /// How long a worker that had a connection must have none before `worker_offline`.
    #[serde(default = "default_offline_minutes")]
    pub worker_offline_minutes: u64,
}

impl Default for NotifyConfig {
    fn default() -> Self {
        Self {
            webhook_url: None,
            events: all_events(),
            worker_offline_minutes: DEFAULT_WORKER_OFFLINE_MINUTES,
        }
    }
}

/// Never prints the URL, so a `{:?}` of the config cannot leak it.
impl std::fmt::Debug for NotifyConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("NotifyConfig")
            .field("webhook_url", &self.webhook_url().map(|_| REDACTED))
            .field("events", &self.events)
            .field("worker_offline_minutes", &self.worker_offline_minutes)
            .finish()
    }
}

impl NotifyConfig {
    /// The webhook URL, if notifications are on.
    pub fn webhook_url(&self) -> Option<&str> {
        self.webhook_url
            .as_deref()
            .map(str::trim)
            .filter(|u| !u.is_empty())
    }

    /// Whether the section says nothing the defaults do not.
    pub fn is_default(&self) -> bool {
        *self == Self::default()
    }

    /// Replace the URL with a placeholder, for the diagnostics view.
    pub fn redact(&mut self) {
        if self.webhook_url().is_some() {
            self.webhook_url = Some(REDACTED.to_string());
        }
    }

    /// How long a worker must be without a connection before `worker_offline`.
    pub fn offline_after(&self) -> Duration {
        Duration::from_secs(self.worker_offline_minutes.saturating_mul(60))
    }

    /// Check the section. Messages never include the URL.
    pub fn validate(&self) -> anyhow::Result<()> {
        if !(1..=MAX_WORKER_OFFLINE_MINUTES).contains(&self.worker_offline_minutes) {
            bail!(
                "[notify] worker_offline_minutes must be between 1 and {MAX_WORKER_OFFLINE_MINUTES} \
                 (a week), not {}",
                self.worker_offline_minutes
            );
        }
        if self.events.is_empty() {
            bail!(
                "[notify] events is empty; list at least one of {}, or leave webhook_url \
                 empty to turn notifications off",
                EventKind::ALL.map(EventKind::as_str).join(", ")
            );
        }
        let Some(url) = self.webhook_url() else {
            return Ok(());
        };
        // The parser's messages describe the problem, never the input.
        let parsed = reqwest::Url::parse(url)
            .map_err(|err| anyhow::anyhow!("[notify] webhook_url is not a valid URL: {err}"))?;
        if !matches!(parsed.scheme(), "http" | "https") {
            bail!("[notify] webhook_url must be an http:// or https:// URL");
        }
        if parsed.host_str().is_none_or(str::is_empty) {
            bail!("[notify] webhook_url has no host");
        }
        Ok(())
    }
}

/// Something worth telling the operator.
#[derive(Clone, Debug, PartialEq)]
pub enum Event {
    /// A block candidate was submitted.
    BlockFound {
        /// Lane (parent coin key) that mined it.
        lane: String,
        /// Ticker of the chain the block is for.
        coin: String,
        /// Block height.
        height: u64,
        /// Block hash, display hex.
        hash: String,
        /// Worker that found it.
        worker: String,
        /// Whether the node accepted it.
        accepted: bool,
        /// The node's reason when it did not.
        reason: Option<String>,
    },
    /// A worker has had no connection for `minutes`.
    WorkerOffline {
        /// Lane.
        lane: String,
        /// Worker name.
        worker: String,
        /// The configured grace, in minutes.
        minutes: u64,
    },
    /// A worker reported offline connected again.
    WorkerOnline {
        /// Lane.
        lane: String,
        /// Worker name.
        worker: String,
    },
    /// A node has been unreachable, or its template withdrawn, past the debounce.
    NodeDown {
        /// Lane.
        lane: String,
        /// Ticker.
        coin: String,
        /// The most recent RPC failure, as the dashboard shows it.
        error: Option<String>,
    },
    /// A node reported down answers again.
    NodeUp {
        /// Lane.
        lane: String,
        /// Ticker.
        coin: String,
    },
    /// A lane gave up starting.
    LaneFailed {
        /// Lane.
        lane: String,
        /// Why, with its causes.
        error: String,
    },
}

impl Event {
    /// Which kind of event this is.
    pub fn kind(&self) -> EventKind {
        match self {
            Event::BlockFound { .. } => EventKind::BlockFound,
            Event::WorkerOffline { .. } => EventKind::WorkerOffline,
            Event::WorkerOnline { .. } => EventKind::WorkerOnline,
            Event::NodeDown { .. } => EventKind::NodeDown,
            Event::NodeUp { .. } => EventKind::NodeUp,
            Event::LaneFailed { .. } => EventKind::LaneFailed,
        }
    }

    /// One human-readable line.
    pub fn text(&self, pool: &str) -> String {
        let line = match self {
            Event::BlockFound {
                coin,
                height,
                worker,
                accepted,
                reason,
                ..
            } => {
                let verdict = match (accepted, reason) {
                    (true, _) => "accepted by node".to_string(),
                    (false, Some(reason)) => format!("REJECTED by node: {reason}"),
                    (false, None) => "REJECTED by node".to_string(),
                };
                format!("{pool}: {coin} block {height} found by {worker} — {verdict}")
            }
            Event::WorkerOffline {
                lane,
                worker,
                minutes,
            } => format!(
                "{pool}: worker {worker} is offline in lane {lane} (no connection for {minutes} min)"
            ),
            Event::WorkerOnline { lane, worker } => {
                format!("{pool}: worker {worker} is back online in lane {lane}")
            }
            Event::NodeDown { lane, coin, .. } => format!(
                "{pool}: {coin} node is unreachable (lane {lane}) for over {} s",
                NODE_DOWN_AFTER.as_secs()
            ),
            Event::NodeUp { lane, coin } => {
                format!("{pool}: {coin} node is reachable again (lane {lane})")
            }
            Event::LaneFailed { lane, error } => {
                format!("{pool}: lane {lane} failed to start: {error}")
            }
        };
        truncate(line, MAX_TEXT_CHARS)
    }

    /// The JSON body: `event`, `pool`, `time`, `text` and `content` (the same line, for
    /// Slack/Mattermost and Discord), then the event's own fields.
    pub fn body(&self, pool: &str, time: u64) -> Value {
        let text = self.text(pool);
        let mut body = json!({
            "event": self.kind().as_str(),
            "pool": pool,
            "time": time,
            "text": text,
            "content": text,
        });
        let fields = match self {
            Event::BlockFound {
                lane,
                coin,
                height,
                hash,
                worker,
                accepted,
                reason,
            } => json!({
                "lane": lane, "coin": coin, "height": height, "hash": hash,
                "worker": worker, "accepted": accepted, "reason": reason,
            }),
            Event::WorkerOffline {
                lane,
                worker,
                minutes,
            } => json!({ "lane": lane, "worker": worker, "offline_minutes": minutes }),
            Event::WorkerOnline { lane, worker } => json!({ "lane": lane, "worker": worker }),
            Event::NodeDown { lane, coin, error } => {
                json!({ "lane": lane, "coin": coin, "error": error })
            }
            Event::NodeUp { lane, coin } => json!({ "lane": lane, "coin": coin }),
            Event::LaneFailed { lane, error } => json!({ "lane": lane, "error": error }),
        };
        if let (Value::Object(body), Value::Object(fields)) = (&mut body, fields) {
            body.extend(fields);
        }
        body
    }
}

fn truncate(mut s: String, max_chars: usize) -> String {
    if let Some((cut, _)) = s.char_indices().nth(max_chars) {
        s.truncate(cut);
        s.push('…');
    }
    s
}

/// A queued event and when it happened.
struct Queued {
    event: Event,
    time: u64,
}

/// Hands events to the sender task. Cheap to clone; never blocks.
#[derive(Clone)]
pub struct Notifier {
    tx: Option<mpsc::Sender<Queued>>,
    /// Bit per [`EventKind`] that is wanted.
    wanted: u8,
}

impl Notifier {
    /// A notifier that drops everything: notifications are off.
    pub fn disabled() -> Self {
        Self {
            tx: None,
            wanted: 0,
        }
    }

    fn with_queue(events: &[EventKind], capacity: usize) -> (Self, mpsc::Receiver<Queued>) {
        let (tx, rx) = mpsc::channel(capacity);
        let wanted = events.iter().fold(0, |m, k| m | k.bit());
        (
            Self {
                tx: Some(tx),
                wanted,
            },
            rx,
        )
    }

    /// Whether events of `kind` are sent at all.
    pub fn wants(&self, kind: EventKind) -> bool {
        self.tx.is_some() && self.wanted & kind.bit() != 0
    }

    /// Queue `event` if its kind is wanted. A full queue drops it with a warning.
    pub fn notify(&self, event: Event) {
        let kind = event.kind();
        let Some(tx) = self.tx.as_ref().filter(|_| self.wants(kind)) else {
            return;
        };
        match tx.try_send(Queued {
            event,
            time: now_unix(),
        }) {
            Ok(()) => {}
            Err(mpsc::error::TrySendError::Full(_)) => {
                tracing::warn!(
                    event = kind.as_str(),
                    "notification queue full; notification dropped"
                );
            }
            Err(mpsc::error::TrySendError::Closed(_)) => {
                tracing::debug!(
                    event = kind.as_str(),
                    "notification sender stopped; notification dropped"
                );
            }
        }
    }
}

/// Current pool name, read when each notification is sent so a rename on the Settings
/// page shows up.
pub type PoolName = Arc<dyn Fn() -> String + Send + Sync>;

/// Start notifications as configured: the sender task and, if any worker, node, or lane
/// event is wanted, the snapshot watcher. Returns a disabled notifier, and starts
/// nothing, when no webhook URL is set. Both tasks stop with `shutdown`.
pub fn start(
    config: &NotifyConfig,
    pool_name: PoolName,
    snapshots: watch::Receiver<PoolSnapshot>,
    shutdown: CancellationToken,
) -> anyhow::Result<Notifier> {
    let Some(url) = config.webhook_url() else {
        tracing::debug!("webhook notifications off");
        return Ok(Notifier::disabled());
    };
    let client = http_client()?;
    let (notifier, rx) = Notifier::with_queue(&config.events, QUEUE_CAPACITY);
    let sender = Sender {
        client,
        url: url.to_string(),
        pool_name,
        delays: RETRY_DELAYS.to_vec(),
    };
    tokio::spawn(sender.run(rx, shutdown.clone()));
    let watched = [
        EventKind::WorkerOffline,
        EventKind::WorkerOnline,
        EventKind::NodeDown,
        EventKind::NodeUp,
        EventKind::LaneFailed,
    ];
    if watched.iter().any(|&k| notifier.wants(k)) {
        let watcher = Watcher::new(config.offline_after(), NODE_DOWN_AFTER);
        tokio::spawn(watch_snapshots(
            snapshots,
            watcher,
            notifier.clone(),
            shutdown,
        ));
    }
    tracing::info!(
        events = ?config.events.iter().map(|k| k.as_str()).collect::<Vec<_>>(),
        worker_offline_minutes = config.worker_offline_minutes,
        "webhook notifications on"
    );
    Ok(notifier)
}

/// The HTTP client, with the same TLS setup as the node RPC client.
fn http_client() -> anyhow::Result<reqwest::Client> {
    // reqwest is built without a TLS provider; ring is the process-wide choice.
    // Installing twice is harmless.
    let _ = rustls::crypto::ring::default_provider().install_default();
    reqwest::Client::builder()
        .timeout(REQUEST_TIMEOUT)
        .build()
        .map_err(|err| anyhow::anyhow!("building the webhook HTTP client: {}", err.without_url()))
}

/// The task that POSTs queued events.
struct Sender {
    client: reqwest::Client,
    url: String,
    pool_name: PoolName,
    /// Pauses between attempts; one attempt more than there are pauses.
    delays: Vec<Duration>,
}

impl Sender {
    async fn run(self, mut rx: mpsc::Receiver<Queued>, shutdown: CancellationToken) {
        loop {
            let queued = tokio::select! {
                next = rx.recv() => match next {
                    Some(q) => q,
                    None => return,
                },
                _ = shutdown.cancelled() => return,
            };
            tokio::select! {
                _ = self.deliver(&queued.event, queued.time) => {}
                _ = shutdown.cancelled() => return,
            }
        }
    }

    /// POST one event, retrying failures. Returns whether it was delivered. Failures are
    /// logged with the event name and the error, never the URL.
    async fn deliver(&self, event: &Event, time: u64) -> bool {
        let kind = event.kind().as_str();
        let body = event.body(&(self.pool_name)(), time);
        let attempts = self.delays.len() + 1;
        for attempt in 1..=attempts {
            let error = match self.client.post(&self.url).json(&body).send().await {
                Ok(resp) if resp.status().is_success() => {
                    tracing::debug!(event = kind, attempt, "notification delivered");
                    return true;
                }
                Ok(resp) => format!("HTTP {}", resp.status()),
                Err(err) => error_chain(err),
            };
            match self.delays.get(attempt - 1) {
                Some(delay) => {
                    tracing::debug!(event = kind, attempt, %error, "webhook delivery failed; retrying");
                    tokio::time::sleep(*delay).await;
                }
                None => tracing::warn!(
                    event = kind,
                    attempts,
                    %error,
                    "webhook delivery failed; notification dropped"
                ),
            }
        }
        false
    }
}

/// A reqwest error and its causes, without the URL (which carries the webhook's token).
fn error_chain(err: reqwest::Error) -> String {
    let err = err.without_url();
    let mut text = err.to_string();
    let mut source = std::error::Error::source(&err);
    while let Some(cause) = source {
        text.push_str(": ");
        text.push_str(&cause.to_string());
        source = cause.source();
    }
    text
}

/// Feed every published snapshot to `watcher` and queue what it finds.
async fn watch_snapshots(
    mut snapshots: watch::Receiver<PoolSnapshot>,
    mut watcher: Watcher,
    notifier: Notifier,
    shutdown: CancellationToken,
) {
    loop {
        tokio::select! {
            changed = snapshots.changed() => {
                if changed.is_err() {
                    return;
                }
                let snapshot = snapshots.borrow_and_update();
                watcher.observe_snapshot(&snapshot, |event| notifier.notify(event));
            }
            _ = shutdown.cancelled() => return,
        }
    }
}

/// One worker as the watcher sees it.
#[derive(Clone, Copy, Debug)]
pub struct WorkerSeen<'a> {
    /// Worker name.
    pub name: &'a str,
    /// Whether it has a live connection.
    pub connected: bool,
}

/// One node as the watcher sees it.
#[derive(Clone, Copy, Debug)]
pub struct NodeSeen<'a> {
    /// Ticker.
    pub coin: &'a str,
    /// Connected and serving a template (not withdrawn).
    pub reachable: bool,
    /// Most recent RPC failure.
    pub error: Option<&'a str>,
}

/// One lane as the watcher sees it.
#[derive(Clone, Copy, Debug)]
pub struct LaneSeen<'a> {
    /// Lane key.
    pub key: &'a str,
    /// Why the lane gave up starting, once it has.
    pub failed: Option<&'a str>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum WorkerState {
    Connected,
    Disconnected { since: u64 },
    ReportedOffline,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum NodeState {
    Reachable,
    Unreachable { since: u64 },
    ReportedDown,
}

/// Turns successive snapshots into transitions, each reported once. Time is the
/// snapshot's own `now`, so tests drive it directly.
pub struct Watcher {
    offline_after: u64,
    node_down_after: u64,
    /// Workers seen connected in this run, by lane then name. A worker never seen
    /// connected is not tracked, so a restart says nothing about idle workers.
    workers: HashMap<String, HashMap<String, WorkerState>>,
    /// Nodes by lane then ticker.
    nodes: HashMap<String, HashMap<String, NodeState>>,
    /// Lanes already reported failed.
    failed: HashSet<String>,
}

impl Watcher {
    /// A watcher with the given worker grace and node debounce.
    pub fn new(offline_after: Duration, node_down_after: Duration) -> Self {
        Self {
            offline_after: offline_after.as_secs(),
            node_down_after: node_down_after.as_secs(),
            workers: HashMap::new(),
            nodes: HashMap::new(),
            failed: HashSet::new(),
        }
    }

    /// Observe a published snapshot.
    pub fn observe_snapshot(&mut self, snapshot: &PoolSnapshot, mut emit: impl FnMut(Event)) {
        for lane in &snapshot.lanes {
            let failed = (lane.state == LaneState::Failed)
                .then(|| lane.error.as_deref().unwrap_or("unknown error"));
            self.observe_lane(
                snapshot.now,
                LaneSeen {
                    key: &lane.key,
                    failed,
                },
                lane.workers.iter().map(|w| WorkerSeen {
                    name: &w.name,
                    connected: w.connections > 0,
                }),
                lane.coins.iter().map(|c| NodeSeen {
                    coin: &c.symbol,
                    reachable: c.node.connected && !c.node.stale,
                    error: c.node.last_error.as_deref(),
                }),
                &mut emit,
            );
        }
    }

    /// Observe one lane at `now` (unix seconds).
    pub fn observe_lane<'a>(
        &mut self,
        now: u64,
        lane: LaneSeen<'_>,
        workers: impl Iterator<Item = WorkerSeen<'a>>,
        nodes: impl Iterator<Item = NodeSeen<'a>>,
        emit: &mut impl FnMut(Event),
    ) {
        if let Some(error) = lane.failed {
            if !self.failed.contains(lane.key) {
                self.failed.insert(lane.key.to_string());
                emit(Event::LaneFailed {
                    lane: lane.key.to_string(),
                    error: error.to_string(),
                });
            }
        }
        self.observe_workers(now, lane.key, workers, emit);
        self.observe_nodes(now, lane.key, nodes, emit);
    }

    fn observe_workers<'a>(
        &mut self,
        now: u64,
        lane: &str,
        workers: impl Iterator<Item = WorkerSeen<'a>>,
        emit: &mut impl FnMut(Event),
    ) {
        let minutes = self.offline_after / 60;
        let mut present: HashSet<&str> = HashSet::new();
        for w in workers {
            present.insert(w.name);
            let tracked = self.workers.get_mut(lane).and_then(|m| m.get_mut(w.name));
            let Some(state) = tracked else {
                if w.connected {
                    self.workers
                        .entry(lane.to_string())
                        .or_default()
                        .insert(w.name.to_string(), WorkerState::Connected);
                }
                continue;
            };
            *state = match (*state, w.connected) {
                (WorkerState::Connected, true) => WorkerState::Connected,
                (WorkerState::Connected, false) => WorkerState::Disconnected { since: now },
                (WorkerState::Disconnected { .. }, true) => WorkerState::Connected,
                (WorkerState::Disconnected { since }, false) => {
                    if now.saturating_sub(since) >= self.offline_after {
                        emit(Event::WorkerOffline {
                            lane: lane.to_string(),
                            worker: w.name.to_string(),
                            minutes,
                        });
                        WorkerState::ReportedOffline
                    } else {
                        WorkerState::Disconnected { since }
                    }
                }
                (WorkerState::ReportedOffline, true) => {
                    emit(Event::WorkerOnline {
                        lane: lane.to_string(),
                        worker: w.name.to_string(),
                    });
                    WorkerState::Connected
                }
                (WorkerState::ReportedOffline, false) => WorkerState::ReportedOffline,
            };
        }
        // A worker no longer listed was removed by the operator (only possible while it
        // has no connection): forget it rather than report it.
        if let Some(tracked) = self.workers.get_mut(lane) {
            tracked.retain(|name, _| present.contains(name.as_str()));
        }
    }

    fn observe_nodes<'a>(
        &mut self,
        now: u64,
        lane: &str,
        nodes: impl Iterator<Item = NodeSeen<'a>>,
        emit: &mut impl FnMut(Event),
    ) {
        for n in nodes {
            let tracked = self.nodes.get_mut(lane).and_then(|m| m.get_mut(n.coin));
            let Some(state) = tracked else {
                // A coin is listed once it has had a template in this run, so its node
                // has been reachable; an unreachable first sight starts the debounce.
                let state = if n.reachable {
                    NodeState::Reachable
                } else {
                    NodeState::Unreachable { since: now }
                };
                self.nodes
                    .entry(lane.to_string())
                    .or_default()
                    .insert(n.coin.to_string(), state);
                continue;
            };
            *state = match (*state, n.reachable) {
                (NodeState::Reachable, true) => NodeState::Reachable,
                (NodeState::Reachable, false) => NodeState::Unreachable { since: now },
                (NodeState::Unreachable { .. }, true) => NodeState::Reachable,
                (NodeState::Unreachable { since }, false) => {
                    if now.saturating_sub(since) >= self.node_down_after {
                        emit(Event::NodeDown {
                            lane: lane.to_string(),
                            coin: n.coin.to_string(),
                            error: n.error.map(str::to_string),
                        });
                        NodeState::ReportedDown
                    } else {
                        NodeState::Unreachable { since }
                    }
                }
                (NodeState::ReportedDown, true) => {
                    emit(Event::NodeUp {
                        lane: lane.to_string(),
                        coin: n.coin.to_string(),
                    });
                    NodeState::Reachable
                }
                (NodeState::ReportedDown, false) => NodeState::ReportedDown,
            };
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    const T0: u64 = 1_700_000_000;

    fn watcher() -> Watcher {
        Watcher::new(Duration::from_secs(300), Duration::from_secs(30))
    }

    /// Observe lane `ltc` at `now` with the given workers and no nodes.
    fn workers(w: &mut Watcher, now: u64, seen: &[(&str, bool)]) -> Vec<Event> {
        let mut out = Vec::new();
        w.observe_lane(
            now,
            LaneSeen {
                key: "ltc",
                failed: None,
            },
            seen.iter()
                .map(|&(name, connected)| WorkerSeen { name, connected }),
            std::iter::empty(),
            &mut |e| out.push(e),
        );
        out
    }

    /// Observe lane `ltc` at `now` with one DOGE node.
    fn node(w: &mut Watcher, now: u64, reachable: bool) -> Vec<Event> {
        let mut out = Vec::new();
        w.observe_lane(
            now,
            LaneSeen {
                key: "ltc",
                failed: None,
            },
            std::iter::empty(),
            std::iter::once(NodeSeen {
                coin: "DOGE",
                reachable,
                error: (!reachable).then_some("connection refused"),
            }),
            &mut |e| out.push(e),
        );
        out
    }

    fn offline(worker: &str) -> Event {
        Event::WorkerOffline {
            lane: "ltc".into(),
            worker: worker.into(),
            minutes: 5,
        }
    }

    #[test]
    fn no_offline_at_startup_for_workers_never_seen_connected() {
        let mut w = watcher();
        for t in [0, 300, 600, 3_600] {
            assert!(workers(&mut w, T0 + t, &[("idle", false)]).is_empty());
        }
    }

    #[test]
    fn worker_offline_waits_for_the_grace_and_fires_once() {
        let mut w = watcher();
        assert!(workers(&mut w, T0, &[("rig", true)]).is_empty());
        assert!(workers(&mut w, T0 + 10, &[("rig", false)]).is_empty());
        assert!(workers(&mut w, T0 + 309, &[("rig", false)]).is_empty());
        assert_eq!(
            workers(&mut w, T0 + 310, &[("rig", false)]),
            vec![offline("rig")]
        );
        for t in [312, 600, 6_000] {
            assert!(
                workers(&mut w, T0 + t, &[("rig", false)]).is_empty(),
                "once"
            );
        }
        assert_eq!(
            workers(&mut w, T0 + 6_002, &[("rig", true)]),
            vec![Event::WorkerOnline {
                lane: "ltc".into(),
                worker: "rig".into()
            }]
        );
        assert!(workers(&mut w, T0 + 6_004, &[("rig", true)]).is_empty());
    }

    #[test]
    fn a_reconnect_within_the_grace_says_nothing_and_restarts_it() {
        let mut w = watcher();
        workers(&mut w, T0, &[("rig", true)]);
        assert!(workers(&mut w, T0 + 2, &[("rig", false)]).is_empty());
        assert!(
            workers(&mut w, T0 + 200, &[("rig", true)]).is_empty(),
            "no online without an offline"
        );
        assert!(workers(&mut w, T0 + 400, &[("rig", false)]).is_empty());
        assert!(
            workers(&mut w, T0 + 600, &[("rig", false)]).is_empty(),
            "the grace restarted at T0+400"
        );
        assert_eq!(
            workers(&mut w, T0 + 700, &[("rig", false)]),
            vec![offline("rig")]
        );
    }

    #[test]
    fn a_removed_worker_is_forgotten_not_reported() {
        let mut w = watcher();
        workers(&mut w, T0, &[("rig", true), ("other", true)]);
        workers(&mut w, T0 + 2, &[("rig", false), ("other", true)]);
        // The operator removed `rig`.
        assert!(workers(&mut w, T0 + 400, &[("other", true)]).is_empty());
        // Should it come back idle, it is a new, never-connected worker.
        assert!(workers(&mut w, T0 + 800, &[("rig", false), ("other", true)]).is_empty());
    }

    #[test]
    fn same_worker_name_in_two_lanes_is_tracked_per_lane() {
        let mut w = watcher();
        let mut out = Vec::new();
        let mut seen = |w: &mut Watcher, now, lane: &str, connected| {
            w.observe_lane(
                now,
                LaneSeen {
                    key: lane,
                    failed: None,
                },
                std::iter::once(WorkerSeen {
                    name: "rig",
                    connected,
                }),
                std::iter::empty(),
                &mut |e| out.push(e),
            )
        };
        seen(&mut w, T0, "ltc", true);
        seen(&mut w, T0, "btc", true);
        seen(&mut w, T0 + 2, "btc", false);
        seen(&mut w, T0 + 400, "ltc", true);
        seen(&mut w, T0 + 400, "btc", false);
        assert_eq!(
            out,
            vec![Event::WorkerOffline {
                lane: "btc".into(),
                worker: "rig".into(),
                minutes: 5
            }]
        );
    }

    #[test]
    fn node_down_is_debounced_and_reported_once() {
        let mut w = watcher();
        assert!(node(&mut w, T0, true).is_empty());
        // One failed poll, then fine again: nothing.
        assert!(node(&mut w, T0 + 2, false).is_empty());
        assert!(node(&mut w, T0 + 4, true).is_empty());
        // Unreachable for good.
        assert!(node(&mut w, T0 + 10, false).is_empty());
        assert!(node(&mut w, T0 + 38, false).is_empty());
        assert_eq!(
            node(&mut w, T0 + 40, false),
            vec![Event::NodeDown {
                lane: "ltc".into(),
                coin: "DOGE".into(),
                error: Some("connection refused".into())
            }]
        );
        for t in [42, 100, 1_000] {
            assert!(node(&mut w, T0 + t, false).is_empty(), "once");
        }
        assert_eq!(
            node(&mut w, T0 + 1_002, true),
            vec![Event::NodeUp {
                lane: "ltc".into(),
                coin: "DOGE".into()
            }]
        );
        assert!(node(&mut w, T0 + 1_004, true).is_empty());
        assert!(
            node(&mut w, T0 + 1_006, false).is_empty(),
            "debounced again"
        );
    }

    #[test]
    fn a_node_unreachable_when_first_listed_still_waits_the_debounce() {
        let mut w = watcher();
        assert!(node(&mut w, T0, false).is_empty());
        assert!(node(&mut w, T0 + 20, false).is_empty());
        assert_eq!(node(&mut w, T0 + 30, false).len(), 1);
    }

    #[test]
    fn lane_failed_fires_once_from_the_snapshot() {
        let mut w = watcher();
        let mut snapshot = PoolSnapshot {
            now: T0,
            lanes: vec![
                alamo_web::LaneStatus {
                    key: "btc".into(),
                    state: LaneState::Connecting,
                    ..Default::default()
                },
                alamo_web::LaneStatus {
                    key: "ltc".into(),
                    state: LaneState::Running,
                    ..Default::default()
                },
            ],
            ..Default::default()
        };
        let mut out = Vec::new();
        w.observe_snapshot(&snapshot, |e| out.push(e));
        assert!(out.is_empty());
        snapshot.lanes[0].state = LaneState::Failed;
        snapshot.lanes[0].error = Some("lane 'btc': wrong network".into());
        for t in 0..3 {
            snapshot.now = T0 + 2 * t;
            w.observe_snapshot(&snapshot, |e| out.push(e));
        }
        assert_eq!(
            out,
            vec![Event::LaneFailed {
                lane: "btc".into(),
                error: "lane 'btc': wrong network".into()
            }]
        );
    }

    #[test]
    fn payload_carries_text_content_and_fields() {
        let event = Event::BlockFound {
            lane: "btc".into(),
            coin: "BTC".into(),
            height: 977_562,
            hash: "00ab".into(),
            worker: "rig1".into(),
            accepted: true,
            reason: None,
        };
        let body = event.body("Alamo Solo Pool", T0);
        let text = "Alamo Solo Pool: BTC block 977562 found by rig1 — accepted by node";
        assert_eq!(body["event"], "block_found");
        assert_eq!(body["pool"], "Alamo Solo Pool");
        assert_eq!(body["time"], T0);
        assert_eq!(body["text"], text);
        assert_eq!(body["content"], text);
        assert_eq!(body["lane"], "btc");
        assert_eq!(body["height"], 977_562);
        assert_eq!(body["hash"], "00ab");
        assert_eq!(body["accepted"], true);
        assert!(body["reason"].is_null());

        assert!(block().text("P").ends_with("— REJECTED by node: high-hash"));

        let long = Event::LaneFailed {
            lane: "btc".into(),
            error: "x".repeat(5_000),
        };
        assert!(long.text("P").chars().count() <= MAX_TEXT_CHARS + 1);
    }

    #[test]
    fn notifier_filters_kinds_and_drops_on_overflow() {
        let (n, mut rx) = Notifier::with_queue(&[EventKind::NodeDown], 1);
        assert!(n.wants(EventKind::NodeDown) && !n.wants(EventKind::BlockFound));
        n.notify(Event::NodeUp {
            lane: "l".into(),
            coin: "C".into(),
        });
        assert!(rx.try_recv().is_err(), "unwanted kind is not queued");
        let down = Event::NodeDown {
            lane: "l".into(),
            coin: "C".into(),
            error: None,
        };
        n.notify(down.clone());
        n.notify(down.clone()); // full: dropped, does not block
        assert_eq!(rx.try_recv().unwrap().event, down);
        assert!(rx.try_recv().is_err());
        Notifier::disabled().notify(down);
    }

    #[test]
    fn config_validation() {
        let parse = |text: &str| -> anyhow::Result<NotifyConfig> {
            let cfg: NotifyConfig = toml::from_str(text)?;
            cfg.validate()?;
            Ok(cfg)
        };
        let cfg = parse("").unwrap();
        assert!(cfg.webhook_url().is_none() && cfg.is_default());
        assert_eq!(cfg.events, EventKind::ALL.to_vec());
        assert_eq!(cfg.worker_offline_minutes, 5);
        assert!(parse("webhook_url = \"\"").unwrap().webhook_url().is_none());

        let cfg = parse(
            "webhook_url = \"https://discord.com/api/webhooks/1/tok\"\n\
             events = [\"block_found\", \"node_down\"]\nworker_offline_minutes = 15",
        )
        .unwrap();
        assert!(cfg.webhook_url().is_some());
        assert_eq!(cfg.offline_after(), Duration::from_secs(900));
        parse("webhook_url = \"http://10.0.0.5:8080/hook\"").unwrap();

        for (text, expected) in [
            (
                "webhook_url = \"ftp://host/secret-token\"",
                "http:// or https://",
            ),
            (
                "webhook_url = \"file:///etc/secret-token\"",
                "http:// or https://",
            ),
            ("webhook_url = \"secret-token\"", "not a valid URL"),
            ("worker_offline_minutes = 0", "between 1 and"),
            ("worker_offline_minutes = 20000", "between 1 and"),
            ("events = []", "events is empty"),
            ("events = [\"block_lost\"]", "unknown variant"),
            ("webhok_url = \"x\"", "unknown field"),
        ] {
            let e = format!("{:#}", parse(text).unwrap_err());
            assert!(e.contains(expected), "{text}: {e}");
            assert!(!e.contains("secret-token"), "{e}");
        }
    }

    #[test]
    fn debug_and_redaction_hide_the_url() {
        let mut cfg = NotifyConfig {
            webhook_url: Some("https://ntfy.sh/secret-topic".into()),
            ..Default::default()
        };
        assert!(!format!("{cfg:?}").contains("secret-topic"));
        cfg.redact();
        assert_eq!(cfg.webhook_url.as_deref(), Some(REDACTED));
        let mut off = NotifyConfig::default();
        off.redact();
        assert!(off.webhook_url.is_none());
    }

    /// One HTTP request as the test server saw it.
    #[derive(Debug)]
    struct Seen {
        method: String,
        path: String,
        content_type: String,
        body: Value,
    }

    /// A server that answers the given statuses in order, one per connection, and
    /// reports each request.
    async fn server(statuses: Vec<u16>) -> (String, mpsc::UnboundedReceiver<Seen>) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let (tx, rx) = mpsc::unbounded_channel();
        tokio::spawn(async move {
            for status in statuses {
                let (mut sock, _) = listener.accept().await.unwrap();
                let mut buf = Vec::new();
                let mut chunk = [0u8; 4096];
                let header_end = loop {
                    let n = sock.read(&mut chunk).await.unwrap();
                    assert!(n > 0, "client closed before the headers ended");
                    buf.extend_from_slice(&chunk[..n]);
                    if let Some(i) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
                        break i + 4;
                    }
                };
                let head = String::from_utf8_lossy(&buf[..header_end]).to_string();
                let header = |name: &str| {
                    head.lines()
                        .find_map(|l| {
                            let (k, v) = l.split_once(':')?;
                            k.eq_ignore_ascii_case(name).then(|| v.trim().to_string())
                        })
                        .unwrap_or_default()
                };
                let length: usize = header("content-length").parse().unwrap();
                while buf.len() < header_end + length {
                    let n = sock.read(&mut chunk).await.unwrap();
                    assert!(n > 0);
                    buf.extend_from_slice(&chunk[..n]);
                }
                let mut request_line = head.lines().next().unwrap().split(' ');
                let _ = tx.send(Seen {
                    method: request_line.next().unwrap().to_string(),
                    path: request_line.next().unwrap().to_string(),
                    content_type: header("content-type"),
                    body: serde_json::from_slice(&buf[header_end..header_end + length]).unwrap(),
                });
                let reply = format!(
                    "HTTP/1.1 {status} X\r\ncontent-length: 0\r\nconnection: close\r\n\r\n"
                );
                sock.write_all(reply.as_bytes()).await.unwrap();
                let _ = sock.shutdown().await;
            }
        });
        (format!("http://{addr}"), rx)
    }

    fn sender(url: String) -> Sender {
        Sender {
            client: http_client().unwrap(),
            url,
            pool_name: Arc::new(|| "Test Pool".to_string()),
            delays: vec![Duration::ZERO; 2],
        }
    }

    fn block() -> Event {
        Event::BlockFound {
            lane: "btc".into(),
            coin: "BTC".into(),
            height: 977_562,
            hash: "00ab".into(),
            worker: "rig1".into(),
            accepted: false,
            reason: Some("high-hash".into()),
        }
    }

    #[tokio::test]
    async fn delivery_posts_json_and_retries_after_a_500() {
        let (base, mut seen) = server(vec![500, 204]).await;
        let s = sender(format!("{base}/hooks/secret-token"));
        assert!(s.deliver(&block(), T0).await);
        for _ in 0..2 {
            let req = seen.recv().await.unwrap();
            assert_eq!(req.method, "POST");
            assert_eq!(req.path, "/hooks/secret-token");
            assert_eq!(req.content_type, "application/json");
            assert_eq!(req.body["event"], "block_found");
            assert_eq!(req.body["pool"], "Test Pool");
            assert_eq!(req.body["time"], T0);
            assert_eq!(req.body["text"], req.body["content"]);
            assert_eq!(
                req.body["text"],
                "Test Pool: BTC block 977562 found by rig1 — REJECTED by node: high-hash"
            );
            assert_eq!(req.body["coin"], "BTC");
            assert_eq!(req.body["lane"], "btc");
            assert_eq!(req.body["height"], 977_562);
            assert_eq!(req.body["hash"], "00ab");
            assert_eq!(req.body["worker"], "rig1");
            assert_eq!(req.body["accepted"], false);
            assert_eq!(req.body["reason"], "high-hash");
        }
        assert!(seen.try_recv().is_err(), "no third attempt after a success");
    }

    /// Captures formatted log output.
    #[derive(Clone, Default)]
    struct Captured(Arc<Mutex<Vec<u8>>>);
    impl std::io::Write for Captured {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(buf);
            Ok(buf.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for Captured {
        type Writer = Captured;
        fn make_writer(&'a self) -> Self::Writer {
            self.clone()
        }
    }

    #[tokio::test(flavor = "current_thread")]
    async fn failed_delivery_logs_the_event_but_never_the_url() {
        let logs = Captured::default();
        let subscriber = tracing_subscriber::fmt()
            .with_writer(logs.clone())
            .with_ansi(false)
            .with_max_level(tracing::Level::DEBUG)
            .finish();
        let _guard = tracing::subscriber::set_default(subscriber);

        // A server that always fails.
        let (base, mut seen) = server(vec![500, 503, 500]).await;
        assert!(
            !sender(format!("{base}/hooks/secret-token"))
                .deliver(&block(), T0)
                .await
        );
        for _ in 0..3 {
            seen.recv().await.unwrap();
        }
        // Nothing listening: a transport error, whose text would name the URL.
        assert!(
            !sender("http://127.0.0.1:1/hooks/secret-token".into())
                .deliver(&block(), T0)
                .await
        );

        let text = String::from_utf8(logs.0.lock().unwrap().clone()).unwrap();
        assert_eq!(
            text.matches("webhook delivery failed; notification dropped")
                .count(),
            2,
            "{text}"
        );
        assert!(text.contains("event=\"block_found\"") || text.contains("event=block_found"));
        assert!(text.contains("HTTP 500"), "{text}");
        assert!(!text.contains("secret-token"), "{text}");
        assert!(!text.contains("http://"), "{text}");
    }
}
