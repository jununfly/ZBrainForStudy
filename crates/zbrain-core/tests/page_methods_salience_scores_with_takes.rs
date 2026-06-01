//! Slice 6c-takes-salience: full salience formula with active takes.
//!
//! The complete TS formula (mirroring `pglite-engine.ts` L2596-2617):
//!
//! ```text
//! score = COALESCE(emotional_weight, 0) * 5
//!       + ln(1 + COUNT(DISTINCT t.id) WHERE t.active = TRUE)
//! ```
//!
//! Three invariants under test:
//!
//! 1. **N-takes positive formula** — a page with `emotional_weight = 0.4`
//!    and N=2 active takes must yield `0.4*5 + ln(1+2) = 2.0 + ln(3)`.
//!
//! 2. **active=FALSE takes excluded** — an active take contributes;
//!    an `active = FALSE` take does not. With 1 active + 1 inactive,
//!    the score is `0.4*5 + ln(1+1) = 2.0 + ln(2)`.
//!
//! 3. **Cross-page isolation** — takes belonging to page B must not inflate
//!    page A's score. Page A has 0 takes → score = `0.4*5 + ln(1+0) = 2.0`.

mod support;

use tempfile::NamedTempFile;
use zbrain_core::engine::{BrainEngine, EngineConfig, PageInput};
use zbrain_core::libsql::LibsqlEngine;
use zbrain_core::PageRef;

// ---------------------------------------------------------------------------
// libsql helpers
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

/// Insert a take row. Returns the auto-generated id (unused by callers but
/// ensures the INSERT succeeded). `active` maps to `SQLite` INTEGER 0/1.
async fn libsql_insert_take(tmp: &NamedTempFile, page_id: i64, active: bool) {
    let path_str = tmp.path().to_string_lossy().into_owned();
    let db = ::libsql::Builder::new_local(&path_str)
        .build()
        .await
        .expect("raw db open for take insert");
    let raw_conn = db.connect().expect("raw conn for take insert");
    raw_conn
        .execute(
            "INSERT INTO takes (page_id, active) VALUES (?1, ?2)",
            ::libsql::params![page_id, i64::from(active)],
        )
        .await
        .expect("insert take");
}

/// Look up the numeric page id by (`slug`, `source_id`).
async fn libsql_page_id(tmp: &NamedTempFile, slug: &str, source_id: &str) -> i64 {
    let path_str = tmp.path().to_string_lossy().into_owned();
    let db = ::libsql::Builder::new_local(&path_str)
        .build()
        .await
        .expect("raw db open for page_id lookup");
    let raw_conn = db.connect().expect("raw conn for page_id lookup");
    let mut rows = raw_conn
        .query(
            "SELECT id FROM pages WHERE slug = ?1 AND source_id = ?2",
            ::libsql::params![slug, source_id],
        )
        .await
        .expect("page_id query");
    let row = rows
        .next()
        .await
        .expect("page_id row")
        .expect("page_id present");
    row.get::<i64>(0).expect("page_id value")
}

// ---------------------------------------------------------------------------
// Invariant 1: N-takes positive formula (libsql)
// ---------------------------------------------------------------------------

#[tokio::test]
async fn libsql_salience_n_active_takes() {
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

    // Insert 2 active takes
    let pid = libsql_page_id(&tmp, "scored", "src-1").await;
    libsql_insert_take(&tmp, pid, true).await;
    libsql_insert_take(&tmp, pid, true).await;

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
    // 0.4 * 5 + ln(1 + 2) = 2.0 + ln(3)
    let expected = 0.4 * 5.0 + (1.0_f64 + 2.0_f64).ln();
    assert!(
        (score - expected).abs() < 1e-9,
        "libsql N-takes: expected ~{expected} (= 0.4*5 + ln(3)), got {score}"
    );
    engine.disconnect().await.expect("disconnect");
}

// ---------------------------------------------------------------------------
// Invariant 2: active=FALSE takes excluded (libsql)
// ---------------------------------------------------------------------------

