//! `whoknows::find_experts` integration test over the production PostgreSQL
//! engine, via an ephemeral `pg-embed` instance (see `support::pg_fixture`).
//!
//! Mirrors `libsql_whoknows.rs` to confirm the expert-routing pipeline —
//! candidate scan, person/company type filter, boost-disabled raw relevance,
//! and effective-date recency ranking — behaves identically on the strongly-
//! typed PG backend. PG's type/constraint enforcement has repeatedly caught
//! bugs the untyped SQLite path hid, so a PG mirror has independent value.

mod support;

use zbrain_core::engine::{BrainEngine, PageInput};
use zbrain_core::whoknows::{find_experts, FindExpertsOpts};

fn page(page_type: &str, title: &str, body: &str, effective_date: Option<&str>) -> PageInput {
    PageInput {
        page_type: page_type.to_string(),
        title: title.to_string(),
        compiled_truth: body.to_string(),
        effective_date: effective_date.map(ToString::to_string),
        ..Default::default()
    }
}

#[tokio::test]
async fn empty_brain_returns_empty() {
    let fix = support::pg_fixture::PgFixture::start().await;
    let out = find_experts(
        &fix.engine,
        &FindExpertsOpts {
            topic: "lab automation".to_string(),
            ..Default::default()
        },
    )
    .await
    .expect("find_experts");
    assert!(out.is_empty());
}

#[tokio::test]
async fn type_filter_excludes_non_person_company() {
    let fix = support::pg_fixture::PgFixture::start().await;

    fix.engine
        .put_page("ada", None, &page("person", "Dr. Ada", "expert in lab automation and robotics", None))
        .await
        .expect("put ada");
    fix.engine
        .put_page("autolab", None, &page("company", "AutoLab Inc", "lab automation hardware vendor", None))
        .await
        .expect("put autolab");
    fix.engine
        .put_page("note1", None, &page("note", "Random Note", "a note about lab automation, not a person", None))
        .await
        .expect("put note1");

    let out = find_experts(
        &fix.engine,
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
    for r in &out {
        assert!(
            r.page_type == "person" || r.page_type == "company",
            "unexpected type {}",
            r.page_type
        );
    }
}

#[tokio::test]
async fn recency_orders_equally_relevant_experts() {
    let fix = support::pg_fixture::PgFixture::start().await;

    let recent = "2026-07-10T00:00:00Z";
    let old = "2023-01-01T00:00:00Z";
    fix.engine
        .put_page("fresh", None, &page("person", "Fresh Expert", "lab automation specialist", Some(recent)))
        .await
        .expect("put fresh");
    fix.engine
        .put_page("stale", None, &page("person", "Stale Expert", "lab automation specialist", Some(old)))
        .await
        .expect("put stale");

    let out = find_experts(
        &fix.engine,
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
    assert!(out[0].factors.recency_factor > out[1].factors.recency_factor);
}

#[tokio::test]
async fn explicit_types_override_default() {
    let fix = support::pg_fixture::PgFixture::start().await;

    fix.engine
        .put_page("ada", None, &page("person", "Ada", "lab automation expert", None))
        .await
        .expect("put ada");
    fix.engine
        .put_page("autolab", None, &page("company", "AutoLab", "lab automation vendor", None))
        .await
        .expect("put autolab");

    let out = find_experts(
        &fix.engine,
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
