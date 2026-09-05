//! Workers, shares, and hashrate samples.

use crate::{Store, StoreError};
use serde::{Deserialize, Serialize};

/// An aux-chain payout recorded for a worker.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AuxPayoutRecord {
    /// Ticker.
    pub coin: String,
    /// Address that will be paid.
    pub address: String,
    /// Whether the fallback address was substituted.
    pub fallback: bool,
}

/// A worker as stored.
#[derive(Clone, Debug, PartialEq)]
pub struct WorkerRow {
    /// Full stratum username.
    pub name: String,
    /// Parent-chain payout address.
    pub payout_address: String,
    /// Whether that address is the configured fallback.
    pub fallback: bool,
    /// Aux-chain payouts.
    pub aux_payouts: Vec<AuxPayoutRecord>,
    /// Unix time first authorized.
    pub first_seen: i64,
    /// Unix time of the last authorize or share.
    pub last_seen: i64,
    /// Lifetime accepted shares.
    pub shares_accepted: i64,
    /// Lifetime rejected shares.
    pub shares_rejected: i64,
    /// Best share difficulty seen.
    pub best_difficulty: f64,
    /// Lifetime accepted work: the sum of job difficulty over accepted shares.
    pub work_accepted: f64,
}

/// Fields written when a worker authorizes.
#[derive(Clone, Debug)]
pub struct WorkerWrite {
    /// Full stratum username.
    pub name: String,
    /// Parent-chain payout address.
    pub payout_address: String,
    /// Whether that address is the configured fallback.
    pub fallback: bool,
    /// Aux-chain payouts.
    pub aux_payouts: Vec<AuxPayoutRecord>,
    /// Unix time of the event.
    pub ts: i64,
}

/// A share to record.
#[derive(Clone, Debug)]
pub struct NewShare {
    /// Unix time received.
    pub ts: i64,
    /// Worker name.
    pub worker: String,
    /// Difficulty the job required.
    pub difficulty: f64,
    /// Difficulty the hash achieved (0 if it was never hashed).
    pub share_diff: f64,
    /// Whether the share was accepted.
    pub accepted: bool,
    /// Rejection slug, if rejected.
    pub reject_reason: Option<String>,
}

/// A stored share.
#[derive(Clone, Debug, PartialEq, Serialize, sqlx::FromRow)]
pub struct ShareRow {
    /// Row id.
    pub id: i64,
    /// Unix time received.
    pub ts: i64,
    /// Worker name.
    pub worker: String,
    /// Difficulty the job required.
    pub difficulty: f64,
    /// Difficulty the hash achieved.
    pub share_diff: f64,
    /// Whether the share was accepted.
    pub accepted: bool,
    /// Rejection slug, if rejected.
    pub reject_reason: Option<String>,
}

/// One hashrate sample.
#[derive(Clone, Debug, PartialEq, Serialize, sqlx::FromRow)]
pub struct HashrateSample {
    /// Unix time of the sample, aligned to the sample interval.
    pub ts: i64,
    /// Worker name; empty string is the pool total.
    pub worker: String,
    /// Hashes per second.
    pub hashrate: f64,
}

#[derive(sqlx::FromRow)]
struct WorkerSql {
    name: String,
    payout_address: String,
    aux_payouts: String,
    fallback: i64,
    first_seen: i64,
    last_seen: i64,
    shares_accepted: i64,
    shares_rejected: i64,
    best_difficulty: f64,
    work_accepted: f64,
}

impl From<WorkerSql> for WorkerRow {
    fn from(row: WorkerSql) -> Self {
        let aux_payouts = serde_json::from_str(&row.aux_payouts).unwrap_or_default();
        Self {
            name: row.name,
            payout_address: row.payout_address,
            fallback: row.fallback != 0,
            aux_payouts,
            first_seen: row.first_seen,
            last_seen: row.last_seen,
            shares_accepted: row.shares_accepted,
            shares_rejected: row.shares_rejected,
            best_difficulty: row.best_difficulty,
            work_accepted: row.work_accepted,
        }
    }
}

fn aux_json(aux: &[AuxPayoutRecord]) -> String {
    serde_json::to_string(aux).unwrap_or_else(|_| "[]".into())
}

fn aux_address(aux: &[AuxPayoutRecord]) -> Option<&str> {
    aux.first().map(|a| a.address.as_str())
}

