-- Slice 6c-takes-salience PG migration: minimal `takes` table for the salience
-- formula. Only the columns required by `get_salience_scores` are materialized
-- here; the full TS schema (21 columns + HNSW vector index + synthesis_evidence
-- association) is intentionally deferred to a later takes-CRUD slice.
--
-- TS reference: src/core/migrate.ts §1180-1296 declares the full takes table.
-- TS salience reference: src/core/pglite-engine.ts §2596-2617 reads
--   COUNT(DISTINCT t.id) FROM takes t WHERE t.page_id = p.id AND t.active = TRUE
-- which depends only on (id, page_id, active). Anything beyond those three
-- columns is out of scope for this slice.
--
-- Subset rationale (path II, accepted by user):
--   * id          BIGSERIAL PK    — needed for COUNT(DISTINCT t.id).
--   * page_id     BIGINT FK       — JOIN key against pages(id) (BIGSERIAL).
--                                   ON DELETE CASCADE mirrors TS behavior:
--                                   hard-deleting a page drops its takes.
--   * active      BOOLEAN NOT NULL DEFAULT TRUE — filter predicate in the
--                                   salience JOIN. Default TRUE matches TS.
--
-- Index on (page_id, active) supports the JOIN selectivity for the salience
-- query and any future per-page active-takes lookup. Once full takes CRUD
-- lands, additional columns and indexes will be added via a follow-up
-- migration; this file MUST NOT be amended retroactively.

CREATE TABLE IF NOT EXISTS takes (
    id      BIGSERIAL PRIMARY KEY,
    page_id BIGINT  NOT NULL
            REFERENCES pages(id) ON DELETE CASCADE,
    active  BOOLEAN NOT NULL DEFAULT TRUE
);

CREATE INDEX IF NOT EXISTS takes_page_active_idx ON takes(page_id, active);
