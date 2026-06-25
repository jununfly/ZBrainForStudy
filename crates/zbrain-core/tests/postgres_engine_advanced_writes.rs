//! Issue #22 — Postgres advanced Page writes behavior.
//!
//! Covers the 7 methods implemented in slice 1-2-2-5 (#22):
//!   put_raw_data / get_raw_data / create_version / get_versions
//!   revert_to_version / update_slug / rewrite_links
//!
//! Each test uses PgFixture for an ephemeral PostgreSQL instance.
//! The 0009 migration is applied automatically by sqlx::migrate!.

mod support;

use serde_json::json;
use zbrain_core::engine::{BrainEngine, GetPageOpts, PageInput};

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
async fn postgres_put_raw_data_and_get_raw_data_round_trip() {
    let fix = support::pg_fixture::PgFixture::start().await;
    let engine = &fix.engine;
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
    assert!(!rows[0].fetched_at.is_empty());
}

#[tokio::test]
async fn postgres_put_raw_data_upserts_by_page_source() {
    let fix = support::pg_fixture::PgFixture::start().await;
    let engine = &fix.engine;
    engine
        .put_page("alpha", None, &note("Alpha", "body"))
        .await
        .expect("seed");

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

    engine
        .put_raw_data("alpha", "scraper", &json!({"v": 2}), None)
        .await
        .expect("upsert");
    let after = engine
        .get_raw_data("alpha", None, None)
        .await
        .expect("after");
    assert_eq!(after.len(), 2);
    let scraper = after.iter().find(|r| r.source == "scraper").expect("row");
    assert_eq!(scraper.data, json!({"v": 2}));
}

#[tokio::test]
async fn postgres_get_raw_data_filtered_by_source() {
    let fix = support::pg_fixture::PgFixture::start().await;
    let engine = &fix.engine;
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
async fn postgres_get_raw_data_returns_empty_for_unknown_page() {
    let fix = support::pg_fixture::PgFixture::start().await;
    let engine = &fix.engine;
    let rows = engine.get_raw_data("ghost", None, None).await.expect("get");
    assert!(rows.is_empty());
}

#[tokio::test]
async fn postgres_put_raw_data_errors_for_unknown_page() {
    let fix = support::pg_fixture::PgFixture::start().await;
    let engine = &fix.engine;
    let result = engine
        .put_raw_data("ghost", "scraper", &json!({}), None)
        .await;
    assert!(result.is_err(), "missing page must error");
}

// ─── create_version / get_versions / revert_to_version ───────────────────────

#[tokio::test]
async fn postgres_create_version_returns_snapshot() {
    let fix = support::pg_fixture::PgFixture::start().await;
    let engine = &fix.engine;
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
async fn postgres_get_versions_returns_newest_first() {
    let fix = support::pg_fixture::PgFixture::start().await;
    let engine = &fix.engine;
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
async fn postgres_revert_to_version_restores_compiled_truth() {
    let fix = support::pg_fixture::PgFixture::start().await;
    let engine = &fix.engine;
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
async fn postgres_revert_to_version_errors_for_unknown_version() {
    let fix = support::pg_fixture::PgFixture::start().await;
    let engine = &fix.engine;
    engine
        .put_page("beta", None, &note("Beta", "body"))
        .await
        .expect("seed");
    let result = engine.revert_to_version("beta", 9999, None).await;
    assert!(result.is_err());
}

// ─── update_slug ─────────────────────────────────────────────────────────────

#[tokio::test]
async fn postgres_update_slug_renames_the_page() {
    let fix = support::pg_fixture::PgFixture::start().await;
    let engine = &fix.engine;
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
async fn postgres_update_slug_errors_on_missing_page() {
    let fix = support::pg_fixture::PgFixture::start().await;
    let engine = &fix.engine;
    let result = engine.update_slug("ghost", "new-slug", None).await;
    assert!(result.is_err());
}

#[tokio::test]
async fn postgres_update_slug_errors_on_conflict() {
    let fix = support::pg_fixture::PgFixture::start().await;
    let engine = &fix.engine;
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
async fn postgres_rewrite_links_is_noop() {
    let fix = support::pg_fixture::PgFixture::start().await;
    let engine = &fix.engine;
    let result = engine.rewrite_links("old", "new").await;
    assert!(result.is_ok());
}
