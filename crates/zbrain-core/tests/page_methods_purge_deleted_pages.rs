//! Slice 6a TS-parity tests: `purge_deleted_pages` libsql implementation.

use tempfile::NamedTempFile;
use zbrain_core::engine::{BrainEngine, EngineConfig, PageInput};
use zbrain_core::libsql::LibsqlEngine;
use zbrain_core::types::PurgeResult;

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

async fn libsql_set_deleted_at_hours_ago(tmp: &NamedTempFile, slug: &str, hours: i64) {
    let path_str = tmp.path().to_string_lossy().into_owned();
    let db = ::libsql::Builder::new_local(&path_str)
        .build()
        .await
        .expect("raw db open");
    let raw_conn = db.connect().expect("raw conn");
    raw_conn
        .execute(
            "UPDATE pages SET deleted_at = datetime('now', '-' || ?2 || ' hours') WHERE slug = ?1",
            ::libsql::params![slug, hours.to_string()],
        )
        .await
        .expect("backdate deleted_at");
}

async fn libsql_slugs_remaining(tmp: &NamedTempFile) -> Vec<String> {
    let path_str = tmp.path().to_string_lossy().into_owned();
    let db = ::libsql::Builder::new_local(&path_str)
        .build()
        .await
        .expect("raw db open");
    let raw_conn = db.connect().expect("raw conn");
    let mut rows = raw_conn
        .query("SELECT slug FROM pages ORDER BY slug ASC", ())
        .await
        .expect("list slugs");
    let mut out = Vec::new();
    while let Some(row) = rows.next().await.expect("slug row fetch") {
        out.push(row.get(0).expect("slug decode"));
    }
    out
}

async fn libsql_seed_page(engine: &LibsqlEngine, slug: &str, source_id: &str, title: &str) {
    engine
        .put_page(
            slug,
            Some(source_id),
            &PageInput {
                page_type: "note".to_string(),
                title: title.to_string(),
                compiled_truth: format!("body of {slug}"),
                ..PageInput::default()
            },
        )
        .await
        .unwrap_or_else(|e| panic!("seed page {slug}: {e}"));
}

fn assert_libsql_purge(actual: &PurgeResult, expected_sorted: &[&str]) {
    let mut got = actual.slugs.clone();
    got.sort();
    let want: Vec<String> = expected_sorted.iter().map(ToString::to_string).collect();
    assert_eq!(
        got,
        want,
        "purge slug set mismatch (count={} expected={})",
        actual.count,
        expected_sorted.len()
    );
    assert_eq!(
        actual.count,
        expected_sorted.len() as u64,
        "PurgeResult.count must match slugs length"
    );
}

#[tokio::test]
async fn libsql_purge_deleted_pages_returns_slugs_for_rows_older_than_window() {
    let (engine, tmp) = init_clean_engine().await;
    libsql_seed_source(&tmp, "src-1").await;

    libsql_seed_page(&engine, "old-a", "src-1", "Old A").await;
    libsql_seed_page(&engine, "old-b", "src-1", "Old B").await;
    libsql_seed_page(&engine, "fresh", "src-1", "Fresh").await;
    libsql_seed_page(&engine, "live", "src-1", "Live").await;

    engine
        .soft_delete_page("old-a", Some("src-1"))
        .await
        .expect("soft delete old-a");
    engine
        .soft_delete_page("old-b", Some("src-1"))
        .await
        .expect("soft delete old-b");
    engine
        .soft_delete_page("fresh", Some("src-1"))
        .await
        .expect("soft delete fresh");

    libsql_set_deleted_at_hours_ago(&tmp, "old-a", 72).await;
    libsql_set_deleted_at_hours_ago(&tmp, "old-b", 48).await;
    libsql_set_deleted_at_hours_ago(&tmp, "fresh", 1).await;

    let result = engine
        .purge_deleted_pages(24)
        .await
        .expect("purge older than 24h");
    assert_libsql_purge(&result, &["old-a", "old-b"]);

    let remaining = libsql_slugs_remaining(&tmp).await;
    assert_eq!(
        remaining,
        vec!["fresh".to_string(), "live".to_string()],
        "only fresh (soft-deleted within window) and live row must remain"
    );

    engine.disconnect().await.expect("disconnect");
}

