//! Workers, shares, and hashrate samples.

use crate::{Store, StoreError};
use serde::{Deserialize, Serialize};
use std::sync::Arc;

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
    /// Unix time the best share arrived; `None` before the first accepted share.
    pub best_at: Option<i64>,
    /// Lifetime accepted work: the sum of job difficulty over accepted shares.
    pub work_accepted: f64,
    /// Display name the operator gave the worker, if any.
    pub alias: Option<String>,
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
    /// Worker name, shared with the stratum session that sent the share.
    pub worker: Arc<str>,
    /// Difficulty the job required.
    pub difficulty: f64,
    /// Difficulty the hash achieved (0 if it was never hashed).
    pub share_diff: f64,
    /// Whether the share was accepted.
    pub accepted: bool,
    /// Rejection slug, if rejected (`RejectReason::as_str`).
    pub reject_reason: Option<&'static str>,
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
    best_at: Option<i64>,
    work_accepted: f64,
    alias: Option<String>,
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
            best_at: row.best_at,
            work_accepted: row.work_accepted,
            alias: row.alias,
        }
    }
}

fn aux_json(aux: &[AuxPayoutRecord]) -> String {
    serde_json::to_string(aux).unwrap_or_else(|_| "[]".into())
}

fn aux_address(aux: &[AuxPayoutRecord]) -> Option<&str> {
    aux.first().map(|a| a.address.as_str())
}

/// One worker's share counters accumulated over a batch, applied with one UPDATE.
struct WorkerDelta<'a> {
    name: &'a str,
    /// Time of the worker's first share in the batch: what a stub row is created with.
    first_ts: i64,
    /// Newest share time.
    last_ts: i64,
    accepted: i64,
    rejected: i64,
    /// Best share in the batch (rejected shares count as 0) and when it first arrived.
    best: f64,
    best_at: i64,
    work: f64,
}

impl<'a> WorkerDelta<'a> {
    fn new(share: &'a NewShare) -> Self {
        Self {
            name: &share.worker,
            first_ts: share.ts,
            last_ts: share.ts,
            accepted: 0,
            rejected: 0,
            best: share_best(share),
            best_at: share.ts,
            work: 0.0,
        }
    }

    fn add(&mut self, share: &NewShare) {
        self.last_ts = self.last_ts.max(share.ts);
        if share.accepted {
            self.accepted += 1;
            self.work += share.difficulty;
        } else {
            self.rejected += 1;
        }
        // Strictly greater, so a tie keeps the earlier share's time, as the per-share
        // `CASE WHEN best > best_difficulty` did.
        let best = share_best(share);
        if best > self.best {
            self.best = best;
            self.best_at = share.ts;
        }
    }
}

/// What a share contributes to its worker's best difficulty.
fn share_best(share: &NewShare) -> f64 {
    if share.accepted {
        share.share_diff
    } else {
        0.0
    }
}

/// Fold a batch's shares into one delta per distinct worker, in first-seen order.
fn worker_deltas(shares: &[NewShare]) -> Vec<WorkerDelta<'_>> {
    let mut deltas: Vec<WorkerDelta<'_>> = Vec::new();
    for share in shares {
        // Batches come from few workers, often in runs: check the last one first.
        let i = match deltas.last() {
            Some(d) if d.name == &*share.worker => deltas.len() - 1,
            _ => match deltas.iter().position(|d| d.name == &*share.worker) {
                Some(i) => i,
                None => {
                    deltas.push(WorkerDelta::new(share));
                    deltas.len() - 1
                }
            },
        };
        deltas[i].add(share);
    }
    deltas
}

/// The `pool_counters` key holding a lane's retired work.
pub(crate) fn retired_work_key(lane: &str) -> String {
    format!("retired_work:{lane}")
}

