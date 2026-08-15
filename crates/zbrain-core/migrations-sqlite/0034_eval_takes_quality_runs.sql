-- 1-1-5-3 (#319 A2): eval_takes_quality_runs — one row per takes-quality run,
-- backing `zbrain eval takes-quality trend` (SQLite dialect). Mirrors TS
-- `eval_takes_quality_runs` (Lane A2). `ran_at` defaults to now(); the Rust
-- writer also supplies an explicit ISO-8601 value so the column is populated
-- identically across backends. The full receipt is stored as TEXT JSON; the
-- 4-sha columns form the idempotent unique key.
CREATE TABLE IF NOT EXISTS eval_takes_quality_runs (
  run_id                     TEXT    PRIMARY KEY,
  ran_at                     TEXT    NOT NULL DEFAULT (datetime('now')),
  schema_version             INTEGER NOT NULL DEFAULT 1,
  rubric_version             TEXT    NOT NULL,
  verdict                    TEXT    NOT NULL,
  overall_score              REAL,
  cost_usd                   REAL    NOT NULL,
  corpus_sha8                TEXT    NOT NULL,
  receipt_sha8_corpus        TEXT    NOT NULL,
  receipt_sha8_prompt        TEXT    NOT NULL,
  receipt_sha8_models        TEXT    NOT NULL,
  receipt_sha8_rubric        TEXT    NOT NULL,
  receipt_json               TEXT    NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_eval_takes_quality_runs_ran_at
  ON eval_takes_quality_runs(ran_at DESC);
