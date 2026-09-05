//! Minimal ZMTP subscriber for a node's `hashblock` notifications.
//!
//! Bitcoin-derived nodes publish over libzmq PUB sockets. Speaking just enough of the wire
//! protocol here (greeting, NULL handshake, subscription, message frames) keeps the daemon
//! free of a C dependency, so the arm64 release build stays a plain `cargo build`.
//! Notifications are best effort by design: template polling stays on as the fallback.

use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::sync::watch;
use tokio_util::sync::CancellationToken;

/// Topic the pool subscribes to.
const TOPIC: &[u8] = b"hashblock";
/// Largest frame accepted. Hashblock messages are tiny; anything larger is the wrong socket.
const MAX_FRAME: usize = 64 * 1024;
/// Time allowed for the TCP connect and the handshake.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
/// Delays between reconnect attempts.
const BACKOFF: [Duration; 5] = [
    Duration::from_secs(1),
    Duration::from_secs(2),
    Duration::from_secs(5),
    Duration::from_secs(10),
    Duration::from_secs(30),
];

/// Errors from the subscriber.
#[derive(Debug, thiserror::Error)]
pub enum ZmqError {
    /// The endpoint is not `tcp://host:port`.
    #[error("unsupported zmq endpoint '{0}': only tcp://host:port is supported")]
    Endpoint(String),
    /// Socket failure.
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    /// Connect or handshake took too long.
    #[error("timed out")]
    Timeout,
    /// The peer did not speak ZMTP 3 with the NULL mechanism.
    #[error("peer is not a ZMTP 3 publisher")]
    Handshake,
    /// A frame exceeded [`MAX_FRAME`].
    #[error("frame too large: {0} bytes")]
    FrameTooLarge(usize),
}

/// What the subscriber reports about its connection.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ZmqStatus {
    /// Whether the subscription is established.
    pub connected: bool,
    /// Number of `hashblock` notifications received since start.
    pub blocks: u64,
    /// The most recent failure, if any.
    pub last_error: Option<String>,
}

/// Turn a `tcp://host:port` endpoint into a socket address string for connecting.
/// Wildcard hosts, as a node's `-zmqpubhashblock=tcp://0.0.0.0:28332` reports them, map to
/// loopback.
pub fn parse_endpoint(endpoint: &str) -> Result<String, ZmqError> {
    let bad = || ZmqError::Endpoint(endpoint.to_string());
    let rest = endpoint.strip_prefix("tcp://").ok_or_else(bad)?;
    let (host, port) = rest.rsplit_once(':').ok_or_else(bad)?;
    let port: u16 = port.parse().map_err(|_| bad())?;
    let host = match host {
        "" | "*" | "0.0.0.0" => "127.0.0.1",
        "[::]" => "[::1]",
        other => other,
    };
    if host.contains('/') || port == 0 {
        return Err(bad());
    }
    Ok(format!("{host}:{port}"))
}

/// Subscribes to one node's `hashblock` topic and reports each notification.
pub struct ZmqSubscriber {
    /// `tcp://host:port` as configured.
    pub endpoint: String,
    /// Coin ticker, for logging.
    pub coin: &'static str,
}

impl ZmqSubscriber {
    /// Run until cancelled, reconnecting with backoff after any failure. Every received
    /// notification bumps `status.blocks`.
    pub async fn run(self, status: watch::Sender<ZmqStatus>, shutdown: CancellationToken) {
        shutdown
            .run_until_cancelled(self.connect_loop(status))
            .await;
    }

    async fn connect_loop(self, status: watch::Sender<ZmqStatus>) {
        let addr = match parse_endpoint(&self.endpoint) {
            Ok(addr) => addr,
            Err(err) => {
                tracing::error!(coin = self.coin, %err, "zmq disabled");
                status.send_modify(|s| s.last_error = Some(err.to_string()));
                return;
            }
        };
        let mut attempt: usize = 0;
        loop {
            match self.session(&addr, &status).await {
                Ok(()) => {}
                Err(err) => {
                    if attempt == 0 {
                        tracing::warn!(coin = self.coin, endpoint = %self.endpoint, %err, "zmq connection failed");
                    } else {
                        tracing::debug!(coin = self.coin, %err, attempt, "zmq connection failed");
                    }
                    status.send_modify(|s| {
                        s.connected = false;
                        s.last_error = Some(err.to_string());
                    });
                }
            }
            let delay = BACKOFF[attempt.min(BACKOFF.len() - 1)];
            attempt += 1;
            tokio::time::sleep(delay).await;
        }
    }

