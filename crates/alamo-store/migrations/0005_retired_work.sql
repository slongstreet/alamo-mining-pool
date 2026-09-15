-- Removing a worker must not move the round: its lifetime accepted work is banked here so
-- pool-wide total work (and every block's `work_at_found` measured against it) stays put.
CREATE TABLE pool_counters (
    key   TEXT PRIMARY KEY,
    value REAL NOT NULL
);
INSERT INTO pool_counters (key, value) VALUES ('retired_work', 0);
