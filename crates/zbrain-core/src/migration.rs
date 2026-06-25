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

    // TODO(1-2-3-5): Full handler/verify signatures go here.
    // Currently stubbed out — exact signatures depend on BrainEngine calling
    // pattern which becomes concrete during libsql/postgres integration.

    /// Placeholder stub for handler function. Signature TBD in 1-2-3-5.
    #[allow(unused_variables)]
    fn handler_stub(&self) -> Result<()> {
        unimplemented!("handler signature deferred to 1-2-3-5")
    }

    /// Placeholder stub for verify hook. Signature TBD in 1-2-3-5.
    #[allow(unused_variables)]
    fn verify_stub(&self) -> Result<bool> {
        unimplemented!("verify signature deferred to 1-2-3-5")
    }
}

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
