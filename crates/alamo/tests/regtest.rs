//! End-to-end test against Litecoin (and optionally Dogecoin) regtest nodes.
//!
//! Skipped unless `ALAMO_REGTEST_RPC` is set, e.g.
//! `ALAMO_REGTEST_RPC=http://alamo:alamo@127.0.0.1:19443`. When `ALAMO_REGTEST_DOGE_RPC`
//! is also set, Dogecoin is merge-mined and the test checks that the same share yields an
//! accepted Dogecoin block paying the address given in the stratum password.
//!
//! Starts the template sources and stratum server in-process, connects a miniature stratum
//! client that mines a share with scrypt on the CPU, and checks that the resulting blocks
//! are accepted by the nodes and pay the addresses the client authorized with.
//!
//! `a_lane_mines_while_another_lane_waits_for_its_node` runs the whole pool with a second
//! lane whose node is down and checks the LTC lane still listens and hands out a job.
//!
//! `mines_bitcoin_blocks_with_version_rolling` needs `ALAMO_REGTEST_BTC_RPC` (e.g.
//! `http://alamo:alamo@127.0.0.1:18443`) and a node prepared by
//! `deploy/regtest/prepare-bitcoin.sh`. It runs the whole pool with a BTC lane, negotiates
//! BIP310 version rolling, mines SHA-256d shares on the CPU with rolled version bits, and
//! checks that bitcoind accepts the blocks, with the rolled version, the mempool's segwit
//! transaction, and coinbases paying a bech32 and then a bech32m (taproot) address.

use alamo::pool::{submit_candidate, SubmitOutcome};
use alamo_coins::{merge, Chain, Coin, Dogecoin, Litecoin, RpcClient, TemplateSource};
use alamo_core::address::{encode_base58, encode_segwit, payout_script};
use alamo_core::auxpow::MERGED_MINING_MAGIC;
use alamo_core::hash::{from_display_hex, sha256d, to_display_hex};
use alamo_core::header::{rolled_version, BlockHeader};
use alamo_core::merkle::root_from_branch;
use alamo_core::payout::{AuxPayoutTable, PayoutSet, PayoutTable};
use alamo_core::target::Target;
use alamo_core::Algorithm;
use alamo_store::Store;
use alamo_stratum::job::prevhash_from_stratum;
use alamo_stratum::{BlockCandidate, PoolEvent, StratumServer, VardiffConfig};
use serde_json::{json, Value};
use std::sync::Arc;
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpStream;
use tokio::sync::{mpsc, watch};
use tokio_util::sync::CancellationToken;

/// Stratum share difficulty for the test session. In scrypt share units (65536 per unit
/// of network difficulty) this is network difficulty 5e-7, still above regtest's ~4.7e-10,
/// so every share the CPU miner finds is also a block on both chains.
const SHARE_DIFFICULTY: f64 = 0.0000005 * 65536.0;

/// Dogecoin regtest rejects auxpow blocks before this height.
const DOGE_AUXPOW_START: u64 = 20;

