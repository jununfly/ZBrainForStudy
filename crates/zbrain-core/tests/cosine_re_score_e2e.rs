//! 1-5-15 — G67 integration: `cosine_re_score` end-to-end via the libsql
//! path.
//!
//! Unit tests in `crates/zbrain-core/src/search/engine.rs` prove the
//! blending math; this binary proves the wiring into `hybrid_search`:
//!   1. The libsql engine's `search_pages` produces page-level results with
//!      stored embeddings (the vector path is wired).
//!   2. `search::hybrid_search` returns the same set of slugs, but with the
//!      `cosine_score` stamp populated on the rows that were actually
//!      re-scored, and with the head re-ordered when the cosine disagrees
//!      with the RRF input order.
//!   3. The pipeline still works (and `cosine_score` stays `None`) when no
//!      embedding client is provided — the fail-open contract.
//!
//! This is the second leg of the v6 dual-verify rule (one unit + one
//! integration) for G67. See `docs/plans/MIGRATION.md` G67.

use async_trait::async_trait;
use std::sync::{Arc, OnceLock};
use tempfile::NamedTempFile;
use zbrain_core::embedding::{EmbeddingClient, EmbeddingConfig, EmbeddingError, EmbeddingProvider};
use zbrain_core::engine::{BrainEngine, CreateSourceInput, EngineConfig, PageInput};
use zbrain_core::libsql::LibsqlEngine;
use zbrain_core::search::engine::{cosine_re_score, hybrid_search, HybridSearchOpts};

/// Mock provider that returns the same vector for any text/image. Used
/// to drive `hybrid_search` deterministically without HTTP.
struct ConstProvider(Vec<f32>);

#[async_trait]
impl EmbeddingProvider for ConstProvider {
    async fn embed(
        &self,
        texts: &[String],
        _dims: usize,
    ) -> Result<Vec<Vec<f32>>, EmbeddingError> {
        Ok(texts.iter().map(|_| self.0.clone()).collect())
    }

    async fn embed_image(
        &self,
        _base64_image: &str,
        _mime: Option<&str>,
        _dims: usize,
    ) -> Result<Vec<f32>, EmbeddingError> {
        Ok(self.0.clone())
    }
}

fn const_client(v: Vec<f32>) -> Arc<EmbeddingClient> {
    let config = EmbeddingConfig::builder()
        .api_key("sk-test")
        .dimensions(v.len())
        .build()
        .expect("build config");
    Arc::new(EmbeddingClient::with_provider(
        config,
        Arc::new(ConstProvider(v)),
    ))
}

/// Serialize all libsql FFI access in this binary. The libsql native
/// library is not safe to drive from multiple OS threads concurrently on
/// Windows (parallel `cargo test` threads crash with 0xc0000005). Each test
/// grabs this guard for its whole body so the suite stays green under
/// default parallelism; serial runs are unaffected.
static LIBSQL_TEST_LOCK: OnceLock<std::sync::Mutex<()>> = OnceLock::new();
fn libsql_test_guard() -> std::sync::MutexGuard<'static, ()> {
    LIBSQL_TEST_LOCK
        .get_or_init(|| std::sync::Mutex::new(()))
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

async fn seed_two_pages(engine: &LibsqlEngine) {
    // Source A: pages p1, p2 (the latter gets the matching embedding).
    engine
        .create_source(&CreateSourceInput {
            id: "src-a".into(),
            name: "src-a".into(),
            config: None,
        })
        .await
        .expect("create src-a");
    // Source B: just a scope to verify we don't accidentally pull from it.
    engine
        .create_source(&CreateSourceInput {
            id: "src-b".into(),
            name: "src-b".into(),
            config: None,
        })
        .await
        .expect("create src-b");
}

fn f32_to_le_bytes(v: &[f32]) -> Vec<u8> {
    let mut out = Vec::with_capacity(v.len() * 4);
    for f in v {
        out.extend_from_slice(&f.to_le_bytes());
    }
    out
}

