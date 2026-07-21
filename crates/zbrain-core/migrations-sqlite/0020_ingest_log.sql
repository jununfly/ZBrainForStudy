-- 1-6-7-5: ingest log (log_ingest / get_ingest_log ops).
-- Mirrors TS `ingest_log` (src/schema.sql).
CREATE TABLE IF NOT EXISTS ingest_log (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  source_id TEXT NOT NULL DEFAULT 'default',
  source_type TEXT NOT NULL,
  source_ref TEXT NOT NULL,
  pages_updated TEXT NOT NULL DEFAULT '[]',
  summary TEXT NOT NULL DEFAULT '',
  created_at TEXT NOT NULL DEFAULT (datetime('now'))
);
CREATE INDEX IF NOT EXISTS idx_ingest_log_created ON ingest_log(created_at DESC);