#[tokio::test]
async fn libsql_purge_deleted_pages_returns_empty_when_nothing_qualifies() {
    let (engine, tmp) = init_clean_engine().await;
    libsql_seed_source(&tmp, "src-1").await;
    libsql_seed_page(&engine, "live-only", "src-1", "Live Only").await;

    let result = engine
        .purge_deleted_pages(24)
        .await
        .expect("purge on a table with no soft-deleted rows");
    assert_libsql_purge(&result, &[]);

    let remaining = libsql_slugs_remaining(&tmp).await;
    assert_eq!(remaining, vec!["live-only".to_string()]);

    engine.disconnect().await.expect("disconnect");
}

#[tokio::test]
async fn libsql_purge_deleted_pages_zero_hours_purges_all_soft_deleted_rows() {
    let (engine, tmp) = init_clean_engine().await;
    libsql_seed_source(&tmp, "src-1").await;
    libsql_seed_page(&engine, "doomed-a", "src-1", "Doomed A").await;
    libsql_seed_page(&engine, "doomed-b", "src-1", "Doomed B").await;
    libsql_seed_page(&engine, "survivor", "src-1", "Survivor").await;

    engine
        .soft_delete_page("doomed-a", Some("src-1"))
        .await
        .expect("soft delete doomed-a");
    engine
        .soft_delete_page("doomed-b", Some("src-1"))
        .await
        .expect("soft delete doomed-b");

    let result = engine
        .purge_deleted_pages(0)
        .await
        .expect("purge with zero-hour window");
    assert_libsql_purge(&result, &["doomed-a", "doomed-b"]);

    let remaining = libsql_slugs_remaining(&tmp).await;
    assert_eq!(remaining, vec!["survivor".to_string()]);

    engine.disconnect().await.expect("disconnect");
}

// ---------------------------------------------------------------------------
// PostgresEngine mirror tests (slice 6a-pg PG-soft-delete: purge_deleted_pages)
//
// Mirrors TS `purgeDeletedPages(olderThanHours)` from `pglite-engine.ts`:
//   DELETE FROM pages
//   WHERE deleted_at IS NOT NULL
//     AND deleted_at < now() - ($1 || ' hours')::interval
//   RETURNING slug;
//
// Returns `{ slugs, count }`. FK CASCADE on chunks/links is exercised
// implicitly via the schema and is not asserted here directly — these
// tests focus on the time-window filter and the returned slug set.
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

/// Force `deleted_at` to a fixed offset in the past (relative to `now()`),
/// bypassing the engine API so the test can position rows across the purge
/// window deterministically.
async fn pg_set_deleted_at_hours_ago(slug: &str, hours: i64) {
    let url = pg_url().expect("ZBRAIN_TEST_PG_URL set for backdate");
    let pool = sqlx::postgres::PgPoolOptions::new()
        .max_connections(1)
        .connect(&url)
        .await
        .expect("backdate pool");
    sqlx::query("UPDATE pages SET deleted_at = now() - ($2 || ' hours')::interval WHERE slug = $1")
        .bind(slug)
        .bind(hours.to_string())
        .execute(&pool)
        .await
        .expect("backdate deleted_at");
    pool.close().await;
}

async fn pg_slugs_remaining() -> Vec<String> {
    let url = pg_url().expect("ZBRAIN_TEST_PG_URL set for inspect");
    let pool = sqlx::postgres::PgPoolOptions::new()
        .max_connections(1)
        .connect(&url)
        .await
        .expect("inspect pool");
    let rows: Vec<(String,)> = sqlx::query_as("SELECT slug FROM pages ORDER BY slug ASC")
        .fetch_all(&pool)
        .await
        .expect("list slugs");
    pool.close().await;
    rows.into_iter().map(|r| r.0).collect()
}

async fn pg_seed_page(engine: &PostgresEngine, slug: &str, source_id: &str, title: &str) {
    engine
        .put_page(
            slug,
            Some(source_id),
            &PageInput {
                page_type: "note".to_string(),
                title: title.to_string(),
                compiled_truth: format!("body of {slug}"),
                ..PageInput::default()
            },
        )
        .await
        .unwrap_or_else(|e| panic!("seed page {slug}: {e}"));
}

