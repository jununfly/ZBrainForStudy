-- 1-1-5-6 (#62 trend): eval_contradictions_runs — one row per probe run
-- (Postgres dialect). Mirrors the SQLite migration 0031.
CREATE TABLE IF NOT EXISTS eval_contradictions_runs (
  run_id                    TEXT    PRIMARY KEY,
  ran_at                    TIMESTAMPTZ NOT NULL DEFAULT NOW(),
  schema_version            INTEGER NOT NULL DEFAULT 1,
  judge_model               TEXT    NOT NULL,
  prompt_version            TEXT    NOT NULL,
  queries_evaluated         INTEGER NOT NULL,
  queries_with_contradiction INTEGER NOT NULL,
  total_contradictions_flagged INTEGER NOT NULL,
  wilson_ci_lower           DOUBLE PRECISION NOT NULL,
  wilson_ci_upper           DOUBLE PRECISION NOT NULL,
  judge_errors_total        INTEGER NOT NULL,
  cost_usd_total            DOUBLE PRECISION NOT NULL,
  duration_ms               BIGINT NOT NULL,
  source_tier_breakdown     JSONB   NOT NULL DEFAULT '{}'::jsonb,
  report_json               JSONB   NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_eval_contradictions_runs_ran_at
  ON eval_contradictions_runs(ran_at DESC);
