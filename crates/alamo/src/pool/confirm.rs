//! Confirmation tracking: re-checking accepted blocks against their chain.

use alamo_coins::{Chain, Coin, RpcClient};
use alamo_store::{BlockStatus, Store};
use std::sync::Arc;
use std::time::Duration;
use tokio_util::sync::CancellationToken;

/// Re-check one coin's accepted blocks against its chain and update their status.
pub async fn track_confirmations(
    rpc: &RpcClient,
    coin: &dyn Coin,
    chain: Chain,
    store: &Store,
) -> anyhow::Result<()> {
    for block in store.unsettled_blocks(coin.symbol()).await? {
        let info = match rpc.get_block_info(&block.hash).await {
            Ok(info) => info,
            Err(err) => {
                tracing::warn!(coin = coin.symbol(), hash = %block.hash, %err, "could not check block");
                continue;
            }
        };
        let status = if info.confirmations < 0 {
            tracing::warn!(coin = coin.symbol(), height = block.height, hash = %block.hash, "block orphaned");
            BlockStatus::Orphaned
        } else if info.confirmations >= coin.coinbase_maturity(chain) {
            BlockStatus::Confirmed
        } else {
            BlockStatus::Accepted
        };
        store
            .set_block_status(block.id, status, info.confirmations)
            .await?;
    }
    Ok(())
}

pub(super) async fn tracker(
    rpc: RpcClient,
    coin: Arc<dyn Coin>,
    chain: Chain,
    store: Store,
    shutdown: CancellationToken,
) -> &'static str {
    let mut interval = tokio::time::interval(Duration::from_secs(30));
    loop {
        tokio::select! {
            _ = interval.tick() => {
                if let Err(err) = track_confirmations(&rpc, coin.as_ref(), chain, &store).await {
                    tracing::warn!(coin = coin.symbol(), %err, "confirmation tracking failed");
                }
            }
            _ = shutdown.cancelled() => return "tracker",
        }
    }
}
