//! Phase 7B: Links CRUD integration tests.
//!
//! Covers `add_links_batch`, `remove_link`, `get_links`, `get_backlinks`,
//! `get_backlink_counts`, and `traverse_paths` across InMemory, Libsql, and
//! Postgres backends. Postgres cases run against an ephemeral pg-embed
//! instance and live at the end of this file.

mod support;

use tempfile::NamedTempFile;
use zbrain_core::engine::{BrainEngine, EngineConfig, PageInput};
use zbrain_core::libsql::LibsqlEngine;
use zbrain_core::types::{GraphPath, LinkBatchInput};
use zbrain_core::InMemoryEngine;

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


// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn li(
    from_slug: &str,
    to_slug: &str,
    link_type: Option<&str>,
    context: Option<&str>,
) -> LinkBatchInput {
    LinkBatchInput {
        from_slug: from_slug.to_string(),
        to_slug: to_slug.to_string(),
        link_type: link_type.map(|s| s.to_string()),
        context: context.map(|s| s.to_string()),
        link_source: None,
        origin_slug: None,
        origin_field: None,
        from_source_id: None,
        to_source_id: None,
        origin_source_id: None,
    }
}

/// Seed a page into the engine. Appends "default" source_id if the engine
/// needs one.
async fn seed_page(engine: &dyn BrainEngine, slug: &str, title: &str) {
    engine
        .put_page(
            slug,
            Some("default"),
            &PageInput {
                page_type: "note".to_string(),
                title: title.to_string(),
                compiled_truth: "body".to_string(),
                ..PageInput::default()
            },
        )
        .await
        .expect(&format!("seed page {slug}"));
}

// ---------------------------------------------------------------------------
// InMemoryEngine tests
// ---------------------------------------------------------------------------

#[tokio::test]
async fn inmem_add_links_batch_single_link_roundtrip() {
    let _guard = libsql_test_guard();
    let engine = InMemoryEngine::new();
    seed_page(&engine, "alpha", "Alpha").await;
    seed_page(&engine, "bravo", "Bravo").await;

    let inserted = engine
        .add_links_batch(&[li("alpha", "bravo", Some("link"), Some("[[bravo]]"))])
        .await
        .expect("add_links_batch");
    assert_eq!(inserted, 1);

    let links = engine.get_links("alpha", None).await.expect("get_links");
    assert_eq!(links.len(), 1);
    let l = &links[0];
    assert_eq!(l.from_slug, "alpha");
    assert_eq!(l.to_slug, "bravo");
    assert_eq!(l.link_type, "link");
    assert_eq!(l.context, "[[bravo]]");
}

#[tokio::test]
async fn inmem_add_links_batch_multiple_links() {
    let _guard = libsql_test_guard();
    let engine = InMemoryEngine::new();
    seed_page(&engine, "alpha", "Alpha").await;
    seed_page(&engine, "bravo", "Bravo").await;
    seed_page(&engine, "charlie", "Charlie").await;

    let inserted = engine
        .add_links_batch(&[
            li("alpha", "bravo", Some("link"), Some("b link")),
            li("alpha", "charlie", Some("ref"), Some("c ref")),
            li("bravo", "charlie", Some("link"), Some("bc link")),
        ])
        .await
        .expect("add_links_batch");
    assert_eq!(inserted, 3);

    let links = engine.get_links("alpha", None).await.expect("get_links");
    assert_eq!(links.len(), 2);
    // get_links returns OUTGOING from alpha
    let to_slugs: Vec<&str> = links.iter().map(|l| l.to_slug.as_str()).collect();
    assert!(to_slugs.contains(&"bravo"));
    assert!(to_slugs.contains(&"charlie"));
}

