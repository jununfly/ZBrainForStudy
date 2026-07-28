//! Slice 6a S6-T3 semantic tests: `soft_delete_page`.

mod support;

use serde_json::json;
use tempfile::NamedTempFile;
use zbrain_core::engine::{BrainEngine, EngineConfig, GetPageOpts, InMemoryEngine, PageInput};
use zbrain_core::libsql::LibsqlEngine;

/// Serialize all libsql FFI access in this binary. The `libsql` native
/// library is not safe to drive from multiple OS threads concurrently on
/// Windows (parallel `cargo test` threads crash with STATUS_ACCESS_VIOLATION
/// 0xc0000005). Each test grabs this guard for its whole body so the suite
/// stays green under default parallelism; serial runs are unaffected.
static LIBSQL_TEST_LOCK: std::sync::OnceLock<std::sync::Mutex<()>> = std::sync::OnceLock::new();
fn libsql_test_guard() -> std::sync::MutexGuard<'static, ()> {
    LIBSQL_TEST_LOCK
        .get_or_init(|| std::sync::Mutex::new(()))
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}


fn assert_in_memory_iso8601_timestamp(ts: &str) {
    assert_eq!(ts.len(), "2026-01-01T00:00:00Z".len());
    assert_eq!(&ts[4..5], "-");
    assert_eq!(&ts[7..8], "-");
    assert_eq!(&ts[10..11], "T");
    assert_eq!(&ts[13..14], ":");
    assert_eq!(&ts[16..17], ":");
    assert_eq!(&ts[19..20], "Z");
    assert_ne!(
        ts, "2026-01-01T00:00:00Z",
        "InMemory timestamp must not be a hardcoded sentinel"
    );
}

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

async fn seed_libsql_page(
    path: &NamedTempFile,
    source_id: &str,
    slug: &str,
    deleted_at: Option<&str>,
) {
    let db_path = path.path().to_string_lossy().into_owned();
    let db = ::libsql::Builder::new_local(db_path)
        .build()
        .await
        .expect("open seed db");
    let conn = db.connect().expect("seed connect");
    conn.execute(
        "INSERT OR IGNORE INTO sources (id, name) VALUES (?1, ?2)",
        ::libsql::params![source_id, format!("source-{source_id}")],
    )
    .await
    .expect("seed source");
    conn.execute(
        "INSERT INTO pages (source_id, slug, type, title, compiled_truth, frontmatter, deleted_at) \
         VALUES (?1, ?2, 'note', ?3, 'body', ?4, ?5)",
        ::libsql::params![
            source_id,
            slug,
            format!("Title {slug}"),
            json!({}).to_string(),
            deleted_at,
        ],
    )
    .await
    .expect("seed page");
}

async fn fetch_libsql_deleted_at(
    path: &NamedTempFile,
    source_id: &str,
    slug: &str,
) -> Option<Option<String>> {
    let db_path = path.path().to_string_lossy().into_owned();
    let db = ::libsql::Builder::new_local(db_path)
        .build()
        .await
        .expect("open inspect db");
    let conn = db.connect().expect("inspect connect");
    let mut rows = conn
        .query(
            "SELECT deleted_at FROM pages WHERE source_id = ?1 AND slug = ?2",
            ::libsql::params![source_id, slug],
        )
        .await
        .expect("inspect deleted_at query");
    rows.next()
        .await
        .expect("inspect deleted_at row")
        .map(|row| row.get(0).expect("decode deleted_at"))
}

#[tokio::test]
async fn libsql_soft_delete_page_marks_live_row_and_returns_slug() {
    let _guard = libsql_test_guard();
    let (engine, tmp) = init_clean_engine().await;
    seed_libsql_page(&tmp, "src-1", "live-slug", None).await;

    let deleted = engine
        .soft_delete_page("live-slug", Some("src-1"))
        .await
        .expect("soft_delete_page");

    assert_eq!(deleted.as_deref(), Some("live-slug"));
    assert!(
        fetch_libsql_deleted_at(&tmp, "src-1", "live-slug")
            .await
            .flatten()
            .is_some(),
        "live row should receive deleted_at timestamp"
    );
    engine.disconnect().await.expect("disconnect");
}

