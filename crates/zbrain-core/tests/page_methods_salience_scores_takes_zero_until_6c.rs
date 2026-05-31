//! Slice 6a-pg S6-T2 strong-semantics test: salience score `takes`
//! contribution is exactly zero until slice 6c.
//!
//! This is the **strong-semantics** sibling of
//! `page_methods_get_salience_scores.rs`. The generic placeholder test
//! only locked the `Unsupported` placeholder; this test pins down a
//! behavioural invariant that must hold across backends:
//!
//! ```text
//! score = COALESCE(emotional_weight, 0) * 5
//!       + ln(1 + distinct_active_take_count)
//! ```
//!
//! In 6a / 6a-pg the `takes` table does not exist yet, so the
//! `distinct_active_take_count` term must be hard-coded to `0`. That
//! collapses `ln(1 + 0) = 0` and the score reduces to
//! `emotional_weight * 5`. This test proves the takes contribution is
//! exactly zero.
//!
//! Two backend branches:
//! - **libsql**: per decision D1 in plan 14 §11.4, the libsql 5
//!   advanced-reads stay on the trait default `Unsupported` until slice
//!   6c. The libsql branch therefore asserts the `Unsupported` placeholder
//!   marker — this is the standing contract until 6c lands.
//! - **`PostgresEngine`** (gated on `ZBRAIN_TEST_PG_URL`): inserts a row
//!   with `emotional_weight = 0.4` and asserts
//!   `(0.4 * 5.0 - score).abs() < 1e-9`, proving the takes term is 0.
//!
//! **Slice 6c**: when the `takes` table lands, rewrite both branches to
//! insert `N` takes and assert `score = 0.4*5 + ln(1+N_tags)`. The
//! libsql branch will also flip from `Unsupported` to the real impl.

use tempfile::NamedTempFile;
use zbrain_core::engine::{BrainEngine, EngineConfig, PageInput};
use zbrain_core::libsql::LibsqlEngine;
use zbrain_core::postgres::PostgresEngine;
use zbrain_core::PageRef;

// ---------------------------------------------------------------------------
// libsql branch: holds the `Unsupported` contract until slice 6c (D1).
// ---------------------------------------------------------------------------

async fn libsql_init_clean_engine() -> (LibsqlEngine, NamedTempFile) {
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
async fn libsql_salience_scores_takes_zero_until_6c_remains_unsupported() {
    // D1: libsql advanced-reads stay on trait default `Unsupported` in
    // 6a-pg. Slice 6c rewrites this branch alongside the PG branch.
    let (engine, _tmp) = libsql_init_clean_engine().await;
    let refs = vec![PageRef {
        slug: "a".to_string(),
        source_id: "src-1".to_string(),
    }];
    let err = engine
        .get_salience_scores(&refs)
        .await
        .expect_err("libsql 6a/6a-pg: get_salience_scores must remain Unsupported (D1)");
    let msg = err.to_string();
    assert!(
        msg.contains("pending slice 6a"),
        "expected placeholder marker, got: {msg}"
    );
    engine.disconnect().await.expect("disconnect");
}

// ---------------------------------------------------------------------------
// PostgresEngine branch: locks the 6a-pg takes-zero invariant.
//
// SQL contract (plan 14 §11.1):
//   SELECT p.slug, p.source_id, COALESCE(p.emotional_weight, 0.0) * 5.0 AS score
//   FROM pages p
//   JOIN unnest($1::text[], $2::text[]) AS u(slug, source_id)
//     ON p.slug = u.slug AND p.source_id = u.source_id
//   WHERE p.deleted_at IS NULL;
//
// The takes term is hard-coded to 0 in 6a-pg, so the score MUST equal
// `emotional_weight * 5` exactly (within 1e-9). If a future change ever
// re-introduces the takes term before 6c, this test will fail loudly.
//
// `PageInput` does NOT carry `emotional_weight` — we `put_page` then
// directly UPDATE the column via raw SQL.
// ---------------------------------------------------------------------------

fn pg_url() -> Option<String> {
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

#[tokio::test]
#[serial_test::serial]
async fn postgres_salience_scores_takes_zero_until_6c() {
    let Some(engine) = pg_init_clean_engine().await else {
        eprintln!("skipping: ZBRAIN_TEST_PG_URL unset");
        return;
    };
    pg_seed_source("src-1").await;
    engine
        .put_page(
            "scored",
            Some("src-1"),
            &PageInput {
                page_type: "note".to_string(),
                title: "Scored".to_string(),
                compiled_truth: "body".to_string(),
                ..PageInput::default()
            },
        )
        .await
        .expect("seed page");
    pg_set_emotional_weight("scored", "src-1", Some(0.4)).await;

    let refs = vec![PageRef {
        slug: "scored".to_string(),
        source_id: "src-1".to_string(),
    }];
    let scores = engine
        .get_salience_scores(&refs)
        .await
        .expect("get_salience_scores");

    let score = *scores
        .get("src-1::scored")
        .expect("missing src-1::scored entry");
    let expected = 0.4 * 5.0;
    assert!(
        (score - expected).abs() < 1e-9,
        "6a-pg takes-zero contract: expected ~{expected} (= 0.4 * 5.0 + ln(1+0)), got {score}"
    );
    engine.disconnect().await.expect("disconnect");
}
