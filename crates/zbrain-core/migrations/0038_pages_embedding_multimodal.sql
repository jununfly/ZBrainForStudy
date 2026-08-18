-- G70 (1-4-5): add a page-level multimodal vector column.
-- Mirrors the SQLite 0038 migration. BYTEA matches the f32 LE BLOB encoding
-- used by `pages.embedding` (G24) and `content_chunks.embedding_multimodal` (G75/0036).
-- Populated by `reindex multimodal` (mean-pool of chunk multimodal vectors)
-- and by ingestion when a multimodal embedding is available.
ALTER TABLE pages ADD COLUMN embedding_multimodal BYTEA;