#[tokio::test]
async fn libsql_soft_delete_page_returns_none_for_missing_or_already_deleted_rows() {
    let _guard = libsql_test_guard();
    let (engine, tmp) = init_clean_engine().await;
    seed_libsql_page(
        &tmp,
        "src-1",
        "already-deleted",
        Some("2026-01-01T00:00:00Z"),
    )
    .await;

    let missing = engine
        .soft_delete_page("missing-slug", Some("src-1"))
        .await
        .expect("soft delete missing slug");
    let already_deleted = engine
        .soft_delete_page("already-deleted", Some("src-1"))
        .await
        .expect("soft delete already deleted slug");

    assert_eq!(missing, None);
    assert_eq!(already_deleted, None);
    assert_eq!(
        fetch_libsql_deleted_at(&tmp, "src-1", "already-deleted")
            .await
            .flatten()
            .as_deref(),
        Some("2026-01-01T00:00:00Z"),
        "already-deleted row should not be updated again"
    );
    engine.disconnect().await.expect("disconnect");
}

#[tokio::test]
async fn libsql_soft_delete_page_honors_source_id_filter() {
    let _guard = libsql_test_guard();
    let (engine, tmp) = init_clean_engine().await;
    seed_libsql_page(&tmp, "src-1", "scoped-slug", None).await;

    let mismatched = engine
        .soft_delete_page("scoped-slug", Some("src-2"))
        .await
        .expect("soft delete with mismatched source");

    assert_eq!(mismatched, None);
    assert_eq!(
        fetch_libsql_deleted_at(&tmp, "src-1", "scoped-slug")
            .await
            .flatten(),
        None,
        "source mismatch must leave the live row untouched"
    );
    engine.disconnect().await.expect("disconnect");
}

#[tokio::test]
async fn in_memory_put_page_uses_current_timestamp_shape_not_old_sentinel() {
    let _guard = libsql_test_guard();
    let engine = InMemoryEngine::default();
    engine
        .connect(&EngineConfig::default())
        .await
        .expect("connect");

    let page = engine
        .put_page(
            "memory-timestamp-slug",
            None,
            &PageInput {
                page_type: "note".to_string(),
                title: "Memory Timestamp".to_string(),
                compiled_truth: "body".to_string(),
                ..PageInput::default()
            },
        )
        .await
        .expect("put page");

    assert_in_memory_iso8601_timestamp(&page.created_at);
    assert_in_memory_iso8601_timestamp(&page.updated_at);
    engine.disconnect().await.expect("disconnect");
}

#[tokio::test]
async fn in_memory_soft_delete_page_matches_libsql_contract() {
    let _guard = libsql_test_guard();
    let engine = InMemoryEngine::default();
    engine
        .connect(&EngineConfig::default())
        .await
        .expect("connect");
    engine
        .put_page(
            "memory-slug",
            None,
            &PageInput {
                page_type: "note".to_string(),
                title: "Memory".to_string(),
                compiled_truth: "body".to_string(),
                ..PageInput::default()
            },
        )
        .await
        .expect("seed page");

    let mismatched = engine
        .soft_delete_page("memory-slug", Some("src-1"))
        .await
        .expect("soft delete with mismatched source");
    let deleted = engine
        .soft_delete_page("memory-slug", Some("default"))
        .await
        .expect("soft delete with default source");
    let repeated = engine
        .soft_delete_page("memory-slug", Some("default"))
        .await
        .expect("repeat soft delete");
    // After soft delete the row is invisible by default; pass
    // `include_deleted: true` to inspect the `deleted_at` stamp, matching
    // the libsql `get_page` contract (`include_deleted = false` filters
    // `deleted_at IS NULL`).
    let page = engine
        .get_page(
            "memory-slug",
            &GetPageOpts {
                include_deleted: true,
                ..GetPageOpts::default()
            },
        )
        .await
        .expect("get page")
        .expect("page still exists after soft delete");

    assert_eq!(mismatched, None);
    assert_eq!(deleted.as_deref(), Some("memory-slug"));
    assert_eq!(repeated, None);
    assert!(page.deleted_at.is_some());
    // The timestamp must be shaped like an ISO-8601 value and must not keep
    // the old hardcoded sentinel used by the test double.
    let ts = page.deleted_at.as_ref().expect("deleted_at");
    assert_in_memory_iso8601_timestamp(ts);
    engine.disconnect().await.expect("disconnect");
}