#[tokio::test]
async fn inmem_add_links_batch_duplicate_suppression() {
    let _guard = libsql_test_guard();
    let engine = InMemoryEngine::new();
    seed_page(&engine, "alpha", "Alpha").await;
    seed_page(&engine, "bravo", "Bravo").await;

    // First insert.
    let n1 = engine
        .add_links_batch(&[li("alpha", "bravo", Some("link"), Some("c"))])
        .await
        .expect("first add");
    assert_eq!(n1, 1);

    // Same link again — should be suppressed (inserted=0).
    let n2 = engine
        .add_links_batch(&[li("alpha", "bravo", Some("link"), Some("c"))])
        .await
        .expect("second add");
    assert_eq!(n2, 0, "duplicate insert must return 0");

    let links = engine.get_links("alpha", None).await.expect("get_links");
    assert_eq!(links.len(), 1, "still only one link");
}

#[tokio::test]
async fn inmem_add_links_batch_different_link_type_is_distinct() {
    let _guard = libsql_test_guard();
    let engine = InMemoryEngine::new();
    seed_page(&engine, "alpha", "Alpha").await;
    seed_page(&engine, "bravo", "Bravo").await;

    // Same from/to, different link_types → two distinct links.
    let n1 = engine
        .add_links_batch(&[
            li("alpha", "bravo", Some("link"), Some("c")),
            li("alpha", "bravo", Some("ref"), Some("c")),
        ])
        .await
        .expect("add_links_batch");
    assert_eq!(n1, 2);

    let links = engine.get_links("alpha", None).await.expect("get_links");
    assert_eq!(links.len(), 2);
    let types: Vec<&str> = links.iter().map(|l| l.link_type.as_str()).collect();
    assert!(types.contains(&"link"));
    assert!(types.contains(&"ref"));
}

#[tokio::test]
async fn inmem_add_links_batch_with_origin_fields() {
    let _guard = libsql_test_guard();
    let engine = InMemoryEngine::new();
    seed_page(&engine, "alpha", "Alpha").await;
    seed_page(&engine, "bravo", "Bravo").await;
    seed_page(&engine, "origin", "Origin").await;

    let input = LinkBatchInput {
        from_slug: "alpha".into(),
        to_slug: "bravo".into(),
        link_type: Some("link".into()),
        context: Some("ctx".into()),
        link_source: Some("mentions".into()),
        origin_slug: Some("origin".into()),
        origin_field: Some("comments".into()),
        from_source_id: None,
        to_source_id: None,
        origin_source_id: None,
    };
    engine
        .add_links_batch(&[input])
        .await
        .expect("add_links_batch");

    let links = engine.get_links("alpha", None).await.expect("get_links");
    assert_eq!(links.len(), 1);
    let l = &links[0];
    assert_eq!(l.origin_slug.as_deref(), Some("origin"));
    assert_eq!(l.origin_field.as_deref(), Some("comments"));
    // link_source defaults to "markdown" when not set in write input
    // but for custom-src we set it explicitly.
    assert_eq!(l.link_source.as_deref(), Some("mentions"));
}

#[tokio::test]
async fn inmem_get_links_filters_by_source_id() {
    let _guard = libsql_test_guard();
    let engine = InMemoryEngine::new();
    seed_page(&engine, "alpha", "Alpha").await;
    seed_page(&engine, "bravo", "Bravo").await;

    // Link with default source_id scope.
    engine
        .add_links_batch(&[li("alpha", "bravo", Some("link"), Some("c"))])
        .await
        .expect("add");

    // get_links with matching source_id
    let links = engine.get_links("alpha", Some("default")).await.expect("get_links");
    assert_eq!(links.len(), 1);

    // get_links with non-matching source_id
    let links2 = engine.get_links("alpha", Some("other")).await.expect("get_links");
    assert_eq!(links2.len(), 0);
}

#[tokio::test]
async fn inmem_get_links_unknown_slug_returns_empty() {
    let _guard = libsql_test_guard();
    let engine = InMemoryEngine::new();
    let links = engine.get_links("nonexistent", None).await.expect("get_links");
    assert!(links.is_empty());
}

