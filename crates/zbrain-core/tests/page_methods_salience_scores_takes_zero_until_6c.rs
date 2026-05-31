//! Slice 6a S6-T2 strong-semantics test: salience score `takes`
//! contribution is exactly zero until slice 6c.
//!
//! This is the **strong-semantics** sibling of
//! `page_methods_get_salience_scores.rs`. The placeholder-lock predecessor
//! only locked the `Unsupported` placeholder; this test pins a
//! behavioural invariant that must hold across both backends:
//!
//! ```text
//! score = COALESCE(emotional_weight, 0) * 5
//!       + ln(1 + distinct_active_take_count)
//! ```
//!
//! In 6a / 6a-pg / 6a-libsql the `takes` table does not exist yet, so the
//! `distinct_active_take_count` term must be hard-coded to `0`. That
//! collapses `ln(1 + 0) = 0` and the score reduces to
//! `emotional_weight * 5`. This test proves the takes contribution is
//! exactly zero on BOTH libsql and PG.
//!
//! Two backend branches:
//! - **libsql** (slice 6a-libsql, plan 14 §11.4 D2): inserts a row with
//!   `emotional_weight = 0.4` and asserts
//!   `(0.4 * 5.0 - score).abs() < 1e-9`, proving the takes term is 0.
//! - **`PostgresEngine`** (gated on `ZBRAIN_TEST_PG_URL`): same invariant.
//!
//! **Slice 6c**: when the `takes` table lands, rewrite both branches to
//! insert `N` takes and assert `score = 0.4*5 + ln(1+N_tags)`.

use tempfile::NamedTempFile;
use zbrain_core::engine::{BrainEngine, EngineConfig, PageInput};
use zbrain_core::libsql::LibsqlEngine;
use zbrain_core::postgres::PostgresEngine;
use zbrain_core::PageRef;

// ---------------------------------------------------------------------------
// libsql branch: locks the 6a-libsql takes-zero invariant.
//
// SQL contract (plan 14 §11.4 D2, slice 6a-libsql):
//   SELECT p.slug, p.source_id, COALESCE(p.emotional_weight, 0.0) * 5.0 AS score
//   FROM pages p
//   WHERE (p.slug, p.source_id) IN ((?,?), ...)
//     AND p.deleted_at IS NULL;
//
// The takes term is hard-coded to 0 in 6a-libsql (the `takes` table does not
// exist yet), so the score MUST equal `emotional_weight * 5` exactly
// (within 1e-9). If a future change ever re-introduces the takes term before
// 6c, this test will fail loudly.
//
// `PageInput` does NOT carry `emotional_weight` — we `put_page` then
// directly UPDATE the column via a raw libsql connection.
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

async fn libsql_seed_source(tmp: &NamedTempFile, id: &str) {
    let path_str = tmp.path().to_string_lossy().into_owned();
    let db = ::libsql::Builder::new_local(&path_str)
        .build()
        .await
        .expect("raw db open for source seed");
    let raw_conn = db.connect().expect("raw conn for source seed");
    raw_conn
        .execute(
            "INSERT OR IGNORE INTO sources (id, name) VALUES (?1, ?2)",
            ::libsql::params![id, id],
        )
        .await
        .expect("seed source");
}

async fn libsql_set_emotional_weight(
    tmp: &NamedTempFile,
    slug: &str,
    source_id: &str,
    weight: Option<f64>,
) {
    let path_str = tmp.path().to_string_lossy().into_owned();
    let db = ::libsql::Builder::new_local(&path_str)
        .build()
        .await
        .expect("raw db open for emotional_weight update");
    let raw_conn = db.connect().expect("raw conn for emotional_weight update");
    raw_conn
        .execute(
            "UPDATE pages SET emotional_weight = ?1 WHERE slug = ?2 AND source_id = ?3",
            ::libsql::params![weight, slug, source_id],
        )
        .await
        .expect("update emotional_weight");
}

#[tokio::test]
async fn libsql_salience_scores_takes_zero_until_6c() {
    let (engine, tmp) = libsql_init_clean_engine().await;
    libsql_seed_source(&tmp, "src-1").await;
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
    libsql_set_emotional_weight(&tmp, "scored", "src-1", Some(0.4)).await;

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
        "6a-libsql takes-zero contract: expected ~{expected} (= 0.4 * 5.0 + ln(1+0)), got {score}"
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
