-- Workers seen by the pool, keyed by the full stratum username.
CREATE TABLE workers (
    name            TEXT PRIMARY KEY,
    payout_address  TEXT NOT NULL,
    aux_address     TEXT,
    first_seen      INTEGER NOT NULL,
    last_seen       INTEGER NOT NULL,
    shares_accepted INTEGER NOT NULL DEFAULT 0,
    shares_rejected INTEGER NOT NULL DEFAULT 0,
    best_difficulty REAL    NOT NULL DEFAULT 0
);

-- Periodic hashrate samples. worker = '' is the pool total.
CREATE TABLE hashrate_samples (
    ts        INTEGER NOT NULL,
    worker    TEXT    NOT NULL,
    hashrate  REAL    NOT NULL,
    PRIMARY KEY (ts, worker)
);

-- Blocks the pool has found.
CREATE TABLE blocks (
    id             INTEGER PRIMARY KEY,
    coin           TEXT    NOT NULL,
    height         INTEGER NOT NULL,
    hash           TEXT    NOT NULL,
    worker         TEXT    NOT NULL,
    difficulty     REAL    NOT NULL,
    share_diff     REAL    NOT NULL,
    reward_sats    INTEGER,
    found_at       INTEGER NOT NULL,
    status         TEXT    NOT NULL,
    confirmations  INTEGER NOT NULL DEFAULT 0,
    UNIQUE (coin, hash)
);
CREATE INDEX blocks_found_at ON blocks (found_at DESC);
CREATE INDEX blocks_status ON blocks (status);

-- Recent shares for the dashboard's live log. Trimmed by retention.
CREATE TABLE shares (
    id          INTEGER PRIMARY KEY,
    ts          INTEGER NOT NULL,
    worker      TEXT    NOT NULL,
    difficulty  REAL    NOT NULL,
    share_diff  REAL    NOT NULL,
    accepted    INTEGER NOT NULL,
    reject_reason TEXT
);
CREATE INDEX shares_ts ON shares (ts);
