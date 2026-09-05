//! SQLite persistence.
//!
//! One database file, WAL mode, migrations embedded at compile time.

#![forbid(unsafe_code)]

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

    /// Number of blocks the pool has found across all coins.
    pub async fn block_count(&self) -> Result<i64, StoreError> {
        let (n,): (i64,) = sqlx::query_as("SELECT COUNT(*) FROM blocks")
            .fetch_one(&self.pool)
            .await?;
        Ok(n)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn opens_and_migrates_fresh_database() {
        let dir = std::env::temp_dir().join(format!("alamo-store-test-{}", std::process::id()));
        let path = dir.join("nested").join("pool.db");
        let store = Store::open(&path).await.unwrap();
        assert_eq!(store.block_count().await.unwrap(), 0);
        // Reopening applies no new migrations and keeps working.
        drop(store);
        let store = Store::open(&path).await.unwrap();
        assert_eq!(store.block_count().await.unwrap(), 0);
        let _ = std::fs::remove_dir_all(dir);
    }
}
