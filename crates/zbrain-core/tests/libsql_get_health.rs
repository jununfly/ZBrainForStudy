//! 1-6-4-5 — `LibsqlEngine::get_health` integration tests.
//!
//! Exercises the production libsql SQL path for the TS `BrainHealth` snapshot
//! consumed by `zbrain features`, `zbrain doctor` (brain_score check), and
//! autopilot's targeted-submit path. Each test allocates its own temp file via
//! `tempfile::NamedTempFile` (torn down on drop), so the suite runs
//! unconditionally in CI with no daemon.
//!
//! Backend-model note (see `admin_queries::BrainStats` docs and
//! `docs/plans/KNOWN-GAPS.md` G24/G46): Rust libsql has no `content_chunks` /
//! `timeline_entries` tables. Embedding coverage is computed at the PAGE level
//! (one embedding BLOB per page, G24), `stale_pages` is always 0 (no
//! timeline_entries.created_at to compare), and timeline coverage parses each
//! page's JSON-array `timeline` string. Soft-deleted pages are excluded and
//! `dead_links` is deleted-aware — matching the InMemory `get_health`.

use serde_json::json;
use tempfile::NamedTempFile;
use zbrain_core::engine::{BrainEngine, EngineConfig, PageInput};
use zbrain_core::libsql::LibsqlEngine;
use zbrain_core::types::LinkBatchInput;
use zbrain_core::PageKind;

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

async fn connected_engine(path: &NamedTempFile) -> LibsqlEngine {
    let engine = LibsqlEngine::new();
    let cfg = EngineConfig {
        database_url: None,
        database_path: Some(path.path().to_string_lossy().into_owned()),
    };
    engine.connect(&cfg).await.expect("connect");
    engine.init_schema().await.expect("init_schema");
    engine
}