#[tokio::test]
async fn mines_a_block_on_regtest() {
    let Ok(url) = std::env::var("ALAMO_REGTEST_RPC") else {
        eprintln!("ALAMO_REGTEST_RPC not set; skipping regtest integration test");
        return;
    };
    let rpc = RpcClient::from_url_with_userinfo(&url).expect("ALAMO_REGTEST_RPC must be a URL");
    let info = rpc.get_blockchain_info().await.expect("node reachable");
    assert_eq!(info.chain, "regtest", "this test only runs against regtest");

    let doge_rpc = std::env::var("ALAMO_REGTEST_DOGE_RPC").ok().map(|url| {
        RpcClient::from_url_with_userinfo(&url).expect("ALAMO_REGTEST_DOGE_RPC must be a URL")
    });
    if let Some(doge_rpc) = &doge_rpc {
        let doge_info = doge_rpc
            .get_blockchain_info()
            .await
            .expect("dogecoin node reachable");
        assert_eq!(doge_info.chain, "regtest");
        assert!(
            doge_info.blocks >= DOGE_AUXPOW_START,
            "dogecoin regtest needs {DOGE_AUXPOW_START}+ blocks before auxpow is allowed; run deploy/regtest/prepare-dogecoin.sh"
        );
    }

    let coin: Arc<dyn Coin> = Arc::new(Litecoin);
    let params = coin.address_params(Chain::Regtest);
    let address = encode_segwit("rltc", 0, &[0x11; 20]).unwrap();
    let script = payout_script(&address, &params).unwrap();
    let fallback = encode_segwit("rltc", 0, &[0x22; 20]).unwrap();
    let doge_params = Dogecoin.address_params(Chain::Regtest);
    let doge_address = encode_base58(111, &[0x33; 20]);
    let doge_script = payout_script(&doge_address, &doge_params).unwrap();
    let doge_fallback = encode_base58(111, &[0x44; 20]);
    let payouts = Arc::new(PayoutSet {
        parent: PayoutTable::new(params, &fallback).unwrap(),
        aux: doge_rpc
            .iter()
            .map(|_| AuxPayoutTable {
                coin: "DOGE",
                table: PayoutTable::new(doge_params.clone(), &doge_fallback).unwrap(),
            })
            .collect(),
    });

    let shutdown = CancellationToken::new();
    let (parent_tx, parent_rx) = watch::channel(None);
    tokio::spawn(
        TemplateSource {
            poll_interval: Duration::from_millis(200),
            ..TemplateSource::new(rpc.clone(), coin.clone(), b"/alamo-test/".to_vec())
        }
        .run(parent_tx, shutdown.child_token()),
    );
    let mut aux_rxs = Vec::new();
    if let Some(doge_rpc) = &doge_rpc {
        let (aux_tx, aux_rx) = watch::channel(None);
        tokio::spawn(
            TemplateSource {
                poll_interval: Duration::from_millis(200),
                ..TemplateSource::new(
                    doge_rpc.clone(),
                    Arc::new(Dogecoin),
                    b"/alamo-test/".to_vec(),
                )
            }
            .run(aux_tx, shutdown.child_token()),
        );
        aux_rxs.push(aux_rx);
    }
    let (work_tx, work_rx) = watch::channel(None);
    tokio::spawn(merge(parent_rx, aux_rxs, work_tx, shutdown.child_token()));

    let (events_tx, mut events_rx) = mpsc::channel::<PoolEvent>(256);
    let (blocks_tx, mut blocks_rx) = mpsc::channel(4);
    let bound = StratumServer {
        listen: "127.0.0.1:0".parse().unwrap(),
        vardiff: watch::channel(VardiffConfig {
            initial_difficulty: SHARE_DIFFICULTY,
            min_difficulty: SHARE_DIFFICULTY,
            ..Default::default()
        })
        .1,
        payouts: watch::channel(payouts).1,
        work: work_rx.clone(),
        events: events_tx,
        blocks: blocks_tx,
        version_mask: Litecoin.version_rolling_mask(),
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
    let password = if doge_rpc.is_some() {
        doge_address.clone()
    } else {
        "x".to_string()
    };
    send(
        &mut writer,
        json!({"id": 2, "method": "mining.authorize", "params": [worker, password]}),
    )
    .await;
    let auth = read_until(&mut lines, |m| m["id"] == 2).await;
    assert_eq!(auth["result"], json!(true), "{auth}");
    let diff_msg = read_until(&mut lines, |m| m["method"] == "mining.set_difficulty").await;
    let difficulty = diff_msg["params"][0].as_f64().unwrap();
    assert!((difficulty - SHARE_DIFFICULTY).abs() < 1e-12);

    // With merge mining, wait for a job whose coinbase carries the aux commitment; the
    // Litecoin-only job may arrive first if dogecoind answered later.
    let notify = read_until(&mut lines, |m| {
        m["method"] == "mining.notify"
            && (doge_rpc.is_none()
                || hex::decode(m["params"][2].as_str().unwrap_or_default())
                    .unwrap_or_default()
                    .windows(4)
                    .any(|w| w == MERGED_MINING_MAGIC))
    })
    .await;
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
    let merged = work_rx.borrow().clone().expect("work published");
    let template = merged.parent.clone();
    assert_eq!(template.height, info.blocks + 1);
    assert_eq!(merged.aux.len(), usize::from(doge_rpc.is_some()));

    // Mine like an ASIC would, on the CPU.
    let extranonce2 = [0u8, 0, 0, 7];
    let mut coinbase = coinb1.clone();
    coinbase.extend_from_slice(&extranonce1);
    coinbase.extend_from_slice(&extranonce2);
    coinbase.extend_from_slice(&coinb2);
    let merkle_root = root_from_branch(&sha256d(&coinbase), &branch);
    // Mine against the target a real scrypt miner would derive from set_difficulty.
    let share_target = Algorithm::Scrypt.share_target(difficulty);
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

    // Regtest targets are equal on both chains, so one share is a block on each.
    let mut candidates: Vec<BlockCandidate> = Vec::new();
    for _ in 0..1 + merged.aux.len() {
        candidates.push(
            tokio::time::timeout(Duration::from_secs(5), blocks_rx.recv())
                .await
                .expect("block candidate")
                .unwrap(),
        );
    }
    let candidate = candidates.iter().find(|c| c.coin == "LTC").unwrap();
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
    let outcome = submit_candidate(&rpc, &store, "ltc", candidate)
        .await
        .unwrap();
    assert_eq!(outcome, SubmitOutcome::Accepted, "node rejected our block");

    assert_eq!(rpc.get_block_count().await.unwrap(), template.height);
    let block = rpc.get_block_verbose(&candidate.block_hash).await.unwrap();
    assert_eq!(block["height"].as_u64().unwrap(), template.height);
    let coinbase_out = &block["tx"][0]["vout"][0];
    assert_eq!(
        coinbase_out["scriptPubKey"]["hex"].as_str().unwrap(),
        hex::encode(&script)
    );
    assert_eq!(
        script_address(coinbase_out).as_deref(),
        Some(address.as_str())
    );

    if let Some(doge_rpc) = &doge_rpc {
        let doge_template = merged.aux[0].clone();
        let doge = candidates.iter().find(|c| c.coin == "DOGE").unwrap();
        assert_eq!(doge.height, doge_template.height);
        assert_eq!(doge.address, doge_address);
        let outcome = submit_candidate(doge_rpc, &store, "ltc", doge)
            .await
            .unwrap();
        assert_eq!(
            outcome,
            SubmitOutcome::Accepted,
            "dogecoin node rejected our merge-mined block"
        );
        assert_eq!(doge_rpc.get_block_count().await.unwrap(), doge.height);
        let block = doge_rpc.get_block_verbose(&doge.block_hash).await.unwrap();
        assert_eq!(block["height"].as_u64().unwrap(), doge.height);
        assert_eq!(block["versionHex"].as_str().unwrap(), "00620104");
        let coinbase_out = &block["tx"][0]["vout"][0];
        assert_eq!(
            coinbase_out["scriptPubKey"]["hex"].as_str().unwrap(),
            hex::encode(&doge_script)
        );
        assert_eq!(
            script_address(coinbase_out).as_deref(),
            Some(doge_address.as_str())
        );
        eprintln!(
            "merge-mined DOGE block {} at height {} (parent LTC {})",
            doge.block_hash, doge.height, candidate.block_hash
        );
    }

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

/// The whole pool with two lanes: LTC on the regtest node and a second lane (DOGE as a
/// parent) whose node is a closed port. The LTC lane must listen and hand out a job while
/// the other lane is still retrying. Only `ALAMO_REGTEST_RPC` is needed; the second lane
/// never reaches a node.
#[tokio::test]
async fn a_lane_mines_while_another_lane_waits_for_its_node() {
    let Ok(url) = std::env::var("ALAMO_REGTEST_RPC") else {
        eprintln!("ALAMO_REGTEST_RPC not set; skipping the lane independence regtest");
        return;
    };
    let rpc = RpcClient::from_url_with_userinfo(&url).expect("ALAMO_REGTEST_RPC must be a URL");
    let info = rpc.get_blockchain_info().await.expect("node reachable");
    assert_eq!(info.chain, "regtest", "this test only runs against regtest");
    let parsed = split_credentials(&url);

    let free_port = || {
        std::net::TcpListener::bind("127.0.0.1:0")
            .unwrap()
            .local_addr()
            .unwrap()
            .port()
    };
    let (ltc_port, doge_port) = (free_port(), free_port());
    let data_dir = std::env::temp_dir().join(format!("alamo-regtest-lanes-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&data_dir);
    let address = encode_segwit("rltc", 0, &[0x11; 20]).unwrap();
    let fallback = encode_segwit("rltc", 0, &[0x22; 20]).unwrap();
    let config = alamo::Config::parse(&format!(
        r#"
[pool]
name = "regtest lanes"
data_dir = "{data_dir}"
[stratum]
listen = "127.0.0.1:{ltc_port}"
[web]
listen = "127.0.0.1:0"
[coins.ltc]
rpc_url = "{rpc_url}"
rpc_user = "{rpc_user}"
rpc_password = "{rpc_password}"
fallback_address = "{fallback}"
poll_interval_ms = 200
[coins.doge]
rpc_url = "http://127.0.0.1:1"
rpc_user = "u"
rpc_password = "p"
fallback_address = "nUnreachable"
[coins.doge.stratum]
listen = "127.0.0.1:{doge_port}"
"#,
        data_dir = data_dir.display(),
        rpc_url = parsed.0,
        rpc_user = parsed.1,
        rpc_password = parsed.2,
    ))
    .unwrap();
    let store = Store::open(&config.database_path()).await.unwrap();
    let state = alamo_web::AppState::new("regtest lanes", ltc_port, store.clone());
    let log = alamo::settings::LogSetup {
        filter: "info".into(),
        control: Arc::new(|_: &str| Ok(())),
    };
    let shutdown = CancellationToken::new();
    let pool = tokio::spawn(alamo::pool::run(
        config,
        store,
        state,
        log,
        shutdown.clone(),
    ));

    let stream = tokio::time::timeout(Duration::from_secs(20), async {
        loop {
            if let Ok(stream) = TcpStream::connect(("127.0.0.1", ltc_port)).await {
                return stream;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    })
    .await
    .expect("the LTC lane never opened its listener");
    let (reader, mut writer) = stream.into_split();
    let mut lines = BufReader::new(reader).lines();
    send(
        &mut writer,
        json!({"id": 1, "method": "mining.subscribe", "params": ["alamo-test/0.1"]}),
    )
    .await;
    read_until(&mut lines, |m| m["id"] == 1).await;
    send(
        &mut writer,
        json!({"id": 2, "method": "mining.authorize", "params": [format!("{address}.lanes"), "x"]}),
    )
    .await;
    let auth = read_until(&mut lines, |m| m["id"] == 2).await;
    assert_eq!(auth["result"], json!(true), "{auth}");
    let notify = read_until(&mut lines, |m| m["method"] == "mining.notify").await;
    let prev_hash = prevhash_from_stratum(notify["params"][1].as_str().unwrap()).unwrap();
    let tip = rpc.get_blockchain_info().await.unwrap();
    assert!(
        prev_hash == from_display_hex(&tip.bestblockhash).unwrap()
            || prev_hash == from_display_hex(&info.bestblockhash).unwrap(),
        "the job builds on the node's tip"
    );

    assert!(
        TcpStream::connect(("127.0.0.1", doge_port)).await.is_err(),
        "the second lane must not listen before its node answers"
    );
    assert!(!pool.is_finished(), "a down node must not stop the pool");

    shutdown.cancel();
    tokio::time::timeout(Duration::from_secs(10), pool)
        .await
        .expect("the pool did not stop")
        .unwrap()
        .unwrap();
    let _ = std::fs::remove_dir_all(&data_dir);
}

/// BIP320's general-purpose bits: what a Bitcoin lane offers to roll.
const BTC_VERSION_MASK: u32 = 0x1fff_e000;

/// A SHA-256d share difficulty every Bitcoin regtest block meets (network difficulty there
/// is about 4.7e-10), so each share is a block. Below the built-in minimum of 0.001, so the
/// test configures its own vardiff table.
const BTC_SHARE_DIFFICULTY: f64 = 0.000_000_001;

#[tokio::test]
async fn mines_bitcoin_blocks_with_version_rolling() {
    let Ok(url) = std::env::var("ALAMO_REGTEST_BTC_RPC") else {
        eprintln!("ALAMO_REGTEST_BTC_RPC not set; skipping the Bitcoin regtest");
        return;
    };
    let rpc = RpcClient::from_url_with_userinfo(&url).expect("ALAMO_REGTEST_BTC_RPC must be a URL");
    let info = rpc.get_blockchain_info().await.expect("bitcoind reachable");
    assert_eq!(info.chain, "regtest", "this test only runs against regtest");
    let segwit_txids = mempool_segwit_txids(&rpc).await;
    assert!(
        !segwit_txids.is_empty(),
        "the bitcoind mempool holds no segwit transaction; run deploy/regtest/prepare-bitcoin.sh"
    );
    let (rpc_url, rpc_user, rpc_password) = split_credentials(&url);

    let port = std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port();
    let data_dir = std::env::temp_dir().join(format!("alamo-regtest-btc-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&data_dir);
    let segwit_address = encode_segwit("bcrt", 0, &[0x55; 20]).unwrap();
    let taproot_address = encode_segwit("bcrt", 1, &[0x66; 32]).unwrap();
    assert!(taproot_address.starts_with("bcrt1p"));
    let fallback = encode_segwit("bcrt", 0, &[0x77; 20]).unwrap();
    let config = alamo::Config::parse(&format!(
        r#"
[pool]
name = "regtest btc"
data_dir = "{data_dir}"
[stratum]
listen = "127.0.0.1:{port}"
[stratum.vardiff]
initial_difficulty = 1
min_difficulty = {BTC_SHARE_DIFFICULTY:e}
max_difficulty = 4294967296
target_share_seconds = 10
retarget_seconds = 60
variance_percent = 30
[web]
listen = "127.0.0.1:0"
[coins.btc]
rpc_url = "{rpc_url}"
rpc_user = "{rpc_user}"
rpc_password = "{rpc_password}"
fallback_address = "{fallback}"
poll_interval_ms = 200
"#,
        data_dir = data_dir.display(),
    ))
    .unwrap();
    let store = Store::open(&config.database_path()).await.unwrap();
    let state = alamo_web::AppState::new("regtest btc", port, store.clone());
    let log = alamo::settings::LogSetup {
        filter: "info".into(),
        control: Arc::new(|_: &str| Ok(())),
    };
    let shutdown = CancellationToken::new();
    let pool = tokio::spawn(alamo::pool::run(
        config,
        store,
        state,
        log,
        shutdown.clone(),
    ));

    // First block: configure between subscribe and authorize, a bech32 v0 payout, the
    // starting difficulty from `d=` in the password, rolled version bits.
    let mut miner = BtcMiner::connect(port).await;
    miner.subscribe().await;
    let mask = miner.configure().await;
    assert_eq!(mask, BTC_VERSION_MASK);
    let difficulty = miner
        .authorize(&format!("{segwit_address}.rig1"), "x,d=0.000000001")
        .await;
    assert!(
        (difficulty - BTC_SHARE_DIFFICULTY).abs() < 1e-18,
        "{difficulty}"
    );
    let job = miner.job(&info.bestblockhash).await;
    assert!(
        !job.branch.is_empty(),
        "the template carries the mempool transactions"
    );
    let first = miner.mine_and_submit(&job, 0x0a5a_e000).await;
    assert_ne!(first.version as u32 & BTC_VERSION_MASK, 0);
    let block = accepted_block(&rpc, &first.hash, info.blocks + 1).await;
    assert_eq!(
        block["versionHex"].as_str().unwrap(),
        format!("{:08x}", first.version as u32)
    );
    assert_eq!(first.version as u32 & BTC_VERSION_MASK, 0x0a5a_e000);
    let coinbase_out = &block["tx"][0]["vout"][0];
    assert_eq!(
        script_address(coinbase_out).as_deref(),
        Some(segwit_address.as_str())
    );
    let in_block: Vec<&str> = block["tx"]
        .as_array()
        .unwrap()
        .iter()
        .map(|tx| tx["txid"].as_str().unwrap())
        .collect();
    for txid in &segwit_txids {
        assert!(
            in_block.contains(&txid.as_str()),
            "mempool segwit tx {txid} not in the block"
        );
    }
    let witness_tx = block["tx"]
        .as_array()
        .unwrap()
        .iter()
        .skip(1)
        .find(|tx| tx["txid"] != tx["hash"])
        .expect("a transaction whose wtxid differs from its txid");
    eprintln!(
        "BTC block {} at height {} version {} pays {} and carries segwit tx {}",
        first.hash,
        info.blocks + 1,
        block["versionHex"],
        segwit_address,
        witness_tx["txid"]
    );

    // Second block: authorize a taproot (bech32m) address first, configure afterwards.
    let mut taproot = BtcMiner::connect(port).await;
    taproot.subscribe().await;
    let difficulty = taproot.authorize(&taproot_address, "d=0.000000001").await;
    assert!((difficulty - BTC_SHARE_DIFFICULTY).abs() < 1e-18);
    assert_eq!(taproot.configure().await, BTC_VERSION_MASK);
    let job = taproot.job(&first.hash).await;
    // A bit outside the negotiated mask is refused with its own reason.
    let reply = taproot.submit_raw(&job, 0, 0x0000_1000).await;
    assert_eq!(reply["error"][0], 20, "{reply}");
    assert_eq!(reply["error"][1], "Invalid version bits", "{reply}");
    let second = taproot.mine_and_submit(&job, 0x0000_6000).await;
    let block = accepted_block(&rpc, &second.hash, info.blocks + 2).await;
    assert_eq!(
        block["versionHex"].as_str().unwrap(),
        format!("{:08x}", second.version as u32)
    );
    assert_eq!(second.version as u32 & BTC_VERSION_MASK, 0x0000_6000);
    let coinbase_out = &block["tx"][0]["vout"][0];
    assert_eq!(
        coinbase_out["scriptPubKey"]["hex"].as_str().unwrap(),
        format!("5120{}", "66".repeat(32))
    );
    assert_eq!(
        script_address(coinbase_out).as_deref(),
        Some(taproot_address.as_str())
    );
    eprintln!(
        "BTC block {} at height {} version {} pays {}",
        second.hash,
        info.blocks + 2,
        block["versionHex"],
        taproot_address
    );

    shutdown.cancel();
    tokio::time::timeout(Duration::from_secs(10), pool)
        .await
        .expect("the pool did not stop")
        .unwrap()
        .unwrap();
    let _ = std::fs::remove_dir_all(&data_dir);
}

/// Txids of mempool transactions that carry witness data.
async fn mempool_segwit_txids(rpc: &RpcClient) -> Vec<String> {
    let txids: Vec<String> = rpc.call("getrawmempool", &[]).await.unwrap();
    let mut segwit = Vec::new();
    for txid in txids {
        let tx: Value = rpc
            .call("getrawtransaction", &[json!(txid), json!(true)])
            .await
            .unwrap();
        let witness = tx["vin"]
            .as_array()
            .is_some_and(|vin| vin.iter().any(|i| i.get("txinwitness").is_some()));
        if witness {
            segwit.push(txid);
        }
    }
    segwit
}

/// Wait for bitcoind to have `hash` as its tip at `height`, and return it decoded.
async fn accepted_block(rpc: &RpcClient, hash: &str, height: u64) -> Value {
    tokio::time::timeout(Duration::from_secs(20), async {
        loop {
            if rpc.get_best_block_hash().await.unwrap() == hash {
                break;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    })
    .await
    .unwrap_or_else(|_| panic!("bitcoind never accepted block {hash}"));
    let block = rpc.get_block_verbose(hash).await.unwrap();
    assert_eq!(block["height"].as_u64().unwrap(), height);
    block
}

/// A `mining.notify` job, decoded the way a miner does.
struct BtcJob {
    id: String,
    prev_hash: [u8; 32],
    coinb1: Vec<u8>,
    coinb2: Vec<u8>,
    branch: Vec<[u8; 32]>,
    version: i32,
    bits: u32,
    ntime: u32,
}

/// A mined and accepted share.
struct BtcShare {
    hash: String,
    version: i32,
}

/// A miniature SHA-256d stratum client with BIP310 version rolling.
struct BtcMiner {
    writer: tokio::net::tcp::OwnedWriteHalf,
    lines: tokio::io::Lines<BufReader<tokio::net::tcp::OwnedReadHalf>>,
    extranonce1: Vec<u8>,
    worker: String,
    difficulty: f64,
    mask: u32,
    next_id: u64,
    /// Messages read while waiting for another, oldest first, so a `mining.notify` that
    /// arrives while a reply is awaited is not lost.
    pending: std::collections::VecDeque<Value>,
}

impl BtcMiner {
    async fn connect(port: u16) -> Self {
        let stream = tokio::time::timeout(Duration::from_secs(30), async {
            loop {
                if let Ok(stream) = TcpStream::connect(("127.0.0.1", port)).await {
                    return stream;
                }
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
        })
        .await
        .expect("the BTC lane never opened its listener");
        let (reader, writer) = stream.into_split();
        Self {
            writer,
            lines: BufReader::new(reader).lines(),
            extranonce1: Vec::new(),
            worker: String::new(),
            difficulty: 0.0,
            mask: 0,
            next_id: 1,
            pending: Default::default(),
        }
    }

    /// The first message, kept or new, that satisfies `pred`; others are kept.
    async fn wait(&mut self, pred: impl Fn(&Value) -> bool) -> Value {
        if let Some(i) = self.pending.iter().position(&pred) {
            return self.pending.remove(i).unwrap();
        }
        let Self { lines, pending, .. } = self;
        tokio::time::timeout(Duration::from_secs(30), async {
            loop {
                let line = lines.next_line().await.unwrap().expect("connection closed");
                let msg: Value = serde_json::from_str(&line).unwrap();
                if pred(&msg) {
                    return msg;
                }
                pending.push_back(msg);
            }
        })
        .await
        .expect("timed out waiting for a stratum message")
    }

    async fn request(&mut self, method: &str, params: Value) -> Value {
        let id = self.next_id;
        self.next_id += 1;
        send(
            &mut self.writer,
            json!({"id": id, "method": method, "params": params}),
        )
        .await;
        self.wait(|m| m["id"] == id).await
    }

    async fn subscribe(&mut self) {
        let sub = self
            .request("mining.subscribe", json!(["alamo-test/0.1"]))
            .await;
        self.extranonce1 = hex::decode(sub["result"][1].as_str().unwrap()).unwrap();
        assert_eq!(sub["result"][2], 4);
    }

    /// Negotiate version rolling the way cgminer-style firmware does, and return the mask.
    async fn configure(&mut self) -> u32 {
        let reply = self
            .request(
                "mining.configure",
                json!([["version-rolling"], {"version-rolling.mask": "ffffffff", "version-rolling.min-bit-count": 2}]),
            )
            .await;
        assert_eq!(reply["result"]["version-rolling"], json!(true), "{reply}");
        self.mask = u32::from_str_radix(
            reply["result"]["version-rolling.mask"].as_str().unwrap(),
            16,
        )
        .unwrap();
        self.mask
    }

    /// Authorize and return the difficulty the pool then sets.
    async fn authorize(&mut self, worker: &str, password: &str) -> f64 {
        self.worker = worker.to_string();
        let auth = self
            .request("mining.authorize", json!([worker, password]))
            .await;
        assert_eq!(auth["result"], json!(true), "{auth}");
        let msg = self.wait(|m| m["method"] == "mining.set_difficulty").await;
        self.difficulty = msg["params"][0].as_f64().unwrap();
        self.difficulty
    }

    /// The first job built on `prev_display_hash`.
    async fn job(&mut self, prev_display_hash: &str) -> BtcJob {
        let prev = from_display_hex(prev_display_hash).unwrap();
        let notify = self
            .wait(|m| {
                m["method"] == "mining.notify"
                    && m["params"][1]
                        .as_str()
                        .and_then(prevhash_from_stratum)
                        .is_some_and(|p| p == prev)
            })
            .await;
        let p = &notify["params"];
        let word = |i: usize| u32::from_str_radix(p[i].as_str().unwrap(), 16).unwrap();
        BtcJob {
            id: p[0].as_str().unwrap().to_string(),
            prev_hash: prev,
            coinb1: hex::decode(p[2].as_str().unwrap()).unwrap(),
            coinb2: hex::decode(p[3].as_str().unwrap()).unwrap(),
            branch: p[4]
                .as_array()
                .unwrap()
                .iter()
                .map(|h| {
                    hex::decode(h.as_str().unwrap())
                        .unwrap()
                        .try_into()
                        .unwrap()
                })
                .collect(),
            version: word(5) as i32,
            bits: word(6),
            ntime: word(7),
        }
    }

    fn header(&self, job: &BtcJob, extranonce2: [u8; 4], version_bits: u32) -> BlockHeader {
        let mut coinbase = job.coinb1.clone();
        coinbase.extend_from_slice(&self.extranonce1);
        coinbase.extend_from_slice(&extranonce2);
        coinbase.extend_from_slice(&job.coinb2);
        BlockHeader {
            version: rolled_version(job.version, version_bits, self.mask),
            prev_hash: job.prev_hash,
            merkle_root: root_from_branch(&sha256d(&coinbase), &job.branch),
            time: job.ntime,
            bits: job.bits,
            nonce: 0,
        }
    }

    async fn submit_raw(&mut self, job: &BtcJob, nonce: u32, version_bits: u32) -> Value {
        let params = json!([
            self.worker,
            job.id,
            "00000009",
            format!("{:08x}", job.ntime),
            format!("{nonce:08x}"),
            format!("{version_bits:08x}"),
        ]);
        self.request("mining.submit", params).await
    }

    /// Mine a share with `version_bits` rolled into the header, submit it, and return the
    /// block it makes.
    async fn mine_and_submit(&mut self, job: &BtcJob, version_bits: u32) -> BtcShare {
        assert_eq!(version_bits & !self.mask, 0, "test bits fit the mask");
        let extranonce2 = [0u8, 0, 0, 7];
        let mut header = self.header(job, extranonce2, version_bits);
        let target = Algorithm::Sha256d.share_target(self.difficulty);
        header.nonce = (0u32..1_000_000)
            .find(|&n| {
                header.nonce = n;
                target.is_met_by(&Algorithm::Sha256d.pow_hash(&header.serialize()))
            })
            .expect("found a share");
        assert!(Target::from_compact(header.bits).is_met_by(&header.block_hash()));
        let params = json!([
            self.worker,
            job.id,
            hex::encode(extranonce2),
            format!("{:08x}", job.ntime),
            format!("{:08x}", header.nonce),
            format!("{version_bits:08x}"),
        ]);
        let reply = self.request("mining.submit", params).await;
        assert_eq!(reply["result"], json!(true), "share rejected: {reply}");
        BtcShare {
            hash: to_display_hex(&header.block_hash()),
            version: header.version,
        }
    }
}

/// Split `http://user:pass@host:port/` into the URL without credentials, the user, and
/// the password, the way the config file wants them.
fn split_credentials(url: &str) -> (String, String, String) {
    let (scheme, rest) = url.split_once("://").expect("URL has a scheme");
    let (userinfo, host) = rest.rsplit_once('@').expect("URL carries user:password@");
    let (user, password) = userinfo.split_once(':').unwrap_or((userinfo, ""));
    (
        format!("{scheme}://{host}"),
        user.to_string(),
        password.to_string(),
    )
}

/// The address a decoded scriptPubKey pays, across node versions.
fn script_address(vout: &Value) -> Option<String> {
    vout["scriptPubKey"]["address"]
        .as_str()
        .map(str::to_string)
        .or_else(|| {
            vout["scriptPubKey"]["addresses"][0]
                .as_str()
                .map(str::to_string)
        })
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
