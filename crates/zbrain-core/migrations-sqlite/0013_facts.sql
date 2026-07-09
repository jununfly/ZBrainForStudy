-- 0013: Create `facts` table — SQLite variant.
--
-- Mirrors migrations/0013_facts.sql with SQLite adaptations:
--   - BIGSERIAL → INTEGER PRIMARY KEY AUTOINCREMENT
--   - TIMESTAMPTZ → TEXT
--   - DOUBLE PRECISION → REAL
--   - REAL[] → TEXT (JSON array stored as text; SQLite has no array type)
--   - No REFERENCES with ON DELETE CASCADE on source_id (SQLite foreign keys
--     are opt-in and the sources table may not exist in test fixtures)
--   - No DO $$ blocks — straight DDL only
--   - No CHECK constraints (SQLite ALTER TABLE limitation; domain constraints
--     are enforced at the Rust application layer)
--   - No partial WHERE clause on CREATE INDEX (SQLite limitation)
--   - If NOT EXISTS on table creation, individual ALTER would need it but
--     we use standalone CREATE TABLE here (caller handles migration tracking)

CREATE TABLE IF NOT EXISTS facts (
    id              INTEGER PRIMARY KEY AUTOINCREMENT,
    source_id       TEXT NOT NULL,
    entity_slug     TEXT,
    fact            TEXT NOT NULL,
    kind            TEXT NOT NULL DEFAULT 'fact',
    visibility      TEXT NOT NULL DEFAULT 'private',
    notability      TEXT NOT NULL DEFAULT 'medium',
    context         TEXT,
    valid_from      TEXT NOT NULL,
    valid_until     TEXT,
    expired_at      TEXT,
    superseded_by   INTEGER,
    consolidated_at TEXT,
    consolidated_into INTEGER,
    source          TEXT NOT NULL,
    source_session  TEXT,
    confidence      REAL NOT NULL DEFAULT 1.0,
    embedding       TEXT,
    embedded_at     TEXT,
    row_num         INTEGER,
    source_markdown_slug TEXT,
    claim_metric    TEXT,
    claim_value     REAL,
    claim_unit      TEXT,
    claim_period    TEXT,
    event_type      TEXT,
    created_at      TEXT NOT NULL DEFAULT (datetime('now'))
);

CREATE INDEX IF NOT EXISTS idx_facts_source ON facts(source_id);
CREATE INDEX IF NOT EXISTS idx_facts_entity ON facts(source_id, entity_slug);
CREATE INDEX IF NOT EXISTS idx_facts_active ON facts(source_id, entity_slug);
CREATE INDEX IF NOT EXISTS idx_facts_created ON facts(source_id, created_at);
CREATE INDEX IF NOT EXISTS idx_facts_consolidated ON facts(source_id);
