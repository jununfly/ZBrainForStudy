-- Subagent tool-execution log (1-3-4-6), SQLite dialect.
-- Type conversions for SQLite (no jsonb / timestamptz / uuid / bigserial):
--   JSONB       → TEXT
--   TIMESTAMPTZ → TEXT (ISO-8601 string)
--   UUID        → TEXT
--   BIGSERIAL   → INTEGER PRIMARY KEY AUTOINCREMENT
--
-- Mirrors TS `subagent_tool_executions` (src/schema.sql:809). FK to
-- minion_jobs omitted — see the postgres counterpart for rationale.
CREATE TABLE IF NOT EXISTS subagent_tool_executions (
  id                 INTEGER PRIMARY KEY AUTOINCREMENT,
  job_id             INTEGER NOT NULL,
  message_idx        INTEGER,
  tool_use_id        TEXT,
  tool_name          TEXT,
  input              TEXT NOT NULL,
  status             TEXT,
  output             TEXT,
  error              TEXT,
  schema_version     INTEGER DEFAULT 1,
  provider_id        TEXT,
  ordinal            INTEGER,
  zbrain_tool_use_id TEXT,
  started_at         TEXT DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ', 'now')),
  ended_at           TEXT,
  CONSTRAINT subagent_tool_executions_stable_id UNIQUE (job_id, tool_use_id),
  CONSTRAINT subagent_tool_executions_ordinal UNIQUE (job_id, message_idx, ordinal),
  CONSTRAINT subagent_tool_executions_status CHECK (status IN ('pending', 'complete', 'failed'))
);
