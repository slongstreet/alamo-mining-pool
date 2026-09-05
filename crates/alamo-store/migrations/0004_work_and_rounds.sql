-- Luck needs the work behind each block. `work_accepted` accumulates the job difficulty of
-- every accepted share per worker (lifetime, unlike the trimmed shares table), and each
-- block records the pool-wide total at the moment it was found so the current round's
-- work is total minus the last block's `work_at_found`.
ALTER TABLE workers ADD COLUMN work_accepted REAL NOT NULL DEFAULT 0;
ALTER TABLE blocks ADD COLUMN work_at_found REAL;
