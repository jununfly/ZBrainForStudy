-- v0.36+ autonomous-remediation wave: shared checkpoint table for
-- long-running ops. Ported from src/schema.sql (the Rust migration set did
-- not previously include it). completed_keys is a JSON array of op-defined
-- string keys (e.g. "<sourceId>|<slug>|<endIso>" for extract-conversation-facts).
-- SQLite has no JSONB type, so completed_keys is stored as TEXT (a JSON array).
CREATE TABLE IF NOT EXISTS op_checkpoints (
  op             TEXT NOT NULL,
  fingerprint    TEXT NOT NULL,
  completed_keys TEXT NOT NULL DEFAULT '[]',
  updated_at     TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
  PRIMARY KEY (op, fingerprint)
);
