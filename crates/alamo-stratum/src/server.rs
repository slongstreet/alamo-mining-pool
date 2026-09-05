//! TCP listener and per-connection tasks.

use crate::config::StratumConfig;
use crate::events::{BlockCandidate, PoolEvent};
use crate::job::EXTRANONCE1_LEN;
use crate::protocol::Request;
use crate::session::{Effects, Outgoing, Session};
use alamo_core::payout::PayoutTable;
use alamo_core::time::now_unix;
use alamo_core::work::WorkTemplate;
use futures::{SinkExt, StreamExt};
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{mpsc, watch};
use tokio_util::codec::{Framed, LinesCodec};
use tokio_util::sync::CancellationToken;

/// Maximum accepted line length. Miners send small messages; anything larger is abuse.
const MAX_LINE_LEN: usize = 16 * 1024;

/// Receives the latest work template; `None` until the first template arrives.
pub type WorkReceiver = watch::Receiver<Option<Arc<WorkTemplate>>>;

/// Errors from the listener.
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
}

/// The stratum server and everything its sessions need.
pub struct StratumServer {
    /// Listener settings.
    pub config: StratumConfig,
    /// Source of work templates.
    pub work: WorkReceiver,
    /// Maps usernames to payout scripts.
    pub payouts: Arc<PayoutTable>,
    /// Where session events go.
    pub events: mpsc::Sender<PoolEvent>,
    /// Where found blocks go.
    pub blocks: mpsc::Sender<BlockCandidate>,
}

/// A bound listener, ready to run.
pub struct Bound {
    server: Arc<StratumServer>,
    listener: TcpListener,
    /// The address actually bound (useful when the config asked for port 0).
    pub local_addr: SocketAddr,
}

impl StratumServer {
    /// Bind the listen address.
    pub async fn bind(self) -> Result<Bound, ServeError> {
        let addr = self.config.listen;
        let listener = TcpListener::bind(addr)
            .await
            .map_err(|source| ServeError::Bind { addr, source })?;
        let local_addr = listener
            .local_addr()
            .map_err(|source| ServeError::Bind { addr, source })?;
        Ok(Bound {
            server: Arc::new(self),
            listener,
            local_addr,
        })
    }
}

impl Bound {
    /// Accept connections until `shutdown` is cancelled.
    pub async fn run(self, shutdown: CancellationToken) {
        tracing::info!(addr = %self.local_addr, "stratum listening");
        let mut next_session: u64 = 1;
        loop {
            tokio::select! {
                accepted = self.listener.accept() => match accepted {
                    Ok((stream, peer)) => {
                        let server = self.server.clone();
                        let child = shutdown.child_token();
                        let id = next_session;
                        next_session += 1;
                        tokio::spawn(async move {
                            if let Err(err) = handle_connection(server, id, stream, peer, child).await {
                                tracing::debug!(%peer, %err, "connection closed with error");
                            }
                        });
                    }
                    Err(err) => {
                        tracing::warn!(%err, "accept failed");
                        tokio::time::sleep(Duration::from_millis(100)).await;
                    }
                },
                _ = shutdown.cancelled() => {
                    tracing::info!("stratum listener shutting down");
                    return;
                }
            }
        }
    }
}

/// Read the current template, dropping the watch guard before any await.
fn latest(work: &mut WorkReceiver) -> Option<Arc<WorkTemplate>> {
    work.borrow_and_update().clone()
}

fn extranonce1(session_id: u64) -> [u8; EXTRANONCE1_LEN] {
    // Unique per connection for the life of the process; the high bits are randomized so
    // two pool instances behind one miner never collide.
    let salt: u32 = rand::random();
    ((session_id as u32) ^ (salt & 0xff00_0000)).to_be_bytes()
}

async fn handle_connection(
    server: Arc<StratumServer>,
    id: u64,
    stream: TcpStream,
    peer: SocketAddr,
    shutdown: CancellationToken,
) -> Result<(), std::io::Error> {
    stream.set_nodelay(true)?;
    let mut framed = Framed::new(stream, LinesCodec::new_with_max_length(MAX_LINE_LEN));
    tracing::info!(session = id, %peer, "miner connected");

    let mut session = Session::new(
        id,
        extranonce1(id),
        server.config.vardiff.clone(),
        server.payouts.clone(),
        Instant::now(),
    );
    let mut work = server.work.clone();
    let mut ticker = tokio::time::interval(Duration::from_secs(5));
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);

    // Deliver whatever template is already current once the miner authorizes; the
    // session handles that itself, so just make sure it has seen the current value.
    if let Some(current) = latest(&mut work) {
        let fx = session.on_work(current);
        apply(&server, &mut framed, fx).await?;
    }

    loop {
        let fx = tokio::select! {
            next = framed.next() => {
                let Some(line) = next else { break };
                let line = match line {
                    Ok(line) => line,
                    Err(err) => {
                        tracing::debug!(session = id, %err, "bad frame");
                        break;
                    }
                };
                match serde_json::from_str::<Request>(&line) {
                    Ok(req) => {
                        tracing::trace!(session = id, method = %req.method, "request");
                        session.handle(req, Instant::now(), now_unix())
                    }
                    Err(err) => {
                        tracing::debug!(session = id, %err, "unparseable request");
                        break;
                    }
                }
            }
            changed = work.changed() => {
                if changed.is_err() {
                    break; // template source is gone
                }
                let Some(current) = latest(&mut work) else { continue };
                session.on_work(current)
            }
            _ = ticker.tick() => session.tick(Instant::now()),
            _ = shutdown.cancelled() => break,
        };
        let close = fx.close;
        apply(&server, &mut framed, fx).await?;
        if close {
            break;
        }
    }

    tracing::info!(session = id, %peer, workers = ?session.workers(), "miner disconnected");
    let _ = server
        .events
        .send(PoolEvent::Disconnected {
            session: id,
            workers: session.workers().to_vec(),
        })
        .await;
    Ok(())
}

async fn apply(
    server: &StratumServer,
    framed: &mut Framed<TcpStream, LinesCodec>,
    fx: Effects,
) -> Result<(), std::io::Error> {
    // Queue every message and flush once so a set_difficulty + notify pair leaves in one write.
    for msg in fx.outgoing {
        let text = match msg {
            Outgoing::Response(r) => serde_json::to_string(&r),
            Outgoing::Notification(n) => serde_json::to_string(&n),
        }
        .expect("stratum messages serialize");
        framed.feed(text).await.map_err(std::io::Error::other)?;
    }
    SinkExt::<String>::flush(framed)
        .await
        .map_err(std::io::Error::other)?;
    for event in fx.events {
        // Never let a slow consumer stall a miner; drop events under backpressure.
        if let Err(err) = server.events.try_send(event) {
            tracing::warn!(%err, "dropping pool event");
        }
    }
    if let Some(block) = fx.block {
        tracing::info!(coin = block.coin, height = block.height, hash = %block.block_hash, worker = %block.worker, "BLOCK FOUND");
        if server.blocks.send(block).await.is_err() {
            tracing::error!("block submitter is gone; block candidate lost");
        }
    }
    Ok(())
}
