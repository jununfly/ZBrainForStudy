-- 1-6-3 / G77: resumable symbol-resolution backfill watermark.
-- Mirrors TS `content_chunks.edges_backfilled_at` (src/core/chunkers/
-- symbol-resolver.ts W0c watermark). NULL until a chunk's emitted
-- code_edges_symbol rows are resolved by resolve_symbol_edges_incremental;
-- a non-NULL value older than EDGE_EXTRACTOR_VERSION_TS is re-walked when
-- the extractor shape changes. Stored as TIMESTAMPTZ so the `< version`
-- comparison is a real chronological check on postgres.
ALTER TABLE content_chunks ADD COLUMN edges_backfilled_at TIMESTAMPTZ;
