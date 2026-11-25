//! Block submission: retrying an unreachable node, keeping a copy of the block on disk
//! until it is accepted, and the per-lane submitter task.

use crate::notify::{Event, Notifier};
use alamo_coins::{RpcClient, RpcError};
use alamo_store::{BlockStatus, NewBlock, Store};
use alamo_stratum::BlockCandidate;
use anyhow::Context;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::mpsc;
use tokio::task::JoinSet;
use tokio_util::sync::CancellationToken;

/// Directory under `data_dir` holding copies of blocks whose submission has not been
/// accepted: still retrying, interrupted by a shutdown, or rejected.
pub const BLOCK_SPOOL_DIR: &str = "blocks";

/// Result of handing a block to the node.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SubmitOutcome {
    /// The node accepted the block.
    Accepted,
    /// The node rejected it with the given reason.
    Rejected(String),
}

impl SubmitOutcome {
    fn status(&self) -> BlockStatus {
        match self {
            SubmitOutcome::Accepted => BlockStatus::Accepted,
            SubmitOutcome::Rejected(_) => BlockStatus::Rejected,
        }
    }
}

/// Delays between `submitblock` attempts when the node cannot be reached. A found block
/// is worth far more than the wait: keep trying for several minutes, quickly at first so
/// a node that is merely restarting its RPC server costs almost nothing.
const SUBMIT_RETRY_DELAYS: [Duration; 12] = [
    Duration::from_millis(250),
    Duration::from_millis(500),
    Duration::from_secs(1),
    Duration::from_secs(2),
    Duration::from_secs(5),
    Duration::from_secs(10),
    Duration::from_secs(15),
    Duration::from_secs(30),
    Duration::from_secs(30),
    Duration::from_secs(60),
    Duration::from_secs(60),
    Duration::from_secs(60),
];

/// Bitcoin-derived nodes answer -28 while they are still starting up.
const RPC_IN_WARMUP: i64 = -28;

/// Whether a failed `submitblock` call says nothing about the block itself, so the same
/// block is worth sending again.
fn retryable(err: &RpcError) -> bool {
    match err {
        RpcError::Transport(_) => true,
        RpcError::Node { code, .. } => *code == RPC_IN_WARMUP,
        RpcError::Decode(_) | RpcError::Url(_) => false,
    }
}

/// Call `attempt` until the node gives a verdict or `delays` runs out.
///
/// A failed call may still have delivered the block (the reply was lost, or the node
/// went down right after connecting it), so once an attempt has failed, `duplicate` from
/// a later one means the node already has the block: that is an acceptance.
async fn submit_with_retry<F, Fut>(
    mut attempt: F,
    delays: &[Duration],
    coin: &'static str,
    hash: &str,
) -> SubmitOutcome
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = Result<Option<String>, RpcError>>,
{
    let mut failed = 0usize;
    loop {
        match attempt().await {
            Ok(None) => return SubmitOutcome::Accepted,
            Ok(Some(reason)) if failed > 0 && reason == "duplicate" => {
                tracing::info!(coin, %hash, "node already has the block from an earlier attempt");
                return SubmitOutcome::Accepted;
            }
            Ok(Some(reason)) => return SubmitOutcome::Rejected(reason),
            Err(err) => {
                let Some(delay) = delays.get(failed).filter(|_| retryable(&err)) else {
                    return SubmitOutcome::Rejected(format!("rpc: {err}"));
                };
                failed += 1;
                tracing::warn!(
                    coin, %hash, %err,
                    attempt = failed,
                    retry_in_ms = delay.as_millis() as u64,
                    "block submission failed; retrying"
                );
                tokio::time::sleep(*delay).await;
            }
        }
    }
}

/// Where a candidate's block is kept on disk while it is being submitted.
fn spool_path(dir: &Path, candidate: &BlockCandidate) -> PathBuf {
    dir.join(format!(
        "{}-{}-{}.hex",
        candidate.coin, candidate.height, candidate.block_hash
    ))
}

/// Write `block_hex` to `path` through a temporary file, so a partial write is never
/// mistaken for a block.
fn write_spool(path: &Path, block_hex: &str) -> std::io::Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let tmp = path.with_extension("hex.tmp");
    std::fs::write(&tmp, block_hex)?;
    std::fs::rename(&tmp, path)
}

/// Submit a block candidate to the node and record the result. An unreachable node is
/// retried for several minutes.
///
/// `lane` is the parent coin's config key, recorded with the block (also for an aux block).
pub async fn submit_candidate(
    rpc: &RpcClient,
    store: &Store,
    lane: &str,
    candidate: &BlockCandidate,
) -> anyhow::Result<SubmitOutcome> {
    submit_candidate_spooled(rpc, store, lane, candidate, None).await
}