fn page(page_type: &str, timeline_json: Option<&str>, embedding: Option<Vec<u8>>) -> PageInput {
    PageInput {
        page_type: page_type.to_string(),
        title: "T".to_string(),
        compiled_truth: "body".to_string(),
        timeline: timeline_json.map(ToString::to_string),
        frontmatter: Some(json!({})),
        content_hash: None,
        page_kind: Some(PageKind::Markdown),
        effective_date: None,
        effective_date_source: None,
        import_filename: None,
        chunker_version: None,
        source_path: None,
        source_kind: None,
        source_uri: None,
        ingested_via: None,
        ingested_at: None,
        last_retrieved_at: None,
        embedding,
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

/// Empty brain scores a PERFECT 100/100 (v0.37.10.0 semantics): nothing to
/// embed, link, or orphan — no coverage problem to penalize. This mirrors the
/// InMemory engine and TS pglite-engine behavior.
#[tokio::test]
async fn empty_brain_scores_perfect_100() {
    let _guard = libsql_test_guard();
    let path = temp_db();
    let engine = connected_engine(&path).await;
    let h = engine.get_health().await.expect("get_health");

    assert_eq!(h.page_count, 0);
    assert_eq!(h.brain_score, 100);
    assert_eq!(h.embed_coverage_score, 35);
    assert_eq!(h.link_density_score, 25);
    assert_eq!(h.timeline_coverage_score, 15);
    assert_eq!(h.no_orphans_score, 15);
    assert_eq!(h.no_dead_links_score, 10);
    // Empty brain → full embed coverage (nothing missing).
    assert_eq!(h.embed_coverage, 1.0);
    assert_eq!(h.missing_embeddings, 0);
    assert!(h.most_connected.is_empty());
    // stale_pages is always 0 (no timeline_entries table).
    assert_eq!(h.stale_pages, 0);
}

/// embed_coverage is page-level (G24): fraction of live pages carrying a
/// non-null embedding BLOB. missing_embeddings counts the rest.
#[tokio::test]
async fn embed_coverage_is_page_level() {
    let _guard = libsql_test_guard();
    let path = temp_db();
    let engine = connected_engine(&path).await;

    engine
        .put_page("p1", Some("default"), &page("note", None, Some(vec![1u8, 2, 3, 4])))
        .await
        .expect("put p1");
    engine
        .put_page("p2", Some("default"), &page("note", None, None))
        .await
        .expect("put p2");

    let h = engine.get_health().await.expect("get_health");
    assert_eq!(h.page_count, 2);
    assert_eq!(h.missing_embeddings, 1);
    assert_eq!(h.embed_coverage, 0.5);
    // 2 pages, 0 links → link_density 0, all orphaned.
    assert_eq!(h.orphan_pages, 2);
}

/// dead_links = links whose target page is missing or soft-deleted
/// (deleted-aware). A link to a live page is not dead.
#[tokio::test]
async fn dead_links_are_deleted_aware() {
    let _guard = libsql_test_guard();
    let path = temp_db();
    let engine = connected_engine(&path).await;

    engine
        .put_page("a", Some("default"), &page("note", None, None))
        .await
        .expect("put a");
    engine
        .put_page("b", Some("default"), &page("note", None, None))
        .await
        .expect("put b");
    // a -> b (live target) and b -> a.
    let n = engine
        .add_links_batch(&[link("a", "b"), link("b", "a")])
        .await
        .expect("add links");
    assert_eq!(n, 2);

    // Before deletion: no dead links, no orphans (a<->b fully linked).
    let h0 = engine.get_health().await.expect("get_health h0");
    assert_eq!(h0.dead_links, 0);
    assert_eq!(h0.orphan_pages, 0);

    // Soft-delete b: the a->b link now points at a soft-deleted page → dead.
    engine
        .soft_delete_page("b", Some("default"))
        .await
        .expect("soft delete b");
    let h1 = engine.get_health().await.expect("get_health h1");
    assert_eq!(h1.page_count, 1); // only a is live
    assert_eq!(h1.dead_links, 1); // a -> b is dead
}

/// orphan_pages = islanded (no inbound AND no outbound). A page with only an
/// outbound link is NOT an orphan.
#[tokio::test]
async fn orphan_is_islanded_not_merely_no_inbound() {
    let _guard = libsql_test_guard();
    let path = temp_db();
    let engine = connected_engine(&path).await;

    engine
        .put_page("hub", Some("default"), &page("note", None, None))
        .await
        .expect("put hub");
    engine
        .put_page("leaf", Some("default"), &page("note", None, None))
        .await
        .expect("put leaf");
    engine
        .put_page("island", Some("default"), &page("note", None, None))
        .await
        .expect("put island");
    // hub -> leaf. hub has outbound, leaf has inbound. island has neither.
    engine
        .add_links_batch(&[link("hub", "leaf")])
        .await
        .expect("add link");

    let h = engine.get_health().await.expect("get_health");
    // Only `island` is islanded.
    assert_eq!(h.orphan_pages, 1);
}

/// Entity (person/company) link_coverage + timeline_coverage, plus
/// most_connected ordering (desc by link count, tie-break slug asc), excluding
/// zero-link entities.
#[tokio::test]
async fn entity_coverage_and_most_connected() {
    let _guard = libsql_test_guard();
    let path = temp_db();
    let engine = connected_engine(&path).await;

    // Two entity pages + one note. alice has inbound + timeline; bob has
    // neither inbound nor timeline.
    engine
        .put_page("alice", Some("default"), &page("person", Some("[{\"e\":1}]"), None))
        .await
        .expect("put alice");
    engine
        .put_page("bob", Some("default"), &page("company", None, None))
        .await
        .expect("put bob");
    engine
        .put_page("doc", Some("default"), &page("note", None, None))
        .await
        .expect("put doc");
    // doc -> alice (alice gets 1 inbound), alice -> bob (alice out, bob in),
    // doc -> bob (bob 2nd inbound). So alice link_count = 2, bob = 2.
    engine
        .add_links_batch(&[link("doc", "alice"), link("alice", "bob"), link("doc", "bob")])
        .await
        .expect("add links");

    let h = engine.get_health().await.expect("get_health");
    // entity_count = 2 (alice, bob). Both have inbound → link_coverage 1.0.
    assert_eq!(h.link_coverage, 1.0);
    // Only alice has timeline → timeline_coverage 0.5.
    assert_eq!(h.timeline_coverage, 0.5);
    // most_connected: alice & bob both lc=2, tie-break slug asc → alice, bob.
    // doc is a note (not entity) so excluded.
    assert_eq!(h.most_connected.len(), 2);
    assert_eq!(h.most_connected[0].slug, "alice");
    assert_eq!(h.most_connected[0].link_count, 2);
    assert_eq!(h.most_connected[1].slug, "bob");
}

/// brain_score components sum to brain_score by construction, and a healthy
/// fully-embedded/linked brain scores high.
#[tokio::test]
async fn brain_score_components_sum_to_total() {
    let _guard = libsql_test_guard();
    let path = temp_db();
    let engine = connected_engine(&path).await;

    engine
        .put_page("a", Some("default"), &page("note", Some("[{\"e\":1}]"), Some(vec![1u8, 2, 3, 4])))
        .await
        .expect("put a");
    engine
        .put_page("b", Some("default"), &page("note", Some("[{\"e\":1}]"), Some(vec![5u8, 6, 7, 8])))
        .await
        .expect("put b");
    engine
        .add_links_batch(&[link("a", "b"), link("b", "a")])
        .await
        .expect("add links");

    let h = engine.get_health().await.expect("get_health");
    let sum = h.embed_coverage_score
        + h.link_density_score
        + h.timeline_coverage_score
        + h.no_orphans_score
        + h.no_dead_links_score;
    assert_eq!(sum, h.brain_score, "components must sum to brain_score");
    // Fully embedded (2/2), well-linked (2 links / 2 pages = density 1.0),
    // full timeline, no orphans, no dead links → perfect.
    assert_eq!(h.brain_score, 100);
}
