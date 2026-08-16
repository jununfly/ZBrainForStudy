-- G75 (reindex-multimodal): add a multimodal chunk embedding column so
-- `reindex multimodal` can persist a second (multimodal) vector per chunk
-- alongside the text `embedding`. Stored as f32 little-endian BLOB — same
-- encoding as `content_chunks.embedding` and `pages.embedding` (G24).
-- Rust production ingestion does not yet populate this column; the
-- `reindex multimodal` CLI command is the first writer.
ALTER TABLE content_chunks ADD COLUMN embedding_multimodal BLOB;
