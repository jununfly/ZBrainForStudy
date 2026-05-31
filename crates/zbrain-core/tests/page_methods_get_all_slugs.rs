//! Slice 6a S6-T1 placeholder-lock test: `get_all_slugs` placeholder lock.
//!
//! Slice 6a-pg (PG-advanced-reads S2 RED) appends `PostgresEngine` mirror
//! tests at the bottom of this file. They lock the PG semantics agreed in
//! plan 14 §11.1: `SELECT slug FROM pages [WHERE source_id = $1]` returning
//! a `HashSet<String>` of *all* rows including soft-deleted ones (matches
//! TS `pglite-engine.ts` L1071-1086 which does NOT filter `deleted_at`).

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
async fn slice_6a_page_methods_get_all_slugs_returns_unsupported() {
    let (engine, _tmp) = init_clean_engine().await;
    let err = engine
        .get_all_slugs(None)
        .await
        .expect_err("6a placeholder-lock: get_all_slugs must be Unsupported");
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
// Locks the PG `get_all_slugs` semantics from plan 14 §11.1:
//   SELECT slug FROM pages [WHERE source_id = $1]
//   ($1::text IS NULL OR source_id = $1) guard, returns HashSet<String>.
//   Per TS `pglite-engine.ts` L1071-1086, results include soft-deleted
//   rows (NO `deleted_at IS NULL` filter).
// Gated on `ZBRAIN_TEST_PG_URL`. `#[serial_test::serial]` because they
// share the `pages` table in the configured test database.
// ---------------------------------------------------------------------------

use zbrain_core::postgres::PostgresEngine;

// LOCAL PG NOTE (do not forget): a working PG instance IS available on this
// workstation — Homebrew PostgreSQL 16.14 listens on `localhost:5434`, the
// database `zbrain_test` already exists, and the URL is persisted in the
// gitignored `<repo-root>/.env`:
//   ZBRAIN_TEST_PG_URL=postgres://postgres:postgres@localhost:5434/zbrain_test
// Activate before `cargo test`:
//   set -a; source .env; set +a
// Do NOT treat `skipping: ZBRAIN_TEST_PG_URL unset` as a valid PG result —
// it only means the shell forgot to source `.env`. See
// docs/plans/20260526/17-session-state-110c.md L139-150 for the original
// provisioning record (#110-c, 2026-05-30).
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

#[tokio::test]
#[serial_test::serial]
async fn postgres_get_all_slugs_returns_every_slug_across_sources() {
    let Some(engine) = pg_init_clean_engine().await else {
        eprintln!("skipping: ZBRAIN_TEST_PG_URL unset");
        return;
    };
    pg_seed_source("src-1").await;
    pg_seed_source("src-2").await;
    for (slug, src) in [("alpha", "src-1"), ("beta", "src-1"), ("gamma", "src-2")] {
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

    let slugs = engine
        .get_all_slugs(None)
        .await
        .expect("get_all_slugs(None)");

    let expected: std::collections::HashSet<String> = ["alpha", "beta", "gamma"]
        .iter()
        .map(std::string::ToString::to_string)
        .collect();
    assert_eq!(slugs, expected);
    engine.disconnect().await.expect("disconnect");
}

#[tokio::test]
#[serial_test::serial]
async fn postgres_get_all_slugs_filters_by_source_id_when_provided() {
    let Some(engine) = pg_init_clean_engine().await else {
        eprintln!("skipping: ZBRAIN_TEST_PG_URL unset");
        return;
    };
    pg_seed_source("src-1").await;
    pg_seed_source("src-2").await;
    for (slug, src) in [("alpha", "src-1"), ("beta", "src-1"), ("gamma", "src-2")] {
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

    let scoped = engine
        .get_all_slugs(Some("src-1"))
        .await
        .expect("get_all_slugs(src-1)");

    let expected: std::collections::HashSet<String> = ["alpha", "beta"]
        .iter()
        .map(std::string::ToString::to_string)
        .collect();
    assert_eq!(scoped, expected);
    engine.disconnect().await.expect("disconnect");
}

#[tokio::test]
#[serial_test::serial]
async fn postgres_get_all_slugs_includes_soft_deleted_rows() {
    // TS `pglite-engine.ts` L1071-1086 does NOT filter `deleted_at IS NULL`;
    // PG mirror must keep that quirk so analytics queries see every slug
    // ever written (logged as PG-advanced-reads R1/R2 in plan §11.1).
    let Some(engine) = pg_init_clean_engine().await else {
        eprintln!("skipping: ZBRAIN_TEST_PG_URL unset");
        return;
    };
    pg_seed_source("src-1").await;
    engine
        .put_page(
            "live-slug",
            Some("src-1"),
            &PageInput {
                page_type: "note".to_string(),
                title: "Live".to_string(),
                compiled_truth: "body".to_string(),
                ..PageInput::default()
            },
        )
        .await
        .expect("seed live");
    engine
        .put_page(
            "tombstone-slug",
            Some("src-1"),
            &PageInput {
                page_type: "note".to_string(),
                title: "Tombstone".to_string(),
                compiled_truth: "body".to_string(),
                ..PageInput::default()
            },
        )
        .await
        .expect("seed tombstone");
    engine
        .soft_delete_page("tombstone-slug", Some("src-1"))
        .await
        .expect("soft delete tombstone");

    let slugs = engine
        .get_all_slugs(None)
        .await
        .expect("get_all_slugs(None)");

    assert!(slugs.contains("live-slug"));
    assert!(
        slugs.contains("tombstone-slug"),
        "PG `get_all_slugs` must include soft-deleted rows (TS parity)"
    );
    engine.disconnect().await.expect("disconnect");
}

#[tokio::test]
#[serial_test::serial]
async fn postgres_get_all_slugs_returns_empty_set_when_no_rows_match() {
    let Some(engine) = pg_init_clean_engine().await else {
        eprintln!("skipping: ZBRAIN_TEST_PG_URL unset");
        return;
    };
    let empty = engine
        .get_all_slugs(None)
        .await
        .expect("get_all_slugs on empty");
    assert!(empty.is_empty());

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

    let missing = engine
        .get_all_slugs(Some("src-missing"))
        .await
        .expect("get_all_slugs(src-missing)");
    assert!(missing.is_empty());
    engine.disconnect().await.expect("disconnect");
}
