-- 0014: Create `minion_jobs` table — BullMQ-inspired Postgres-native job queue.
--
-- This is the engine-layer persistence for the minion/job domain (Phase 9).
-- A job progresses through a lifecycle state machine:
--   waiting → active → completed | failed | delayed | dead | cancelled
--   (waiting-children / paused are dependency/pause states, added later)
--
-- TS reference:
--   - schema:     src/schema.sql L685-735 (minion_jobs table)
--   - MinionJob:  src/core/minions/types.ts L34-92
--   - queue ops:  src/core/minions/queue.ts (add/claim/complete/fail/…)
--
-- Slice scope (roadmap 1-1-1, A+B = foundation + concurrency core):
--   This migration creates the FULL 40-column table in one shot so later
--   slices (1-1-2 sweep, 1-1-3 dependencies/inbox/attachments) never have to
--   ALTER the table — the CHECK constraints, self-referencing FK, and 7
--   partial indexes are one atomic design unit. Columns unused by A+B
--   (depth/timeout/tokens/quiet_hours/…) simply sit at their DEFAULTs.
--
--   The sibling tables `minion_inbox` and `minion_attachments` are NOT created
--   here — they are physically independent tables owned by the D-layer
--   (1-1-3) and A+B never references them.
--
-- Design decisions:
--   - id SERIAL (matches TS; INTEGER PK AUTOINCREMENT on the SQLite side).
--   - data/result/progress/stacktrace JSONB (TEXT on SQLite, serde_json at
--     the app layer).
--   - TIMESTAMPTZ scheduling columns (lock_until/delay_until/timeout_at) use
--     `now() + interval` arithmetic here; the SQLite port stores them as
--     INTEGER epoch-ms and does the arithmetic in Rust (no interval type).
--   - Self-referencing parent_job_id FK (ON DELETE SET NULL) is created now;
--     the dependency-graph logic that uses it lands in 1-1-3.
--   - backoff_jitter is DOUBLE PRECISION (f64), NOT REAL/float4: the Rust
--     MinionJob.backoff_jitter is f64 and 0.2 does not survive a float4
--     round-trip within 1e-9, so REAL would break exact-value assertions and
--     diverge from the SQLite REAL (8-byte) port. Do not narrow it back.

CREATE TABLE IF NOT EXISTS minion_jobs (
  id               SERIAL PRIMARY KEY,
  name             TEXT        NOT NULL,
  queue            TEXT        NOT NULL DEFAULT 'default',
  status           TEXT        NOT NULL DEFAULT 'waiting',
  priority         INTEGER     NOT NULL DEFAULT 0,
  data             JSONB       NOT NULL DEFAULT '{}',
  max_attempts     INTEGER     NOT NULL DEFAULT 3,
  attempts_made    INTEGER     NOT NULL DEFAULT 0,
  attempts_started INTEGER     NOT NULL DEFAULT 0,
  backoff_type     TEXT        NOT NULL DEFAULT 'exponential',
  backoff_delay    INTEGER     NOT NULL DEFAULT 1000,
  backoff_jitter   DOUBLE PRECISION NOT NULL DEFAULT 0.2,
  stalled_counter  INTEGER     NOT NULL DEFAULT 0,
  max_stalled      INTEGER     NOT NULL DEFAULT 5,
  lock_token       TEXT,
  lock_until       TIMESTAMPTZ,
  delay_until      TIMESTAMPTZ,
  parent_job_id    INTEGER     REFERENCES minion_jobs(id) ON DELETE SET NULL,
  on_child_fail    TEXT        NOT NULL DEFAULT 'fail_parent',
  tokens_input     INTEGER     NOT NULL DEFAULT 0,
  tokens_output    INTEGER     NOT NULL DEFAULT 0,
  tokens_cache_read INTEGER    NOT NULL DEFAULT 0,
  result           JSONB,
  progress         JSONB,
  error_text       TEXT,
  stacktrace       JSONB       DEFAULT '[]',
  depth            INTEGER     NOT NULL DEFAULT 0,
  max_children     INTEGER,
  timeout_ms       INTEGER,
  timeout_at       TIMESTAMPTZ,
  remove_on_complete BOOLEAN   NOT NULL DEFAULT FALSE,
  remove_on_fail   BOOLEAN     NOT NULL DEFAULT FALSE,
  idempotency_key  TEXT,
  quiet_hours      JSONB,
  stagger_key      TEXT,
  created_at       TIMESTAMPTZ NOT NULL DEFAULT now(),
  started_at       TIMESTAMPTZ,
  finished_at      TIMESTAMPTZ,
  updated_at       TIMESTAMPTZ NOT NULL DEFAULT now(),
  CONSTRAINT chk_minion_status CHECK (status IN ('waiting','active','completed','failed','delayed','dead','cancelled','waiting-children','paused')),
  CONSTRAINT chk_minion_backoff_type CHECK (backoff_type IN ('fixed','exponential')),
  CONSTRAINT chk_minion_on_child_fail CHECK (on_child_fail IN ('fail_parent','remove_dep','ignore','continue')),
  CONSTRAINT chk_minion_jitter_range CHECK (backoff_jitter >= 0.0 AND backoff_jitter <= 1.0),
  CONSTRAINT chk_minion_attempts_order CHECK (attempts_made <= attempts_started),
  CONSTRAINT chk_minion_nonnegative CHECK (attempts_made >= 0 AND attempts_started >= 0 AND stalled_counter >= 0 AND max_attempts >= 1 AND max_stalled >= 0),
  CONSTRAINT chk_minion_depth_nonnegative CHECK (depth >= 0),
  CONSTRAINT chk_minion_max_children_positive CHECK (max_children IS NULL OR max_children > 0),
  CONSTRAINT chk_minion_timeout_positive CHECK (timeout_ms IS NULL OR timeout_ms > 0)
);

-- Claim path: pull the highest-priority waiting job (priority ASC, FIFO tie-break).
CREATE INDEX IF NOT EXISTS idx_minion_jobs_claim ON minion_jobs (queue, priority ASC, created_at ASC) WHERE status = 'waiting';
CREATE INDEX IF NOT EXISTS idx_minion_jobs_status ON minion_jobs(status);
CREATE INDEX IF NOT EXISTS idx_minion_jobs_stalled ON minion_jobs (lock_until) WHERE status = 'active';
CREATE INDEX IF NOT EXISTS idx_minion_jobs_delayed ON minion_jobs (delay_until) WHERE status = 'delayed';
CREATE INDEX IF NOT EXISTS idx_minion_jobs_parent ON minion_jobs(parent_job_id);
CREATE INDEX IF NOT EXISTS idx_minion_jobs_timeout ON minion_jobs (timeout_at) WHERE status = 'active' AND timeout_at IS NOT NULL;
CREATE INDEX IF NOT EXISTS idx_minion_jobs_parent_status ON minion_jobs (parent_job_id, status) WHERE parent_job_id IS NOT NULL;
CREATE UNIQUE INDEX IF NOT EXISTS uniq_minion_jobs_idempotency ON minion_jobs (idempotency_key) WHERE idempotency_key IS NOT NULL;
