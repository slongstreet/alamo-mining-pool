//! Per-coin round accounting: work submitted since the last block found on that coin.

use crate::{Store, StoreError};
use serde::Serialize;

/// A round as stored.
#[derive(Clone, Debug, PartialEq, Serialize, sqlx::FromRow)]
pub struct RoundRow {
    /// Coin ticker.
    pub coin: String,
    /// Unix time the round started (daemon first start or last block found).
    pub started_at: i64,
    /// Accepted work this round in difficulty-1 shares (2^32 hashes each).
    pub work: f64,
    /// Accepted shares this round.
    pub shares: i64,
    /// Best share difficulty this round.
    pub best_share: f64,
}

impl Store {
    /// Create the round row for `coin` if it does not exist yet.
    pub async fn ensure_round(&self, coin: &str, now: i64) -> Result<(), StoreError> {
        sqlx::query("INSERT INTO rounds (coin, started_at) VALUES (?, ?) ON CONFLICT DO NOTHING")
            .bind(coin)
            .bind(now)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    /// Start a fresh round for `coin` after a block was found.
    pub async fn reset_round(&self, coin: &str, now: i64) -> Result<(), StoreError> {
        sqlx::query(
            "INSERT INTO rounds (coin, started_at) VALUES (?, ?)
             ON CONFLICT(coin) DO UPDATE SET
                started_at = excluded.started_at, work = 0, shares = 0, best_share = 0",
        )
        .bind(coin)
        .bind(now)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// Every round, ordered by coin.
    pub async fn rounds(&self) -> Result<Vec<RoundRow>, StoreError> {
        Ok(sqlx::query_as(
            "SELECT coin, started_at, work, shares, best_share FROM rounds ORDER BY coin",
        )
        .fetch_all(&self.pool)
        .await?)
    }
}

#[cfg(test)]
mod tests {
    use crate::{temp_path, NewShare, Store};

    #[tokio::test]
    async fn rounds_accumulate_and_reset() {
        let path = temp_path("rounds");
        let store = Store::open(&path).await.unwrap();
        store.ensure_round("LTC", 100).await.unwrap();
        store.ensure_round("DOGE", 100).await.unwrap();
        store.ensure_round("LTC", 999).await.unwrap();
        let share = |accepted: bool, diff: f64, best: f64| NewShare {
            ts: 200,
            worker: "w".into(),
            difficulty: diff,
            share_diff: best,
            accepted,
            reject_reason: None,
        };
        store
            .persist_batch(
                &[],
                &[
                    share(true, 8.0, 9.0),
                    share(true, 8.0, 40.0),
                    share(false, 8.0, 0.0),
                ],
            )
            .await
            .unwrap();
        let rounds = store.rounds().await.unwrap();
        assert_eq!(rounds.len(), 2);
        for r in &rounds {
            assert_eq!(r.started_at, 100);
            assert_eq!(r.work, 16.0);
            assert_eq!(r.shares, 2);
            assert_eq!(r.best_share, 40.0);
        }
        store.reset_round("DOGE", 300).await.unwrap();
        let rounds = store.rounds().await.unwrap();
        let doge = rounds.iter().find(|r| r.coin == "DOGE").unwrap();
        assert_eq!((doge.started_at, doge.work, doge.shares), (300, 0.0, 0));
        let ltc = rounds.iter().find(|r| r.coin == "LTC").unwrap();
        assert_eq!(ltc.work, 16.0);
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }
}
