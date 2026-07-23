-- Calibration tables migration (1-3-3-1)
-- Ported from TS canonical schema, with:
--   - take_nudge_log.proposal_id: drop REFERENCES take_proposals (Rust migration does not yet have this table)
--   - All defaults match TS: wave_version defaults to 'v0.36.1.0'
--   - CHECK constraints preserved exactly
--   - Foreign keys preserved where possible (cascade deletes)

CREATE TABLE IF NOT EXISTS calibration_profiles (
    id                      BIGSERIAL PRIMARY KEY,
    source_id               TEXT         NOT NULL REFERENCES sources(id) ON DELETE CASCADE,
    holder                  TEXT         NOT NULL,
    wave_version            TEXT         NOT NULL DEFAULT 'v0.36.1.0',
    generated_at            TIMESTAMPTZ  NOT NULL DEFAULT now(),
    published               BOOLEAN      NOT NULL DEFAULT false,
    total_resolved          INTEGER      NOT NULL,
    brier                   REAL,
    accuracy                REAL,
    partial_rate            REAL,
    grade_completion        REAL         NOT NULL DEFAULT 1.0,
    domain_scorecards       JSONB        NOT NULL,
    pattern_statements      TEXT[]       NOT NULL,
    voice_gate_passed       BOOLEAN      NOT NULL,
    voice_gate_attempts     SMALLINT     NOT NULL,
    active_bias_tags        TEXT[]       NOT NULL,
    model_id                TEXT         NOT NULL,
    cost_usd                NUMERIC(10,4),
    judge_model_agreement   REAL
);

CREATE TABLE IF NOT EXISTS take_nudge_log (
    id              BIGSERIAL PRIMARY KEY,
    source_id       TEXT         NOT NULL REFERENCES sources(id) ON DELETE CASCADE,
    take_id         BIGINT,
    proposal_id     BIGINT, -- dropped REFERENCES take_proposals (Rust does not yet have this table), keep as nullable BIGINT
    nudge_pattern   TEXT         NOT NULL,
    fired_at        TIMESTAMPTZ  NOT NULL DEFAULT now(),
    channel         TEXT         NOT NULL DEFAULT 'stderr',
    wave_version    TEXT         NOT NULL DEFAULT 'v0.36.1.0',
    CONSTRAINT take_nudge_log_target_xor
        CHECK ((take_id IS NOT NULL) <> (proposal_id IS NOT NULL))
);

CREATE TABLE IF NOT EXISTS think_ab_results (
    id              BIGSERIAL PRIMARY KEY,
    source_id       TEXT         NOT NULL REFERENCES sources(id) ON DELETE CASCADE,
    wave_version    TEXT         NOT NULL DEFAULT 'v0.36.1.0',
    ran_at          TIMESTAMPTZ  NOT NULL DEFAULT now(),
    question        TEXT         NOT NULL,
    baseline_answer TEXT         NOT NULL,
    with_calibration_answer TEXT NOT NULL,
    preferred       TEXT         NOT NULL CHECK (preferred IN ('baseline','with_calibration','neither','tie')),
    model_id        TEXT,
    notes           TEXT
);

CREATE TABLE IF NOT EXISTS take_grade_cache (
    take_id            BIGINT       NOT NULL,
    prompt_version     TEXT         NOT NULL,
    judge_model_id     TEXT         NOT NULL,
    evidence_signature TEXT         NOT NULL,
    wave_version       TEXT         NOT NULL DEFAULT 'v0.36.1.0',
    graded_at          TIMESTAMPTZ  NOT NULL DEFAULT now(),
    verdict            TEXT         NOT NULL
                                  CHECK (verdict IN ('correct','incorrect','partial','unresolvable')),
    confidence         REAL         NOT NULL,
    applied            BOOLEAN      NOT NULL DEFAULT false,
    cost_usd           NUMERIC(10,4),
    PRIMARY KEY (take_id, prompt_version, judge_model_id, evidence_signature)
);

CREATE TABLE IF NOT EXISTS take_domain_assignments (
    take_id         BIGINT   NOT NULL REFERENCES takes(id) ON DELETE CASCADE,
    domain          TEXT     NOT NULL,
    pack            TEXT     NOT NULL,
    source          TEXT, -- optional source of manual assignment
    confidence      REAL     NOT NULL DEFAULT 1.0 CHECK (confidence BETWEEN 0.0 AND 1.0),
    assigned_at     TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (take_id, domain)
);

CREATE INDEX IF NOT EXISTS idx_take_domain_assignments_domain ON take_domain_assignments(domain);
