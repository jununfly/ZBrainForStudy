-- v0.28: synthesis_evidence (SQLite dialect).
-- Type conversions: BIGSERIAL -> INTEGER PRIMARY KEY AUTOINCREMENT.
-- FK to pages omitted — see the postgres counterpart for rationale.
CREATE TABLE IF NOT EXISTS synthesis_evidence (
  id                INTEGER PRIMARY KEY AUTOINCREMENT,
  synthesis_page_id INTEGER NOT NULL,
  take_page_id      INTEGER NOT NULL,
  take_row_num      INTEGER,
  citation_index    INTEGER NOT NULL DEFAULT 0
);
