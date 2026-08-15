-- 1-6-3 / G77: resumable symbol-resolution backfill watermark.
-- Mirrors TS `content_chunks.edges_backfilled_at`. NULL until backfilled.
-- Stored as TEXT (ISO-8601, e.g. 2026-08-15T14:30:00Z) so the `< version`
-- comparison is a correct lexical check against the ISO version constant.
ALTER TABLE content_chunks ADD COLUMN edges_backfilled_at TEXT;
