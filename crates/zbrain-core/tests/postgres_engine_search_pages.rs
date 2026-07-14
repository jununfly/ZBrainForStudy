//! G23 — `PostgresEngine::search_pages` integration tests.
//!
//! Before this slice `search_pages` fell through to the `BrainEngine` trait
//! *default* (returning `Ok(Vec::new())`), so a Postgres-backed deployment's
//! `zbrain query` silently returned zero results — production search was dead
//! on PG even though pages were indexed. Only libsql + InMemory had real
//! implementations.
//!
//! This slice gives Postgres a real `search_pages` mirroring the libsql slice
//! (1-3-2):
//!   1. Materialize the live (non-deleted), optionally source-scoped candidate
//!      pages via `FULL_PAGE_PROJECTION` (which now includes `embedding` after
//!      G24), then
//!   2. Delegate to the shared backend-agnostic `fuse_and_boost` core so PG,
//!      libsql, and InMemory fuse/snippet/boost with a single scoring truth.
//!
//! Coverage:
//! - lexical hit in title / content (real results, not an empty Vec)
//! - no-match returns empty
//! - `source_id` scoping
//! - soft-deleted pages excluded
//! - vector path active with a stored embedding (G24 write path proof)
//! - vector path degrades gracefully when pages carry no embedding
//!
//! Each test launches its own ephemeral `PostgreSQL` via `PgFixture`.

mod support;

use zbrain_core::engine::{BrainEngine, PageInput, SearchOpts};

fn note_input(title: &str, body: &str) -> PageInput {
    PageInput {
        page_type: "note".to_string(),
        title: title.to_string(),
        compiled_truth: body.to_string(),
        ..Default::default()
    }
}

/// Seed a source row via raw SQL (avoids `engine.create_source`, whose
/// RETURNING decode is exercised by its own tests). Mirrors the helper in
/// `postgres_engine_page_crud.rs`.
async fn seed_source(url: &str, id: &str) {
    let pool = sqlx::postgres::PgPoolOptions::new()
        .max_connections(1)
        .connect(url)
        .await
        .expect("source seed pool");
    sqlx::query("INSERT INTO sources (id, name) VALUES ($1, $2) ON CONFLICT (id) DO NOTHING")
        .bind(id)
        .bind(id)
        .execute(&pool)
        .await
        .expect("seed source");
    pool.close().await;
}

/// Encode an f32 slice as the little-endian byte blob the engine stores.
fn embed_le(v: &[f32]) -> Vec<u8> {
    v.iter().flat_map(|f| f.to_le_bytes()).collect()
}

#[tokio::test]
async fn search_pages_finds_keyword_in_title() {
    let fix = support::pg_fixture::PgFixture::start().await;
    let engine = &fix.engine;

    engine
        .put_page("alpha", None, &note_input("uniquekeyword title", "body"))
        .await
        .expect("put_page");
    engine
        .put_page("beta", None, &note_input("other", "unrelated body"))
        .await
        .expect("put_page");

    let opts = SearchOpts {
        keywords: vec!["uniquekeyword".to_string()],
        ..Default::default()
    };
    let results = engine.search_pages(&opts).await.expect("search_pages");

    assert!(
        !results.is_empty(),
        "keyword must produce a real (non-empty) result — G23"
    );
    assert_eq!(results[0].page.slug, "alpha");
}

#[tokio::test]
async fn search_pages_returns_empty_for_no_match() {
    let fix = support::pg_fixture::PgFixture::start().await;
    let engine = &fix.engine;

    engine
        .put_page("alpha", None, &note_input("some title", "some body"))
        .await
        .expect("put_page");

    let opts = SearchOpts {
        keywords: vec!["zzznomatchzzz".to_string()],
        ..Default::default()
    };
    let results = engine.search_pages(&opts).await.expect("search_pages");
    assert!(
        results.is_empty(),
        "no lexical/vector match must return empty"
    );
}

