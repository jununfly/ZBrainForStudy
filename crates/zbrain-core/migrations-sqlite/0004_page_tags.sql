-- Slice 6a S6-T5c migration: page_tags association table for tag filter.
--
-- Mirrors the TS PGLite prototype `tags` table from src/core/pglite-engine.ts:
--   * Composite primary key (page_id, tag) — same de-duplication semantics
--     as the TS ON CONFLICT (page_id, tag) DO NOTHING insert path.
--   * FK on page_id with ON DELETE CASCADE — when a page is hard-deleted,
--     its tag rows go with it. Soft-delete (deleted_at) does NOT touch
--     page_tags; list_pages still excludes soft-deleted rows via the
--     existing `p.deleted_at IS NULL` clause on the pages side.
--   * Index on (tag) — speeds up the tag JOIN selectivity path used by
--     list_pages when tag filter is the most-restrictive predicate.
--
-- Naming choice: we call the table `page_tags` (Rust mirror) instead of
-- the TS bare `tags`, because the relational meaning is "page-to-tag
-- association" rather than a tag dictionary. The TS name was kept for
-- backwards compatibility with the legacy schema; the Rust port has no
-- such constraint. The 6a-pg mirror slice will create a Postgres
-- `page_tags` table with identical shape for parity.
--
-- IMPORTANT: ON DELETE CASCADE requires `PRAGMA foreign_keys = ON` to be
-- set on every libsql connection. LibsqlEngine::conn() enforces this
-- starting from S6-T5c so all reads/writes share the same FK semantics.

CREATE TABLE IF NOT EXISTS page_tags (
    page_id  INTEGER NOT NULL
             REFERENCES pages(id) ON DELETE CASCADE,
    tag      TEXT NOT NULL,
    PRIMARY KEY (page_id, tag)
);

CREATE INDEX IF NOT EXISTS page_tags_tag_idx ON page_tags(tag);
