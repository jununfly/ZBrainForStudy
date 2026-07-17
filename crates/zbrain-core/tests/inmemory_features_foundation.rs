//! `list_stale_pages` / `put_page_embedding` / `add_timeline_entry` tests over
//! the `InMemoryEngine` (the default engine used across crate unit tests).
//!
//! The SQL backends get their own integration files; this one proves the
//! in-process engine honours the same contract.

use serde_json::json;
use zbrain_core::engine::{BrainEngine, PageInput};
use zbrain_core::InMemoryEngine;
use zbrain_core::PageKind;

fn page(page_type: &str, title: &str, body: &str, embedding: Option<Vec<u8>>) -> PageInput {
    PageInput {
        page_type: page_type.to_string(),
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

#[tokio::test]
async fn inmemory_list_stale_pages_returns_only_null_embedding() {
    let engine = InMemoryEngine::new();
    engine
        .put_page(
            "done",
            Some("default"),
            &page("note", "Done", "embedded", Some(vec![9u8, 9])),
        )
        .await
        .expect("put done");
    engine
        .put_page(
            "stale",
            Some("default"),
            &page("note", "Stale", "needs embedding", None),
        )
        .await
        .expect("put stale");

    let stale = engine.list_stale_pages().await.expect("list");
    assert_eq!(stale.len(), 1);
    assert_eq!(stale[0].slug, "stale");
}

#[tokio::test]
async fn inmemory_put_page_embedding_backfills_vector() {
    let engine = InMemoryEngine::new();
    engine
        .put_page(
            "stale",
            Some("default"),
            &page("note", "Stale", "needs embedding", None),
        )
        .await
        .expect("put stale");

    let vec: Vec<u8> = vec![1u8, 2, 3, 4];
    engine
        .put_page_embedding("stale", "default", vec.clone())
        .await
        .expect("put embedding");

    let got = engine
        .get_page("stale", &Default::default())
        .await
        .expect("get")
        .expect("page present");
    assert_eq!(got.embedding, Some(vec));

    // And it now drops out of the stale set.
    let stale = engine.list_stale_pages().await.expect("list");
    assert!(stale.is_empty(), "page should no longer be stale");
}
