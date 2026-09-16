//! Blocks the pool found.

use crate::{Store, StoreError};
use serde::Serialize;

/// Lifecycle of a block the pool found.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, sqlx::Type)]
#[serde(rename_all = "lowercase")]
#[sqlx(rename_all = "lowercase")]
pub enum BlockStatus {
    /// The node accepted the block; waiting for maturity.
    Accepted,
    /// The node rejected the block.
    Rejected,
    /// Matured on the active chain.
    Confirmed,
    /// Fell off the active chain.
    Orphaned,
}

/// A block the pool found, as stored.
#[derive(Clone, Debug, Serialize, sqlx::FromRow)]
pub struct BlockRow {
    /// Row id.
    pub id: i64,
    /// Coin ticker.
    pub coin: String,
    /// Block height.
    pub height: i64,
    /// Block hash, display hex.
    pub hash: String,
    /// Worker that found it.
    pub worker: String,
    /// Network difficulty at the time.
    pub difficulty: f64,
    /// Difficulty the winning share achieved.
    pub share_diff: f64,
    /// Coinbase value in base units, if known.
    pub reward_sats: Option<i64>,
    /// Unix time found.
    pub found_at: i64,
    /// Lifecycle status.
    pub status: BlockStatus,
    /// Confirmations at last check.
    pub confirmations: i64,
    /// Pool-wide accepted work (difficulty units) when the block was found, if recorded.
    pub work_at_found: Option<f64>,
}

/// Per-coin round and luck figures derived from the blocks table.
#[derive(Clone, Debug, Default, PartialEq, sqlx::FromRow)]
pub struct CoinRounds {
    /// Coin ticker.
    pub coin: String,
    /// Blocks the node accepted (whether or not they later matured or were orphaned).
    pub blocks_found: i64,
    /// Sum of network difficulty over those blocks: the work they were expected to take.
    pub expected_work: f64,
    /// When the most recent one was found.
    pub last_found_at: Option<i64>,
    /// Pool-wide work when the most recent one was found; the current round starts here.
    pub last_work_at_found: Option<f64>,
}

/// A block to record.
#[derive(Clone, Debug)]
pub struct NewBlock {
    /// Coin ticker.
    pub coin: String,
    /// Block height.
    pub height: u64,
    /// Block hash, display hex.
    pub hash: String,
    /// Worker that found it.
    pub worker: String,
    /// Network difficulty at the time.
    pub difficulty: f64,
    /// Difficulty the winning share achieved.
    pub share_diff: f64,
    /// Coinbase value in base units, if known.
    pub reward_sats: Option<i64>,
    /// Unix time found.
    pub found_at: u64,
    /// Initial status (`Accepted` or `Rejected`).
    pub status: BlockStatus,
}

impl Store {
    /// Number of blocks the pool has found across all coins.
    pub async fn block_count(&self) -> Result<i64, StoreError> {
        let (n,): (i64,) = sqlx::query_as("SELECT COUNT(*) FROM blocks")
            .fetch_one(&self.pool)
            .await?;
        Ok(n)
    }

