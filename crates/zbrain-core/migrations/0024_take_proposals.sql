-- take_proposals table migration (Part12 1-1-4)
-- Ported from TS canonical schema (src/schema.sql). The propose_takes phase
-- writes gradeable claims here as a write-only proposal buffer; idempotency
-- cache via the composite unique index (source_id, page_slug, content_hash,
-- prompt_version) mirrors v0.23 dream_verdicts. proposal_run_id supports
-- --rollback by run. acted_* / promoted_row_num / predicted_brier_* columns
-- are populated by the later grade/promote phases (1-1-5, 1-1-6).

CREATE TABLE IF NOT EXISTS take_proposals (
    id                          BIGSERIAL PRIMARY KEY,
    source_id                   TEXT         NOT NULL REFERENCES sources(id) ON DELETE CASCADE,
    page_slug                   TEXT         NOT NULL,
    content_hash                TEXT         NOT NULL,
    prompt_version              TEXT         NOT NULL,
    wave_version                TEXT         NOT NULL DEFAULT 'v0.36.1.0',
    proposed_at                 TIMESTAMPTZ  NOT NULL DEFAULT now(),
    proposal_run_id             TEXT         NOT NULL,
    status                      TEXT         NOT NULL DEFAULT 'pending'
                                         CHECK (status IN ('pending','accepted','rejected','superseded')),
    claim_text                  TEXT         NOT NULL,
    kind                        TEXT         NOT NULL,
    holder                      TEXT         NOT NULL,
    weight                      REAL         NOT NULL,
    domain                      TEXT,
    dedup_against_fence_rows    JSONB,
    model_id                    TEXT         NOT NULL,
    acted_at                    TIMESTAMPTZ,
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
