//! Slice 6a-libsql advanced reads (S2 RED): `list_all_page_refs` behavior tests.
//!
//! Mirrors the PG semantics locked in slice 6a-pg (plan 14 §11.1):
//!   `SELECT slug, source_id FROM pages`
//!   `WHERE deleted_at IS NULL`
//!   `ORDER BY source_id, slug;`
//! (i.e. only live rows, deterministic ordering).
//!
//! These tests are RED until S3 GREEN replaces the libsql default
//! `Err(Error::unsupported("pending slice 6a"))` with a real implementation.

use tempfile::NamedTempFile;
use zbrain_core::engine::{BrainEngine, EngineConfig, PageInput};
use zbrain_core::libsql::LibsqlEngine;
use zbrain_core::PageRef;

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

/// Seed a non-default `sources` row via a raw libsql connection.
/// `init_schema` only seeds the `'default'` source, but `pages.source_id`
/// has `REFERENCES sources(id) ON DELETE CASCADE`, so any custom source_id
/// must exist before `put_page` can succeed.
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
async fn libsql_list_all_page_refs_returns_live_refs_ordered_by_source_then_slug() {
    let (engine, tmp) = init_clean_engine().await;
    libsql_seed_source(&tmp, "src-1").await;
    libsql_seed_source(&tmp, "src-2").await;
    // Insert out-of-order to force the engine's ORDER BY to do the work.
    for (slug, src) in [
        ("gamma", "src-2"),
        ("beta", "src-1"),
        ("alpha", "src-1"),
        ("delta", "src-2"),
    ] {
        engine
            .put_page(slug, Some(src), &note_input(slug, "body"))
            .await
            .expect("seed page");
    }

    let refs = engine
        .list_all_page_refs()
        .await
        .expect("list_all_page_refs");

    let expected = vec![
        PageRef {
            slug: "alpha".to_string(),
            source_id: "src-1".to_string(),
        },
        PageRef {
            slug: "beta".to_string(),
            source_id: "src-1".to_string(),
        },
        PageRef {
            slug: "delta".to_string(),
            source_id: "src-2".to_string(),
        },
        PageRef {
            slug: "gamma".to_string(),
            source_id: "src-2".to_string(),
        },
    ];
    assert_eq!(
        refs, expected,
        "refs must be ordered by (source_id, slug) ascending"
    );
    engine.disconnect().await.expect("disconnect");
}

#[tokio::test]
async fn libsql_list_all_page_refs_excludes_soft_deleted_rows() {
    // Mirrors plan §11.1: list_all_page_refs MUST filter `deleted_at IS NULL`.
    // Contrasts with `get_all_slugs` which intentionally keeps tombstones
    // (TS quirk). Both behaviors are pinned by libsql + PG mirror tests.
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

    let refs = engine
        .list_all_page_refs()
        .await
        .expect("list_all_page_refs");

    assert_eq!(
        refs,
        vec![PageRef {
            slug: "live-slug".to_string(),
            source_id: "src-1".to_string(),
        }],
        "tombstoned rows must be excluded"
    );
    engine.disconnect().await.expect("disconnect");
}

#[tokio::test]
async fn libsql_list_all_page_refs_returns_empty_vec_when_no_live_rows() {
    let (engine, tmp) = init_clean_engine().await;

    // Empty table → empty vec.
    let empty = engine
        .list_all_page_refs()
        .await
        .expect("list_all_page_refs on empty");
    assert!(empty.is_empty(), "empty table must produce empty vec");

    // Single tombstone row → still empty (no live rows).
    libsql_seed_source(&tmp, "src-1").await;
    engine
        .put_page(
            "only-tombstone",
            Some("src-1"),
            &note_input("Tomb", "body"),
        )
        .await
        .expect("seed page");
    engine
        .soft_delete_page("only-tombstone", Some("src-1"))
        .await
        .expect("soft delete");

    let after_tombstone = engine
        .list_all_page_refs()
        .await
        .expect("list_all_page_refs after tombstone");
    assert!(
        after_tombstone.is_empty(),
        "table with only tombstones must produce empty vec"
    );
    engine.disconnect().await.expect("disconnect");
}

// ---------------------------------------------------------------------------
// PostgresEngine mirror tests (slice 6a-pg PG-advanced-reads)
//
// Locks the PG `list_all_page_refs` semantics from plan 14 §11.1:
//   SELECT slug, source_id FROM pages
//   WHERE deleted_at IS NULL
//   ORDER BY source_id, slug;
// (i.e. ONLY live rows, deterministic (source_id, slug) ordering).
// Gated on `ZBRAIN_TEST_PG_URL`. `#[serial_test::serial]` because they
// share the `pages` table in the configured test database.
// ---------------------------------------------------------------------------

