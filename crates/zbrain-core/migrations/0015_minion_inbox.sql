-- 0015: Create `minion_inbox` table — sidechannel messaging for the minion queue.
--
-- This is the engine-layer persistence for the minion queue D-layer:
-- parent<->child coordination + admin/parent sidechannel messages.
--
-- TS reference:
--   - schema:     src/schema.sql L746-756 (minion_inbox table)
--   - queue ops:  src/core/minions/queue.ts sendMessage/readInbox/readChildCompletions
--
-- Message model:
--   Every row is one message delivered to job_id's inbox. `sender` is either
--   'admin', 'minions' (automatic child_done hook), or a parent job id string.
--   `payload` is an arbitrary JSON envelope; the automatic child-completion
--   notification carries `{"type":"child_done", ...}` and is the only shape the
--   queue itself introspects (via the child_done partial index below).
--   `read_at` is NULL until readInbox marks the message consumed; the unread
--   partial index keeps the "any pending messages?" probe cheap.
--
-- Scope: only minion_inbox is created here. The sibling table
-- minion_attachments is physically independent (no jobs/inbox coupling) and
-- gets its own migration; do NOT add it here.
--
-- Design decisions (mirror the minion_jobs port, migrations/0014):
--   - id SERIAL (INTEGER PK AUTOINCREMENT on SQLite).
--   - payload JSONB (TEXT on SQLite, serde_json at the app layer).
--   - sent_at/read_at TIMESTAMPTZ record columns (TEXT ISO-8601 on SQLite).
--     Unlike the minion_jobs *scheduling* columns, these are never compared
--     against `now()` with interval arithmetic, so ISO-8601 TEXT is fine on the
--     SQLite side — no epoch-ms integer needed.
--   - job_id FK ON DELETE CASCADE: an inbox message has no meaning once its job
--     row is gone (contrast minion_jobs.parent_job_id which is SET NULL because
--     an orphaned child is still a valid, runnable job).

CREATE TABLE IF NOT EXISTS minion_inbox (
  id       SERIAL PRIMARY KEY,
  job_id   INTEGER     NOT NULL REFERENCES minion_jobs(id) ON DELETE CASCADE,
  sender   TEXT        NOT NULL,
  payload  JSONB       NOT NULL,
  sent_at  TIMESTAMPTZ NOT NULL DEFAULT now(),
  read_at  TIMESTAMPTZ
);

-- Unread probe: "does this job have any pending messages?" — hot path for
-- readInbox and worker polling. Partial index over unread rows only.
CREATE INDEX IF NOT EXISTS idx_minion_inbox_unread ON minion_inbox (job_id) WHERE read_at IS NULL;

-- Child-completion cursor: readChildCompletions scans child_done envelopes for a
-- parent in send order. Partial index keeps it to the child_done rows only.
CREATE INDEX IF NOT EXISTS idx_minion_inbox_child_done ON minion_inbox (job_id, sent_at) WHERE payload->>'type' = 'child_done';
