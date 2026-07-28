//! Phase 7C: Graph integration tests.
//!
//! Covers `get_adjacency_boosts` and `traverse_paths` across InMemory and
//! Libsql backends. Postgres stays in separate files (pg-embed startup ~5-8s).

mod support;

use tempfile::NamedTempFile;
use zbrain_core::engine::{BrainEngine, CreateSourceInput, EngineConfig, Page, PageInput};
use zbrain_core::libsql::LibsqlEngine;
use zbrain_core::types::LinkBatchInput;
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

/// Like `li` but with explicit from/to source_ids for cross-source link
/// resolution in libsql/postgres (which resolve slugs within source scope).
fn li_src(
    from_slug: &str,
    to_slug: &str,
    from_source_id: &str,
    to_source_id: &str,
    link_type: &str,
    context: &str,
) -> LinkBatchInput {
    LinkBatchInput {
        from_slug: from_slug.to_string(),
        to_slug: to_slug.to_string(),
        link_type: Some(link_type.to_string()),
        context: Some(context.to_string()),
        link_source: None,
        origin_slug: None,
        origin_field: None,
        from_source_id: Some(from_source_id.to_string()),
        to_source_id: Some(to_source_id.to_string()),
        origin_source_id: None,
    }
}

/// Seed a page, return the full Page including its auto-assigned id.
async fn seed_and_get(engine: &dyn BrainEngine, slug: &str, source_id: Option<&str>) -> Page {
    engine
        .put_page(
            slug,
            source_id,
            &PageInput {
                page_type: "note".to_string(),
                title: slug.to_string(),
                compiled_truth: format!("truth for {slug}"),
                ..PageInput::default()
            },
        )
        .await
        .expect(&format!("seed page {slug}"))
}

/// Seed a page with source_id = "default". Kept for tests that don't care
/// about the returned Page.
async fn seed_page(engine: &dyn BrainEngine, slug: &str) {
    seed_and_get(engine, slug, Some("default")).await;
}