    /// One connection: handshake, subscribe, then relay notifications until it drops.
    async fn session(&self, addr: &str, status: &watch::Sender<ZmqStatus>) -> Result<(), ZmqError> {
        let mut stream = tokio::time::timeout(CONNECT_TIMEOUT, async {
            let mut stream = TcpStream::connect(addr).await?;
            stream.set_nodelay(true)?;
            handshake(&mut stream).await?;
            Ok::<_, ZmqError>(stream)
        })
        .await
        .map_err(|_| ZmqError::Timeout)??;
        tracing::info!(coin = self.coin, endpoint = %self.endpoint, "zmq subscribed to hashblock");
        status.send_modify(|s| {
            s.connected = true;
            s.last_error = None;
        });

        let mut frames: Vec<Vec<u8>> = Vec::with_capacity(3);
        loop {
            let (flags, frame) = read_frame(&mut stream).await?;
            if flags & FLAG_COMMAND != 0 {
                continue; // no commands are expected from a ZMTP 3.0 publisher
            }
            frames.push(frame);
            if flags & FLAG_MORE != 0 {
                continue;
            }
            // A hashblock message is [topic, 32-byte hash, 4-byte sequence].
            if frames.first().is_some_and(|topic| topic == TOPIC) {
                if let Some(hash) = frames.get(1).filter(|h| h.len() == 32) {
                    let mut reversed = *<&[u8; 32]>::try_from(hash.as_slice()).expect("32 bytes");
                    reversed.reverse();
                    tracing::debug!(coin = self.coin, hash = %hex::encode(reversed), "zmq hashblock");
                }
                status.send_modify(|s| s.blocks += 1);
            }
            frames.clear();
        }
    }
}

const FLAG_MORE: u8 = 0x01;
const FLAG_LONG: u8 = 0x02;
const FLAG_COMMAND: u8 = 0x04;

/// ZMTP 3.0 greeting, NULL mechanism, READY exchange, then a `hashblock` subscription.
/// Version 3.0 is announced on purpose: its subscriptions are plain messages starting with
/// `0x01`, which every libzmq 4.x publisher accepts.
async fn handshake(stream: &mut TcpStream) -> Result<(), ZmqError> {
    let mut greeting = [0u8; 64];
    greeting[0] = 0xff;
    greeting[9] = 0x7f;
    greeting[10] = 3; // major
    greeting[11] = 0; // minor
    greeting[12..16].copy_from_slice(b"NULL");
    stream.write_all(&greeting).await?;

    let mut peer = [0u8; 64];
    stream.read_exact(&mut peer).await?;
    if peer[0] != 0xff || peer[9] & 0x01 != 0x01 || peer[10] < 3 || &peer[12..16] != b"NULL" {
        return Err(ZmqError::Handshake);
    }

    let mut ready = Vec::with_capacity(32);
    ready.push(5);
    ready.extend_from_slice(b"READY");
    ready.push(11);
    ready.extend_from_slice(b"Socket-Type");
    ready.extend_from_slice(&3u32.to_be_bytes());
    ready.extend_from_slice(b"SUB");
    write_frame(stream, FLAG_COMMAND, &ready).await?;

    let (flags, frame) = read_frame(stream).await?;
    if flags & FLAG_COMMAND == 0 || !frame.starts_with(b"\x05READY") {
        return Err(ZmqError::Handshake);
    }

    let mut subscribe = Vec::with_capacity(1 + TOPIC.len());
    subscribe.push(1);
    subscribe.extend_from_slice(TOPIC);
    write_frame(stream, 0, &subscribe).await?;
    Ok(())
}

async fn write_frame(stream: &mut TcpStream, flags: u8, body: &[u8]) -> Result<(), ZmqError> {
    let mut out = Vec::with_capacity(body.len() + 9);
    if body.len() > 255 {
        out.push(flags | FLAG_LONG);
        out.extend_from_slice(&(body.len() as u64).to_be_bytes());
    } else {
        out.push(flags);
        out.push(body.len() as u8);
    }
    out.extend_from_slice(body);
    stream.write_all(&out).await?;
    Ok(())
}

