# 1-2-3-2: Add Rust Migration registry + runner foundation

Date: 2026-06-25
Parent roadmap node: 1-2-3 Move schema migrations ownership to Rust

## Scope

Compile-only foundation slice for the Rust migration system. Creates the shared `Migration` trait and `MigrationRegistry` abstraction used by both libsql and Postgres backends. Object-safety is the primary design constraint — the trait must be dispatchable via `dyn Migration` across backends.

**In scope:**
1. New dedicated `crates/zbrain-core/src/migration.rs` module (Q1 decision)
2. Object-safe `Migration` trait with version/name/sql core methods (Q2 decision)
3. `MigrationRegistry` ordered collection with version-sorted iteration (Q3 decision)
4. `sqlite_sql()` / `postgres_sql()` method stubs for engine specialization
5. `transaction()` and `idempotent()` boolean flags with defaults
6. `handler()` and `verify()` default implementations (signatures finalized in 1-2-3-5)
7. `InMemoryMigration` struct for registry validation tests
8. Dedicated object-safety test file `engine_object_safety_migration.rs` (Q5 decision)

**Out of scope:**
- libsql backend integration (moves to 1-2-3-3)
- Postgres backend integration (moves to 1-2-3-4)
- TS bridge layer (moves to 1-2-3-5)
- Concrete handler/verify implementations (deferred to 1-2-3-5)
- Runner execution logic (BEGIN/COMMIT, version table reads/writes)
- Any actual migration SQL content
- Any runtime behavior — compile-only verification only

---

## Decisions (Grill complete)

| # | Question | Answer | Rationale |
|---|----------|--------|-----------|
| 1 | Module location | New dedicated `migration.rs` | Follows modular pattern: types.rs for domain types, engine.rs for backend trait, migration.rs for migration-specific abstractions. Shared by both backends without circular dependency. |
| 2 | Dispatch pattern | `trait Migration` + `dyn` object-safe | Each backend implements its own Migration type; unified dispatch via trait object. Matches existing BrainEngine pattern. |
| 3 | Version number type | `i64` | Aligns with SQLite `PRAGMA user_version` return type; zero conversion overhead; PostgreSQL `INTEGER` compatible; future-proof against overflow (unlikely to hit 2 billion migrations). |
| 4 | Handler/verify signatures in this slice? | Stub only with default impls | Exact signatures depend on BrainEngine trait method availability, which is best finalized during libsql/postgres integration when the calling pattern is concrete. |
| 5 | Object-safety tests? | Full dedicated `engine_object_safety_migration.rs` | Follows the same pattern as advanced Page writes: explicit compile-only slice with object-safety test as the acceptance gate. Critical design constraint worth its own test file. |

---

## Implementation Plan

### Phase 1: Migration Trait Definition

**File:** `crates/zbrain-core/src/migration.rs`

