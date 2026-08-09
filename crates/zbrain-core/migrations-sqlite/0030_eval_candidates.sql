-- v0.25 (BrainBench-Real substrate): eval_candidates (SQLite dialect).
-- Type conversions vs the postgres counterpart:
--   SERIAL              -> INTEGER PRIMARY KEY AUTOINCREMENT
--   TIMESTAMPTZ         -> TEXT (ISO-8601, default datetime('now'))
--   TEXT[] / INTEGER[]  -> JSON TEXT ('[]' default); parsed in the engine layer
-- Capture-side writes are a follow-up (G74 1-1-4); the table starts empty.
CREATE TABLE IF NOT EXISTS eval_candidates (
  id                    INTEGER PRIMARY KEY AUTOINCREMENT,
  tool_name             TEXT         NOT NULL CHECK (tool_name IN ('query', 'search')),
  query                 TEXT         NOT NULL CHECK (length(query) <= 51200),
  retrieved_slugs       TEXT         NOT NULL DEFAULT '[]',
  retrieved_chunk_ids   TEXT         NOT NULL DEFAULT '[]',
  source_ids            TEXT         NOT NULL DEFAULT '[]',
  expand_enabled        INTEGER,
  detail                TEXT         CHECK (detail IS NULL OR detail IN ('low', 'medium', 'high')),
  detail_resolved       TEXT         CHECK (detail_resolved IS NULL OR detail_resolved IN ('low', 'medium', 'high')),
  vector_enabled        BOOLEAN      NOT NULL,
  expansion_applied     BOOLEAN      NOT NULL,
  latency_ms            INTEGER      NOT NULL,
  remote                BOOLEAN      NOT NULL,
  job_id                INTEGER,
  subagent_id           INTEGER,
  created_at            TEXT         NOT NULL DEFAULT (datetime('now')),
  as_of_ts              TEXT,
  salience_param        TEXT,
  recency_param         TEXT,
  salience_resolved     TEXT,
  recency_resolved      TEXT,
  salience_source       TEXT,
  recency_source        TEXT,
  embedding_column      TEXT
);
CREATE INDEX IF NOT EXISTS idx_eval_candidates_created_at ON eval_candidates(created_at DESC);