impl Store {
    /// Persist a batch of authorizations and shares in one transaction.
    ///
    /// Authorizations are applied first so a share in the same batch sees the worker row.
    pub async fn persist_batch(
        &self,
        authorized: &[WorkerWrite],
        shares: &[NewShare],
    ) -> Result<(), StoreError> {
        if authorized.is_empty() && shares.is_empty() {
            return Ok(());
        }
        let mut tx = self.pool.begin().await?;
        for w in authorized {
            let aux = aux_json(&w.aux_payouts);
            sqlx::query(
                "INSERT INTO workers (
                    name, payout_address, aux_address, aux_payouts, fallback,
                    first_seen, last_seen, shares_accepted, shares_rejected, best_difficulty
                 ) VALUES (?, ?, ?, ?, ?, ?, ?, 0, 0, 0)
                 ON CONFLICT(name) DO UPDATE SET
                    payout_address = excluded.payout_address,
                    aux_address = excluded.aux_address,
                    aux_payouts = excluded.aux_payouts,
                    fallback = excluded.fallback,
                    last_seen = MAX(last_seen, excluded.last_seen)",
            )
            .bind(&w.name)
            .bind(&w.payout_address)
            .bind(aux_address(&w.aux_payouts))
            .bind(&aux)
            .bind(w.fallback as i64)
            .bind(w.ts)
            .bind(w.ts)
            .execute(&mut *tx)
            .await?;
        }
        for s in shares {
            sqlx::query(
                "INSERT INTO workers (
                    name, payout_address, aux_payouts, fallback, first_seen, last_seen
                 ) VALUES (?, '', '[]', 0, ?, ?)
                 ON CONFLICT(name) DO NOTHING",
            )
            .bind(&s.worker)
            .bind(s.ts)
            .bind(s.ts)
            .execute(&mut *tx)
            .await?;
            sqlx::query(
                "INSERT INTO shares (ts, worker, difficulty, share_diff, accepted, reject_reason)
                 VALUES (?, ?, ?, ?, ?, ?)",
            )
            .bind(s.ts)
            .bind(&s.worker)
            .bind(s.difficulty)
            .bind(s.share_diff)
            .bind(s.accepted as i64)
            .bind(&s.reject_reason)
            .execute(&mut *tx)
            .await?;
            let accepted = s.accepted as i64;
            let rejected = (!s.accepted) as i64;
            let best = if s.accepted { s.share_diff } else { 0.0 };
            let work = if s.accepted { s.difficulty } else { 0.0 };
            sqlx::query(
                "UPDATE workers SET
                    last_seen = MAX(last_seen, ?),
                    shares_accepted = shares_accepted + ?,
                    shares_rejected = shares_rejected + ?,
                    best_difficulty = MAX(best_difficulty, ?),
                    work_accepted = work_accepted + ?
                 WHERE name = ?",
            )
            .bind(s.ts)
            .bind(accepted)
            .bind(rejected)
            .bind(best)
            .bind(work)
            .bind(&s.worker)
            .execute(&mut *tx)
            .await?;
        }
        tx.commit().await?;
        Ok(())
    }

    /// Every worker the pool has seen, ordered by name.
    pub async fn load_workers(&self) -> Result<Vec<WorkerRow>, StoreError> {
        let rows: Vec<WorkerSql> = sqlx::query_as(
            "SELECT name, payout_address, aux_payouts, fallback, first_seen, last_seen,
                    shares_accepted, shares_rejected, best_difficulty, work_accepted
             FROM workers ORDER BY name",
        )
        .fetch_all(&self.pool)
        .await?;
        Ok(rows.into_iter().map(WorkerRow::from).collect())
    }

    /// Pool-wide accepted work in difficulty units, summed over every worker.
    pub async fn total_work(&self) -> Result<f64, StoreError> {
        let (work,): (f64,) =
            sqlx::query_as("SELECT COALESCE(SUM(work_accepted), 0.0) FROM workers")
                .fetch_one(&self.pool)
                .await?;
        Ok(work)
    }

    /// Accepted shares at or after `since`, oldest first, capped so a restart stays bounded.
    pub async fn accepted_shares_since(&self, since: i64) -> Result<Vec<ShareRow>, StoreError> {
        const CAP: i64 = 50_000;
        let mut rows: Vec<ShareRow> = sqlx::query_as(
            "SELECT id, ts, worker, difficulty, share_diff, accepted, reject_reason
             FROM shares
             WHERE accepted = 1 AND ts >= ?
             ORDER BY ts DESC, id DESC
             LIMIT ?",
        )
        .bind(since)
        .bind(CAP)
        .fetch_all(&self.pool)
        .await?;
        rows.reverse();
        Ok(rows)
    }

    /// Most recent shares, newest first, for the live log.
    pub async fn recent_shares(&self, limit: i64) -> Result<Vec<ShareRow>, StoreError> {
        Ok(sqlx::query_as(
            "SELECT id, ts, worker, difficulty, share_diff, accepted, reject_reason
             FROM shares
             ORDER BY ts DESC, id DESC
             LIMIT ?",
        )
        .bind(limit)
        .fetch_all(&self.pool)
        .await?)
    }

    /// Record aligned hashrate samples. Empty `worker` is the pool total.
    pub async fn insert_hashrate_samples(
        &self,
        samples: &[HashrateSample],
    ) -> Result<(), StoreError> {
        if samples.is_empty() {
            return Ok(());
        }
        let mut tx = self.pool.begin().await?;
        for s in samples {
            sqlx::query(
                "INSERT INTO hashrate_samples (ts, worker, hashrate) VALUES (?, ?, ?)
                 ON CONFLICT(ts, worker) DO UPDATE SET hashrate = excluded.hashrate",
            )
            .bind(s.ts)
            .bind(&s.worker)
            .bind(s.hashrate)
            .execute(&mut *tx)
            .await?;
        }
        tx.commit().await?;
        Ok(())
    }

    /// Samples at or after `since`, oldest first.
    pub async fn hashrate_samples_since(
        &self,
        worker: Option<&str>,
        since: i64,
    ) -> Result<Vec<HashrateSample>, StoreError> {
        match worker {
            Some(name) => Ok(sqlx::query_as(
                "SELECT ts, worker, hashrate FROM hashrate_samples
                 WHERE worker = ? AND ts >= ? ORDER BY ts",
            )
            .bind(name)
            .bind(since)
            .fetch_all(&self.pool)
            .await?),
            None => Ok(sqlx::query_as(
                "SELECT ts, worker, hashrate FROM hashrate_samples
                 WHERE ts >= ? ORDER BY ts, worker",
            )
            .bind(since)
            .fetch_all(&self.pool)
            .await?),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{temp_path, Store};

    fn worker(name: &str, ts: i64) -> WorkerWrite {
        WorkerWrite {
            name: name.into(),
            payout_address: "ltc1qtest".into(),
            fallback: false,
            aux_payouts: vec![AuxPayoutRecord {
                coin: "DOGE".into(),
                address: "Dtest".into(),
                fallback: true,
            }],
            ts,
        }
    }

    fn share(worker: &str, ts: i64, accepted: bool) -> NewShare {
        NewShare {
            ts,
            worker: worker.into(),
            difficulty: 16.0,
            share_diff: if accepted { 32.0 } else { 0.0 },
            accepted,
            reject_reason: (!accepted).then(|| "low_difficulty".into()),
        }
    }

    #[tokio::test]
    async fn accounting_survives_reopen() {
        let path = temp_path("accounting");
        let store = Store::open(&path).await.unwrap();
        store
            .persist_batch(
                &[worker("rig1", 1000)],
                &[share("rig1", 1010, true), share("rig1", 1020, false)],
            )
            .await
            .unwrap();
        drop(store);

        let store = Store::open(&path).await.unwrap();
        let workers = store.load_workers().await.unwrap();
        assert_eq!(workers.len(), 1);
        assert_eq!(workers[0].name, "rig1");
        assert_eq!(workers[0].payout_address, "ltc1qtest");
        assert!(!workers[0].fallback);
        assert_eq!(workers[0].aux_payouts[0].coin, "DOGE");
        assert!(workers[0].aux_payouts[0].fallback);
        assert_eq!(workers[0].first_seen, 1000);
        assert_eq!(workers[0].last_seen, 1020);
        assert_eq!(workers[0].shares_accepted, 1);
        assert_eq!(workers[0].shares_rejected, 1);
        assert_eq!(workers[0].best_difficulty, 32.0);
        assert_eq!(workers[0].work_accepted, 16.0);
        assert_eq!(store.total_work().await.unwrap(), 16.0);

        let accepted = store.accepted_shares_since(0).await.unwrap();
        assert!(accepted[0].accepted);
        assert_eq!(accepted.len(), 1);
        assert_eq!(accepted[0].difficulty, 16.0);
        let recent = store.recent_shares(10).await.unwrap();
        assert_eq!(recent.len(), 2);
        assert!(recent[0].ts >= recent[1].ts);
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[tokio::test]
    async fn share_without_authorize_creates_a_stub_worker() {
        let path = temp_path("stub");
        let store = Store::open(&path).await.unwrap();
        store
            .persist_batch(&[], &[share("ghost", 5, true)])
            .await
            .unwrap();
        let workers = store.load_workers().await.unwrap();
        assert_eq!(workers[0].name, "ghost");
        assert_eq!(workers[0].shares_accepted, 1);
        assert_eq!(workers[0].payout_address, "");
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[tokio::test]
    async fn hashrate_samples_round_trip() {
        let path = temp_path("samples");
        let store = Store::open(&path).await.unwrap();
        store
            .insert_hashrate_samples(&[
                HashrateSample {
                    ts: 60,
                    worker: String::new(),
                    hashrate: 100.0,
                },
                HashrateSample {
                    ts: 60,
                    worker: "rig1".into(),
                    hashrate: 40.0,
                },
            ])
            .await
            .unwrap();
        store
            .insert_hashrate_samples(&[HashrateSample {
                ts: 60,
                worker: "rig1".into(),
                hashrate: 50.0,
            }])
            .await
            .unwrap();
        let pool = store.hashrate_samples_since(Some(""), 0).await.unwrap();
        assert_eq!(pool[0].hashrate, 100.0);
        let rig = store.hashrate_samples_since(Some("rig1"), 0).await.unwrap();
        assert_eq!(rig[0].hashrate, 50.0);
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }
}
