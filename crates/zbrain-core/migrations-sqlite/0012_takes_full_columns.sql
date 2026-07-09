-- 0012: Expand `takes` table — SQLite variant.
--
-- Mirrors migrations/0012_takes_full_columns.sql with SQLite adaptations:
--   - TIMESTAMPTZ → TEXT
--   - DOUBLE PRECISION → REAL
--   - BOOLEAN → INTEGER (0/1, same as the `active` column already uses)
--   - No IF NOT EXISTS on ALTER TABLE (SQLite ignores duplicates gracefully
--     via the caller's try-run-then-catch pattern)
--   - No CHECK constraints on ALTER TABLE (SQLite limitation); domain
--     constraints are enforced at the Rust application layer.
--   - No partial WHERE clause on CREATE INDEX (SQLite limitation).
--   - No DO $$ blocks — straight DDL only.

ALTER TABLE takes ADD COLUMN row_num INTEGER NOT NULL DEFAULT 1;
ALTER TABLE takes ADD COLUMN claim TEXT NOT NULL DEFAULT '';
ALTER TABLE takes ADD COLUMN kind TEXT NOT NULL DEFAULT 'take';
ALTER TABLE takes ADD COLUMN holder TEXT NOT NULL DEFAULT 'brain';
ALTER TABLE takes ADD COLUMN weight REAL NOT NULL DEFAULT 0.5;
ALTER TABLE takes ADD COLUMN since_date TEXT;
ALTER TABLE takes ADD COLUMN until_date TEXT;
ALTER TABLE takes ADD COLUMN source TEXT;
ALTER TABLE takes ADD COLUMN superseded_by INTEGER;
ALTER TABLE takes ADD COLUMN resolved_at TEXT;
ALTER TABLE takes ADD COLUMN resolved_quality TEXT;
ALTER TABLE takes ADD COLUMN resolved_outcome INTEGER;
ALTER TABLE takes ADD COLUMN resolved_evidence TEXT;
ALTER TABLE takes ADD COLUMN resolved_value REAL;
ALTER TABLE takes ADD COLUMN resolved_unit TEXT;
ALTER TABLE takes ADD COLUMN resolved_by TEXT;
ALTER TABLE takes ADD COLUMN created_at TEXT NOT NULL DEFAULT (datetime('now'));
ALTER TABLE takes ADD COLUMN updated_at TEXT NOT NULL DEFAULT (datetime('now'));

CREATE INDEX IF NOT EXISTS idx_takes_page_row ON takes(page_id, row_num);
CREATE INDEX IF NOT EXISTS idx_takes_holder ON takes(holder);
