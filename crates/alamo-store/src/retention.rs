//! Retention and downsampling so the database stays small.

use crate::{Store, StoreError};

/// How long raw rows are kept and how older hashrate samples are coarsened.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RetentionPolicy {
    /// Delete individual shares older than this many seconds.
    pub share_secs: i64,
    /// Extra cap on each lane's shares, newest rows kept, so a busy lane cannot push
    /// another lane's recent shares out of the log and the restart replay.
    pub share_max_rows: i64,
    /// Keep 1-minute hashrate samples this long.
    pub sample_raw_secs: i64,
    /// After `sample_raw_secs`, keep samples this long at `sample_mid_bucket`.
    pub sample_mid_secs: i64,
    /// After `sample_mid_secs`, keep samples this long at `sample_long_bucket`.
    pub sample_long_secs: i64,
    /// Bucket width in seconds for the mid-term samples (5 minutes).
    pub sample_mid_bucket: i64,
    /// Bucket width in seconds for the long-term samples (1 hour).
    pub sample_long_bucket: i64,
}

impl Default for RetentionPolicy {
    fn default() -> Self {
        Self {
            share_secs: 24 * 3600,
            share_max_rows: 50_000,
            sample_raw_secs: 24 * 3600,
            sample_mid_secs: 7 * 24 * 3600,
            sample_long_secs: 30 * 24 * 3600,
            sample_mid_bucket: 300,
            sample_long_bucket: 3600,
        }
    }
}

/// How many rows a retention pass removed.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct RetentionReport {
    /// Shares deleted.
    pub shares_deleted: u64,
    /// Hashrate samples deleted (including those replaced by a coarser bucket).
    pub samples_deleted: u64,
}

impl Store {
    /// Trim old shares and downsample hashrate samples. Deletes by age apply across all
    /// lanes; the share row cap applies to each lane; downsampling never merges lanes.
    ///
    /// Every tier boundary is aligned down to its bucket width so only complete buckets are
    /// folded. A bucket that straddles a boundary is left alone until it has fully aged in,
    /// otherwise its partial average would be averaged again on the next pass.
    pub async fn retain(
        &self,
        policy: &RetentionPolicy,
        now: i64,
    ) -> Result<RetentionReport, StoreError> {
        let mut report = RetentionReport::default();
        report.shares_deleted += self.trim_shares(policy, now).await?;
        let long_start = align_down(
            now.saturating_sub(policy.sample_long_secs),
            policy.sample_long_bucket,
        );
        let mid_start = now.saturating_sub(policy.sample_mid_secs);
        let raw_start = now.saturating_sub(policy.sample_raw_secs);
        report.samples_deleted += self
            .downsample_range(
                long_start,
                align_down(mid_start, policy.sample_long_bucket),
                policy.sample_long_bucket,
            )
            .await?;
        report.samples_deleted += self
            .downsample_range(
                align_down(mid_start, policy.sample_mid_bucket),
                align_down(raw_start, policy.sample_mid_bucket),
                policy.sample_mid_bucket,
            )
            .await?;
        let expired = sqlx::query("DELETE FROM hashrate_samples WHERE ts < ?")
            .bind(long_start)
            .execute(&self.pool)
            .await?;
        report.samples_deleted += expired.rows_affected();
        Ok(report)
    }

