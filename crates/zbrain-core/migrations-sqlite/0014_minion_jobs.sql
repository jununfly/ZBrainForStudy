-- 0014: Create `minion_jobs` table — SQLite port of the BullMQ-inspired queue.
--
-- Mirrors migrations/0014_minion_jobs.sql (PostgreSQL). Type mapping:
--   SERIAL PRIMARY KEY  → INTEGER PRIMARY KEY AUTOINCREMENT
--   JSONB               → TEXT (JSON string; serde_json at the app layer)
--   DOUBLE PRECISION    → REAL (SQLite REAL is an 8-byte IEEE float = f64, so
--                          backoff_jitter round-trips exactly; PG uses DOUBLE
--                          PRECISION rather than REAL/float4 for the same reason)
--   BOOLEAN             → INTEGER (0/1)
--   TIMESTAMPTZ (record columns: created_at/updated_at/started_at/finished_at)
--                       → TEXT (ISO-8601, CURRENT_TIMESTAMP default)
--   TIMESTAMPTZ (scheduling columns: lock_until/delay_until/timeout_at)
--                       → INTEGER (Unix epoch MILLISECONDS)
--
-- Why the scheduling columns are INTEGER, not TEXT:
--   The PG side computes these with `now() + N * interval '1 millisecond'` and
--   compares them with `< now()`. SQLite has no interval type, so the
--   arithmetic must move into Rust regardless. Given that, storing epoch-ms
--   makes the comparison a plain integer `<` — no lexicographic-ordering trap,
--   no timezone/format ambiguity, no zero-padding risk. row_to_job on both
--   backends normalizes these to the same Rust type (Option<i64> epoch-ms).
--   (roadmap 1-1-1 decision 5)
--
-- Slice scope (roadmap 1-1-1, A+B): full 40-column table in one shot; the
-- sibling tables minion_inbox / minion_attachments are deferred to 1-1-3.
--
-- CHECK constraints are inlined in the CREATE (SQLite cannot ADD CONSTRAINT
-- after the fact without a 12-step table rebuild). Partial indexes ARE
-- supported by SQLite (3.8.0+) and are kept 1:1 with the PG side.

CREATE TABLE IF NOT EXISTS minion_jobs (
  id               INTEGER PRIMARY KEY AUTOINCREMENT,
  name             TEXT    NOT NULL,
  queue            TEXT    NOT NULL DEFAULT 'default',
  status           TEXT    NOT NULL DEFAULT 'waiting',
  priority         INTEGER NOT NULL DEFAULT 0,
  data             TEXT    NOT NULL DEFAULT '{}',
  max_attempts     INTEGER NOT NULL DEFAULT 3,
  attempts_made    INTEGER NOT NULL DEFAULT 0,
  attempts_started INTEGER NOT NULL DEFAULT 0,
  backoff_type     TEXT    NOT NULL DEFAULT 'exponential',
  backoff_delay    INTEGER NOT NULL DEFAULT 1000,
  backoff_jitter   REAL    NOT NULL DEFAULT 0.2,
  stalled_counter  INTEGER NOT NULL DEFAULT 0,
  max_stalled      INTEGER NOT NULL DEFAULT 5,
  lock_token       TEXT,
  lock_until       INTEGER,
  delay_until      INTEGER,
  parent_job_id    INTEGER REFERENCES minion_jobs(id) ON DELETE SET NULL,
  on_child_fail    TEXT    NOT NULL DEFAULT 'fail_parent',
  tokens_input     INTEGER NOT NULL DEFAULT 0,
  tokens_output    INTEGER NOT NULL DEFAULT 0,
  tokens_cache_read INTEGER NOT NULL DEFAULT 0,
  result           TEXT,
  progress         TEXT,
  error_text       TEXT,
  stacktrace       TEXT    DEFAULT '[]',
  depth            INTEGER NOT NULL DEFAULT 0,
  max_children     INTEGER,
  timeout_ms       INTEGER,
  timeout_at       INTEGER,
  remove_on_complete INTEGER NOT NULL DEFAULT 0,
  remove_on_fail   INTEGER NOT NULL DEFAULT 0,
  idempotency_key  TEXT,
  quiet_hours      TEXT,
  stagger_key      TEXT,
  created_at       TEXT    NOT NULL DEFAULT CURRENT_TIMESTAMP,
  started_at       TEXT,
  finished_at      TEXT,
  updated_at       TEXT    NOT NULL DEFAULT CURRENT_TIMESTAMP,
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

CREATE INDEX IF NOT EXISTS idx_minion_jobs_claim ON minion_jobs (queue, priority ASC, created_at ASC) WHERE status = 'waiting';
CREATE INDEX IF NOT EXISTS idx_minion_jobs_status ON minion_jobs(status);
CREATE INDEX IF NOT EXISTS idx_minion_jobs_stalled ON minion_jobs (lock_until) WHERE status = 'active';
CREATE INDEX IF NOT EXISTS idx_minion_jobs_delayed ON minion_jobs (delay_until) WHERE status = 'delayed';
CREATE INDEX IF NOT EXISTS idx_minion_jobs_parent ON minion_jobs(parent_job_id);
CREATE INDEX IF NOT EXISTS idx_minion_jobs_timeout ON minion_jobs (timeout_at) WHERE status = 'active' AND timeout_at IS NOT NULL;
CREATE INDEX IF NOT EXISTS idx_minion_jobs_parent_status ON minion_jobs (parent_job_id, status) WHERE parent_job_id IS NOT NULL;
CREATE UNIQUE INDEX IF NOT EXISTS uniq_minion_jobs_idempotency ON minion_jobs (idempotency_key) WHERE idempotency_key IS NOT NULL;
