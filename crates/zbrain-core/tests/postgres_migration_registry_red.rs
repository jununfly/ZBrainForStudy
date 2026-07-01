//! RED test for issue #27 - PostgresMigration trait impl + registry wiring
//!
//! These tests should FAIL initially because PostgresMigration and
//! POSTGRES_MIGRATIONS don't exist yet. They should PASS after implementation.

#![allow(unused)]

use zbrain_core::migration::{Migration, MigrationRegistry};
use zbrain_core::postgres::POSTGRES_MIGRATIONS;

#[test]
fn postgres_registry_is_exported() {
    // Just verify POSTGRES_MIGRATIONS exists and is accessible
    let _ = &POSTGRES_MIGRATIONS;
}

#[test]
fn postgres_registry_has_ten_migrations() {
    assert_eq!(POSTGRES_MIGRATIONS.len(), 10);
}

#[test]
fn postgres_registry_latest_version_is_ten() {
    assert_eq!(POSTGRES_MIGRATIONS.latest_version(), 10);
}

#[test]
fn postgres_registry_versions_are_1_through_10() {
    let versions: Vec<i64> = POSTGRES_MIGRATIONS.iter().map(|m| m.version()).collect();
    assert_eq!(versions, vec![1, 2, 3, 4, 5, 6, 7, 8, 9, 10]);
}

#[test]
fn postgres_migration_implements_migration_trait() {
    // Call all trait methods on the first migration
    for m in POSTGRES_MIGRATIONS.iter() {
        let _v = m.version();
        let _n = m.name();
        let _s = m.sql();
        let _ss = m.sqlite_sql();
        let _ps = m.postgres_sql();
        let _t = m.transaction();
        let _i = m.idempotent();
    }
}
