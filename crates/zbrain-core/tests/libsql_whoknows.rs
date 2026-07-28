//! `whoknows::find_experts` integration test over the production libsql engine.
//!
//! Proves the search-driven expert-routing pipeline end-to-end on a real SQL
//! backend (not InMemory): candidate retrieval via `search_pages`, the
//! person/company **type filter**, boost-disabled raw relevance as the
//! expertise proxy, effective-date → recency ranking, and the locked
//! score/sort/limit contract.
//!
//! The pure ranking-formula axes (salience center, cold-start floor, tie-break,
//! clamps) are exhaustively covered by the unit tests in
//! `zbrain_core::whoknows`; this suite focuses on the parts that only the real
//! engine can exercise — the SQL candidate scan feeding `fuse_and_boost` with
//! `disable_salience_boost` / `disable_recency_boost` / `types` set.
//!
//! Harness mirrors `libsql_get_health.rs`: each test allocates its own
//! `NamedTempFile` DB (torn down on drop), so the suite runs unconditionally in
//! CI with no daemon.

use serde_json::json;
use tempfile::NamedTempFile;
use zbrain_core::engine::{BrainEngine, EngineConfig, PageInput};
use zbrain_core::libsql::LibsqlEngine;
use zbrain_core::whoknows::{find_experts, FindExpertsOpts};
use zbrain_core::PageKind;

/// Serialize all libsql FFI access in this binary. The `libsql` native
/// library is not safe to drive from multiple OS threads concurrently on
/// Windows (parallel `cargo test` threads crash with STATUS_ACCESS_VIOLATION
/// 0xc0000005). Each test grabs this guard for its whole body so the suite
/// stays green under default parallelism; serial runs are unaffected.
static LIBSQL_TEST_LOCK: std::sync::OnceLock<std::sync::Mutex<()>> = std::sync::OnceLock::new();
fn libsql_test_guard() -> std::sync::MutexGuard<'static, ()> {
    LIBSQL_TEST_LOCK
        .get_or_init(|| std::sync::Mutex::new(()))
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
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

