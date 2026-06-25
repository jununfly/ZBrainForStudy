# 1-2-3-4: Integrate Rust runner into Postgres backend

Date: 2026-06-25
Parent roadmap node: 1-2-3 Move schema migrations ownership to Rust

## Scope

Replace `sqlx::migrate!()` in postgres.rs with the shared `MigrationRegistry` from migration.rs.

**Hard cutover**: no dual-runner transition period, no backward compatibility with `_sqlx_migrations` table. There are no production users yet.

**Critical alignment**: libsql was RETROACTIVELY changed from one-transaction-per-migration to SINGLE TRANSACTION FOR ALL MIGRATIONS. Postgres must implement the same all-or-nothing atomic pattern.

### In scope:
1. `PostgresMigration` struct implementing the `Migration` trait (symmetric with `LibsqlMigration`)
2. `POSTGRES_MIGRATIONS: LazyLock<MigrationRegistry>` containing all 9 migrations
3. `rust_schema_version` table (same semantic as libsql, Postgres dialect)
4. `init_schema()` rewritten with SINGLE TRANSACTION for all migrations
5. Remove `static MIGRATOR: sqlx::migrate::Migrator` entirely
6. Dedicated test file `postgres_engine_migrations.rs` using `PgFixture`

### Out of scope:
- `transaction: false` support for `CREATE INDEX CONCURRENTLY` (deferred to future slice if needed)
- Handler/verify function implementations (deferred to 1-2-3-5)
- libsql changes (already done retroactively)

---

## Decisions (Grill complete)

| # | Question | Answer | Rationale |
|---|----------|--------|-----------|
| 1 | Version tracking mechanism | `rust_schema_version` standalone table (B) | Hard cutover from `_sqlx_migrations`. Aligns 100% with libsql. No users = no migration history needed. |
| 2 | Transaction boundary | Single transaction for ALL migrations (C) | All-or-nothing atomicity. libsql retroactively aligned to same pattern. Both backends behave identically. |
| 3 | Implementation pattern | `PostgresMigration` struct impl `Migration` trait (A) | Symmetric code structure with libsql.rs. Easy to maintain, minimal cognitive load. |
| 4 | Test strategy | New dedicated `postgres_engine_migrations.rs` (A) | Single responsibility. Symmetric with libsql test file. Uses existing `PgFixture` infrastructure. |

---

## Implementation Plan (3 vertical slices)

### Slice 1: PostgresMigration struct + registry wiring

**File:** `crates/zbrain-core/src/postgres.rs`

- Add `PostgresMigration` struct with fields: `version: i64`, `name: &'static str`, `sql: &'static str`
- Implement `Migration` trait for `PostgresMigration`
- Embed all 9 migration SQL files as `&'static str` using `include_str!()`
- Build `POSTGRES_MIGRATIONS: LazyLock<MigrationRegistry>` static
- Remove the old `static MIGRATOR: sqlx::migrate::Migrator`

### Slice 2: rust_schema_version table + init_schema rewrite

**File:** `crates/zbrain-core/src/postgres.rs`

- Add `RUST_SCHEMA_VERSION_BOOTSTRAP` const:
  ```sql
  CREATE TABLE IF NOT EXISTS rust_schema_version (
      version INTEGER PRIMARY KEY NOT NULL DEFAULT 0,
      applied_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP
  );
  INSERT INTO rust_schema_version (version) VALUES (0) ON CONFLICT DO NOTHING;
  ```
- Add helper functions:
  - `read_rust_schema_version(pool: &PgPool) -> Result<i64>`
  - `set_rust_schema_version(pool: &PgPool, ver: i64) -> Result<()>`
- Rewrite `init_schema()`:
  1. Begin transaction (all migrations in ONE transaction per Q2 decision)
  2. Run bootstrap to create `rust_schema_version` table
  3. Read current version
  4. Iterate registry, apply all migrations where version > current
  5. If any migration applied, set version to latest
  6. COMMIT

### Slice 3: Postgres migration tests

**File:** `crates/zbrain-core/tests/postgres_engine_migrations.rs`

Using existing `PgFixture`:
- Fresh DB runs all 9 migrations, ends at version 9
- Idempotency: run init_schema twice, second run applies 0 migrations
- `rust_schema_version` table exists with correct schema
- Tables from migrations 1, 4, 7, 9 all present (pages, page_tags, files, raw_data/page_versions)
- Bootstrap creates version 0 row correctly

---

## Acceptance Criteria

1. ✅ `cargo check -p zbrain-core` passes (no Rust errors)
2. ✅ `cargo fmt -p zbrain-core` passes (no style issues)
3. ✅ No references to `sqlx::migrate!` remain in postgres.rs
4. ✅ No references to `_sqlx_migrations` anywhere
5. ✅ All migration tests pass
6. ✅ libsql and postgres migration runners have structurally identical code patterns

---

## Next Node

**1-2-3-5:** Build TS bridge + port handler/verify functions to Rust
