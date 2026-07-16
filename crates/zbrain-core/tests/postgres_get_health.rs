//! 1-6-4-5 — `PostgresEngine::get_health` integration tests.
//!
//! Mirrors `libsql_get_health.rs` against the production PostgreSQL backend
//! via an ephemeral `pg-embed` instance (see `support::pg_fixture`). Confirms
//! the `BrainHealth` snapshot (consumed by `zbrain features`, `zbrain doctor`
//! brain_score, and autopilot targeted-submit) computes identically on both
//! SQL backends.
//!
//! Backend-model note (KNOWN-GAPS G24/G46): no `content_chunks` /
//! `timeline_entries` tables — embedding coverage is page-level, `stale_pages`
//! is always 0, timeline coverage parses the JSON-array `timeline` column.

mod support;

use zbrain_core::engine::{BrainEngine, PageInput};
use zbrain_core::types::LinkBatchInput;

fn page(page_type: &str, timeline_json: Option<&str>, embedding: Option<Vec<u8>>) -> PageInput {
    PageInput {
        page_type: page_type.to_string(),
        title: "T".to_string(),
        compiled_truth: "body".to_string(),
        timeline: timeline_json.map(ToString::to_string),
        embedding,
        ..Default::default()
    }
}

fn link(from: &str, to: &str) -> LinkBatchInput {
    LinkBatchInput {
        from_slug: from.to_string(),
        to_slug: to.to_string(),
        link_type: None,
        context: None,
        link_source: None,
        origin_slug: None,
        origin_field: None,
        from_source_id: None,
        to_source_id: None,
        origin_source_id: None,
    }
}

#[tokio::test]
async fn empty_brain_scores_perfect_100() {
    let fix = support::pg_fixture::PgFixture::start().await;
    let h = fix.engine.get_health().await.expect("get_health");

    assert_eq!(h.page_count, 0);
    assert_eq!(h.brain_score, 100);
    assert_eq!(h.embed_coverage_score, 35);
    assert_eq!(h.link_density_score, 25);
    assert_eq!(h.timeline_coverage_score, 15);
    assert_eq!(h.no_orphans_score, 15);
    assert_eq!(h.no_dead_links_score, 10);
    assert_eq!(h.embed_coverage, 1.0);
    assert_eq!(h.missing_embeddings, 0);
    assert!(h.most_connected.is_empty());
    assert_eq!(h.stale_pages, 0);
}

#[tokio::test]
async fn embed_coverage_is_page_level() {
    let fix = support::pg_fixture::PgFixture::start().await;

    fix.engine
        .put_page("p1", None, &page("note", None, Some(vec![1u8, 2, 3, 4])))
        .await
        .expect("put p1");
    fix.engine
        .put_page("p2", None, &page("note", None, None))
        .await
        .expect("put p2");

    let h = fix.engine.get_health().await.expect("get_health");
    assert_eq!(h.page_count, 2);
    assert_eq!(h.missing_embeddings, 1);
    assert_eq!(h.embed_coverage, 0.5);
    assert_eq!(h.orphan_pages, 2);
}

#[tokio::test]
async fn dead_links_are_deleted_aware() {
    let fix = support::pg_fixture::PgFixture::start().await;

    fix.engine
        .put_page("a", None, &page("note", None, None))
        .await
        .expect("put a");
    fix.engine
        .put_page("b", None, &page("note", None, None))
        .await
        .expect("put b");
    let n = fix
        .engine
        .add_links_batch(&[link("a", "b"), link("b", "a")])
        .await
        .expect("add links");
    assert_eq!(n, 2);

    let h0 = fix.engine.get_health().await.expect("get_health h0");
    assert_eq!(h0.dead_links, 0);
    assert_eq!(h0.orphan_pages, 0);

    fix.engine
        .soft_delete_page("b", None)
        .await
        .expect("soft delete b");
    let h1 = fix.engine.get_health().await.expect("get_health h1");
    assert_eq!(h1.page_count, 1);
    assert_eq!(h1.dead_links, 1);
}

#[tokio::test]
async fn orphan_is_islanded_not_merely_no_inbound() {
    let fix = support::pg_fixture::PgFixture::start().await;

    fix.engine
        .put_page("hub", None, &page("note", None, None))
        .await
        .expect("put hub");
    fix.engine
        .put_page("leaf", None, &page("note", None, None))
        .await
        .expect("put leaf");
    fix.engine
        .put_page("island", None, &page("note", None, None))
        .await
        .expect("put island");
    fix.engine
        .add_links_batch(&[link("hub", "leaf")])
        .await
        .expect("add link");

    let h = fix.engine.get_health().await.expect("get_health");
    assert_eq!(h.orphan_pages, 1);
}

#[tokio::test]
async fn entity_coverage_and_most_connected() {
    let fix = support::pg_fixture::PgFixture::start().await;

    fix.engine
        .put_page("alice", None, &page("person", Some("[{\"e\":1}]"), None))
        .await
        .expect("put alice");
    fix.engine
        .put_page("bob", None, &page("company", None, None))
        .await
        .expect("put bob");
    fix.engine
        .put_page("doc", None, &page("note", None, None))
        .await
        .expect("put doc");
    fix.engine
        .add_links_batch(&[link("doc", "alice"), link("alice", "bob"), link("doc", "bob")])
        .await
        .expect("add links");

    let h = fix.engine.get_health().await.expect("get_health");
    assert_eq!(h.link_coverage, 1.0);
    assert_eq!(h.timeline_coverage, 0.5);
    assert_eq!(h.most_connected.len(), 2);
    assert_eq!(h.most_connected[0].slug, "alice");
    assert_eq!(h.most_connected[0].link_count, 2);
    assert_eq!(h.most_connected[1].slug, "bob");
}

#[tokio::test]
async fn brain_score_components_sum_to_total() {
    let fix = support::pg_fixture::PgFixture::start().await;

    fix.engine
        .put_page("a", None, &page("note", Some("[{\"e\":1}]"), Some(vec![1u8, 2, 3, 4])))
        .await
        .expect("put a");
    fix.engine
        .put_page("b", None, &page("note", Some("[{\"e\":1}]"), Some(vec![5u8, 6, 7, 8])))
        .await
        .expect("put b");
    fix.engine
        .add_links_batch(&[link("a", "b"), link("b", "a")])
        .await
        .expect("add links");

    let h = fix.engine.get_health().await.expect("get_health");
    let sum = h.embed_coverage_score
        + h.link_density_score
        + h.timeline_coverage_score
        + h.no_orphans_score
        + h.no_dead_links_score;
    assert_eq!(sum, h.brain_score, "components must sum to brain_score");
    assert_eq!(h.brain_score, 100);
}