use zbrain_core::postgres::PostgresEngine;

fn pg_url() -> Option<String> {
    // LOCAL PG NOTE: working Homebrew PostgreSQL 16.14 on `localhost:5434`,
    // db `zbrain_test`. URL persisted in gitignored `<repo-root>/.env`:
    //   ZBRAIN_TEST_PG_URL=postgres://postgres:postgres@localhost:5434/zbrain_test
    // Activate: `set -a; source .env; set +a`. `skipping: ... unset` ≠ valid
    // PG result — re-source `.env`. Source of truth:
    // docs/plans/20260526/17-session-state-110c.md L139-150 (#110-c, 2026-05-30).
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

#[tokio::test]
#[serial_test::serial]
async fn postgres_list_all_page_refs_returns_live_refs_ordered_by_source_then_slug() {
    let Some(engine) = pg_init_clean_engine().await else {
        eprintln!("skipping: ZBRAIN_TEST_PG_URL unset");
        return;
    };
    pg_seed_source("src-1").await;
    pg_seed_source("src-2").await;
    // Insert out-of-order to force the engine's ORDER BY to do the work.
    for (slug, src) in [
        ("gamma", "src-2"),
        ("beta", "src-1"),
        ("alpha", "src-1"),
        ("delta", "src-2"),
    ] {
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

    let refs = engine
        .list_all_page_refs()
        .await
        .expect("list_all_page_refs");

    let expected = vec![
        PageRef {
            slug: "alpha".to_string(),
            source_id: "src-1".to_string(),
        },
        PageRef {
            slug: "beta".to_string(),
            source_id: "src-1".to_string(),
        },
        PageRef {
            slug: "delta".to_string(),
            source_id: "src-2".to_string(),
        },
        PageRef {
            slug: "gamma".to_string(),
            source_id: "src-2".to_string(),
        },
    ];
    assert_eq!(
        refs, expected,
        "refs must be ordered by (source_id, slug) ascending"
    );
    engine.disconnect().await.expect("disconnect");
}

#[tokio::test]
#[serial_test::serial]
async fn postgres_list_all_page_refs_excludes_soft_deleted_rows() {
    // Mirrors plan §11.1: list_all_page_refs MUST filter `deleted_at IS NULL`.
    // Contrasts with `get_all_slugs` which intentionally keeps tombstones
    // (TS quirk). PG-advanced-reads R1/R2 documents both behaviors.
    let Some(engine) = pg_init_clean_engine().await else {
        eprintln!("skipping: ZBRAIN_TEST_PG_URL unset");
        return;
    };
    pg_seed_source("src-1").await;
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

    let refs = engine
        .list_all_page_refs()
        .await
        .expect("list_all_page_refs");

    assert_eq!(
        refs,
        vec![PageRef {
            slug: "live-slug".to_string(),
            source_id: "src-1".to_string(),
        }],
        "tombstoned rows must be excluded"
    );
    engine.disconnect().await.expect("disconnect");
}

#[tokio::test]
#[serial_test::serial]
async fn postgres_list_all_page_refs_returns_empty_vec_when_no_live_rows() {
    let Some(engine) = pg_init_clean_engine().await else {
        eprintln!("skipping: ZBRAIN_TEST_PG_URL unset");
        return;
    };

    // Empty table → empty vec.
    let empty = engine
        .list_all_page_refs()
        .await
        .expect("list_all_page_refs on empty");
    assert!(empty.is_empty(), "empty table must produce empty vec");

    // Single tombstone row → still empty (no live rows).
    pg_seed_source("src-1").await;
    engine
        .put_page(
            "only-tombstone",
            Some("src-1"),
            &PageInput {
                page_type: "note".to_string(),
                title: "Tomb".to_string(),
                compiled_truth: "body".to_string(),
                ..PageInput::default()
            },
        )
        .await
        .expect("seed page");
    engine
        .soft_delete_page("only-tombstone", Some("src-1"))
        .await
        .expect("soft delete");

    let after_tombstone = engine
        .list_all_page_refs()
        .await
        .expect("list_all_page_refs after tombstone");
    assert!(
        after_tombstone.is_empty(),
        "table with only tombstones must produce empty vec"
    );
    engine.disconnect().await.expect("disconnect");
}
