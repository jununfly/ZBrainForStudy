-- Slice 6a S4 migration: close the last two PG-parity gaps on the pages table.
--
-- 1. Add `salience_score REAL` column.
--    Mirrors the column added by the same upgrade in `src/core/pglite-schema.ts`.
--    Used by getSalienceScores / salience-touched workflows. It is deliberately
--    NOT part of the generation-bump allow-list (PG baseline excludes it too —
--    salience updates are background recomputations and must not invalidate
--    query_cache rows).
--
-- 2. Rebuild `bump_page_generation_update` so its UPDATE OF list and WHEN
--    clause cover the full 10-column PG allow-list defined in
--    `bump_page_generation_fn` (pglite-schema.ts ~lines 122-135):
--      compiled_truth, timeline, frontmatter, deleted_at,
--      contextual_retrieval_mode, title, type, page_kind,
--      corpus_generation, content_hash
--    The 0002 version was missing timeline / type / page_kind. Without those,
--    edits that change a page's `type` or `timeline` would silently skip the
--    cache-invalidation bump and downstream readers would serve stale plans.
--
-- The INSERT trigger from 0002 (bump_page_generation_insert) is unchanged and
-- is intentionally not rebuilt here.

ALTER TABLE pages ADD COLUMN salience_score REAL;

DROP TRIGGER IF EXISTS bump_page_generation_update;
CREATE TRIGGER bump_page_generation_update
AFTER UPDATE OF
    compiled_truth,
    timeline,
    frontmatter,
    deleted_at,
    contextual_retrieval_mode,
    title,
    type,
    page_kind,
    corpus_generation,
    content_hash
ON pages
FOR EACH ROW
WHEN
    NEW.compiled_truth            IS NOT OLD.compiled_truth
 OR NEW.timeline                  IS NOT OLD.timeline
 OR NEW.frontmatter               IS NOT OLD.frontmatter
 OR NEW.deleted_at                IS NOT OLD.deleted_at
 OR NEW.contextual_retrieval_mode IS NOT OLD.contextual_retrieval_mode
 OR NEW.title                     IS NOT OLD.title
 OR NEW.type                      IS NOT OLD.type
 OR NEW.page_kind                 IS NOT OLD.page_kind
 OR NEW.corpus_generation         IS NOT OLD.corpus_generation
 OR NEW.content_hash              IS NOT OLD.content_hash
BEGIN
    UPDATE pages
       SET generation = OLD.generation + 1
     WHERE id = NEW.id;
END;
