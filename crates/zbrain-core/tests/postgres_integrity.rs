//! `integrity::scan_integrity` integration test over the production
//! PostgreSQL engine, via an ephemeral `pg-embed` instance.
//!
//! Mirrors `libsql_integrity.rs` to confirm the read-only `check` path behaves
//! identically on the strongly-typed PG backend. PG's type/constraint
//! enforcement has repeatedly caught bugs the untyped SQLite path hid, so a PG
//! mirror has independent value.

mod support;

use serde_json::json;
use zbrain_core::engine::{BrainEngine, PageInput};
use zbrain_core::integrity::{scan_integrity, IntegrityScanOptions};

fn page(page_type: &str, title: &str, body: &str, frontmatter: serde_json::Value) -> PageInput {
    PageInput {
        page_type: page_type.to_string(),
        title: title.to_string(),
        compiled_truth: body.to_string(),
        frontmatter: Some(frontmatter),
        ..Default::default()
    }
}

#[tokio::test]
async fn empty_brain_scans_clean() {
    let fix = support::pg_fixture::PgFixture::start().await;
    let r = scan_integrity(&fix.engine, &IntegrityScanOptions::default())
        .await
        .expect("scan");
    assert_eq!(r.pages_scanned, 0);
    assert!(r.bare_hits.is_empty());
    assert!(r.external_hits.is_empty());
}

#[tokio::test]
async fn bare_tweet_detected_on_real_page() {
    let fix = support::pg_fixture::PgFixture::start().await;
    fix.engine
        .put_page(
            "garry",
            None,
            &page(
                "person",
                "Garry",
                "Line one is fine.\nGarry tweeted about the new model architecture.\nLine three.",
                json!({}),
            ),
        )
        .await
        .expect("put garry");

    let r = scan_integrity(&fix.engine, &IntegrityScanOptions::default())
        .await
        .expect("scan");
    assert_eq!(r.pages_scanned, 1);
    assert_eq!(r.bare_hits.len(), 1);
    assert_eq!(r.bare_hits[0].slug, "garry");
    assert_eq!(r.bare_hits[0].line, 2);
    assert_eq!(r.bare_hits[0].phrase, "tweeted about");
}

#[tokio::test]
async fn external_links_collected() {
    let fix = support::pg_fixture::PgFixture::start().await;
    fix.engine
        .put_page(
            "src",
            None,
            &page(
                "note",
                "Source",
                "See [docs](https://example.com/a) and [guide](https://example.com/b).",
                json!({}),
            ),
        )
        .await
        .expect("put src");

    let r = scan_integrity(&fix.engine, &IntegrityScanOptions::default())
        .await
        .expect("scan");
    assert_eq!(r.external_hits.len(), 2);
    let urls: Vec<&str> = r.external_hits.iter().map(|h| h.url.as_str()).collect();
    assert_eq!(urls, vec!["https://example.com/a", "https://example.com/b"]);
}

#[tokio::test]
async fn validate_false_skips_page() {
    let fix = support::pg_fixture::PgFixture::start().await;
    fix.engine
        .put_page(
            "grandfathered",
            None,
            &page(
                "person",
                "GF",
                "This page tweeted about things but is grandfathered.",
                json!({ "validate": false }),
            ),
        )
        .await
        .expect("put gf");

    let r = scan_integrity(&fix.engine, &IntegrityScanOptions::default())
        .await
        .expect("scan");
    assert_eq!(r.pages_scanned, 0, "grandfathered page must be skipped");
    assert_eq!(r.bare_hits.len(), 0);
}
