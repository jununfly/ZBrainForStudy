-- Engine key/value config store (1-3-4-6).
-- Backs dream.synthesize.* settings and cooldown timestamps.
-- Mirrors TS `config` table (src/schema.sql:462).
CREATE TABLE IF NOT EXISTS config (
  key   TEXT PRIMARY KEY,
  value TEXT NOT NULL
);
