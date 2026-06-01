//! Slice 6a S6-T2 semantic tests: `find_duplicate_page`.
//!
//! Mirrors TS `findDuplicatePage`: within one source, a live page is a duplicate
//! when either `content_hash` matches or `frontmatter.id` matches. Soft-deleted
//! rows are ignored.

mod support;

use serde_json::json;
use tempfile::NamedTempFile;
use zbrain_core::engine::{BrainEngine, EngineConfig, InMemoryEngine, PageInput};
use zbrain_core::libsql::LibsqlEngine;
use zbrain_core::FindDuplicatePageOpts;

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
    content_hash: Option<&str>,
    frontmatter: serde_json::Value,
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
        "INSERT INTO pages (source_id, slug, type, title, compiled_truth, frontmatter, content_hash, deleted_at) \
         VALUES (?1, ?2, 'note', ?3, 'body', ?4, ?5, ?6)",
        ::libsql::params![
            source_id,
            slug,
            format!("Title {slug}"),
            frontmatter.to_string(),
            content_hash,
            deleted_at,
        ],
    )
    .await
    .expect("seed page");
}

#[tokio::test]
async fn libsql_find_duplicate_page_matches_content_hash() {
    let (engine, tmp) = init_clean_engine().await;
    seed_libsql_page(
        &tmp,
        "src-1",
        "hash-hit",
        Some("hash-1"),
        json!({"id": "fm-other"}),
        None,
    )
    .await;

    let found = engine
        .find_duplicate_page(
            "src-1",
            &FindDuplicatePageOpts {
                content_hash: "hash-1".to_string(),
                frontmatter_id: None,
            },
        )
        .await
        .expect("find duplicate");

    let page = found.expect("matching content_hash should return a page");
    assert_eq!(page.slug, "hash-hit");
    assert_eq!(page.source_id, "src-1");
    assert_eq!(page.content_hash.as_deref(), Some("hash-1"));
    engine.disconnect().await.expect("disconnect");
}

#[tokio::test]
async fn libsql_find_duplicate_page_matches_frontmatter_id() {
    let (engine, tmp) = init_clean_engine().await;
    seed_libsql_page(
        &tmp,
        "src-1",
        "frontmatter-hit",
        Some("hash-other"),
        json!({"id": "fm-1"}),
        None,
    )
    .await;

    let found = engine
        .find_duplicate_page(
            "src-1",
            &FindDuplicatePageOpts {
                content_hash: "hash-miss".to_string(),
                frontmatter_id: Some("fm-1".to_string()),
            },
        )
        .await
        .expect("find duplicate");

    let page = found.expect("matching frontmatter.id should return a page");
    assert_eq!(page.slug, "frontmatter-hit");
    assert_eq!(page.frontmatter["id"], "fm-1");
    engine.disconnect().await.expect("disconnect");
}

#[tokio::test]
async fn libsql_find_duplicate_page_ignores_soft_deleted_rows() {
    let (engine, tmp) = init_clean_engine().await;
    seed_libsql_page(
        &tmp,
        "src-1",
        "deleted-hit",
        Some("hash-1"),
        json!({"id": "fm-1"}),
        Some("2026-01-01T00:00:00Z"),
    )
    .await;

    let found = engine
        .find_duplicate_page(
            "src-1",
            &FindDuplicatePageOpts {
                content_hash: "hash-1".to_string(),
                frontmatter_id: Some("fm-1".to_string()),
            },
        )
        .await
        .expect("find duplicate");

    assert!(found.is_none(), "soft-deleted duplicates must be ignored");
    engine.disconnect().await.expect("disconnect");
}

#[tokio::test]
async fn in_memory_find_duplicate_page_matches_content_hash_and_frontmatter_id() {
    let engine = InMemoryEngine::default();
    engine
        .connect(&EngineConfig::default())
        .await
        .expect("connect");
    engine
        .put_page(
            "hash-hit",
            None,
            &PageInput {
                page_type: "note".to_string(),
                title: "Hash Hit".to_string(),
                compiled_truth: "body".to_string(),
                content_hash: Some("hash-1".to_string()),
                frontmatter: Some(json!({"id": "fm-other"})),
                ..PageInput::default()
            },
        )
        .await
        .expect("seed hash page");
    engine
        .put_page(
            "frontmatter-hit",
            None,
            &PageInput {
                page_type: "note".to_string(),
                title: "Frontmatter Hit".to_string(),
                compiled_truth: "body".to_string(),
                content_hash: Some("hash-other".to_string()),
                frontmatter: Some(json!({"id": "fm-1"})),
                ..PageInput::default()
            },
        )
        .await
        .expect("seed frontmatter page");

    let hash_hit = engine
        .find_duplicate_page(
            "default",
            &FindDuplicatePageOpts {
                content_hash: "hash-1".to_string(),
                frontmatter_id: None,
            },
        )
        .await
        .expect("find duplicate")
        .expect("content_hash match");
    assert_eq!(hash_hit.slug, "hash-hit");

    let frontmatter_hit = engine
        .find_duplicate_page(
            "default",
            &FindDuplicatePageOpts {
                content_hash: "hash-miss".to_string(),
                frontmatter_id: Some("fm-1".to_string()),
            },
        )
        .await
        .expect("find duplicate")
        .expect("frontmatter.id match");
    assert_eq!(frontmatter_hit.slug, "frontmatter-hit");

    let miss = engine
        .find_duplicate_page(
            "other-source",
            &FindDuplicatePageOpts {
                content_hash: "hash-1".to_string(),
                frontmatter_id: Some("fm-1".to_string()),
            },
        )
        .await
        .expect("find duplicate");
    assert!(miss.is_none(), "source_id must scope duplicate lookup");
    engine.disconnect().await.expect("disconnect");
}

