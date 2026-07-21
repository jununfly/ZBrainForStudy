-- 1-6-7-5: content chunks (read side for get_chunks op).
-- Mirrors TS `content_chunks` (src/schema.sql). Rust production ingestion
-- does not yet populate this table, so get_chunks returns [] until the
-- Rust chunk pipeline writes here.
CREATE TABLE IF NOT EXISTS content_chunks (
  id SERIAL PRIMARY KEY,
  page_id BIGINT NOT NULL,
  chunk_index INTEGER NOT NULL,
  chunk_text TEXT NOT NULL,
  chunk_source TEXT NOT NULL DEFAULT 'text',
  model TEXT,
  token_count INTEGER,
  language TEXT,
  symbol_name TEXT,
  symbol_type TEXT,
  start_line INTEGER,
  end_line INTEGER,
  parent_symbol_path TEXT,
  doc_comment TEXT,
  symbol_name_qualified TEXT,
  created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX IF NOT EXISTS idx_content_chunks_page ON content_chunks(page_id, chunk_index);
