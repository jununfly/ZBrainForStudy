-- 1-1-5-5 (brainstorm domain-bank): add a chunk embedding column so
-- `representative_chunk_id` (lowest embedded chunk per page) and
-- `get_embeddings_by_chunk_ids` can compute cross-page distance scores.
-- Mirrors TS `content_chunks.embedding` (vector). Stored as BYTEA
-- f32-LE — same encoding as `pages.embedding` (G24).
ALTER TABLE content_chunks ADD COLUMN embedding BYTEA;