#[tokio::test]
async fn inmem_get_backlinks_symmetry() {
    let _guard = libsql_test_guard();
    let engine = InMemoryEngine::new();
    seed_page(&engine, "alpha", "Alpha").await;
    seed_page(&engine, "bravo", "Bravo").await;

    engine
        .add_links_batch(&[li("alpha", "bravo", Some("link"), Some("c"))])
        .await
        .expect("add");

    // Outgoing from alpha → bravo.
    let outgoing = engine.get_links("alpha", None).await.expect("get_links");
    assert_eq!(outgoing.len(), 1);
    assert_eq!(outgoing[0].to_slug, "bravo");

    // Incoming to bravo (backlinks).
    let backlinks = engine.get_backlinks("bravo", None).await.expect("get_backlinks");
    assert_eq!(backlinks.len(), 1);
    assert_eq!(backlinks[0].from_slug, "alpha");
    assert_eq!(backlinks[0].to_slug, "bravo");
}

#[tokio::test]
async fn inmem_get_backlinks_from_multiple_sources() {
    let _guard = libsql_test_guard();
    let engine = InMemoryEngine::new();
    seed_page(&engine, "alpha", "Alpha").await;
    seed_page(&engine, "bravo", "Bravo").await;
    seed_page(&engine, "charlie", "Charlie").await;

    engine
        .add_links_batch(&[
            li("alpha", "bravo", Some("link"), Some("a->b")),
            li("charlie", "bravo", Some("link"), Some("c->b")),
        ])
        .await
        .expect("add");

    let backlinks = engine.get_backlinks("bravo", None).await.expect("get_backlinks");
    assert_eq!(backlinks.len(), 2);
    let from_slugs: Vec<&str> = backlinks.iter().map(|l| l.from_slug.as_str()).collect();
    assert!(from_slugs.contains(&"alpha"));
    assert!(from_slugs.contains(&"charlie"));
}

#[tokio::test]
async fn inmem_get_backlink_counts_roundtrip() {
    let _guard = libsql_test_guard();
    let engine = InMemoryEngine::new();
    seed_page(&engine, "alpha", "Alpha").await;
    seed_page(&engine, "bravo", "Bravo").await;
    seed_page(&engine, "charlie", "Charlie").await;

    // alpha → bravo (1), charlie → bravo (2), alpha → charlie (1)
    engine
        .add_links_batch(&[
            li("alpha", "bravo", Some("link"), Some("a->b")),
            li("charlie", "bravo", Some("link"), Some("c->b")),
            li("alpha", "charlie", Some("link"), Some("a->c")),
        ])
        .await
        .expect("add");

    let counts = engine
        .get_backlink_counts(&[
            "alpha".into(),
            "bravo".into(),
            "charlie".into(),
            "zulu".into(),
        ])
        .await
        .expect("get_backlink_counts");

    assert_eq!(counts.get("alpha").copied(), Some(0));
    assert_eq!(counts.get("bravo").copied(), Some(2));
    assert_eq!(counts.get("charlie").copied(), Some(1));
    assert_eq!(counts.get("zulu").copied(), Some(0));
}

#[tokio::test]
async fn inmem_remove_link_basic() {
    let _guard = libsql_test_guard();
    let engine = InMemoryEngine::new();
    seed_page(&engine, "alpha", "Alpha").await;
    seed_page(&engine, "bravo", "Bravo").await;

    engine
        .add_links_batch(&[li("alpha", "bravo", Some("link"), Some("c"))])
        .await
        .expect("add");

    engine
        .remove_link("alpha", "bravo", Some("link"), None, None, None)
        .await
        .expect("remove_link");

    let links = engine.get_links("alpha", None).await.expect("get_links");
    assert!(links.is_empty());
}

#[tokio::test]
async fn inmem_remove_link_nonexistent_is_noop() {
    let _guard = libsql_test_guard();
    let engine = InMemoryEngine::new();
    seed_page(&engine, "alpha", "Alpha").await;
    seed_page(&engine, "bravo", "Bravo").await;

    // Should not error even though no such link exists.
    engine
        .remove_link("alpha", "bravo", Some("link"), None, None, None)
        .await
        .expect("remove_link should be no-op");
}

