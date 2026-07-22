-- 0022: Image-search spend log — immutable audit trail for paid
--       multimodal-embedding API calls. SQLite port of
--       migrations/0022_image_search_spend_log.sql.
--
-- Context: search_by_image (1-6-7-11) calls a multimodal-embedding API
-- (e.g. Voyage voyage-multimodal-3). We track every paid call so we can
-- enforce a per-client daily spend budget on the MCP operation handler.
--
-- NOTE: This table is intentionally SEPARATE from the admin budget
-- feature's reserved `mcp_spend_log` table (which uses `amount_cents`
-- and pairs with `mcp_spend_reservations`). Keeping image-search spend
-- isolated avoids colliding with that future migration's column layout.
--
-- Type mapping:
--   SERIAL PRIMARY KEY  → INTEGER PRIMARY KEY AUTOINCREMENT
--   TIMESTAMPTZ         → TEXT (ISO-8601 UTC, e.g. 2026-07-22T11:00:00Z)
--
-- amount_cents is always positive — we log completed calls only
-- (failed calls don't consume budget).
--
-- Requires PRAGMA foreign_keys = ON at connection time (the engine
-- sets this on connect).

CREATE TABLE IF NOT EXISTS image_search_spend_log (
  id           INTEGER PRIMARY KEY AUTOINCREMENT,
  client_id    TEXT NOT NULL,
  amount_cents INTEGER NOT NULL,
  provider     TEXT NOT NULL DEFAULT '',
  model        TEXT NOT NULL DEFAULT '',
  created_at   TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
  CONSTRAINT chk_image_search_spend_log_positive CHECK (amount_cents > 0)
);

CREATE INDEX IF NOT EXISTS idx_image_search_spend_client_date
  ON image_search_spend_log (client_id, created_at);
