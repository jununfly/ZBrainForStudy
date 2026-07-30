-- v0.28: synthesis_evidence — take→synthesis citation FK table.
-- Mirrors TS pglite-engine.ts:3953 INSERT. Written by persistSynthesis
-- (auto-think auto_commit path). Page-level citations (row_num IS NULL)
-- are NOT persisted; synthesis_evidence is a take→synthesis FK only.
-- FK to pages intentionally omitted to stay backend-agnostic (libsql/sqlite
-- do not enforce FK by default); the engine resolves slugs->page_ids itself.
CREATE TABLE IF NOT EXISTS synthesis_evidence (
  id                BIGSERIAL PRIMARY KEY,
  synthesis_page_id BIGINT NOT NULL,
  take_page_id      BIGINT NOT NULL,
  take_row_num      INTEGER,
  citation_index    INTEGER NOT NULL DEFAULT 0
);
