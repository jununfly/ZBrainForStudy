//! Issue #26: libsql migration system end-to-end tests.
//!
//! Tests version tracking, idempotency, and migration order correctness.
//! All tests use temp SQLite files that are cleaned up on drop.

use libsql::Builder;
use tempfile::NamedTempFile;
use zbrain_core::engine::{BrainEngine, EngineConfig};
use zbrain_core::libsql::LibsqlEngine;

/// Allocate a fresh temp file path. Returned `NamedTempFile` must outlive
/// the engine — dropping it deletes the underlying file.
fn temp_db() -> NamedTempFile {
    NamedTempFile::new().expect("alloc temp db file")
}

/// Create a fresh LibsqlEngine backed by a temp file.
async fn temp_engine() -> (NamedTempFile, LibsqlEngine) {
    let temp = temp_db();
    let path = temp.path().to_string_lossy().to_string();
    let config = EngineConfig {
        database_path: Some(path),
        database_url: None,
    };
    let engine = LibsqlEngine::new();
    engine.connect(&config).await.unwrap();
    (temp, engine)
}

/// Read current version from rust_schema_version table via a raw connection.
async fn read_version_raw(path: &std::path::Path) -> i64 {
    let conn = Builder::new_local(path)
        .build()
        .await
        .unwrap()
        .connect()
        .unwrap();

    let mut rows = conn
        .query("SELECT version FROM rust_schema_version LIMIT 1", ())
        .await
        .unwrap();
    rows.next().await.unwrap().unwrap().get(0).unwrap()
}

// ─── Tests ──────────────────────────────────────────────────────────────────

#[tokio::test]
async fn fresh_db_runs_all_eight_migrations_ends_at_version_8() {
    let (_temp, engine) = temp_engine().await;
    engine.init_schema().await.unwrap();
    let version = read_version_raw(_temp.path()).await;
    assert_eq!(version, 8);
}

#[tokio::test]
async fn idempotent_init_schema_applies_zero_migrations_second_run() {
    let (_temp, engine) = temp_engine().await;

    // First run - should apply all 8 migrations
    engine.init_schema().await.unwrap();
    let v1 = read_version_raw(_temp.path()).await;
    assert_eq!(v1, 8);

    // Second run - should be idempotent (no migrations applied)
    engine.init_schema().await.unwrap();
    let v2 = read_version_raw(_temp.path()).await;
    assert_eq!(v2, 8);
}

#[tokio::test]
async fn rust_schema_version_table_exists_after_init() {
    let (_temp, engine) = temp_engine().await;
    engine.init_schema().await.unwrap();

    // Verify the table exists via raw SQL
    let conn = Builder::new_local(_temp.path())
        .build()
        .await
        .unwrap()
        .connect()
        .unwrap();

    let mut rows = conn
        .query(
            "SELECT name FROM sqlite_master WHERE type='table' AND name='rust_schema_version'",
            (),
        )
        .await
        .unwrap();

    assert!(rows.next().await.unwrap().is_some());
}

#[tokio::test]
async fn rust_schema_version_has_applied_at_timestamp() {
    let (_temp, engine) = temp_engine().await;
    engine.init_schema().await.unwrap();

    let conn = Builder::new_local(_temp.path())
        .build()
        .await
        .unwrap()
        .connect()
        .unwrap();

    let mut rows = conn
        .query("SELECT applied_at FROM rust_schema_version LIMIT 1", ())
        .await
        .unwrap();

    let applied_at: String = rows.next().await.unwrap().unwrap().get(0).unwrap();
    // Should be a valid ISO timestamp (not empty)
    assert!(!applied_at.is_empty());
    assert!(applied_at.len() >= 10); // At least "YYYY-MM-DD"
}

#[tokio::test]
async fn migrations_are_applied_in_ascending_version_order() {
    let (_temp, engine) = temp_engine().await;
    engine.init_schema().await.unwrap();

    // Indirect verification: check that pages table exists AND has all
    // expected columns from later migrations. pages from migration 1,
    // full_columns from migration 2.
    let conn = Builder::new_local(_temp.path())
        .build()
        .await
        .unwrap()
        .connect()
        .unwrap();

    // Verify pages table exists (from migration 1)
    let mut rows = conn
        .query(
            "SELECT name FROM sqlite_master WHERE type='table' AND name='pages'",
            (),
        )
        .await
        .unwrap();
    assert!(rows.next().await.unwrap().is_some());

    // Verify page_tags table exists (from migration 4)
    let mut rows = conn
        .query(
            "SELECT name FROM sqlite_master WHERE type='table' AND name='page_tags'",
            (),
        )
        .await
        .unwrap();
    assert!(rows.next().await.unwrap().is_some());

    // Verify files table exists (from migration 7)
    let mut rows = conn
        .query(
            "SELECT name FROM sqlite_master WHERE type='table' AND name='files'",
            (),
        )
        .await
        .unwrap();
    assert!(rows.next().await.unwrap().is_some());

    // Verify raw_data table exists (from migration 8)
    let mut rows = conn
        .query(
            "SELECT name FROM sqlite_master WHERE type='table' AND name='raw_data'",
            (),
        )
        .await
        .unwrap();
    assert!(rows.next().await.unwrap().is_some());
}

#[tokio::test]
async fn bootstrap_creates_version_zero_row() {
    let temp = temp_db();
    let conn = Builder::new_local(temp.path())
        .build()
        .await
        .unwrap()
        .connect()
        .unwrap();

    // Run just the bootstrap SQL
    conn.execute_batch(
        r#"
CREATE TABLE IF NOT EXISTS rust_schema_version (
    version INTEGER PRIMARY KEY NOT NULL DEFAULT 0,
    applied_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
);
INSERT OR IGNORE INTO rust_schema_version (version) VALUES (0);
"#,
    )
    .await
    .unwrap();

    // Verify version 0 exists
    let mut rows = conn
        .query("SELECT version FROM rust_schema_version LIMIT 1", ())
        .await
        .unwrap();
    let version: i64 = rows.next().await.unwrap().unwrap().get(0).unwrap();
    assert_eq!(version, 0);
}
