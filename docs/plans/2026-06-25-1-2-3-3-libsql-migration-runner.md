# 1-2-3-3: Integrate Rust runner into libsql backend

Date: 2026-06-25
Parent roadmap node: 1-2-3 Move schema migrations ownership to Rust

## Scope

Replace the current ad-hoc `MIGRATIONS` const array + `SCHEMA_VERSION` + `PRAGMA user_version` pattern in `libsql.rs` with the shared `MigrationRegistry` from `migration.rs`.

**In scope:**
1. `LibsqlMigration` struct implementing the `Migration` trait
2. Global `LIBQL_MIGRATIONS: LazyLock<MigrationRegistry>` containing all 8 migrations
3. New `rust_schema_version` table for version tracking (hard cutover from TS)
4. One transaction per migration (Q2 decision)
5. `sql()` only — no SQLite-specific override logic (Q3 decision)
6. All 8 existing migrations ported with **zero SQL changes** (Q4 decision)
7. Dedicated test file `libsql_engine_migrations.rs` (Q5 decision)

**Out of scope:**
- Postgres runner integration (moves to 1-2-3-4)
- TS bridge layer (moves to 1-2-3-5)
- Handler/verify function implementations (deferred to 1-2-3-5)
- `transaction: false` support for CONCURRENTLY (deferred to future slice if needed)

---

## Decisions

| # | Question | Answer | Rationale |
|---|----------|--------|-----------|
| 1 | Version tracking mechanism | `rust_schema_version` standalone table | Hard cutover per 1-2-3 Q4; completely separate from TS `PRAGMA user_version` |
| 2 | Transaction boundary | One transaction per migration | Aligns with existing behavior; failure rolls back only current migration |
| 3 | SQL specialization | `sql()` always contains SQLite variant | Backends are fully decoupled; no need for shared SQL overrides |
| 4 | Existing migration porting | Keep SQL exactly as-is | Migration code is most sensitive; zero behavior drift; "if it ain't broke, don't fix it" |
| 5 | Test strategy | New dedicated `libsql_engine_migrations.rs` | Single responsibility; symmetric with future Postgres tests |

---

## Implementation Plan

### Phase 1: Trait impl + Registry

**File:** `crates/zbrain-core/src/libsql.rs`

```rust
/// libsql-specific migration implementation.
#[derive(Debug, Clone)]
struct LibsqlMigration {
    version: i64,
    name: &'static str,
    sql: &'static str,
}

impl Migration for LibsqlMigration {
    fn version(&self) -> i64 { self.version }
    fn name(&self) -> &str { self.name }
    fn sql(&self) -> &str { self.sql }
}

/// Bootstrap migration 0: creates rust_schema_version table.
const RUST_SCHEMA_VERSION_BOOTSTRAP: &str = r#"
CREATE TABLE IF NOT EXISTS rust_schema_version (
    version INTEGER PRIMARY KEY NOT NULL DEFAULT 0,
    applied_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
);
INSERT OR IGNORE INTO rust_schema_version (version) VALUES (0);
"#;

/// Global migration registry. Built once at runtime first use.
static LIBQL_MIGRATIONS: LazyLock<MigrationRegistry> = LazyLock::new(|| {
    let mut registry = MigrationRegistry::new();
    registry.add(Box::new(LibsqlMigration { version: 1, name: "init", sql: MIGRATION_0001 }));
    // ... versions 2-8 follow same pattern
    registry
});
```

### Phase 2: Runner logic in `init_schema`

**File:** `crates/zbrain-core/src/libsql.rs`

Replace the existing `PRAGMA user_version` loop with:
1. Run bootstrap migration to create `rust_schema_version` table
2. Read current version from `rust_schema_version`
3. Iterate `registry.iter()` for migrations where `version > current_version`
4. Run each migration in its own transaction
5. Update `rust_schema_version.version` after each successful migration

### Phase 3: Tests

**File:** `crates/zbrain-core/tests/libsql_engine_migrations.rs`

Test cases:
1. **Fresh DB:** runs all 8 migrations, ends at version 8
2. **Idempotency:** run `init_schema` twice, second run applies 0 migrations
3. **Order correctness:** migrations applied in strictly ascending version order
4. **Version table:** `rust_schema_version` row exists with correct version + timestamp
5. **Bootstrap only:** empty registry correctly stays at version 0

---

## Acceptance Criteria

1. ✅ `cargo check -p zbrain-core` passes (no Rust errors)
2. ✅ `cargo fmt -p zbrain-core` passes (no style issues)
3. ✅ All migration tests pass
4. ✅ No changes to any SQL file under `migrations-sqlite/`
5. ✅ No references to `PRAGMA user_version` remain in migration runner logic

---

## Next Node

**1-2-3-4:** Integrate Rust runner into Postgres backend
