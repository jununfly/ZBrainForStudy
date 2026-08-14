-- 1-1-5-5 (brainstorm domain-bank): add a chunk embedding column so
-- `representative_chunk_id` (lowest embedded chunk per page) and
-- `get_embeddings_by_chunk_ids` can compute cross-page distance scores.
-- Mirrors TS `content_chunks.embedding` (vector). Stored as f32
-- little-endian BLOB — same encoding as `pages.embedding` (G24).
-- Rust production ingestion does not yet populate this column; the
-- brainstorm domain-bank module is the first writer.
ALTER TABLE content_chunks ADD COLUMN embedding BLOB;
