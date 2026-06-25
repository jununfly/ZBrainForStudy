//! Issue #20 — InMemory advanced Page writes behavior.
//!
//! Covers the 7 methods added in slice 1-2-2-2 (#19):
//!   put_raw_data / get_raw_data
//!   create_version / get_versions / revert_to_version
//!   update_slug / rewrite_links
//!
//! All tests target InMemoryEngine only; SQL backends land in later slices.

use serde_json::json;
use zbrain_core::engine::{BrainEngine, EngineConfig, InMemoryEngine, PageInput};

async fn init() -> InMemoryEngine {
    let engine = InMemoryEngine::default();
    engine
        .connect(&EngineConfig::default())
        .await
        .expect("connect");
    engine.init_schema().await.expect("init_schema");
    engine
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
async fn put_raw_data_and_get_raw_data_round_trip() {
    let engine = init().await;
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

    assert_eq!(rows.len(), 1, "should return the inserted row");
    assert_eq!(rows[0].source, "scraper");
    assert_eq!(rows[0].data, json!({"url": "https://example.com"}));
    assert!(!rows[0].fetched_at.is_empty(), "fetched_at must be set");
}

#[tokio::test]
async fn put_raw_data_upserts_by_page_source() {
    let engine = init().await;
    engine
        .put_page("alpha", None, &note("Alpha", "body"))
        .await
        .expect("seed page");

    // Two different sources → two rows.
    engine
        .put_raw_data("alpha", "scraper", &json!({"v": 1}), None)
        .await
        .expect("put v1");
    engine
        .put_raw_data("alpha", "rss", &json!({"feed": "x"}), None)
        .await
        .expect("put rss");

    let all = engine.get_raw_data("alpha", None, None).await.expect("all");
    assert_eq!(all.len(), 2, "two distinct sources → two rows");

    // Re-inserting same source overwrites.
    engine
        .put_raw_data("alpha", "scraper", &json!({"v": 2}), None)
        .await
        .expect("upsert v2");

    let after = engine
        .get_raw_data("alpha", None, None)
        .await
        .expect("after upsert");
    assert_eq!(after.len(), 2, "upsert should not append a third row");
    let scraper = after
        .iter()
        .find(|r| r.source == "scraper")
        .expect("scraper row");
    assert_eq!(scraper.data, json!({"v": 2}), "data should be updated");
}

#[tokio::test]
async fn get_raw_data_filtered_by_source() {
    let engine = init().await;
    engine
        .put_page("alpha", None, &note("Alpha", "body"))
        .await
        .expect("seed page");
    engine
        .put_raw_data("alpha", "scraper", &json!({"s": "scraper"}), None)
        .await
        .expect("put scraper");
    engine
        .put_raw_data("alpha", "rss", &json!({"s": "rss"}), None)
        .await
        .expect("put rss");

    let scraped = engine
        .get_raw_data("alpha", Some("scraper"), None)
        .await
        .expect("filtered get");
    assert_eq!(scraped.len(), 1, "source filter should narrow result");
    assert_eq!(scraped[0].source, "scraper");
}

#[tokio::test]
async fn get_raw_data_returns_empty_for_unknown_page() {
    let engine = init().await;
    let rows = engine
        .get_raw_data("no-such-page", None, None)
        .await
        .expect("get on missing page");
    assert!(rows.is_empty(), "missing page → empty result");
}

// ─── create_version / get_versions / revert_to_version ───────────────────────

#[tokio::test]
async fn create_version_returns_snapshot() {
    let engine = init().await;
    engine
        .put_page("beta", None, &note("Beta", "v1 body"))
        .await
        .expect("seed");

    let ver = engine
        .create_version("beta", None)
        .await
        .expect("create_version");

    assert!(ver.id > 0, "version id must be assigned");
    assert!(ver.page_id > 0, "page_id must reference the page");
    assert_eq!(ver.compiled_truth, "v1 body", "snapshot captures body");
    assert!(!ver.snapshot_at.is_empty(), "snapshot_at must be set");
}

#[tokio::test]
async fn get_versions_returns_newest_first() {
    let engine = init().await;
    engine
        .put_page("beta", None, &note("Beta", "v1 body"))
        .await
        .expect("seed");

    engine.create_version("beta", None).await.expect("v1");

    // Mutate the page to create a second version.
    engine
        .put_page("beta", None, &note("Beta", "v2 body"))
        .await
        .expect("update page");
    engine.create_version("beta", None).await.expect("v2");

    let versions = engine
        .get_versions("beta", None)
        .await
        .expect("get_versions");

    assert_eq!(versions.len(), 2, "should have 2 versions");
    // Newest-first ordering.
    assert_eq!(
        versions[0].compiled_truth, "v2 body",
        "newest version first"
    );
    assert_eq!(versions[1].compiled_truth, "v1 body", "oldest version last");
}

#[tokio::test]
async fn revert_to_version_restores_compiled_truth() {
    let engine = init().await;
    engine
        .put_page("beta", None, &note("Beta", "original body"))
        .await
        .expect("seed");

    let v1 = engine
        .create_version("beta", None)
        .await
        .expect("create v1");

    // Mutate page.
    engine
        .put_page("beta", None, &note("Beta", "mutated body"))
        .await
        .expect("update");

    // Revert to v1.
    engine
        .revert_to_version("beta", v1.id, None)
        .await
        .expect("revert");

    let page = engine
        .get_page("beta", &zbrain_core::engine::GetPageOpts::default())
        .await
        .expect("get after revert")
        .expect("page should exist");

    assert_eq!(
        page.compiled_truth, "original body",
        "compiled_truth must be restored"
    );
}

#[tokio::test]
async fn revert_to_version_returns_error_for_unknown_version() {
    let engine = init().await;
    engine
        .put_page("beta", None, &note("Beta", "body"))
        .await
        .expect("seed");

    let result = engine.revert_to_version("beta", 9999, None).await;
    assert!(result.is_err(), "unknown version_id must return an error");
}

// ─── update_slug ─────────────────────────────────────────────────────────────

#[tokio::test]
async fn update_slug_renames_the_page() {
    let engine = init().await;
    engine
        .put_page("old-slug", None, &note("Old", "body"))
        .await
        .expect("seed");

    engine
        .update_slug("old-slug", "new-slug", None)
        .await
        .expect("update_slug");

    let old = engine
        .get_page("old-slug", &zbrain_core::engine::GetPageOpts::default())
        .await
        .expect("get old");
    assert!(old.is_none(), "old slug must no longer exist");

    let new = engine
        .get_page("new-slug", &zbrain_core::engine::GetPageOpts::default())
        .await
        .expect("get new")
        .expect("new slug must exist");
    assert_eq!(new.slug, "new-slug");
}

#[tokio::test]
async fn update_slug_returns_error_when_page_not_found() {
    let engine = init().await;
    let result = engine.update_slug("ghost", "new-slug", None).await;
    assert!(result.is_err(), "missing page must return error");
}

#[tokio::test]
async fn update_slug_returns_error_when_new_slug_already_exists() {
    let engine = init().await;
    engine
        .put_page("source-slug", None, &note("Source", "body"))
        .await
        .expect("seed source");
    engine
        .put_page("taken-slug", None, &note("Taken", "body"))
        .await
        .expect("seed taken");

    let result = engine.update_slug("source-slug", "taken-slug", None).await;
    assert!(result.is_err(), "conflict on new_slug must return error");
}

// ─── rewrite_links ───────────────────────────────────────────────────────────

#[tokio::test]
async fn rewrite_links_is_a_noop_and_returns_ok() {
    let engine = init().await;
    // No pages needed — pure no-op.
    let result = engine.rewrite_links("old-slug", "new-slug").await;
    assert!(result.is_ok(), "rewrite_links must return Ok(())");
}
