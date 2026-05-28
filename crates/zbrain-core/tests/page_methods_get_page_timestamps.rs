//! Slice 6a S6-T1 placeholder-lock test: `get_page_timestamps` placeholder lock.

use tempfile::NamedTempFile;
use zbrain_core::engine::{BrainEngine, EngineConfig};
use zbrain_core::libsql::LibsqlEngine;

async fn init_clean_engine() -> (LibsqlEngine, NamedTempFile) {
    let path = NamedTempFile::new().expect("alloc temp db file");
    let engine = LibsqlEngine::new();
    let cfg = EngineConfig {
        database_url: None,
        database_path: Some(path.path().to_string_lossy().into_owned()),
    };
    engine.connect(&cfg).await.expect("connect");
    engine.init_schema().await.expect("init_schema");
    (engine, path)
}

#[tokio::test]
async fn slice_6a_page_methods_get_page_timestamps_returns_unsupported() {
    let (engine, _tmp) = init_clean_engine().await;
    let slugs = vec!["a".to_string(), "b".to_string()];
    let err = engine
        .get_page_timestamps(&slugs)
        .await
        .expect_err("6a placeholder-lock: get_page_timestamps must be Unsupported");
    let msg = err.to_string();
    assert!(
        msg.contains("pending slice 6a"),
        "expected placeholder marker, got: {msg}"
    );
    engine.disconnect().await.expect("disconnect");
}
