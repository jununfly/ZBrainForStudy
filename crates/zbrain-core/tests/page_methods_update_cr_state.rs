//! Slice PG-advanced-writes RED: `update_page_contextual_retrieval_state` behavior tests.

mod support;

use tempfile::NamedTempFile;
use zbrain_core::engine::{BrainEngine, EngineConfig, GetPageOpts, PageInput};
use zbrain_core::libsql::LibsqlEngine;
use zbrain_core::CRMode;

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

async fn libsql_force_old_updated_at(tmp: &NamedTempFile, slug: &str, source_id: &str) {
    let path_str = tmp.path().to_string_lossy().into_owned();
    let db = ::libsql::Builder::new_local(&path_str)
        .build()
        .await
        .expect("raw db open");
    let raw_conn = db.connect().expect("raw conn");
    raw_conn
        .execute(
            "UPDATE pages \
             SET updated_at = '2000-01-01 00:00:00' \
             WHERE slug = ?1 AND source_id = ?2",
            ::libsql::params![slug, source_id],
        )
        .await
        .expect("force old updated_at");
}

#[tokio::test]
async fn libsql_update_cr_state_updates_exact_live_source_row() {
    let _guard = libsql_test_guard();
    let (engine, tmp) = init_clean_engine().await;
    libsql_seed_source(&tmp, "src-1").await;
    libsql_seed_source(&tmp, "src-2").await;
    for src in ["src-1", "src-2"] {
        engine
            .put_page("shared-slug", Some(src), &note_input("Shared"))
            .await
            .expect("seed page");
        libsql_force_old_updated_at(&tmp, "shared-slug", src).await;
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
async fn libsql_update_cr_state_accepts_null_corpus_generation() {
    let _guard = libsql_test_guard();
    let (engine, tmp) = init_clean_engine().await;
    libsql_seed_source(&tmp, "src-1").await;
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
async fn libsql_update_cr_state_skips_soft_deleted_rows() {
    let _guard = libsql_test_guard();
    let (engine, tmp) = init_clean_engine().await;
    libsql_seed_source(&tmp, "src-1").await;
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

// ---------------------------------------------------------------------------
// PostgresEngine mirror tests (PG-advanced-writes: CR state)
//
// Mirrors TS `updatePageContextualRetrievalState`: update
// `contextual_retrieval_mode`, `corpus_generation`, and `updated_at` for
// exactly one live `(source_id, slug)` row. Soft-deleted rows are skipped by
// `deleted_at IS NULL`.
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

async fn pg_force_old_updated_at(url: &str, slug: &str, source_id: &str) {
    let pool = sqlx::postgres::PgPoolOptions::new()
        .max_connections(1)
        .connect(url)
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
async fn postgres_update_cr_state_updates_exact_live_source_row() {
    let _guard = libsql_test_guard();
    let fix = support::pg_fixture::PgFixture::start().await;
    let engine = &fix.engine;
    pg_seed_source(&fix.url, "src-1").await;
    pg_seed_source(&fix.url, "src-2").await;
    for src in ["src-1", "src-2"] {
        engine
            .put_page("shared-slug", Some(src), &note_input("Shared"))
            .await
            .expect("seed page");
        pg_force_old_updated_at(&fix.url, "shared-slug", src).await;
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
}

#[tokio::test]
async fn postgres_update_cr_state_accepts_null_corpus_generation() {
    let _guard = libsql_test_guard();
    let fix = support::pg_fixture::PgFixture::start().await;
    let engine = &fix.engine;
    pg_seed_source(&fix.url, "src-1").await;
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
}

#[tokio::test]
async fn postgres_update_cr_state_skips_soft_deleted_rows() {
    let _guard = libsql_test_guard();
    let fix = support::pg_fixture::PgFixture::start().await;
    let engine = &fix.engine;
    pg_seed_source(&fix.url, "src-1").await;
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
}
