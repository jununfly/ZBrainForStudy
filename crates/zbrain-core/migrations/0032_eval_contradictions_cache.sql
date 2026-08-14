-- 1-1-5-8 (JudgeCache): eval_contradictions_cache — persistent judge-verdict
-- cache backing `zbrain eval suspected-contradictions`. Postgres dialect.
-- Mirrors the SQLite migration 0032.
--
-- The primary key is the five-tuple the probe uses to identify a pair under a
-- given judge / prompt / truncation config. (chunk_a_hash, chunk_b_hash) are
-- stored in sorted order by `buildCacheKey`, so (a,b) and (b,a) collide onto
-- the same row. `ON CONFLICT DO UPDATE` slides `expires_at` forward when the
-- same pair is re-judged, keeping the table bounded without a cron.
CREATE TABLE IF NOT EXISTS eval_contradictions_cache (
  chunk_a_hash      TEXT    NOT NULL,
  chunk_b_hash      TEXT    NOT NULL,
  model_id          TEXT    NOT NULL,
  prompt_version    TEXT    NOT NULL,
  truncation_policy TEXT    NOT NULL,
  verdict           JSONB   NOT NULL,
  created_at        TIMESTAMPTZ NOT NULL DEFAULT NOW(),
  expires_at        TIMESTAMPTZ NOT NULL,
  PRIMARY KEY (chunk_a_hash, chunk_b_hash, model_id, prompt_version, truncation_policy)
);
CREATE INDEX IF NOT EXISTS idx_eval_contradictions_cache_expires
  ON eval_contradictions_cache(expires_at);
