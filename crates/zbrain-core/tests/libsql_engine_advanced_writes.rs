//! Issue #21 — libsql advanced Page writes behavior.
//!
//! Covers the 7 methods implemented in slice 1-2-2-4 (#21):
//!   put_raw_data / get_raw_data / create_version / get_versions
//!   revert_to_version / update_slug / rewrite_links
//!
//! Each test uses its own temp SQLite file via LibsqlEngine so tests run
//! unconditionally in CI.

use serde_json::json;
use tempfile::NamedTempFile;
use zbrain_core::engine::{BrainEngine, EngineConfig, GetPageOpts, PageInput};
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


async fn init() -> (LibsqlEngine, NamedTempFile) {
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

fn note(title: &str, body: &str) -> PageInput {
    PageInput {
        page_type: "note".to_string(),
        title: title.to_string(),
        compiled_truth: body.to_string(),
        ..PageInput::default()
    }
}

// ─── put_raw_data / get_raw_data ─────────────────────────────────────────────

#[tokio::test]
async fn libsql_put_raw_data_and_get_raw_data_round_trip() {
    let _guard = libsql_test_guard();
    let (engine, _tmp) = init().await;
    engine
        .put_page("alpha", None, &note("Alpha", "body"))
        .await
        .expect("seed page");

    engine
        .put_raw_data(
            "alpha",
            "scraper",
            &json!({"url": "https://example.com"}),
            None,
        )
        .await
        .expect("put_raw_data");

    let rows = engine
        .get_raw_data("alpha", None, None)
        .await
        .expect("get_raw_data");
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].source, "scraper");
    assert_eq!(rows[0].data, json!({"url": "https://example.com"}));
    assert!(!rows[0].fetched_at.is_empty(), "fetched_at must be set");
}

#[tokio::test]
async fn libsql_put_raw_data_upserts_by_page_source() {
    let _guard = libsql_test_guard();
    let (engine, _tmp) = init().await;
    engine
        .put_page("alpha", None, &note("Alpha", "body"))
        .await
        .expect("seed");

    // Two distinct sources.
    engine
        .put_raw_data("alpha", "scraper", &json!({"v": 1}), None)
        .await
        .expect("put v1");
    engine
        .put_raw_data("alpha", "rss", &json!({"feed": "x"}), None)
        .await
        .expect("put rss");
    let all = engine.get_raw_data("alpha", None, None).await.expect("all");
    assert_eq!(all.len(), 2, "two distinct sources");

    // Re-insert same source → upsert.
    engine
        .put_raw_data("alpha", "scraper", &json!({"v": 2}), None)
        .await
        .expect("upsert");
    let after = engine
        .get_raw_data("alpha", None, None)
        .await
        .expect("after");
    assert_eq!(after.len(), 2, "upsert should not append");
    let scraper = after.iter().find(|r| r.source == "scraper").expect("row");
    assert_eq!(scraper.data, json!({"v": 2}));
}

#[tokio::test]
async fn libsql_get_raw_data_filtered_by_source() {
    let _guard = libsql_test_guard();
    let (engine, _tmp) = init().await;
    engine
        .put_page("alpha", None, &note("Alpha", "body"))
        .await
        .expect("seed");
    engine
        .put_raw_data("alpha", "scraper", &json!({"s": "scraper"}), None)
        .await
        .expect("put scraper");
    engine
        .put_raw_data("alpha", "rss", &json!({"s": "rss"}), None)
        .await
        .expect("put rss");

    let filtered = engine
        .get_raw_data("alpha", Some("scraper"), None)
        .await
        .expect("filtered");
    assert_eq!(filtered.len(), 1);
    assert_eq!(filtered[0].source, "scraper");
}

#[tokio::test]
async fn libsql_get_raw_data_returns_empty_for_unknown_page() {
    let _guard = libsql_test_guard();
    let (engine, _tmp) = init().await;
    let rows = engine.get_raw_data("ghost", None, None).await.expect("get");
    assert!(rows.is_empty());
}

#[tokio::test]
async fn libsql_put_raw_data_errors_for_unknown_page() {
    let _guard = libsql_test_guard();
    let (engine, _tmp) = init().await;
    let result = engine
        .put_raw_data("ghost", "scraper", &json!({}), None)
        .await;
    assert!(result.is_err(), "missing page must error");
}

