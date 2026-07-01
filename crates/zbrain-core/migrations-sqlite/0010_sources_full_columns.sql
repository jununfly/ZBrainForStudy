-- 0011: Expand `sources` table to match the TypeScript `src/schema.sql` schema.
-- SQLite variant — TEXT for timestamps, no JSONB, no IF NOT EXISTS for ALTER.
--
-- The 0001 migration created a minimal `sources` table (id, name, created_at).
-- This migration adds the remaining columns required by the Source CRUD API
-- (1-7-1-1), the import/clone pipeline (1-7-1-2), and downstream sync/chunk
-- operations.
--
-- Column sources and design notes:
--   src/schema.sql:CREATE TABLE sources
--   src/core/sources-load.ts:SourceRow interface

ALTER TABLE sources ADD COLUMN config TEXT NOT NULL DEFAULT '{}';

ALTER TABLE sources ADD COLUMN local_path TEXT;

ALTER TABLE sources ADD COLUMN last_commit TEXT;

ALTER TABLE sources ADD COLUMN last_sync_at TEXT;

ALTER TABLE sources ADD COLUMN chunker_version TEXT;

ALTER TABLE sources ADD COLUMN archived INTEGER NOT NULL DEFAULT 0;

ALTER TABLE sources ADD COLUMN archived_at TEXT;

ALTER TABLE sources ADD COLUMN archive_expires_at TEXT;

ALTER TABLE sources ADD COLUMN contextual_retrieval_mode TEXT;

ALTER TABLE sources ADD COLUMN trust_frontmatter_overrides INTEGER NOT NULL DEFAULT 0;
