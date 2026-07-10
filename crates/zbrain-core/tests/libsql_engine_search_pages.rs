//! 1-3-2 — `LibsqlEngine::search_pages` integration tests.
//!
//! Before this slice `search_pages` was the `BrainEngine` trait *default*
//! (`engine.rs:816`), returning `Ok(Vec::new())`. That made production search
//! silently dead: the CLI `query` path constructs a `LibsqlEngine`, so every
//! query returned zero results even though pages were indexed. Only the
//! `InMemoryEngine` had a real implementation.
//!
//! This slice gives libsql a real `search_pages` that:
//!   1. Materializes the live (non-deleted), optionally source-scoped candidate
//!      pages via a 30-column SELECT (`full_row_to_page`), then
//!   2. Delegates to the shared backend-agnostic `fuse_and_boost` core
//!      (extracted in 1-3-1), so libsql and InMemory fuse/snippet/boost with a
//!      single scoring truth.
//!
//! Coverage:
//! - lexical hit in title / content (real results, not an empty Vec)
//! - no-match returns empty
//! - `source_id` scoping
//! - soft-deleted pages excluded
//! - `limit` respected
//! - vector path degrades gracefully: pages carry no embedding, so supplying a
//!   `query_embedding` must not crash and must still return the lexical hits.
//!
//! Test strategy mirrors `libsql_engine_list_pages.rs`: each test gets its own
//! `NamedTempFile`; rows inserted via `put_page`; `SCHEMA_INIT_LOCK` inside
//! `init_schema` makes parallel runs safe.

use tempfile::NamedTempFile;
use zbrain_core::engine::{BrainEngine, CreateSourceInput, EngineConfig, PageInput, SearchOpts};
use zbrain_core::libsql::LibsqlEngine;

/// Build a connected, schema-initialized engine on a fresh temp file.
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

/// Create a source record so pages with a custom source_id don't hit the
/// `pages.source_id → sources.id` FK constraint in libsql.
async fn seed_source(engine: &LibsqlEngine, id: &str) {
    engine
        .create_source(&CreateSourceInput {
            id: id.to_string(),
            name: id.to_string(),
            config: None,
        })
        .await
        .unwrap_or_else(|e| panic!("seed source {id}: {e:?}"));
}

fn note_input(title: &str, body: &str) -> PageInput {
    PageInput {
        page_type: "note".to_string(),
        title: title.to_string(),
        compiled_truth: body.to_string(),
        ..Default::default()
    }
}

fn keyword_opts(keywords: &[&str]) -> SearchOpts {
    SearchOpts {
        keywords: keywords.iter().map(|k| (*k).to_string()).collect(),
        ..Default::default()
    }
}

// ─── lexical hit in title ─────────────────────────────────────────────────

#[tokio::test]
async fn search_pages_finds_keyword_in_title() {
    let (engine, _tmp) = init_clean_engine().await;
    engine
        .put_page("rust-async", None, &note_input("Rust async runtimes", "body"))
        .await
        .expect("put_page");
    engine
        .put_page("py-gil", None, &note_input("Python GIL", "unrelated body"))
        .await
        .expect("put_page");

    let results = engine
        .search_pages(&keyword_opts(&["rust"]))
        .await
        .expect("search_pages");

    // Real result, NOT the trait-default empty Vec.
    assert_eq!(results.len(), 1, "exactly one title matches 'rust'");
    assert_eq!(results[0].page.slug, "rust-async");
    assert!(results[0].score > 0.0, "matched page must have a positive fused score");
}

// ─── lexical hit in content ───────────────────────────────────────────────

#[tokio::test]
async fn search_pages_finds_keyword_in_content() {
    let (engine, _tmp) = init_clean_engine().await;
    engine
        .put_page(
            "topic-a",
            None,
            &note_input("Some title", "the tokio scheduler steals work between threads"),
        )
        .await
        .expect("put_page");

    let results = engine
        .search_pages(&keyword_opts(&["scheduler"]))
        .await
        .expect("search_pages");

    assert_eq!(results.len(), 1);
    assert_eq!(results[0].page.slug, "topic-a");
    // Snippet is extracted from compiled_truth around the keyword hit.
    let snippet = results[0].snippet.as_deref().unwrap_or("");
    assert!(
        snippet.contains("scheduler"),
        "snippet should anchor on the keyword hit, got: {snippet:?}"
    );
}

