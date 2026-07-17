//! `list_stale_pages` / `put_page_embedding` / `add_timeline_entry` integration
//! tests over the production postgres engine (via `pg-embed` fixture).
//!
//! Mirrors `postgres_integrity.rs`: uses the `support` module's `PgFixture`,
//! which launches an ephemeral PG, connects a `PostgresEngine`, and runs
//! `init_schema()` for us.

mod support;

use serde_json::json;
use zbrain_core::engine::{BrainEngine, PageInput};
use zbrain_core::PageKind;

use support::pg_fixture::PgFixture;

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

/// Postgres: only the null-embedding live page is returned.
#[tokio::test]
async fn postgres_list_stale_pages_returns_only_null_embedding() {
    let fix = PgFixture::start().await;
    fix.engine
        .put_page(
            "done",
            Some("default"),
            &page("note", "Done", "embedded", Some(vec![1u8, 2, 3])),
        )
        .await
        .expect("put done");
    fix.engine
        .put_page(
            "stale",
            Some("default"),
            &page("note", "Stale", "needs embedding", None),
        )
        .await
        .expect("put stale");

    let stale = fix.engine.list_stale_pages().await.expect("list");
    assert_eq!(stale.len(), 1);
    assert_eq!(stale[0].slug, "stale");
}

/// Postgres: `put_page_embedding` backfills the vector without clobbering
/// other columns.
#[tokio::test]
async fn postgres_put_page_embedding_backfills_without_clobber() {
    let fix = PgFixture::start().await;
    fix.engine
        .put_page(
            "stale",
            Some("default"),
            &page("note", "Stale Title", "original body", None),
        )
        .await
        .expect("put stale");

    let vec: Vec<u8> = vec![1u8, 2, 3, 4];
    fix.engine
        .put_page_embedding("stale", "default", vec.clone())
        .await
        .expect("put_page_embedding");

    let got = fix
        .engine
        .get_page("stale", &Default::default())
        .await
        .expect("get_page")
        .expect("page present");
    assert_eq!(got.embedding, Some(vec), "vector written back");
    assert_eq!(got.title, "Stale Title", "title preserved");
    assert_eq!(got.compiled_truth, "original body", "body preserved");

    let stale = fix.engine.list_stale_pages().await.expect("list");
    assert!(stale.is_empty(), "page no longer stale after backfill");
}

/// Postgres: `add_timeline_entry` appends a line to `pages.timeline` (TEXT).
#[tokio::test]
async fn postgres_add_timeline_entry_appends_line() {
    let fix = PgFixture::start().await;
    fix.engine
        .put_page("p", Some("default"), &page("note", "P", "body", None))
        .await
        .expect("put p");

    fix.engine
        .add_timeline_entry("p", "default", "2024-01-01 first event")
        .await
        .expect("add first");
    fix.engine
        .add_timeline_entry("p", "default", "2024-06-01 second event")
        .await
        .expect("add second");

    let got = fix
        .engine
        .get_page("p", &Default::default())
        .await
        .expect("get_page")
        .expect("page present");
    assert_eq!(
        got.timeline,
        "2024-01-01 first event\n2024-06-01 second event"
    );
}
