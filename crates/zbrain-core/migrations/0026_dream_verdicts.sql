-- Dream-cycle significance verdict cache (1-3-4-1)
-- Ported from TS canonical schema (src/schema.sql dream_verdicts, v0.23).
-- Idempotency key is the composite primary key (file_path, content_hash).

CREATE TABLE IF NOT EXISTS dream_verdicts (
  file_path        TEXT        NOT NULL,
  content_hash     TEXT        NOT NULL,
  worth_processing BOOLEAN     NOT NULL,
  reasons          JSONB,
  judged_at        TIMESTAMPTZ NOT NULL DEFAULT now(),
  PRIMARY KEY (file_path, content_hash)
);
