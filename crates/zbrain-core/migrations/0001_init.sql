-- Slice 4a initial migration: minimal schema for `BrainEngine` page CRUD.
--
-- Mirrors the TypeScript `src/schema.sql` `sources` and `pages` tables, but
-- carries ONLY the columns required by the slice 3 BrainEngine trait
-- (kind/connect/disconnect/init_schema/get_page/put_page/delete_page/
-- list_pages/resolve_slugs). Soft-delete, embeddings, salience, contextual
-- retrieval, and generation columns land in later slices alongside the
-- methods that consult them.
--
-- The `sources` row with id='default' is seeded so the FK on pages.source_id
-- has a destination for the common single-source case.

CREATE TABLE IF NOT EXISTS sources (
    id          TEXT PRIMARY KEY,
    name        TEXT NOT NULL UNIQUE,
    created_at  TIMESTAMPTZ NOT NULL DEFAULT now()
);

INSERT INTO sources (id, name)
VALUES ('default', 'default')
ON CONFLICT (id) DO NOTHING;

CREATE TABLE IF NOT EXISTS pages (
    id              BIGSERIAL PRIMARY KEY,
    source_id       TEXT NOT NULL DEFAULT 'default'
                    REFERENCES sources(id) ON DELETE CASCADE,
    slug            TEXT NOT NULL,
    type            TEXT NOT NULL,
    page_kind       TEXT NOT NULL DEFAULT 'markdown'
                    CHECK (page_kind IN ('markdown', 'code', 'image')),
    title           TEXT NOT NULL,
    compiled_truth  TEXT NOT NULL DEFAULT '',
    timeline        TEXT NOT NULL DEFAULT '',
    created_at      TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at      TIMESTAMPTZ NOT NULL DEFAULT now(),
    CONSTRAINT pages_source_slug_key UNIQUE (source_id, slug)
);

CREATE INDEX IF NOT EXISTS pages_type_idx ON pages(type);
