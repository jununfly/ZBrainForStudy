-- 0012: Expand `takes` table from the 3-col salience stub to the full TS schema.
--
-- The 0007 migration (slice 6c-takes-salience) created a minimal `takes` table
-- (id, page_id, active) solely for the salience COUNT(DISTINCT) query. This
-- migration adds the remaining columns required by Phase 7A takes CRUD, the
-- fence parser, resolution tracking, and the scorecard.
--
-- TS reference: src/core/postgres-engine.ts INSERT/UPDATE statements
--   INSERT INTO takes (page_id, row_num, claim, kind, holder, weight,
--     since_date, until_date, source, superseded_by, active)
--   UPDATE takes SET resolved_at, resolved_quality, resolved_outcome,
--     resolved_evidence, resolved_value, resolved_unit, resolved_by
--
-- The existing placeholder rows (created by 0007 for salience stubs) have
-- no meaningful data; they will be populated with DEFAULT values for the
-- new columns. Application code should upsert real takes to replace them.
--
-- Design decisions:
--   - row_num is NOT unique per page at the DB level yet; app-layer enforcement
--     is sufficient until a data-migration backfills collisions. A non-unique
--     index is added for query performance.
--   - embedding column is deferred to a later slice (needs pgvector setup).
--   - CHECK constraints are PostgreSQL-only; SQLite variant omits them per
--     ALTER TABLE limitations.

-- Core take fields
ALTER TABLE takes ADD COLUMN IF NOT EXISTS row_num INTEGER NOT NULL DEFAULT 1;
ALTER TABLE takes ADD COLUMN IF NOT EXISTS claim TEXT NOT NULL DEFAULT '';
ALTER TABLE takes ADD COLUMN IF NOT EXISTS kind TEXT NOT NULL DEFAULT 'take';
ALTER TABLE takes ADD COLUMN IF NOT EXISTS holder TEXT NOT NULL DEFAULT 'brain';
ALTER TABLE takes ADD COLUMN IF NOT EXISTS weight DOUBLE PRECISION NOT NULL DEFAULT 0.5;
ALTER TABLE takes ADD COLUMN IF NOT EXISTS since_date TEXT;
ALTER TABLE takes ADD COLUMN IF NOT EXISTS until_date TEXT;
ALTER TABLE takes ADD COLUMN IF NOT EXISTS source TEXT;
ALTER TABLE takes ADD COLUMN IF NOT EXISTS superseded_by INTEGER;

-- Resolution fields (v0.30+)
ALTER TABLE takes ADD COLUMN IF NOT EXISTS resolved_at TIMESTAMPTZ;
ALTER TABLE takes ADD COLUMN IF NOT EXISTS resolved_quality TEXT;
ALTER TABLE takes ADD COLUMN IF NOT EXISTS resolved_outcome BOOLEAN;
ALTER TABLE takes ADD COLUMN IF NOT EXISTS resolved_evidence TEXT;
ALTER TABLE takes ADD COLUMN IF NOT EXISTS resolved_value DOUBLE PRECISION;
ALTER TABLE takes ADD COLUMN IF NOT EXISTS resolved_unit TEXT;
ALTER TABLE takes ADD COLUMN IF NOT EXISTS resolved_by TEXT;

-- Timestamps
ALTER TABLE takes ADD COLUMN IF NOT EXISTS created_at TIMESTAMPTZ NOT NULL DEFAULT now();
ALTER TABLE takes ADD COLUMN IF NOT EXISTS updated_at TIMESTAMPTZ NOT NULL DEFAULT now();

-- Domain constraints (safe to add since existing placeholder rows have defaults)
DO $$
BEGIN
    IF NOT EXISTS (SELECT 1 FROM pg_constraint WHERE conname = 'takes_row_num_positive') THEN
        ALTER TABLE takes ADD CONSTRAINT takes_row_num_positive CHECK (row_num > 0);
    END IF;
    IF NOT EXISTS (SELECT 1 FROM pg_constraint WHERE conname = 'takes_weight_range') THEN
        ALTER TABLE takes ADD CONSTRAINT takes_weight_range CHECK (weight >= 0 AND weight <= 1);
    END IF;
    IF NOT EXISTS (SELECT 1 FROM pg_constraint WHERE conname = 'takes_kind_valid') THEN
        ALTER TABLE takes ADD CONSTRAINT takes_kind_valid
            CHECK (kind IN ('fact', 'take', 'bet', 'hunch'));
    END IF;
END
$$;

-- Indexes for common query patterns
CREATE INDEX IF NOT EXISTS idx_takes_page_row ON takes(page_id, row_num);
CREATE INDEX IF NOT EXISTS idx_takes_holder ON takes(holder);
CREATE INDEX IF NOT EXISTS idx_takes_active ON takes(page_id, active) WHERE active = TRUE;
