//! Tests for LibsqlMigration trait impl + LIBQL_MIGRATIONS registry wiring.
//!
//! The expected migration set is derived dynamically from the on-disk
//! `migrations-sqlite/*.sql` files so the tests stay drift-resistant: adding a
//! new `00NN_*.sql` automatically bumps the expected count / versions, instead
//! of silently rotting behind a hardcoded `17` like before.

#![allow(unused)]

use std::path::Path;

use zbrain_core::libsql::LIBQL_MIGRATIONS;
use zbrain_core::migration::{Migration, MigrationRegistry};

/// Scan a migrations directory and return `(count, max_version, sorted_versions)`
/// derived purely from the `NNNN_*.sql` filenames on disk.
fn scan_migration_dir(rel: &str) -> (usize, i64, Vec<i64>) {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join(rel);
    let mut versions: Vec<i64> = Vec::new();
    for entry in std::fs::read_dir(&dir)
        .unwrap_or_else(|e| panic!("cannot read migration dir {:?}: {}", dir, e))
    {
        let path = entry.unwrap().path();
        if path.extension().and_then(|e| e.to_str()) != Some("sql") {
            continue;
        }
        let stem = path.file_stem().unwrap().to_str().unwrap().to_string();
        // filename shape: NNNN_name.sql — take the leading numeric prefix
        let prefix: String = stem.chars().take_while(|c| c.is_ascii_digit()).collect();
        if let Ok(v) = prefix.parse::<i64>() {
            versions.push(v);
        }
    }
    versions.sort_unstable();
    let count = versions.len();
    let max_version = versions.last().copied().unwrap_or(0);
    (count, max_version, versions)
}

#[test]
fn libsql_registry_is_exported() {
    // Just verify LIBQL_MIGRATIONS exists and is accessible
    let _ = &LIBQL_MIGRATIONS;
}

#[test]
fn libsql_registry_matches_sql_files_on_disk() {
    let (count, max_version, versions) = scan_migration_dir("migrations-sqlite");
    assert_eq!(
        LIBQL_MIGRATIONS.len(),
        count,
        "registry must register every .sql file in migrations-sqlite/"
    );
    assert_eq!(
        LIBQL_MIGRATIONS.latest_version(),
        max_version,
        "latest_version must equal the highest .sql prefix"
    );
    let actual: Vec<i64> = LIBQL_MIGRATIONS.iter().map(|m| m.version()).collect();
    assert_eq!(
        actual, versions,
        "registered version set must match .sql filenames (skipped numbers allowed)"
    );
}

#[test]
fn libsql_migration_implements_migration_trait() {
    // Call all trait methods on every migration
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
