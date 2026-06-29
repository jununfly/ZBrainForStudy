-- Advanced Page writes parity (slice 1-2-2-5 / issue #22).
-- Adds raw_data (sidecar data) and page_versions (snapshot history) tables.
-- Mirrors TS pglite-schema.ts:260-330 verbatim.

CREATE TABLE IF NOT EXISTS raw_data (
    id         BIGSERIAL PRIMARY KEY,
    page_id    BIGINT NOT NULL REFERENCES pages(id) ON DELETE CASCADE,
    source     TEXT    NOT NULL,
    data       JSONB   NOT NULL,
    fetched_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE(page_id, source)
);

CREATE INDEX IF NOT EXISTS idx_raw_data_page ON raw_data(page_id);

CREATE TABLE IF NOT EXISTS page_versions (
    id             BIGSERIAL PRIMARY KEY,
    page_id        BIGINT NOT NULL REFERENCES pages(id) ON DELETE CASCADE,
    compiled_truth TEXT    NOT NULL,
    frontmatter    JSONB   NOT NULL DEFAULT '{}',
    snapshot_at    TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX IF NOT EXISTS idx_versions_page ON page_versions(page_id);
