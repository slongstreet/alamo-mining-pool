-- When each worker's best share arrived, so the dashboard can say who found the pool's
-- best share and when. NULL until the worker's first accepted share (or after a reset).
ALTER TABLE workers ADD COLUMN best_at INTEGER;
