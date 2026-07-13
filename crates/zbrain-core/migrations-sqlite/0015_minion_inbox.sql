-- 0015: Create `minion_inbox` table — SQLite port of the sidechannel inbox.
--
-- Mirrors migrations/0015_minion_inbox.sql (PostgreSQL). Type mapping:
--   SERIAL PRIMARY KEY  → INTEGER PRIMARY KEY AUTOINCREMENT
--   JSONB               → TEXT (JSON string; serde_json at the app layer)
--   TIMESTAMPTZ         → TEXT (ISO-8601, CURRENT_TIMESTAMP default)
--
-- Why sent_at/read_at are TEXT (not INTEGER epoch-ms like minion_jobs):
--   The minion_jobs scheduling columns (lock_until/delay_until/timeout_at) are
--   compared against `now()` with interval arithmetic, so they store epoch-ms to
--   keep the comparison a plain integer `<`. The inbox timestamps are never used
--   in interval arithmetic — sent_at only orders child_done envelopes and read_at
--   is a NULL/not-NULL flag — so ISO-8601 TEXT (matching created_at/updated_at on
--   minion_jobs) is the right choice.
--
-- Scope: only minion_inbox here. The minion_attachments table (job artifact
-- storage) is a separate follow-up and intentionally NOT created in this file.
--
-- The child_done partial index uses the SQLite `->>` JSON operator (3.38.0+),
-- matching the PG side `payload->>'type' = 'child_done'`.

CREATE TABLE IF NOT EXISTS minion_inbox (
  id       INTEGER PRIMARY KEY AUTOINCREMENT,
  job_id   INTEGER NOT NULL REFERENCES minion_jobs(id) ON DELETE CASCADE,
  sender   TEXT    NOT NULL,
  payload  TEXT    NOT NULL,
  sent_at  TEXT    NOT NULL DEFAULT CURRENT_TIMESTAMP,
  read_at  TEXT
);

CREATE INDEX IF NOT EXISTS idx_minion_inbox_unread ON minion_inbox (job_id) WHERE read_at IS NULL;
CREATE INDEX IF NOT EXISTS idx_minion_inbox_child_done ON minion_inbox (job_id, sent_at) WHERE payload->>'type' = 'child_done';