// ─── no match ─────────────────────────────────────────────────────────────

#[tokio::test]
async fn search_pages_returns_empty_for_no_match() {
    let (engine, _tmp) = init_clean_engine().await;
    engine
        .put_page("only-note", None, &note_input("Alpha", "beta gamma"))
        .await
        .expect("put_page");

    let results = engine
        .search_pages(&keyword_opts(&["nonexistent"]))
        .await
        .expect("search_pages");

    assert!(results.is_empty());
}

// ─── source scoping ───────────────────────────────────────────────────────

#[tokio::test]
async fn search_pages_filters_by_source() {
    let (engine, _tmp) = init_clean_engine().await;
    seed_source(&engine, "alpha").await;
    seed_source(&engine, "beta").await;
    engine
        .put_page("doc-1", Some("alpha"), &note_input("shared keyword here", "x"))
        .await
        .expect("put_page alpha");
    engine
        .put_page("doc-2", Some("beta"), &note_input("shared keyword here", "y"))
        .await
        .expect("put_page beta");

    let opts = SearchOpts {
        keywords: vec!["shared".to_string()],
        source_id: Some("alpha".to_string()),
        ..Default::default()
    };
    let results = engine.search_pages(&opts).await.expect("search_pages");

    assert_eq!(results.len(), 1, "only the alpha-source page is in scope");
    assert_eq!(results[0].page.source_id, "alpha");
}

// ─── soft-deleted excluded ────────────────────────────────────────────────

#[tokio::test]
async fn search_pages_excludes_soft_deleted() {
    let (engine, _tmp) = init_clean_engine().await;
    engine
        .put_page("live", None, &note_input("keeper keyword", "body"))
        .await
        .expect("put_page live");
    engine
        .put_page("gone", None, &note_input("keeper keyword", "body"))
        .await
        .expect("put_page gone");
    engine.delete_page("gone", None).await.expect("delete_page");

    let results = engine
        .search_pages(&keyword_opts(&["keeper"]))
        .await
        .expect("search_pages");

    assert_eq!(results.len(), 1, "soft-deleted page must be excluded");
    assert_eq!(results[0].page.slug, "live");
}

// ─── limit respected ──────────────────────────────────────────────────────

#[tokio::test]
async fn search_pages_respects_limit() {
    let (engine, _tmp) = init_clean_engine().await;
    for i in 0..5 {
        engine
            .put_page(
                &format!("hit-{i}"),
                None,
                &note_input("common keyword", &format!("body {i}")),
            )
            .await
            .expect("put_page");
    }

    let opts = SearchOpts {
        keywords: vec!["common".to_string()],
        limit: Some(3),
        ..Default::default()
    };
    let results = engine.search_pages(&opts).await.expect("search_pages");

    assert_eq!(results.len(), 3, "limit truncates the result set");
}

// ─── vector path degrades gracefully ──────────────────────────────────────

#[tokio::test]
async fn search_pages_vector_path_degrades_without_embeddings() {
    let (engine, _tmp) = init_clean_engine().await;
    engine
        .put_page("vec-note", None, &note_input("vector keyword note", "body"))
        .await
        .expect("put_page");

    // Pages have no stored embedding (put_page doesn't write one yet). Supplying
    // a query embedding must NOT crash — the vector path finds no candidate with
    // a decodable embedding and fusion degenerates to lexical-only.
    let opts = SearchOpts {
        keywords: vec!["vector".to_string()],
        query_embedding: Some(vec![0.1_f32; 8]),
        ..Default::default()
    };
    let results = engine.search_pages(&opts).await.expect("search_pages");

    assert_eq!(results.len(), 1, "lexical hit still returned despite empty vector path");
    assert_eq!(results[0].page.slug, "vec-note");
}
