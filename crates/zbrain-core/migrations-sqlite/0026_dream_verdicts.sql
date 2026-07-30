-- Dream-cycle significance verdict cache (1-3-4-1), SQLite dialect
-- Ported from TS canonical schema, with type conversions for SQLite
-- (no timestamptz/jsonb):
--   TIMESTAMPTZ → TEXT (ISO-8601 string)
--   JSONB → TEXT (JSON-encoded array)
--   BOOLEAN → INTEGER (0/1, default 0)

CREATE TABLE IF NOT EXISTS dream_verdicts (
  file_path        TEXT        NOT NULL,
  content_hash     TEXT        NOT NULL,
  worth_processing BOOLEAN     NOT NULL DEFAULT 0,
  reasons          TEXT, -- JSON-encoded array
  judged_at        TEXT        NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ', 'now')),
  PRIMARY KEY (file_path, content_hash)
);
