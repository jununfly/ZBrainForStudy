-- 1-6-7-10-1: code-graph edge storage (write side for code-intel ops).
-- Mirrors TS `code_edges_chunk` + `code_edges_symbol`
-- (src/core/schema-embedded.ts:303-338). Two tables, no promotion step:
--   - code_edges_chunk: resolved edges (both endpoints = known chunk IDs)
--   - code_edges_symbol: unresolved refs (target known by qualified name only,
--     defining chunk not yet imported)
-- Readers UNION both tables. source_id is derived from
-- from_chunk_id -> content_chunks -> pages.source_id but stored redundantly for
-- direct source-scoped queries (mirrors TS design).
-- Unique keys use separate CREATE UNIQUE INDEX statements (the established
-- pattern in this repo, e.g. 0006_links.sql) for libsql execute_batch parity.
CREATE TABLE IF NOT EXISTS code_edges_chunk (
  id                    SERIAL PRIMARY KEY,
  from_chunk_id         INTEGER NOT NULL REFERENCES content_chunks(id) ON DELETE CASCADE,
  to_chunk_id           INTEGER NOT NULL REFERENCES content_chunks(id) ON DELETE CASCADE,
  from_symbol_qualified TEXT NOT NULL,
  to_symbol_qualified   TEXT NOT NULL,
  edge_type             TEXT NOT NULL,
  edge_metadata         JSONB NOT NULL DEFAULT '{}',
  source_id             TEXT REFERENCES sources(id) ON DELETE CASCADE,
  created_at            TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE UNIQUE INDEX IF NOT EXISTS code_edges_chunk_unique
  ON code_edges_chunk(from_chunk_id, to_chunk_id, edge_type);

CREATE INDEX IF NOT EXISTS idx_code_edges_chunk_from
  ON code_edges_chunk(from_chunk_id, edge_type);
CREATE INDEX IF NOT EXISTS idx_code_edges_chunk_to
  ON code_edges_chunk(to_chunk_id, edge_type);
CREATE INDEX IF NOT EXISTS idx_code_edges_chunk_to_symbol
  ON code_edges_chunk(to_symbol_qualified, edge_type);

CREATE TABLE IF NOT EXISTS code_edges_symbol (
  id                    SERIAL PRIMARY KEY,
  from_chunk_id         INTEGER NOT NULL REFERENCES content_chunks(id) ON DELETE CASCADE,
  from_symbol_qualified TEXT NOT NULL,
  to_symbol_qualified   TEXT NOT NULL,
  edge_type             TEXT NOT NULL,
  edge_metadata         JSONB NOT NULL DEFAULT '{}',
  source_id             TEXT REFERENCES sources(id) ON DELETE CASCADE,
  created_at            TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE UNIQUE INDEX IF NOT EXISTS code_edges_symbol_unique
  ON code_edges_symbol(from_chunk_id, to_symbol_qualified, edge_type);

CREATE INDEX IF NOT EXISTS idx_code_edges_symbol_from
  ON code_edges_symbol(from_chunk_id, edge_type);
CREATE INDEX IF NOT EXISTS idx_code_edges_symbol_to
  ON code_edges_symbol(to_symbol_qualified, edge_type);
