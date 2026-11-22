//! SQLite persistence.
//!
//! One database file, WAL mode, migrations embedded at compile time. Workers, shares,
//! hashrate samples, and blocks survive a restart; jobs and sessions do not.

#![forbid(unsafe_code)]

mod accounting;
mod blocks;
mod retention;
mod settings;

pub use accounting::{AuxPayoutRecord, HashrateSample, NewShare, ShareRow, WorkerRow, WorkerWrite};
pub use blocks::{BlockRow, BlockStatus, CoinRounds, NewBlock};
pub use retention::{RetentionPolicy, RetentionReport};

use sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions, SqliteSynchronous};
use sqlx::SqlitePool;
use std::path::{Path, PathBuf};

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
    path: PathBuf,
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
        Ok(Self {
            pool,
            path: path.to_path_buf(),
        })
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
        assert!(store.load_workers("ltc").await.unwrap().is_empty());
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    /// The migrations before lanes, applied by hand to build a database as an older release
    /// left it.
    const PRE_LANES: [&str; 7] = [
        include_str!("../migrations/0001_init.sql"),
        include_str!("../migrations/0002_worker_payouts.sql"),
        include_str!("../migrations/0003_hashrate_samples_worker_index.sql"),
        include_str!("../migrations/0004_work_and_rounds.sql"),
        include_str!("../migrations/0005_retired_work.sql"),
        include_str!("../migrations/0006_settings.sql"),
        include_str!("../migrations/0007_best_at.sql"),
    ];
    const LANES: &str = include_str!("../migrations/0008_lanes.sql");

    #[tokio::test]
    async fn lanes_migration_keeps_every_row_and_assigns_ltc() {
        use sqlx::{Connection, SqliteConnection};
        let mut db = SqliteConnection::connect("sqlite::memory:").await.unwrap();
        for sql in PRE_LANES {
            sqlx::raw_sql(sql).execute(&mut db).await.unwrap();
        }
        sqlx::raw_sql(
            r#"
            INSERT INTO workers (name, payout_address, aux_address, first_seen, last_seen,
                                 shares_accepted, shares_rejected, best_difficulty,
                                 aux_payouts, fallback, work_accepted, best_at)
            VALUES ('rig1', 'ltc1qa', 'Da', 100, 200, 7, 2, 512.5,
                    '[{"coin":"DOGE","address":"Da","fallback":false}]', 0, 96.0, 150),
                   ('rig2', 'ltc1qb', NULL, 110, 210, 0, 1, 0, '[]', 1, 0, NULL);
            INSERT INTO shares (ts, worker, difficulty, share_diff, accepted, reject_reason)
            VALUES (150, 'rig1', 16, 512.5, 1, NULL),
                   (160, 'rig2', 16, 0, 0, 'low_difficulty');
            INSERT INTO hashrate_samples (ts, worker, hashrate)
            VALUES (60, '', 100.0), (60, 'rig1', 60.0), (120, 'rig1', 70.0);
            INSERT INTO blocks (coin, height, hash, worker, difficulty, share_diff,
                                reward_sats, found_at, status, confirmations, work_at_found)
            VALUES ('LTC', 10, 'aa', 'rig1', 1.5, 2.5, 625000000, 150, 'confirmed', 120, 80.0),
                   ('DOGE', 20, 'bb', 'rig1', 1.0, 2.5, NULL, 150, 'accepted', 3, NULL);
            UPDATE pool_counters SET value = 42.0 WHERE key = 'retired_work';
            INSERT INTO settings (key, value, updated_at) VALUES ('vardiff', '{}', 5);
            "#,
        )
        .execute(&mut db)
        .await
        .unwrap();

        sqlx::raw_sql(LANES).execute(&mut db).await.unwrap();

        // Split in two: tuples past twelve fields have no Debug or PartialEq.
        let ids: Vec<(String, String, String, Option<String>, i64, i64)> = sqlx::query_as(
            "SELECT lane, name, payout_address, aux_address, first_seen, last_seen
             FROM workers ORDER BY name",
        )
        .fetch_all(&mut db)
        .await
        .unwrap();
        assert_eq!(
            ids,
            vec![
                (
                    "ltc".into(),
                    "rig1".into(),
                    "ltc1qa".into(),
                    Some("Da".into()),
                    100,
                    200
                ),
                ("ltc".into(), "rig2".into(), "ltc1qb".into(), None, 110, 210),
            ]
        );
        type Counts = (i64, i64, f64, String, i64, f64, Option<i64>);
        let counts: Vec<Counts> = sqlx::query_as(
            "SELECT shares_accepted, shares_rejected, best_difficulty, aux_payouts, fallback,
                    work_accepted, best_at
             FROM workers ORDER BY name",
        )
        .fetch_all(&mut db)
        .await
        .unwrap();
        assert_eq!(
            counts,
            vec![
                (
                    7,
                    2,
                    512.5,
                    r#"[{"coin":"DOGE","address":"Da","fallback":false}]"#.into(),
                    0,
                    96.0,
                    Some(150),
                ),
                (0, 1, 0.0, "[]".into(), 1, 0.0, None),
            ]
        );

        type Share = (String, i64, String, f64, f64, i64, Option<String>);
        let shares: Vec<Share> = sqlx::query_as(
            "SELECT lane, ts, worker, difficulty, share_diff, accepted, reject_reason
             FROM shares ORDER BY ts",
        )
        .fetch_all(&mut db)
        .await
        .unwrap();
        assert_eq!(
            shares,
            vec![
                ("ltc".into(), 150, "rig1".into(), 16.0, 512.5, 1, None),
                (
                    "ltc".into(),
                    160,
                    "rig2".into(),
                    16.0,
                    0.0,
                    0,
                    Some("low_difficulty".into())
                ),
            ]
        );

        let samples: Vec<(i64, String, String, f64)> = sqlx::query_as(
            "SELECT ts, lane, worker, hashrate FROM hashrate_samples ORDER BY ts, worker",
        )
        .fetch_all(&mut db)
        .await
        .unwrap();
        assert_eq!(
            samples,
            vec![
                (60, "ltc".into(), String::new(), 100.0),
                (60, "ltc".into(), "rig1".into(), 60.0),
                (120, "ltc".into(), "rig1".into(), 70.0),
            ]
        );

        let blocks: Vec<BlockRow> = sqlx::query_as("SELECT * FROM blocks ORDER BY id")
            .fetch_all(&mut db)
            .await
            .unwrap();
        assert_eq!(blocks.len(), 2);
        assert!(blocks.iter().all(|b| b.lane == "ltc"));
        assert_eq!(
            (
                blocks[0].coin.as_str(),
                blocks[0].status,
                blocks[0].confirmations
            ),
            ("LTC", BlockStatus::Confirmed, 120)
        );
        assert_eq!(blocks[0].reward_sats, Some(625_000_000));
        assert_eq!(blocks[0].work_at_found, Some(80.0));
        assert_eq!(
            (blocks[1].coin.as_str(), blocks[1].work_at_found),
            ("DOGE", None)
        );

        let counters: Vec<(String, f64)> =
            sqlx::query_as("SELECT key, value FROM pool_counters ORDER BY key")
                .fetch_all(&mut db)
                .await
                .unwrap();
        assert_eq!(counters, vec![("retired_work:ltc".into(), 42.0)]);

        let (settings,): (i64,) = sqlx::query_as("SELECT COUNT(*) FROM settings")
            .fetch_one(&mut db)
            .await
            .unwrap();
        assert_eq!(settings, 1, "settings are untouched");

        // The new keys: a worker name and a (ts, worker) sample may repeat across lanes,
        // but not within one.
        sqlx::query(
            "INSERT INTO workers (lane, name, payout_address, first_seen, last_seen)
             VALUES ('btc', 'rig1', 'bc1q', 1, 1)",
        )
        .execute(&mut db)
        .await
        .unwrap();
        assert!(sqlx::query(
            "INSERT INTO workers (lane, name, payout_address, first_seen, last_seen)
             VALUES ('ltc', 'rig1', 'ltc1q', 1, 1)",
        )
        .execute(&mut db)
        .await
        .is_err());
        sqlx::query(
            "INSERT INTO hashrate_samples (ts, lane, worker, hashrate)
             VALUES (60, 'btc', 'rig1', 1.0)",
        )
        .execute(&mut db)
        .await
        .unwrap();
        assert!(sqlx::query(
            "INSERT INTO hashrate_samples (ts, lane, worker, hashrate)
             VALUES (60, 'ltc', 'rig1', 1.0)",
        )
        .execute(&mut db)
        .await
        .is_err());

        // The indexes the lane-scoped reads rely on exist.
        let indexes: Vec<(String,)> = sqlx::query_as(
            "SELECT name FROM sqlite_master WHERE type = 'index' AND name IN
                ('shares_lane_ts', 'hashrate_samples_lane_worker_ts') ORDER BY name",
        )
        .fetch_all(&mut db)
        .await
        .unwrap();
        assert_eq!(indexes.len(), 2);
    }

    #[tokio::test]
    async fn fresh_database_starts_ltc_retired_work_at_zero() {
        let path = temp_path("fresh-lanes");
        let store = Store::open(&path).await.unwrap();
        let counters: Vec<(String, f64)> = sqlx::query_as("SELECT key, value FROM pool_counters")
            .fetch_all(store.pool())
            .await
            .unwrap();
        assert_eq!(counters, vec![("retired_work:ltc".into(), 0.0)]);
        assert_eq!(store.retired_work("btc").await.unwrap(), 0.0);
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }
}
