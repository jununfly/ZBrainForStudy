//! `embed_stale` integration test over the production postgres engine
//! (via `pg-embed` fixture). Mirrors `postgres_features_foundation.rs`: uses
//! the `support` module's `PgFixture`. Fake embedding provider — no network.

mod support;

use std::sync::Arc;
use zbrain_core::auto_fix::{embed_stale, EmbedStaleOpts};
use zbrain_core::embedding::{EmbeddingClient, EmbeddingConfig, EmbeddingError, EmbeddingProvider};
use zbrain_core::engine::{BrainEngine, PageInput};
use zbrain_core::PageKind;

use support::pg_fixture::PgFixture;

struct ConstProvider;

#[async_trait::async_trait]
impl EmbeddingProvider for ConstProvider {
    async fn embed(
        &self,
        texts: &[String],
        dims: usize,
    ) -> std::result::Result<Vec<Vec<f32>>, EmbeddingError> {
        Ok(texts.iter().map(|_| vec![1.5f32; dims]).collect())
    }
}

fn fake_client(dims: usize) -> EmbeddingClient {
    EmbeddingClient::with_provider(
        EmbeddingConfig {
            dimensions: dims,
            ..EmbeddingConfig::default()
        },
        Arc::new(ConstProvider),
    )
}

fn page(title: &str, body: &str) -> PageInput {
    PageInput {
        page_type: "note".to_string(),
        title: title.to_string(),
        compiled_truth: body.to_string(),
        timeline: None,
        frontmatter: Some(serde_json::json!({})),
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
        embedding: None,
    }
}

/// Full path on postgres: a null-embedding page is embedded and its vector
/// written back so it is no longer stale.
#[tokio::test]
async fn postgres_embed_stale_backfills_page() {
    let fix = PgFixture::start().await;
    fix.engine
        .put_page("stale", Some("default"), &page("Stale", "body to embed"))
        .await
        .expect("put stale");

    let res = embed_stale(&fix.engine, &fake_client(4), &EmbedStaleOpts::default())
        .await
        .expect("embed_stale");
    assert_eq!(res.total, 1);
    assert_eq!(res.embedded, 1);

    let got = fix
        .engine
        .get_page("stale", &Default::default())
        .await
        .expect("get_page")
        .expect("page present");
    assert_eq!(got.embedding.map(|b| b.len()), Some(16));

    let stale = fix.engine.list_stale_pages().await.expect("list");
    assert!(stale.is_empty(), "page no longer stale after embed_stale");
}

/// `embed_stale` with `dry_run` counts but writes nothing on postgres.
#[tokio::test]
async fn postgres_embed_stale_dry_run_leaves_db_untouched() {
    let fix = PgFixture::start().await;
    fix.engine
        .put_page("stale", Some("default"), &page("Stale", "body to embed"))
        .await
        .expect("put stale");

    let res = embed_stale(
        &fix.engine,
        &fake_client(4),
        &EmbedStaleOpts {
            dry_run: true,
            source_id: None,
        },
    )
    .await
    .expect("embed_stale");
    assert_eq!(res.would_embed, 1);
    assert_eq!(res.embedded, 0);

    let got = fix
        .engine
        .get_page("stale", &Default::default())
        .await
        .expect("get_page")
        .expect("page present");
    assert!(got.embedding.is_none());
}
