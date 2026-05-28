//! Slice 6a S6-T1 placeholder-lock test: `find_duplicate_page` placeholder lock.
//!
//! Locks the `Err(Unsupported("pending slice 6a"))` contract so that
//! S6-T2 green phase has a guaranteed trigger when the real impl lands.
//! When this test starts failing, replace it with the real semantic
//! assertions (mirror behaviour from TS `findDuplicatePage`).

use tempfile::NamedTempFile;
use zbrain_core::engine::{BrainEngine, EngineConfig};
use zbrain_core::FindDuplicatePageOpts;
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
async fn slice_6a_page_methods_find_duplicate_page_returns_unsupported() {
    let (engine, _tmp) = init_clean_engine().await;
    let opts = FindDuplicatePageOpts {
        content_hash: "hash-1".to_string(),
        frontmatter_id: None,
    };
    let err = engine
        .find_duplicate_page("src-1", &opts)
        .await
        .expect_err("6a placeholder-lock: find_duplicate_page must be Unsupported");
    let msg = err.to_string();
    assert!(
        msg.contains("pending slice 6a"),
        "expected placeholder marker, got: {msg}"
    );
    engine.disconnect().await.expect("disconnect");
}
