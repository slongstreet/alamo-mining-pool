//! Lanes start independently: one lane's unreachable node must not keep another lane's
//! stratum listener closed.
//!
//! Runs the whole pool (`alamo::pool::run`) with two lanes. The LTC lane's node is a stub
//! that answers `getblockchaininfo` (all a lane needs to open its listener) and refuses
//! everything else, so no template ever arrives; the DOGE lane's node is a closed port.
//! The regtest test in `regtest.rs` repeats this against a real node and also checks
//! that a job is handed out.
//!
//! A lane that cannot start fails alone and shows as failed; the daemon exits only when
//! no lane is left running or connecting.

use alamo::settings::LogSetup;
use alamo::Config;
use alamo_core::address::encode_segwit;
use serde_json::{json, Value};
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::{TcpListener, TcpStream};
use tokio_util::sync::CancellationToken;

/// A port nothing listens on right now. Racy in principle; fine for a test.
fn free_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

/// A JSON-RPC node that says it is on `chain` and knows nothing else.
async fn stub_node(chain: &'static str) -> SocketAddr {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        loop {
            let Ok((stream, _)) = listener.accept().await else {
                return;
            };
            tokio::spawn(serve_rpc(stream, chain));
        }
    });
    addr
}

async fn serve_rpc(stream: TcpStream, chain: &'static str) {
    let mut reader = BufReader::new(stream);
    loop {
        let mut length = 0usize;
        loop {
            let mut line = String::new();
            if reader.read_line(&mut line).await.unwrap_or(0) == 0 {
                return;
            }
            let line = line.trim_end();
            if line.is_empty() {
                break;
            }
            if let Some((name, value)) = line.split_once(':') {
                if name.eq_ignore_ascii_case("content-length") {
                    length = value.trim().parse().unwrap_or(0);
                }
            }
        }
        let mut body = vec![0; length];
        if reader.read_exact(&mut body).await.is_err() {
            return;
        }
        let request: Value = serde_json::from_slice(&body).unwrap_or_default();
        let reply = match request["method"].as_str() {
            Some("getblockchaininfo") => json!({
                "result": {
                    "chain": chain,
                    "blocks": 0,
                    "difficulty": 1.0,
                    "bestblockhash": "00".repeat(32),
                },
                "error": null,
                "id": request["id"],
            }),
            _ => json!({
                "result": null,
                "error": {"code": -32601, "message": "Method not found"},
                "id": request["id"],
            }),
        }
        .to_string();
        let response = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{}",
            reply.len(),
            reply
        );
        if reader
            .get_mut()
            .write_all(response.as_bytes())
            .await
            .is_err()
        {
            return;
        }
    }
}

fn log() -> LogSetup {
    LogSetup {
        filter: "info".into(),
        control: Arc::new(|_: &str| Ok(())),
    }
}

fn config(ltc_rpc: &str, ltc_port: u16, doge_port: u16, data_dir: &std::path::Path) -> Config {
    config_with_doge(ltc_rpc, "http://127.0.0.1:1", ltc_port, doge_port, data_dir)
}

/// LTC on `[stratum]` and DOGE as a second parent with its own section. The DOGE
/// fallback address is valid on no network, so a DOGE node that answers fails the lane.
fn config_with_doge(
    ltc_rpc: &str,
    doge_rpc: &str,
    ltc_port: u16,
    doge_port: u16,
    data_dir: &std::path::Path,
) -> Config {
    let ltc_fallback = encode_segwit("rltc", 0, &[0x22; 20]).unwrap();
    Config::parse(&format!(
        r#"
[pool]
name = "lanes test"
data_dir = "{}"
[stratum]
listen = "127.0.0.1:{ltc_port}"
[web]
listen = "127.0.0.1:0"
[coins.ltc]
rpc_url = "{ltc_rpc}"
rpc_user = "u"
rpc_password = "p"
fallback_address = "{ltc_fallback}"
poll_interval_ms = 100
[coins.doge]
rpc_url = "{doge_rpc}"
rpc_user = "u"
rpc_password = "p"
fallback_address = "nUnreachable"
[coins.doge.stratum]
listen = "127.0.0.1:{doge_port}"
"#,
        data_dir.display()
    ))
    .unwrap()
}

