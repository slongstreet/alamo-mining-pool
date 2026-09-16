-- Operator settings changed from the dashboard. Each row overrides one value from the
-- config file; the file stays the source of every value with no row here.
CREATE TABLE settings (
    key        TEXT PRIMARY KEY,
    value      TEXT    NOT NULL,
    updated_at INTEGER NOT NULL
);