    /// Record a found block, stamping it with the pool's total accepted work so far:
    /// every worker's plus the work of workers since removed, in stratum share units as
    /// the worker rows keep it. Returns its row id.
    pub async fn insert_block(&self, block: &NewBlock) -> Result<i64, StoreError> {
        let result = sqlx::query(
            "INSERT INTO blocks (coin, height, hash, worker, difficulty, share_diff, reward_sats,
                                 found_at, status, work_at_found)
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?,
                     (SELECT COALESCE(SUM(work_accepted), 0.0) FROM workers)
                     + (SELECT COALESCE(SUM(value), 0.0) FROM pool_counters
                        WHERE key = 'retired_work'))
             ON CONFLICT (coin, hash) DO UPDATE SET status = excluded.status",
        )
        .bind(&block.coin)
        .bind(block.height as i64)
        .bind(&block.hash)
        .bind(&block.worker)
        .bind(block.difficulty)
        .bind(block.share_diff)
        .bind(block.reward_sats)
        .bind(block.found_at as i64)
        .bind(block.status)
        .execute(&self.pool)
        .await?;
        Ok(result.last_insert_rowid())
    }

    /// Most recent blocks, newest first.
    pub async fn recent_blocks(&self, limit: i64) -> Result<Vec<BlockRow>, StoreError> {
        Ok(
            sqlx::query_as("SELECT * FROM blocks ORDER BY found_at DESC, id DESC LIMIT ?")
                .bind(limit)
                .fetch_all(&self.pool)
                .await?,
        )
    }

    /// Blocks of one coin whose final status is not yet known.
    pub async fn unsettled_blocks(&self, coin: &str) -> Result<Vec<BlockRow>, StoreError> {
        Ok(sqlx::query_as(
            "SELECT * FROM blocks WHERE coin = ? AND status = 'accepted' ORDER BY height",
        )
        .bind(coin)
        .fetch_all(&self.pool)
        .await?)
    }

    /// Round and luck inputs for every coin that has found a block. Rejected blocks do not
    /// count: no work was banked and no round ended.
    pub async fn coin_rounds(&self) -> Result<Vec<CoinRounds>, StoreError> {
        Ok(sqlx::query_as(
            "SELECT b.coin AS coin,
                    COUNT(*) AS blocks_found,
                    SUM(b.difficulty) AS expected_work,
                    MAX(b.found_at) AS last_found_at,
                    (SELECT work_at_found FROM blocks l
                     WHERE l.coin = b.coin AND l.status != 'rejected'
                     ORDER BY l.found_at DESC, l.id DESC LIMIT 1) AS last_work_at_found
             FROM blocks b WHERE b.status != 'rejected'
             GROUP BY b.coin ORDER BY b.coin",
        )
        .fetch_all(&self.pool)
        .await?)
    }

    /// Update a block's status and confirmation count.
    pub async fn set_block_status(
        &self,
        id: i64,
        status: BlockStatus,
        confirmations: i64,
    ) -> Result<(), StoreError> {
        sqlx::query("UPDATE blocks SET status = ?, confirmations = ? WHERE id = ?")
            .bind(status)
            .bind(confirmations)
            .bind(id)
            .execute(&self.pool)
            .await?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{temp_path, Store};

    #[tokio::test]
    async fn block_lifecycle() {
        let path = temp_path("blocks");
        let store = Store::open(&path).await.unwrap();
        let id = store
            .insert_block(&NewBlock {
                coin: "LTC".into(),
                height: 10,
                hash: "ab".into(),
                worker: "w".into(),
                difficulty: 1.5,
                share_diff: 2.5,
                reward_sats: Some(1),
                found_at: 100,
                status: BlockStatus::Accepted,
            })
            .await
            .unwrap();
        assert_eq!(store.unsettled_blocks("LTC").await.unwrap().len(), 1);
        assert!(store.unsettled_blocks("DOGE").await.unwrap().is_empty());
        store
            .set_block_status(id, BlockStatus::Confirmed, 120)
            .await
            .unwrap();
        assert!(store.unsettled_blocks("LTC").await.unwrap().is_empty());
        let recent = store.recent_blocks(5).await.unwrap();
        assert_eq!(recent[0].status, BlockStatus::Confirmed);
        assert_eq!(recent[0].confirmations, 120);
        assert_eq!(recent[0].work_at_found, Some(0.0));
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[tokio::test]
    async fn rounds_bank_work_at_each_block_and_skip_rejected_ones() {
        use crate::NewShare;
        let path = temp_path("rounds");
        let store = Store::open(&path).await.unwrap();
        let share = |ts: i64, difficulty: f64| NewShare {
            ts,
            worker: "w".into(),
            difficulty,
            share_diff: difficulty,
            accepted: true,
            reject_reason: None,
        };
        let block = |hash: &str, found_at: u64, status: BlockStatus| NewBlock {
            coin: "LTC".into(),
            height: 1,
            hash: hash.into(),
            worker: "w".into(),
            difficulty: 100.0,
            share_diff: 150.0,
            reward_sats: Some(1),
            found_at,
            status,
        };
        assert!(store.coin_rounds().await.unwrap().is_empty());

        store.persist_batch(&[], &[share(1, 30.0)]).await.unwrap();
        store
            .insert_block(&block("a", 10, BlockStatus::Accepted))
            .await
            .unwrap();
        store
            .persist_batch(&[], &[share(11, 50.0), share(12, 20.0)])
            .await
            .unwrap();
        store
            .insert_block(&block("bad", 20, BlockStatus::Rejected))
            .await
            .unwrap();
        store
            .insert_block(&block("b", 30, BlockStatus::Accepted))
            .await
            .unwrap();
        store.persist_batch(&[], &[share(31, 5.0)]).await.unwrap();

        let rounds = store.coin_rounds().await.unwrap();
        assert_eq!(
            rounds,
            vec![CoinRounds {
                coin: "LTC".into(),
                blocks_found: 2,
                expected_work: 200.0,
                last_found_at: Some(30),
                last_work_at_found: Some(100.0),
            }]
        );
        // Current round: 105 total minus 100 banked at block "b".
        assert_eq!(store.total_work().await.unwrap() - 100.0, 5.0);

        // Work retired with a removed worker is banked at the next block as well.
        assert_eq!(store.remove_worker("w").await.unwrap(), Some(105.0));
        store
            .insert_block(&block("c", 40, BlockStatus::Accepted))
            .await
            .unwrap();
        let rounds = store.coin_rounds().await.unwrap();
        assert_eq!(rounds[0].last_work_at_found, Some(105.0));
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }
}