#[tokio::test]
async fn libsql_salience_inactive_takes_excluded() {
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

    let pid = libsql_page_id(&tmp, "scored", "src-1").await;
    libsql_insert_take(&tmp, pid, true).await; // 1 active
    libsql_insert_take(&tmp, pid, false).await; // 1 inactive — must NOT count

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
    // 0.4 * 5 + ln(1 + 1) = 2.0 + ln(2) — inactive take excluded
    let expected = 0.4 * 5.0 + (1.0_f64 + 1.0_f64).ln();
    assert!(
        (score - expected).abs() < 1e-9,
        "libsql inactive-excluded: expected ~{expected} (= 0.4*5 + ln(2)), got {score}"
    );
    engine.disconnect().await.expect("disconnect");
}

// ---------------------------------------------------------------------------
// Invariant 3: cross-page isolation (libsql)
// ---------------------------------------------------------------------------

#[tokio::test]
async fn libsql_salience_cross_page_isolation() {
    let (engine, tmp) = libsql_init_clean_engine().await;
    libsql_seed_source(&tmp, "src-1").await;

    // Page A: no takes
    engine
        .put_page(
            "page-a",
            Some("src-1"),
            &PageInput {
                page_type: "note".to_string(),
                title: "A".to_string(),
                compiled_truth: "body".to_string(),
                ..PageInput::default()
            },
        )
        .await
        .expect("seed page-a");
    libsql_set_emotional_weight(&tmp, "page-a", "src-1", Some(0.4)).await;

    // Page B: 3 active takes
    engine
        .put_page(
            "page-b",
            Some("src-1"),
            &PageInput {
                page_type: "note".to_string(),
                title: "B".to_string(),
                compiled_truth: "body".to_string(),
                ..PageInput::default()
            },
        )
        .await
        .expect("seed page-b");
    libsql_set_emotional_weight(&tmp, "page-b", "src-1", Some(0.4)).await;

    let pid_b = libsql_page_id(&tmp, "page-b", "src-1").await;
    libsql_insert_take(&tmp, pid_b, true).await;
    libsql_insert_take(&tmp, pid_b, true).await;
    libsql_insert_take(&tmp, pid_b, true).await;

    let refs = vec![
        PageRef {
            slug: "page-a".to_string(),
            source_id: "src-1".to_string(),
        },
        PageRef {
            slug: "page-b".to_string(),
            source_id: "src-1".to_string(),
        },
    ];
    let scores = engine
        .get_salience_scores(&refs)
        .await
        .expect("get_salience_scores");

    // Page A: 0.4*5 + ln(1+0) = 2.0 — takes from B must not leak
    let score_a = *scores
        .get("src-1::page-a")
        .expect("missing src-1::page-a entry");
    let expected_a = 0.4 * 5.0;
    assert!(
        (score_a - expected_a).abs() < 1e-9,
        "libsql cross-page isolation: page-a expected ~{expected_a}, got {score_a}"
    );

    // Page B: 0.4*5 + ln(1+3) = 2.0 + ln(4)
    let score_b = *scores
        .get("src-1::page-b")
        .expect("missing src-1::page-b entry");
    let expected_b = 0.4 * 5.0 + (1.0_f64 + 3.0_f64).ln();
    assert!(
        (score_b - expected_b).abs() < 1e-9,
        "libsql cross-page isolation: page-b expected ~{expected_b}, got {score_b}"
    );
    engine.disconnect().await.expect("disconnect");
}

// ---------------------------------------------------------------------------
// PostgresEngine helpers
// ---------------------------------------------------------------------------

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

