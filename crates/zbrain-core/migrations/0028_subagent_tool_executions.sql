-- Subagent tool-execution log (1-3-4-6).
-- Mirrors TS `subagent_tool_executions` (src/schema.sql:809).
-- Read by the synthesize phase to collect child put_page slugs
-- (tool_name = 'brain_put_page' AND status = 'complete').
--
-- NOTE: FK to minion_jobs is intentionally omitted. The Rust minion does not
-- yet write this table (write path registered in docs/plans/KNOWN-GAPS.md
-- (G63); only the read path is wired here). Adding the FK would couple this
-- migration to the minion_jobs schema and provide no enforcement value until
-- the writer lands.
CREATE TABLE IF NOT EXISTS subagent_tool_executions (
  id                 BIGSERIAL PRIMARY KEY,
  job_id             BIGINT NOT NULL,
  message_idx        INTEGER,
  tool_use_id        TEXT,
  tool_name          TEXT,
  input              JSONB NOT NULL,
  status             TEXT,
  output             JSONB,
  error              TEXT,
  schema_version     INTEGER DEFAULT 1,
  provider_id        TEXT,
  ordinal            INTEGER,
  zbrain_tool_use_id UUID,
  started_at         TIMESTAMPTZ DEFAULT now(),
  ended_at           TIMESTAMPTZ,
  CONSTRAINT subagent_tool_executions_stable_id UNIQUE (job_id, tool_use_id),
  CONSTRAINT subagent_tool_executions_ordinal UNIQUE (job_id, message_idx, ordinal),
  CONSTRAINT subagent_tool_executions_status CHECK (status IN ('pending', 'complete', 'failed'))
);
