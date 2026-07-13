-- 0017: Add budget columns to minion_jobs + create minion_budget_log table.
--
-- Budged management for minion jobs (roadmap 1-3-2). Two additions:
--   1. ALTER minion_jobs: add budget_remaining_cents and budget_owner_job_id
--      columns. budget_owner_job_id is a self-referencing FK to minion_jobs(id)
--      that indicates which job "owns" the budget pool. A job can be its own
--      owner (budget_owner_job_id = id) or inherit from a parent.
--   2. CREATE minion_budget_log: immutable audit trail of all budget mutations.
--      cents_delta is positive for charges (reserve), negative for refunds.
--
-- TS reference: the libsql watch_snapshot query already references these
-- columns/tables — this migration makes the query functional where it was
-- previously a silent no-op (Err(_) => vec![]).
--
-- Design decisions (grill 1-3-2):
--   - Single migration file (not split per table).
--   - Column name cents_delta (not amount_cents) to reflect signed delta.
--   - PG CAS reserve uses UPDATE WHERE budget_remaining_cents >= amount
--     without an explicit transaction; the WHERE clause is the atomic guard.
--   - ON DELETE CASCADE on minion_budget_log.job_id so pruning jobs
--     cleans up their audit trail automatically.
--   - ON DELETE SET NULL on budget_owner_job_id so deleting the owner
--     does not cascade-delete the owned jobs.

-- (1) Add budget columns to minion_jobs
ALTER TABLE minion_jobs ADD COLUMN IF NOT EXISTS budget_remaining_cents INTEGER;
ALTER TABLE minion_jobs ADD COLUMN IF NOT EXISTS budget_owner_job_id INTEGER REFERENCES minion_jobs(id) ON DELETE SET NULL;

CREATE INDEX IF NOT EXISTS idx_minion_jobs_budget_owner ON minion_jobs (budget_owner_job_id)
  WHERE budget_owner_job_id IS NOT NULL;
CREATE INDEX IF NOT EXISTS idx_minion_jobs_budget_remaining ON minion_jobs (budget_remaining_cents)
  WHERE budget_remaining_cents IS NOT NULL;

-- (2) Create minion_budget_log — immutable audit trail
CREATE TABLE IF NOT EXISTS minion_budget_log (
  id           SERIAL PRIMARY KEY,
  job_id       INTEGER NOT NULL REFERENCES minion_jobs(id) ON DELETE CASCADE,
  cents_delta  INTEGER NOT NULL,
  reason       TEXT NOT NULL,
  created_at   TIMESTAMPTZ NOT NULL DEFAULT now(),
  CONSTRAINT chk_minion_budget_log_nonzero CHECK (cents_delta != 0)
);

CREATE INDEX IF NOT EXISTS idx_minion_budget_log_job ON minion_budget_log (job_id);
CREATE INDEX IF NOT EXISTS idx_minion_budget_log_created ON minion_budget_log (created_at);
