//! Admin spend query integration tests against LibsqlEngine.
//!
//! Verifies graceful degradation: since `oauth_clients` / `mcp_spend_log` /
//! `mcp_spend_reservations` / `minion_jobs` do not exist in the Rust schema
//! yet, all spend queries must return empty results instead of 500s.

use tempfile::NamedTempFile;
use zbrain_core::admin_queries::AdminQueries;
use zbrain_core::engine::{BrainEngine, EngineConfig};
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


fn temp_db() -> NamedTempFile {
    NamedTempFile::new().expect("alloc temp db file")
}

async fn init_libsql(temp: &NamedTempFile) -> LibsqlEngine {
    let engine = LibsqlEngine::new();
    let config = EngineConfig {
        database_path: Some(temp.path().to_string_lossy().to_string()),
        ..Default::default()
    };
    engine.connect(&config).await.expect("connect");
    engine.init_schema().await.expect("init_schema");
    engine
}

#[tokio::test]
async fn libsql_spend_graceful_degradation_empty() {
    let _guard = libsql_test_guard();
    // No oauth_clients / mcp_spend_log migration exists yet, so the query
    // hits "no such table" and must return empty Vec (not 500).
    let temp = temp_db();
    let engine = init_libsql(&temp).await;

    let result = engine.list_agent_client_spend().await;
    assert!(result.is_ok(), "graceful degradation must not error");
    let items = result.unwrap();
    assert!(items.is_empty(), "missing tables → empty vec");
}
