-- 1-1-5-6 (#62 trend): eval_contradictions_runs — one row per probe run,
-- backing `zbrain eval suspected-contradictions trend`. SQLite dialect.
-- Mirrors TS `eval_contradictions_runs` (src/schema.sql, v0.34 / Lane A2).
-- `ran_at` defaults to now(); the Rust writer also supplies an explicit
-- ISO-8601 value so the column is populated identically across backends.
CREATE TABLE IF NOT EXISTS eval_contradictions_runs (
  run_id                    TEXT    PRIMARY KEY,
  ran_at                    TEXT    NOT NULL DEFAULT (datetime('now')),
  schema_version            INTEGER NOT NULL DEFAULT 1,
  judge_model               TEXT    NOT NULL,
  prompt_version            TEXT    NOT NULL,
  queries_evaluated         INTEGER NOT NULL,
  queries_with_contradiction INTEGER NOT NULL,
  total_contradictions_flagged INTEGER NOT NULL,
  wilson_ci_lower           REAL    NOT NULL,
  wilson_ci_upper           REAL    NOT NULL,
  judge_errors_total        INTEGER NOT NULL,
  cost_usd_total            REAL    NOT NULL,
  duration_ms               INTEGER NOT NULL,
  source_tier_breakdown     TEXT    NOT NULL DEFAULT '{}',
  report_json               TEXT    NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_eval_contradictions_runs_ran_at
  ON eval_contradictions_runs(ran_at DESC);
