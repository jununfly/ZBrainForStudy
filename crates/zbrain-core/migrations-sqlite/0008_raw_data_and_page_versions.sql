-- Advanced Page writes parity (slice 1-2-2-4 / issue #21).
-- Adds raw_data (sidecar data) and page_versions (snapshot history) tables.
-- Mirrors TS pglite-schema.ts:260-330.

-- Type adaptations (PG → SQLite):
--   JSONB → TEXT (serde_json at app layer)
--   TIMESTAMPTZ → TEXT (ISO-8601 string, CURRENT_TIMESTAMP default)
--   SERIAL → INTEGER PRIMARY KEY AUTOINCREMENT

CREATE TABLE IF NOT EXISTS raw_data (
    id         INTEGER PRIMARY KEY AUTOINCREMENT,
    page_id    INTEGER NOT NULL REFERENCES pages(id) ON DELETE CASCADE,
    source     TEXT    NOT NULL,
    data       TEXT    NOT NULL,
    fetched_at TEXT    NOT NULL DEFAULT CURRENT_TIMESTAMP,
    UNIQUE(page_id, source)
);

CREATE INDEX IF NOT EXISTS idx_raw_data_page ON raw_data(page_id);

CREATE TABLE IF NOT EXISTS page_versions (
    id             INTEGER PRIMARY KEY AUTOINCREMENT,
    page_id        INTEGER NOT NULL REFERENCES pages(id) ON DELETE CASCADE,
    compiled_truth TEXT    NOT NULL,
    frontmatter    TEXT    NOT NULL DEFAULT '{}',
    snapshot_at    TEXT    NOT NULL DEFAULT CURRENT_TIMESTAMP
);

CREATE INDEX IF NOT EXISTS idx_versions_page ON page_versions(page_id);