```rust
//! Migration registry and runner foundation.
//!
//! Object-safe migration abstraction shared by both libsql and Postgres backends.
//! Version numbers use `i64` to match SQLite's `PRAGMA user_version` return type
//! with zero conversion overhead. Handler and verify function signatures are
//! deferred to the bridge/porting slice (1-2-3-5).

use crate::error::Result;

/// A single migration with versioned SQL and optional engine-specific overrides.
///
/// Object-safe trait — all methods can be called through a `dyn Migration`
/// trait object, enabling dynamic dispatch across backends.
///
/// Handler/verify function signatures are intentionally stubbed in this slice
/// and will be fully defined during the TS bridge/porting work (1-2-3-5).
pub trait Migration: Send + Sync {
    /// Migration version number. Must be strictly increasing within a registry.
    /// Uses `i64` to match SQLite `PRAGMA user_version` return type.
    fn version(&self) -> i64;

    /// Human-readable migration name/identifier.
    fn name(&self) -> &str;

    /// Engine-agnostic SQL body. Empty string for handler-only migrations.
    fn sql(&self) -> &str;

    /// SQLite/libsql-specific SQL override. Returns `None` if generic `sql()`
    /// should be used instead.
    fn sqlite_sql(&self) -> Option<&str> {
        None
    }

    /// Postgres-specific SQL override. Returns `None` if generic `sql()`
    /// should be used instead.
    fn postgres_sql(&self) -> Option<&str> {
        None
    }

    /// Whether to wrap this migration in a transaction. Defaults to `true`.
    /// Set to `false` for `CREATE INDEX CONCURRENTLY` which Postgres refuses
    /// to run inside a transaction.
    fn transaction(&self) -> bool {
        true
    }

    /// Whether this migration is idempotent (can be safely re-run). Defaults
    /// to `true`. Non-idempotent migrations block verify-hook self-healing.
    fn idempotent(&self) -> bool {
        true
    }

    /// Optional handler function executed after the SQL migration succeeds.
    /// For application-level transformations that cannot be expressed in SQL.
    /// Default implementation is a no-op.
    fn handler(&self) -> Result<()> {
        Ok(())
    }

    /// Optional verification hook executed after migration and handler.
    /// Returns Ok(true) if verification passed, Ok(false) if failed.
    /// Default implementation always returns Ok(true).
    fn verify(&self) -> Result<bool> {
        Ok(true)
    }
}
```

### Phase 2: Migration Registry

**File:** `crates/zbrain-core/src/migration.rs` (continuation)

```rust
/// Ordered collection of migrations. Applied in strict version order.
pub struct MigrationRegistry {
    migrations: Vec<Box<dyn Migration>>,
}

impl MigrationRegistry {
    /// Create an empty registry.
    pub fn new() -> Self {
        Self {
            migrations: Vec::new(),
        }
    }

    /// Add a migration to the registry. Migrations are automatically sorted
    /// by version number when returned via `iter()`.
    pub fn add(&mut self, migration: Box<dyn Migration>) {
        self.migrations.push(migration);
    }

    /// Iterate migrations in strict ascending version order.
    pub fn iter(&self) -> impl Iterator<Item = &dyn Migration> {
        let mut sorted: Vec<_> = self.migrations.iter().map(|m| m.as_ref()).collect();
        sorted.sort_by_key(|m| m.version());
        sorted.into_iter()
    }

    /// Highest version number in this registry.
    pub fn latest_version(&self) -> i64 {
        self.iter().map(|m| m.version()).max().unwrap_or(0)
    }

    /// Number of migrations in this registry.
    pub fn len(&self) -> usize {
        self.migrations.len()
    }
}

impl Default for MigrationRegistry {
    fn default() -> Self {
        Self::new()
    }
}
```

### Phase 3: In-Memory Test Implementation

**File:** `crates/zbrain-core/src/migration.rs` (continuation)

```rust
/// Simple in-memory migration implementation for tests and registry validation.
#[derive(Debug, Clone)]
pub struct InMemoryMigration {
    version: i64,
    name: String,
    sql: String,
    transaction: bool,
    idempotent: bool,
}

impl InMemoryMigration {
    /// Create a new in-memory migration with the given version, name, and SQL.
    pub fn new(version: i64, name: impl Into<String>, sql: impl Into<String>) -> Self {
        Self {
            version,
            name: name.into(),
            sql: sql.into(),
            transaction: true,
            idempotent: true,
        }
    }
}

impl Migration for InMemoryMigration {
    fn version(&self) -> i64 {
        self.version
    }

    fn name(&self) -> &str {
        &self.name
    }

    fn sql(&self) -> &str {
        &self.sql
    }

    fn transaction(&self) -> bool {
        self.transaction
    }

    fn idempotent(&self) -> bool {
        self.idempotent
    }
}
```

### Phase 4: Module Export

**File:** `crates/zbrain-core/src/lib.rs`

