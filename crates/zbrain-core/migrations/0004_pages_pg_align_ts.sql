-- 0004_pages_pg_align_ts.sql
--
-- Slice #110-c: align PG `pages` schema to TS source-of-truth.
--
-- Background: #110-a 0003 introduced two divergences from
-- TS schema.sql + pglite-engine.ts + postgres-engine.ts which #110-b's
-- module doc-comment mistakenly classified as "intentional". The PG↔libsql
-- contract review (between #110-b and #110-c) confirmed they are bugs.
--
-- Fixes:
--   1. `corpus_generation` INTEGER → TEXT.
--      Source of truth: TS schema.sql:131 + both TS engines'
--      `ALTER TABLE pages ADD COLUMN IF NOT EXISTS corpus_generation TEXT;`
--      The trigger's `OLD.corpus_generation IS DISTINCT FROM NEW.corpus_generation`
--      check is type-agnostic, so no trigger rebuild is required.
--
--   2. `frontmatter` JSONB nullable → `JSONB NOT NULL DEFAULT '{}'::jsonb`.
--      Source of truth: TS schema.sql:93 — `frontmatter JSONB NOT NULL DEFAULT '{}'`.
--      Backfill any existing NULLs to `'{}'::jsonb` before tightening.
--
-- IMPORTANT: only ALTER COLUMN statements; no plpgsql, so sqlx migrate
-- splitter (`;`-aware) handles each statement cleanly.

-- ---------------------------------------------------------------------------
-- 1. corpus_generation: INTEGER → TEXT.
-- ---------------------------------------------------------------------------

ALTER TABLE pages
    ALTER COLUMN corpus_generation TYPE TEXT
    USING corpus_generation::text;

-- ---------------------------------------------------------------------------
-- 2. frontmatter: backfill NULLs, then enforce NOT NULL + DEFAULT '{}'::jsonb.
-- ---------------------------------------------------------------------------

UPDATE pages SET frontmatter = '{}'::jsonb WHERE frontmatter IS NULL;

ALTER TABLE pages ALTER COLUMN frontmatter SET DEFAULT '{}'::jsonb;
ALTER TABLE pages ALTER COLUMN frontmatter SET NOT NULL;
