-- 1-1-5-3 (#319 A2): eval_takes_quality_runs — one row per takes-quality run
-- (Postgres dialect). Mirrors the SQLite migration 0034. Backs
-- `zbrain eval takes-quality trend` (and replay --from-db). The full receipt
-- is stored as JSONB; the 4-sha columns form the idempotent unique key.
CREATE TABLE IF NOT EXISTS eval_takes_quality_runs (
  run_id                     TEXT    PRIMARY KEY,
  ran_at                     TIMESTAMPTZ NOT NULL DEFAULT NOW(),
  schema_version             INTEGER NOT NULL DEFAULT 1,
  rubric_version             TEXT    NOT NULL,
  verdict                    TEXT    NOT NULL,
  overall_score              DOUBLE PRECISION,
  cost_usd                   DOUBLE PRECISION NOT NULL,
  corpus_sha8                TEXT    NOT NULL,
  receipt_sha8_corpus        TEXT    NOT NULL,
  receipt_sha8_prompt        TEXT    NOT NULL,
  receipt_sha8_models        TEXT    NOT NULL,
  receipt_sha8_rubric        TEXT    NOT NULL,
  receipt_json               JSONB   NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_eval_takes_quality_runs_ran_at
  ON eval_takes_quality_runs(ran_at DESC);
