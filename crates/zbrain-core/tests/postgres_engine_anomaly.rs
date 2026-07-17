//! Slice 11 anomalies PG `find_anomalies` tests.
//!
//! Mirrors the TS `findAnomalies` contract against a real pg-embed Postgres,
//! proving the `generate_series` / `date_trunc` / `array_agg` SQL matches the
//! libsql + InMemory backends (Phase7 PG-mirror convention). The pure stats
//! are shared via `zbrain_core::anomaly`, so only the SQL dialect differs.

mod support;

use chrono::{Duration, Utc};
use zbrain_core::anomaly::{AnomaliesOpts, CohortKind};
use zbrain_core::engine::{BrainEngine, PageInput};

async fn pg_seed_source(url: &str, id: &str) {
    let pool = sqlx::postgres::PgPoolOptions::new()
        .max_connections(1)
        .connect(url)
        .await
        .expect("source seed pool");
    sqlx::query("INSERT INTO sources (id, name) VALUES ($1, $2) ON CONFLICT (id) DO NOTHING")
        .bind(id)
        .bind(id)
        .execute(&pool)
        .await
        .expect("seed source");
    pool.close().await;
}

/// Backdate a page's `updated_at` to a specific RFC3339 timestamp (test helper
/// for baseline-window seeding — `put_page` always stamps `now()`).
async fn pg_backdate_page(url: &str, slug: &str, iso: &str) {
    let pool = sqlx::postgres::PgPoolOptions::new()
        .max_connections(1)
        .connect(url)
        .await
        .expect("backdate pool");
    sqlx::query("UPDATE pages SET updated_at = $1::timestamptz WHERE slug = $2")
        .bind(iso)
        .bind(slug)
        .execute(&pool)
        .await
        .expect("backdate");
    pool.close().await;
}

/// Brand-new tag cohort: 3 pages tagged `rust` created today with no baseline
/// history must surface as an anomaly (count >= 2, baseline_mean 0).
#[tokio::test]
async fn postgres_find_anomalies_brand_new_tag_cohort_surfaces() {
    let fix = support::pg_fixture::PgFixture::start().await;
    let engine = &fix.engine;
    pg_seed_source(&fix.url, "src-anom").await;
    for slug in ["r1", "r2", "r3"] {
        engine
            .put_page(
                slug,
                Some("src-anom"),
                &PageInput {
                    page_type: "note".to_string(),
                    title: slug.to_string(),
                    compiled_truth: "body".to_string(),
                    ..PageInput::default()
                },
            )
            .await
            .expect("seed");
        engine
            .add_tag(slug, "rust", Some("src-anom"))
            .await
            .expect("tag");
    }
    let today = Utc::now().format("%Y-%m-%d").to_string();
    let results = engine
        .find_anomalies(AnomaliesOpts {
            since: Some(today),
            lookback_days: Some(30),
            sigma: Some(3.0),
        })
        .await
        .expect("find_anomalies");

    let rust = results
        .iter()
        .find(|r| r.cohort_value == "rust" && matches!(r.cohort_kind, CohortKind::Tag));
    let rust = rust.expect("brand-new rust tag cohort must be an anomaly");
    assert_eq!(rust.count, 3);
    assert!(
        (rust.baseline_mean).abs() < 1e-9,
        "brand-new cohort baseline_mean 0"
    );
}

/// Spike: a tag cohort touched once/day for 10 of the 30 baseline-window
/// days (the `generate_series` CROSS JOIN zeroes the other 20 days), then 25
/// times today, must surface with baseline_mean = 10/30 and a positive sigma.
/// Exercises the densified `generate_series` CROSS JOIN baseline (matches TS).
#[tokio::test]
async fn postgres_find_anomalies_spike_with_densified_baseline() {
    let fix = support::pg_fixture::PgFixture::start().await;
    let engine = &fix.engine;
    pg_seed_source(&fix.url, "src-spike").await;

    let today = Utc::now().date_naive();
    // Baseline: 10 days, 1 page/day (type daily, tag burst), backdated.
    for i in 1..=10 {
        let day = today - Duration::days(i as i64);
        let slug = format!("base{i}");
        engine
            .put_page(
                &slug,
                Some("src-spike"),
                &PageInput {
                    page_type: "daily".to_string(),
                    title: slug.clone(),
                    compiled_truth: "body".to_string(),
                    ..PageInput::default()
                },
            )
            .await
            .expect("seed base");
        engine
            .add_tag(&slug, "burst", Some("src-spike"))
            .await
            .expect("tag base");
        let iso = format!("{}T12:00:00Z", day.format("%Y-%m-%d"));
        pg_backdate_page(&fix.url, &slug, &iso).await;
    }
    // Today: 25 pages (type daily, tag burst).
    for i in 0..25 {
        let slug = format!("spike{i}");
        engine
            .put_page(
                &slug,
                Some("src-spike"),
                &PageInput {
                    page_type: "daily".to_string(),
                    title: slug.clone(),
                    compiled_truth: "body".to_string(),
                    ..PageInput::default()
                },
            )
            .await
            .expect("seed spike");
        engine
            .add_tag(&slug, "burst", Some("src-spike"))
            .await
            .expect("tag spike");
    }

    let results = engine
        .find_anomalies(AnomaliesOpts {
            since: Some(today.format("%Y-%m-%d").to_string()),
            lookback_days: Some(30),
            sigma: Some(3.0),
        })
        .await
        .expect("find_anomalies");

    let burst = results
        .iter()
        .find(|r| r.cohort_value == "burst" && matches!(r.cohort_kind, CohortKind::Tag));
    let burst = burst.expect("burst tag cohort must be an anomaly");
    assert_eq!(burst.count, 25, "today count of burst-tagged pages");
    assert!(
        (burst.baseline_mean - 10.0 / 30.0).abs() < 1e-9,
        "baseline mean = 10/30 (10 of 30 lookback days touched, densified generate_series)"
    );
    assert!(burst.sigma_observed > 0.0, "positive sigma for a large spike");
}
