//! Slice 6a S6-T1 placeholder-lock test: `get_page_timestamps` placeholder lock.
//!
//! Slice 6a-pg (PG-advanced-reads S2 RED) appends `PostgresEngine` mirror
//! tests at the bottom of this file. They lock the PG semantics agreed in
//! plan 14 §11.1:
//!   `SELECT slug, COALESCE(updated_at, created_at)::text AS ts FROM pages
//!    WHERE slug = ANY($1::text[]) AND deleted_at IS NULL`
//! returning a `HashMap<String, String>` keyed by slug. Soft-deleted rows
//! are excluded; missing slugs are silently dropped from the result.

use tempfile::NamedTempFile;
use zbrain_core::engine::{BrainEngine, EngineConfig, PageInput};
use zbrain_core::libsql::LibsqlEngine;

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
async fn slice_6a_page_methods_get_page_timestamps_returns_unsupported() {
    let (engine, _tmp) = init_clean_engine().await;
    let slugs = vec!["a".to_string(), "b".to_string()];
    let err = engine
        .get_page_timestamps(&slugs)
        .await
        .expect_err("6a placeholder-lock: get_page_timestamps must be Unsupported");
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
// Locks the PG `get_page_timestamps` semantics from plan 14 §11.1:
//   SELECT slug, COALESCE(updated_at, created_at)::text AS ts
//   FROM pages
//   WHERE slug = ANY($1::text[]) AND deleted_at IS NULL
// (i.e. keyed by slug; soft-deleted rows excluded; missing slugs silently
// dropped.) Gated on `ZBRAIN_TEST_PG_URL`. `#[serial_test::serial]` because
// they share the `pages` table in the configured test database.
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

#[tokio::test]
#[serial_test::serial]
async fn postgres_get_page_timestamps_returns_iso_ts_for_each_existing_slug() {
    let Some(engine) = pg_init_clean_engine().await else {
        eprintln!("skipping: ZBRAIN_TEST_PG_URL unset");
        return;
    };
    pg_seed_source("src-1").await;
    for slug in ["alpha", "beta"] {
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

    let stamps = engine
        .get_page_timestamps(&["alpha".to_string(), "beta".to_string()])
        .await
        .expect("get_page_timestamps");

    assert_eq!(stamps.len(), 2, "two rows requested → two entries");
    for slug in ["alpha", "beta"] {
        let ts = stamps.get(slug).unwrap_or_else(|| panic!("missing {slug}"));
        // ISO-8601 timestamps from PG `::text` cast always start with a 4-digit year.
        assert!(
            ts.len() >= 10 && ts.starts_with("20"),
            "expected ISO-8601 ts for {slug}, got {ts}"
        );
    }
    engine.disconnect().await.expect("disconnect");
}

#[tokio::test]
#[serial_test::serial]
async fn postgres_get_page_timestamps_excludes_soft_deleted_rows() {
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
    engine
        .soft_delete_page("tombstone-slug", Some("src-1"))
        .await
        .expect("soft delete tombstone");

    let stamps = engine
        .get_page_timestamps(&["live-slug".to_string(), "tombstone-slug".to_string()])
        .await
        .expect("get_page_timestamps");

    assert!(stamps.contains_key("live-slug"));
    assert!(
        !stamps.contains_key("tombstone-slug"),
        "soft-deleted rows must be excluded by `deleted_at IS NULL` filter"
    );
    assert_eq!(stamps.len(), 1);
    engine.disconnect().await.expect("disconnect");
}

#[tokio::test]
#[serial_test::serial]
async fn postgres_get_page_timestamps_silently_drops_missing_slugs() {
    let Some(engine) = pg_init_clean_engine().await else {
        eprintln!("skipping: ZBRAIN_TEST_PG_URL unset");
        return;
    };
    pg_seed_source("src-1").await;
    engine
        .put_page(
            "only-slug",
            Some("src-1"),
            &PageInput {
                page_type: "note".to_string(),
                title: "Only".to_string(),
                compiled_truth: "body".to_string(),
                ..PageInput::default()
            },
        )
        .await
        .expect("seed page");

    let stamps = engine
        .get_page_timestamps(&[
            "only-slug".to_string(),
            "does-not-exist".to_string(),
            "neither-does-this".to_string(),
        ])
        .await
        .expect("get_page_timestamps");

    assert_eq!(stamps.len(), 1, "missing slugs are silently dropped");
    assert!(stamps.contains_key("only-slug"));
    engine.disconnect().await.expect("disconnect");
}

#[tokio::test]
#[serial_test::serial]
async fn postgres_get_page_timestamps_returns_empty_map_for_empty_input() {
    let Some(engine) = pg_init_clean_engine().await else {
        eprintln!("skipping: ZBRAIN_TEST_PG_URL unset");
        return;
    };

    let stamps = engine
        .get_page_timestamps(&[])
        .await
        .expect("get_page_timestamps on empty input");
    assert!(stamps.is_empty(), "empty slugs slice → empty map");
    engine.disconnect().await.expect("disconnect");
}
