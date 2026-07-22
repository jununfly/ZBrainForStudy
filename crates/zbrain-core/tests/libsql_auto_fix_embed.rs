//! `embed_stale` integration test over the production libsql engine.
//!
//! Composes the foundation primitives (`list_stale_pages` +
//! `put_page_embedding`) with a deterministic fake `EmbeddingProvider`,
//! proving the full page-level auto-fix path works on a real SQL backend
//! (not InMemory). No network — the fake provider returns constant vectors.

use serde_json::json;
use std::sync::Arc;
use tempfile::NamedTempFile;
use zbrain_core::auto_fix::{
    embed_stale, extract_links, extract_timeline, EmbedStaleOpts, ExtractLinksOpts,
    ExtractTimelineOpts,
};
use zbrain_core::embedding::{EmbeddingClient, EmbeddingConfig, EmbeddingError, EmbeddingProvider};
use zbrain_core::engine::{BrainEngine, EngineConfig, PageInput};
use zbrain_core::libsql::LibsqlEngine;
use zbrain_core::PageKind;

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

    async fn embed_image(
        &self,
        _base64_image: &str,
        _mime: Option<&str>,
        dims: usize,
    ) -> std::result::Result<Vec<f32>, EmbeddingError> {
        Ok(vec![1.5f32; dims])
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

fn page(title: &str, body: &str, embedding: Option<Vec<u8>>) -> PageInput {
    PageInput {
        page_type: "note".to_string(),
        title: title.to_string(),
        compiled_truth: body.to_string(),
        timeline: None,
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

/// Full path: a null-embedding page is enumerated, embedded via the fake
/// client, and its vector is written back so it is no longer stale.
#[tokio::test]
async fn embed_stale_backfills_page_on_libsql() {
    let path = temp_db();
    let engine = connected_engine(&path).await;
    engine
        .put_page(
            "stale",
            Some("default"),
            &page("Stale", "body to embed", None),
        )
        .await
        .expect("put stale");

    let res = embed_stale(&engine, &fake_client(4), &EmbedStaleOpts::default())
        .await
        .expect("embed_stale");
    assert_eq!(res.total, 1);
    assert_eq!(res.embedded, 1);

    let got = engine
        .get_page("stale", &Default::default())
        .await
        .expect("get_page")
        .expect("page present");
    // 4 dims * 4 bytes.
    assert_eq!(got.embedding.map(|b| b.len()), Some(16));

    let stale = engine
        .list_stale_pages()
        .await
        .expect("list_stale_pages");
    assert!(stale.is_empty(), "page no longer stale after embed_stale");
}

/// `embed_stale` with `dry_run` counts but writes nothing on a real backend.
#[tokio::test]
async fn embed_stale_dry_run_leaves_db_untouched() {
    let path = temp_db();
    let engine = connected_engine(&path).await;
    engine
        .put_page(
            "stale",
            Some("default"),
            &page("Stale", "body to embed", None),
        )
        .await
        .expect("put stale");

    let res = embed_stale(
        &engine,
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

    let got = engine
        .get_page("stale", &Default::default())
        .await
        .expect("get_page")
        .expect("page present");
    assert!(got.embedding.is_none());
}

/// `extract_links` scans page bodies for wikilinks, resolves them against
/// existing slugs, and writes outgoing links on a real backend.
#[tokio::test]
async fn extract_links_creates_resolved_link_on_libsql() {
    let path = temp_db();
    let engine = connected_engine(&path).await;
    engine
        .put_page("alice", Some("default"), &page("alice", "see [[bob]]", None))
        .await
        .expect("put alice");
    engine
        .put_page("bob", Some("default"), &page("bob", "i am bob", None))
        .await
        .expect("put bob");

    let res = extract_links(&engine, &ExtractLinksOpts::default())
        .await
        .expect("extract_links");
    assert_eq!(res.pages_processed, 2);
    assert_eq!(res.links_created, 1);
    assert_eq!(res.dangling, 0);
}

/// Dangling wikilinks (target slug absent) are counted, not written.
#[tokio::test]
async fn extract_links_skips_dangling_on_libsql() {
    let path = temp_db();
    let engine = connected_engine(&path).await;
    engine
        .put_page("alice", Some("default"), &page("alice", "see [[ghost]]", None))
        .await
        .expect("put alice");

    let res = extract_links(&engine, &ExtractLinksOpts::default())
        .await
        .expect("extract_links");
    assert_eq!(res.links_created, 0);
    assert_eq!(res.dangling, 1);
}

/// `extract_timeline` parses dated entries from a page body and appends them
/// to `pages.timeline` on a real backend.
#[tokio::test]
async fn extract_timeline_appends_entries_on_libsql() {
    let path = temp_db();
    let engine = connected_engine(&path).await;
    engine
        .put_page(
            "p",
            Some("default"),
            &page(
                "p",
                "- **2024-01-01** | Source — First event\n- **2024-06-15** | Other — Second event",
                None,
            ),
        )
        .await
        .expect("put p");

    let res = extract_timeline(&engine, &ExtractTimelineOpts::default())
        .await
        .expect("extract_timeline");
    assert_eq!(res.pages_processed, 1);
    assert_eq!(res.entries_added, 2);

    let timeline = engine
        .get_page("p", &Default::default())
        .await
        .expect("get_page")
        .expect("page present")
        .timeline;
    assert!(timeline.contains("2024-01-01 First event"));
    assert!(timeline.contains("2024-06-15 Second event"));
}

/// `extract_timeline` is idempotent: a re-run adds nothing new.
#[tokio::test]
async fn extract_timeline_idempotent_on_libsql() {
    let path = temp_db();
    let engine = connected_engine(&path).await;
    engine
        .put_page(
            "p",
            Some("default"),
            &page("p", "- **2024-01-01** | Source — First event", None),
        )
        .await
        .expect("put p");

    let first = extract_timeline(&engine, &ExtractTimelineOpts::default())
        .await
        .expect("first");
    assert_eq!(first.entries_added, 1);
    let second = extract_timeline(&engine, &ExtractTimelineOpts::default())
        .await
        .expect("second");
    assert_eq!(second.entries_added, 0);
}
