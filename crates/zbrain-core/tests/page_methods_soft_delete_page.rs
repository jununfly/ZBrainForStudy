//! Slice 6a S6-T3 semantic tests: `soft_delete_page`.

use serde_json::json;
use tempfile::NamedTempFile;
use zbrain_core::engine::{BrainEngine, EngineConfig, GetPageOpts, InMemoryEngine, PageInput};
use zbrain_core::libsql::LibsqlEngine;

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
    let (engine, tmp) = init_clean_engine().await;
    seed_libsql_page(&tmp, "src-1", "already-deleted", Some("2026-01-01T00:00:00Z")).await;

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
    let engine = InMemoryEngine::default();
    engine.connect(&EngineConfig::default()).await.expect("connect");

    let page = engine
        .put_page(
            "memory-timestamp-slug",
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
    let engine = InMemoryEngine::default();
    engine.connect(&EngineConfig::default()).await.expect("connect");
    engine
        .put_page(
            "memory-slug",
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
    let page = engine
        .get_page("memory-slug", &GetPageOpts::default())
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
