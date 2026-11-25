-- An operator-chosen display name for a worker, scoped to its lane like the row itself.
-- NULL means none. Removing the worker removes its alias with the row; re-authorizing
-- leaves it alone, since the authorize upsert never names this column.
ALTER TABLE workers ADD COLUMN alias TEXT;
