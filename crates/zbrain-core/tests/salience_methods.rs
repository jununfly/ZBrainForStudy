//! Phase 7C 1-3-2: Salience tests — touch_salience + get_recent_salience.
//!
//! Covers InMemory + Libsql backends. Postgres stays in a separate file
//! (pg-embed startup ~5-8s).

mod support;

use tempfile::NamedTempFile;
use zbrain_core::engine::{BrainEngine, EngineConfig, GetPageOpts, PageInput};
use zbrain_core::libsql::LibsqlEngine;
use zbrain_core::InMemoryEngine;

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

async fn seed_page(engine: &dyn BrainEngine, slug: &str, source_id: &str) {
    engine
        .put_page(
            slug,
            Some(source_id),
            &PageInput {
                page_type: "note".to_string(),
                title: slug.to_string(),
                compiled_truth: format!("truth for {slug}"),
                ..PageInput::default()
            },
        )
        .await
        .expect(&format!("seed page {slug}"));
}

async fn init_in_memory() -> InMemoryEngine {
    let engine = InMemoryEngine::new();
    engine
        .connect(&EngineConfig::default())
        .await
        .expect("connect");
    engine.init_schema().await.expect("init_schema");
    engine
}

async fn init_clean_libsql() -> (LibsqlEngine, NamedTempFile) {
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

// ---------------------------------------------------------------------------
// touch_salience — InMemory
// ---------------------------------------------------------------------------

#[tokio::test]
async fn inmem_touch_salience_bumps_timestamp() {
    let engine = init_in_memory().await;
    seed_page(&engine, "alpha", "default").await;

    let result = engine.touch_salience("alpha", "default").await.expect("touch");
    assert!(result, "should return true when page exists");

    // Verify salience_touched_at is set
    let page = engine.get_page("alpha", &GetPageOpts { source_id: Some("default".to_string()), include_deleted: false }).await.expect("get").expect("page");    assert!(page.salience_touched_at.is_some(), "salience_touched_at should be Some after touch");
}

#[tokio::test]
async fn inmem_touch_salience_returns_false_for_missing_page() {
    let engine = init_in_memory().await;
    let result = engine.touch_salience("nonexistent", "default").await.expect("touch");
    assert!(!result, "should return false when page doesn't exist");
}

#[tokio::test]
async fn inmem_touch_salience_returns_false_for_wrong_source() {
    let engine = init_in_memory().await;
    seed_page(&engine, "beta", "src-a").await;

    let result = engine.touch_salience("beta", "src-b").await.expect("touch");
    assert!(!result, "wrong source_id should return false");
}

// ---------------------------------------------------------------------------
// touch_salience — Libsql
// ---------------------------------------------------------------------------

#[tokio::test]
async fn libsql_touch_salience_bumps_timestamp() {
    let (engine, _tmp) = init_clean_libsql().await;
    seed_page(&engine, "alpha", "default").await;

    let result = engine.touch_salience("alpha", "default").await.expect("touch");
    assert!(result);

    let page = engine.get_page("alpha", &GetPageOpts { source_id: Some("default".to_string()), include_deleted: false }).await.expect("get").expect("page");    assert!(page.salience_touched_at.is_some());
    engine.disconnect().await.expect("disconnect");
}

#[tokio::test]
async fn libsql_touch_salience_returns_false_for_missing_page() {
    let (engine, _tmp) = init_clean_libsql().await;

    let result = engine.touch_salience("nonexistent", "default").await.expect("touch");
    assert!(!result);
    engine.disconnect().await.expect("disconnect");
}

// ---------------------------------------------------------------------------
// get_recent_salience — InMemory
// ---------------------------------------------------------------------------

#[tokio::test]
async fn inmem_get_recent_salience_returns_empty_for_fresh_brain() {
    let engine = init_in_memory().await;
    let results = engine
        .get_recent_salience(14, 20, None)
        .await
        .expect("get_recent");
    assert!(results.is_empty(), "empty brain should return empty");
}

#[tokio::test]
async fn inmem_get_recent_salience_includes_recent_pages() {
    let engine = init_in_memory().await;
    seed_page(&engine, "recent", "default").await;
    seed_page(&engine, "old", "default").await;

    let results = engine
        .get_recent_salience(14, 20, None)
        .await
        .expect("get_recent");

    assert!(!results.is_empty(), "should find recent pages");
    assert!(results.iter().any(|r| r.slug == "recent"));
    assert!(results.iter().any(|r| r.slug == "old"));
}

#[tokio::test]
async fn inmem_get_recent_salience_respects_limit() {
    let engine = init_in_memory().await;
    for i in 0..10 {
        seed_page(&engine, &format!("page_{i}"), "default").await;
    }

    let results = engine
        .get_recent_salience(14, 3, None)
        .await
        .expect("get_recent");

    assert_eq!(results.len(), 3, "should respect limit");
}

#[tokio::test]
async fn inmem_get_recent_salience_respects_slug_prefix() {
    let engine = init_in_memory().await;
    seed_page(&engine, "wiki/foo", "default").await;
    seed_page(&engine, "wiki/bar", "default").await;
    seed_page(&engine, "blog/post1", "default").await;

    let results = engine
        .get_recent_salience(14, 20, Some("wiki/"))
        .await
        .expect("get_recent");

    assert_eq!(results.len(), 2, "should only return wiki/ pages");
    assert!(results.iter().all(|r| r.slug.starts_with("wiki/")));
}

#[tokio::test]
async fn inmem_get_recent_salience_sorts_by_score_desc() {
    let engine = init_in_memory().await;
    seed_page(&engine, "low", "default").await;
    seed_page(&engine, "high", "default").await;

    let results = engine
        .get_recent_salience(14, 20, None)
        .await
        .expect("get_recent");

    for w in results.windows(2) {
        assert!(
            w[0].score >= w[1].score,
            "results should be sorted by score desc: {} >= {}",
            w[0].score, w[1].score
        );
    }
}

#[tokio::test]
async fn inmem_get_recent_salience_has_correct_fields() {
    let engine = init_in_memory().await;
    seed_page(&engine, "check_fields", "default").await;

    let results = engine
        .get_recent_salience(14, 20, None)
        .await
        .expect("get_recent");

    let r = results.iter().find(|r| r.slug == "check_fields").expect("should find page");
    assert_eq!(r.slug, "check_fields");
    assert_eq!(r.source_id, "default");
    assert_eq!(r.title, "check_fields");
    assert_eq!(r.page_type, "note");
    assert!(!r.updated_at.is_empty());
    assert_eq!(r.take_count, 0);
    assert_eq!(r.take_avg_weight, 0.0);
    assert!(r.score > 0.0, "score should be positive");
}

// ---------------------------------------------------------------------------
// get_recent_salience — Libsql
// ---------------------------------------------------------------------------

#[tokio::test]
async fn libsql_get_recent_salience_returns_empty_for_fresh_brain() {
    let (engine, _tmp) = init_clean_libsql().await;

    let results = engine
        .get_recent_salience(14, 20, None)
        .await
        .expect("get_recent");
    assert!(results.is_empty());
    engine.disconnect().await.expect("disconnect");
}

#[tokio::test]
async fn libsql_get_recent_salience_includes_recent_pages() {
    let (engine, _tmp) = init_clean_libsql().await;
    seed_page(&engine, "recent", "default").await;
    seed_page(&engine, "old", "default").await;

    let results = engine
        .get_recent_salience(14, 20, None)
        .await
        .expect("get_recent");

    assert!(!results.is_empty());
    assert!(results.iter().any(|r| r.slug == "recent"));
    assert!(results.iter().any(|r| r.slug == "old"));
    engine.disconnect().await.expect("disconnect");
}

#[tokio::test]
async fn libsql_get_recent_salience_respects_limit() {
    let (engine, _tmp) = init_clean_libsql().await;
    for i in 0..10 {
        seed_page(&engine, &format!("page_{i}"), "default").await;
    }

    let results = engine
        .get_recent_salience(14, 3, None)
        .await
        .expect("get_recent");

    assert_eq!(results.len(), 3);
    engine.disconnect().await.expect("disconnect");
}

#[tokio::test]
async fn libsql_get_recent_salience_respects_slug_prefix() {
    let (engine, _tmp) = init_clean_libsql().await;
    seed_page(&engine, "wiki/foo", "default").await;
    seed_page(&engine, "wiki/bar", "default").await;
    seed_page(&engine, "blog/post1", "default").await;

    let results = engine
        .get_recent_salience(14, 20, Some("wiki/"))
        .await
        .expect("get_recent");

    assert_eq!(results.len(), 2);
    assert!(results.iter().all(|r| r.slug.starts_with("wiki/")));
    engine.disconnect().await.expect("disconnect");
}

#[tokio::test]
async fn libsql_get_recent_salience_sorts_by_score_desc() {
    let (engine, _tmp) = init_clean_libsql().await;
    seed_page(&engine, "low", "default").await;
    seed_page(&engine, "high", "default").await;

    let results = engine
        .get_recent_salience(14, 20, None)
        .await
        .expect("get_recent");

    for w in results.windows(2) {
        assert!(w[0].score >= w[1].score);
    }
    engine.disconnect().await.expect("disconnect");
}

#[tokio::test]
async fn libsql_get_recent_salience_touch_affects_window() {
    let (engine, _tmp) = init_clean_libsql().await;
    seed_page(&engine, "touched", "default").await;

    // Touch the page — this bumps salience_touched_at
    engine.touch_salience("touched", "default").await.expect("touch");

    // Even with days=0 (only pages touched within the last 0 days),
    // the touched page should still appear because salience_touched_at is recent.
    let results = engine
        .get_recent_salience(0, 20, None)
        .await
        .expect("get_recent");

    // With days=0, boundary = now, so only pages with salience_touched_at >= now appear.
    // Our touch_salience sets salience_touched_at = datetime('now'), so it should appear.
    assert!(results.iter().any(|r| r.slug == "touched"), "touched page should appear in 0-day window");
    engine.disconnect().await.expect("disconnect");
}

#[tokio::test]
async fn libsql_get_recent_salience_has_correct_fields() {
    let (engine, _tmp) = init_clean_libsql().await;
    seed_page(&engine, "check_fields", "default").await;

    let results = engine
        .get_recent_salience(14, 20, None)
        .await
        .expect("get_recent");

    let r = results.iter().find(|r| r.slug == "check_fields").expect("should find page");
    assert_eq!(r.slug, "check_fields");
    assert_eq!(r.source_id, "default");
    assert_eq!(r.title, "check_fields");
    assert_eq!(r.page_type, "note");
    assert!(!r.updated_at.is_empty());
    assert_eq!(r.take_count, 0);
    assert_eq!(r.take_avg_weight, 0.0);
    assert!(r.score > 0.0);
    engine.disconnect().await.expect("disconnect");
}
