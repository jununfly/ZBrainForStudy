-- Slice 6a PG mirror: links table for cross-page references.
--
-- Mirrors `src/core/pglite-schema.ts` §209-231 with one type adaptation:
--   * from_page_id / to_page_id / origin_page_id use BIGINT to reference
--     pages.id (which is BIGSERIAL in this crate, see 0001_init.sql).
--     TS uses INTEGER because the original schema predates the BIGSERIAL
--     migration; PG would otherwise reject the FK because integer cannot
--     reference bigint.
--   * The CHECK constraints, UNIQUE NULLS NOT DISTINCT, and four indexes
--     are preserved verbatim from TS to keep contract parity.
--
-- The `page_links` compatibility VIEW from `pglite-schema.ts` §244 is NOT
-- ported here: `find_orphan_pages` queries the canonical `links` table
-- directly, and no current Rust slice references `page_links`. Add the
-- view in a follow-up slice if/when graph/backlink methods land.

CREATE TABLE IF NOT EXISTS links (
    id              SERIAL PRIMARY KEY,
    from_page_id    BIGINT NOT NULL
                    REFERENCES pages(id) ON DELETE CASCADE,
    to_page_id      BIGINT NOT NULL
                    REFERENCES pages(id) ON DELETE CASCADE,
    link_type       TEXT    NOT NULL DEFAULT '',
    context         TEXT    NOT NULL DEFAULT '',
    link_source     TEXT    CHECK (link_source IS NULL
                                   OR link_source IN ('markdown', 'frontmatter', 'manual', 'mentions')),
    origin_page_id  BIGINT  REFERENCES pages(id) ON DELETE SET NULL,
    origin_field    TEXT,
    resolution_type TEXT    CHECK (resolution_type IS NULL
                                   OR resolution_type IN ('qualified', 'unqualified')),
    created_at      TIMESTAMPTZ NOT NULL DEFAULT now(),
    CONSTRAINT links_from_to_type_source_origin_unique
        UNIQUE NULLS NOT DISTINCT (from_page_id, to_page_id, link_type, link_source, origin_page_id)
);

CREATE INDEX IF NOT EXISTS idx_links_from   ON links(from_page_id);
CREATE INDEX IF NOT EXISTS idx_links_to     ON links(to_page_id);
CREATE INDEX IF NOT EXISTS idx_links_source ON links(link_source);
CREATE INDEX IF NOT EXISTS idx_links_origin ON links(origin_page_id);
