-- Wave 4: per-coin round accounting for the luck display. A round is the work submitted
-- since the last block the pool found on that coin. Work is in difficulty-1 share units
-- (2^32 hashes each). Rows are created at startup for every mined coin.
CREATE TABLE rounds (
    coin        TEXT    PRIMARY KEY,
    started_at  INTEGER NOT NULL,
    work        REAL    NOT NULL DEFAULT 0,
    shares      INTEGER NOT NULL DEFAULT 0,
    best_share  REAL    NOT NULL DEFAULT 0
);
