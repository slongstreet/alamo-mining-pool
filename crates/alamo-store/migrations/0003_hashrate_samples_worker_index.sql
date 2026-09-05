-- Per-worker hashrate charts read `WHERE worker = ? AND ts >= ?`. The primary key leads
-- with ts, so give that query its own index.
CREATE INDEX hashrate_samples_worker_ts ON hashrate_samples (worker, ts);
