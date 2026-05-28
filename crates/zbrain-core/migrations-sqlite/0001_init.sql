-- Slice 5 initial migration: SQLite (libsql) mirror of migrations/0001_init.sql.
--
-- Mirrors the Postgres schema column-for-column for the slice 3 BrainEngine
-- trait surface, but adapted to SQLite dialect:
--   * BIGSERIAL              -> INTEGER PRIMARY KEY AUTOINCREMENT
--   * TIMESTAMPTZ DEFAULT now() -> TEXT DEFAULT CURRENT_TIMESTAMP
--   * ON CONFLICT ON CONSTRAINT <name> -> ON CONFLICT(<cols>)
--
-- libsql ships no embedded-migration macro (no `sqlx::migrate!` equivalent),
-- so this file is read with `include_str!` and executed via `execute_batch`
-- gated on `PRAGMA user_version`. See LibsqlEngine::init_schema for the
-- version-bump protocol.

CREATE TABLE IF NOT EXISTS sources (
    id          TEXT PRIMARY KEY,
    name        TEXT NOT NULL UNIQUE,
    created_at  TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
);

INSERT INTO sources (id, name)
VALUES ('default', 'default')
ON CONFLICT(id) DO NOTHING;

CREATE TABLE IF NOT EXISTS pages (
    id              INTEGER PRIMARY KEY AUTOINCREMENT,
    source_id       TEXT NOT NULL DEFAULT 'default'
                    REFERENCES sources(id) ON DELETE CASCADE,
    slug            TEXT NOT NULL,
    type            TEXT NOT NULL,
    page_kind       TEXT NOT NULL DEFAULT 'markdown'
                    CHECK (page_kind IN ('markdown', 'code', 'image')),
    title           TEXT NOT NULL,
    compiled_truth  TEXT NOT NULL DEFAULT '',
    timeline        TEXT NOT NULL DEFAULT '',
    created_at      TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
    updated_at      TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
    UNIQUE (source_id, slug)
);

CREATE INDEX IF NOT EXISTS pages_type_idx ON pages(type);
