-- Slice 6a migration: full pages-column parity with PG baseline.
--
-- Adds every pages column that the 12 target methods (findDuplicatePage,
-- soft/restore/purge, refreshPageBody, updatePageContextualRetrievalState,
-- getAllSlugs/listAllPageRefs, updateSlug, getPageTimestamps,
-- getEffectiveDates, getSalienceScores) and the broader putPage upsert path
-- read or write. Mirrors `src/core/pglite-schema.ts` lines 70-160.
--
-- Type adaptations (PG → SQLite):
--   * JSONB                 -> TEXT (JSON string; serde_json at the app layer)
--   * REAL                  -> REAL (identical)
--   * TIMESTAMPTZ           -> TEXT (ISO-8601 string; CURRENT_TIMESTAMP default)
--   * BIGINT                -> INTEGER (SQLite stores INTEGERs as int64 anyway)
--   * BYTEA                 -> BLOB
--
-- Indexes:
--   * idx_pages_source_id           -> direct mirror
--   * pages_deleted_at_purge_idx    -> direct mirror (partial index)
--   * pages_coalesce_date_idx       -> expression index on COALESCE(effective_date, updated_at)
--   * pages_last_retrieved_at_idx   -> direct mirror
-- Deliberately deferred:
--   * GIN(frontmatter) / GIN trgm(title) / search_vector tsvector — these need
--     FTS5 VIRTUAL TABLE which lands in slice 6e.
--   * HNSW(embedding) — vector search uses in-memory linear cosine (slice 6e).
--
-- generation column + trigger: PG bumps `pages.generation` via
-- bump_page_generation_trg whenever an allow-listed content column changes.
-- The SQLite equivalent is a pair of BEFORE INSERT / BEFORE UPDATE triggers
-- with the same allow-list, using NEW/OLD just like PG.

ALTER TABLE pages ADD COLUMN frontmatter            TEXT    NOT NULL DEFAULT '{}';
ALTER TABLE pages ADD COLUMN content_hash           TEXT;
ALTER TABLE pages ADD COLUMN emotional_weight       REAL    NOT NULL DEFAULT 0.0;
ALTER TABLE pages ADD COLUMN deleted_at             TEXT;
ALTER TABLE pages ADD COLUMN effective_date         TEXT;
ALTER TABLE pages ADD COLUMN effective_date_source  TEXT;
ALTER TABLE pages ADD COLUMN import_filename        TEXT;
ALTER TABLE pages ADD COLUMN chunker_version        INTEGER DEFAULT 1;
ALTER TABLE pages ADD COLUMN source_path            TEXT;
ALTER TABLE pages ADD COLUMN source_kind            TEXT;
ALTER TABLE pages ADD COLUMN source_uri             TEXT;
ALTER TABLE pages ADD COLUMN ingested_via           TEXT;
ALTER TABLE pages ADD COLUMN ingested_at            TEXT;
ALTER TABLE pages ADD COLUMN salience_touched_at    TEXT;
ALTER TABLE pages ADD COLUMN last_retrieved_at      TEXT;
ALTER TABLE pages ADD COLUMN contextual_retrieval_mode TEXT;
ALTER TABLE pages ADD COLUMN corpus_generation      TEXT;
ALTER TABLE pages ADD COLUMN generation             INTEGER NOT NULL DEFAULT 1;
ALTER TABLE pages ADD COLUMN embedding              BLOB;

CREATE INDEX IF NOT EXISTS idx_pages_source_id
  ON pages(source_id);

CREATE INDEX IF NOT EXISTS pages_deleted_at_purge_idx
  ON pages(deleted_at)
  WHERE deleted_at IS NOT NULL;

CREATE INDEX IF NOT EXISTS pages_coalesce_date_idx
  ON pages (COALESCE(effective_date, updated_at));

CREATE INDEX IF NOT EXISTS pages_last_retrieved_at_idx
  ON pages(last_retrieved_at);

-- Generation bump triggers — SQLite equivalent of bump_page_generation_trg.
-- INSERT: seed `generation` from MAX over the table so cache bookmarks fire
-- for any pre-existing query_cache row that stored a count from before the
-- new page existed. UPDATE: bump by 1 only when one of the allow-listed
-- content columns actually changed (matches the PG `IS DISTINCT FROM` list).

-- HISTORICAL NOTE (kept for migration immutability — body is frozen, comment
-- is corrected here and superseded by 0003):
--
-- The original 0002 commentary claimed that AFTER UPDATE was the only viable
-- SQLite path because SQLite forbids `NEW.generation := ...` in a BEFORE
-- trigger. That conflated two separate facts:
--   (a) BEFORE UPDATE indeed cannot directly assign NEW.* (NEW is read-only
--       in BEFORE UPDATE; only BEFORE INSERT can mutate NEW).
--   (b) BUT a BEFORE UPDATE trigger CAN run a nested
--           UPDATE pages SET col = ... WHERE id = NEW.id
--       to write the same row, and that write IS visible to the outer
--       statement's RETURNING projection.
-- An AFTER UPDATE trigger with the same nested UPDATE persists the bump but
-- the outer statement's RETURNING has already snapshotted the row, so
-- callers like put_page's 30-col UPSERT RETURNING see the stale value.
--
-- 0003 rebuilds bump_page_generation_update as a BEFORE UPDATE trigger to
-- fix this. The bump_page_generation_insert trigger below is intentionally
-- left as AFTER INSERT (its bump does not need to surface in any RETURNING
-- in current usage). If a future caller needs the post-insert generation in
-- a RETURNING, switch this insert trigger to BEFORE INSERT and use a nested
-- UPDATE (same pattern as 0003), not direct NEW.* assignment.

DROP TRIGGER IF EXISTS bump_page_generation_insert;
CREATE TRIGGER bump_page_generation_insert
AFTER INSERT ON pages
FOR EACH ROW
BEGIN
    UPDATE pages
       SET generation = COALESCE((SELECT MAX(generation) FROM pages WHERE id <> NEW.id), 0) + 1
     WHERE id = NEW.id;
END;

DROP TRIGGER IF EXISTS bump_page_generation_update;
CREATE TRIGGER bump_page_generation_update
AFTER UPDATE OF
    compiled_truth,
    title,
    frontmatter,
    deleted_at,
    contextual_retrieval_mode,
    corpus_generation,
    content_hash
ON pages
FOR EACH ROW
WHEN
    NEW.compiled_truth            IS NOT OLD.compiled_truth
 OR NEW.title                     IS NOT OLD.title
 OR NEW.frontmatter               IS NOT OLD.frontmatter
 OR NEW.deleted_at                IS NOT OLD.deleted_at
 OR NEW.contextual_retrieval_mode IS NOT OLD.contextual_retrieval_mode
 OR NEW.corpus_generation         IS NOT OLD.corpus_generation
 OR NEW.content_hash              IS NOT OLD.content_hash
BEGIN
    UPDATE pages
       SET generation = OLD.generation + 1
     WHERE id = NEW.id;
END;