async fn read_frame(stream: &mut TcpStream) -> Result<(u8, Vec<u8>), ZmqError> {
    let flags = stream.read_u8().await?;
    let len = if flags & FLAG_LONG != 0 {
        stream.read_u64().await?
    } else {
        u64::from(stream.read_u8().await?)
    };
    let len = usize::try_from(len).map_err(|_| ZmqError::FrameTooLarge(usize::MAX))?;
    if len > MAX_FRAME {
        return Err(ZmqError::FrameTooLarge(len));
    }
    let mut body = vec![0u8; len];
    stream.read_exact(&mut body).await?;
    Ok((flags, body))
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::net::TcpListener;

    #[test]
    fn endpoints_parse_and_wildcards_map_to_loopback() {
        assert_eq!(
            parse_endpoint("tcp://127.0.0.1:28332").unwrap(),
            "127.0.0.1:28332"
        );
        assert_eq!(
            parse_endpoint("tcp://0.0.0.0:28332").unwrap(),
            "127.0.0.1:28332"
        );
        assert_eq!(parse_endpoint("tcp://*:28332").unwrap(), "127.0.0.1:28332");
        assert_eq!(parse_endpoint("tcp://[::1]:28332").unwrap(), "[::1]:28332");
        assert_eq!(
            parse_endpoint("tcp://node.lan:28332").unwrap(),
            "node.lan:28332"
        );
        assert!(parse_endpoint("ipc:///tmp/x").is_err());
        assert!(parse_endpoint("tcp://127.0.0.1").is_err());
        assert!(parse_endpoint("tcp://127.0.0.1:0").is_err());
        assert!(parse_endpoint("tcp://127.0.0.1:99999").is_err());
    }

    /// A publisher that speaks the server side of what the subscriber expects, the way
    /// libzmq does when the peer announces ZMTP 3.0.
    async fn fake_publisher(listener: TcpListener, hashes: Vec<[u8; 32]>) -> Vec<u8> {
        let (mut stream, _) = listener.accept().await.unwrap();
        let mut peer = [0u8; 64];
        stream.read_exact(&mut peer).await.unwrap();
        assert_eq!(peer[0], 0xff);
        assert_eq!(&peer[12..16], b"NULL");
        let mut greeting = [0u8; 64];
        greeting[0] = 0xff;
        greeting[9] = 0x7f;
        greeting[10] = 3;
        greeting[11] = 1;
        greeting[12..16].copy_from_slice(b"NULL");
        stream.write_all(&greeting).await.unwrap();

        let (flags, ready) = read_frame(&mut stream).await.unwrap();
        assert_eq!(flags, FLAG_COMMAND);
        assert!(ready.starts_with(b"\x05READY"));
        assert!(ready.windows(3).any(|w| w == b"SUB"));
        let mut mine = b"\x05READY\x0bSocket-Type\x00\x00\x00\x03PUB".to_vec();
        mine.shrink_to_fit();
        write_frame(&mut stream, FLAG_COMMAND, &mine).await.unwrap();

        let (flags, subscription) = read_frame(&mut stream).await.unwrap();
        assert_eq!(flags, 0);

        for (i, hash) in hashes.iter().enumerate() {
            write_frame(&mut stream, FLAG_MORE, TOPIC).await.unwrap();
            write_frame(&mut stream, FLAG_MORE, hash).await.unwrap();
            write_frame(&mut stream, 0, &(i as u32).to_le_bytes())
                .await
                .unwrap();
        }
        // Something on another topic must not count.
        write_frame(&mut stream, FLAG_MORE, b"hashtx")
            .await
            .unwrap();
        write_frame(&mut stream, 0, &[0u8; 32]).await.unwrap();
        // Keep the connection open until the test is done reading.
        let _ = stream.read_u8().await;
        subscription
    }

    #[tokio::test]
    async fn subscribes_and_counts_hashblock_notifications() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let publisher = tokio::spawn(fake_publisher(listener, vec![[0x11; 32], [0x22; 32]]));

        let (status_tx, mut status_rx) = watch::channel(ZmqStatus::default());
        let shutdown = CancellationToken::new();
        let subscriber = ZmqSubscriber {
            endpoint: format!("tcp://{addr}"),
            coin: "LTC",
        };
        tokio::spawn(subscriber.run(status_tx, shutdown.child_token()));

        let seen = tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                status_rx.changed().await.unwrap();
                let s = status_rx.borrow_and_update().clone();
                if s.blocks == 2 {
                    break s;
                }
            }
        })
        .await
        .expect("two notifications");
        assert!(seen.connected);
        assert_eq!(seen.last_error, None);
        // Give the off-topic message time to arrive; it must not bump the count.
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert_eq!(status_rx.borrow().blocks, 2);

        shutdown.cancel();
        let subscription = publisher.await.unwrap();
        assert_eq!(subscription, b"\x01hashblock");
    }

    #[tokio::test]
    async fn reports_failure_when_nothing_listens() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        drop(listener);
        let (status_tx, mut status_rx) = watch::channel(ZmqStatus::default());
        let shutdown = CancellationToken::new();
        tokio::spawn(
            ZmqSubscriber {
                endpoint: format!("tcp://{addr}"),
                coin: "LTC",
            }
            .run(status_tx, shutdown.child_token()),
        );
        tokio::time::timeout(Duration::from_secs(5), status_rx.changed())
            .await
            .unwrap()
            .unwrap();
        let s = status_rx.borrow().clone();
        assert!(!s.connected);
        assert!(s.last_error.is_some());
        shutdown.cancel();
    }
}