// ---------------------------------------------------------------------------
// PostgresEngine mirror tests (slice 6a-pg PG-soft-delete)
//
// Mirrors the libsql tests to prove behavior parity.
// Uses pg-embed via PgFixture for ephemeral, isolated databases.
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

async fn pg_fetch_deleted_at(
    url: &str,
    slug: &str,
) -> Option<Option<sqlx::types::chrono::DateTime<sqlx::types::chrono::Utc>>> {
    let pool = sqlx::postgres::PgPoolOptions::new()
        .max_connections(1)
        .connect(url)
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
async fn postgres_soft_delete_page_marks_live_row_and_returns_slug() {
    let _guard = libsql_test_guard();
    let fix = support::pg_fixture::PgFixture::start().await;
    let engine = &fix.engine;
    pg_seed_source(&fix.url, "src-1").await;
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
        .expect("seed live page");

    let deleted = engine
        .soft_delete_page("live-slug", Some("src-1"))
        .await
        .expect("soft_delete_page");

    assert_eq!(deleted.as_deref(), Some("live-slug"));
    assert!(
        pg_fetch_deleted_at(&fix.url, "live-slug")
            .await
            .flatten()
            .is_some(),
        "live row should receive deleted_at timestamp"
    );
}

#[tokio::test]
async fn postgres_soft_delete_page_returns_none_for_missing_or_already_deleted_rows() {
    let _guard = libsql_test_guard();
    let fix = support::pg_fixture::PgFixture::start().await;
    let engine = &fix.engine;
    pg_seed_source(&fix.url, "src-1").await;
    engine
        .put_page(
            "already-deleted",
            Some("src-1"),
            &PageInput {
                page_type: "note".to_string(),
                title: "Pre".to_string(),
                compiled_truth: "body".to_string(),
                ..PageInput::default()
            },
        )
        .await
        .expect("seed page");

    // First soft delete establishes the deleted_at timestamp.
    let first = engine
        .soft_delete_page("already-deleted", Some("src-1"))
        .await
        .expect("first soft delete");
    assert_eq!(first.as_deref(), Some("already-deleted"));
    let first_ts = pg_fetch_deleted_at(&fix.url, "already-deleted")
        .await
        .flatten()
        .expect("deleted_at after first soft delete");

    let missing = engine
        .soft_delete_page("missing-slug", Some("src-1"))
        .await
        .expect("soft delete missing slug");
    let already_deleted = engine
        .soft_delete_page("already-deleted", Some("src-1"))
        .await
        .expect("soft delete already deleted slug");

    assert_eq!(missing, None);
    assert_eq!(already_deleted, None);
    let after_ts = pg_fetch_deleted_at(&fix.url, "already-deleted")
        .await
        .flatten()
        .expect("deleted_at still set");
    assert_eq!(
        after_ts, first_ts,
        "already-deleted row must not be updated again"
    );
}

#[tokio::test]
async fn postgres_soft_delete_page_honors_source_id_filter() {
    let _guard = libsql_test_guard();
    let fix = support::pg_fixture::PgFixture::start().await;
    let engine = &fix.engine;
    pg_seed_source(&fix.url, "src-1").await;
    engine
        .put_page(
            "scoped-slug",
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

    let mismatched = engine
        .soft_delete_page("scoped-slug", Some("src-2"))
        .await
        .expect("soft delete with mismatched source");

    assert_eq!(mismatched, None);
    assert_eq!(
        pg_fetch_deleted_at(&fix.url, "scoped-slug").await.flatten(),
        None,
        "source mismatch must leave the live row untouched"
    );
}
