-- 0018_rate_leases: SQLite port.
-- id uses INTEGER PRIMARY KEY AUTOINCREMENT (no BIGSERIAL).
-- TIMESTAMPTZ → TEXT DEFAULT CURRENT_TIMESTAMP.

CREATE TABLE IF NOT EXISTS subagent_rate_leases (
    id            INTEGER PRIMARY KEY AUTOINCREMENT,
    key           TEXT    NOT NULL,
    owner_job_id  INTEGER NOT NULL REFERENCES minion_jobs(id) ON DELETE CASCADE,
    acquired_at   TEXT    NOT NULL DEFAULT (datetime('now')),
    expires_at    TEXT    NOT NULL
);

CREATE INDEX IF NOT EXISTS idx_rate_leases_key_expires
    ON subagent_rate_leases (key, expires_at);