/// [`submit_candidate`], keeping a copy of the block under `spool` until the node accepts
/// it. The copy is written alongside the first attempt, never ahead of it, and is what an
/// operator hands to `submitblock` if the daemon dies or every attempt fails.
pub async fn submit_candidate_spooled(
    rpc: &RpcClient,
    store: &Store,
    lane: &str,
    candidate: &BlockCandidate,
    spool: Option<&Path>,
) -> anyhow::Result<SubmitOutcome> {
    let outcome = submit_spooled(rpc, candidate, spool).await;
    record_block(store, lane, candidate, &outcome).await?;
    Ok(outcome)
}

/// Hand the block to the node (with retries), keeping the spool copy as
/// [`submit_candidate_spooled`] describes, and log the verdict.
async fn submit_spooled(
    rpc: &RpcClient,
    candidate: &BlockCandidate,
    spool: Option<&Path>,
) -> SubmitOutcome {
    let hex_block = Arc::new(hex::encode(&candidate.block));
    let spooled = spool.map(|dir| {
        let path = spool_path(dir, candidate);
        let (hex_block, target) = (hex_block.clone(), path.clone());
        let write = tokio::task::spawn_blocking(move || write_spool(&target, &hex_block));
        (path, write)
    });
    let outcome = submit_with_retry(
        || rpc.submit_block(&hex_block),
        &SUBMIT_RETRY_DELAYS,
        candidate.coin,
        &candidate.block_hash,
    )
    .await;
    let copy = match spooled {
        Some((path, write)) => match write.await {
            Ok(Ok(())) => Some(path),
            Ok(Err(err)) => {
                tracing::warn!(coin = candidate.coin, path = %path.display(), %err, "could not keep a copy of the block");
                None
            }
            Err(_) => None,
        },
        None => None,
    };
    match &outcome {
        SubmitOutcome::Accepted => {
            tracing::info!(
                coin = candidate.coin, height = candidate.height, hash = %candidate.block_hash,
                worker = %candidate.worker, address = %candidate.address, "block accepted by node"
            );
            if let Some(path) = &copy {
                let _ = std::fs::remove_file(path);
            }
        }
        SubmitOutcome::Rejected(reason) => tracing::error!(
            coin = candidate.coin, height = candidate.height, hash = %candidate.block_hash,
            %reason,
            copy = copy.as_ref().map(|p| p.display().to_string()),
            "block REJECTED by node"
        ),
    }
    outcome
}

/// Record a submitted block and its verdict.
async fn record_block(
    store: &Store,
    lane: &str,
    candidate: &BlockCandidate,
    outcome: &SubmitOutcome,
) -> anyhow::Result<()> {
    store
        .insert_block(&NewBlock {
            lane: lane.to_string(),
            coin: candidate.coin.to_string(),
            height: candidate.height,
            hash: candidate.block_hash.clone(),
            worker: candidate.worker.clone(),
            difficulty: candidate.network_difficulty,
            share_diff: candidate.share_difficulty,
            reward_sats: Some(candidate.coinbase_value as i64),
            found_at: candidate.found_at,
            status: outcome.status(),
        })
        .await
        .context("recording block")?;
    Ok(())
}

/// The `block_found` notification for a submitted candidate.
fn block_event(lane: &str, candidate: &BlockCandidate, outcome: &SubmitOutcome) -> Event {
    let (accepted, reason) = match outcome {
        SubmitOutcome::Accepted => (true, None),
        SubmitOutcome::Rejected(reason) => (false, Some(reason.clone())),
    };
    Event::BlockFound {
        lane: lane.to_string(),
        coin: candidate.coin.to_string(),
        height: candidate.height,
        hash: candidate.block_hash.clone(),
        worker: candidate.worker.clone(),
        accepted,
        reason,
    }
}

