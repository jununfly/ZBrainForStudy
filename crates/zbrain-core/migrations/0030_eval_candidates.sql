-- v0.25 (BrainBench-Real substrate): eval_candidates — captured query/search
-- calls from the op-layer wrapper, surfaced by `zbrain eval export|prune|replay`.
-- PII is scrubbed before insert by the (not-yet-wired) capture layer.
-- Ported from TS src/schema.sql (eval_candidates). Capture-side writes are a
-- follow-up (G74 1-1-4); the table starts empty until that lands.
CREATE TABLE IF NOT EXISTS eval_candidates (
  id                    SERIAL PRIMARY KEY,
  tool_name             TEXT         NOT NULL CHECK (tool_name IN ('query', 'search')),
  query                 TEXT         NOT NULL CHECK (length(query) <= 51200),
  retrieved_slugs       TEXT[]       NOT NULL DEFAULT '{}',
  retrieved_chunk_ids   INTEGER[]    NOT NULL DEFAULT '{}',
  source_ids            TEXT[]       NOT NULL DEFAULT '{}',
  expand_enabled        BOOLEAN,
  detail                TEXT         CHECK (detail IS NULL OR detail IN ('low', 'medium', 'high')),
  detail_resolved       TEXT         CHECK (detail_resolved IS NULL OR detail_resolved IN ('low', 'medium', 'high')),
  vector_enabled        BOOLEAN      NOT NULL,
  expansion_applied     BOOLEAN      NOT NULL,
  latency_ms            INTEGER      NOT NULL,
  remote                BOOLEAN      NOT NULL,
  job_id                INTEGER,
  subagent_id           INTEGER,
  created_at            TIMESTAMPTZ  NOT NULL DEFAULT NOW(),
  as_of_ts              TIMESTAMPTZ,
  salience_param        TEXT,
  recency_param         TEXT,
  salience_resolved     TEXT,
  recency_resolved      TEXT,
  salience_source       TEXT,
  recency_source        TEXT,
  embedding_column      TEXT
);
CREATE INDEX IF NOT EXISTS idx_eval_candidates_created_at ON eval_candidates(created_at DESC);
