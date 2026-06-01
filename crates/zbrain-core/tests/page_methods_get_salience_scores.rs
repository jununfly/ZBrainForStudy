//! Slice 6a-libsql advanced reads (S2 RED): `get_salience_scores` behavioural tests.
//!
//! Flips the prior S6-T1 placeholder-lock test into 4 positive libsql tests
//! mirroring the PG mirror tests below. Locks the libsql 6a-quirk formula
//!   `score = COALESCE(emotional_weight, 0.0) * 5.0`
//! (the `+ ln(1 + COUNT DISTINCT take)` term is hard-coded to 0 until 6c
//! lands the `takes` table; that quirk is locked separately by the sibling
//! test `page_methods_salience_scores_takes_zero_until_6c.rs`).
//!
//! Result keyed by `format!("{source_id}::{slug}")`, soft-deleted rows
//! filtered by `deleted_at IS NULL`, empty input -> empty map.
//!
//! `PageInput` does NOT carry `emotional_weight` (it is computed by the
//! `recompute_emotional_weight` pipeline). To exercise the score arithmetic
//! here we `put_page` first, then directly UPDATE the column via a raw
//! libsql connection (`libsql_set_emotional_weight` helper).
//!
//! Slice 6a-pg (PG-advanced-reads S2 RED) appends `PostgresEngine` mirror
//! tests at the bottom of this file; they lock plan 14 §11.1 PG SQL.

mod support;

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

async fn libsql_seed_source(tmp: &NamedTempFile, id: &str) {
    let path_str = tmp.path().to_string_lossy().into_owned();
    let db = ::libsql::Builder::new_local(&path_str)
        .build()
        .await
        .expect("raw db open");
    let raw_conn = db.connect().expect("raw conn");
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
        .expect("raw db open");
    let raw_conn = db.connect().expect("raw conn");
    raw_conn
        .execute(
            "UPDATE pages SET emotional_weight = ?1 WHERE slug = ?2 AND source_id = ?3",
            ::libsql::params![weight, slug, source_id],
        )
        .await
        .expect("update emotional_weight");
}

fn note_input(title: &str, body: &str) -> PageInput {
    PageInput {
        page_type: "note".to_string(),
        title: title.to_string(),
        compiled_truth: body.to_string(),
        ..PageInput::default()
    }
}

fn assert_close_libsql(actual: f64, expected: f64, label: &str) {
    assert!(
        (actual - expected).abs() < 1e-9,
        "{label}: expected ~{expected}, got {actual}"
    );
}

#[tokio::test]
async fn libsql_get_salience_scores_returns_score_for_each_ref() {
    let (engine, tmp) = init_clean_engine().await;
    libsql_seed_source(&tmp, "src-1").await;
    libsql_seed_source(&tmp, "src-2").await;
    for (slug, src) in [("alpha", "src-1"), ("beta", "src-2")] {
        engine
            .put_page(slug, Some(src), &note_input(slug, "body"))
            .await
            .expect("seed page");
    }
    libsql_set_emotional_weight(&tmp, "alpha", "src-1", Some(0.4)).await;
    libsql_set_emotional_weight(&tmp, "beta", "src-2", Some(1.0)).await;

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

    assert_eq!(scores.len(), 2, "two refs requested -> two entries");
    assert_close_libsql(
        *scores.get("src-1::alpha").expect("missing src-1::alpha"),
        0.4 * 5.0,
        "alpha score",
    );
    assert_close_libsql(
        *scores.get("src-2::beta").expect("missing src-2::beta"),
        1.0 * 5.0,
        "beta score",
    );
    engine.disconnect().await.expect("disconnect");
}

