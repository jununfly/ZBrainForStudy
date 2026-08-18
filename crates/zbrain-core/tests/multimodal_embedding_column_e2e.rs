//! 1-4-5 — G70 integration: multi-column embedding selection end-to-end via
//! the libsql path, plus the `QueryParams::validate` column-name gate.
//!
//! Acceptance points covered (per docs/plans/MIGRATION.md G70):
//!   1. Default `embedding` column ranks by the text page-level vector.
//!   2. `embedding_multimodal` column ranks by the multimodal page-level
//!      vector — proves `validate_embedding_column` *accepts* the column and
//!      the search path *routes* to it (D12 layer 1 "放行").
//!   3. `search_pages` main-search wiring: setting `embedding_column` swaps
//!      the selected vector into `page.embedding` so the shared
//!      `fuse_and_boost` fusion re-ranks by the chosen space.
//!   4. `QueryParams::validate` rejects any column name other than the two
//!      accepted values (D12 layer 1 hard gate).
//!
//! Mirrors the libsql-only integration approach of
//! `cosine_re_score_e2e.rs` (one unit + one integration per v6 dual-verify).

use std::sync::{MutexGuard, OnceLock, Mutex};
use tempfile::NamedTempFile;
use zbrain_core::engine::{BrainEngine, CreateSourceInput, EngineConfig, PageInput, SearchOpts};
use zbrain_core::libsql::LibsqlEngine;
use zbrain_core::operation::{QueryParams, ValidateParams};

/// Serialize all libsql FFI access in this binary. The libsql native library
/// is not safe to drive from multiple OS threads concurrently on Windows
/// (parallel `cargo test` threads crash with 0xc0000005). Each test grabs this
/// guard for its whole body so the suite stays green under default parallelism.
static LIBSQL_TEST_LOCK: OnceLock<Mutex<()>> = OnceLock::new();
fn libsql_test_guard() -> MutexGuard<'static, ()> {
    LIBSQL_TEST_LOCK
        .get_or_init(|| Mutex::new(()))
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

async fn init_clean_engine() -> (LibsqlEngine, NamedTempFile) {
    let _g = libsql_test_guard();
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

fn f32_to_le_bytes(v: &[f32]) -> Vec<u8> {
    let mut out = Vec::with_capacity(v.len() * 4);
    for f in v {
        out.extend_from_slice(&f.to_le_bytes());
    }
    out
}

/// Seed one page in `src-a` with both a text and a multimodal page-level
/// vector. The text vector satisfies the `WHERE embedding IS NOT NULL`
/// candidate filter used by `search_pages_by_embedding`; the multimodal vector
/// is written separately via `put_page_multimodal_embedding`.
async fn seed_page(engine: &LibsqlEngine, slug: &str, text: &[f32], mm: &[f32]) {
    engine
        .put_page(
            slug,
            Some("src-a"),
            &PageInput {
                page_type: "note".to_string(),
                title: slug.to_string(),
                compiled_truth: "shared keyword anchor for both columns".to_string(),
                embedding: Some(f32_to_le_bytes(text)),
                source_kind: Some("src-a".into()),
                ..Default::default()
            },
        )
        .await
        .expect("put_page");
    engine
        .put_page_multimodal_embedding(slug, "src-a", f32_to_le_bytes(mm))
        .await
        .expect("put_page_multimodal_embedding");
}

async fn seed_source(engine: &LibsqlEngine) {
    engine
        .create_source(&CreateSourceInput {
            id: "src-a".into(),
            name: "src-a".into(),
            config: None,
        })
        .await
        .expect("create src-a");
}

#[tokio::test]
async fn embedding_multimodal_column_routes_to_multimodal_vector() {
    let (engine, _path) = init_clean_engine().await;
    seed_source(&engine).await;
    // p1: text=[0,0,1], mm=[1,0,0];  p2: text=[1,0,0], mm=[0,0,1]
    seed_page(&engine, "p1", &[0.0, 0.0, 1.0], &[1.0, 0.0, 0.0]).await;
    seed_page(&engine, "p2", &[1.0, 0.0, 0.0], &[0.0, 0.0, 1.0]).await;

    // Default (text) column, query = [1,0,0] -> only p2's text vector matches.
    let text_hits = engine
        .search_pages_by_embedding(&[1.0, 0.0, 0.0], 10, Some("src-a"), None)
        .await
        .expect("search default column");
    assert!(!text_hits.is_empty(), "expected hits on default column");
    assert_eq!(
        text_hits[0].slug, "p2",
        "default column must rank p2 (text match) first"
    );

    // Multimodal column, same query = [1,0,0] -> now p1's mm vector matches best.
    let mm_hits = engine
        .search_pages_by_embedding(
            &[1.0, 0.0, 0.0],
            10,
            Some("src-a"),
            Some("embedding_multimodal"),
        )
        .await
        .expect("search multimodal column");
    assert!(!mm_hits.is_empty(), "expected hits on multimodal column");
    assert_eq!(
        mm_hits[0].slug, "p1",
        "embedding_multimodal column must accept + route to p1 first"
    );
}

#[tokio::test]
async fn search_pages_wiring_respects_embedding_column_swap() {
    let (engine, _path) = init_clean_engine().await;
    seed_source(&engine).await;
    seed_page(&engine, "p1", &[0.0, 0.0, 1.0], &[1.0, 0.0, 0.0]).await;
    seed_page(&engine, "p2", &[1.0, 0.0, 0.0], &[0.0, 0.0, 1.0]).await;

    // Default column: fused cosine uses the text vectors. Query [1,0,0] scores
    // p2's text (1.0) and p1's text (0.0) -> p2 wins the head.
    let default_out = engine
        .search_pages(&SearchOpts {
            keywords: vec!["shared".into()],
            query_embedding: Some(vec![1.0, 0.0, 0.0]),
            limit: Some(10),
            embedding_column: None,
            ..Default::default()
        })
        .await
        .expect("search_pages default");
    assert_eq!(
        default_out[0].page.slug, "p2",
        "default column head should be p2 (text vector)"
    );

    // Multimodal column: the swap puts p1's mm [1,0,0] into `embedding`, so the
    // same query now scores p1 (1.0) and p2 (0.0) -> head flips to p1.
    let mm_out = engine
        .search_pages(&SearchOpts {
            keywords: vec!["shared".into()],
            query_embedding: Some(vec![1.0, 0.0, 0.0]),
            limit: Some(10),
            embedding_column: Some("embedding_multimodal".into()),
            ..Default::default()
        })
        .await
        .expect("search_pages multimodal");
    assert_eq!(
        mm_out[0].page.slug, "p1",
        "embedding_multimodal column must flip head to p1 (swap wired)"
    );
}

#[tokio::test]
async fn query_params_validate_rejects_illegal_embedding_column() {
    // Accepted values pass.
    for col in [None, Some("embedding"), Some("embedding_multimodal")] {
        let p = QueryParams {
            query: Some("x".into()),
            embedding_column: col.map(str::to_string),
            ..Default::default()
        };
        assert!(p.validate().is_ok(), "column {:?} should be accepted", col);
    }
    // Anything else is rejected (D12 layer 1 hard gate).
    for bad in [
        "embedding_multimoda",
        "embeddingx",
        "text",
        "1col",
        "drop table",
        "",
    ] {
        let p = QueryParams {
            query: Some("x".into()),
            embedding_column: Some(bad.into()),
            ..Default::default()
        };
        assert!(
            p.validate().is_err(),
            "column {:?} should be rejected",
            bad
        );
    }
}
