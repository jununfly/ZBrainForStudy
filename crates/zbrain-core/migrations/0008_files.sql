-- File metadata rows for TS BrainEngine file-storage parity.
-- Bytes are not stored in the DB; storage_path points to repo/external storage.
-- Identity follows the running TS schema/backend implementation: UNIQUE(storage_path).

CREATE TABLE IF NOT EXISTS files (
    id           BIGSERIAL PRIMARY KEY,
    source_id    TEXT NOT NULL DEFAULT 'default'
                 REFERENCES sources(id) ON DELETE CASCADE,
    page_slug    TEXT,
    page_id      BIGINT REFERENCES pages(id) ON DELETE SET NULL,
    filename     TEXT NOT NULL,
    storage_path TEXT NOT NULL,
    mime_type    TEXT,
    size_bytes   BIGINT,
    content_hash TEXT NOT NULL,
    metadata     JSONB NOT NULL DEFAULT '{}'::jsonb,
    created_at   TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE(storage_path)
);

CREATE INDEX IF NOT EXISTS idx_files_page ON files(page_slug);
CREATE INDEX IF NOT EXISTS idx_files_page_id ON files(page_id);
CREATE INDEX IF NOT EXISTS idx_files_source_id ON files(source_id);
CREATE INDEX IF NOT EXISTS idx_files_hash ON files(content_hash);
