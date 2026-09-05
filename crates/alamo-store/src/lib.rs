//! SQLite persistence.
//!
//! One database file, WAL mode, migrations embedded at compile time.

#![forbid(unsafe_code)]

use serde::Serialize;
use sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions, SqliteSynchronous};
use sqlx::SqlitePool;
use std::path::Path;

/// Storage errors.
#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    /// Database error.
    #[error(transparent)]
    Sqlx(#[from] sqlx::Error),
    /// Migration error.
    #[error(transparent)]
    Migrate(#[from] sqlx::migrate::MigrateError),
    /// Filesystem error creating the data directory.
    #[error("could not create data directory: {0}")]
    Io(#[from] std::io::Error),
}

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

/// Handle to the pool database.
#[derive(Clone, Debug)]
pub struct Store {
    pool: SqlitePool,
}

impl Store {
    /// Open (creating if needed) the database at `path` and apply migrations.
    pub async fn open(path: &Path) -> Result<Self, StoreError> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let options = SqliteConnectOptions::new()
            .filename(path)
            .create_if_missing(true)
            .journal_mode(SqliteJournalMode::Wal)
            .synchronous(SqliteSynchronous::Normal)
            .foreign_keys(true);
        let pool = SqlitePoolOptions::new()
            .max_connections(4)
            .connect_with(options)
            .await?;
        sqlx::migrate!("./migrations").run(&pool).await?;
        tracing::info!(path = %path.display(), "database ready");
        Ok(Self { pool })
    }

    /// The underlying connection pool.
    pub fn pool(&self) -> &SqlitePool {
        &self.pool
    }

    /// Number of blocks the pool has found across all coins.
    pub async fn block_count(&self) -> Result<i64, StoreError> {
        let (n,): (i64,) = sqlx::query_as("SELECT COUNT(*) FROM blocks")
            .fetch_one(&self.pool)
            .await?;
        Ok(n)
    }

    /// Record a found block. Returns its row id.
    pub async fn insert_block(&self, block: &NewBlock) -> Result<i64, StoreError> {
        let result = sqlx::query(
            "INSERT INTO blocks (coin, height, hash, worker, difficulty, share_diff, reward_sats, found_at, status)
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)
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

    fn temp_path(tag: &str) -> std::path::PathBuf {
        std::env::temp_dir()
            .join(format!("alamo-store-{tag}-{}", std::process::id()))
            .join("pool.db")
    }

    #[tokio::test]
    async fn opens_and_migrates_fresh_database() {
        let path = temp_path("open");
        let store = Store::open(&path).await.unwrap();
        assert_eq!(store.block_count().await.unwrap(), 0);
        drop(store);
        let store = Store::open(&path).await.unwrap();
        assert_eq!(store.block_count().await.unwrap(), 0);
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

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
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }
}