// NOTE: `libsql_get_salience_scores_treats_null_emotional_weight_as_zero` was
// dropped on 2026-05-31. Reason: the libsql `pages.emotional_weight` column is
// declared `REAL NOT NULL DEFAULT 0.0` (migrations-sqlite/0002_pages_full_columns.sql
// L33), matching TS parity (TS schema-embedded / pglite-schema / migrate.ts all
// pin NOT NULL DEFAULT 0.0). The schema makes it impossible to land a NULL
// here, so a "treats NULL as zero" test asserts a state the schema forbids and
// only succeeds in tripping `NOT NULL constraint failed` in the setup helper.
// The `COALESCE(emotional_weight, 0.0)` wrapper in the libsql query stays as
// defensive code, but there is no in-band path that produces NULL to exercise.
// If a future slice relaxes the column to nullable (to mirror the PG-side
// `emotional_weight DOUBLE PRECISION` quirk in migrations/0003), reintroduce
// this test then.

#[tokio::test]
async fn libsql_get_salience_scores_excludes_soft_deleted_rows() {
    let (engine, tmp) = init_clean_engine().await;
    libsql_seed_source(&tmp, "src-1").await;
    for slug in ["live-slug", "tombstone-slug"] {
        engine
            .put_page(slug, Some("src-1"), &note_input(slug, "body"))
            .await
            .expect("seed page");
    }
    libsql_set_emotional_weight(&tmp, "live-slug", "src-1", Some(0.2)).await;
    libsql_set_emotional_weight(&tmp, "tombstone-slug", "src-1", Some(0.9)).await;
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
    assert_close_libsql(
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
async fn libsql_get_salience_scores_returns_empty_map_for_empty_input() {
    let (engine, _tmp) = init_clean_engine().await;
    let scores = engine
        .get_salience_scores(&[])
        .await
        .expect("get_salience_scores on empty input");
    assert!(scores.is_empty(), "empty refs slice -> empty map");
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
// Uses `PgFixture::start()` to spin up an ephemeral pg-embed instance per
// test, so tests no longer share state and `#[serial_test::serial]` is gone.
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

fn assert_close(actual: f64, expected: f64, label: &str) {
    assert!(
        (actual - expected).abs() < 1e-9,
        "{label}: expected ~{expected}, got {actual}"
    );
}

#[tokio::test]
async fn postgres_get_salience_scores_returns_score_for_each_ref() {
    let fix = support::pg_fixture::PgFixture::start().await;
    let engine = &fix.engine;
    pg_seed_source(&fix.url, "src-1").await;
    pg_seed_source(&fix.url, "src-2").await;
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
    pg_set_emotional_weight(&fix.url, "alpha", "src-1", Some(0.4)).await;
    pg_set_emotional_weight(&fix.url, "beta", "src-2", Some(1.0)).await;

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
}

#[tokio::test]
async fn postgres_get_salience_scores_treats_null_emotional_weight_as_zero() {
    // Brand-new pages start with emotional_weight = NULL (the recompute
    // pipeline lands later). The COALESCE(..., 0.0) wrapper MUST collapse
    // NULL → 0.0 so the score is exactly 0.0, NOT a propagated NULL / NaN /
    // dropped key.
    let fix = support::pg_fixture::PgFixture::start().await;
    let engine = &fix.engine;
    pg_seed_source(&fix.url, "src-1").await;
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
    pg_set_emotional_weight(&fix.url, "fresh", "src-1", None).await;

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
}

#[tokio::test]
async fn postgres_get_salience_scores_excludes_soft_deleted_rows() {
    let fix = support::pg_fixture::PgFixture::start().await;
    let engine = &fix.engine;
    pg_seed_source(&fix.url, "src-1").await;
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
    pg_set_emotional_weight(&fix.url, "live-slug", "src-1", Some(0.2)).await;
    pg_set_emotional_weight(&fix.url, "tombstone-slug", "src-1", Some(0.9)).await;
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
}

#[tokio::test]
async fn postgres_get_salience_scores_returns_empty_map_for_empty_input() {
    let fix = support::pg_fixture::PgFixture::start().await;
    let engine = &fix.engine;

    let scores = engine
        .get_salience_scores(&[])
        .await
        .expect("get_salience_scores on empty input");
    assert!(scores.is_empty(), "empty refs slice → empty map");
}
