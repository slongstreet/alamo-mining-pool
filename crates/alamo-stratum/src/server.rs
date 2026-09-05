//! TCP listener and per-connection tasks.

use crate::config::StratumConfig;
use crate::protocol::{Request, Response, StratumError};
use futures::{SinkExt, StreamExt};
use serde_json::Value;
use std::net::SocketAddr;
use tokio::net::{TcpListener, TcpStream};
use tokio_util::codec::{Framed, LinesCodec};
use tokio_util::sync::CancellationToken;

/// Maximum accepted line length. Miners send small messages; anything larger is abuse.
const MAX_LINE_LEN: usize = 16 * 1024;

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

/// Run the stratum listener until `shutdown` is cancelled.
pub async fn serve(config: StratumConfig, shutdown: CancellationToken) -> Result<(), ServeError> {
    let listener = TcpListener::bind(config.listen)
        .await
        .map_err(|source| ServeError::Bind {
            addr: config.listen,
            source,
        })?;
    tracing::info!(addr = %config.listen, "stratum listening");

    loop {
        tokio::select! {
            accepted = listener.accept() => match accepted {
                Ok((stream, peer)) => {
                    let child = shutdown.child_token();
                    tokio::spawn(async move {
                        if let Err(err) = handle_connection(stream, peer, child).await {
                            tracing::debug!(%peer, %err, "connection closed with error");
                        }
                    });
                }
                Err(err) => {
                    tracing::warn!(%err, "accept failed");
                    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
                }
            },
            _ = shutdown.cancelled() => {
                tracing::info!("stratum listener shutting down");
                return Ok(());
            }
        }
    }
}

async fn handle_connection(
    stream: TcpStream,
    peer: SocketAddr,
    shutdown: CancellationToken,
) -> Result<(), std::io::Error> {
    stream.set_nodelay(true)?;
    let mut framed = Framed::new(stream, LinesCodec::new_with_max_length(MAX_LINE_LEN));
    tracing::info!(%peer, "miner connected");

    loop {
        tokio::select! {
            next = framed.next() => {
                let Some(line) = next else { break };
                let line = match line {
                    Ok(line) => line,
                    Err(err) => {
                        tracing::debug!(%peer, %err, "bad frame");
                        break;
                    }
                };
                let response = match serde_json::from_str::<Request>(&line) {
                    Ok(req) => dispatch(&peer, req),
                    Err(err) => {
                        tracing::debug!(%peer, %err, "unparseable request");
                        Some(Response::err(Value::Null, StratumError::other("Parse error")))
                    }
                };
                if let Some(response) = response {
                    let text = serde_json::to_string(&response).expect("response serializes");
                    if framed.send(text).await.is_err() {
                        break;
                    }
                }
            }
            _ = shutdown.cancelled() => break,
        }
    }

    tracing::info!(%peer, "miner disconnected");
    Ok(())
}

/// Route a request. Wave 0 acknowledges nothing; every method is reported as unsupported
/// so that miners disconnect cleanly rather than hang.
fn dispatch(peer: &SocketAddr, req: Request) -> Option<Response> {
    tracing::debug!(%peer, method = %req.method, "request");
    if req.id.is_null() {
        return None;
    }
    Some(Response::err(
        req.id,
        StratumError::unknown_method(&req.method),
    ))
}
