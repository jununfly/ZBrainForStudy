-- G70 (1-4-5): add a page-level multimodal vector column so the hybrid
-- search path can score against the multimodal (image) space alongside the
-- text `embedding`. Stored as f32 little-endian BLOB — same encoding as
-- `pages.embedding` (G24) and `content_chunks.embedding_multimodal` (G75/0036).
-- Populated by `reindex multimodal` (mean-pool of chunk multimodal vectors)
-- and by ingestion when a multimodal embedding is available.
ALTER TABLE pages ADD COLUMN embedding_multimodal BLOB;
