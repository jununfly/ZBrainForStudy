-- G75 (reindex-multimodal): add a multimodal chunk embedding column.
-- Mirrors the SQLite 0036 migration. BYTEA matches the f32 LE BLOB encoding
-- used by `content_chunks.embedding` / `pages.embedding` (G24).
-- Rust production ingestion does not yet populate this column; the
-- `reindex multimodal` CLI command is the first writer.
ALTER TABLE content_chunks ADD COLUMN embedding_multimodal BYTEA;
