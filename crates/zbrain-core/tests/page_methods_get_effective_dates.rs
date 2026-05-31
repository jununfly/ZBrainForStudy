//! Slice 6a-libsql advanced reads (S2 RED): `get_effective_dates` behavior tests.
//!
//! Mirrors the PG semantics locked in slice 6a-pg (plan 14 §11.1):
//!   `SELECT p.slug, p.source_id, COALESCE(p.updated_at, p.created_at) AS ts
//!    FROM pages p
//!    WHERE (p.slug, p.source_id) IN ((?1,?2), …) AND p.deleted_at IS NULL`
//! returning a `HashMap<String, String>` keyed by `format!("{source_id}::{slug}")`.
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
async fn libsql_get_effective_dates_returns_compound_key_for_each_ref() {
    let (engine, tmp) = init_clean_engine().await;
    libsql_seed_source(&tmp, "src-1").await;
    libsql_seed_source(&tmp, "src-2").await;
    for (slug, src) in [("alpha", "src-1"), ("beta", "src-2")] {
        engine
            .put_page(slug, Some(src), &note_input(slug, "body"))
            .await
            .expect("seed page");
    }

    let refs = vec![
        PageRef {
            slug: "alpha".to_string(),
            source_id: "src-1".to_string(),
        },
        PageRef {
            slug: "beta".to_string(),
            source_id: "src-2".to_string(),
        },
    ];
    let dates = engine
        .get_effective_dates(&refs)
        .await
        .expect("get_effective_dates");

    assert_eq!(dates.len(), 2, "two refs requested → two entries");
    for (slug, src) in [("alpha", "src-1"), ("beta", "src-2")] {
        let key = format!("{src}::{slug}");
        let ts = dates
            .get(&key)
            .unwrap_or_else(|| panic!("missing key {key}"));
        assert!(
            ts.len() >= 10 && ts.starts_with("20"),
            "expected ISO-8601 ts for {key}, got {ts}"
        );
    }
    engine.disconnect().await.expect("disconnect");
}

#[tokio::test]
async fn libsql_get_effective_dates_disambiguates_same_slug_across_sources() {
    // Two sources both have a page with slug `shared`. The implementation MUST
    // return BOTH and key them by `{source_id}::{slug}` so callers can tell
    // them apart.
    let (engine, tmp) = init_clean_engine().await;
    libsql_seed_source(&tmp, "src-1").await;
    libsql_seed_source(&tmp, "src-2").await;
    for src in ["src-1", "src-2"] {
        engine
            .put_page("shared", Some(src), &note_input("Shared", "body"))
            .await
            .expect("seed page");
    }

    let refs = vec![
        PageRef {
            slug: "shared".to_string(),
            source_id: "src-1".to_string(),
        },
        PageRef {
            slug: "shared".to_string(),
            source_id: "src-2".to_string(),
        },
    ];
    let dates = engine
        .get_effective_dates(&refs)
        .await
        .expect("get_effective_dates");

    assert_eq!(dates.len(), 2);
    assert!(dates.contains_key("src-1::shared"));
    assert!(dates.contains_key("src-2::shared"));
    engine.disconnect().await.expect("disconnect");
}

#[tokio::test]
async fn libsql_get_effective_dates_excludes_soft_deleted_rows() {
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

    let refs = vec![
        PageRef {
            slug: "live-slug".to_string(),
            source_id: "src-1".to_string(),
        },
        PageRef {
            slug: "tombstone-slug".to_string(),
            source_id: "src-1".to_string(),
        },
    ];
    let dates = engine
        .get_effective_dates(&refs)
        .await
        .expect("get_effective_dates");

    assert!(dates.contains_key("src-1::live-slug"));
    assert!(
        !dates.contains_key("src-1::tombstone-slug"),
        "soft-deleted rows must be excluded by `deleted_at IS NULL` filter"
    );
    assert_eq!(dates.len(), 1);
    engine.disconnect().await.expect("disconnect");
}

#[tokio::test]
async fn libsql_get_effective_dates_returns_empty_map_for_empty_input() {
    let (engine, _tmp) = init_clean_engine().await;

    let dates = engine
        .get_effective_dates(&[])
        .await
        .expect("get_effective_dates on empty input");
    assert!(dates.is_empty(), "empty refs slice → empty map");
    engine.disconnect().await.expect("disconnect");
}

