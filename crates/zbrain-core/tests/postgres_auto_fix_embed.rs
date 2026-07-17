//! `embed_stale` integration test over the production postgres engine
//! (via `pg-embed` fixture). Mirrors `postgres_features_foundation.rs`: uses
//! the `support` module's `PgFixture`. Fake embedding provider — no network.

mod support;

use std::sync::Arc;
use zbrain_core::auto_fix::{
    embed_stale, extract_links, extract_timeline, EmbedStaleOpts, ExtractLinksOpts,
    ExtractTimelineOpts,
};
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

/// `extract_links` scans page bodies and writes resolved outgoing links on
/// postgres.
#[tokio::test]
async fn postgres_extract_links_creates_resolved_link() {
    let fix = PgFixture::start().await;
    fix.engine
        .put_page("alice", Some("default"), &page("alice", "see [[bob]]"))
        .await
        .expect("put alice");
    fix.engine
        .put_page("bob", Some("default"), &page("bob", "i am bob"))
        .await
        .expect("put bob");

    let res = extract_links(&fix.engine, &ExtractLinksOpts::default())
        .await
        .expect("extract_links");
    assert_eq!(res.pages_processed, 2);
    assert_eq!(res.links_created, 1);
    assert_eq!(res.dangling, 0);
}

/// Dangling wikilinks are counted, not written (postgres).
#[tokio::test]
async fn postgres_extract_links_skips_dangling() {
    let fix = PgFixture::start().await;
    fix.engine
        .put_page("alice", Some("default"), &page("alice", "see [[ghost]]"))
        .await
        .expect("put alice");

    let res = extract_links(&fix.engine, &ExtractLinksOpts::default())
        .await
        .expect("extract_links");
    assert_eq!(res.links_created, 0);
    assert_eq!(res.dangling, 1);
}

/// `extract_timeline` appends parsed dated entries to `pages.timeline` on
/// postgres.
#[tokio::test]
async fn postgres_extract_timeline_appends_entries() {
    let fix = PgFixture::start().await;
    fix.engine
        .put_page(
            "p",
            Some("default"),
            &page(
                "p",
                "- **2024-01-01** | Source — First event\n- **2024-06-15** | Other — Second event",
            ),
        )
        .await
        .expect("put p");

    let res = extract_timeline(&fix.engine, &ExtractTimelineOpts::default())
        .await
        .expect("extract_timeline");
    assert_eq!(res.pages_processed, 1);
    assert_eq!(res.entries_added, 2);

    let timeline = fix
        .engine
        .get_page("p", &Default::default())
        .await
        .expect("get_page")
        .expect("page present")
        .timeline;
    assert!(timeline.contains("2024-01-01 First event"));
    assert!(timeline.contains("2024-06-15 Second event"));
}

/// `extract_timeline` is idempotent on postgres.
#[tokio::test]
async fn postgres_extract_timeline_idempotent() {
    let fix = PgFixture::start().await;
    fix.engine
        .put_page(
            "p",
            Some("default"),
            &page("p", "- **2024-01-01** | Source — First event"),
        )
        .await
        .expect("put p");

    let first = extract_timeline(&fix.engine, &ExtractTimelineOpts::default())
        .await
        .expect("first");
    assert_eq!(first.entries_added, 1);
    let second = extract_timeline(&fix.engine, &ExtractTimelineOpts::default())
        .await
        .expect("second");
    assert_eq!(second.entries_added, 0);
}
