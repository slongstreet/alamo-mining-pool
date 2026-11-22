-- Lanes: one parent coin plus its aux coins, each with its own workers, shares, hashrate
-- samples, and retired work. A lane is keyed by the parent coin's config key in lower case.
-- Every existing row belongs to 'ltc': no other parent chain has ever been supported.

-- workers: primary key becomes (lane, name). SQLite cannot change a primary key in place,
-- so rebuild the table, carrying every column from 0001, 0002, 0004, and 0007.
CREATE TABLE workers_new (
    lane            TEXT    NOT NULL,
    name            TEXT    NOT NULL,
    payout_address  TEXT    NOT NULL,
    aux_address     TEXT,
    first_seen      INTEGER NOT NULL,
    last_seen       INTEGER NOT NULL,
    shares_accepted INTEGER NOT NULL DEFAULT 0,
    shares_rejected INTEGER NOT NULL DEFAULT 0,
    best_difficulty REAL    NOT NULL DEFAULT 0,
    aux_payouts     TEXT    NOT NULL DEFAULT '[]',
    fallback        INTEGER NOT NULL DEFAULT 0,
    work_accepted   REAL    NOT NULL DEFAULT 0,
    best_at         INTEGER,
    PRIMARY KEY (lane, name)
);
INSERT INTO workers_new (
    lane, name, payout_address, aux_address, first_seen, last_seen, shares_accepted,
    shares_rejected, best_difficulty, aux_payouts, fallback, work_accepted, best_at
)
SELECT 'ltc', name, payout_address, aux_address, first_seen, last_seen, shares_accepted,
       shares_rejected, best_difficulty, aux_payouts, fallback, work_accepted, best_at
FROM workers;
DROP TABLE workers;
ALTER TABLE workers_new RENAME TO workers;

-- shares: the live log and the restart replay read one lane, newest first.
ALTER TABLE shares ADD COLUMN lane TEXT NOT NULL DEFAULT 'ltc';
CREATE INDEX shares_lane_ts ON shares (lane, ts);

-- hashrate_samples: primary key becomes (ts, lane, worker); worker = '' is the lane total.
CREATE TABLE hashrate_samples_new (
    ts        INTEGER NOT NULL,
    lane      TEXT    NOT NULL,
    worker    TEXT    NOT NULL,
    hashrate  REAL    NOT NULL,
    PRIMARY KEY (ts, lane, worker)
);
INSERT INTO hashrate_samples_new (ts, lane, worker, hashrate)
SELECT ts, 'ltc', worker, hashrate FROM hashrate_samples;
DROP TABLE hashrate_samples;
ALTER TABLE hashrate_samples_new RENAME TO hashrate_samples;
CREATE INDEX hashrate_samples_lane_worker_ts ON hashrate_samples (lane, worker, ts);

-- blocks: the lane that found it. A DOGE block merge-mined on LTC belongs to 'ltc'.
ALTER TABLE blocks ADD COLUMN lane TEXT NOT NULL DEFAULT 'ltc';

-- pool_counters: retired work is kept per lane as 'retired_work:<lane>'.
UPDATE pool_counters SET key = 'retired_work:ltc' WHERE key = 'retired_work';