async fn pg_set_emotional_weight(url: &str, slug: &str, source_id: &str, weight: Option<f64>) {
    let pool = sqlx::postgres::PgPoolOptions::new()
        .max_connections(1)
        .connect(url)
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

async fn pg_insert_take(url: &str, page_id: i64, active: bool) {
    let pool = sqlx::postgres::PgPoolOptions::new()
        .max_connections(1)
        .connect(url)
        .await
        .expect("take insert pool");
    sqlx::query("INSERT INTO takes (page_id, active) VALUES ($1, $2)")
        .bind(page_id)
        .bind(active)
        .execute(&pool)
        .await
        .expect("insert take");
    pool.close().await;
}

async fn pg_page_id(url: &str, slug: &str, source_id: &str) -> i64 {
    let pool = sqlx::postgres::PgPoolOptions::new()
        .max_connections(1)
        .connect(url)
        .await
        .expect("page_id pool");
    let row: (i64,) = sqlx::query_as("SELECT id FROM pages WHERE slug = $1 AND source_id = $2")
        .bind(slug)
        .bind(source_id)
        .fetch_one(&pool)
        .await
        .expect("page_id row");
    pool.close().await;
    row.0
}

// ---------------------------------------------------------------------------
// Invariant 1: N-takes positive formula (Postgres)
// ---------------------------------------------------------------------------

#[tokio::test]
async fn postgres_salience_n_active_takes() {
    let fix = support::pg_fixture::PgFixture::start().await;
    let engine = &fix.engine;
    pg_seed_source(&fix.url, "src-1").await;
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
    pg_set_emotional_weight(&fix.url, "scored", "src-1", Some(0.4)).await;

    let pid = pg_page_id(&fix.url, "scored", "src-1").await;
    pg_insert_take(&fix.url, pid, true).await;
    pg_insert_take(&fix.url, pid, true).await;

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
    let expected = 0.4 * 5.0 + (1.0_f64 + 2.0_f64).ln();
    assert!(
        (score - expected).abs() < 1e-9,
        "pg N-takes: expected ~{expected} (= 0.4*5 + ln(3)), got {score}"
    );
}

// ---------------------------------------------------------------------------
// Invariant 2: active=FALSE takes excluded (Postgres)
// ---------------------------------------------------------------------------

#[tokio::test]
async fn postgres_salience_inactive_takes_excluded() {
    let fix = support::pg_fixture::PgFixture::start().await;
    let engine = &fix.engine;
    pg_seed_source(&fix.url, "src-1").await;
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
    pg_set_emotional_weight(&fix.url, "scored", "src-1", Some(0.4)).await;

    let pid = pg_page_id(&fix.url, "scored", "src-1").await;
    pg_insert_take(&fix.url, pid, true).await;
    pg_insert_take(&fix.url, pid, false).await;

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
    let expected = 0.4 * 5.0 + (1.0_f64 + 1.0_f64).ln();
    assert!(
        (score - expected).abs() < 1e-9,
        "pg inactive-excluded: expected ~{expected} (= 0.4*5 + ln(2)), got {score}"
    );
}

// ---------------------------------------------------------------------------
// Invariant 3: cross-page isolation (Postgres)
// ---------------------------------------------------------------------------

#[tokio::test]
async fn postgres_salience_cross_page_isolation() {
    let fix = support::pg_fixture::PgFixture::start().await;
    let engine = &fix.engine;
    pg_seed_source(&fix.url, "src-1").await;

    engine
        .put_page(
            "page-a",
            Some("src-1"),
            &PageInput {
                page_type: "note".to_string(),
                title: "A".to_string(),
                compiled_truth: "body".to_string(),
                ..PageInput::default()
            },
        )
        .await
        .expect("seed page-a");
    pg_set_emotional_weight(&fix.url, "page-a", "src-1", Some(0.4)).await;

    engine
        .put_page(
            "page-b",
            Some("src-1"),
            &PageInput {
                page_type: "note".to_string(),
                title: "B".to_string(),
                compiled_truth: "body".to_string(),
                ..PageInput::default()
            },
        )
        .await
        .expect("seed page-b");
    pg_set_emotional_weight(&fix.url, "page-b", "src-1", Some(0.4)).await;

    let pid_b = pg_page_id(&fix.url, "page-b", "src-1").await;
    pg_insert_take(&fix.url, pid_b, true).await;
    pg_insert_take(&fix.url, pid_b, true).await;
    pg_insert_take(&fix.url, pid_b, true).await;

    let refs = vec![
        PageRef {
            slug: "page-a".to_string(),
            source_id: "src-1".to_string(),
        },
        PageRef {
            slug: "page-b".to_string(),
            source_id: "src-1".to_string(),
        },
    ];
    let scores = engine
        .get_salience_scores(&refs)
        .await
        .expect("get_salience_scores");

    let score_a = *scores
        .get("src-1::page-a")
        .expect("missing src-1::page-a entry");
    let expected_a = 0.4 * 5.0;
    assert!(
        (score_a - expected_a).abs() < 1e-9,
        "pg cross-page isolation: page-a expected ~{expected_a}, got {score_a}"
    );

    let score_b = *scores
        .get("src-1::page-b")
        .expect("missing src-1::page-b entry");
    let expected_b = 0.4 * 5.0 + (1.0_f64 + 3.0_f64).ln();
    assert!(
        (score_b - expected_b).abs() < 1e-9,
        "pg cross-page isolation: page-b expected ~{expected_b}, got {score_b}"
    );
}
