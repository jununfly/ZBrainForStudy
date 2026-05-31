//! Slice PG-advanced-writes RED: `update_page_contextual_retrieval_state` behavior tests.

use tempfile::NamedTempFile;
use zbrain_core::engine::{BrainEngine, EngineConfig, GetPageOpts, PageInput};
use zbrain_core::libsql::LibsqlEngine;
use zbrain_core::postgres::PostgresEngine;
use zbrain_core::CRMode;

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
async fn slice_6a_page_methods_update_cr_state_returns_unsupported() {
    let (engine, _tmp) = init_clean_engine().await;
    let err = engine
        .update_page_contextual_retrieval_state("slug-1", "src-1", "off", None)
        .await
        .expect_err(
            "6a placeholder-lock: update_page_contextual_retrieval_state must be Unsupported",
        );
    let msg = err.to_string();
    assert!(
        msg.contains("pending slice 6a"),
        "expected placeholder marker, got: {msg}"
    );
    engine.disconnect().await.expect("disconnect");
}

// ---------------------------------------------------------------------------
// PostgresEngine mirror tests (PG-advanced-writes: CR state)
//
// Mirrors TS `updatePageContextualRetrievalState`: update
// `contextual_retrieval_mode`, `corpus_generation`, and `updated_at` for
// exactly one live `(source_id, slug)` row. Soft-deleted rows are skipped by
// `deleted_at IS NULL`.
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

fn note_input(title: &str) -> PageInput {
    PageInput {
        page_type: "note".to_string(),
        title: title.to_string(),
        compiled_truth: "body".to_string(),
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
async fn postgres_update_cr_state_updates_exact_live_source_row() {
    let Some(engine) = pg_init_clean_engine().await else {
        eprintln!("skipping: ZBRAIN_TEST_PG_URL unset");
        return;
    };
    pg_seed_source("src-1").await;
    pg_seed_source("src-2").await;
    for src in ["src-1", "src-2"] {
        engine
            .put_page("shared-slug", Some(src), &note_input("Shared"))
            .await
            .expect("seed page");
        pg_force_old_updated_at("shared-slug", src).await;
    }

    engine
        .update_page_contextual_retrieval_state(
            "shared-slug",
            "src-1",
            "per_chunk_synopsis",
            Some("corpus-v2"),
        )
        .await
        .expect("update_page_contextual_retrieval_state");

    let updated = engine
        .get_page("shared-slug", &get_opts("src-1", false))
        .await
        .expect("get updated page")
        .expect("updated page exists");
    assert_eq!(
        updated.contextual_retrieval_mode,
        Some(CRMode::PerChunkSynopsis)
    );
    assert_eq!(updated.corpus_generation.as_deref(), Some("corpus-v2"));
    assert!(
        !updated.updated_at.starts_with("2000-01-01"),
        "CR state update must bump updated_at, got {}",
        updated.updated_at
    );

    let untouched = engine
        .get_page("shared-slug", &get_opts("src-2", false))
        .await
        .expect("get untouched page")
        .expect("untouched page exists");
    assert_eq!(untouched.contextual_retrieval_mode, None);
    assert_eq!(untouched.corpus_generation, None);
    assert!(
        untouched.updated_at.starts_with("2000-01-01"),
        "source mismatch must remain untouched, got {}",
        untouched.updated_at
    );
    engine.disconnect().await.expect("disconnect");
}

#[tokio::test]
#[serial_test::serial]
async fn postgres_update_cr_state_accepts_null_corpus_generation() {
    let Some(engine) = pg_init_clean_engine().await else {
        eprintln!("skipping: ZBRAIN_TEST_PG_URL unset");
        return;
    };
    pg_seed_source("src-1").await;
    engine
        .put_page("cr-null", Some("src-1"), &note_input("CR Null"))
        .await
        .expect("seed page");

    engine
        .update_page_contextual_retrieval_state("cr-null", "src-1", "title", None)
        .await
        .expect("update CR state with null corpus generation");

    let page = engine
        .get_page("cr-null", &get_opts("src-1", false))
        .await
        .expect("get page")
        .expect("page exists");
    assert_eq!(page.contextual_retrieval_mode, Some(CRMode::Title));
    assert_eq!(page.corpus_generation, None);
    engine.disconnect().await.expect("disconnect");
}

#[tokio::test]
#[serial_test::serial]
async fn postgres_update_cr_state_skips_soft_deleted_rows() {
    let Some(engine) = pg_init_clean_engine().await else {
        eprintln!("skipping: ZBRAIN_TEST_PG_URL unset");
        return;
    };
    pg_seed_source("src-1").await;
    engine
        .put_page("soft-deleted", Some("src-1"), &note_input("Soft Deleted"))
        .await
        .expect("seed page");
    engine
        .soft_delete_page("soft-deleted", Some("src-1"))
        .await
        .expect("soft delete prep");

    engine
        .update_page_contextual_retrieval_state(
            "soft-deleted",
            "src-1",
            "per_chunk_synopsis",
            Some("corpus-v2"),
        )
        .await
        .expect("CR state update no-ops on soft-deleted row");

    let page = engine
        .get_page("soft-deleted", &get_opts("src-1", true))
        .await
        .expect("get soft-deleted page")
        .expect("soft-deleted page exists when include_deleted");
    assert_eq!(page.contextual_retrieval_mode, None);
    assert_eq!(page.corpus_generation, None);
    assert!(page.deleted_at.is_some(), "row remains soft-deleted");
    engine.disconnect().await.expect("disconnect");
}
