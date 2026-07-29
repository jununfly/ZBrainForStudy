-- take_proposals table migration (Part12 1-1-4), SQLite dialect
-- Ported from TS canonical schema, dialect conversions:
--   BIGSERIAL → INTEGER PRIMARY KEY AUTOINCREMENT
--   TIMESTAMPTZ → TEXT (ISO-8601 string)
--   JSONB → TEXT (JSON-encoded string)

CREATE TABLE IF NOT EXISTS take_proposals (
    id                          INTEGER PRIMARY KEY AUTOINCREMENT,
    source_id                   TEXT         NOT NULL REFERENCES sources(id) ON DELETE CASCADE,
    page_slug                   TEXT         NOT NULL,
    content_hash                TEXT         NOT NULL,
    prompt_version              TEXT         NOT NULL,
    wave_version                TEXT         NOT NULL DEFAULT 'v0.36.1.0',
    proposed_at                 TEXT         NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ', 'now')),
    proposal_run_id             TEXT         NOT NULL,
    status                      TEXT         NOT NULL DEFAULT 'pending'
                                         CHECK (status IN ('pending','accepted','rejected','superseded')),
    claim_text                  TEXT         NOT NULL,
    kind                        TEXT         NOT NULL,
    holder                      TEXT         NOT NULL,
    weight                      REAL         NOT NULL,
    domain                      TEXT,
    dedup_against_fence_rows    TEXT, -- JSON-encoded string
    model_id                    TEXT         NOT NULL,
    acted_at                    TEXT,
    acted_by                    TEXT,
    promoted_row_num            INTEGER,
    predicted_brier             REAL,
    predicted_brier_bucket_n    INTEGER
);

CREATE UNIQUE INDEX IF NOT EXISTS take_proposals_idempotency_idx
    ON take_proposals (source_id, page_slug, content_hash, prompt_version);
CREATE INDEX IF NOT EXISTS take_proposals_pending_idx
    ON take_proposals (source_id, status, proposed_at DESC)
    WHERE status = 'pending';
CREATE INDEX IF NOT EXISTS take_proposals_run_id_idx
    ON take_proposals (proposal_run_id);