#[tokio::test]
async fn inmem_traverse_paths_basic_bfs() {
    let _guard = libsql_test_guard();
    let engine = InMemoryEngine::new();
    seed_page(&engine, "alpha", "Alpha").await;
    seed_page(&engine, "bravo", "Bravo").await;
    seed_page(&engine, "charlie", "Charlie").await;

    // alpha → bravo → charlie
    engine
        .add_links_batch(&[
            li("alpha", "bravo", Some("link"), Some("a->b")),
            li("bravo", "charlie", Some("link"), Some("b->c")),
        ])
        .await
        .expect("add");

    let paths: Vec<GraphPath> = engine
        .traverse_paths("alpha", Some(2), None, Some("out"), None, None)
        .await
        .expect("traverse_paths");

    assert_eq!(paths.len(), 2, "two edges: a→b, b→c");
    // First edge: a → b at depth 1
    let ab: Vec<&GraphPath> = paths
        .iter()
        .filter(|p| p.from_slug == "alpha" && p.to_slug == "bravo")
        .collect();
    assert_eq!(ab.len(), 1);
    assert_eq!(ab[0].depth, 1);

    // Second edge: b → c at depth 2
    let bc: Vec<&GraphPath> = paths
        .iter()
        .filter(|p| p.from_slug == "bravo" && p.to_slug == "charlie")
        .collect();
    assert_eq!(bc.len(), 1);
    assert_eq!(bc[0].depth, 2);
}

#[tokio::test]
async fn inmem_traverse_paths_direction_in() {
    let _guard = libsql_test_guard();
    let engine = InMemoryEngine::new();
    seed_page(&engine, "alpha", "Alpha").await;
    seed_page(&engine, "bravo", "Bravo").await;
    seed_page(&engine, "charlie", "Charlie").await;

    // alpha → bravo, charlie → bravo (two incoming to bravo)
    engine
        .add_links_batch(&[
            li("alpha", "bravo", Some("link"), Some("a->b")),
            li("charlie", "bravo", Some("link"), Some("c->b")),
        ])
        .await
        .expect("add");

    let paths: Vec<GraphPath> = engine
        .traverse_paths("bravo", Some(2), None, Some("in"), None, None)
        .await
        .expect("traverse_paths");

    assert_eq!(paths.len(), 2);
    let froms: Vec<&str> = paths.iter().map(|p| p.from_slug.as_str()).collect();
    assert!(froms.contains(&"alpha"));
    assert!(froms.contains(&"charlie"));

    // incoming edges have depth 1
    for p in &paths {
        assert_eq!(p.depth, 1, "incoming edges from direct neighbors depth=1");
    }
}

#[tokio::test]
async fn inmem_traverse_paths_depth_limit() {
    let _guard = libsql_test_guard();
    let engine = InMemoryEngine::new();
    seed_page(&engine, "alpha", "Alpha").await;
    seed_page(&engine, "bravo", "Bravo").await;
    seed_page(&engine, "charlie", "Charlie").await;

    // alpha → bravo → charlie
    engine
        .add_links_batch(&[
            li("alpha", "bravo", Some("link"), Some("a->b")),
            li("bravo", "charlie", Some("link"), Some("b->c")),
        ])
        .await
        .expect("add");

    // depth=1: only a→b
    let paths = engine
        .traverse_paths("alpha", Some(1), None, Some("out"), None, None)
        .await
        .expect("traverse_paths depth=1");
    assert_eq!(paths.len(), 1);
    assert_eq!(paths[0].to_slug, "bravo");
}

