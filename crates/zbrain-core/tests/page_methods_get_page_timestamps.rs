//! Slice 6a-libsql advanced reads (libsql parity): `get_page_timestamps` behavior tests.
//!
//! Mirrors TS-compatible `getPageTimestamps` semantics:
//!   `SELECT slug, COALESCE(updated_at, created_at) AS ts FROM pages
//!    WHERE slug IN (?...)`
//! returning a `HashMap<String, String>` keyed by slug. Soft-deleted rows
//! remain visible because TS does not filter `deleted_at`; missing slugs are
//! silently dropped from the result.
//!
//! PG mirror tests below this libsql block use the same public behavior.

mod support;

use tempfile::NamedTempFile;
use zbrain_core::engine::{BrainEngine, EngineConfig, PageInput};
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

fn note_input(title: &str, body: &str) -> PageInput {
    PageInput {
        page_type: "note".to_string(),
        title: title.to_string(),
        compiled_truth: body.to_string(),
        ..PageInput::default()
    }
}

#[tokio::test]
async fn libsql_get_page_timestamps_returns_iso_ts_for_each_existing_slug() {
    let _guard = libsql_test_guard();
    let (engine, tmp) = init_clean_engine().await;
    libsql_seed_source(&tmp, "src-1").await;
    for slug in ["alpha", "beta"] {
        engine
            .put_page(slug, Some("src-1"), &note_input(slug, "body"))
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
        // ISO-8601 timestamps always start with a 4-digit year.
        assert!(
            ts.len() >= 10 && ts.starts_with("20"),
            "expected ISO-8601 ts for {slug}, got {ts}"
        );
    }
    engine.disconnect().await.expect("disconnect");
}

#[tokio::test]
async fn libsql_get_page_timestamps_includes_soft_deleted_rows() {
    let _guard = libsql_test_guard();
    let (engine, tmp) = init_clean_engine().await;
    libsql_seed_source(&tmp, "src-1").await;
    for slug in ["live-slug", "tombstone-slug"] {
        engine
            .put_page(slug, Some("src-1"), &note_input(slug, "body"))
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
        stamps.contains_key("tombstone-slug"),
        "TS getPageTimestamps does not filter deleted_at, so tombstones stay visible"
    );
    assert_eq!(stamps.len(), 2);
    engine.disconnect().await.expect("disconnect");
}

#[tokio::test]
async fn libsql_get_page_timestamps_silently_drops_missing_slugs() {
    let _guard = libsql_test_guard();
    let (engine, tmp) = init_clean_engine().await;
    libsql_seed_source(&tmp, "src-1").await;
    engine
        .put_page("only-slug", Some("src-1"), &note_input("Only", "body"))
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
async fn libsql_get_page_timestamps_returns_empty_map_for_empty_input() {
    let _guard = libsql_test_guard();
    let (engine, _tmp) = init_clean_engine().await;

    let stamps = engine
        .get_page_timestamps(&[])
        .await
        .expect("get_page_timestamps on empty input");
    assert!(stamps.is_empty(), "empty slugs slice → empty map");
    engine.disconnect().await.expect("disconnect");
}

// ---------------------------------------------------------------------------
// PostgresEngine mirror tests (slice 6a-pg PG-advanced-reads)
//
// Locks TS-compatible PG `get_page_timestamps` semantics:
//   SELECT slug, COALESCE(updated_at, created_at)::text AS ts
//   FROM pages
//   WHERE slug = ANY($1::text[])
// (i.e. keyed by slug; soft-deleted rows remain visible; missing slugs silently
// dropped.) Uses pg-embed via PgFixture for ephemeral, isolated databases.
// No serial gating needed — each test gets its own database.
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

#[tokio::test]
async fn postgres_get_page_timestamps_returns_iso_ts_for_each_existing_slug() {
    let _guard = libsql_test_guard();
    let fix = support::pg_fixture::PgFixture::start().await;
    let engine = &fix.engine;
    pg_seed_source(&fix.url, "src-1").await;
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
}

#[tokio::test]
async fn postgres_get_page_timestamps_includes_soft_deleted_rows() {
    let _guard = libsql_test_guard();
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
        stamps.contains_key("tombstone-slug"),
        "TS getPageTimestamps does not filter deleted_at, so tombstones stay visible"
    );
    assert_eq!(stamps.len(), 2);
}

#[tokio::test]
async fn postgres_get_page_timestamps_silently_drops_missing_slugs() {
    let _guard = libsql_test_guard();
    let fix = support::pg_fixture::PgFixture::start().await;
    let engine = &fix.engine;
    pg_seed_source(&fix.url, "src-1").await;
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
}

#[tokio::test]
async fn postgres_get_page_timestamps_returns_empty_map_for_empty_input() {
    let _guard = libsql_test_guard();
    let fix = support::pg_fixture::PgFixture::start().await;
    let engine = &fix.engine;

    let stamps = engine
        .get_page_timestamps(&[])
        .await
        .expect("get_page_timestamps on empty input");
    assert!(stamps.is_empty(), "empty slugs slice → empty map");
}
