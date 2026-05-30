-- 0003_pages_full_columns.sql
--
-- Slice #110-a (DDL only): extend `pages` to full-column parity with libsql
-- migration 0003 + install the `bump_page_generation` trigger so that the
-- cache-bookmark gate has authoritative monotonic `generation` values.
--
-- Mirrors:
--   - libsql migrations-sqlite/0002_pages_full_columns.sql (column shape)
--   - libsql migrations-sqlite/0003_salience_and_full_generation_trigger.sql
--     (10-column allow-list + monotonic bump on INSERT)
--   - TS source-of-truth: src/core/pglite-schema.ts lines 60-159
--
-- IMPORTANT: this slice is DDL only. Production decoder/projection updates
-- live in slice #110-b. Local three-color verification will pass with PG tests
-- skipped (no ZBRAIN_TEST_PG_URL); CI will exercise the migration.

-- ---------------------------------------------------------------------------
-- 1. Add the 19 columns missing from `pages` after 0001 + 0002.
-- ---------------------------------------------------------------------------

ALTER TABLE pages ADD COLUMN IF NOT EXISTS frontmatter JSONB;
ALTER TABLE pages ADD COLUMN IF NOT EXISTS content_hash TEXT;
ALTER TABLE pages ADD COLUMN IF NOT EXISTS emotional_weight DOUBLE PRECISION;
ALTER TABLE pages ADD COLUMN IF NOT EXISTS effective_date TEXT;
ALTER TABLE pages ADD COLUMN IF NOT EXISTS effective_date_source TEXT;
ALTER TABLE pages ADD COLUMN IF NOT EXISTS import_filename TEXT;
ALTER TABLE pages ADD COLUMN IF NOT EXISTS chunker_version INTEGER;
ALTER TABLE pages ADD COLUMN IF NOT EXISTS source_path TEXT;
ALTER TABLE pages ADD COLUMN IF NOT EXISTS source_kind TEXT;
ALTER TABLE pages ADD COLUMN IF NOT EXISTS source_uri TEXT;
ALTER TABLE pages ADD COLUMN IF NOT EXISTS ingested_via TEXT;
ALTER TABLE pages ADD COLUMN IF NOT EXISTS ingested_at TIMESTAMPTZ;
ALTER TABLE pages ADD COLUMN IF NOT EXISTS salience_touched_at TIMESTAMPTZ;
ALTER TABLE pages ADD COLUMN IF NOT EXISTS salience_score DOUBLE PRECISION;
ALTER TABLE pages ADD COLUMN IF NOT EXISTS last_retrieved_at TIMESTAMPTZ;
ALTER TABLE pages ADD COLUMN IF NOT EXISTS contextual_retrieval_mode TEXT;
ALTER TABLE pages ADD COLUMN IF NOT EXISTS corpus_generation INTEGER;
ALTER TABLE pages ADD COLUMN IF NOT EXISTS generation BIGINT NOT NULL DEFAULT 1;
ALTER TABLE pages ADD COLUMN IF NOT EXISTS embedding BYTEA;

-- ---------------------------------------------------------------------------
-- 2. `bump_page_generation_fn` — plpgsql trigger function.
--
-- Contract (mirrors TS pglite-schema.ts and libsql 0003 trigger):
--   - INSERT: NEW.generation := COALESCE((SELECT MAX(generation) FROM pages), 0) + 1
--     (cache-bookmark gate: every new row guarantees a fresh monotonic id).
--   - UPDATE: bump only when one of the 10 watched columns IS DISTINCT FROM
--     its OLD value; otherwise leave generation untouched so cache stays warm.
--
-- The 10-column allow-list (must stay byte-identical to libsql 0003):
--   compiled_truth, timeline, frontmatter, deleted_at,
--   contextual_retrieval_mode, title, type, page_kind,
--   corpus_generation, content_hash
-- ---------------------------------------------------------------------------

CREATE OR REPLACE FUNCTION bump_page_generation_fn() RETURNS trigger AS $func$
BEGIN
  IF (TG_OP = 'INSERT') THEN
    NEW.generation := COALESCE((SELECT MAX(generation) FROM pages), 0) + 1;
  ELSIF (OLD.compiled_truth IS DISTINCT FROM NEW.compiled_truth)
     OR (OLD.timeline IS DISTINCT FROM NEW.timeline)
     OR (OLD.frontmatter IS DISTINCT FROM NEW.frontmatter)
     OR (OLD.deleted_at IS DISTINCT FROM NEW.deleted_at)
     OR (OLD.contextual_retrieval_mode IS DISTINCT FROM NEW.contextual_retrieval_mode)
     OR (OLD.title IS DISTINCT FROM NEW.title)
     OR (OLD.type IS DISTINCT FROM NEW.type)
     OR (OLD.page_kind IS DISTINCT FROM NEW.page_kind)
     OR (OLD.corpus_generation IS DISTINCT FROM NEW.corpus_generation)
     OR (OLD.content_hash IS DISTINCT FROM NEW.content_hash)
  THEN
    NEW.generation := OLD.generation + 1;
  END IF;
  RETURN NEW;
END;
$func$ LANGUAGE plpgsql;

DROP TRIGGER IF EXISTS bump_page_generation_trg ON pages;
CREATE TRIGGER bump_page_generation_trg
BEFORE INSERT OR UPDATE ON pages
FOR EACH ROW EXECUTE FUNCTION bump_page_generation_fn();
