//! Slice 6a TS-parity tests: `restore_page` libsql implementation.

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

async fn libsql_fetch_deleted_at(tmp: &NamedTempFile, slug: &str) -> Option<Option<String>> {
    let path_str = tmp.path().to_string_lossy().into_owned();
    let db = ::libsql::Builder::new_local(&path_str)
        .build()
        .await
        .expect("raw db open");
    let raw_conn = db.connect().expect("raw conn");
    let mut rows = raw_conn
        .query(
            "SELECT deleted_at FROM pages WHERE slug = ?1",
            ::libsql::params![slug],
        )
        .await
        .expect("inspect deleted_at");
    rows.next()
        .await
        .expect("deleted_at row fetch")
        .map(|row| row.get(0).expect("deleted_at decode"))
}

#[tokio::test]
async fn libsql_restore_page_clears_deleted_at_for_soft_deleted_row() {
    let (engine, tmp) = init_clean_engine().await;
    libsql_seed_source(&tmp, "src-1").await;
    engine
        .put_page(
            "to-restore",
            Some("src-1"),
            &PageInput {
                page_type: "note".to_string(),
                title: "To Restore".to_string(),
                compiled_truth: "body".to_string(),
                ..PageInput::default()
            },
        )
        .await
        .expect("seed page");
    engine
        .soft_delete_page("to-restore", Some("src-1"))
        .await
        .expect("soft delete prep");
    assert!(
        libsql_fetch_deleted_at(&tmp, "to-restore")
            .await
            .flatten()
            .is_some(),
        "precondition: row is soft deleted"
    );

    let restored = engine
        .restore_page("to-restore", Some("src-1"))
        .await
        .expect("restore_page");

    assert!(restored, "soft-deleted row should be restored");
    assert_eq!(
        libsql_fetch_deleted_at(&tmp, "to-restore").await.flatten(),
        None,
        "deleted_at must be cleared"
    );
    engine.disconnect().await.expect("disconnect");
}

#[tokio::test]
async fn libsql_restore_page_returns_false_for_live_or_missing_rows() {
    let (engine, tmp) = init_clean_engine().await;
    libsql_seed_source(&tmp, "src-1").await;
    engine
        .put_page(
            "live-row",
            Some("src-1"),
            &PageInput {
                page_type: "note".to_string(),
                title: "Live".to_string(),
                compiled_truth: "body".to_string(),
                ..PageInput::default()
            },
        )
        .await
        .expect("seed page");

    let live_restore = engine
        .restore_page("live-row", Some("src-1"))
        .await
        .expect("restore live row");
    let missing_restore = engine
        .restore_page("never-existed", Some("src-1"))
        .await
        .expect("restore missing row");

    assert!(!live_restore, "live row must not be restored");
    assert!(!missing_restore, "missing row must not be restored");
    assert_eq!(
        libsql_fetch_deleted_at(&tmp, "live-row").await.flatten(),
        None,
        "live row stays live (deleted_at remains NULL)"
    );
    engine.disconnect().await.expect("disconnect");
}

#[tokio::test]
async fn libsql_restore_page_honors_source_id_filter() {
    let (engine, tmp) = init_clean_engine().await;
    libsql_seed_source(&tmp, "src-1").await;
    engine
        .put_page(
            "scoped-restore",
            Some("src-1"),
            &PageInput {
                page_type: "note".to_string(),
                title: "Scoped".to_string(),
                compiled_truth: "body".to_string(),
                ..PageInput::default()
            },
        )
        .await
        .expect("seed page");
    engine
        .soft_delete_page("scoped-restore", Some("src-1"))
        .await
        .expect("soft delete prep");

    let mismatched = engine
        .restore_page("scoped-restore", Some("src-2"))
        .await
        .expect("restore with mismatched source");

    assert!(!mismatched, "source mismatch must not restore");
    assert!(
        libsql_fetch_deleted_at(&tmp, "scoped-restore")
            .await
            .flatten()
            .is_some(),
        "source mismatch must leave deleted_at set"
    );
    engine.disconnect().await.expect("disconnect");
}

// ---------------------------------------------------------------------------
// PostgresEngine mirror tests (slice 6a-pg PG-soft-delete: restore_page)
//
// Mirrors TS `restorePage` from `pglite-engine.ts`: a soft-deleted row gets
// its `deleted_at` cleared and the call returns `true`. Live rows and rows
// from another source must remain untouched and return `false`.
// ---------------------------------------------------------------------------

