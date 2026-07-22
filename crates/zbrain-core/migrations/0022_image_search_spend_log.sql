-- 0022: Image-search spend log — immutable audit trail for paid
--       multimodal-embedding API calls.
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
-- client_id is the MCP client making the request. amount_cents is always
-- positive — we log completed calls only (failed calls don't consume
-- budget).

CREATE TABLE IF NOT EXISTS image_search_spend_log (
  id           BIGSERIAL PRIMARY KEY,
  client_id    TEXT NOT NULL,
  amount_cents BIGINT NOT NULL,
  provider     TEXT NOT NULL DEFAULT '',
  model        TEXT NOT NULL DEFAULT '',
  created_at   TIMESTAMPTZ NOT NULL DEFAULT now(),
  CONSTRAINT chk_image_search_spend_log_positive CHECK (amount_cents > 0)
);

CREATE INDEX IF NOT EXISTS idx_image_search_spend_client_date
  ON image_search_spend_log (client_id, created_at);
