-- 0017: Add budget columns to minion_jobs + create minion_budget_log table
--       — SQLite port of migrations/0017_minion_budget.sql.
--
-- Type mapping:
--   SERIAL PRIMARY KEY  → INTEGER PRIMARY KEY AUTOINCREMENT
--   TIMESTAMPTZ         → TEXT (ISO-8601, CURRENT_TIMESTAMP default)
--
-- Requires PRAGMA foreign_keys = ON at connection time for ON DELETE
-- CASCADE / ON DELETE SET NULL to fire (the engine sets this on connect).
--
-- Mirrors the Postgres migration's design decisions verbatim — see
-- migrations/0017_minion_budget.sql for full rationale.

-- (1) Add budget columns to minion_jobs
ALTER TABLE minion_jobs ADD COLUMN budget_remaining_cents INTEGER;
ALTER TABLE minion_jobs ADD COLUMN budget_owner_job_id INTEGER REFERENCES minion_jobs(id) ON DELETE SET NULL;

CREATE INDEX IF NOT EXISTS idx_minion_jobs_budget_owner ON minion_jobs (budget_owner_job_id)
  WHERE budget_owner_job_id IS NOT NULL;
CREATE INDEX IF NOT EXISTS idx_minion_jobs_budget_remaining ON minion_jobs (budget_remaining_cents)
  WHERE budget_remaining_cents IS NOT NULL;

-- (2) Create minion_budget_log — immutable audit trail
CREATE TABLE IF NOT EXISTS minion_budget_log (
  id           INTEGER PRIMARY KEY AUTOINCREMENT,
  job_id       INTEGER NOT NULL REFERENCES minion_jobs(id) ON DELETE CASCADE,
  cents_delta  INTEGER NOT NULL,
  reason       TEXT NOT NULL,
  created_at   TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
  CONSTRAINT chk_minion_budget_log_nonzero CHECK (cents_delta != 0)
);

CREATE INDEX IF NOT EXISTS idx_minion_budget_log_job ON minion_budget_log (job_id);
CREATE INDEX IF NOT EXISTS idx_minion_budget_log_created ON minion_budget_log (created_at);
