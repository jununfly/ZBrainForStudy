-- 0018_rate_leases: rate-lease concurrency slots for subagent calls.
-- Mirrors TS subagent_rate_leases table (src/schema.sql L838-848).
-- One row = one in-flight lease. acquire_rate_lease serialises via
-- pg_advisory_xact_lock(fnv1a(key)) + a single transaction, so a
-- second concurrent acquire on the same key blocks until the first
-- commits (or its xact rolls back).

CREATE TABLE IF NOT EXISTS subagent_rate_leases (
    id            BIGSERIAL PRIMARY KEY,
    key           TEXT        NOT NULL,
    owner_job_id  BIGINT      NOT NULL REFERENCES minion_jobs(id) ON DELETE CASCADE,
    acquired_at   TIMESTAMPTZ NOT NULL DEFAULT now(),
    expires_at    TIMESTAMPTZ NOT NULL
);

-- Speed up the "delete expired leases" step inside acquire_rate_lease.
CREATE INDEX IF NOT EXISTS idx_rate_leases_key_expires
    ON subagent_rate_leases (key, expires_at);
