//! Slice 4a — `PostgresEngine` lifecycle integration tests.
//!
//! Gated on the `ZBRAIN_TEST_PG_URL` environment variable. When unset (CI
//! without Postgres, fresh clones), each test prints a skip notice and exits
//! green — the suite stays buildable everywhere. To exercise the tests:
//!
//! ```sh
//! # From the Rust worktree root. Reuses the TS worktree compose file.
//! docker-compose -f docker-compose.test.yml up -d
//! ZBRAIN_TEST_PG_URL=postgres://postgres:postgres@localhost:5434/gbrain_test \
//!   cargo test -p zbrain-core --test postgres_engine_lifecycle
//! ```

use zbrain_core::engine::{BrainEngine, EngineConfig, EngineKind};
use zbrain_core::postgres::PostgresEngine;

fn pg_url() -> Option<String> {
    std::env::var("ZBRAIN_TEST_PG_URL").ok()
}

#[tokio::test]
async fn kind_reports_postgres() {
    let engine = PostgresEngine::new();
    assert_eq!(engine.kind(), EngineKind::Postgres);
}

#[tokio::test]
async fn connect_succeeds_against_live_postgres() {
    let Some(url) = pg_url() else {
        eprintln!("skipping: ZBRAIN_TEST_PG_URL unset");
        return;
    };
    let engine = PostgresEngine::new();
    let cfg = EngineConfig {
        database_url: Some(url),
        database_path: None,
    };
    engine.connect(&cfg).await.expect("connect should succeed");
    engine.disconnect().await.expect("disconnect should succeed");
}

#[tokio::test]
async fn connect_without_url_errors() {
    let engine = PostgresEngine::new();
    let cfg = EngineConfig::default();
    let result = engine.connect(&cfg).await;
    assert!(
        result.is_err(),
        "connect without database_url must error, got {result:?}"
    );
}

#[tokio::test]
async fn init_schema_creates_pages_and_sources_tables() {
    let Some(url) = pg_url() else {
        eprintln!("skipping: ZBRAIN_TEST_PG_URL unset");
        return;
    };
    let engine = PostgresEngine::new();
    let cfg = EngineConfig {
        database_url: Some(url.clone()),
        database_path: None,
    };
    engine.connect(&cfg).await.expect("connect");
    engine.init_schema().await.expect("init_schema");

    // Verify schema landed by talking directly to PG through a fresh pool.
    // Avoids leaking internal state out of PostgresEngine.
    let pool = sqlx::postgres::PgPoolOptions::new()
        .max_connections(1)
        .connect(&url)
        .await
        .expect("verification pool");

    let pages_exists: (bool,) = sqlx::query_as(
        "SELECT EXISTS (SELECT 1 FROM information_schema.tables WHERE table_name = 'pages')",
    )
    .fetch_one(&pool)
    .await
    .expect("pages table existence check");
    assert!(pages_exists.0, "pages table must exist after init_schema");

    let sources_exists: (bool,) = sqlx::query_as(
        "SELECT EXISTS (SELECT 1 FROM information_schema.tables WHERE table_name = 'sources')",
    )
    .fetch_one(&pool)
    .await
    .expect("sources table existence check");
    assert!(sources_exists.0, "sources table must exist after init_schema");

    let default_source: (String,) =
        sqlx::query_as("SELECT id FROM sources WHERE id = 'default'")
            .fetch_one(&pool)
            .await
            .expect("default source row must be seeded");
    assert_eq!(default_source.0, "default");

    engine.disconnect().await.expect("disconnect");
    pool.close().await;
}

#[tokio::test]
async fn init_schema_is_idempotent() {
    let Some(url) = pg_url() else {
        eprintln!("skipping: ZBRAIN_TEST_PG_URL unset");
        return;
    };
    let engine = PostgresEngine::new();
    let cfg = EngineConfig {
        database_url: Some(url),
        database_path: None,
    };
    engine.connect(&cfg).await.expect("connect");
    engine.init_schema().await.expect("first init_schema");
    engine
        .init_schema()
        .await
        .expect("second init_schema must be a no-op");
    engine.disconnect().await.expect("disconnect");
}