// ---------------------------------------------------------------------------
// PostgresEngine mirror tests (slice 6a-pg PG-advanced-reads)
//
// Locks the PG `get_effective_dates` semantics from plan 14 §11.1:
//   SELECT p.slug, p.source_id, COALESCE(p.updated_at, p.created_at)::text AS ts
//   FROM pages p
//   JOIN unnest($1::text[], $2::text[]) AS u(slug, source_id)
//     ON p.slug = u.slug AND p.source_id = u.source_id
//   WHERE p.deleted_at IS NULL;
// keyed by `format!("{source_id}::{slug}")`. The unnest join enforces
// per-(slug, source_id) precision so cross-source slug collisions are
// disambiguated. Gated on `ZBRAIN_TEST_PG_URL`. `#[serial_test::serial]`
// because they share the `pages` table in the configured test database.
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
async fn postgres_get_effective_dates_returns_compound_key_for_each_ref() {
    let Some(engine) = pg_init_clean_engine().await else {
        eprintln!("skipping: ZBRAIN_TEST_PG_URL unset");
        return;
    };
    pg_seed_source("src-1").await;
    pg_seed_source("src-2").await;
    for (slug, src) in [("alpha", "src-1"), ("beta", "src-2")] {
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

    let refs = vec![
        PageRef {
            slug: "alpha".to_string(),
            source_id: "src-1".to_string(),
        },
        PageRef {
            slug: "beta".to_string(),
            source_id: "src-2".to_string(),
        },
    ];
    let dates = engine
        .get_effective_dates(&refs)
        .await
        .expect("get_effective_dates");

    assert_eq!(dates.len(), 2, "two refs requested → two entries");
    for (slug, src) in [("alpha", "src-1"), ("beta", "src-2")] {
        let key = format!("{src}::{slug}");
        let ts = dates
            .get(&key)
            .unwrap_or_else(|| panic!("missing key {key}"));
        assert!(
            ts.len() >= 10 && ts.starts_with("20"),
            "expected ISO-8601 ts for {key}, got {ts}"
        );
    }
    engine.disconnect().await.expect("disconnect");
}

#[tokio::test]
#[serial_test::serial]
async fn postgres_get_effective_dates_disambiguates_same_slug_across_sources() {
    // Two sources both have a page with slug `shared`. The unnest join MUST
    // return BOTH and key them by `{source_id}::{slug}` so callers can tell
    // them apart.
    let Some(engine) = pg_init_clean_engine().await else {
        eprintln!("skipping: ZBRAIN_TEST_PG_URL unset");
        return;
    };
    pg_seed_source("src-1").await;
    pg_seed_source("src-2").await;
    for src in ["src-1", "src-2"] {
        engine
            .put_page(
                "shared",
                Some(src),
                &PageInput {
                    page_type: "note".to_string(),
                    title: "Shared".to_string(),
                    compiled_truth: "body".to_string(),
                    ..PageInput::default()
                },
            )
            .await
            .expect("seed page");
    }

    let refs = vec![
        PageRef {
            slug: "shared".to_string(),
            source_id: "src-1".to_string(),
        },
        PageRef {
            slug: "shared".to_string(),
            source_id: "src-2".to_string(),
        },
    ];
    let dates = engine
        .get_effective_dates(&refs)
        .await
        .expect("get_effective_dates");

    assert_eq!(dates.len(), 2);
    assert!(dates.contains_key("src-1::shared"));
    assert!(dates.contains_key("src-2::shared"));
    engine.disconnect().await.expect("disconnect");
}

#[tokio::test]
#[serial_test::serial]
async fn postgres_get_effective_dates_excludes_soft_deleted_rows() {
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

    let refs = vec![
        PageRef {
            slug: "live-slug".to_string(),
            source_id: "src-1".to_string(),
        },
        PageRef {
            slug: "tombstone-slug".to_string(),
            source_id: "src-1".to_string(),
        },
    ];
    let dates = engine
        .get_effective_dates(&refs)
        .await
        .expect("get_effective_dates");

    assert!(dates.contains_key("src-1::live-slug"));
    assert!(
        !dates.contains_key("src-1::tombstone-slug"),
        "soft-deleted rows must be excluded by `deleted_at IS NULL` filter"
    );
    assert_eq!(dates.len(), 1);
    engine.disconnect().await.expect("disconnect");
}

#[tokio::test]
#[serial_test::serial]
async fn postgres_get_effective_dates_returns_empty_map_for_empty_input() {
    let Some(engine) = pg_init_clean_engine().await else {
        eprintln!("skipping: ZBRAIN_TEST_PG_URL unset");
        return;
    };

    let dates = engine
        .get_effective_dates(&[])
        .await
        .expect("get_effective_dates on empty input");
    assert!(dates.is_empty(), "empty refs slice → empty map");
    engine.disconnect().await.expect("disconnect");
}