fn assert_purge(actual: &PurgeResult, expected_sorted: &[&str]) {
    let mut got = actual.slugs.clone();
    got.sort();
    let want: Vec<String> = expected_sorted.iter().map(ToString::to_string).collect();
    assert_eq!(
        got,
        want,
        "purge slug set mismatch (count={} expected={})",
        actual.count,
        expected_sorted.len()
    );
    assert_eq!(
        actual.count,
        expected_sorted.len() as u64,
        "PurgeResult.count must match slugs length"
    );
}

#[tokio::test]
#[serial_test::serial]
async fn postgres_purge_deleted_pages_returns_slugs_for_rows_older_than_window() {
    let Some(engine) = pg_init_clean_engine().await else {
        eprintln!("skipping: ZBRAIN_TEST_PG_URL unset");
        return;
    };
    pg_seed_source("src-1").await;

    // Three soft-deleted rows backdated 72h / 48h / 1h, plus one live row.
    pg_seed_page(&engine, "old-a", "src-1", "Old A").await;
    pg_seed_page(&engine, "old-b", "src-1", "Old B").await;
    pg_seed_page(&engine, "fresh", "src-1", "Fresh").await;
    pg_seed_page(&engine, "live", "src-1", "Live").await;

    engine
        .soft_delete_page("old-a", Some("src-1"))
        .await
        .expect("soft delete old-a");
    engine
        .soft_delete_page("old-b", Some("src-1"))
        .await
        .expect("soft delete old-b");
    engine
        .soft_delete_page("fresh", Some("src-1"))
        .await
        .expect("soft delete fresh");

    pg_set_deleted_at_hours_ago("old-a", 72).await;
    pg_set_deleted_at_hours_ago("old-b", 48).await;
    pg_set_deleted_at_hours_ago("fresh", 1).await;

    let result = engine
        .purge_deleted_pages(24)
        .await
        .expect("purge older than 24h");
    assert_purge(&result, &["old-a", "old-b"]);

    let remaining = pg_slugs_remaining().await;
    assert_eq!(
        remaining,
        vec!["fresh".to_string(), "live".to_string()],
        "only fresh (soft-deleted within window) and live row must remain"
    );

    engine.disconnect().await.expect("disconnect");
}

#[tokio::test]
#[serial_test::serial]
async fn postgres_purge_deleted_pages_returns_empty_when_nothing_qualifies() {
    let Some(engine) = pg_init_clean_engine().await else {
        eprintln!("skipping: ZBRAIN_TEST_PG_URL unset");
        return;
    };
    pg_seed_source("src-1").await;
    pg_seed_page(&engine, "live-only", "src-1", "Live Only").await;

    let result = engine
        .purge_deleted_pages(24)
        .await
        .expect("purge on a table with no soft-deleted rows");
    assert_purge(&result, &[]);

    let remaining = pg_slugs_remaining().await;
    assert_eq!(remaining, vec!["live-only".to_string()]);

    engine.disconnect().await.expect("disconnect");
}

#[tokio::test]
#[serial_test::serial]
async fn postgres_purge_deleted_pages_zero_hours_purges_all_soft_deleted_rows() {
    let Some(engine) = pg_init_clean_engine().await else {
        eprintln!("skipping: ZBRAIN_TEST_PG_URL unset");
        return;
    };
    pg_seed_source("src-1").await;
    pg_seed_page(&engine, "doomed-a", "src-1", "Doomed A").await;
    pg_seed_page(&engine, "doomed-b", "src-1", "Doomed B").await;
    pg_seed_page(&engine, "survivor", "src-1", "Survivor").await;

    engine
        .soft_delete_page("doomed-a", Some("src-1"))
        .await
        .expect("soft delete doomed-a");
    engine
        .soft_delete_page("doomed-b", Some("src-1"))
        .await
        .expect("soft delete doomed-b");

    // older_than_hours = 0 should match every row with deleted_at IS NOT NULL,
    // regardless of how recent the timestamp is.
    let result = engine
        .purge_deleted_pages(0)
        .await
        .expect("purge with zero-hour window");
    assert_purge(&result, &["doomed-a", "doomed-b"]);

    let remaining = pg_slugs_remaining().await;
    assert_eq!(remaining, vec!["survivor".to_string()]);

    engine.disconnect().await.expect("disconnect");
}
