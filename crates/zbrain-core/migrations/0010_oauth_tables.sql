-- OAuth client, token, and authorization-code tables (Postgres).
-- Mirrors the SQLite migration from migrations-sqlite/0009_oauth_tables.sql.
-- Uses native PostgreSQL types: TEXT[] for arrays, TIMESTAMPTZ for timestamps.

CREATE TABLE IF NOT EXISTS oauth_clients (
    client_id                  TEXT PRIMARY KEY,
    client_secret_hash         TEXT,
    client_name                TEXT NOT NULL,
    redirect_uris              TEXT,
    grant_types                TEXT DEFAULT '["client_credentials"]',
    scope                      TEXT,
    token_endpoint_auth_method TEXT,
    client_id_issued_at        BIGINT,
    client_secret_expires_at   BIGINT,
    token_ttl                  BIGINT,
    deleted_at                 TIMESTAMPTZ,
    source_id                  TEXT REFERENCES sources(id) ON DELETE RESTRICT,
    federated_read             TEXT NOT NULL DEFAULT '[]',
    budget_usd_per_day         NUMERIC(10,2),
    bound_tools                TEXT,
    bound_source_id            TEXT,
    bound_brain_id             TEXT,
    bound_slug_prefixes        TEXT,
    bound_max_concurrent       INTEGER NOT NULL DEFAULT 1,
    created_at                 TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE INDEX IF NOT EXISTS idx_oauth_clients_source_id
    ON oauth_clients(source_id);

CREATE TABLE IF NOT EXISTS oauth_tokens (
    token_hash  TEXT PRIMARY KEY,
    token_type  TEXT NOT NULL,
    client_id   TEXT NOT NULL REFERENCES oauth_clients(client_id) ON DELETE CASCADE,
    scopes      TEXT,
    expires_at  BIGINT,
    resource    TEXT,
    created_at  TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE INDEX IF NOT EXISTS idx_oauth_tokens_expiry ON oauth_tokens(expires_at);
CREATE INDEX IF NOT EXISTS idx_oauth_tokens_client ON oauth_tokens(client_id);

CREATE TABLE IF NOT EXISTS oauth_codes (
    code_hash              TEXT PRIMARY KEY,
    client_id              TEXT NOT NULL REFERENCES oauth_clients(client_id) ON DELETE CASCADE,
    scopes                 TEXT,
    code_challenge         TEXT NOT NULL,
    code_challenge_method  TEXT NOT NULL DEFAULT 'S256',
    redirect_uri           TEXT NOT NULL,
    state                  TEXT,
    resource               TEXT,
    expires_at             BIGINT NOT NULL,
    created_at             TIMESTAMPTZ NOT NULL DEFAULT NOW()
);
