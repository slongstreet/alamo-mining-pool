//! End-to-end test against a Litecoin regtest node.
//!
//! Skipped unless `ALAMO_REGTEST_RPC` is set, e.g.
//! `ALAMO_REGTEST_RPC=http://alamo:alamo@127.0.0.1:19443`.
//!
//! Starts the template source and stratum server in-process, connects a miniature stratum
//! client that mines a share with scrypt on the CPU, and checks that the resulting block is
//! accepted by the node and pays the address the client authorized with.

use alamo::pool::{submit_candidate, SubmitOutcome};
use alamo_coins::{Chain, Coin, CoinPayouts, Litecoin, RpcClient, TemplateSource};
use alamo_core::address::{encode_segwit, payout_script};
use alamo_core::hash::{from_display_hex, sha256d, to_display_hex};
use alamo_core::header::BlockHeader;
use alamo_core::merkle::root_from_branch;
use alamo_core::target::Target;
use alamo_core::Algorithm;
use alamo_store::Store;
use alamo_stratum::job::prevhash_from_stratum;
use alamo_stratum::{PoolEvent, StratumConfig, StratumServer, VardiffConfig};
use serde_json::{json, Value};
use std::sync::Arc;
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpStream;
use tokio::sync::{mpsc, watch};
use tokio_util::sync::CancellationToken;

const SHARE_DIFFICULTY: f64 = 0.0000005;