/// Hand every block candidate to its node. Each submission runs on its own, so a node
/// that is slow or down never holds up a block for another chain; on shutdown the
/// submissions still retrying are dropped and their copies stay in `spool`.
pub(super) async fn submitter(
    rpcs: HashMap<&'static str, RpcClient>,
    store: Store,
    lane: String,
    spool: PathBuf,
    mut blocks: mpsc::Receiver<BlockCandidate>,
    notifier: Notifier,
    shutdown: CancellationToken,
) -> &'static str {
    let mut pending: JoinSet<()> = JoinSet::new();
    loop {
        tokio::select! {
            next = blocks.recv() => {
                let Some(candidate) = next else { return "submitter" };
                let Some(rpc) = rpcs.get(candidate.coin).cloned() else {
                    tracing::error!(coin = candidate.coin, "no node for block candidate");
                    continue;
                };
                let (store, lane, spool) = (store.clone(), lane.clone(), spool.clone());
                let notifier = notifier.clone();
                pending.spawn(async move {
                    let outcome = submit_spooled(&rpc, &candidate, Some(&spool)).await;
                    // The verdict is known: notify before the database write, which
                    // cannot change it.
                    notifier.notify(block_event(&lane, &candidate, &outcome));
                    if let Err(err) = record_block(&store, &lane, &candidate, &outcome).await {
                        tracing::error!(coin = candidate.coin, %err, "block submission failed");
                    }
                });
            }
            Some(done) = pending.join_next() => {
                if let Err(join) = done {
                    tracing::error!(%join, "block submission task failed");
                }
            }
            _ = shutdown.cancelled() => {
                if !pending.is_empty() {
                    tracing::warn!(
                        pending = pending.len(),
                        spool = %spool.display(),
                        "shutting down with block submissions still retrying"
                    );
                }
                return "submitter";
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;
    use std::collections::VecDeque;

    /// A real transport error: nothing listens on port 1.
    async fn transport_error() -> RpcError {
        RpcClient::new("http://127.0.0.1:1", "u", "p")
            .submit_block("00")
            .await
            .expect_err("port 1 refuses connections")
    }

    /// Run `submit_with_retry` against scripted node answers; returns the outcome and how
    /// many attempts were made.
    async fn scripted(
        answers: Vec<Result<Option<String>, RpcError>>,
        retries: usize,
    ) -> (SubmitOutcome, usize) {
        let total = answers.len();
        let answers = RefCell::new(VecDeque::from(answers));
        let delays = vec![Duration::ZERO; retries];
        let outcome = submit_with_retry(
            || {
                let next = answers
                    .borrow_mut()
                    .pop_front()
                    .expect("unexpected attempt");
                async move { next }
            },
            &delays,
            "BTC",
            "00ff",
        )
        .await;
        let used = total - answers.borrow().len();
        (outcome, used)
    }

    #[tokio::test]
    async fn unreachable_node_is_retried_until_it_accepts() {
        let answers = vec![
            Err(transport_error().await),
            Err(RpcError::Node {
                code: RPC_IN_WARMUP,
                message: "Loading block index".into(),
            }),
            Ok(None),
        ];
        assert_eq!(scripted(answers, 5).await, (SubmitOutcome::Accepted, 3));
    }

    #[tokio::test]
    async fn duplicate_after_a_failed_attempt_is_an_acceptance() {
        let answers = vec![Err(transport_error().await), Ok(Some("duplicate".into()))];
        assert_eq!(scripted(answers, 5).await, (SubmitOutcome::Accepted, 2));
        // With no failed attempt before it, the node knew the block from someone else.
        let answers = vec![Ok(Some("duplicate".into()))];
        assert_eq!(
            scripted(answers, 5).await,
            (SubmitOutcome::Rejected("duplicate".into()), 1)
        );
    }

    #[tokio::test]
    async fn a_verdict_or_a_permanent_error_is_not_retried() {
        let answers = vec![Ok(Some("high-hash".into())), Ok(None)];
        assert_eq!(
            scripted(answers, 5).await,
            (SubmitOutcome::Rejected("high-hash".into()), 1)
        );
        let answers = vec![
            Err(RpcError::Node {
                code: -22,
                message: "Block decode failed".into(),
            }),
            Ok(None),
        ];
        let (outcome, used) = scripted(answers, 5).await;
        assert_eq!(used, 1);
        assert!(matches!(outcome, SubmitOutcome::Rejected(r) if r.contains("Block decode failed")));
    }

    #[tokio::test]
    async fn retries_stop_when_the_schedule_runs_out() {
        let answers = vec![
            Err(transport_error().await),
            Err(transport_error().await),
            Err(transport_error().await),
        ];
        let (outcome, used) = scripted(answers, 2).await;
        assert_eq!(used, 3, "one attempt plus two retries");
        assert!(matches!(outcome, SubmitOutcome::Rejected(r) if r.starts_with("rpc: ")));
    }

    #[test]
    fn spool_copy_is_written_whole_under_the_block_name() {
        let dir = std::env::temp_dir().join(format!("alamo-spool-{}", std::process::id()));
        let path = dir.join("nested").join("BTC-1-00ff.hex");
        write_spool(&path, "deadbeef").unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "deadbeef");
        assert!(!path.with_extension("hex.tmp").exists());
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
