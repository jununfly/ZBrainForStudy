//! Slice 6a S6-T1 placeholder-lock test: `get_salience_scores` placeholder lock.
//!
//! Slice 6a-pg (PG-advanced-reads S2 RED) appends `PostgresEngine` mirror
//! tests at the bottom of this file. They lock the PG semantics agreed in
//! plan 14 §11.1:
//!   `SELECT p.slug, p.source_id, COALESCE(p.emotional_weight, 0.0) * 5.0 AS score
//!    FROM pages p
//!    JOIN unnest($1::text[], $2::text[]) AS u(slug, source_id)
//!      ON p.slug = u.slug AND p.source_id = u.source_id
//!    WHERE p.deleted_at IS NULL`
//! returning a `HashMap<String, f64>` keyed by `format!("{source_id}::{slug}")`.
//!
//! The 6a quirk (`COUNT DISTINCT takes` term hard-coded to 0 until 6c) is
//! locked separately by the sibling test
//! `page_methods_salience_scores_takes_zero_until_6c.rs`.

use tempfile::NamedTempFile;
use zbrain_core::engine::{BrainEngine, EngineConfig, PageInput};
use zbrain_core::libsql::LibsqlEngine;
use zbrain_core::PageRef;

async fn init_clean_engine() -> (LibsqlEngine, NamedTempFile) {
    let path = NamedTempFile::new().expect("alloc temp db file");
    let engine = LibsqlEngine::new();
    let cfg = EngineConfig {
        database_url: None,
        database_path: Some(path.path().to_string_lossy().into_owned()),
    };
    engine.connect(&cfg).await.expect("connect");
    engine.init_schema().await.expect("init_schema");
    (engine, path)
}

#[tokio::test]
async fn slice_6a_page_methods_get_salience_scores_returns_unsupported() {
    let (engine, _tmp) = init_clean_engine().await;
    let refs = vec![PageRef {
        slug: "a".to_string(),
        source_id: "src-1".to_string(),
    }];
    let err = engine
        .get_salience_scores(&refs)
        .await
        .expect_err("6a placeholder-lock: get_salience_scores must be Unsupported");
    let msg = err.to_string();
    assert!(
        msg.contains("pending slice 6a"),
        "expected placeholder marker, got: {msg}"
    );
    engine.disconnect().await.expect("disconnect");
}

// ---------------------------------------------------------------------------
// PostgresEngine mirror tests (slice 6a-pg PG-advanced-reads)
//
// Locks the PG `get_salience_scores` semantics from plan 14 §11.1:
//   SELECT p.slug, p.source_id, COALESCE(p.emotional_weight, 0.0) * 5.0 AS score
//   FROM pages p
//   JOIN unnest($1::text[], $2::text[]) AS u(slug, source_id)
//     ON p.slug = u.slug AND p.source_id = u.source_id
//   WHERE p.deleted_at IS NULL;
// keyed by `format!("{source_id}::{slug}")`. The 6a quirk hard-codes the
// `+ ln(1 + COUNT DISTINCT take)` term to 0 until 6c lands the `takes` table,
// so the formula degenerates to `COALESCE(emotional_weight, 0.0) * 5.0`.
//
// `PageInput` does NOT carry `emotional_weight` (it is computed by the
// `recompute_emotional_weight` pipeline). To exercise the score arithmetic
// here we `put_page` first, then directly UPDATE the column via raw SQL.
//
// Gated on `ZBRAIN_TEST_PG_URL`. `#[serial_test::serial]` because they share
// the `pages` table in the configured test database.
// ---------------------------------------------------------------------------

use zbrain_core::postgres::PostgresEngine;