#[tokio::test]
async fn mines_a_block_on_regtest() {
    let Ok(url) = std::env::var("ALAMO_REGTEST_RPC") else {
        eprintln!("ALAMO_REGTEST_RPC not set; skipping regtest integration test");
        return;
    };
    let rpc = RpcClient::from_url_with_userinfo(&url)
        .expect("ALAMO_REGTEST_RPC must look like http://user:pass@host:port");
    let info = rpc.get_blockchain_info().await.expect("node reachable");
    assert_eq!(info.chain, "regtest", "this test only runs against regtest");

    let coin: Arc<dyn Coin> = Arc::new(Litecoin);
    let params = coin.address_params(Chain::Regtest);
    let address = encode_segwit("rltc", 0, &[0x11; 20]).unwrap();
    let script = payout_script(&address, &params).unwrap();
    let fallback = encode_segwit("rltc", 0, &[0x22; 20]).unwrap();
    let payouts = Arc::new(CoinPayouts::new(params, &fallback).unwrap());

    let shutdown = CancellationToken::new();
    let (work_tx, work_rx) = watch::channel(None);
    tokio::spawn(
        TemplateSource {
            rpc: rpc.clone(),
            coin: coin.clone(),
            coinbase_tag: b"/alamo-test/".to_vec(),
            poll_interval: Duration::from_millis(200),
            refresh_interval: Duration::from_secs(30),
        }
        .run(work_tx, shutdown.child_token()),
    );

    let (events_tx, mut events_rx) = mpsc::channel::<PoolEvent>(256);
    let (blocks_tx, mut blocks_rx) = mpsc::channel(4);
    let bound = StratumServer {
        config: StratumConfig {
            listen: "127.0.0.1:0".parse().unwrap(),
            vardiff: VardiffConfig {
                initial_difficulty: SHARE_DIFFICULTY,
                min_difficulty: SHARE_DIFFICULTY,
                ..Default::default()
            },
        },
        work: work_rx.clone(),
        payouts,
        events: events_tx,
        blocks: blocks_tx,
    }
    .bind()
    .await
    .unwrap();
    let addr = bound.local_addr;
    tokio::spawn(bound.run(shutdown.child_token()));

    // A miniature stratum client.
    let stream = TcpStream::connect(addr).await.unwrap();
    let (reader, mut writer) = stream.into_split();
    let mut lines = BufReader::new(reader).lines();

    send(
        &mut writer,
        json!({"id": 1, "method": "mining.subscribe", "params": ["alamo-test/0.1"]}),
    )
    .await;
    let sub = read_until(&mut lines, |m| m["id"] == 1).await;
    let extranonce1 = hex::decode(sub["result"][1].as_str().unwrap()).unwrap();
    let en2_size = sub["result"][2].as_u64().unwrap() as usize;
    assert_eq!(extranonce1.len(), 4);
    assert_eq!(en2_size, 4);

    let worker = format!("{address}.rig1");
    send(
        &mut writer,
        json!({"id": 2, "method": "mining.authorize", "params": [worker, "x"]}),
    )
    .await;
    let auth = read_until(&mut lines, |m| m["id"] == 2).await;
    assert_eq!(auth["result"], json!(true), "{auth}");
    let diff_msg = read_until(&mut lines, |m| m["method"] == "mining.set_difficulty").await;
    let difficulty = diff_msg["params"][0].as_f64().unwrap();
    assert!((difficulty - SHARE_DIFFICULTY).abs() < 1e-12);
    let notify = read_until(&mut lines, |m| m["method"] == "mining.notify").await;
    let p = &notify["params"];
    let job_id = p[0].as_str().unwrap().to_string();
    let prev_hash = prevhash_from_stratum(p[1].as_str().unwrap()).unwrap();
    let coinb1 = hex::decode(p[2].as_str().unwrap()).unwrap();
    let coinb2 = hex::decode(p[3].as_str().unwrap()).unwrap();
    let branch: Vec<[u8; 32]> = p[4]
        .as_array()
        .unwrap()
        .iter()
        .map(|h| {
            hex::decode(h.as_str().unwrap())
                .unwrap()
                .try_into()
                .unwrap()
        })
        .collect();
    let version = u32::from_str_radix(p[5].as_str().unwrap(), 16).unwrap() as i32;
    let bits = u32::from_str_radix(p[6].as_str().unwrap(), 16).unwrap();
    let ntime = u32::from_str_radix(p[7].as_str().unwrap(), 16).unwrap();
    assert_eq!(prev_hash, from_display_hex(&info.bestblockhash).unwrap());
    let template = work_rx.borrow().clone().expect("template published");
    assert_eq!(template.height, info.blocks + 1);

    // Mine like an ASIC would, on the CPU.
    let extranonce2 = [0u8, 0, 0, 7];
    let mut coinbase = coinb1.clone();
    coinbase.extend_from_slice(&extranonce1);
    coinbase.extend_from_slice(&extranonce2);
    coinbase.extend_from_slice(&coinb2);
    let merkle_root = root_from_branch(&sha256d(&coinbase), &branch);
    let share_target = Target::from_difficulty(difficulty);
    let mut header = BlockHeader {
        version,
        prev_hash,
        merkle_root,
        time: ntime,
        bits,
        nonce: 0,
    };
    let nonce = (0u32..10_000_000)
        .find(|&n| {
            header.nonce = n;
            share_target.is_met_by(&Algorithm::Scrypt.pow_hash(&header.serialize()))
        })
        .expect("found a share");
    eprintln!("found share with nonce {nonce}");

    send(&mut writer, json!({
        "id": 3,
        "method": "mining.submit",
        "params": [worker, job_id, hex::encode(extranonce2), format!("{ntime:08x}"), format!("{nonce:08x}")]
    }))
    .await;
    let reply = read_until(&mut lines, |m| m["id"] == 3).await;
    assert_eq!(reply["result"], json!(true), "share rejected: {reply}");

    let share_event = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if let Some(PoolEvent::Share { rejected, .. }) = events_rx.recv().await {
                return rejected;
            }
        }
    })
    .await
    .expect("share event");
    assert_eq!(share_event, None);

    let candidate = tokio::time::timeout(Duration::from_secs(5), blocks_rx.recv())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(candidate.height, template.height);
    assert_eq!(candidate.address, address);
    header.nonce = nonce;
    assert_eq!(candidate.block_hash, to_display_hex(&header.block_hash()));

    let store = Store::open(
        &std::env::temp_dir()
            .join(format!("alamo-regtest-{}", std::process::id()))
            .join("t.db"),
    )
    .await
    .unwrap();
    let outcome = submit_candidate(&rpc, &store, &candidate).await.unwrap();
    assert_eq!(outcome, SubmitOutcome::Accepted, "node rejected our block");

    assert_eq!(rpc.get_block_count().await.unwrap(), template.height);
    let block = rpc.get_block_verbose(&candidate.block_hash).await.unwrap();
    assert_eq!(block["height"].as_u64().unwrap(), template.height);
    let coinbase_out = &block["tx"][0]["vout"][0];
    assert_eq!(
        coinbase_out["scriptPubKey"]["hex"].as_str().unwrap(),
        hex::encode(&script)
    );
    let addresses = coinbase_out["scriptPubKey"]["address"]
        .as_str()
        .map(str::to_string)
        .or_else(|| {
            coinbase_out["scriptPubKey"]["addresses"][0]
                .as_str()
                .map(str::to_string)
        });
    assert_eq!(addresses.as_deref(), Some(address.as_str()));

    // The pool sees the new tip and hands out clean work for the next height.
    let next = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let msg = lines.next_line().await.unwrap().unwrap();
            let msg: Value = serde_json::from_str(&msg).unwrap();
            if msg["method"] == "mining.notify" && msg["params"][8] == json!(true) {
                return msg;
            }
        }
    })
    .await
    .expect("clean notify after block");
    assert_eq!(
        prevhash_from_stratum(next["params"][1].as_str().unwrap()).unwrap(),
        from_display_hex(&candidate.block_hash).unwrap()
    );

    shutdown.cancel();
}

async fn send(writer: &mut tokio::net::tcp::OwnedWriteHalf, v: Value) {
    let mut text = v.to_string();
    text.push('\n');
    writer.write_all(text.as_bytes()).await.unwrap();
}

async fn read_until<R: tokio::io::AsyncBufRead + Unpin>(
    lines: &mut tokio::io::Lines<R>,
    pred: impl Fn(&Value) -> bool,
) -> Value {
    tokio::time::timeout(Duration::from_secs(20), async {
        loop {
            let line = lines.next_line().await.unwrap().expect("connection closed");
            let msg: Value = serde_json::from_str(&line).unwrap();
            if pred(&msg) {
                return msg;
            }
        }
    })
    .await
    .expect("timed out waiting for a stratum message")
}
