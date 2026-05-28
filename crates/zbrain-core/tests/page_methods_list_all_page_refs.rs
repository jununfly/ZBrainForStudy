//! Slice 6a S6-T1 placeholder-lock test: `list_all_page_refs` placeholder lock.

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
async fn slice_6a_page_methods_list_all_page_refs_returns_unsupported() {
    let (engine, _tmp) = init_clean_engine().await;
    let err = engine
        .list_all_page_refs()
        .await
        .expect_err("6a placeholder-lock: list_all_page_refs must be Unsupported");
    let msg = err.to_string();
    assert!(
        msg.contains("pending slice 6a"),
        "expected placeholder marker, got: {msg}"
    );
    engine.disconnect().await.expect("disconnect");
}