fn pg_url() -> Option<String> {
    // LOCAL PG NOTE: working Homebrew PostgreSQL 16.14 on `localhost:5434`,
    // db `zbrain_test`. URL persisted in gitignored `<repo-root>/.env`:
    //   ZBRAIN_TEST_PG_URL=postgres://postgres:postgres@localhost:5434/zbrain_test
    // Activate: `set -a; source .env; set +a`. `skipping: ... unset` ≠ valid
    // PG result — re-source `.env`. Source of truth:
    // docs/plans/20260526/17-session-state-110c.md L139-150 (#110-c, 2026-05-30).
    std::env::var("ZBRAIN_TEST_PG_URL").ok()
}

async fn pg_init_clean_engine() -> Option<PostgresEngine> {
    let url = pg_url()?;
    let engine = PostgresEngine::new();
    let cfg = EngineConfig {
        database_url: Some(url.clone()),
        database_path: None,
    };
    engine.connect(&cfg).await.expect("connect");
    engine.init_schema().await.expect("init_schema");

    let pool = sqlx::postgres::PgPoolOptions::new()
        .max_connections(1)
        .connect(&url)
        .await
        .expect("verification pool");
    sqlx::query("TRUNCATE TABLE pages RESTART IDENTITY CASCADE")
        .execute(&pool)
        .await
        .expect("truncate pages");
    pool.close().await;
    Some(engine)
}

