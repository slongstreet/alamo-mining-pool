-- Wave 2 added per-worker aux payouts; persist them as JSON. `fallback` records whether
-- the parent address was substituted so a restart can restore the dashboard flags.
ALTER TABLE workers ADD COLUMN aux_payouts TEXT NOT NULL DEFAULT '[]';
ALTER TABLE workers ADD COLUMN fallback INTEGER NOT NULL DEFAULT 0;
