//! Slice 4a — `PostgresEngine` lifecycle integration tests.
//!
//! Uses `pg-embed` to launch an ephemeral `PostgreSQL` instance per test.
//! No external `PostgreSQL` or Docker installation is required.

mod support;

use zbrain_core::engine::{BrainEngine, EngineConfig, EngineKind};
use zbrain_core::postgres::PostgresEngine;

#[tokio::test]
async fn kind_reports_postgres() {
    let engine = PostgresEngine::new();
    assert_eq!(engine.kind(), EngineKind::Postgres);
}

#[tokio::test]
async fn connect_succeeds_against_live_postgres() {
    let mut fix = support::pg_fixture::PgFixture::start().await;
    // PgFixture already connected and init_schema'd.
    // Verify disconnect works.
    let engine = std::mem::replace(&mut fix.engine, PostgresEngine::new());
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
    let fix = support::pg_fixture::PgFixture::start().await;
    // init_schema already called by PgFixture. Verify tables exist.
    let pool = sqlx::postgres::PgPoolOptions::new()
        .max_connections(1)
        .connect(&fix.url)
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
    assert!(
        sources_exists.0,
        "sources table must exist after init_schema"
    );

    let default_source: (String,) = sqlx::query_as("SELECT id FROM sources WHERE id = 'default'")
        .fetch_one(&pool)
        .await
        .expect("default source row must be seeded");
    assert_eq!(default_source.0, "default");

    pool.close().await;
}

#[tokio::test]
async fn init_schema_is_idempotent() {
    let fix = support::pg_fixture::PgFixture::start().await;
    fix.engine
        .init_schema()
        .await
        .expect("second init_schema must be a no-op");
}
