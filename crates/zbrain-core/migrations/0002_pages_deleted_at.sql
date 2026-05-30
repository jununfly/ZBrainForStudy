-- Slice 6.5a (#73) — add `deleted_at` to `pages` to support
-- `GetPageOpts.include_deleted` read-path filtering.
--
-- Scope is intentionally narrow:
-- - column only, nullable, defaults to NULL (existing rows treated as live).
-- - no soft_delete_page trait wiring, no list/upsert changes (separate slices).
-- - no index — `get_page` already filters by `(source_id, slug)` which hits the
--   UNIQUE index from 0001; the IS NULL check is on the matched row only.

ALTER TABLE pages
    ADD COLUMN IF NOT EXISTS deleted_at TIMESTAMPTZ;