#[tokio::test]
async fn inmem_traverse_paths_link_type_filter() {
    let _guard = libsql_test_guard();
    let engine = InMemoryEngine::new();
    seed_page(&engine, "alpha", "Alpha").await;
    seed_page(&engine, "bravo", "Bravo").await;
    seed_page(&engine, "charlie", "Charlie").await;

    // alpha --link--> bravo, alpha --ref--> charlie
    engine
        .add_links_batch(&[
            li("alpha", "bravo", Some("link"), Some("a->b")),
            li("alpha", "charlie", Some("ref"), Some("a->c")),
        ])
        .await
        .expect("add");

    let paths = engine
        .traverse_paths("alpha", Some(2), Some("link"), Some("out"), None, None)
        .await
        .expect("traverse_paths link_type=link");
    assert_eq!(paths.len(), 1);
    assert_eq!(paths[0].to_slug, "bravo");
    assert_eq!(paths[0].link_type, "link");
}

// ---------------------------------------------------------------------------
// LibsqlEngine tests
// ---------------------------------------------------------------------------

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

#[tokio::test]
async fn libsql_add_links_batch_roundtrip() {
    let _guard = libsql_test_guard();
    let (engine, _tmp) = init_clean_libsql().await;
    seed_page(&engine, "alpha", "Alpha").await;
    seed_page(&engine, "bravo", "Bravo").await;

    let inserted = engine
        .add_links_batch(&[li("alpha", "bravo", Some("link"), Some("[[bravo]]"))])
        .await
        .expect("add_links_batch");
    assert_eq!(inserted, 1);

    let links = engine.get_links("alpha", None).await.expect("get_links");
    assert_eq!(links.len(), 1);
    assert_eq!(links[0].from_slug, "alpha");
    assert_eq!(links[0].to_slug, "bravo");
    assert_eq!(links[0].link_type, "link");
    assert_eq!(links[0].context, "[[bravo]]");
    // link_source defaults to "markdown"
    assert_eq!(links[0].link_source.as_deref(), Some("markdown"));
    engine.disconnect().await.expect("disconnect");
}

#[tokio::test]
async fn libsql_add_links_batch_empty_vec_returns_zero() {
    let _guard = libsql_test_guard();
    let (engine, _tmp) = init_clean_libsql().await;
    let n = engine
        .add_links_batch(&[])
        .await
        .expect("empty add_links_batch");
    assert_eq!(n, 0);
    engine.disconnect().await.expect("disconnect");
}

#[tokio::test]
async fn libsql_add_links_batch_duplicate_suppression() {
    let _guard = libsql_test_guard();
    let (engine, _tmp) = init_clean_libsql().await;
    seed_page(&engine, "alpha", "Alpha").await;
    seed_page(&engine, "bravo", "Bravo").await;

    let n1 = engine
        .add_links_batch(&[li("alpha", "bravo", Some("link"), Some("c"))])
        .await
        .expect("first add");
    assert_eq!(n1, 1);

    // INSERT OR IGNORE should silently no-op, returning 0 affected rows.
    let n2 = engine
        .add_links_batch(&[li("alpha", "bravo", Some("link"), Some("c"))])
        .await
        .expect("second add");
    assert_eq!(n2, 0, "INSERT OR IGNORE duplicate returns 0");

    let links = engine.get_links("alpha", None).await.expect("get_links");
    assert_eq!(links.len(), 1);
    engine.disconnect().await.expect("disconnect");
}

#[tokio::test]
async fn libsql_get_links_filters_deleted_pages() {
    let _guard = libsql_test_guard();
    let (engine, tmp) = init_clean_libsql().await;
    seed_page(&engine, "alpha", "Alpha").await;
    seed_page(&engine, "bravo", "Bravo").await;

    engine
        .add_links_batch(&[li("alpha", "bravo", Some("link"), Some("c"))])
        .await
        .expect("add");

    // Soft-delete bravo via raw libsql connection.
    let path_str = tmp.path().to_string_lossy().into_owned();
    let db = ::libsql::Builder::new_local(&path_str)
        .build()
        .await
        .expect("raw db");
    let raw_conn = db.connect().expect("raw conn");
    raw_conn
        .execute(
            "UPDATE pages SET deleted_at = CURRENT_TIMESTAMP WHERE slug = 'bravo' AND deleted_at IS NULL",
            (),
        )
        .await
        .expect("soft delete bravo");

    // get_links returns empty because to_page (bravo) is deleted.
    let links = engine.get_links("alpha", None).await.expect("get_links");
    assert!(links.is_empty(), "deleted to_page is filtered");
    engine.disconnect().await.expect("disconnect");
}