impl Store {
    /// Persist a batch of one lane's authorizations and shares in one transaction.
    ///
    /// Authorizations are applied first so a share in the same batch sees the worker row.
    pub async fn persist_batch(
        &self,
        lane: &str,
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
                    lane, name, payout_address, aux_address, aux_payouts, fallback,
                    first_seen, last_seen, shares_accepted, shares_rejected, best_difficulty
                 ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, 0, 0, 0)
                 ON CONFLICT(lane, name) DO UPDATE SET
                    payout_address = excluded.payout_address,
                    aux_address = excluded.aux_address,
                    aux_payouts = excluded.aux_payouts,
                    fallback = excluded.fallback,
                    last_seen = MAX(last_seen, excluded.last_seen)",
            )
            .bind(lane)
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
        // Worker rows are created and updated once per distinct worker rather than once
        // per share. The result matches applying each share in turn: counts and work add
        // up, last_seen and best_difficulty take the maximum, and best_at is the time of
        // the first share reaching the batch's best, if that beats the stored best.
        let deltas = worker_deltas(shares);
        for d in &deltas {
            sqlx::query(
                "INSERT INTO workers (
                    lane, name, payout_address, aux_payouts, fallback, first_seen, last_seen
                 ) VALUES (?, ?, '', '[]', 0, ?, ?)
                 ON CONFLICT(lane, name) DO NOTHING",
            )
            .bind(lane)
            .bind(d.name)
            .bind(d.first_ts)
            .bind(d.first_ts)
            .execute(&mut *tx)
            .await?;
        }
        for s in shares {
            sqlx::query(
                "INSERT INTO shares (
                    lane, ts, worker, difficulty, share_diff, accepted, reject_reason
                 ) VALUES (?, ?, ?, ?, ?, ?, ?)",
            )
            .bind(lane)
            .bind(s.ts)
            .bind(&*s.worker)
            .bind(s.difficulty)
            .bind(s.share_diff)
            .bind(s.accepted as i64)
            .bind(s.reject_reason)
            .execute(&mut *tx)
            .await?;
        }
        for d in &deltas {
            sqlx::query(
                "UPDATE workers SET
                    last_seen = MAX(last_seen, ?),
                    shares_accepted = shares_accepted + ?,
                    shares_rejected = shares_rejected + ?,
                    best_at = CASE WHEN ? > best_difficulty THEN ? ELSE best_at END,
                    best_difficulty = MAX(best_difficulty, ?),
                    work_accepted = work_accepted + ?
                 WHERE lane = ? AND name = ?",
            )
            .bind(d.last_ts)
            .bind(d.accepted)
            .bind(d.rejected)
            .bind(d.best)
            .bind(d.best_at)
            .bind(d.best)
            .bind(d.work)
            .bind(lane)
            .bind(d.name)
            .execute(&mut *tx)
            .await?;
        }
        tx.commit().await?;
        Ok(())
    }

    /// Zero the accepted/rejected counts and best share of every worker in `lane`. Share
    /// history, accepted work, and blocks are kept.
    pub async fn reset_share_counters(&self, lane: &str) -> Result<(), StoreError> {
        sqlx::query(
            "UPDATE workers SET
                shares_accepted = 0, shares_rejected = 0, best_difficulty = 0, best_at = NULL
             WHERE lane = ?",
        )
        .bind(lane)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// Forget a worker in `lane`: its row, share log entries, and hashrate samples. Its
    /// accepted work is banked in the lane's retired work so the lane's totals and rounds do
    /// not move; blocks it found are kept. Returns the work retired, or `None` if the lane
    /// had no such worker.
    pub async fn remove_worker(&self, lane: &str, name: &str) -> Result<Option<f64>, StoreError> {
        let mut tx = self.pool.begin().await?;
        let row: Option<(f64,)> =
            sqlx::query_as("SELECT work_accepted FROM workers WHERE lane = ? AND name = ?")
                .bind(lane)
                .bind(name)
                .fetch_optional(&mut *tx)
                .await?;
        let Some((work,)) = row else {
            return Ok(None);
        };
        sqlx::query(
            "INSERT INTO pool_counters (key, value) VALUES (?, ?)
             ON CONFLICT(key) DO UPDATE SET value = value + excluded.value",
        )
        .bind(retired_work_key(lane))
        .bind(work)
        .execute(&mut *tx)
        .await?;
        for sql in [
            "DELETE FROM workers WHERE lane = ? AND name = ?",
            "DELETE FROM shares WHERE lane = ? AND worker = ?",
            "DELETE FROM hashrate_samples WHERE lane = ? AND worker = ?",
        ] {
            sqlx::query(sql)
                .bind(lane)
                .bind(name)
                .execute(&mut *tx)
                .await?;
        }
        tx.commit().await?;
        Ok(Some(work))
    }

    /// Set or clear (`None`) the display alias of a worker in `lane`. Returns false when the
    /// lane has no such worker. The caller validates the text.
    pub async fn set_worker_alias(
        &self,
        lane: &str,
        name: &str,
        alias: Option<&str>,
    ) -> Result<bool, StoreError> {
        let done = sqlx::query("UPDATE workers SET alias = ? WHERE lane = ? AND name = ?")
            .bind(alias)
            .bind(lane)
            .bind(name)
            .execute(&self.pool)
            .await?;
        Ok(done.rows_affected() > 0)
    }

    /// Accepted work of workers removed from `lane`, in difficulty units. Zero for a lane
    /// that has never removed one.
    pub async fn retired_work(&self, lane: &str) -> Result<f64, StoreError> {
        let row: Option<(f64,)> = sqlx::query_as("SELECT value FROM pool_counters WHERE key = ?")
            .bind(retired_work_key(lane))
            .fetch_optional(&self.pool)
            .await?;
        Ok(row.map_or(0.0, |(work,)| work))
    }

    /// Every worker `lane` has seen, ordered by name.
    pub async fn load_workers(&self, lane: &str) -> Result<Vec<WorkerRow>, StoreError> {
        let rows: Vec<WorkerSql> = sqlx::query_as(
            "SELECT name, payout_address, aux_payouts, fallback, first_seen, last_seen,
                    shares_accepted, shares_rejected, best_difficulty, best_at, work_accepted,
                    alias
             FROM workers WHERE lane = ? ORDER BY name",
        )
        .bind(lane)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows.into_iter().map(WorkerRow::from).collect())
    }

    /// A lane's accepted work in difficulty units: every worker's plus the work of
    /// workers since removed.
    pub async fn total_work(&self, lane: &str) -> Result<f64, StoreError> {
        let (work,): (f64,) =
            sqlx::query_as("SELECT COALESCE(SUM(work_accepted), 0.0) FROM workers WHERE lane = ?")
                .bind(lane)
                .fetch_one(&self.pool)
                .await?;
        Ok(work + self.retired_work(lane).await?)
    }

    /// A lane's accepted shares at or after `since`, oldest first, capped so a restart
    /// stays bounded.
    pub async fn accepted_shares_since(
        &self,
        lane: &str,
        since: i64,
    ) -> Result<Vec<ShareRow>, StoreError> {
        const CAP: i64 = 50_000;
        let mut rows: Vec<ShareRow> = sqlx::query_as(
            "SELECT id, ts, worker, difficulty, share_diff, accepted, reject_reason
             FROM shares
             WHERE lane = ? AND accepted = 1 AND ts >= ?
             ORDER BY ts DESC, id DESC
             LIMIT ?",
        )
        .bind(lane)
        .bind(since)
        .bind(CAP)
        .fetch_all(&self.pool)
        .await?;
        rows.reverse();
        Ok(rows)
    }

    /// A lane's most recent shares, newest first, for the live log.
    pub async fn recent_shares(&self, lane: &str, limit: i64) -> Result<Vec<ShareRow>, StoreError> {
        Ok(sqlx::query_as(
            "SELECT id, ts, worker, difficulty, share_diff, accepted, reject_reason
             FROM shares
             WHERE lane = ?
             ORDER BY ts DESC, id DESC
             LIMIT ?",
        )
        .bind(lane)
        .bind(limit)
        .fetch_all(&self.pool)
        .await?)
    }

    /// Record a lane's aligned hashrate samples. Empty `worker` is the lane total.
    pub async fn insert_hashrate_samples(
        &self,
        lane: &str,
        samples: &[HashrateSample],
    ) -> Result<(), StoreError> {
        if samples.is_empty() {
            return Ok(());
        }
        let mut tx = self.pool.begin().await?;
        for s in samples {
            sqlx::query(
                "INSERT INTO hashrate_samples (ts, lane, worker, hashrate) VALUES (?, ?, ?, ?)
                 ON CONFLICT(ts, lane, worker) DO UPDATE SET hashrate = excluded.hashrate",
            )
            .bind(s.ts)
            .bind(lane)
            .bind(&s.worker)
            .bind(s.hashrate)
            .execute(&mut *tx)
            .await?;
        }
        tx.commit().await?;
        Ok(())
    }

    /// A lane's samples at or after `since`, oldest first. `worker` narrows to one worker
    /// (`Some("")` is the lane total); `None` returns every series in the lane.
    pub async fn hashrate_samples_since(
        &self,
        lane: &str,
        worker: Option<&str>,
        since: i64,
    ) -> Result<Vec<HashrateSample>, StoreError> {
        match worker {
            Some(name) => Ok(sqlx::query_as(
                "SELECT ts, worker, hashrate FROM hashrate_samples
                 WHERE lane = ? AND worker = ? AND ts >= ? ORDER BY ts",
            )
            .bind(lane)
            .bind(name)
            .bind(since)
            .fetch_all(&self.pool)
            .await?),
            None => Ok(sqlx::query_as(
                "SELECT ts, worker, hashrate FROM hashrate_samples
                 WHERE lane = ? AND ts >= ? ORDER BY ts, worker",
            )
            .bind(lane)
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

    const LTC: &str = "ltc";

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
            reject_reason: (!accepted).then_some("low_difficulty"),
        }
    }

    #[tokio::test]
    async fn accounting_survives_reopen() {
        let path = temp_path("accounting");
        let store = Store::open(&path).await.unwrap();
        store
            .persist_batch(
                LTC,
                &[worker("rig1", 1000)],
                &[share("rig1", 1010, true), share("rig1", 1020, false)],
            )
            .await
            .unwrap();
        drop(store);

        let store = Store::open(&path).await.unwrap();
        let workers = store.load_workers(LTC).await.unwrap();
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
        assert_eq!(
            workers[0].best_at,
            Some(1010),
            "best_at follows the best share"
        );
        assert_eq!(workers[0].work_accepted, 16.0);
        assert_eq!(store.total_work(LTC).await.unwrap(), 16.0);

        let accepted = store.accepted_shares_since(LTC, 0).await.unwrap();
        assert!(accepted[0].accepted);
        assert_eq!(accepted.len(), 1);
        assert_eq!(accepted[0].difficulty, 16.0);
        let recent = store.recent_shares(LTC, 10).await.unwrap();
        assert_eq!(recent.len(), 2);
        assert!(recent[0].ts >= recent[1].ts);
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[tokio::test]
    async fn removing_a_worker_banks_its_work_and_drops_its_history() {
        let path = temp_path("remove");
        let store = Store::open(&path).await.unwrap();
        store
            .persist_batch(
                LTC,
                &[worker("rig1", 1000), worker("rig2", 1000)],
                &[share("rig1", 1010, true), share("rig2", 1020, true)],
            )
            .await
            .unwrap();
        store
            .insert_hashrate_samples(
                LTC,
                &[HashrateSample {
                    ts: 1080,
                    worker: "rig1".into(),
                    hashrate: 5.0,
                }],
            )
            .await
            .unwrap();
        assert_eq!(store.total_work(LTC).await.unwrap(), 32.0);

        assert_eq!(store.remove_worker(LTC, "rig1").await.unwrap(), Some(16.0));
        assert_eq!(store.remove_worker(LTC, "rig1").await.unwrap(), None);

        let workers = store.load_workers(LTC).await.unwrap();
        assert_eq!(workers.len(), 1);
        assert_eq!(workers[0].name, "rig2");
        assert_eq!(store.retired_work(LTC).await.unwrap(), 16.0);
        assert_eq!(store.total_work(LTC).await.unwrap(), 32.0);
        assert!(store
            .recent_shares(LTC, 10)
            .await
            .unwrap()
            .iter()
            .all(|s| s.worker == "rig2"));
        assert!(store
            .hashrate_samples_since(LTC, Some("rig1"), 0)
            .await
            .unwrap()
            .is_empty());
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[tokio::test]
    async fn a_batch_updates_workers_as_its_shares_would_one_by_one() {
        let s = |worker: &str,
                 ts: i64,
                 difficulty: f64,
                 share_diff: f64,
                 reject: Option<&'static str>| NewShare {
            ts,
            worker: worker.into(),
            difficulty,
            share_diff,
            accepted: reject.is_none(),
            reject_reason: reject,
        };
        // rig1 already has a best of 100 from an earlier batch.
        let earlier = [s("rig1", 1001, 16.0, 100.0, None)];
        let batch = [
            s("rig1", 1010, 16.0, 50.0, None),
            // rig2 never authorized: its row comes from this share, first_seen 1011.
            s("rig2", 1011, 8.0, 64.0, None),
            // Rejected: counts, but no work and no best whatever its share_diff says.
            s("rig1", 1012, 16.0, 999.0, Some("low_difficulty")),
            // Older than the share before it: last_seen must not go backwards.
            s("rig2", 1009, 8.0, 128.0, None),
            // Ties rig2's best: the earlier share keeps best_at.
            s("rig2", 1013, 4.0, 128.0, None),
            s("rig2", 1014, 0.0, 0.0, Some("stale_job")),
            // rig1's batch best (80) does not beat its stored 100: best_at stays 1001.
            s("rig1", 1008, 16.0, 80.0, None),
        ];

        let batched = temp_path("batched");
        let one_by_one = temp_path("one-by-one");
        let a = Store::open(&batched).await.unwrap();
        let b = Store::open(&one_by_one).await.unwrap();
        for store in [&a, &b] {
            store
                .persist_batch(LTC, &[worker("rig1", 1000)], &earlier)
                .await
                .unwrap();
        }
        a.persist_batch(LTC, &[], &batch).await.unwrap();
        for share in &batch {
            b.persist_batch(LTC, &[], std::slice::from_ref(share))
                .await
                .unwrap();
        }

        let workers = a.load_workers(LTC).await.unwrap();
        assert_eq!(workers.len(), 2);
        let (rig1, rig2) = (&workers[0], &workers[1]);
        assert_eq!(rig1.name, "rig1");
        assert_eq!(rig1.payout_address, "ltc1qtest");
        assert_eq!((rig1.first_seen, rig1.last_seen), (1000, 1012));
        assert_eq!((rig1.shares_accepted, rig1.shares_rejected), (3, 1));
        assert_eq!((rig1.best_difficulty, rig1.best_at), (100.0, Some(1001)));
        assert_eq!(rig1.work_accepted, 48.0);
        assert_eq!(rig2.name, "rig2");
        assert_eq!(rig2.payout_address, "");
        assert_eq!((rig2.first_seen, rig2.last_seen), (1011, 1014));
        assert_eq!((rig2.shares_accepted, rig2.shares_rejected), (3, 1));
        assert_eq!((rig2.best_difficulty, rig2.best_at), (128.0, Some(1009)));
        assert_eq!(rig2.work_accepted, 20.0);

        assert_eq!(workers, b.load_workers(LTC).await.unwrap());
        // Share rows go in one per share, in order, either way.
        let shares = a.recent_shares(LTC, 100).await.unwrap();
        assert_eq!(shares.len(), 1 + batch.len());
        assert_eq!(shares, b.recent_shares(LTC, 100).await.unwrap());
        let mut by_id = shares.clone();
        by_id.sort_by_key(|r| r.id);
        let order: Vec<i64> = by_id.iter().map(|r| r.ts).collect();
        assert_eq!(order, [1001, 1010, 1011, 1012, 1009, 1013, 1014, 1008]);
        assert_eq!(by_id[3].reject_reason.as_deref(), Some("low_difficulty"));
        drop((a, b));
        for path in [batched, one_by_one] {
            let _ = std::fs::remove_dir_all(path.parent().unwrap());
        }
    }

    #[tokio::test]
    async fn share_without_authorize_creates_a_stub_worker() {
        let path = temp_path("stub");
        let store = Store::open(&path).await.unwrap();
        store
            .persist_batch(LTC, &[], &[share("ghost", 5, true)])
            .await
            .unwrap();
        let workers = store.load_workers(LTC).await.unwrap();
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
            .insert_hashrate_samples(
                LTC,
                &[
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
                ],
            )
            .await
            .unwrap();
        store
            .insert_hashrate_samples(
                LTC,
                &[HashrateSample {
                    ts: 60,
                    worker: "rig1".into(),
                    hashrate: 50.0,
                }],
            )
            .await
            .unwrap();
        let pool = store
            .hashrate_samples_since(LTC, Some(""), 0)
            .await
            .unwrap();
        assert_eq!(pool[0].hashrate, 100.0);
        let rig = store
            .hashrate_samples_since(LTC, Some("rig1"), 0)
            .await
            .unwrap();
        assert_eq!(rig[0].hashrate, 50.0);
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[tokio::test]
    async fn lanes_keep_same_named_workers_apart() {
        const BTC: &str = "btc";
        let path = temp_path("lanes");
        let store = Store::open(&path).await.unwrap();
        store
            .persist_batch(
                LTC,
                &[worker("rig", 1000)],
                &[share("rig", 1010, true), share("rig", 1020, false)],
            )
            .await
            .unwrap();
        let mut big = share("rig", 1030, true);
        big.difficulty = 100.0;
        big.share_diff = 400.0;
        let mut btc_worker = worker("rig", 1001);
        btc_worker.payout_address = "bc1qtest".into();
        store
            .persist_batch(BTC, &[btc_worker], &[big])
            .await
            .unwrap();
        for (lane, hashrate) in [(LTC, 5.0), (BTC, 900.0)] {
            store
                .insert_hashrate_samples(
                    lane,
                    &[
                        HashrateSample {
                            ts: 60,
                            worker: "rig".into(),
                            hashrate,
                        },
                        HashrateSample {
                            ts: 60,
                            worker: String::new(),
                            hashrate,
                        },
                    ],
                )
                .await
                .unwrap();
        }

        let ltc = store.load_workers(LTC).await.unwrap();
        let btc = store.load_workers(BTC).await.unwrap();
        assert_eq!((ltc.len(), btc.len()), (1, 1));
        assert_eq!(ltc[0].payout_address, "ltc1qtest");
        assert_eq!(btc[0].payout_address, "bc1qtest");
        assert_eq!((ltc[0].shares_accepted, ltc[0].shares_rejected), (1, 1));
        assert_eq!((btc[0].shares_accepted, btc[0].shares_rejected), (1, 0));
        assert_eq!(
            (ltc[0].best_difficulty, btc[0].best_difficulty),
            (32.0, 400.0)
        );
        assert_eq!(store.total_work(LTC).await.unwrap(), 16.0);
        assert_eq!(store.total_work(BTC).await.unwrap(), 100.0);
        assert_eq!(store.recent_shares(LTC, 10).await.unwrap().len(), 2);
        let btc_shares = store.recent_shares(BTC, 10).await.unwrap();
        assert_eq!(btc_shares.len(), 1);
        assert_eq!(btc_shares[0].share_diff, 400.0);
        assert_eq!(store.accepted_shares_since(LTC, 0).await.unwrap().len(), 1);
        assert_eq!(
            store.accepted_shares_since(BTC, 0).await.unwrap()[0].difficulty,
            100.0
        );
        let ltc_samples = store.hashrate_samples_since(LTC, None, 0).await.unwrap();
        assert_eq!(ltc_samples.len(), 2);
        assert!(ltc_samples.iter().all(|s| s.hashrate == 5.0));
        let btc_total = store
            .hashrate_samples_since(BTC, Some(""), 0)
            .await
            .unwrap();
        assert_eq!(btc_total.len(), 1);
        assert_eq!(btc_total[0].hashrate, 900.0);

        // Resetting one lane's counters leaves the other's alone.
        store.reset_share_counters(LTC).await.unwrap();
        let ltc = store.load_workers(LTC).await.unwrap();
        let btc = store.load_workers(BTC).await.unwrap();
        assert_eq!((ltc[0].shares_accepted, ltc[0].best_at), (0, None));
        assert_eq!((btc[0].shares_accepted, btc[0].best_at), (1, Some(1030)));
        assert_eq!(btc[0].best_difficulty, 400.0);

        // Removing the worker from one lane banks its work there and nowhere else.
        assert_eq!(store.remove_worker(BTC, "rig").await.unwrap(), Some(100.0));
        assert_eq!(store.remove_worker(BTC, "rig").await.unwrap(), None);
        assert!(store.load_workers(BTC).await.unwrap().is_empty());
        assert_eq!(store.load_workers(LTC).await.unwrap().len(), 1);
        assert_eq!(store.retired_work(BTC).await.unwrap(), 100.0);
        assert_eq!(store.retired_work(LTC).await.unwrap(), 0.0);
        assert_eq!(store.total_work(BTC).await.unwrap(), 100.0);
        assert_eq!(store.total_work(LTC).await.unwrap(), 16.0);
        assert!(store.recent_shares(BTC, 10).await.unwrap().is_empty());
        assert_eq!(store.recent_shares(LTC, 10).await.unwrap().len(), 2);
        assert!(store
            .hashrate_samples_since(BTC, Some("rig"), 0)
            .await
            .unwrap()
            .is_empty());
        assert_eq!(
            store
                .hashrate_samples_since(LTC, Some("rig"), 0)
                .await
                .unwrap()
                .len(),
            1
        );

        // A second removal in the same lane adds to the existing counter.
        assert_eq!(store.remove_worker(LTC, "rig").await.unwrap(), Some(16.0));
        assert_eq!(store.retired_work(LTC).await.unwrap(), 16.0);
        assert_eq!(store.retired_work(BTC).await.unwrap(), 100.0);
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[tokio::test]
    async fn alias_is_lane_scoped_survives_reauthorize_and_goes_with_the_row() {
        const BTC: &str = "btc";
        let path = temp_path("alias");
        let store = Store::open(&path).await.unwrap();
        for lane in [LTC, BTC] {
            store
                .persist_batch(lane, &[worker("rig", 1000)], &[])
                .await
                .unwrap();
        }
        assert_eq!(store.load_workers(LTC).await.unwrap()[0].alias, None);
        assert!(store
            .set_worker_alias(LTC, "rig", Some("Garage S19"))
            .await
            .unwrap());
        assert!(
            !store
                .set_worker_alias(LTC, "nobody", Some("x"))
                .await
                .unwrap(),
            "an unknown worker reports false"
        );
        assert_eq!(
            store.load_workers(LTC).await.unwrap()[0].alias.as_deref(),
            Some("Garage S19")
        );
        assert_eq!(
            store.load_workers(BTC).await.unwrap()[0].alias,
            None,
            "the same name in another lane has its own alias"
        );

        // Re-authorizing (with a new address) and new shares leave the alias alone.
        let mut again = worker("rig", 2000);
        again.payout_address = "ltc1qother".into();
        store
            .persist_batch(LTC, &[again], &[share("rig", 2001, true)])
            .await
            .unwrap();
        store.reset_share_counters(LTC).await.unwrap();
        let row = &store.load_workers(LTC).await.unwrap()[0];
        assert_eq!(row.payout_address, "ltc1qother");
        assert_eq!(row.alias.as_deref(), Some("Garage S19"));

        // Removing the worker removes its alias: it comes back without one.
        store.remove_worker(LTC, "rig").await.unwrap();
        store
            .persist_batch(LTC, &[worker("rig", 3000)], &[])
            .await
            .unwrap();
        assert_eq!(store.load_workers(LTC).await.unwrap()[0].alias, None);

        // Clearing.
        store.set_worker_alias(BTC, "rig", Some("b")).await.unwrap();
        assert!(store.set_worker_alias(BTC, "rig", None).await.unwrap());
        assert_eq!(store.load_workers(BTC).await.unwrap()[0].alias, None);
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }
}
