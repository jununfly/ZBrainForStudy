-- Slice 6a PG mirror: page_tags association table for tag filter.
--
-- Mirrors migrations-sqlite/0004_page_tags.sql with PostgreSQL types:
--   * page_id is BIGINT because pages.id is BIGSERIAL.
--   * Composite primary key (page_id, tag) preserves idempotent add_tag semantics.
--   * ON DELETE CASCADE removes tag rows on hard page deletes.
--   * tag index supports list_pages(tag) selectivity.

CREATE TABLE IF NOT EXISTS page_tags (
    page_id BIGINT NOT NULL
            REFERENCES pages(id) ON DELETE CASCADE,
    tag     TEXT NOT NULL,
    PRIMARY KEY (page_id, tag)
);

CREATE INDEX IF NOT EXISTS page_tags_tag_idx ON page_tags(tag);
