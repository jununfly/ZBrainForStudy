-- 0016: Create `minion_attachments` table — SQLite port of per-job blob storage.
--
-- Mirrors migrations/0016_minion_attachments.sql (PostgreSQL). Type mapping:
--   SERIAL PRIMARY KEY  → INTEGER PRIMARY KEY AUTOINCREMENT
--   BYTEA               → BLOB
--   TIMESTAMPTZ         → TEXT (ISO-8601, CURRENT_TIMESTAMP default)
--
-- Why created_at is TEXT (not INTEGER epoch-ms like minion_jobs scheduling
-- columns): created_at only records insertion order and is never compared
-- against now() with interval arithmetic, so ISO-8601 TEXT (matching
-- minion_inbox.sent_at) is the right choice.
--
-- UNIQUE(job_id, filename) is the authoritative dedupe backstop; the app-layer
-- existing-filename check is only a friendly early-out. The two CHECK
-- constraints (storage channel present + non-negative size) carry over verbatim
-- from the PG side.
--
-- The PG `ALTER TABLE ... SET STORAGE EXTERNAL` line is a TOAST tuning no-op on
-- SQLite and is intentionally omitted here.
--
-- Requires PRAGMA foreign_keys = ON at connection time for the ON DELETE CASCADE
-- to fire (the engine sets this on connect).

CREATE TABLE IF NOT EXISTS minion_attachments (
  id            INTEGER PRIMARY KEY AUTOINCREMENT,
  job_id        INTEGER NOT NULL REFERENCES minion_jobs(id) ON DELETE CASCADE,
  filename      TEXT NOT NULL,
  content_type  TEXT NOT NULL,
  content       BLOB,
  storage_uri   TEXT,
  size_bytes    INTEGER NOT NULL,
  sha256        TEXT NOT NULL,
  created_at    TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
  CONSTRAINT uniq_minion_attachments_job_filename UNIQUE (job_id, filename),
  CONSTRAINT chk_attachment_storage CHECK (content IS NOT NULL OR storage_uri IS NOT NULL),
  CONSTRAINT chk_attachment_size CHECK (size_bytes >= 0)
);

CREATE INDEX IF NOT EXISTS idx_minion_attachments_job ON minion_attachments (job_id);