// ---------------------------------------------------------------------------
// PostgresEngine mirror tests (slice 6a-pg PG-find-duplicate)
//
// Mirrors the libsql tests above to prove behavior parity. Each test launches
// its own ephemeral `PostgreSQL` instance via `PgFixture`, so no external
// database or environment variable is required.
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

async fn pg_soft_delete_via_sql(url: &str, slug: &str) {
    let pool = sqlx::postgres::PgPoolOptions::new()
        .max_connections(1)
        .connect(url)
        .await
        .expect("soft-delete pool");
    let rows = sqlx::query("UPDATE pages SET deleted_at = now() WHERE slug = $1")
        .bind(slug)
        .execute(&pool)
        .await
        .expect("update deleted_at")
        .rows_affected();
    assert!(
        rows >= 1,
        "pg_soft_delete_via_sql expected to update at least one row for slug={slug}, got {rows}"
    );
    pool.close().await;
}

#[tokio::test]
async fn postgres_find_duplicate_page_matches_content_hash() {
    let fix = support::pg_fixture::PgFixture::start().await;
    let engine = &fix.engine;
    pg_seed_source(&fix.url, "src-1").await;
    engine
        .put_page(
            "hash-hit",
            Some("src-1"),
            &PageInput {
                page_type: "note".to_string(),
                title: "Hash Hit".to_string(),
                compiled_truth: "body".to_string(),
                content_hash: Some("hash-1".to_string()),
                frontmatter: Some(json!({"id": "fm-other"})),
                ..PageInput::default()
            },
        )
        .await
        .expect("seed hash page");

    let found = engine
        .find_duplicate_page(
            "src-1",
            &FindDuplicatePageOpts {
                content_hash: "hash-1".to_string(),
                frontmatter_id: None,
            },
        )
        .await
        .expect("find duplicate");

    let page = found.expect("matching content_hash should return a page");
    assert_eq!(page.slug, "hash-hit");
    assert_eq!(page.source_id, "src-1");
    assert_eq!(page.content_hash.as_deref(), Some("hash-1"));
}

#[tokio::test]
async fn postgres_find_duplicate_page_matches_frontmatter_id() {
    let fix = support::pg_fixture::PgFixture::start().await;
    let engine = &fix.engine;
    pg_seed_source(&fix.url, "src-1").await;
    engine
        .put_page(
            "frontmatter-hit",
            Some("src-1"),
            &PageInput {
                page_type: "note".to_string(),
                title: "Frontmatter Hit".to_string(),
                compiled_truth: "body".to_string(),
                content_hash: Some("hash-other".to_string()),
                frontmatter: Some(json!({"id": "fm-1"})),
                ..PageInput::default()
            },
        )
        .await
        .expect("seed frontmatter page");

    let found = engine
        .find_duplicate_page(
            "src-1",
            &FindDuplicatePageOpts {
                content_hash: "hash-miss".to_string(),
                frontmatter_id: Some("fm-1".to_string()),
            },
        )
        .await
        .expect("find duplicate");

    let page = found.expect("matching frontmatter.id should return a page");
    assert_eq!(page.slug, "frontmatter-hit");
    assert_eq!(page.frontmatter["id"], "fm-1");
}

#[tokio::test]
async fn postgres_find_duplicate_page_ignores_soft_deleted_rows() {
    let fix = support::pg_fixture::PgFixture::start().await;
    let engine = &fix.engine;
    pg_seed_source(&fix.url, "src-1").await;
    engine
        .put_page(
            "deleted-hit",
            Some("src-1"),
            &PageInput {
                page_type: "note".to_string(),
                title: "Deleted Hit".to_string(),
                compiled_truth: "body".to_string(),
                content_hash: Some("hash-1".to_string()),
                frontmatter: Some(json!({"id": "fm-1"})),
                ..PageInput::default()
            },
        )
        .await
        .expect("seed deleted page");
    pg_soft_delete_via_sql(&fix.url, "deleted-hit").await;

    let found = engine
        .find_duplicate_page(
            "src-1",
            &FindDuplicatePageOpts {
                content_hash: "hash-1".to_string(),
                frontmatter_id: Some("fm-1".to_string()),
            },
        )
        .await
        .expect("find duplicate");

    assert!(found.is_none(), "soft-deleted duplicates must be ignored");
}
