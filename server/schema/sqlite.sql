-- UnionC SQLite schema.
--
-- This final-state schema is the sole schema for new installations. All
-- timestamps are Unix microseconds, UUIDs are canonical text, and JSON is
-- validated text. STRICT tables strengthen SQLite's default affinity rules.

CREATE TABLE audit_logs (
    id         INTEGER PRIMARY KEY,
    action     TEXT NOT NULL CHECK (length(action) BETWEEN 1 AND 128),
    target     TEXT NOT NULL CHECK (length(target) BETWEEN 1 AND 128),
    detail     TEXT CHECK (detail IS NULL OR length(detail) <= 512),
    actor      TEXT NOT NULL DEFAULT 'system',
    request_id TEXT,
    created_at INTEGER NOT NULL
                   DEFAULT (CAST(unixepoch('subsec') * 1000000 AS INTEGER))
) STRICT;

CREATE INDEX idx_audit_logs_created_at ON audit_logs(created_at);
