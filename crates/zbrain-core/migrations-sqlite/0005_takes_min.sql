-- Slice 6c-takes-salience libsql mirror: minimal `takes` table for the salience
-- formula. Mirrors migrations/0007_takes_min.sql with SQLite types.
--
-- Subset rationale (same as PG mirror):
--   * id      INTEGER PK    — needed for COUNT(DISTINCT t.id).
--   * page_id INTEGER FK    — JOIN key against pages(id). ON DELETE CASCADE
--                             requires `PRAGMA foreign_keys = ON` to be set
--                             on every libsql connection. LibsqlEngine::conn()
--                             enforces this since 6a S6-T5c.
--   * active  INTEGER NOT NULL DEFAULT 1 — SQLite has no native BOOLEAN;
--                                          0 = FALSE, 1 = TRUE. The salience
--                                          query compares with `= 1` (or
--                                          `= TRUE` which SQLite normalizes
--                                          to 1) to keep PG/libsql parity.
--
-- Index on (page_id, active) supports the JOIN selectivity for the salience
-- query. Full TS takes schema (21 cols + HNSW + synthesis_evidence) is
-- deferred to a later takes-CRUD slice; do NOT amend this migration when
-- those columns land — add a new 000N_takes_*.sql file instead.

CREATE TABLE IF NOT EXISTS takes (
    id      INTEGER PRIMARY KEY AUTOINCREMENT,
    page_id INTEGER NOT NULL
            REFERENCES pages(id) ON DELETE CASCADE,
    active  INTEGER NOT NULL DEFAULT 1
);

CREATE INDEX IF NOT EXISTS takes_page_active_idx ON takes(page_id, active);