// ─── create_version / get_versions / revert_to_version ───────────────────────

#[tokio::test]
async fn libsql_create_version_returns_snapshot() {
    let _guard = libsql_test_guard();
    let (engine, _tmp) = init().await;
    engine
        .put_page("beta", None, &note("Beta", "v1 body"))
        .await
        .expect("seed");

    let ver = engine
        .create_version("beta", None)
        .await
        .expect("create_version");
    assert!(ver.id > 0);
    assert!(ver.page_id > 0);
    assert_eq!(ver.compiled_truth, "v1 body");
    assert!(!ver.snapshot_at.is_empty());
}

#[tokio::test]
async fn libsql_get_versions_returns_newest_first() {
    let _guard = libsql_test_guard();
    let (engine, _tmp) = init().await;
    engine
        .put_page("beta", None, &note("Beta", "v1 body"))
        .await
        .expect("seed");
    engine.create_version("beta", None).await.expect("v1");

    engine
        .put_page("beta", None, &note("Beta", "v2 body"))
        .await
        .expect("update");
    engine.create_version("beta", None).await.expect("v2");

    let versions = engine.get_versions("beta", None).await.expect("get");
    assert_eq!(versions.len(), 2);
    assert_eq!(versions[0].compiled_truth, "v2 body", "newest first");
    assert_eq!(versions[1].compiled_truth, "v1 body", "oldest last");
}

#[tokio::test]
async fn libsql_revert_to_version_restores_compiled_truth() {
    let _guard = libsql_test_guard();
    let (engine, _tmp) = init().await;
    engine
        .put_page("beta", None, &note("Beta", "original body"))
        .await
        .expect("seed");
    let v1 = engine.create_version("beta", None).await.expect("v1");

    engine
        .put_page("beta", None, &note("Beta", "mutated body"))
        .await
        .expect("update");

    engine
        .revert_to_version("beta", v1.id, None)
        .await
        .expect("revert");

    let page = engine
        .get_page("beta", &GetPageOpts::default())
        .await
        .expect("get")
        .expect("exist");
    assert_eq!(page.compiled_truth, "original body");
}

#[tokio::test]
async fn libsql_revert_to_version_errors_for_unknown_version() {
    let _guard = libsql_test_guard();
    let (engine, _tmp) = init().await;
    engine
        .put_page("beta", None, &note("Beta", "body"))
        .await
        .expect("seed");
    let result = engine.revert_to_version("beta", 9999, None).await;
    assert!(result.is_err());
}

// ─── update_slug ─────────────────────────────────────────────────────────────

#[tokio::test]
async fn libsql_update_slug_renames_the_page() {
    let _guard = libsql_test_guard();
    let (engine, _tmp) = init().await;
    engine
        .put_page("old-slug", None, &note("Old", "body"))
        .await
        .expect("seed");

    engine
        .update_slug("old-slug", "new-slug", None)
        .await
        .expect("update_slug");

    let old = engine
        .get_page("old-slug", &GetPageOpts::default())
        .await
        .expect("get old");
    assert!(old.is_none());

    let new = engine
        .get_page("new-slug", &GetPageOpts::default())
        .await
        .expect("get new")
        .expect("exist");
    assert_eq!(new.slug, "new-slug");
}

#[tokio::test]
async fn libsql_update_slug_errors_on_missing_page() {
    let _guard = libsql_test_guard();
    let (engine, _tmp) = init().await;
    let result = engine.update_slug("ghost", "new-slug", None).await;
    assert!(result.is_err());
}

#[tokio::test]
async fn libsql_update_slug_errors_on_conflict() {
    let _guard = libsql_test_guard();
    let (engine, _tmp) = init().await;
    engine
        .put_page("a", None, &note("A", "body"))
        .await
        .expect("seed a");
    engine
        .put_page("b", None, &note("B", "body"))
        .await
        .expect("seed b");

    let result = engine.update_slug("a", "b", None).await;
    assert!(result.is_err());
}

// ─── rewrite_links ───────────────────────────────────────────────────────────

#[tokio::test]
async fn libsql_rewrite_links_is_noop() {
    let _guard = libsql_test_guard();
    let (engine, _tmp) = init().await;
    let result = engine.rewrite_links("old", "new").await;
    assert!(result.is_ok());
}