    async fn trim_shares(&self, policy: &RetentionPolicy, now: i64) -> Result<u64, StoreError> {
        let cutoff = sqlx::query("DELETE FROM shares WHERE ts < ?")
            .bind(now.saturating_sub(policy.share_secs))
            .execute(&self.pool)
            .await?;
        let mut deleted = cutoff.rows_affected();
        let counts: Vec<(String, i64)> =
            sqlx::query_as("SELECT lane, COUNT(*) FROM shares GROUP BY lane")
                .fetch_all(&self.pool)
                .await?;
        for (lane, count) in counts {
            if count <= policy.share_max_rows {
                continue;
            }
            let capped = sqlx::query(
                "DELETE FROM shares WHERE id IN (
                    SELECT id FROM shares WHERE lane = ? ORDER BY ts ASC, id ASC LIMIT ?
                 )",
            )
            .bind(&lane)
            .bind(count - policy.share_max_rows)
            .execute(&self.pool)
            .await?;
            deleted += capped.rows_affected();
        }
        Ok(deleted)
    }

    /// Average samples in `[lo, hi)` into `bucket`-second rows per lane and worker, then
    /// drop the originals. `lo` and `hi` must be multiples of `bucket`.
    async fn downsample_range(&self, lo: i64, hi: i64, bucket: i64) -> Result<u64, StoreError> {
        if hi <= lo || bucket <= 0 {
            return Ok(0);
        }
        debug_assert!(lo % bucket == 0 && hi % bucket == 0);
        sqlx::query(
            "INSERT INTO hashrate_samples (ts, lane, worker, hashrate)
             SELECT (ts / ?) * ? AS bucket_ts, lane, worker, AVG(hashrate)
             FROM hashrate_samples
             WHERE ts >= ? AND ts < ?
             GROUP BY lane, worker, bucket_ts
             ON CONFLICT(ts, lane, worker) DO UPDATE SET hashrate = excluded.hashrate",
        )
        .bind(bucket)
        .bind(bucket)
        .bind(lo)
        .bind(hi)
        .execute(&self.pool)
        .await?;
        let dropped = sqlx::query(
            "DELETE FROM hashrate_samples
             WHERE ts >= ? AND ts < ? AND (ts / ?) * ? != ts",
        )
        .bind(lo)
        .bind(hi)
        .bind(bucket)
        .bind(bucket)
        .execute(&self.pool)
        .await?;
        Ok(dropped.rows_affected())
    }
}