#[tokio::test]
async fn libsql_get_backlinks_roundtrip() {
    let _guard = libsql_test_guard();
    let (engine, _tmp) = init_clean_libsql().await;
    seed_page(&engine, "alpha", "Alpha").await;
    seed_page(&engine, "bravo", "Bravo").await;
    seed_page(&engine, "charlie", "Charlie").await;

    engine
        .add_links_batch(&[
            li("alpha", "bravo", Some("link"), Some("a->b")),
            li("charlie", "bravo", Some("link"), Some("c->b")),
        ])
        .await
        .expect("add");

    let backlinks = engine
        .get_backlinks("bravo", None)
        .await
        .expect("get_backlinks");
    assert_eq!(backlinks.len(), 2);
    let froms: Vec<&str> = backlinks.iter().map(|l| l.from_slug.as_str()).collect();
    assert!(froms.contains(&"alpha"));
    assert!(froms.contains(&"charlie"));

    engine.disconnect().await.expect("disconnect");
}

#[tokio::test]
async fn libsql_get_backlink_counts_with_zeros() {
    let _guard = libsql_test_guard();
    let (engine, _tmp) = init_clean_libsql().await;
    seed_page(&engine, "alpha", "Alpha").await;
    seed_page(&engine, "bravo", "Bravo").await;

    engine
        .add_links_batch(&[li("alpha", "bravo", Some("link"), Some("c"))])
        .await
        .expect("add");

    let counts = engine
        .get_backlink_counts(&["alpha".into(), "bravo".into(), "zulu".into()])
        .await
        .expect("get_backlink_counts");

    assert_eq!(counts.get("alpha").copied(), Some(0), "alpha has no backlinks");
    assert_eq!(counts.get("bravo").copied(), Some(1), "bravo has 1 backlink");
    assert_eq!(counts.get("zulu").copied(), Some(0), "unknown slug → 0");
    engine.disconnect().await.expect("disconnect");
}

#[tokio::test]
async fn libsql_remove_link_basic() {
    let _guard = libsql_test_guard();
    let (engine, _tmp) = init_clean_libsql().await;
    seed_page(&engine, "alpha", "Alpha").await;
    seed_page(&engine, "bravo", "Bravo").await;

    engine
        .add_links_batch(&[li("alpha", "bravo", Some("link"), Some("c"))])
        .await
        .expect("add");

    engine
        .remove_link("alpha", "bravo", Some("link"), None, None, None)
        .await
        .expect("remove_link");

    let links = engine.get_links("alpha", None).await.expect("get_links");
    assert!(links.is_empty());
    engine.disconnect().await.expect("disconnect");
}

#[tokio::test]
async fn libsql_remove_link_without_link_type_removes_all() {
    let _guard = libsql_test_guard();
    let (engine, _tmp) = init_clean_libsql().await;
    seed_page(&engine, "alpha", "Alpha").await;
    seed_page(&engine, "bravo", "Bravo").await;

    // Two links with different link_types
    engine
        .add_links_batch(&[
            li("alpha", "bravo", Some("link"), Some("c")),
            li("alpha", "bravo", Some("ref"), Some("c")),
        ])
        .await
        .expect("add");

    // Remove without link_type → removes all links matching (from, to).
    engine
        .remove_link("alpha", "bravo", None, None, None, None)
        .await
        .expect("remove_link no filter");

    let links = engine.get_links("alpha", None).await.expect("get_links");
    assert!(links.is_empty());
    engine.disconnect().await.expect("disconnect");
}