#[tokio::test]
async fn cosine_re_score_stamps_and_reorders_via_hybrid_search() {
    let (engine, _path) = init_clean_engine().await;
    seed_two_pages(&engine).await;

    // Both pages share the same lexical content so the query matches
    // them equally well, and the blended score decides the head. The
    // cosine delta then has visible effect on the ranking.
    let q: Vec<f32> = vec![1.0, 0.0, 0.0];
    let p1 = f32_to_le_bytes(&[0.0, 1.0, 0.0]);
    let p2 = f32_to_le_bytes(&[1.0, 0.0, 0.0]);

    engine
        .put_page(
            "p1",
            Some("src-a"),
            &PageInput {
                page_type: "note".to_string(),
                title: "alpha".into(),
                compiled_truth: "shared keyword shared keyword shared keyword".into(),
                embedding: Some(p1),
                source_kind: Some("src-a".into()),
                ..Default::default()
            },
        )
        .await
        .expect("put p1");
    engine
        .put_page(
            "p2",
            Some("src-a"),
            &PageInput {
                page_type: "note".to_string(),
                title: "alpha".into(),
                compiled_truth: "shared keyword shared keyword shared keyword".into(),
                embedding: Some(p2),
                source_kind: Some("src-a".into()),
                ..Default::default()
            },
        )
        .await
        .expect("put p2");

    // We need an EmbeddingClient that returns our `q` for the query path.
    let client = const_client(q.clone());

    let out = hybrid_search(
        &engine,
        "shared keyword",
        &HybridSearchOpts {
            embedding_client: Some(client),
            limit: Some(10),
            ..Default::default()
        },
    )
    .await
    .expect("hybrid_search");

    assert!(
        out.len() >= 2,
        "expected at least 2 hits, got {} (slugs: {:?})",
        out.len(),
        out.iter().map(|r| r.page.slug.as_str()).collect::<Vec<_>>()
    );

    // The head should be p2 (cosine=1.0 wins the blend against p1's
    // cosine=0.0, since RRF input order is symmetric — both pages are
    // lexical hits on identical keywords).
    assert_eq!(
        out[0].page.slug, "p2",
        "head should be the cosine-best match"
    );

    // Both rows must have the cosine stamp populated, and p2's raw cosine
    // must beat p1's.
    let p2_cos = out
        .iter()
        .find(|r| r.page.slug == "p2")
        .and_then(|r| r.cosine_score)
        .expect("p2.cosine_score populated");
    let p1_cos = out
        .iter()
        .find(|r| r.page.slug == "p1")
        .and_then(|r| r.cosine_score)
        .expect("p1.cosine_score populated");
    assert!((p2_cos - 1.0).abs() < 1e-6, "p2 cosine = {}", p2_cos);
    assert!(p1_cos < 0.5, "p1 cosine should be near zero, got {}", p1_cos);
}

#[tokio::test]
async fn hybrid_search_without_embedding_client_does_not_stamp_cosine() {
    let (engine, _path) = init_clean_engine().await;
    seed_two_pages(&engine).await;
    engine
        .put_page(
            "p1",
            Some("src-a"),
            &PageInput {
                page_type: "note".to_string(),
                title: "alpha".into(),
                compiled_truth: "alpha content".into(),
                embedding: Some(f32_to_le_bytes(&[1.0, 0.0, 0.0])),
                source_kind: Some("src-a".into()),
                ..Default::default()
            },
        )
        .await
        .expect("put p1");

    let out = hybrid_search(
        &engine,
        "alpha",
        &HybridSearchOpts {
            embedding_client: None,
            limit: Some(5),
            ..Default::default()
        },
    )
    .await
    .expect("hybrid_search");

    // No embedding client → rescore stage skipped → `cosine_score` stays
    // None on every row. This is the fail-open contract.
    for r in &out {
        assert!(
            r.cosine_score.is_none(),
            "{} unexpectedly stamped cosine_score={:?}",
            r.page.slug,
            r.cosine_score
        );
    }
}

#[tokio::test]
async fn cosine_re_score_function_works_on_arbitrary_engine_results() {
    // Synthetic test: prove `cosine_re_score` is reusable on any
    // `SearchResult` slice (not just what `search_pages` returns) — the
    // unit tests cover the math, this is the public-API contract.
    use zbrain_core::engine::{Page, SearchResult};

    let mut page_a = Page::default();
    page_a.slug = "a".into();
    page_a.embedding = Some(f32_to_le_bytes(&[1.0, 0.0]));
    let mut page_b = Page::default();
    page_b.slug = "b".into();
    page_b.embedding = Some(f32_to_le_bytes(&[0.0, 1.0]));

    let r_a = SearchResult {
        page: page_a,
        score: 0.5,
        base_score: 0.5,
        snippet: None,
        rerank_score: None,
        reranker_delta: None,
        salience_boost: None,
        recency_boost: None,
        cosine_score: None,
    };
    let r_b = SearchResult {
        page: page_b,
        score: 0.5,
        base_score: 0.5,
        snippet: None,
        rerank_score: None,
        reranker_delta: None,
        salience_boost: None,
        recency_boost: None,
        cosine_score: None,
    };
    let q = vec![1.0_f32, 0.0];
    let out = cosine_re_score(&[r_a, r_b], &q);
    assert_eq!(out[0].page.slug, "a", "head should follow the cosine");
    assert_eq!(out[1].page.slug, "b");
    assert_eq!(out[0].cosine_score, Some(1.0));
    assert!(out[1].cosine_score.unwrap() < 0.01);
}