/// Round `ts` down to a multiple of `bucket`.
fn align_down(ts: i64, bucket: i64) -> i64 {
    if bucket <= 0 {
        return ts;
    }
    ts.div_euclid(bucket) * bucket
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{temp_path, HashrateSample, NewShare, Store};

    const LTC: &str = "ltc";

    fn policy() -> RetentionPolicy {
        RetentionPolicy {
            share_secs: 100,
            share_max_rows: 3,
            sample_raw_secs: 100,
            sample_mid_secs: 1_000,
            sample_long_secs: 10_000,
            sample_mid_bucket: 300,
            sample_long_bucket: 3_600,
        }
    }

    #[tokio::test]
    async fn drops_old_shares_and_caps_the_table() {
        let path = temp_path("retain-shares");
        let store = Store::open(&path).await.unwrap();
        let shares: Vec<NewShare> = (0..5)
            .map(|i| NewShare {
                ts: 1_000 + i * 10,
                worker: "w".into(),
                difficulty: 1.0,
                share_diff: 1.0,
                accepted: true,
                reject_reason: None,
            })
            .collect();
        store.persist_batch(LTC, &[], &shares).await.unwrap();
        // now=1110: share_secs=100 keeps ts>=1010, which is 4 rows, then cap to 3.
        let report = store.retain(&policy(), 1_110).await.unwrap();
        assert!(report.shares_deleted >= 2);
        let left = store.recent_shares(LTC, 10).await.unwrap();
        assert_eq!(left.len(), 3);
        assert!(left.iter().all(|s| s.ts >= 1_010));
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[tokio::test]
    async fn expires_samples_older_than_the_long_window() {
        let path = temp_path("retain-expire");
        let store = Store::open(&path).await.unwrap();
        // now=20_000, long_secs=10_000: the long window starts at 10_000, aligned down to
        // the hour it is 7_200. Anything below that is gone; a fresh raw sample stays.
        let samples: Vec<HashrateSample> = [3_600, 7_140, 7_200, 19_950]
            .into_iter()
            .map(|ts| HashrateSample {
                ts,
                worker: String::new(),
                hashrate: ts as f64,
            })
            .collect();
        store.insert_hashrate_samples(LTC, &samples).await.unwrap();
        let report = store.retain(&policy(), 20_000).await.unwrap();
        assert_eq!(report.samples_deleted, 2);
        let left: Vec<i64> = store
            .hashrate_samples_since(LTC, None, 0)
            .await
            .unwrap()
            .into_iter()
            .map(|s| s.ts)
            .collect();
        assert_eq!(left, vec![7_200, 19_950]);
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[tokio::test]
    async fn partial_buckets_wait_until_complete_so_averages_do_not_drift() {
        let path = temp_path("retain-drift");
        let store = Store::open(&path).await.unwrap();
        // Five 60s samples in bucket [300, 600) with mean 30.
        let samples: Vec<HashrateSample> = [300, 360, 420, 480, 540]
            .into_iter()
            .zip([10.0, 20.0, 30.0, 40.0, 50.0])
            .map(|(ts, hashrate)| HashrateSample {
                ts,
                worker: "rig".into(),
                hashrate,
            })
            .collect();
        store.insert_hashrate_samples(LTC, &samples).await.unwrap();

        // now=520, raw=100: the mid window ends at 420, mid-bucket, so nothing is folded.
        store.retain(&policy(), 520).await.unwrap();
        assert_eq!(
            store
                .hashrate_samples_since(LTC, Some("rig"), 0)
                .await
                .unwrap()
                .len(),
            5
        );

        // now=700: the mid window ends at 600 and the whole bucket has aged in.
        store.retain(&policy(), 700).await.unwrap();
        let left = store
            .hashrate_samples_since(LTC, Some("rig"), 0)
            .await
            .unwrap();
        assert_eq!(left.len(), 1);
        assert_eq!(left[0].ts, 300);
        assert!(
            (left[0].hashrate - 30.0).abs() < 1e-9,
            "{}",
            left[0].hashrate
        );

        // Folding again is idempotent.
        store.retain(&policy(), 800).await.unwrap();
        let again = store
            .hashrate_samples_since(LTC, Some("rig"), 0)
            .await
            .unwrap();
        assert_eq!(again, left);
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[tokio::test]
    async fn mid_window_averages_into_five_minute_buckets() {
        let path = temp_path("retain-mid");
        let store = Store::open(&path).await.unwrap();
        // now=1_000, raw=100 so mid is [0, 900). Samples at 0, 60, 120 -> bucket 0.
        let samples: Vec<HashrateSample> = [0, 60, 120]
            .into_iter()
            .map(|ts| HashrateSample {
                ts,
                worker: "rig".into(),
                hashrate: ts as f64,
            })
            .collect();
        store.insert_hashrate_samples(LTC, &samples).await.unwrap();
        store.retain(&policy(), 1_000).await.unwrap();
        let left = store
            .hashrate_samples_since(LTC, Some("rig"), 0)
            .await
            .unwrap();
        assert_eq!(left.len(), 1);
        assert_eq!(left[0].ts, 0);
        assert!(
            (left[0].hashrate - 60.0).abs() < 1e-9,
            "{}",
            left[0].hashrate
        );
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[tokio::test]
    async fn downsampling_keeps_lanes_apart() {
        let path = temp_path("retain-lanes");
        let store = Store::open(&path).await.unwrap();
        // Same worker name and timestamps in two lanes, with different rates.
        for (lane, base) in [(LTC, 10.0), ("btc", 1_000.0)] {
            let samples: Vec<HashrateSample> = [0, 60, 120]
                .into_iter()
                .map(|ts| HashrateSample {
                    ts,
                    worker: "rig".into(),
                    hashrate: base + ts as f64,
                })
                .collect();
            store.insert_hashrate_samples(lane, &samples).await.unwrap();
        }
        store.retain(&policy(), 1_000).await.unwrap();
        for (lane, mean) in [(LTC, 70.0), ("btc", 1_060.0)] {
            let left = store
                .hashrate_samples_since(lane, Some("rig"), 0)
                .await
                .unwrap();
            assert_eq!(left.len(), 1, "{lane}");
            assert_eq!(left[0].ts, 0);
            assert!(
                (left[0].hashrate - mean).abs() < 1e-9,
                "{lane}: {}",
                left[0].hashrate
            );
        }
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[tokio::test]
    async fn the_share_cap_applies_to_each_lane() {
        let path = temp_path("retain-lane-cap");
        let store = Store::open(&path).await.unwrap();
        let shares = |n: i64, base: i64| -> Vec<NewShare> {
            (0..n)
                .map(|i| NewShare {
                    ts: base + i,
                    worker: "w".into(),
                    difficulty: 1.0,
                    share_diff: 1.0,
                    accepted: true,
                    reject_reason: None,
                })
                .collect()
        };
        // A busy lane with newer shares and a quiet one with two older shares.
        store
            .persist_batch(LTC, &[], &shares(6, 1_050))
            .await
            .unwrap();
        store
            .persist_batch("doge", &[], &shares(2, 1_020))
            .await
            .unwrap();
        let report = store.retain(&policy(), 1_100).await.unwrap();
        assert_eq!(report.shares_deleted, 3, "only the busy lane is capped");
        let ltc = store.recent_shares(LTC, 10).await.unwrap();
        assert_eq!(
            ltc.iter().map(|s| s.ts).collect::<Vec<_>>(),
            vec![1_055, 1_054, 1_053]
        );
        assert_eq!(store.recent_shares("doge", 10).await.unwrap().len(), 2);
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }
}
