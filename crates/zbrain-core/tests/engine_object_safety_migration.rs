//! Migration trait object-safety test.
//!
//! Compile-only test: verifies that `dyn Migration` is object-safe, that
//! `MigrationRegistry` accepts trait objects, and that basic type
//! relationships hold. No runtime behavior tested in this slice.

#![allow(unused)]

use zbrain_core::error::Result;
use zbrain_core::migration::{InMemoryMigration, Migration, MigrationRegistry};

/// Compile-time assertion that `dyn Migration` is object-safe.
fn _assert_migration_object_safe() -> Result<()> {
    // Migration trait can be boxed
    let m: Box<dyn Migration> = Box::new(InMemoryMigration::new(1, "test", "SELECT 1"));

    // Methods can be called through trait object
    let _version = m.version();
    let _name = m.name();
    let _sql = m.sql();
    let _sqlite_sql = m.sqlite_sql();
    let _postgres_sql = m.postgres_sql();
    let _transaction = m.transaction();
    let _idempotent = m.idempotent();

    // Stubs are callable (will panic if actually executed, but compile-checks here)
    // let _handler = m.handler_stub();
    // let _verify = m.verify_stub();

    Ok(())
}

/// Compile-time assertion that MigrationRegistry works with trait objects.
fn _assert_registry_accepts_trait_objects() {
    let mut registry = MigrationRegistry::new();

    // Add a boxed trait object
    registry.add(Box::new(InMemoryMigration::new(1, "m1", "SELECT 1")));
    registry.add(Box::new(InMemoryMigration::new(2, "m2", "SELECT 2")));

    // Iterate and call methods through the trait object
    for m in registry.iter() {
        let _v = m.version();
        let _n = m.name();
        let _s = m.sql();
    }

    // Latest version works
    let _latest = registry.latest_version();
    let _len = registry.len();
}

/// Compile-time assertion: Migration can be implemented by another type.
struct TestCustomMigration {
    version: i64,
    name: String,
}

impl Migration for TestCustomMigration {
    fn version(&self) -> i64 {
        self.version
    }

    fn name(&self) -> &str {
        &self.name
    }

    fn sql(&self) -> &str {
        "SELECT 42"
    }

    // Override defaults
    fn transaction(&self) -> bool {
        false
    }

    fn idempotent(&self) -> bool {
        false
    }

    fn sqlite_sql(&self) -> Option<&str> {
        Some("SELECT 42 -- sqlite")
    }

    fn postgres_sql(&self) -> Option<&str> {
        Some("SELECT 42 -- postgres")
    }
}

fn _assert_custom_migration_implements_trait() {
    let custom = TestCustomMigration {
        version: 3,
        name: "custom".into(),
    };

    let _boxed: Box<dyn Migration> = Box::new(custom);
}

/// Actual runtime test — just verifies basic type behavior, no I/O.
#[test]
fn migration_registry_sorts_by_version() {
    let mut registry = MigrationRegistry::new();

    // Add in reverse order
    registry.add(Box::new(InMemoryMigration::new(3, "m3", "SELECT 3")));
    registry.add(Box::new(InMemoryMigration::new(1, "m1", "SELECT 1")));
    registry.add(Box::new(InMemoryMigration::new(2, "m2", "SELECT 2")));

    let versions: Vec<i64> = registry.iter().map(|m| m.version()).collect();
    assert_eq!(versions, vec![1, 2, 3]);
}

#[test]
fn migration_registry_latest_version() {
    let mut registry = MigrationRegistry::new();
    assert_eq!(registry.latest_version(), 0);

    registry.add(Box::new(InMemoryMigration::new(1, "m1", "SELECT 1")));
    registry.add(Box::new(InMemoryMigration::new(5, "m5", "SELECT 5")));

    assert_eq!(registry.latest_version(), 5);
}

#[test]
fn migration_defaults() {
    let m = InMemoryMigration::new(1, "test", "SELECT 1");
    assert_eq!(m.transaction(), true);
    assert_eq!(m.idempotent(), true);
    assert_eq!(m.sqlite_sql(), None);
    assert_eq!(m.postgres_sql(), None);
}
