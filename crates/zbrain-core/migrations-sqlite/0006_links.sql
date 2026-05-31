-- Slice 6a-libsql find-orphan-pages: minimal `links` table mirror.
--
-- Mirrors the TS PGLite `links` shape used by findOrphanPages():
--   * from_page_id / to_page_id reference pages(id) with ON DELETE CASCADE.
--   * link_source uses the same constrained vocabulary as TS / PG.
--   * origin_page_id is kept for uniqueness parity, but higher-level link CRUD
--     remains deferred to a later slice.
--
-- This migration intentionally does NOT add page_links view or full link CRUD
-- behavior. The current slice only needs enough schema for inbound-link
-- existence checks in `find_orphan_pages`.

CREATE TABLE IF NOT EXISTS links (
    id              INTEGER PRIMARY KEY AUTOINCREMENT,
    from_page_id    INTEGER NOT NULL
                    REFERENCES pages(id) ON DELETE CASCADE,
    to_page_id      INTEGER NOT NULL
                    REFERENCES pages(id) ON DELETE CASCADE,
    link_type       TEXT    NOT NULL DEFAULT '',
    context         TEXT    NOT NULL DEFAULT '',
    link_source     TEXT    CHECK (link_source IS NULL
                                   OR link_source IN ('markdown', 'frontmatter', 'manual', 'mentions')),
    origin_page_id  INTEGER REFERENCES pages(id) ON DELETE SET NULL,
    origin_field    TEXT,
    resolution_type TEXT    CHECK (resolution_type IS NULL
                                   OR resolution_type IN ('qualified', 'unqualified')),
    created_at      TEXT    NOT NULL DEFAULT CURRENT_TIMESTAMP
);

CREATE UNIQUE INDEX IF NOT EXISTS links_from_to_type_source_origin_unique
ON links(
    from_page_id,
    to_page_id,
    link_type,
    COALESCE(link_source, '__zbrain_null__'),
    COALESCE(origin_page_id, -1)
);

CREATE INDEX IF NOT EXISTS idx_links_from ON links(from_page_id);
CREATE INDEX IF NOT EXISTS idx_links_to ON links(to_page_id);
CREATE INDEX IF NOT EXISTS idx_links_source ON links(link_source);
CREATE INDEX IF NOT EXISTS idx_links_origin ON links(origin_page_id);
