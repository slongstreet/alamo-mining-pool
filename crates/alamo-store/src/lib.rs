//! SQLite persistence.
//!
//! One database file, WAL mode, migrations embedded at compile time. Workers, shares,
//! hashrate samples, and blocks survive a restart; jobs and sessions do not.

#![forbid(unsafe_code)]

mod accounting;
mod blocks;
mod retention;

pub use accounting::{AuxPayoutRecord, HashrateSample, NewShare, ShareRow, WorkerRow, WorkerWrite};
pub use blocks::{BlockRow, BlockStatus, NewBlock};
pub use retention::{RetentionPolicy, RetentionReport};

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
}

#[cfg(test)]
pub(crate) fn temp_path(tag: &str) -> std::path::PathBuf {
    std::env::temp_dir()
        .join(format!(
            "alamo-store-{tag}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ))
        .join("pool.db")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn opens_and_migrates_fresh_database() {
        let path = temp_path("open");
        let store = Store::open(&path).await.unwrap();
        assert_eq!(store.block_count().await.unwrap(), 0);
        drop(store);
        let store = Store::open(&path).await.unwrap();
        assert_eq!(store.block_count().await.unwrap(), 0);
        assert!(store.load_workers().await.unwrap().is_empty());
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }
}