#[tokio::test]
async fn libsql_remove_link_with_link_source_filter() {
    let _guard = libsql_test_guard();
    let (engine, _tmp) = init_clean_libsql().await;
    seed_page(&engine, "alpha", "Alpha").await;
    seed_page(&engine, "bravo", "Bravo").await;

    // Insert with explicit link_source = "frontmatter".
    let input = LinkBatchInput {
        from_slug: "alpha".into(),
        to_slug: "bravo".into(),
        link_type: Some("link".into()),
        context: Some("c".into()),
        link_source: Some("frontmatter".into()),
        origin_slug: None,
        origin_field: None,
        from_source_id: None,
        to_source_id: None,
        origin_source_id: None,
    };
    let n = engine.add_links_batch(&[input]).await.expect("add");
    assert_eq!(n, 1, "insert should affect 1 row");

    // Verify link was inserted.
    let links = engine.get_links("alpha", None).await.expect("get_links 1");
    assert_eq!(links.len(), 1, "link should exist after insert");
    assert_eq!(links[0].link_source.as_deref(), Some("frontmatter"));

    // Remove with non-matching link_source → should NOT delete.
    engine
        .remove_link("alpha", "bravo", Some("link"), Some("manual"), None, None)
        .await
        .expect("remove_link with wrong source");
    let links = engine.get_links("alpha", None).await.expect("get_links 2");
    assert_eq!(links.len(), 1, "link should remain when source filter mismatches");

    // Remove with matching link_source → should delete.
    engine
        .remove_link("alpha", "bravo", Some("link"), Some("frontmatter"), None, None)
        .await
        .expect("remove_link with matching source");
    let links = engine.get_links("alpha", None).await.expect("get_links 3");
    assert!(links.is_empty(), "link should be deleted when source filter matches");

    engine.disconnect().await.expect("disconnect");
}

#[tokio::test]
async fn libsql_traverse_paths_basic_bfs() {
    let _guard = libsql_test_guard();
    let (engine, _tmp) = init_clean_libsql().await;
    seed_page(&engine, "alpha", "Alpha").await;
    seed_page(&engine, "bravo", "Bravo").await;
    seed_page(&engine, "charlie", "Charlie").await;

    engine
        .add_links_batch(&[
            li("alpha", "bravo", Some("link"), Some("a->b")),
            li("bravo", "charlie", Some("link"), Some("b->c")),
        ])
        .await
        .expect("add");

    let paths = engine
        .traverse_paths("alpha", Some(2), None, Some("out"), None, None)
        .await
        .expect("traverse_paths");

    assert_eq!(paths.len(), 2, "two edges: a→b, b→c");
    let ab: Vec<&GraphPath> = paths
        .iter()
        .filter(|p| p.from_slug == "alpha" && p.to_slug == "bravo")
        .collect();
    assert_eq!(ab.len(), 1);
    assert_eq!(ab[0].depth, 1);
    assert_eq!(ab[0].context, "a->b");
    let bc: Vec<&GraphPath> = paths
        .iter()
        .filter(|p| p.from_slug == "bravo" && p.to_slug == "charlie")
        .collect();
    assert_eq!(bc.len(), 1);
    assert_eq!(bc[0].depth, 2);
    engine.disconnect().await.expect("disconnect");
}

// ---------------------------------------------------------------------------
// Postgres integration tests
// ---------------------------------------------------------------------------
//
// Mirror the core links contract against an ephemeral pg-embed instance.
// Postgres enforces the `pages.source_id REFERENCES sources(id)` FK (SQLite
// does not by default), so the "default" source must be seeded before
// `seed_page` can insert pages.

/// Seed the "default" source so `seed_page` satisfies the pages FK.
async fn pg_seed_default_source(url: &str) {
    let pool = sqlx::postgres::PgPoolOptions::new()
        .max_connections(1)
        .connect(url)
        .await
        .expect("source seed pool");
    sqlx::query("INSERT INTO sources (id, name) VALUES ($1, $2) ON CONFLICT (id) DO NOTHING")
        .bind("default")
        .bind("default")
        .execute(&pool)
        .await
        .expect("seed default source");
    pool.close().await;
}

