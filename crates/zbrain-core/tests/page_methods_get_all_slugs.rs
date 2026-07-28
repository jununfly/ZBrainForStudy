//! Slice 6a-libsql advanced reads (S2 RED): `get_all_slugs` behaviour tests.
//!
//! Replaces the original 6a S6-T1 placeholder-lock test (which only asserted
//! `Error::Unsupported`). libsql now mirrors the PG semantics from plan 14
//! §11.1: `SELECT slug FROM pages [WHERE source_id = ?1]` returning a
//! `HashSet<String>` of *all* rows including soft-deleted ones (matches TS
//! `pglite-engine.ts` L1071-1086 which does NOT filter `deleted_at`).
//!
//! PG mirror tests below this libsql block stay unchanged.

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

// ---------------------------------------------------------------------------
// LibsqlEngine behaviour tests (slice 6a-libsql advanced reads, S2 RED)
//
// Mirrors the PG semantics from plan 14 §11.1 — TS `pglite-engine.ts`
// L1071-1086 returns every `slug` regardless of `deleted_at` and applies
// `WHERE source_id = ?1` only when an id is supplied. libsql translates
// the PG `($1::text IS NULL OR source_id = $1)` guard to a Rust-side
// branch (build two SQL strings) and accumulates rows into a `HashSet`.
// `pages.source_id` carries a FK to `sources(id)`; only the `"default"`
// seed exists after `init_schema`, so non-default sources must be seeded
// via raw libsql connection (mirrors `libsql_engine_put_page_source_id`).
// ---------------------------------------------------------------------------

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
async fn libsql_get_all_slugs_returns_every_slug_across_sources() {
    let _guard = libsql_test_guard();
    let (engine, tmp) = init_clean_engine().await;
    libsql_seed_source(&tmp, "src-1").await;
    libsql_seed_source(&tmp, "src-2").await;
    for (slug, src) in [("alpha", "src-1"), ("beta", "src-1"), ("gamma", "src-2")] {
        engine
            .put_page(slug, Some(src), &note_input(slug, "body"))
            .await
            .expect("seed page");
    }

    let slugs = engine
        .get_all_slugs(None)
        .await
        .expect("get_all_slugs(None)");

    let expected: std::collections::HashSet<String> = ["alpha", "beta", "gamma"]
        .iter()
        .map(std::string::ToString::to_string)
        .collect();
    assert_eq!(slugs, expected);
    engine.disconnect().await.expect("disconnect");
}

#[tokio::test]
async fn libsql_get_all_slugs_filters_by_source_id_when_provided() {
    let _guard = libsql_test_guard();
    let (engine, tmp) = init_clean_engine().await;
    libsql_seed_source(&tmp, "src-1").await;
    libsql_seed_source(&tmp, "src-2").await;
    for (slug, src) in [("alpha", "src-1"), ("beta", "src-1"), ("gamma", "src-2")] {
        engine
            .put_page(slug, Some(src), &note_input(slug, "body"))
            .await
            .expect("seed page");
    }

    let scoped = engine
        .get_all_slugs(Some("src-1"))
        .await
        .expect("get_all_slugs(src-1)");

    let expected: std::collections::HashSet<String> = ["alpha", "beta"]
        .iter()
        .map(std::string::ToString::to_string)
        .collect();
    assert_eq!(scoped, expected);
    engine.disconnect().await.expect("disconnect");
}

#[tokio::test]
async fn libsql_get_all_slugs_includes_soft_deleted_rows() {
    let _guard = libsql_test_guard();
    // TS parity: `pglite-engine.ts` L1071-1086 does NOT filter
    // `deleted_at IS NULL`. libsql must keep the same quirk so analytics
    // queries see every slug ever written (PG-advanced-reads R1/R2).
    let (engine, tmp) = init_clean_engine().await;
    libsql_seed_source(&tmp, "src-1").await;
    engine
        .put_page("live-slug", Some("src-1"), &note_input("Live", "body"))
        .await
        .expect("seed live");
    engine
        .put_page(
            "tombstone-slug",
            Some("src-1"),
            &note_input("Tombstone", "body"),
        )
        .await
        .expect("seed tombstone");
    engine
        .soft_delete_page("tombstone-slug", Some("src-1"))
        .await
        .expect("soft delete tombstone");

    let slugs = engine
        .get_all_slugs(None)
        .await
        .expect("get_all_slugs(None)");

    assert!(slugs.contains("live-slug"));
    assert!(
        slugs.contains("tombstone-slug"),
        "libsql `get_all_slugs` must include soft-deleted rows (TS parity)"
    );
    engine.disconnect().await.expect("disconnect");
}