use zbrain_core::postgres::PostgresEngine;

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

async fn pg_fetch_deleted_at(
    slug: &str,
) -> Option<Option<sqlx::types::chrono::DateTime<sqlx::types::chrono::Utc>>> {
    let url = pg_url().expect("ZBRAIN_TEST_PG_URL set for inspect");
    let pool = sqlx::postgres::PgPoolOptions::new()
        .max_connections(1)
        .connect(&url)
        .await
        .expect("inspect pool");
    let row: Option<(Option<sqlx::types::chrono::DateTime<sqlx::types::chrono::Utc>>,)> =
        sqlx::query_as("SELECT deleted_at FROM pages WHERE slug = $1")
            .bind(slug)
            .fetch_optional(&pool)
            .await
            .expect("inspect deleted_at");
    pool.close().await;
    row.map(|r| r.0)
}

#[tokio::test]
#[serial_test::serial]
async fn postgres_restore_page_clears_deleted_at_for_soft_deleted_row() {
    let Some(engine) = pg_init_clean_engine().await else {
        eprintln!("skipping: ZBRAIN_TEST_PG_URL unset");
        return;
    };
    pg_seed_source("src-1").await;
    engine
        .put_page(
            "to-restore",
            Some("src-1"),
            &PageInput {
                page_type: "note".to_string(),
                title: "To Restore".to_string(),
                compiled_truth: "body".to_string(),
                ..PageInput::default()
            },
        )
        .await
        .expect("seed page");
    // Soft delete via the engine API so we exercise the real timestamp path.
    engine
        .soft_delete_page("to-restore", Some("src-1"))
        .await
        .expect("soft delete prep");
    assert!(
        pg_fetch_deleted_at("to-restore").await.flatten().is_some(),
        "precondition: row is soft deleted"
    );

    let restored = engine
        .restore_page("to-restore", Some("src-1"))
        .await
        .expect("restore_page");

    assert!(restored, "soft-deleted row should be restored");
    assert_eq!(
        pg_fetch_deleted_at("to-restore").await.flatten(),
        None,
        "deleted_at must be cleared"
    );
    engine.disconnect().await.expect("disconnect");
}

#[tokio::test]
#[serial_test::serial]
async fn postgres_restore_page_returns_false_for_live_or_missing_rows() {
    let Some(engine) = pg_init_clean_engine().await else {
        eprintln!("skipping: ZBRAIN_TEST_PG_URL unset");
        return;
    };
    pg_seed_source("src-1").await;
    engine
        .put_page(
            "live-row",
            Some("src-1"),
            &PageInput {
                page_type: "note".to_string(),
                title: "Live".to_string(),
                compiled_truth: "body".to_string(),
                ..PageInput::default()
            },
        )
        .await
        .expect("seed page");

    let live_restore = engine
        .restore_page("live-row", Some("src-1"))
        .await
        .expect("restore live row");
    let missing_restore = engine
        .restore_page("never-existed", Some("src-1"))
        .await
        .expect("restore missing row");

    assert!(!live_restore, "live row must not be restored");
    assert!(!missing_restore, "missing row must not be restored");
    assert_eq!(
        pg_fetch_deleted_at("live-row").await.flatten(),
        None,
        "live row stays live (deleted_at remains NULL)"
    );
    engine.disconnect().await.expect("disconnect");
}

#[tokio::test]
#[serial_test::serial]
async fn postgres_restore_page_honors_source_id_filter() {
    let Some(engine) = pg_init_clean_engine().await else {
        eprintln!("skipping: ZBRAIN_TEST_PG_URL unset");
        return;
    };
    pg_seed_source("src-1").await;
    engine
        .put_page(
            "scoped-restore",
            Some("src-1"),
            &PageInput {
                page_type: "note".to_string(),
                title: "Scoped".to_string(),
                compiled_truth: "body".to_string(),
                ..PageInput::default()
            },
        )
        .await
        .expect("seed page");
    engine
        .soft_delete_page("scoped-restore", Some("src-1"))
        .await
        .expect("soft delete prep");

    let mismatched = engine
        .restore_page("scoped-restore", Some("src-2"))
        .await
        .expect("restore with mismatched source");

    assert!(!mismatched, "source mismatch must not restore");
    assert!(
        pg_fetch_deleted_at("scoped-restore")
            .await
            .flatten()
            .is_some(),
        "source mismatch must leave deleted_at set"
    );
    engine.disconnect().await.expect("disconnect");
}
