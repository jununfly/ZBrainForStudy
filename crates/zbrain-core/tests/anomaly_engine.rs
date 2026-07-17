//! Slice 1-6-4-11-2 — `BrainEngine::find_anomalies` integration tests for the
//! Libsql + InMemory backends (SQL dialect + in-Rust compute). Postgres is
//! covered separately by slice 1-6-4-11-3.
//!
//! The scenario is deliberately date-independent: with `--lookback-days 1` the
//! baseline window is just *yesterday*, so pages touched *today* are brand-new
//! cohorts → `baseline_mean = 0` and they surface whenever `count >= 2`
//! (the zero-stddev fallback). This exercises the full engine →
//! `compute_anomalies_from_buckets` path without needing to backdate rows.

use std::collections::HashSet;

use zbrain_core::anomaly::{AnomaliesOpts, CohortKind};
use zbrain_core::engine::{BrainEngine, EngineConfig, InMemoryEngine, PageInput};
use zbrain_core::libsql::LibsqlEngine;

fn today_iso() -> String {
    chrono::Utc::now().date_naive().format("%Y-%m-%d").to_string()
}

fn minimal_input(page_type: &str) -> PageInput {
    PageInput {
        page_type: page_type.to_string(),
        title: "anomaly-test".to_string(),
        compiled_truth: "body".to_string(),
        ..Default::default()
    }
}

async fn init_clean_libsql() -> (LibsqlEngine, tempfile::NamedTempFile) {
    let path = tempfile::NamedTempFile::new().expect("alloc temp db file");
    let path_str = path.path().to_string_lossy().into_owned();
    let engine = LibsqlEngine::new();
    engine
        .connect(&EngineConfig {
            database_url: None,
            database_path: Some(path_str),
        })
        .await
        .expect("connect");
    engine.init_schema().await.expect("init_schema");
    (engine, path)
}

/// Assert that both `tag=hot` and `type=note` surface as brand-new-cohort
/// anomalies with `count == 3` and `baseline_mean == 0`.
fn assert_brand_new_cohorts(rows: &[zbrain_core::anomaly::AnomalyResult]) {
    let mut found = HashSet::new();
    for r in rows {
        if r.count == 3 && r.baseline_mean.abs() < 1e-9 {
            found.insert((r.cohort_kind, r.cohort_value.clone()));
        }
    }
    assert!(
        found.contains(&(CohortKind::Tag, "hot".to_string())),
        "expected tag=hot anomaly, got: {rows:?}"
    );
    assert!(
        found.contains(&(CohortKind::Type, "note".to_string())),
        "expected type=note anomaly, got: {rows:?}"
    );
}

#[tokio::test]
async fn inmemory_find_anomalies_brand_new_cohort() {
    let engine = InMemoryEngine::new();
    for i in 0..3 {
        let mut input = minimal_input("note");
        // InMemory keeps tags in frontmatter (page_tags helper reads it).
        input.frontmatter = Some(serde_json::json!({ "tags": ["hot"] }));
        engine
            .put_page(&format!("p{i}"), None, &input)
            .await
            .expect("put_page");
    }
    let opts = AnomaliesOpts {
        since: Some(today_iso()),
        lookback_days: Some(1),
        sigma: Some(3.0),
    };
    let rows = engine.find_anomalies(opts).await.expect("find_anomalies");
    assert_brand_new_cohorts(&rows);
}

#[tokio::test]
async fn libsql_find_anomalies_brand_new_cohort() {
    let (engine, _path) = init_clean_libsql().await;
    for i in 0..3 {
        let input = minimal_input("note");
        engine
            .put_page(&format!("p{i}"), None, &input)
            .await
            .expect("put_page");
        // Libsql keeps tags in the `page_tags` table (separate from frontmatter).
        engine
            .add_tag(&format!("p{i}"), "hot", None)
            .await
            .expect("add_tag");
    }
    let opts = AnomaliesOpts {
        since: Some(today_iso()),
        lookback_days: Some(1),
        sigma: Some(3.0),
    };
    let rows = engine.find_anomalies(opts).await.expect("find_anomalies");
    assert_brand_new_cohorts(&rows);
}

#[tokio::test]
async fn libsql_find_anomalies_empty_store_returns_no_anomalies() {
    let (engine, _path) = init_clean_libsql().await;
    let opts = AnomaliesOpts {
        since: Some(today_iso()),
        lookback_days: Some(30),
        sigma: Some(3.0),
    };
    let rows = engine.find_anomalies(opts).await.expect("find_anomalies");
    assert!(rows.is_empty(), "empty store must yield no anomalies: {rows:?}");
}
