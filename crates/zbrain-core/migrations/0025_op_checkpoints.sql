-- v0.36+ autonomous-remediation wave: shared checkpoint table for
-- long-running ops. Ported from src/schema.sql (the Rust migration set did
-- not previously include it). completed_keys is a JSON array of op-defined
-- string keys (e.g. "<sourceId>|<slug>|<endIso>" for extract-conversation-facts).
CREATE TABLE IF NOT EXISTS op_checkpoints (
  op             TEXT NOT NULL,
  fingerprint    TEXT NOT NULL,
  completed_keys JSONB NOT NULL DEFAULT '[]'::jsonb,
  updated_at     TIMESTAMPTZ NOT NULL DEFAULT now(),
  PRIMARY KEY (op, fingerprint)
);