```rust
// Add near other module exports:
pub mod migration;
```

### Phase 5: Object-Safety Tests

**File:** `crates/zbrain-core/tests/engine_object_safety_migration.rs`

```rust
//! Migration trait object-safety validation.
//!
//! Compile-only test — ensures `dyn Migration` is dispatchable and
//! `MigrationRegistry` works with trait objects across backends.

use zbrain_core::migration::{InMemoryMigration, Migration, MigrationRegistry};

/// Compile-time assertion: dyn Migration is object-safe.
fn _assert_object_safe(_m: &dyn Migration) {}

/// Compile-time assertion: registry accepts Box<dyn Migration>.
#[test]
fn migration_registry_accepts_box_dyn_migration() {
    let mut registry = MigrationRegistry::new();
    registry.add(Box::new(InMemoryMigration::new(1, "test", "")));
    assert_eq!(registry.len(), 1);
}

/// Version sorting works correctly.
#[test]
fn migration_registry_iterates_in_version_order() {
    let mut registry = MigrationRegistry::new();
    registry.add(Box::new(InMemoryMigration::new(3, "c", "")));
    registry.add(Box::new(InMemoryMigration::new(1, "a", "")));
    registry.add(Box::new(InMemoryMigration::new(2, "b", "")));

    let versions: Vec<i64> = registry.iter().map(|m| m.version()).collect();
    assert_eq!(versions, vec![1, 2, 3]);
}

/// Latest version calculation is correct.
#[test]
fn migration_registry_latest_version() {
    let mut registry = MigrationRegistry::new();
    assert_eq!(registry.latest_version(), 0); // empty registry = 0

    registry.add(Box::new(InMemoryMigration::new(5, "v5", "")));
    assert_eq!(registry.latest_version(), 5);

    registry.add(Box::new(InMemoryMigration::new(3, "v3", "")));
    assert_eq!(registry.latest_version(), 5); // still 5, not affected by lower
}

/// All default method implementations work correctly through dyn object.
#[test]
fn migration_trait_defaults_work_through_dyn() {
    let m = InMemoryMigration::new(1, "test", "SELECT 1");
    let dm: &dyn Migration = &m;

    assert_eq!(dm.version(), 1);
    assert_eq!(dm.name(), "test");
    assert_eq!(dm.sql(), "SELECT 1");
    assert_eq!(dm.sqlite_sql(), None);
    assert_eq!(dm.postgres_sql(), None);
    assert!(dm.transaction());
    assert!(dm.idempotent());
    assert!(dm.handler().is_ok());
    assert_eq!(dm.verify().unwrap(), true);
}
```

---

## Acceptance Criteria

This slice is complete when:

1. ✅ **`cargo check -p zbrain-core` passes** with zero errors
2. ✅ **`cargo test -p zbrain-core migration` passes** all 4 tests
3. ✅ **`cargo test -p zbrain-core -- doc`** passes — doc tests compile
4. ✅ Module is properly exported via `lib.rs`
5. ✅ No actual migration SQL exists in this slice (foundation only)
6. ✅ No backend-specific code exists in this slice (libsql/postgres-agnostic)

---

## Dependencies

- **Predecessor:** 1-2-3-1 (schema migrations audit plan) — must be accepted first
- **Successor:** 1-2-3-3 (libsql runner integration) — builds on this foundation
- **Successor:** 1-2-3-4 (Postgres runner integration) — builds on this foundation

---

## Related Nodes

- Parent: [1-2-3 Move schema migrations ownership to Rust](../ZBRAIN_TS_TO_RUST_ROADMAP.md)
- Follow-up: [1-2-3-3 Integrate Rust runner into libsql backend](./2026-06-25-1-2-3-3-libsql-migration-runner.md)
- Follow-up: [1-2-3-4 Integrate Rust runner into Postgres backend](./2026-06-25-1-2-3-4-postgres-migration-runner.md)
