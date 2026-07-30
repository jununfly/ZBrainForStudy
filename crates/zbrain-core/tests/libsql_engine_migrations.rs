//! Issue #26: libsql migration system end-to-end tests.
//!
//! Tests version tracking, idempotency, and migration order correctness.
//! All tests use temp SQLite files that are cleaned up on drop.

use libsql::Builder;
use tempfile::NamedTempFile;
use zbrain_core::engine::{BrainEngine, DreamVerdictInput, EngineConfig};
use zbrain_core::libsql::LibsqlEngine;

/// Serialize all libsql FFI access in this binary. The `libsql` native
/// library is not safe to drive from multiple OS threads concurrently on
/// Windows (parallel `cargo test` threads crash with STATUS_ACCESS_VIOLATION
/// 0xc0000005). Each test grabs this guard for its whole body so the suite
/// stays green under default parallelism; serial runs are unaffected.
static LIBSQL_TEST_LOCK: std::sync::OnceLock<std::sync::Mutex<()>> = std::sync::OnceLock::new();
fn libsql_test_guard() -> std::sync::MutexGuard<'static, ()> {
    LIBSQL_TEST_LOCK
        .get_or_init(|| std::sync::Mutex::new(()))
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}


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

/// Current migration version. Bump when new migrations are added.
/// 28 = through 0028_subagent_tool_executions (latest applied migration). The
/// actual count is derived from the on-disk migrations/*.sql files; this
/// constant must track the highest migration number so the fresh-db /
/// idempotent tests assert the right version.
const EXPECTED_VERSION: i64 = 28;

// ─── Tests ──────────────────────────────────────────────────────────────────

#[tokio::test]
async fn fresh_db_runs_all_migrations_ends_at_expected_version() {
    let _guard = libsql_test_guard();
    let (_temp, engine) = temp_engine().await;
    engine.init_schema().await.unwrap();
    let version = read_version_raw(_temp.path()).await;
    assert_eq!(version, EXPECTED_VERSION);
}

#[tokio::test]
async fn idempotent_init_schema_applies_zero_migrations_second_run() {
    let _guard = libsql_test_guard();
    let (_temp, engine) = temp_engine().await;

    // First run - should apply all migrations
    engine.init_schema().await.unwrap();
    let v1 = read_version_raw(_temp.path()).await;
    assert_eq!(v1, EXPECTED_VERSION);

    // Second run - should be idempotent (no migrations applied)
    engine.init_schema().await.unwrap();
    let v2 = read_version_raw(_temp.path()).await;
    assert_eq!(v2, EXPECTED_VERSION);
}

#[tokio::test]
async fn rust_schema_version_table_exists_after_init() {
    let _guard = libsql_test_guard();
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
    let _guard = libsql_test_guard();
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
    let _guard = libsql_test_guard();
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
async fn code_edges_tables_exist_after_0021() {
    let _guard = libsql_test_guard();
    let (_temp, engine) = temp_engine().await;
    engine.init_schema().await.unwrap();

    let conn = Builder::new_local(_temp.path())
        .build()
        .await
        .unwrap()
        .connect()
        .unwrap();

    for table in ["code_edges_chunk", "code_edges_symbol"] {
        let mut rows = conn
            .query(
                "SELECT name FROM sqlite_master WHERE type='table' AND name=?",
                [table],
            )
            .await
            .unwrap();
        assert!(
            rows.next().await.unwrap().is_some(),
            "expected table {table} to exist after migration 0021"
        );
    }
}

#[tokio::test]
async fn bootstrap_creates_version_zero_row() {
    let _guard = libsql_test_guard();
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

#[tokio::test]
async fn dream_verdict_round_trip_libsql() {
    let _guard = libsql_test_guard();
    let (_temp, engine) = temp_engine().await;
    engine.init_schema().await.unwrap();

    let fp = "/brain/transcripts/2026-07-30.md";
    let hash = "ab12cd34ef56";

    // Cache miss before any write.
    assert!(engine.get_dream_verdict(fp, hash).await.unwrap().is_none());

    engine
        .put_dream_verdict(
            fp,
            hash,
            &DreamVerdictInput {
                worth_processing: true,
                reasons: vec!["recurring theme X".to_string()],
            },
        )
        .await
        .unwrap();

    let v = engine.get_dream_verdict(fp, hash).await.unwrap().unwrap();
    assert!(v.worth_processing);
    assert_eq!(v.reasons, vec!["recurring theme X".to_string()]);
    assert!(!v.judged_at.is_empty());

    // Upsert refreshes rather than duplicating the (file_path, content_hash) key.
    engine
        .put_dream_verdict(
            fp,
            hash,
            &DreamVerdictInput {
                worth_processing: false,
                reasons: vec![],
            },
        )
        .await
        .unwrap();
    let v2 = engine.get_dream_verdict(fp, hash).await.unwrap().unwrap();
    assert!(!v2.worth_processing);
    assert!(v2.reasons.is_empty());

    // A different content_hash is an independent cache entry.
    assert!(engine
        .get_dream_verdict(fp, "otherhash")
        .await
        .unwrap()
        .is_none());
}

#[tokio::test]
async fn config_round_trip_libsql() {
    let _guard = libsql_test_guard();
    let (_temp, engine) = temp_engine().await;
    engine.init_schema().await.unwrap();

    let key = "dream.synthesize.cooldown_hours";

    // Missing key → None; unset on missing key → 0 rows.
    assert!(engine.get_config(key).await.unwrap().is_none());
    assert_eq!(engine.unset_config(key).await.unwrap(), 0);

    // Set + get.
    engine.set_config(key, "6").await.unwrap();
    assert_eq!(engine.get_config(key).await.unwrap().as_deref(), Some("6"));

    // Upsert overwrites in place.
    engine.set_config(key, "24").await.unwrap();
    assert_eq!(engine.get_config(key).await.unwrap().as_deref(), Some("24"));

    // Unset removes exactly one row; key is gone afterwards.
    assert_eq!(engine.unset_config(key).await.unwrap(), 1);
    assert!(engine.get_config(key).await.unwrap().is_none());
}

#[tokio::test]
async fn collect_child_put_page_slugs_empty_and_missing_children() {
    let _guard = libsql_test_guard();
    let (_temp, engine) = temp_engine().await;
    engine.init_schema().await.unwrap();

    // Empty id list short-circuits without SQL.
    assert!(engine
        .collect_child_put_page_slugs(&[])
        .await
        .unwrap()
        .is_empty());

    // Table exists (migration 0028) but has no rows for these ids.
    assert!(engine
        .collect_child_put_page_slugs(&[101, 102])
        .await
        .unwrap()
        .is_empty());
}
