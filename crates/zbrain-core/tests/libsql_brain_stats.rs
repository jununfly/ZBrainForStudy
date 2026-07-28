//! 1-6-4-2 — `LibsqlEngine::get_brain_stats` integration tests.
//!
//! Exercises the production libsql SQL path for the TS `BrainStats` counters.
//! Each test allocates its own temp file via `tempfile::NamedTempFile` (torn
//! down on drop), so the suite runs unconditionally in CI with no daemon.
//!
//! Backend-sourcing note (see `admin_queries::BrainStats` docs and
//! `docs/plans/KNOWN-GAPS.md`): Rust libsql has no `content_chunks` /
//! `timeline_entries` tables. `chunk_count` is the count of live pages with
//! non-empty `compiled_truth`; `embedded_count` is live pages carrying a
//! page-level embedding (G24); `timeline_entry_count` sums the JSON-array
//! lengths of each page's `timeline` string.

use serde_json::json;
use tempfile::NamedTempFile;
use zbrain_core::engine::{BrainEngine, EngineConfig, EngineKind, PageInput};
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

fn page(
    page_type: &str,
    compiled_truth: &str,
    timeline_json: Option<&str>,
    embedding: Option<Vec<u8>>,
) -> PageInput {
    PageInput {
        page_type: page_type.to_string(),
        title: "T".to_string(),
        compiled_truth: compiled_truth.to_string(),
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

#[tokio::test]
async fn kind_is_libsql() {
    let _guard = libsql_test_guard();
    let engine = LibsqlEngine::new();
    assert_eq!(engine.kind(), EngineKind::Libsql);
}

#[tokio::test]
async fn empty_brain_is_all_zero() {
    let _guard = libsql_test_guard();
    let path = temp_db();
    let engine = connected_engine(&path).await;
    let s = engine.get_brain_stats().await.expect("get_brain_stats");
    assert_eq!(s.page_count, 0);
    assert_eq!(s.chunk_count, 0);
    assert_eq!(s.embedded_count, 0);
    assert_eq!(s.link_count, 0);
    assert_eq!(s.tag_count, 0);
    assert_eq!(s.timeline_entry_count, 0);
    assert!(s.pages_by_type.is_empty());
}

#[tokio::test]
async fn counts_pages_chunks_embeddings_and_types() {
    let _guard = libsql_test_guard();
    let path = temp_db();
    let engine = connected_engine(&path).await;

    // p1: non-empty compiled_truth (counts toward chunk_count), with embedding.
    engine
        .put_page("p1", Some("default"), &page("note", "body one", None, Some(vec![1u8, 2, 3, 4])))
        .await
        .expect("put p1");
    // p2: non-empty compiled_truth, no embedding.
    engine
        .put_page("p2", Some("default"), &page("guide", "body two", None, None))
        .await
        .expect("put p2");
    // p3: EMPTY compiled_truth → excluded from chunk_count proxy.
    engine
        .put_page("p3", Some("default"), &page("note", "", None, None))
        .await
        .expect("put p3");

    let s = engine.get_brain_stats().await.expect("get_brain_stats");
    assert_eq!(s.page_count, 3);
    // chunk_count proxy = live pages with non-empty compiled_truth (p1, p2).
    assert_eq!(s.chunk_count, 2);
    // embedded_count = live pages with a page-level embedding (p1).
    assert_eq!(s.embedded_count, 1);
    // pages_by_type over all pages: note=2, guide=1.
    assert_eq!(s.pages_by_type.get("note"), Some(&2));
    assert_eq!(s.pages_by_type.get("guide"), Some(&1));
}

#[tokio::test]
async fn counts_distinct_tags_and_links() {
    let _guard = libsql_test_guard();
    let path = temp_db();
    let engine = connected_engine(&path).await;

    engine
        .put_page("a", Some("default"), &page("note", "x", None, None))
        .await
        .expect("put a");
    engine
        .put_page("b", Some("default"), &page("note", "y", None, None))
        .await
        .expect("put b");

    engine.add_tag("a", "rust", Some("default")).await.expect("tag a rust");
    engine.add_tag("a", "db", Some("default")).await.expect("tag a db");
    engine.add_tag("b", "rust", Some("default")).await.expect("tag b rust"); // dup tag value

    let inserted = engine.add_links_batch(&[link("a", "b")]).await.expect("add link");
    assert_eq!(inserted, 1);

    let s = engine.get_brain_stats().await.expect("get_brain_stats");
    // DISTINCT tag values: rust, db = 2 (a-rust and b-rust collapse).
    assert_eq!(s.tag_count, 2);
    assert_eq!(s.link_count, 1);
}

#[tokio::test]
async fn timeline_entry_count_sums_json_array_lengths() {
    let _guard = libsql_test_guard();
    let path = temp_db();
    let engine = connected_engine(&path).await;

    engine
        .put_page(
            "a",
            Some("default"),
            &page("note", "x", Some("[{\"e\":1},{\"e\":2},{\"e\":3}]"), None),
        )
        .await
        .expect("put a");
    engine
        .put_page("b", Some("default"), &page("note", "y", Some("[{\"e\":1}]"), None))
        .await
        .expect("put b");
    // Non-JSON timeline string parses to 0 (legacy free-text form).
    engine
        .put_page("c", Some("default"), &page("note", "z", Some("T1 -> T2"), None))
        .await
        .expect("put c");

    let s = engine.get_brain_stats().await.expect("get_brain_stats");
    // 3 + 1 + 0 = 4
    assert_eq!(s.timeline_entry_count, 4);
}

#[tokio::test]
async fn page_count_excludes_soft_deleted_but_pages_by_type_does_not() {
    let _guard = libsql_test_guard();
    let path = temp_db();
    let engine = connected_engine(&path).await;

    engine
        .put_page("live", Some("default"), &page("note", "x", None, None))
        .await
        .expect("put live");
    engine
        .put_page("gone", Some("default"), &page("note", "y", None, None))
        .await
        .expect("put gone");
    engine
        .soft_delete_page("gone", Some("default"))
        .await
        .expect("soft delete gone");

    let s = engine.get_brain_stats().await.expect("get_brain_stats");
    // page_count filters WHERE deleted_at IS NULL.
    assert_eq!(s.page_count, 1);
    // pages_by_type has no soft-delete filter (mirrors TS) → both counted.
    assert_eq!(s.pages_by_type.get("note"), Some(&2));
    // chunk_count also filters soft-deleted (WHERE deleted_at IS NULL) → only live.
    assert_eq!(s.chunk_count, 1);
}