#[tokio::test]
async fn a_lane_listens_while_another_lane_waits_for_its_node() {
    let data_dir = std::env::temp_dir().join(format!("alamo-lanes-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&data_dir);
    let node = stub_node("regtest").await;
    let (ltc_port, doge_port) = (free_port(), free_port());
    let config = config(&format!("http://{node}"), ltc_port, doge_port, &data_dir);
    assert_eq!(config.lanes().len(), 2);

    let store = alamo_store::Store::open(&config.database_path())
        .await
        .unwrap();
    let state = alamo_web::AppState::new("lanes test", ltc_port, store.clone());
    let log = log();
    let shutdown = CancellationToken::new();
    let pool = tokio::spawn(alamo::pool::run(
        config,
        store,
        state.clone(),
        log,
        shutdown.clone(),
    ));

    // The LTC lane opens its listener and talks stratum.
    let stream = tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            if let Ok(stream) = TcpStream::connect(("127.0.0.1", ltc_port)).await {
                return stream;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .expect("the LTC lane never opened its listener");
    let (reader, mut writer) = stream.into_split();
    let mut lines = BufReader::new(reader).lines();
    writer
        .write_all(b"{\"id\":1,\"method\":\"mining.subscribe\",\"params\":[\"test/0.1\"]}\n")
        .await
        .unwrap();
    let reply = tokio::time::timeout(Duration::from_secs(5), lines.next_line())
        .await
        .expect("no subscribe reply")
        .unwrap()
        .unwrap();
    let reply: Value = serde_json::from_str(&reply).unwrap();
    assert_eq!(reply["id"], 1);
    assert!(reply["error"].is_null(), "{reply}");

    // The DOGE lane is still retrying its node: no listener, and the pool keeps running.
    assert!(
        TcpStream::connect(("127.0.0.1", doge_port)).await.is_err(),
        "the DOGE lane must not listen before its node answers"
    );
    assert!(!pool.is_finished(), "a down node must not stop the pool");
    assert_eq!(state.primary_lane(), "ltc");
    let doc = state.operator().expect("settings installed").settings();
    assert_eq!(
        doc.coins
            .iter()
            .map(|c| (c.key.as_str(), c.connected))
            .collect::<Vec<_>>(),
        vec![("ltc", true), ("doge", false)],
        "every coin, connected or not"
    );
    let lanes = wait_for_lanes(&state, |l| l[0].state == alamo_web::LaneState::Running).await;
    assert_eq!(lanes[1].state, alamo_web::LaneState::Connecting);
    assert_eq!(lanes[0].stratum_port, ltc_port);
    assert_eq!(lanes[1].stratum_port, doge_port);

    shutdown.cancel();
    tokio::time::timeout(Duration::from_secs(10), pool)
        .await
        .expect("the pool did not stop")
        .unwrap()
        .unwrap();
    let _ = std::fs::remove_dir_all(&data_dir);
}

/// Wait until the published lanes satisfy `ready`, and return them.
async fn wait_for_lanes(
    state: &alamo_web::AppState,
    ready: impl Fn(&[alamo_web::LaneStatus]) -> bool,
) -> Vec<alamo_web::LaneStatus> {
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let lanes = state.snapshot().lanes;
            if !lanes.is_empty() && ready(&lanes) {
                return lanes;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .expect("the lanes never reached the expected states")
}

#[tokio::test]
async fn a_lane_that_cannot_start_fails_alone() {
    let data_dir = std::env::temp_dir().join(format!("alamo-lanes-one-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&data_dir);
    let ltc_node = stub_node("regtest").await;
    // DOGE answers, but its fallback address fits no Dogecoin network.
    let doge_node = stub_node("main").await;
    let (ltc_port, doge_port) = (free_port(), free_port());
    let config = config_with_doge(
        &format!("http://{ltc_node}"),
        &format!("http://{doge_node}"),
        ltc_port,
        doge_port,
        &data_dir,
    );
    let store = alamo_store::Store::open(&config.database_path())
        .await
        .unwrap();
    let state = alamo_web::AppState::new("lanes test", ltc_port, store.clone());
    let shutdown = CancellationToken::new();
    let pool = tokio::spawn(alamo::pool::run(
        config,
        store,
        state.clone(),
        log(),
        shutdown.clone(),
    ));

    let lanes = wait_for_lanes(&state, |l| {
        l[0].state == alamo_web::LaneState::Running && l[1].state == alamo_web::LaneState::Failed
    })
    .await;
    assert_eq!(lanes[0].key, "ltc");
    assert!(lanes[0].error.is_none());
    let error = lanes[1].error.as_deref().unwrap();
    assert!(error.contains("lane 'doge'"), "{error}");
    assert!(
        error.contains("fallback_address is not a valid DOGE address"),
        "{error}"
    );
    assert!(lanes[1].chain.is_none());
    assert!(TcpStream::connect(("127.0.0.1", ltc_port)).await.is_ok());
    assert!(TcpStream::connect(("127.0.0.1", doge_port)).await.is_err());
    assert!(!pool.is_finished(), "the healthy lane keeps the daemon up");
    let doc = state.operator().unwrap().settings();
    assert!(doc.coins.iter().any(|c| c.key == "doge" && !c.connected));

    shutdown.cancel();
    tokio::time::timeout(Duration::from_secs(10), pool)
        .await
        .expect("the pool did not stop")
        .unwrap()
        .unwrap();
    let _ = std::fs::remove_dir_all(&data_dir);
}

#[tokio::test]
async fn the_daemon_exits_when_every_lane_has_failed() {
    let data_dir = std::env::temp_dir().join(format!("alamo-lanes-err-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&data_dir);
    // Mainnet nodes, but the LTC fallback address is a regtest one and the DOGE one fits
    // no network.
    let node = stub_node("main").await;
    let config = config_with_doge(
        &format!("http://{node}"),
        &format!("http://{node}"),
        free_port(),
        free_port(),
        &data_dir,
    );
    let store = alamo_store::Store::open(&config.database_path())
        .await
        .unwrap();
    let state = alamo_web::AppState::new("lanes test", 0, store.clone());
    let err = tokio::time::timeout(
        Duration::from_secs(10),
        alamo::pool::run(config, store, state, log(), CancellationToken::new()),
    )
    .await
    .expect("the pool should stop on its own")
    .unwrap_err();
    let err = format!("{err:#}");
    assert!(err.contains("no lane is left running"), "{err}");
    assert!(err.contains("not a valid"), "{err}");
    let _ = std::fs::remove_dir_all(&data_dir);
}

#[tokio::test]
async fn a_single_lane_with_a_bad_config_stops_the_daemon_as_before() {
    let data_dir = std::env::temp_dir().join(format!("alamo-lanes-single-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&data_dir);
    let node = stub_node("main").await;
    let ltc_fallback = encode_segwit("rltc", 0, &[0x22; 20]).unwrap();
    let config = Config::parse(&format!(
        r#"
[pool]
name = "lanes test"
data_dir = "{}"
[stratum]
listen = "127.0.0.1:{}"
[web]
listen = "127.0.0.1:0"
[coins.ltc]
rpc_url = "http://{node}"
rpc_user = "u"
rpc_password = "p"
fallback_address = "{ltc_fallback}"
"#,
        data_dir.display(),
        free_port()
    ))
    .unwrap();
    let store = alamo_store::Store::open(&config.database_path())
        .await
        .unwrap();
    let state = alamo_web::AppState::new("lanes test", 0, store.clone());
    let err = tokio::time::timeout(
        Duration::from_secs(10),
        alamo::pool::run(config, store, state, log(), CancellationToken::new()),
    )
    .await
    .expect("the pool should stop on its own")
    .unwrap_err();
    let err = format!("{err:#}");
    assert!(err.contains("lane 'ltc'"), "{err}");
    assert!(
        err.contains("fallback_address is not a valid LTC address"),
        "{err}"
    );
    let _ = std::fs::remove_dir_all(&data_dir);
}

/// A Bitcoin node on signet fails the BTC lane at connect with a message that says why,
/// and the LTC lane keeps mining.
#[tokio::test]
async fn a_bitcoin_lane_on_signet_fails_alone() {
    let data_dir = std::env::temp_dir().join(format!("alamo-lanes-signet-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&data_dir);
    let ltc_node = stub_node("regtest").await;
    let btc_node = stub_node("signet").await;
    let (ltc_port, btc_port) = (free_port(), free_port());
    let ltc_fallback = encode_segwit("rltc", 0, &[0x22; 20]).unwrap();
    // Valid on signet (tb), so the only thing wrong is the network itself.
    let btc_fallback = encode_segwit("tb", 1, &[0x33; 32]).unwrap();
    let config = Config::parse(&format!(
        r#"
[pool]
name = "lanes test"
data_dir = "{}"
[stratum]
listen = "127.0.0.1:{ltc_port}"
[web]
listen = "127.0.0.1:0"
[coins.ltc]
rpc_url = "http://{ltc_node}"
rpc_user = "u"
rpc_password = "p"
fallback_address = "{ltc_fallback}"
[coins.btc]
rpc_url = "http://{btc_node}"
rpc_user = "u"
rpc_password = "p"
fallback_address = "{btc_fallback}"
[coins.btc.stratum]
listen = "127.0.0.1:{btc_port}"
"#,
        data_dir.display()
    ))
    .unwrap();
    let store = alamo_store::Store::open(&config.database_path())
        .await
        .unwrap();
    let state = alamo_web::AppState::new("lanes test", ltc_port, store.clone());
    let shutdown = CancellationToken::new();
    let pool = tokio::spawn(alamo::pool::run(
        config,
        store,
        state.clone(),
        log(),
        shutdown.clone(),
    ));

    let lanes = wait_for_lanes(&state, |l| {
        l.iter()
            .any(|l| l.key == "ltc" && l.state == alamo_web::LaneState::Running)
            && l.iter()
                .any(|l| l.key == "btc" && l.state == alamo_web::LaneState::Failed)
    })
    .await;
    let btc = lanes.iter().find(|l| l.key == "btc").unwrap();
    let error = btc.error.as_deref().unwrap();
    assert!(error.contains("lane 'btc'"), "{error}");
    assert!(error.contains("coins.btc node is on signet"), "{error}");
    assert!(error.contains("'signet' rule"), "{error}");
    assert!(TcpStream::connect(("127.0.0.1", ltc_port)).await.is_ok());
    assert!(TcpStream::connect(("127.0.0.1", btc_port)).await.is_err());
    assert!(!pool.is_finished(), "the healthy lane keeps the daemon up");

    shutdown.cancel();
    tokio::time::timeout(Duration::from_secs(10), pool)
        .await
        .expect("the pool did not stop")
        .unwrap()
        .unwrap();
    let _ = std::fs::remove_dir_all(&data_dir);
}
