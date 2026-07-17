//! `list_stale_pages` / `put_page_embedding` / `add_timeline_entry` integration
//! tests over the production libsql engine.
//!
//! Proves the three foundation primitives end-to-end on a real SQL backend
//! (not InMemory): enumerating null-embedding pages, writing a page vector
//! back without clobbering other columns, and appending a timeline entry to
//! the `pages.timeline` TEXT column surgically.
//!
//! Harness mirrors `libsql_integrity.rs` / `libsql_whoknows.rs`: each test
//! allocates its own `NamedTempFile` DB (torn down on drop), so the suite runs
//! unconditionally in CI with no daemon.

use serde_json::json;
use tempfile::NamedTempFile;
use zbrain_core::engine::{BrainEngine, EngineConfig, PageInput};
use zbrain_core::libsql::LibsqlEngine;
use zbrain_core::PageKind;

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

fn page(
    page_type: &str,
    title: &str,
    body: &str,
    embedding: Option<Vec<u8>>,
) -> PageInput {
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

/// A brain with one embedded page and one null-embedding page surfaces
/// exactly the null one from `list_stale_pages` (and NOT the embedded one).
#[tokio::test]
async fn list_stale_pages_returns_only_null_embedding() {
    let path = temp_db();
    let engine = connected_engine(&path).await;
    engine
        .put_page(
            "done",
            Some("default"),
            &page("note", "Done", "already embedded", Some(vec![1u8, 2, 3])),
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

    let stale = engine
        .list_stale_pages()
        .await
        .expect("list_stale_pages");
    assert_eq!(stale.len(), 1, "exactly one stale page");
    assert_eq!(stale[0].slug, "stale");
}

/// `list_stale_pages` excludes soft-deleted pages even if their embedding is
/// null (deleted rows are not re-embed candidates).
#[tokio::test]
async fn list_stale_pages_skips_deleted() {
    let path = temp_db();
    let engine = connected_engine(&path).await;
    engine
        .put_page(
            "ghost",
            Some("default"),
            &page("note", "Ghost", "deleted and null", None),
        )
        .await
        .expect("put ghost");
    engine
        .delete_page("ghost", Some("default"))
        .await
        .expect("delete ghost");

    let stale = engine
        .list_stale_pages()
        .await
        .expect("list_stale_pages");
    assert!(stale.is_empty(), "deleted null-embedding page must be skipped");
}

/// `put_page_embedding` writes the vector back without clobbering other
/// columns (title / body survive the surgical UPDATE).
#[tokio::test]
async fn put_page_embedding_backfills_without_clobber() {
    let path = temp_db();
    let engine = connected_engine(&path).await;
    engine
        .put_page(
            "stale",
            Some("default"),
            &page("note", "Stale Title", "original body", None),
        )
        .await
        .expect("put stale");

    let vec: Vec<u8> = vec![1u8, 2, 3, 4];
    engine
        .put_page_embedding("stale", "default", vec.clone())
        .await
        .expect("put_page_embedding");

    let got = engine
        .get_page("stale", &Default::default())
        .await
        .expect("get_page")
        .expect("page present");
    assert_eq!(got.embedding, Some(vec), "vector written back");
    assert_eq!(got.title, "Stale Title", "title preserved");
    assert_eq!(got.compiled_truth, "original body", "body preserved");

    let stale = engine
        .list_stale_pages()
        .await
        .expect("list_stale_pages");
    assert!(stale.is_empty(), "page no longer stale after backfill");
}

/// `add_timeline_entry` appends a single line to `pages.timeline` (TEXT),
/// preserving any prior content.
#[tokio::test]
async fn add_timeline_entry_appends_line() {
    let path = temp_db();
    let engine = connected_engine(&path).await;
    engine
        .put_page(
            "p",
            Some("default"),
            &page("note", "P", "body", None),
        )
        .await
        .expect("put p");

    engine
        .add_timeline_entry("p", "default", "2024-01-01 first event")
        .await
        .expect("add first");
    engine
        .add_timeline_entry("p", "default", "2024-06-01 second event")
        .await
        .expect("add second");

    let got = engine
        .get_page("p", &Default::default())
        .await
        .expect("get_page")
        .expect("page present");
    assert_eq!(
        got.timeline,
        "2024-01-01 first event\n2024-06-01 second event"
    );
}