#[tokio::test]
async fn postgres_add_links_batch_and_get_links() {
    let _guard = libsql_test_guard();
    let fix = support::pg_fixture::PgFixture::start().await;
    let engine = &fix.engine;
    pg_seed_default_source(&fix.url).await;
    seed_page(engine, "alpha", "Alpha").await;
    seed_page(engine, "bravo", "Bravo").await;

    let inserted = engine
        .add_links_batch(&[li("alpha", "bravo", Some("link"), Some("[[bravo]]"))])
        .await
        .expect("add_links_batch");
    assert_eq!(inserted, 1);

    let links = engine.get_links("alpha", None).await.expect("get_links");
    assert_eq!(links.len(), 1);
    assert_eq!(links[0].from_slug, "alpha");
    assert_eq!(links[0].to_slug, "bravo");
}

#[tokio::test]
async fn postgres_get_backlinks_and_counts() {
    let _guard = libsql_test_guard();
    let fix = support::pg_fixture::PgFixture::start().await;
    let engine = &fix.engine;
    pg_seed_default_source(&fix.url).await;
    seed_page(engine, "alpha", "Alpha").await;
    seed_page(engine, "bravo", "Bravo").await;
    seed_page(engine, "charlie", "Charlie").await;

    engine
        .add_links_batch(&[
            li("alpha", "bravo", Some("link"), Some("a->b")),
            li("charlie", "bravo", Some("link"), Some("c->b")),
        ])
        .await
        .expect("add");

    let backlinks = engine
        .get_backlinks("bravo", None)
        .await
        .expect("get_backlinks");
    assert_eq!(backlinks.len(), 2);

    let counts = engine
        .get_backlink_counts(&["bravo".to_string(), "alpha".to_string()])
        .await
        .expect("get_backlink_counts");
    assert_eq!(counts.get("bravo").copied(), Some(2));
    assert_eq!(counts.get("alpha").copied().unwrap_or(0), 0);
}

#[tokio::test]
async fn postgres_remove_link() {
    let _guard = libsql_test_guard();
    let fix = support::pg_fixture::PgFixture::start().await;
    let engine = &fix.engine;
    pg_seed_default_source(&fix.url).await;
    seed_page(engine, "alpha", "Alpha").await;
    seed_page(engine, "bravo", "Bravo").await;

    engine
        .add_links_batch(&[li("alpha", "bravo", Some("link"), Some("c"))])
        .await
        .expect("add");
    assert_eq!(engine.get_links("alpha", None).await.unwrap().len(), 1);

    engine
        .remove_link("alpha", "bravo", Some("link"), None, None, None)
        .await
        .expect("remove_link");
    assert_eq!(engine.get_links("alpha", None).await.unwrap().len(), 0);
}

#[tokio::test]
async fn postgres_traverse_paths_basic_bfs() {
    let _guard = libsql_test_guard();
    let fix = support::pg_fixture::PgFixture::start().await;
    let engine = &fix.engine;
    pg_seed_default_source(&fix.url).await;
    seed_page(engine, "alpha", "Alpha").await;
    seed_page(engine, "bravo", "Bravo").await;
    seed_page(engine, "charlie", "Charlie").await;

    engine
        .add_links_batch(&[
            li("alpha", "bravo", Some("link"), Some("a->b")),
            li("bravo", "charlie", Some("link"), Some("b->c")),
        ])
        .await
        .expect("add");

    let paths = engine
        .traverse_paths("alpha", Some(2), None, Some("out"), None, None)
        .await
        .expect("traverse_paths");

    assert_eq!(paths.len(), 2, "two edges: a→b, b→c");
    let ab: Vec<&GraphPath> = paths
        .iter()
        .filter(|p| p.from_slug == "alpha" && p.to_slug == "bravo")
        .collect();
    assert_eq!(ab.len(), 1);
    assert_eq!(ab[0].depth, 1);
    let bc: Vec<&GraphPath> = paths
        .iter()
        .filter(|p| p.from_slug == "bravo" && p.to_slug == "charlie")
        .collect();
    assert_eq!(bc.len(), 1);
    assert_eq!(bc[0].depth, 2);
}
