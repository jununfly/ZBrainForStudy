//! `integrity::scan_integrity` integration test over the production libsql engine.
//!
//! Proves the read-only `check` path end-to-end on a real SQL backend (not
//! InMemory): `list_all_page_refs` + `get_page` enumeration, the bare-tweet
//! and external-link pure detectors applied to real page bodies, the
//! `validate: false` grandfather skip, and the `--type` slug-prefix filter.
//!
//! The pure detectors (fence-skipping, URL-nearby skip, multi-hit, top-page
//! sort) are exhaustively covered by the unit tests in `zbrain_core::integrity`;
//! this suite focuses on what only the real engine exercises — the enumerated
//! page scan and the frontmatter/type-filter gates.
//!
//! Harness mirrors `libsql_whoknows.rs`: each test allocates its own
//! `NamedTempFile` DB (torn down on drop), so the suite runs unconditionally
//! in CI with no daemon.

use serde_json::json;
use tempfile::NamedTempFile;
use zbrain_core::engine::{BrainEngine, EngineConfig, PageInput};
use zbrain_core::integrity::{scan_integrity, IntegrityScanOptions};
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

fn page(page_type: &str, title: &str, body: &str, frontmatter: serde_json::Value) -> PageInput {
    PageInput {
        page_type: page_type.to_string(),
        title: title.to_string(),
        compiled_truth: body.to_string(),
        timeline: None,
        frontmatter: Some(frontmatter),
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

/// Empty brain → zero scanned, zero hits (not an error). Proves the pipeline
/// is live on the production backend.
#[tokio::test]
async fn empty_brain_scans_clean() {
    let path = temp_db();
    let engine = connected_engine(&path).await;
    let r = scan_integrity(&engine, &IntegrityScanOptions::default())
        .await
        .expect("scan");
    assert_eq!(r.pages_scanned, 0);
    assert!(r.bare_hits.is_empty());
    assert!(r.external_hits.is_empty());
}

/// A page with a bare-tweet phrase surfaces exactly one bare hit with the
/// correct 1-based line and phrase.
#[tokio::test]
async fn bare_tweet_detected_on_real_page() {
    let path = temp_db();
    let engine = connected_engine(&path).await;
    engine
        .put_page(
            "garry",
            Some("default"),
            &page(
                "person",
                "Garry",
                "Line one is fine.\nGarry tweeted about the new model architecture.\nLine three.",
                json!({}),
            ),
        )
        .await
        .expect("put garry");

    let r = scan_integrity(&engine, &IntegrityScanOptions::default())
        .await
        .expect("scan");
    assert_eq!(r.pages_scanned, 1);
    assert_eq!(r.bare_hits.len(), 1);
    assert_eq!(r.bare_hits[0].slug, "garry");
    assert_eq!(r.bare_hits[0].line, 2);
    assert_eq!(r.bare_hits[0].phrase, "tweeted about");
}

/// A page with a real tweet-status URL is NOT a bare hit (already cited).
#[tokio::test]
async fn cited_tweet_not_flagged() {
    let path = temp_db();
    let engine = connected_engine(&path).await;
    engine
        .put_page(
            "cited",
            Some("default"),
            &page(
                "person",
                "Cited",
                "He tweeted about it https://x.com/foo/status/1234567890 and more.",
                json!({}),
            ),
        )
        .await
        .expect("put cited");

    let r = scan_integrity(&engine, &IntegrityScanOptions::default())
        .await
        .expect("scan");
    assert_eq!(r.bare_hits.len(), 0, "cited tweet must not be a bare hit");
}

/// External markdown links are collected with their source slug + line.
#[tokio::test]
async fn external_links_collected() {
    let path = temp_db();
    let engine = connected_engine(&path).await;
    engine
        .put_page(
            "src",
            Some("default"),
            &page(
                "note",
                "Source",
                "See [docs](https://example.com/a) and [guide](https://example.com/b).",
                json!({}),
            ),
        )
        .await
        .expect("put src");

    let r = scan_integrity(&engine, &IntegrityScanOptions::default())
        .await
        .expect("scan");
    assert_eq!(r.external_hits.len(), 2);
    let urls: Vec<&str> = r.external_hits.iter().map(|h| h.url.as_str()).collect();
    assert_eq!(urls, vec!["https://example.com/a", "https://example.com/b"]);
    assert_eq!(r.external_hits[0].slug, "src");
    assert_eq!(r.external_hits[0].line, 1);
}

/// `frontmatter.validate: false` opts the page out of the integrity scan.
#[tokio::test]
async fn validate_false_skips_page() {
    let path = temp_db();
    let engine = connected_engine(&path).await;
    engine
        .put_page(
            "grandfathered",
            Some("default"),
            &page(
                "person",
                "GF",
                "This page tweeted about things but is grandfathered.",
                json!({ "validate": false }),
            ),
        )
        .await
        .expect("put gf");

    let r = scan_integrity(&engine, &IntegrityScanOptions::default())
        .await
        .expect("scan");
    assert_eq!(r.pages_scanned, 0, "grandfathered page must be skipped");
    assert_eq!(r.bare_hits.len(), 0);
}

/// `--type person` (slug prefix `person/`) restricts the scan.
#[tokio::test]
async fn type_filter_restricts_scan() {
    let path = temp_db();
    let engine = connected_engine(&path).await;
    engine
        .put_page(
            "person/ada",
            Some("default"),
            &page("person", "Ada", "Ada tweeted about the project recently.", json!({})),
        )
        .await
        .expect("put person/ada");
    engine
        .put_page(
            "note/misc",
            Some("default"),
            &page("note", "Misc", "Misc tweeted about unrelated things.", json!({})),
        )
        .await
        .expect("put note/misc");

    let r = scan_integrity(
        &engine,
        &IntegrityScanOptions {
            type_filter: Some("person".to_string()),
            ..Default::default()
        },
    )
    .await
    .expect("scan");

    assert_eq!(r.pages_scanned, 1, "only the person/ page should scan");
    assert_eq!(r.bare_hits.len(), 1);
    assert_eq!(r.bare_hits[0].slug, "person/ada");
}
