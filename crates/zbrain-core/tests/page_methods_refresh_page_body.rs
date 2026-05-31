//! Slice PG-advanced-writes RED: `refresh_page_body` behavior tests.

use tempfile::NamedTempFile;
use zbrain_core::engine::{BrainEngine, EngineConfig, GetPageOpts, PageInput};
use zbrain_core::libsql::LibsqlEngine;
use zbrain_core::postgres::PostgresEngine;
use zbrain_core::RefreshPageBodyArgs;

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
async fn slice_6a_page_methods_refresh_page_body_returns_unsupported() {
    let (engine, _tmp) = init_clean_engine().await;
    let args = RefreshPageBodyArgs {
        slug: "slug-1".to_string(),
        source_id: "src-1".to_string(),
        compiled_truth: "updated body".to_string(),
        timeline: serde_json::Value::Array(Vec::new()),
        content_hash: "hash-1".to_string(),
    };
    let err = engine
        .refresh_page_body(&args)
        .await
        .expect_err("6a placeholder-lock: refresh_page_body must be Unsupported");
    let msg = err.to_string();
    assert!(
        msg.contains("pending slice 6a"),
        "expected placeholder marker, got: {msg}"
    );
    engine.disconnect().await.expect("disconnect");
}

// ---------------------------------------------------------------------------
// PostgresEngine mirror tests (PG-advanced-writes: refresh_page_body)
//
// Mirrors TS `refreshPageBody`: update `compiled_truth`, `timeline`,
// `content_hash`, and `updated_at` for exactly one live `(source_id, slug)` row.
// Soft-deleted rows are skipped by `deleted_at IS NULL`.
// ---------------------------------------------------------------------------

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

async fn pg_force_old_updated_at(slug: &str, source_id: &str) {
    let url = pg_url().expect("ZBRAIN_TEST_PG_URL set for timestamp prep");
    let pool = sqlx::postgres::PgPoolOptions::new()
        .max_connections(1)
        .connect(&url)
        .await
        .expect("timestamp prep pool");
    sqlx::query(
        "UPDATE pages \
         SET updated_at = TIMESTAMPTZ '2000-01-01 00:00:00+00' \
         WHERE slug = $1 AND source_id = $2",
    )
    .bind(slug)
    .bind(source_id)
    .execute(&pool)
    .await
    .expect("force old updated_at");
    pool.close().await;
}

fn note_input(title: &str, body: &str, timeline: &str, content_hash: &str) -> PageInput {
    PageInput {
        page_type: "note".to_string(),
        title: title.to_string(),
        compiled_truth: body.to_string(),
        timeline: Some(timeline.to_string()),
        content_hash: Some(content_hash.to_string()),
        ..PageInput::default()
    }
}

fn get_opts(source_id: &str, include_deleted: bool) -> GetPageOpts {
    GetPageOpts {
        source_id: Some(source_id.to_string()),
        include_deleted,
    }
}

#[tokio::test]
#[serial_test::serial]
async fn postgres_refresh_page_body_updates_exact_live_source_row() {
    let Some(engine) = pg_init_clean_engine().await else {
        eprintln!("skipping: ZBRAIN_TEST_PG_URL unset");
        return;
    };
    pg_seed_source("src-1").await;
    pg_seed_source("src-2").await;
    for src in ["src-1", "src-2"] {
        engine
            .put_page(
                "shared-slug",
                Some(src),
                &note_input("Shared", "old body", "[]", "old-hash"),
            )
            .await
            .expect("seed page");
        pg_force_old_updated_at("shared-slug", src).await;
    }

    let timeline = serde_json::json!([{ "kind": "refresh", "at": "now" }]);
    engine
        .refresh_page_body(&RefreshPageBodyArgs {
            slug: "shared-slug".to_string(),
            source_id: "src-1".to_string(),
            compiled_truth: "new body".to_string(),
            timeline: timeline.clone(),
            content_hash: "new-hash".to_string(),
        })
        .await
        .expect("refresh_page_body");

    let updated = engine
        .get_page("shared-slug", &get_opts("src-1", false))
        .await
        .expect("get updated page")
        .expect("updated page exists");
    assert_eq!(updated.compiled_truth, "new body");
    assert_eq!(updated.timeline, timeline.to_string());
    assert_eq!(updated.content_hash.as_deref(), Some("new-hash"));
    assert!(
        !updated.updated_at.starts_with("2000-01-01"),
        "refresh_page_body must bump updated_at, got {}",
        updated.updated_at
    );

    let untouched = engine
        .get_page("shared-slug", &get_opts("src-2", false))
        .await
        .expect("get untouched page")
        .expect("untouched page exists");
    assert_eq!(untouched.compiled_truth, "old body");
    assert_eq!(untouched.timeline, "[]");
    assert_eq!(untouched.content_hash.as_deref(), Some("old-hash"));
    assert!(
        untouched.updated_at.starts_with("2000-01-01"),
        "source mismatch must remain untouched, got {}",
        untouched.updated_at
    );
    engine.disconnect().await.expect("disconnect");
}

#[tokio::test]
#[serial_test::serial]
async fn postgres_refresh_page_body_skips_soft_deleted_rows() {
    let Some(engine) = pg_init_clean_engine().await else {
        eprintln!("skipping: ZBRAIN_TEST_PG_URL unset");
        return;
    };
    pg_seed_source("src-1").await;
    engine
        .put_page(
            "soft-deleted",
            Some("src-1"),
            &note_input("Soft Deleted", "old body", "[]", "old-hash"),
        )
        .await
        .expect("seed page");
    engine
        .soft_delete_page("soft-deleted", Some("src-1"))
        .await
        .expect("soft delete prep");

    engine
        .refresh_page_body(&RefreshPageBodyArgs {
            slug: "soft-deleted".to_string(),
            source_id: "src-1".to_string(),
            compiled_truth: "new body".to_string(),
            timeline: serde_json::json!([{ "kind": "refresh" }]),
            content_hash: "new-hash".to_string(),
        })
        .await
        .expect("refresh_page_body no-ops on soft-deleted row");

    let page = engine
        .get_page("soft-deleted", &get_opts("src-1", true))
        .await
        .expect("get soft-deleted page")
        .expect("soft-deleted page exists when include_deleted");
    assert_eq!(page.compiled_truth, "old body");
    assert_eq!(page.timeline, "[]");
    assert_eq!(page.content_hash.as_deref(), Some("old-hash"));
    assert!(page.deleted_at.is_some(), "row remains soft-deleted");
    engine.disconnect().await.expect("disconnect");
}