async fn pg_seed_source(id: &str) {
    let url = pg_url().expect("ZBRAIN_TEST_PG_URL set for source seed");
    let pool = sqlx::postgres::PgPoolOptions::new()
        .max_connections(1)
        .connect(&url)
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

async fn pg_set_emotional_weight(slug: &str, source_id: &str, weight: Option<f64>) {
    let url = pg_url().expect("ZBRAIN_TEST_PG_URL set for emotional_weight update");
    let pool = sqlx::postgres::PgPoolOptions::new()
        .max_connections(1)
        .connect(&url)
        .await
        .expect("emotional_weight pool");
    sqlx::query("UPDATE pages SET emotional_weight = $1 WHERE slug = $2 AND source_id = $3")
        .bind(weight)
        .bind(slug)
        .bind(source_id)
        .execute(&pool)
        .await
        .expect("update emotional_weight");
    pool.close().await;
}

fn assert_close(actual: f64, expected: f64, label: &str) {
    assert!(
        (actual - expected).abs() < 1e-9,
        "{label}: expected ~{expected}, got {actual}"
    );
}

#[tokio::test]
#[serial_test::serial]
async fn postgres_get_salience_scores_returns_score_for_each_ref() {
    let Some(engine) = pg_init_clean_engine().await else {
        eprintln!("skipping: ZBRAIN_TEST_PG_URL unset");
        return;
    };
    pg_seed_source("src-1").await;
    pg_seed_source("src-2").await;
    for (slug, src) in [("alpha", "src-1"), ("beta", "src-2")] {
        engine
            .put_page(
                slug,
                Some(src),
                &PageInput {
                    page_type: "note".to_string(),
                    title: slug.to_string(),
                    compiled_truth: "body".to_string(),
                    ..PageInput::default()
                },
            )
            .await
            .expect("seed page");
    }
    // 6a quirk: score = COALESCE(emotional_weight, 0.0) * 5.0
    pg_set_emotional_weight("alpha", "src-1", Some(0.4)).await;
    pg_set_emotional_weight("beta", "src-2", Some(1.0)).await;

    let refs = vec![
        PageRef {
            slug: "alpha".to_string(),
            source_id: "src-1".to_string(),
        },
        PageRef {
            slug: "beta".to_string(),
            source_id: "src-2".to_string(),
        },
    ];
    let scores = engine
        .get_salience_scores(&refs)
        .await
        .expect("get_salience_scores");

    assert_eq!(scores.len(), 2, "two refs requested → two entries");
    assert_close(
        *scores.get("src-1::alpha").expect("missing src-1::alpha"),
        0.4 * 5.0,
        "alpha score",
    );
    engine.disconnect().await.expect("disconnect");
}

#[tokio::test]
#[serial_test::serial]
async fn postgres_get_salience_scores_treats_null_emotional_weight_as_zero() {
    // Brand-new pages start with emotional_weight = NULL (the recompute
    // pipeline lands later). The COALESCE(..., 0.0) wrapper MUST collapse
    // NULL → 0.0 so the score is exactly 0.0, NOT a propagated NULL / NaN /
    // dropped key.
    let Some(engine) = pg_init_clean_engine().await else {
        eprintln!("skipping: ZBRAIN_TEST_PG_URL unset");
        return;
    };
    pg_seed_source("src-1").await;
    engine
        .put_page(
            "fresh",
            Some("src-1"),
            &PageInput {
                page_type: "note".to_string(),
                title: "Fresh".to_string(),
                compiled_truth: "body".to_string(),
                ..PageInput::default()
            },
        )
        .await
        .expect("seed page");
    // Explicitly assert the column is NULL (no recompute step ran).
    pg_set_emotional_weight("fresh", "src-1", None).await;

    let refs = vec![PageRef {
        slug: "fresh".to_string(),
        source_id: "src-1".to_string(),
    }];
    let scores = engine
        .get_salience_scores(&refs)
        .await
        .expect("get_salience_scores");

    assert_eq!(
        scores.len(),
        1,
        "NULL emotional_weight must still yield an entry"
    );
    assert_close(
        *scores.get("src-1::fresh").expect("missing src-1::fresh"),
        0.0,
        "NULL emotional_weight → 0.0 * 5.0",
    );
    engine.disconnect().await.expect("disconnect");
}

#[tokio::test]
#[serial_test::serial]
async fn postgres_get_salience_scores_excludes_soft_deleted_rows() {
    let Some(engine) = pg_init_clean_engine().await else {
        eprintln!("skipping: ZBRAIN_TEST_PG_URL unset");
        return;
    };
    pg_seed_source("src-1").await;
    for slug in ["live-slug", "tombstone-slug"] {
        engine
            .put_page(
                slug,
                Some("src-1"),
                &PageInput {
                    page_type: "note".to_string(),
                    title: slug.to_string(),
                    compiled_truth: "body".to_string(),
                    ..PageInput::default()
                },
            )
            .await
            .expect("seed page");
    }
    pg_set_emotional_weight("live-slug", "src-1", Some(0.2)).await;
    pg_set_emotional_weight("tombstone-slug", "src-1", Some(0.9)).await;
    engine
        .soft_delete_page("tombstone-slug", Some("src-1"))
        .await
        .expect("soft delete tombstone");

    let refs = vec![
        PageRef {
            slug: "live-slug".to_string(),
            source_id: "src-1".to_string(),
        },
        PageRef {
            slug: "tombstone-slug".to_string(),
            source_id: "src-1".to_string(),
        },
    ];
    let scores = engine
        .get_salience_scores(&refs)
        .await
        .expect("get_salience_scores");

    assert_eq!(scores.len(), 1, "soft-deleted rows must be dropped");
    assert_close(
        *scores
            .get("src-1::live-slug")
            .expect("missing src-1::live-slug"),
        0.2 * 5.0,
        "live-slug score",
    );
    assert!(
        !scores.contains_key("src-1::tombstone-slug"),
        "soft-deleted rows must be excluded by `deleted_at IS NULL` filter"
    );
    engine.disconnect().await.expect("disconnect");
}

#[tokio::test]
#[serial_test::serial]
async fn postgres_get_salience_scores_returns_empty_map_for_empty_input() {
    let Some(engine) = pg_init_clean_engine().await else {
        eprintln!("skipping: ZBRAIN_TEST_PG_URL unset");
        return;
    };

    let scores = engine
        .get_salience_scores(&[])
        .await
        .expect("get_salience_scores on empty input");
    assert!(scores.is_empty(), "empty refs slice → empty map");
    engine.disconnect().await.expect("disconnect");
}