#[tokio::test]
async fn search_pages_filters_by_source() {
    let fix = support::pg_fixture::PgFixture::start().await;
    let engine = &fix.engine;

    seed_source(&fix.url, "src-a").await;

    engine
        .put_page("in-default", None, &note_input("sharedkeyword one", "b"))
        .await
        .expect("put_page default");
    engine
        .put_page(
            "in-src-a",
            Some("src-a"),
            &note_input("sharedkeyword two", "b"),
        )
        .await
        .expect("put_page src-a");

    let opts = SearchOpts {
        keywords: vec!["sharedkeyword".to_string()],
        source_id: Some("src-a".to_string()),
        ..Default::default()
    };
    let results = engine.search_pages(&opts).await.expect("search_pages");
    assert_eq!(results.len(), 1, "source scoping must limit candidates");
    assert_eq!(results[0].page.slug, "in-src-a");
}

#[tokio::test]
async fn search_pages_excludes_soft_deleted() {
    let fix = support::pg_fixture::PgFixture::start().await;
    let engine = &fix.engine;

    engine
        .put_page("live", None, &note_input("delkeyword live", "b"))
        .await
        .expect("put_page live");
    engine
        .put_page("gone", None, &note_input("delkeyword gone", "b"))
        .await
        .expect("put_page gone");
    engine.delete_page("gone", None).await.expect("soft delete");

    let opts = SearchOpts {
        keywords: vec!["delkeyword".to_string()],
        ..Default::default()
    };
    let results = engine.search_pages(&opts).await.expect("search_pages");
    assert_eq!(results.len(), 1, "soft-deleted pages must be excluded");
    assert_eq!(results[0].page.slug, "live");
}

#[tokio::test]
async fn search_pages_vector_path_active_with_stored_embedding() {
    // G23 + G24 combined proof: a PG page whose embedding was persisted via
    // put_page is retrievable through the vector half of fuse_and_boost, with
    // no lexical overlap.
    let fix = support::pg_fixture::PgFixture::start().await;
    let engine = &fix.engine;

    let aligned = vec![1.0_f32, 0.0, 0.0, 0.0];
    let orthogonal = vec![0.0_f32, 1.0, 0.0, 0.0];

    let mut hit = note_input("alpha doc", "alpha body");
    hit.embedding = Some(embed_le(&aligned));
    engine
        .put_page("vec-hit", None, &hit)
        .await
        .expect("put_page hit");

    let mut miss = note_input("beta doc", "beta body");
    miss.embedding = Some(embed_le(&orthogonal));
    engine
        .put_page("vec-miss", None, &miss)
        .await
        .expect("put_page miss");

    let opts = SearchOpts {
        keywords: vec!["nonexistentkeyword".to_string()],
        query_embedding: Some(aligned.clone()),
        min_score: Some(0.0),
        ..Default::default()
    };
    let results = engine.search_pages(&opts).await.expect("search_pages");

    assert!(
        !results.is_empty(),
        "stored embedding must make the vector path produce results (G23/G24)"
    );
    assert_eq!(
        results[0].page.slug, "vec-hit",
        "the page whose stored vector aligns with the query must rank first"
    );
}

#[tokio::test]
async fn search_pages_vector_path_degrades_without_embeddings() {
    let fix = support::pg_fixture::PgFixture::start().await;
    let engine = &fix.engine;

    engine
        .put_page("vec-note", None, &note_input("vector keyword note", "body"))
        .await
        .expect("put_page");

    // No stored embedding; supplying a query embedding must not crash and must
    // still return the lexical hit.
    let opts = SearchOpts {
        keywords: vec!["vector".to_string()],
        query_embedding: Some(vec![0.1_f32; 8]),
        ..Default::default()
    };
    let results = engine.search_pages(&opts).await.expect("search_pages");
    assert_eq!(
        results.len(),
        1,
        "lexical hit still returned despite empty vector path"
    );
    assert_eq!(results[0].page.slug, "vec-note");
}
