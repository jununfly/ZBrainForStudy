-- 0011: Expand `sources` table to match the TypeScript `src/schema.sql` schema.
--
-- The 0001 migration created a minimal `sources` table (id, name, created_at)
-- sufficient for the initial BrainEngine page CRUD. This migration adds the
-- remaining columns required by the Source CRUD API (1-7-1-1), the import/clone
-- pipeline (1-7-1-2), and downstream sync/chunk operations.
--
-- Column sources and design notes:
--   src/schema.sql:CREATE TABLE sources
--   src/core/sources-load.ts:SourceRow interface

ALTER TABLE sources ADD COLUMN IF NOT EXISTS config JSONB NOT NULL DEFAULT '{}'::jsonb;

ALTER TABLE sources ADD COLUMN IF NOT EXISTS local_path TEXT;

ALTER TABLE sources ADD COLUMN IF NOT EXISTS last_commit TEXT;

ALTER TABLE sources ADD COLUMN IF NOT EXISTS last_sync_at TIMESTAMPTZ;

ALTER TABLE sources ADD COLUMN IF NOT EXISTS chunker_version TEXT;

ALTER TABLE sources ADD COLUMN IF NOT EXISTS archived BOOLEAN NOT NULL DEFAULT false;

ALTER TABLE sources ADD COLUMN IF NOT EXISTS archived_at TIMESTAMPTZ;

ALTER TABLE sources ADD COLUMN IF NOT EXISTS archive_expires_at TIMESTAMPTZ;

ALTER TABLE sources ADD COLUMN IF NOT EXISTS contextual_retrieval_mode TEXT;

ALTER TABLE sources ADD COLUMN IF NOT EXISTS trust_frontmatter_overrides BOOLEAN NOT NULL DEFAULT false;
