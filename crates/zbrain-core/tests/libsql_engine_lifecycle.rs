//! Slice 5 — `LibsqlEngine` lifecycle integration tests.
//!
//! Mirror of `postgres_engine_lifecycle.rs` against the libsql (embedded
//! `SQLite`) backend. Unlike the Postgres suite, libsql needs no external
//! daemon: each test allocates its own temp file via `tempfile::NamedTempFile`
//! and tears it down on drop, so the tests run unconditionally in CI.
//!
//! Schema-verification queries (`sqlite_master`) talk to the file directly
//! through a second libsql connection, mirroring the PG side's "fresh pool"
//! pattern so internal `LibsqlEngine` state is never leaked.

use libsql::Builder;
use tempfile::NamedTempFile;
use zbrain_core::engine::{BrainEngine, EngineConfig, EngineKind};
use zbrain_core::libsql::LibsqlEngine;

/// Allocate a fresh temp file path. Returned `NamedTempFile` must outlive
/// the engine — dropping it deletes the underlying file.
fn temp_db() -> NamedTempFile {
    NamedTempFile::new().expect("alloc temp db file")
}

#[tokio::test]
async fn kind_reports_libsql() {
    let engine = LibsqlEngine::new();
    assert_eq!(engine.kind(), EngineKind::Libsql);
}

#[tokio::test]
async fn connect_succeeds_against_local_file() {
    let path = temp_db();
    let engine = LibsqlEngine::new();
    let cfg = EngineConfig {
        database_url: None,
        database_path: Some(path.path().to_string_lossy().into_owned()),
    };
    engine.connect(&cfg).await.expect("connect should succeed");
    engine.disconnect().await.expect("disconnect should succeed");
}

#[tokio::test]
async fn connect_without_path_errors() {
    let engine = LibsqlEngine::new();
    let cfg = EngineConfig::default();
    let result = engine.connect(&cfg).await;
    assert!(
        result.is_err(),
        "connect without database_path must error, got {result:?}"
    );
}

#[tokio::test]
async fn init_schema_creates_pages_and_sources_tables() {
    let path = temp_db();
    let path_str = path.path().to_string_lossy().into_owned();

    let engine = LibsqlEngine::new();
    let cfg = EngineConfig {
        database_url: None,
        database_path: Some(path_str.clone()),
    };
    engine.connect(&cfg).await.expect("connect");
    engine.init_schema().await.expect("init_schema");

    // Verify schema landed by opening a side-channel connection to the
    // same file. Mirrors the PG suite's "fresh verification pool" pattern.
    let db = Builder::new_local(&path_str)
        .build()
        .await
        .expect("verification db");
    let conn = db.connect().expect("verification conn");

    let mut rows = conn
        .query(
            "SELECT name FROM sqlite_master WHERE type='table' AND name=?1",
            libsql::params!["pages"],
        )
        .await
        .expect("pages existence query");
    let pages_row = rows.next().await.expect("rows iter");
    assert!(pages_row.is_some(), "pages table must exist after init_schema");

    let mut rows = conn
        .query(
            "SELECT name FROM sqlite_master WHERE type='table' AND name=?1",
            libsql::params!["sources"],
        )
        .await
        .expect("sources existence query");
    let sources_row = rows.next().await.expect("rows iter");
    assert!(sources_row.is_some(), "sources table must exist after init_schema");

    let mut rows = conn
        .query(
            "SELECT id FROM sources WHERE id = ?1",
            libsql::params!["default"],
        )
        .await
        .expect("default source query");
    let row = rows
        .next()
        .await
        .expect("rows iter")
        .expect("default source row must be seeded");
    let id: String = row.get(0).expect("decode id");
    assert_eq!(id, "default");

    engine.disconnect().await.expect("disconnect");
}

#[tokio::test]
async fn init_schema_is_idempotent() {
    let path = temp_db();
    let engine = LibsqlEngine::new();
    let cfg = EngineConfig {
        database_url: None,
        database_path: Some(path.path().to_string_lossy().into_owned()),
    };
    engine.connect(&cfg).await.expect("connect");
    engine.init_schema().await.expect("first init_schema");
    engine
        .init_schema()
        .await
        .expect("second init_schema must be a no-op");
    engine.disconnect().await.expect("disconnect");
}
