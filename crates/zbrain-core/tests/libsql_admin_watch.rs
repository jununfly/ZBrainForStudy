//! Admin watch query integration tests against LibsqlEngine.
//!
//! Verifies graceful degradation: since `minion_jobs` does not exist in
//! the Rust schema yet, the watch snapshot must return default-zero values.

use tempfile::NamedTempFile;
use zbrain_core::admin_queries::AdminQueries;
use zbrain_core::engine::{BrainEngine, EngineConfig};
use zbrain_core::libsql::LibsqlEngine;

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
async fn libsql_watch_graceful_degradation_defaults() {
    // No minion_jobs migration → all sub-queries hit "no such table".
    let temp = temp_db();
    let engine = init_libsql(&temp).await;

    let result = engine.get_watch_snapshot().await;
    assert!(result.is_ok(), "graceful degradation must not error");
    let snap = result.unwrap();

    assert!(snap.ts_ms > 0);
    assert!(snap.by_type.is_empty());
    assert_eq!(snap.queue_health.waiting, 0);
    assert_eq!(snap.queue_health.active, 0);
    assert_eq!(snap.queue_health.stalled, 0);
    assert_eq!(snap.lease_pressure_1h, 0);
    assert!(snap.top_errors.is_empty());
    assert!(snap.budget_owners.is_empty());
}