fn page(page_type: &str, title: &str, body: &str, effective_date: Option<&str>) -> PageInput {
    PageInput {
        page_type: page_type.to_string(),
        title: title.to_string(),
        compiled_truth: body.to_string(),
        timeline: None,
        frontmatter: Some(json!({})),
        content_hash: None,
        page_kind: Some(PageKind::Markdown),
        effective_date: effective_date.map(ToString::to_string),
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

/// Empty brain → empty result (no candidates), not an error. Proves the
/// pipeline is live on the production backend (the class of bug that hid in
/// `get_health` before 1-6-4-5: a trait default that errors at runtime).
#[tokio::test]
async fn empty_brain_returns_empty() {
    let _guard = libsql_test_guard();
    let path = temp_db();
    let engine = connected_engine(&path).await;
    let out = find_experts(
        &engine,
        &FindExpertsOpts {
            topic: "lab automation".to_string(),
            ..Default::default()
        },
    )
    .await
    .expect("find_experts");
    assert!(out.is_empty());
}

/// The default person/company type filter excludes note/article pages even when
/// they match the topic. This is the whole point of `SearchOpts.types` — the
/// candidate budget goes to routable pages, not transcripts.
#[tokio::test]
async fn type_filter_excludes_non_person_company() {
    let _guard = libsql_test_guard();
    let path = temp_db();
    let engine = connected_engine(&path).await;

    engine
        .put_page(
            "ada",
            Some("default"),
            &page("person", "Dr. Ada", "expert in lab automation and robotics", None),
        )
        .await
        .expect("put ada");
    engine
        .put_page(
            "autolab",
            Some("default"),
            &page("company", "AutoLab Inc", "lab automation hardware vendor", None),
        )
        .await
        .expect("put autolab");
    // A note that matches the topic just as strongly — must NOT surface.
    engine
        .put_page(
            "note1",
            Some("default"),
            &page("note", "Random Note", "a note about lab automation, not a person", None),
        )
        .await
        .expect("put note1");

    let out = find_experts(
        &engine,
        &FindExpertsOpts {
            topic: "lab automation".to_string(),
            ..Default::default()
        },
    )
    .await
    .expect("find_experts");

    let slugs: Vec<&str> = out.iter().map(|r| r.slug.as_str()).collect();
    assert!(slugs.contains(&"ada"), "person must surface: {slugs:?}");
    assert!(slugs.contains(&"autolab"), "company must surface: {slugs:?}");
    assert!(!slugs.contains(&"note1"), "note must be filtered out: {slugs:?}");
    // Every returned row is a routable type.
    for r in &out {
        assert!(
            r.page_type == "person" || r.page_type == "company",
            "unexpected type {}",
            r.page_type
        );
    }
}

/// An explicit `types` override replaces the default filter — passing
/// `["company"]` keeps only companies.
#[tokio::test]
async fn explicit_types_override_default() {
    let _guard = libsql_test_guard();
    let path = temp_db();
    let engine = connected_engine(&path).await;

    engine
        .put_page("ada", Some("default"), &page("person", "Ada", "lab automation expert", None))
        .await
        .expect("put ada");
    engine
        .put_page("autolab", Some("default"), &page("company", "AutoLab", "lab automation vendor", None))
        .await
        .expect("put autolab");

    let out = find_experts(
        &engine,
        &FindExpertsOpts {
            topic: "lab automation".to_string(),
            types: Some(vec!["company".to_string()]),
            ..Default::default()
        },
    )
    .await
    .expect("find_experts");

    let slugs: Vec<&str> = out.iter().map(|r| r.slug.as_str()).collect();
    assert_eq!(slugs, vec!["autolab"], "only company should remain: {slugs:?}");
}

/// Recency ranking: two equally-relevant people, one recently effective and one
/// old, with no salience seeded. The recent one ranks first because
/// recency_decay(exp(-days/180)) multiplies its score higher. Also proves the
/// effective-date signal flows from the SQL column through `get_effective_dates`
/// into the ranker on the production backend.
#[tokio::test]
async fn recency_orders_equally_relevant_experts() {
    let _guard = libsql_test_guard();
    let path = temp_db();
    let engine = connected_engine(&path).await;

    // Same body → same lexical relevance (raw_match). Differ only by effective date.
    let recent = "2026-07-10T00:00:00Z"; // days ago ≈ small → decay ≈ 1
    let old = "2023-01-01T00:00:00Z"; // years ago → decay near the 0.1 floor
    engine
        .put_page(
            "fresh",
            Some("default"),
            &page("person", "Fresh Expert", "lab automation specialist", Some(recent)),
        )
        .await
        .expect("put fresh");
    engine
        .put_page(
            "stale",
            Some("default"),
            &page("person", "Stale Expert", "lab automation specialist", Some(old)),
        )
        .await
        .expect("put stale");

    let out = find_experts(
        &engine,
        &FindExpertsOpts {
            topic: "lab automation".to_string(),
            ..Default::default()
        },
    )
    .await
    .expect("find_experts");

    assert_eq!(out.len(), 2, "both people should surface");
    assert_eq!(out[0].slug, "fresh", "recent expert ranks first: {out:?}");
    assert_eq!(out[1].slug, "stale");
    // Recent decay > stale decay; stale sits near the 0.1 floor.
    assert!(out[0].factors.recency_factor > out[1].factors.recency_factor);
    assert!(out[1].factors.days_since_effective.unwrap() > out[0].factors.days_since_effective.unwrap());
}

/// The `limit` is honored end-to-end (candidate over-fetch → rank → truncate).
#[tokio::test]
async fn limit_truncates_ranked_output() {
    let _guard = libsql_test_guard();
    let path = temp_db();
    let engine = connected_engine(&path).await;

    for i in 0..5 {
        let slug = format!("p{i}");
        engine
            .put_page(
                &slug,
                Some("default"),
                &page("person", &format!("Person {i}"), "lab automation practitioner", None),
            )
            .await
            .expect("put person");
    }

    let out = find_experts(
        &engine,
        &FindExpertsOpts {
            topic: "lab automation".to_string(),
            limit: Some(3),
            ..Default::default()
        },
    )
    .await
    .expect("find_experts");

    assert_eq!(out.len(), 3, "limit=3 must cap the output");
}

/// A topic that matches nothing returns empty (not the full person list).
#[tokio::test]
async fn no_topic_match_returns_empty() {
    let _guard = libsql_test_guard();
    let path = temp_db();
    let engine = connected_engine(&path).await;
    engine
        .put_page("ada", Some("default"), &page("person", "Ada", "lab automation expert", None))
        .await
        .expect("put ada");

    let out = find_experts(
        &engine,
        &FindExpertsOpts {
            topic: "quantum gastronomy underwater basket weaving".to_string(),
            ..Default::default()
        },
    )
    .await
    .expect("find_experts");
    assert!(out.is_empty(), "irrelevant topic must not surface anyone: {out:?}");
}
