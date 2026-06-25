//! RED test for issue #24 - LibsqlMigration trait impl + registry wiring
//!
//! These tests should FAIL initially because LibsqlMigration and LIBQL_MIGRATIONS
//! don't exist yet. They should PASS after implementation.

#![allow(unused)]

use zbrain_core::migration::{Migration, MigrationRegistry};
use zbrain_core::libsql::LIBQL_MIGRATIONS;

#[test]
fn libsql_registry_is_exported() {
    // Just verify LIBQL_MIGRATIONS exists and is accessible
    let _ = &LIBQL_MIGRATIONS;
}

#[test]
fn libsql_registry_has_eight_migrations() {
    assert_eq!(LIBQL_MIGRATIONS.len(), 8);
}

#[test]
fn libsql_registry_latest_version_is_eight() {
    assert_eq!(LIBQL_MIGRATIONS.latest_version(), 8);
}

#[test]
fn libsql_registry_versions_are_1_through_8() {
    let versions: Vec<i64> = LIBQL_MIGRATIONS.iter().map(|m| m.version()).collect();
    assert_eq!(versions, vec![1, 2, 3, 4, 5, 6, 7, 8]);
}

#[test]
fn libsql_migration_implements_migration_trait() {
    // Call all trait methods on the first migration
    for m in LIBQL_MIGRATIONS.iter() {
        let _v = m.version();
        let _n = m.name();
        let _s = m.sql();
        // sqlite_sql defaults to None
        let _ss = m.sqlite_sql();
        // postgres_sql defaults to None
        let _ps = m.postgres_sql();
        // transaction defaults to true
        let _t = m.transaction();
        // idempotent defaults to true
        let _i = m.idempotent();
    }
}