/// Create a source record so pages with a custom source_id don't hit FK
/// constraints in libsql/postgres.
async fn seed_source(engine: &dyn BrainEngine, id: &str) {
    engine
        .create_source(&CreateSourceInput {
            id: id.to_string(),
            name: id.to_string(),
            config: None,
        })
        .await
        .expect(&format!("seed source {id}"));
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

/// Run getAdjacencyBoosts contract tests against any engine.
async fn run_adjacency_boosts_contract(engine: &dyn BrainEngine) {
    // ---- 1. empty input → empty HashMap -----------------------------------
    {
        let result = engine.get_adjacency_boosts(&[]).await.expect("empty input");
        assert!(result.is_empty(), "empty input must return empty map");
    }

    // ---- 2. no links → empty HashMap (only pages with hits ≥ 1 appear) ---
    {
        let a = seed_and_get(engine, "a", Some("default")).await;
        let b = seed_and_get(engine, "b", Some("default")).await;

        let result = engine
            .get_adjacency_boosts(&[a.id, b.id])
            .await
            .expect("no links");
        assert!(
            result.is_empty(),
            "no links: pages with zero hits must not appear"
        );
    }

    // ---- 3. single link A→B, same source → B has hits=1, cross_source=0 --
    {
        let a = seed_and_get(engine, "a3", Some("default")).await;
        let b = seed_and_get(engine, "b3", Some("default")).await;

        engine
            .add_links_batch(&[li("a3", "b3", Some("link"), Some("[[b3]]"))])
            .await
            .expect("add link");

        let result = engine
            .get_adjacency_boosts(&[a.id, b.id])
            .await
            .expect("get boosts");

        assert_eq!(result.len(), 1, "only B should have a hit");
        let row = result.get(&b.id).expect("B should be in result");
        assert_eq!(row.hits, 1);
        assert_eq!(row.cross_source_hits, 0, "same source → no cross-source");
    }
}

/// Engine-specific adjacency_boosts tests that need fresh engine per scenario.
/// (The contract fn above can share a single InMemory, but for libsql we init
/// fresh each time anyway.)
async fn run_adjacency_boosts_full(engine: &dyn BrainEngine) {
    // ---- 4. reciprocal links A↔B (each links to the other) ---------------
    {
        let a = seed_and_get(engine, "alpha", Some("default")).await;
        let b = seed_and_get(engine, "bravo", Some("default")).await;

        engine
            .add_links_batch(&[
                li("alpha", "bravo", Some("link"), Some("a->b")),
                li("bravo", "alpha", Some("link"), Some("b->a")),
            ])
            .await
            .expect("add links");

        let result = engine
            .get_adjacency_boosts(&[a.id, b.id])
            .await
            .expect("get boosts");

        assert_eq!(result.len(), 2, "both pages have incoming links");
        let row_a = result.get(&a.id).expect("A should have hit from B");
        let row_b = result.get(&b.id).expect("B should have hit from A");
        assert_eq!(row_a.hits, 1);
        assert_eq!(row_b.hits, 1);
        // same source → cross_source_hits = 0
        assert_eq!(row_a.cross_source_hits, 0);
        assert_eq!(row_b.cross_source_hits, 0);
    }
}

// ---------------------------------------------------------------------------
// InMemory getAdjacencyBoosts
// ---------------------------------------------------------------------------

#[tokio::test]
async fn inmem_adjacency_boosts_empty_and_no_links() {
    let _guard = libsql_test_guard();
    let engine = init_in_memory().await;
    run_adjacency_boosts_contract(&engine).await;
}

#[tokio::test]
async fn inmem_adjacency_boosts_reciprocal() {
    let _guard = libsql_test_guard();
    let engine = init_in_memory().await;
    run_adjacency_boosts_full(&engine).await;
}

#[tokio::test]
async fn inmem_adjacency_boosts_many_to_one() {
    let _guard = libsql_test_guard();
    let engine = init_in_memory().await;
    let a = seed_and_get(&engine, "alpha", Some("default")).await;
    let b = seed_and_get(&engine, "bravo", Some("default")).await;
    let c = seed_and_get(&engine, "charlie", Some("default")).await;

    engine
        .add_links_batch(&[
            li("alpha", "charlie", Some("link"), Some("a->c")),
            li("bravo", "charlie", Some("link"), Some("b->c")),
        ])
        .await
        .expect("add links");

    let result = engine
        .get_adjacency_boosts(&[a.id, b.id, c.id])
        .await
        .expect("get boosts");

    assert_eq!(result.len(), 1);
    let row_c = result.get(&c.id).unwrap();
    assert_eq!(row_c.hits, 2);
    assert_eq!(row_c.cross_source_hits, 0);
}

#[tokio::test]
async fn inmem_adjacency_boosts_cross_source() {
    let _guard = libsql_test_guard();
    let engine = init_in_memory().await;
    seed_source(&engine, "source-one").await;
    seed_source(&engine, "source-two").await;
    seed_source(&engine, "source-three").await;
    let a = seed_and_get(&engine, "a-cs", Some("source-one")).await;
    let b = seed_and_get(&engine, "b-cs", Some("source-two")).await;
    let c = seed_and_get(&engine, "c-cs", Some("source-three")).await;

    engine
        .add_links_batch(&[
            li_src("a-cs", "c-cs", "source-one", "source-three", "link", "x"),
            li_src("b-cs", "c-cs", "source-two", "source-three", "link", "y"),
        ])
        .await
        .expect("add links");

    let result = engine
        .get_adjacency_boosts(&[a.id, b.id, c.id])
        .await
        .expect("get boosts");

    let row_c = result.get(&c.id).unwrap();
    assert_eq!(row_c.hits, 2);
    assert_eq!(row_c.cross_source_hits, 2);
}

#[tokio::test]
async fn inmem_adjacency_boosts_same_source_exclusion() {
    let _guard = libsql_test_guard();
    let engine = init_in_memory().await;
    seed_source(&engine, "team-src").await;
    let a = seed_and_get(&engine, "a-same", Some("team-src")).await;
    let b = seed_and_get(&engine, "b-same", Some("team-src")).await;

    engine
        .add_links_batch(&[li_src("a-same", "b-same", "team-src", "team-src", "link", "same-src")])
        .await
        .expect("add link");

    let result = engine
        .get_adjacency_boosts(&[a.id, b.id])
        .await
        .expect("get boosts");

    let row_b = result.get(&b.id).unwrap();
    assert_eq!(row_b.hits, 1);
    assert_eq!(row_b.cross_source_hits, 0);
}

#[tokio::test]
async fn inmem_adjacency_boosts_not_in_input_filtered() {
    let _guard = libsql_test_guard();
    let engine = init_in_memory().await;
    let a = seed_and_get(&engine, "a-filter", Some("default")).await;
    let _d = seed_and_get(&engine, "d-filter", Some("default")).await;

    engine
        .add_links_batch(&[li("d-filter", "a-filter", Some("link"), Some("d->a"))])
        .await
        .expect("add link");

    let result = engine
        .get_adjacency_boosts(&[a.id])
        .await
        .expect("get boosts");

    assert!(result.is_empty(), "D not in input → no hit for A");
}

#[tokio::test]
async fn inmem_adjacency_boosts_chain_topology() {
    let _guard = libsql_test_guard();
    let engine = init_in_memory().await;
    let a = seed_and_get(&engine, "a-chain", Some("default")).await;
    let b = seed_and_get(&engine, "b-chain", Some("default")).await;
    let c = seed_and_get(&engine, "c-chain", Some("default")).await;

    engine
        .add_links_batch(&[
            li("a-chain", "b-chain", Some("link"), Some("a->b")),
            li("b-chain", "c-chain", Some("link"), Some("b->c")),
        ])
        .await
        .expect("add links");

    let result = engine
        .get_adjacency_boosts(&[a.id, b.id, c.id])
        .await
        .expect("get boosts");

    assert_eq!(result.len(), 2);
    let row_b = result.get(&b.id).unwrap();
    assert_eq!(row_b.hits, 1);
    let row_c = result.get(&c.id).unwrap();
    assert_eq!(row_c.hits, 1);
}

#[tokio::test]
async fn inmem_adjacency_boosts_mixed_cross_source() {
    let _guard = libsql_test_guard();
    let engine = init_in_memory().await;
    seed_source(&engine, "s1").await;
    seed_source(&engine, "s3").await;
    let s1a = seed_and_get(&engine, "s1-a", Some("s1")).await;
    let s1b = seed_and_get(&engine, "s1-b", Some("s1")).await;
    let t = seed_and_get(&engine, "t-mix", Some("s3")).await;

    engine
        .add_links_batch(&[
            li("s1-a", "t-mix", Some("link"), Some("x")),
            li("s1-b", "t-mix", Some("link"), Some("y")),
        ])
        .await
        .expect("add links");

    let result = engine
        .get_adjacency_boosts(&[s1a.id, s1b.id, t.id])
        .await
        .expect("get boosts");

    let row = result.get(&t.id).unwrap();
    assert_eq!(row.hits, 2);
    // 2 pages from s1 → target in s3. Distinct sources from target: just "s1" → 1.
    assert_eq!(row.cross_source_hits, 1);
}

#[tokio::test]
async fn inmem_adjacency_boosts_null_source_coalesced() {
    let _guard = libsql_test_guard();
    let engine = init_in_memory().await;
    let a = seed_and_get(&engine, "a-null", None).await;
    let b = seed_and_get(&engine, "b-null", None).await;

    engine
        .add_links_batch(&[li("a-null", "b-null", Some("link"), Some("null"))])
        .await
        .expect("add link");

    let result = engine
        .get_adjacency_boosts(&[a.id, b.id])
        .await
        .expect("get boosts");

    let row_b = result.get(&b.id).unwrap();
    assert_eq!(row_b.hits, 1);
    assert_eq!(row_b.cross_source_hits, 0);
}

// ---------------------------------------------------------------------------
// Libsql getAdjacencyBoosts
// ---------------------------------------------------------------------------

#[tokio::test]
async fn libsql_adjacency_boosts_empty_and_no_links() {
    let _guard = libsql_test_guard();
    let (engine, _tmp) = init_clean_libsql().await;
    run_adjacency_boosts_contract(&engine).await;
}

#[tokio::test]
async fn libsql_adjacency_boosts_reciprocal() {
    let _guard = libsql_test_guard();
    let (engine, _tmp) = init_clean_libsql().await;
    run_adjacency_boosts_full(&engine).await;
}

#[tokio::test]
async fn libsql_adjacency_boosts_many_to_one() {
    let _guard = libsql_test_guard();
    let (engine, _tmp) = init_clean_libsql().await;
    let a = seed_and_get(&engine, "alpha", Some("default")).await;
    let b = seed_and_get(&engine, "bravo", Some("default")).await;
    let c = seed_and_get(&engine, "charlie", Some("default")).await;

    engine
        .add_links_batch(&[
            li("alpha", "charlie", Some("link"), Some("a->c")),
            li("bravo", "charlie", Some("link"), Some("b->c")),
        ])
        .await
        .expect("add links");

    let result = engine
        .get_adjacency_boosts(&[a.id, b.id, c.id])
        .await
        .expect("get boosts");

    assert_eq!(result.len(), 1);
    let row_c = result.get(&c.id).unwrap();
    assert_eq!(row_c.hits, 2);
    assert_eq!(row_c.cross_source_hits, 0);
}

#[tokio::test]
async fn libsql_adjacency_boosts_cross_source() {
    let _guard = libsql_test_guard();
    let (engine, _tmp) = init_clean_libsql().await;
    seed_source(&engine, "source-one").await;
    seed_source(&engine, "source-two").await;
    seed_source(&engine, "source-three").await;
    let a = seed_and_get(&engine, "a-cs", Some("source-one")).await;
    let b = seed_and_get(&engine, "b-cs", Some("source-two")).await;
    let c = seed_and_get(&engine, "c-cs", Some("source-three")).await;

    engine
        .add_links_batch(&[
            li_src("a-cs", "c-cs", "source-one", "source-three", "link", "x"),
            li_src("b-cs", "c-cs", "source-two", "source-three", "link", "y"),
        ])
        .await
        .expect("add links");

    let result = engine
        .get_adjacency_boosts(&[a.id, b.id, c.id])
        .await
        .expect("get boosts");

    let row_c = result.get(&c.id).unwrap();
    assert_eq!(row_c.hits, 2);
    assert_eq!(row_c.cross_source_hits, 2);
}

#[tokio::test]
async fn libsql_adjacency_boosts_same_source_exclusion() {
    let _guard = libsql_test_guard();
    let (engine, _tmp) = init_clean_libsql().await;
    seed_source(&engine, "team-src").await;
    let a = seed_and_get(&engine, "a-se", Some("team-src")).await;
    let b = seed_and_get(&engine, "b-se", Some("team-src")).await;

    engine
        .add_links_batch(&[li_src("a-se", "b-se", "team-src", "team-src", "link", "same-src")])
        .await
        .expect("add link");

    let result = engine
        .get_adjacency_boosts(&[a.id, b.id])
        .await
        .expect("get boosts");

    let row_b = result.get(&b.id).unwrap();
    assert_eq!(row_b.hits, 1);
    assert_eq!(row_b.cross_source_hits, 0);
}

#[tokio::test]
async fn libsql_adjacency_boosts_not_in_input_filtered() {
    let _guard = libsql_test_guard();
    let (engine, _tmp) = init_clean_libsql().await;
    let a = seed_and_get(&engine, "a-filt", Some("default")).await;
    let _d = seed_and_get(&engine, "d-filt", Some("default")).await;

    engine
        .add_links_batch(&[li("d-filt", "a-filt", Some("link"), Some("d->a"))])
        .await
        .expect("add link");

    let result = engine
        .get_adjacency_boosts(&[a.id])
        .await
        .expect("get boosts");

    assert!(result.is_empty());
}

#[tokio::test]
async fn libsql_adjacency_boosts_chain_topology() {
    let _guard = libsql_test_guard();
    let (engine, _tmp) = init_clean_libsql().await;
    let a = seed_and_get(&engine, "a-chain", Some("default")).await;
    let b = seed_and_get(&engine, "b-chain", Some("default")).await;
    let c = seed_and_get(&engine, "c-chain", Some("default")).await;

    engine
        .add_links_batch(&[
            li("a-chain", "b-chain", Some("link"), Some("a->b")),
            li("b-chain", "c-chain", Some("link"), Some("b->c")),
        ])
        .await
        .expect("add links");

    let result = engine
        .get_adjacency_boosts(&[a.id, b.id, c.id])
        .await
        .expect("get boosts");

    assert_eq!(result.len(), 2);
    let row_b = result.get(&b.id).unwrap();
    assert_eq!(row_b.hits, 1);
    let row_c = result.get(&c.id).unwrap();
    assert_eq!(row_c.hits, 1);
}

#[tokio::test]
async fn libsql_adjacency_boosts_mixed_cross_source() {
    let _guard = libsql_test_guard();
    let (engine, _tmp) = init_clean_libsql().await;
    seed_source(&engine, "s1").await;
    seed_source(&engine, "s3").await;
    let s1a = seed_and_get(&engine, "s1-a", Some("s1")).await;
    let s1b = seed_and_get(&engine, "s1-b", Some("s1")).await;
    let t = seed_and_get(&engine, "t-mix", Some("s3")).await;

    engine
        .add_links_batch(&[
            li_src("s1-a", "t-mix", "s1", "s3", "link", "x"),
            li_src("s1-b", "t-mix", "s1", "s3", "link", "y"),
        ])
        .await
        .expect("add links");

    let result = engine
        .get_adjacency_boosts(&[s1a.id, s1b.id, t.id])
        .await
        .expect("get boosts");

    let row = result.get(&t.id).unwrap();
    assert_eq!(row.hits, 2);
    assert_eq!(row.cross_source_hits, 1);
}

#[tokio::test]
async fn libsql_adjacency_boosts_null_source_coalesced() {
    let _guard = libsql_test_guard();
    let (engine, _tmp) = init_clean_libsql().await;
    let a = seed_and_get(&engine, "a-null", None).await;
    let b = seed_and_get(&engine, "b-null", None).await;

    engine
        .add_links_batch(&[li("a-null", "b-null", Some("link"), Some("null"))])
        .await
        .expect("add link");

    let result = engine
        .get_adjacency_boosts(&[a.id, b.id])
        .await
        .expect("get boosts");

    let row_b = result.get(&b.id).unwrap();
    assert_eq!(row_b.hits, 1);
    assert_eq!(row_b.cross_source_hits, 0);
}

// ---------------------------------------------------------------------------
// InMemory traverse_paths (Libsql/Postgres stubs → skip for now)
// ---------------------------------------------------------------------------

#[tokio::test]
async fn inmem_traverse_paths_single_hop_forward() {
    let _guard = libsql_test_guard();
    let engine = init_in_memory().await;
    seed_page(&engine, "start").await;
    seed_page(&engine, "target").await;

    engine
        .add_links_batch(&[li("start", "target", Some("link"), Some("[[target]]"))])
        .await
        .expect("add link");

    let paths = engine
        .traverse_paths("start", Some(1), None, None, Some("default"), None)
        .await
        .expect("traverse");

    assert_eq!(paths.len(), 1);
    assert_eq!(paths[0].from_slug, "start");
    assert_eq!(paths[0].to_slug, "target");
    assert_eq!(paths[0].depth, 1);
    assert_eq!(paths[0].link_type, "link");
}

#[tokio::test]
async fn inmem_traverse_paths_chain_depth_2() {
    let _guard = libsql_test_guard();
    let engine = init_in_memory().await;
    seed_page(&engine, "a").await;
    seed_page(&engine, "b").await;
    seed_page(&engine, "c").await;

    engine
        .add_links_batch(&[
            li("a", "b", Some("link"), Some("a->b")),
            li("b", "c", Some("link"), Some("b->c")),
        ])
        .await
        .expect("add links");

    let paths = engine
        .traverse_paths("a", Some(2), None, None, Some("default"), None)
        .await
        .expect("traverse");

    // BFS visits a→b (depth 1), then b→c (depth 2)
    assert_eq!(paths.len(), 2);
    let froms: Vec<&str> = paths.iter().map(|p| p.from_slug.as_str()).collect();
    let tos: Vec<&str> = paths.iter().map(|p| p.to_slug.as_str()).collect();
    assert!(froms.contains(&"a"));
    assert!(tos.contains(&"b"));
    assert!(froms.contains(&"b"));
    assert!(tos.contains(&"c"));
}

#[tokio::test]
async fn inmem_traverse_paths_depth_0_returns_empty() {
    let _guard = libsql_test_guard();
    let engine = init_in_memory().await;
    seed_page(&engine, "a").await;
    seed_page(&engine, "b").await;

    engine
        .add_links_batch(&[li("a", "b", Some("link"), Some("c"))])
        .await
        .expect("add link");

    let paths = engine
        .traverse_paths("a", Some(0), None, None, Some("default"), None)
        .await
        .expect("traverse");

    assert!(paths.is_empty(), "depth=0 must return empty");
}

#[tokio::test]
async fn inmem_traverse_paths_reverse_direction() {
    let _guard = libsql_test_guard();
    let engine = init_in_memory().await;
    seed_page(&engine, "a").await;
    seed_page(&engine, "b").await;

    engine
        .add_links_batch(&[li("a", "b", Some("link"), Some("a->b"))])
        .await
        .expect("add link");

    let paths = engine
        .traverse_paths("b", Some(1), None, Some("in"), Some("default"), None)
        .await
        .expect("traverse");

    assert_eq!(paths.len(), 1);
    // In reverse, we walk from b → a (incoming), but GraphPath preserves
    // original edge direction: from_slug=a, to_slug=b.
    assert_eq!(paths[0].from_slug, "a");
    assert_eq!(paths[0].to_slug, "b");
}

#[tokio::test]
async fn inmem_traverse_paths_both_directions() {
    let _guard = libsql_test_guard();
    let engine = init_in_memory().await;
    seed_page(&engine, "center").await;
    seed_page(&engine, "left").await;
    seed_page(&engine, "right").await;

    engine
        .add_links_batch(&[
            li("left", "center", Some("link"), Some("l->c")),
            li("center", "right", Some("link"), Some("c->r")),
        ])
        .await
        .expect("add links");

    let paths = engine
        .traverse_paths("center", Some(1), None, Some("both"), Some("default"), None)
        .await
        .expect("traverse");

    assert_eq!(paths.len(), 2);
    // Should find both: left→center (in) and center→right (out)
}

#[tokio::test]
async fn inmem_traverse_paths_unknown_slug_returns_empty() {
    let _guard = libsql_test_guard();
    let engine = init_in_memory().await;
    let paths = engine
        .traverse_paths("nonexistent", Some(1), None, None, Some("default"), None)
        .await
        .expect("traverse");
    assert!(paths.is_empty());
}

#[tokio::test]
async fn inmem_traverse_paths_link_type_filter() {
    let _guard = libsql_test_guard();
    let engine = init_in_memory().await;
    seed_page(&engine, "a").await;
    seed_page(&engine, "b").await;
    seed_page(&engine, "c").await;

    engine
        .add_links_batch(&[
            li("a", "b", Some("link"), Some("wikilink")),
            li("a", "c", Some("ref"), Some("reference")),
        ])
        .await
        .expect("add links");

    // With filter: only "link" type
    let paths = engine
        .traverse_paths("a", Some(1), Some("link"), None, Some("default"), None)
        .await
        .expect("traverse");

    assert_eq!(paths.len(), 1);
    assert_eq!(paths[0].to_slug, "b");
    assert_eq!(paths[0].link_type, "link");
}

#[tokio::test]
async fn inmem_traverse_paths_respects_depth_limit() {
    let _guard = libsql_test_guard();
    let engine = init_in_memory().await;
    seed_page(&engine, "d0").await;
    seed_page(&engine, "d1").await;
    seed_page(&engine, "d2").await;
    seed_page(&engine, "d3").await;

    engine
        .add_links_batch(&[
            li("d0", "d1", Some("link"), Some("0->1")),
            li("d1", "d2", Some("link"), Some("1->2")),
            li("d2", "d3", Some("link"), Some("2->3")),
        ])
        .await
        .expect("add links");

    // depth=1: only d0→d1
    let paths = engine
        .traverse_paths("d0", Some(1), None, None, Some("default"), None)
        .await
        .expect("traverse");
    assert_eq!(paths.len(), 1);
    assert_eq!(paths[0].to_slug, "d1");

    // depth=2: d0→d1 + d1→d2
    let paths = engine
        .traverse_paths("d0", Some(2), None, None, Some("default"), None)
        .await
        .expect("traverse");
    assert_eq!(paths.len(), 2);
    let tos: Vec<&str> = paths.iter().map(|p| p.to_slug.as_str()).collect();
    assert!(tos.contains(&"d1"));
    assert!(tos.contains(&"d2"));
}

// ---------------------------------------------------------------------------
// Libsql traverse_paths
// ---------------------------------------------------------------------------

#[tokio::test]
async fn libsql_traverse_paths_single_hop_forward() {
    let _guard = libsql_test_guard();
    let (engine, _tmp) = init_clean_libsql().await;
    seed_page(&engine, "start").await;
    seed_page(&engine, "target").await;

    engine
        .add_links_batch(&[li("start", "target", Some("link"), Some("[[target]]"))])
        .await
        .expect("add link");

    let paths = engine
        .traverse_paths("start", Some(1), None, None, Some("default"), None)
        .await
        .expect("traverse");

    assert_eq!(paths.len(), 1);
    assert_eq!(paths[0].from_slug, "start");
    assert_eq!(paths[0].to_slug, "target");
    assert_eq!(paths[0].depth, 1);
    assert_eq!(paths[0].link_type, "link");
}

#[tokio::test]
async fn libsql_traverse_paths_chain_depth_2() {
    let _guard = libsql_test_guard();
    let (engine, _tmp) = init_clean_libsql().await;
    seed_page(&engine, "a").await;
    seed_page(&engine, "b").await;
    seed_page(&engine, "c").await;

    engine
        .add_links_batch(&[
            li("a", "b", Some("link"), Some("a->b")),
            li("b", "c", Some("link"), Some("b->c")),
        ])
        .await
        .expect("add links");

    let paths = engine
        .traverse_paths("a", Some(2), None, None, Some("default"), None)
        .await
        .expect("traverse");

    assert_eq!(paths.len(), 2);
    let ab: Vec<_> = paths
        .iter()
        .filter(|p| p.from_slug == "a" && p.to_slug == "b")
        .collect();
    assert_eq!(ab.len(), 1);
    assert_eq!(ab[0].depth, 1);
    let bc: Vec<_> = paths
        .iter()
        .filter(|p| p.from_slug == "b" && p.to_slug == "c")
        .collect();
    assert_eq!(bc.len(), 1);
    assert_eq!(bc[0].depth, 2);
}

#[tokio::test]
async fn libsql_traverse_paths_depth_zero_returns_empty() {
    let _guard = libsql_test_guard();
    let (engine, _tmp) = init_clean_libsql().await;
    seed_page(&engine, "a").await;
    seed_page(&engine, "b").await;

    engine
        .add_links_batch(&[li("a", "b", Some("link"), Some("c"))])
        .await
        .expect("add link");

    let paths = engine
        .traverse_paths("a", Some(0), None, None, Some("default"), None)
        .await
        .expect("traverse");

    assert!(paths.is_empty(), "depth=0 must return empty");
}

#[tokio::test]
async fn libsql_traverse_paths_reverse_direction() {
    let _guard = libsql_test_guard();
    let (engine, _tmp) = init_clean_libsql().await;
    seed_page(&engine, "a").await;
    seed_page(&engine, "b").await;

    engine
        .add_links_batch(&[li("a", "b", Some("link"), Some("a->b"))])
        .await
        .expect("add link");

    let paths = engine
        .traverse_paths("b", Some(1), None, Some("in"), Some("default"), None)
        .await
        .expect("traverse");

    assert_eq!(paths.len(), 1);
    assert_eq!(paths[0].from_slug, "a");
    assert_eq!(paths[0].to_slug, "b");
}

#[tokio::test]
async fn libsql_traverse_paths_both_directions() {
    let _guard = libsql_test_guard();
    let (engine, _tmp) = init_clean_libsql().await;
    seed_page(&engine, "center").await;
    seed_page(&engine, "left").await;
    seed_page(&engine, "right").await;

    engine
        .add_links_batch(&[
            li("left", "center", Some("link"), Some("l->c")),
            li("center", "right", Some("link"), Some("c->r")),
        ])
        .await
        .expect("add links");

    let paths = engine
        .traverse_paths("center", Some(1), None, Some("both"), Some("default"), None)
        .await
        .expect("traverse");

    assert_eq!(paths.len(), 2);
}

#[tokio::test]
async fn libsql_traverse_paths_unknown_slug_returns_empty() {
    let _guard = libsql_test_guard();
    let (engine, _tmp) = init_clean_libsql().await;
    let paths = engine
        .traverse_paths("nonexistent", Some(1), None, None, Some("default"), None)
        .await
        .expect("traverse");
    assert!(paths.is_empty());
}

#[tokio::test]
async fn libsql_traverse_paths_link_type_filter() {
    let _guard = libsql_test_guard();
    let (engine, _tmp) = init_clean_libsql().await;
    seed_page(&engine, "a").await;
    seed_page(&engine, "b").await;
    seed_page(&engine, "c").await;

    engine
        .add_links_batch(&[
            li("a", "b", Some("link"), Some("wikilink")),
            li("a", "c", Some("ref"), Some("reference")),
        ])
        .await
        .expect("add links");

    let paths = engine
        .traverse_paths("a", Some(1), Some("link"), None, Some("default"), None)
        .await
        .expect("traverse");

    assert_eq!(paths.len(), 1);
    assert_eq!(paths[0].to_slug, "b");
    assert_eq!(paths[0].link_type, "link");
}

#[tokio::test]
async fn libsql_traverse_paths_respects_depth_limit() {
    let _guard = libsql_test_guard();
    let (engine, _tmp) = init_clean_libsql().await;
    seed_page(&engine, "d0").await;
    seed_page(&engine, "d1").await;
    seed_page(&engine, "d2").await;
    seed_page(&engine, "d3").await;

    engine
        .add_links_batch(&[
            li("d0", "d1", Some("link"), Some("0->1")),
            li("d1", "d2", Some("link"), Some("1->2")),
            li("d2", "d3", Some("link"), Some("2->3")),
        ])
        .await
        .expect("add links");

    // depth=1: only d0→d1
    let paths = engine
        .traverse_paths("d0", Some(1), None, None, Some("default"), None)
        .await
        .expect("traverse");
    assert_eq!(paths.len(), 1);
    assert_eq!(paths[0].to_slug, "d1");

    // depth=2: d0→d1 + d1→d2
    let paths = engine
        .traverse_paths("d0", Some(2), None, None, Some("default"), None)
        .await
        .expect("traverse");
    assert_eq!(paths.len(), 2);
    let tos: Vec<&str> = paths.iter().map(|p| p.to_slug.as_str()).collect();
    assert!(tos.contains(&"d1"));
    assert!(tos.contains(&"d2"));
}
