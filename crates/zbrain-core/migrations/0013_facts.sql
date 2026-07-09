-- 0013: Create `facts` table with full TS schema (~27 columns).
--
-- This table is the engine-layer persistence for the facts domain:
-- facts are atomic claims about entities, tracked through their lifecycle
-- (insert → expire → supersede → consolidate into takes).
--
-- TS reference:
--   - FactRowSqlShape:  src/core/pglite-engine.ts L4749
--   - insertFact:       src/core/pglite-engine.ts L2917
--   - insertFacts batch: src/core/pglite-engine.ts L3019
--   - listFactsByEntity: src/core/pglite-engine.ts L3106
--   - expireFact:       src/core/pglite-engine.ts L3009
--   - getFactsHealth:   src/core/pglite-engine.ts L3324
--
-- Column groups:
--   CRUD core:      id, source_id, entity_slug, fact, kind, visibility,
--                   notability, context, valid_from, valid_until, expired_at,
--                   superseded_by, source, source_session, confidence, created_at
--   Consolidation:  consolidated_at, consolidated_into
--   Vector:         embedding (REAL[]), embedded_at
--   Typed-claim:    claim_metric, claim_value, claim_unit, claim_period, event_type
--   Fence sync:     row_num, source_markdown_slug
--
-- Phase 7B only implements insertFact, listFactsByEntity, getFactsHealth,
-- and expireFact. Batch insertFacts, consolidateFact, and typed-claim
-- querying are deferred to later phases but columns are included now to
-- avoid ALTER TABLE churn.
--
-- Design decisions:
--   - REAL[] for embedding (not pgvector vector type): pgvector is not a
--     hard dependency yet; the column stores the raw float array. Application
--     code can cast/convert when pgvector is wired in a later phase.
--   - CHECK constraints on kind, visibility, notability for data integrity.
--   - Partial index on expired_at IS NULL for the common "active only" query.
--   - No UNIQUE constraint on (source_id, fact, kind) — dedup is done at the
--     application layer via embedding cosine similarity, not exact text match.

CREATE TABLE IF NOT EXISTS facts (
    -- Primary key
    id              BIGSERIAL PRIMARY KEY,

    -- Tenant / entity routing
    source_id       TEXT NOT NULL REFERENCES sources(id) ON DELETE CASCADE,
    entity_slug     TEXT,

    -- Core claim
    fact            TEXT NOT NULL,
    kind            TEXT NOT NULL DEFAULT 'fact',
    visibility      TEXT NOT NULL DEFAULT 'private',
    notability      TEXT NOT NULL DEFAULT 'medium',
    context         TEXT,

    -- Temporal validity
    valid_from      TIMESTAMPTZ NOT NULL,
    valid_until     TIMESTAMPTZ,
    expired_at      TIMESTAMPTZ,
    superseded_by   INTEGER,

    -- Consolidation into takes
    consolidated_at TIMESTAMPTZ,
    consolidated_into INTEGER,

    -- Provenance
    source          TEXT NOT NULL,
    source_session  TEXT,
    confidence      DOUBLE PRECISION NOT NULL DEFAULT 1.0,

    -- Embedding (deferred; REAL[] as placeholder until pgvector is wired)
    embedding       REAL[],
    embedded_at     TIMESTAMPTZ,

    -- Fence reconciliation (v0.32.2)
    row_num             INTEGER,
    source_markdown_slug TEXT,

    -- Typed claims (v0.35.4)
    claim_metric    TEXT,
    claim_value     DOUBLE PRECISION,
    claim_unit      TEXT,
    claim_period    TEXT,

    -- Event type tag (v0.40.2)
    event_type      TEXT,

    -- Metadata
    created_at      TIMESTAMPTZ NOT NULL DEFAULT now()
);

-- Domain constraints
DO $$
BEGIN
    IF NOT EXISTS (SELECT 1 FROM pg_constraint WHERE conname = 'facts_kind_valid') THEN
        ALTER TABLE facts ADD CONSTRAINT facts_kind_valid
            CHECK (kind IN ('event', 'preference', 'commitment', 'belief', 'fact'));
    END IF;
    IF NOT EXISTS (SELECT 1 FROM pg_constraint WHERE conname = 'facts_visibility_valid') THEN
        ALTER TABLE facts ADD CONSTRAINT facts_visibility_valid
            CHECK (visibility IN ('private', 'world'));
    END IF;
    IF NOT EXISTS (SELECT 1 FROM pg_constraint WHERE conname = 'facts_notability_valid') THEN
        ALTER TABLE facts ADD CONSTRAINT facts_notability_valid
            CHECK (notability IN ('high', 'medium', 'low'));
    END IF;
    IF NOT EXISTS (SELECT 1 FROM pg_constraint WHERE conname = 'facts_confidence_range') THEN
        ALTER TABLE facts ADD CONSTRAINT facts_confidence_range
            CHECK (confidence >= 0 AND confidence <= 1);
    END IF;
END
$$;

-- Indexes for common query patterns
CREATE INDEX IF NOT EXISTS idx_facts_source ON facts(source_id);
CREATE INDEX IF NOT EXISTS idx_facts_entity ON facts(source_id, entity_slug);
CREATE INDEX IF NOT EXISTS idx_facts_active ON facts(source_id, entity_slug)
    WHERE expired_at IS NULL;
CREATE INDEX IF NOT EXISTS idx_facts_created ON facts(source_id, created_at DESC);
CREATE INDEX IF NOT EXISTS idx_facts_consolidated ON facts(source_id)
    WHERE consolidated_at IS NULL AND expired_at IS NULL;