#[tokio::test]
async fn libsql_get_all_slugs_returns_empty_set_when_no_rows_match() {
    let _guard = libsql_test_guard();
    let (engine, tmp) = init_clean_engine().await;
    let empty = engine
        .get_all_slugs(None)
        .await
        .expect("get_all_slugs on empty");
    assert!(empty.is_empty());

    libsql_seed_source(&tmp, "src-1").await;
    engine
        .put_page("only-slug", Some("src-1"), &note_input("Only", "body"))
        .await
        .expect("seed page");

    let missing = engine
        .get_all_slugs(Some("src-missing"))
        .await
        .expect("get_all_slugs(src-missing)");
    assert!(missing.is_empty());
    engine.disconnect().await.expect("disconnect");
}

// ---------------------------------------------------------------------------
// PostgresEngine mirror tests (slice 6a-pg PG-advanced-reads)
//
// Locks the PG `get_all_slugs` semantics from plan 14 §11.1:
//   SELECT slug FROM pages [WHERE source_id = $1]
//   ($1::text IS NULL OR source_id = $1) guard, returns HashSet<String>.
//   Per TS `pglite-engine.ts` L1071-1086, results include soft-deleted
//   rows (NO `deleted_at IS NULL` filter).
// Uses pg-embed via PgFixture for ephemeral, isolated databases.
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
async fn postgres_get_all_slugs_returns_every_slug_across_sources() {
    let _guard = libsql_test_guard();
    let fix = support::pg_fixture::PgFixture::start().await;
    let engine = &fix.engine;
    pg_seed_source(&fix.url, "src-1").await;
    pg_seed_source(&fix.url, "src-2").await;
    for (slug, src) in [("alpha", "src-1"), ("beta", "src-1"), ("gamma", "src-2")] {
        engine
            .put_page(
                slug,
                Some(src),
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

    let slugs = engine
        .get_all_slugs(None)
        .await
        .expect("get_all_slugs(None)");

    let expected: std::collections::HashSet<String> = ["alpha", "beta", "gamma"]
        .iter()
        .map(std::string::ToString::to_string)
        .collect();
    assert_eq!(slugs, expected);
}

#[tokio::test]
async fn postgres_get_all_slugs_filters_by_source_id_when_provided() {
    let _guard = libsql_test_guard();
    let fix = support::pg_fixture::PgFixture::start().await;
    let engine = &fix.engine;
    pg_seed_source(&fix.url, "src-1").await;
    pg_seed_source(&fix.url, "src-2").await;
    for (slug, src) in [("alpha", "src-1"), ("beta", "src-1"), ("gamma", "src-2")] {
        engine
            .put_page(
                slug,
                Some(src),
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

    let scoped = engine
        .get_all_slugs(Some("src-1"))
        .await
        .expect("get_all_slugs(src-1)");

    let expected: std::collections::HashSet<String> = ["alpha", "beta"]
        .iter()
        .map(std::string::ToString::to_string)
        .collect();
    assert_eq!(scoped, expected);
}

#[tokio::test]
async fn postgres_get_all_slugs_includes_soft_deleted_rows() {
    let _guard = libsql_test_guard();
    // TS `pglite-engine.ts` L1071-1086 does NOT filter `deleted_at IS NULL`;
    // PG mirror must keep that quirk so analytics queries see every slug
    // ever written (logged as PG-advanced-reads R1/R2 in plan §11.1).
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
        .expect("seed live");
    engine
        .put_page(
            "tombstone-slug",
            Some("src-1"),
            &PageInput {
                page_type: "note".to_string(),
                title: "Tombstone".to_string(),
                compiled_truth: "body".to_string(),
                ..PageInput::default()
            },
        )
        .await
        .expect("seed tombstone");
    engine
        .soft_delete_page("tombstone-slug", Some("src-1"))
        .await
        .expect("soft delete tombstone");

    let slugs = engine
        .get_all_slugs(None)
        .await
        .expect("get_all_slugs(None)");

    assert!(slugs.contains("live-slug"));
    assert!(
        slugs.contains("tombstone-slug"),
        "PG `get_all_slugs` must include soft-deleted rows (TS parity)"
    );
}

#[tokio::test]
async fn postgres_get_all_slugs_returns_empty_set_when_no_rows_match() {
    let _guard = libsql_test_guard();
    let fix = support::pg_fixture::PgFixture::start().await;
    let engine = &fix.engine;
    let empty = engine
        .get_all_slugs(None)
        .await
        .expect("get_all_slugs on empty");
    assert!(empty.is_empty());

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

    let missing = engine
        .get_all_slugs(Some("src-missing"))
        .await
        .expect("get_all_slugs(src-missing)");
    assert!(missing.is_empty());
}
