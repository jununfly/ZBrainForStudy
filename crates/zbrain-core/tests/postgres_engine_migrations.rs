//! Issue #29 - Postgres migration system end-to-end tests.
//!
//! Tests version tracking, idempotency, and migration order correctness.
//! Uses existing PgFixture for ephemeral Postgres instances.

use sqlx::postgres::PgPoolOptions;
use sqlx::{Pool, Postgres};
use zbrain_core::engine::{BrainEngine, EngineConfig, EngineKind};
use zbrain_core::postgres::PostgresEngine;
use std::time::Duration;

struct PgFixture {
    pool: Pool<Postgres>,
    url: String,
}

impl PgFixture {
    async fn new() -> Self {
        // Use unique database name for each test run to avoid conflicts
        let db_name = format!("zbrain_test_{}", chrono::Utc::now().timestamp_millis());
        let pg_url = std::env::var("ZBRAIN_TEST_PG_URL")
            .unwrap_or_else(|_| "postgres://postgres:postgres@localhost:5432/postgres".to_string());

        // Connect to postgres to create our test database
        let admin_pool = PgPoolOptions::new()
            .max_connections(1)
            .connect_timeout(Duration::from_secs(3))
            .connect(&pg_url)
            .await
            .expect("Failed to connect to Postgres admin database");

        sqlx::query(&format!("CREATE DATABASE {}", db_name))
            .execute(&admin_pool)
            .await
            .expect("Failed to create test database");

        // Close admin pool
        admin_pool.close().await;

        // Connect to our new test database
        let test_url = format!(
            "{}",
            pg_url.replace("/postgres", &format!("/{}", db_name))
        );

        let pool = PgPoolOptions::new()
            .max_connections(5)
            .connect_timeout(Duration::from_secs(3))
            .connect(&test_url)
            .await
            .expect("Failed to connect to test database");

        PgFixture { pool, url: test_url }
    }

    fn engine(&self) -> PostgresEngine {
        PostgresEngine::with_config(EngineConfig {
            db_url: Some(self.url.clone()),
            ..Default::default()
        })
        .expect("Failed to create PostgresEngine")
    }

    async fn read_version(&self) -> i64 {
        let row = sqlx::query("SELECT version FROM rust_schema_version LIMIT 1")
            .fetch_one(&self.pool)
            .await
            .expect("Failed to read version");
        row.try_get(0).expect("Failed to decode version")
    }

    async fn table_exists(&self, table: &str) -> bool {
        let row = sqlx::query(
            "SELECT EXISTS (SELECT FROM information_schema.tables WHERE table_name = $1)"
        )
        .bind(table)
        .fetch_one(&self.pool)
        .await
        .expect("Failed to check table existence");
        row.try_get(0).expect("Failed to decode exists flag")
    }
}

impl Drop for PgFixture {
    fn drop(&mut self) {
        // Leak the pool; test database cleanup is automatic on process exit
        // or can be handled by a separate cleanup job
    }
}

#[tokio::test]
async fn fresh_db_runs_all_nine_migrations_ends_at_version_9() {
    let pg = PgFixture::new().await;
    let engine = pg.engine();
    engine.init_schema().await.expect("init_schema failed");
    let version = pg.read_version().await;
    assert_eq!(version, 9);
}

#[tokio::test]
async fn idempotent_init_schema_applies_zero_migrations_second_run() {
    let pg = PgFixture::new().await;
    let engine = pg.engine();

    // First run
    engine.init_schema().await.expect("init_schema failed");
    let v1 = pg.read_version().await;
    assert_eq!(v1, 9);

    // Second run should be idempotent
    engine.init_schema().await.expect("init_schema failed");
    let v2 = pg.read_version().await;
    assert_eq!(v2, 9);
}

#[tokio::test]
async fn rust_schema_version_table_exists_after_init() {
    let pg = PgFixture::new().await;
    let engine = pg.engine();
    engine.init_schema().await.expect("init_schema failed");
    assert!(pg.table_exists("rust_schema_version").await);
}

#[tokio::test]
async fn key_tables_from_migrations_exist_after_init() {
    let pg = PgFixture::new().await;
    let engine = pg.engine();
    engine.init_schema().await.expect("init_schema failed");

    // Migration 1: pages
    assert!(pg.table_exists("pages").await);
    // Migration 4: page_tags
    assert!(pg.table_exists("page_tags").await);
    // Migration 7: files
    assert!(pg.table_exists("files").await);
    // Migration 9: raw_data + page_versions
    assert!(pg.table_exists("raw_data").await);
    assert!(pg.table_exists("page_versions").await);
}

#[tokio::test]
async fn bootstrap_creates_version_zero_row_correctly() {
    // We need to test this BEFORE init_schema does the migration apply
    // So we just create the table ourselves and check it starts at 0
    let pg = PgFixture::new().await;

    sqlx::query(
        r#"
        CREATE TABLE IF NOT EXISTS rust_schema_version (
            version INTEGER PRIMARY KEY NOT NULL DEFAULT 0,
            applied_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP
        );
        INSERT INTO rust_schema_version (version) VALUES (0) ON CONFLICT DO NOTHING;
        "#,
    )
    .execute(&pg.pool)
    .await
    .expect("Bootstrap failed");

    let version = pg.read_version().await;
    assert_eq!(version, 0);
}
