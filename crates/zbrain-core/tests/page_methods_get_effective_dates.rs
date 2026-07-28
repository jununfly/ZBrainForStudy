//! Slice 6a-libsql advanced reads (libsql parity): `get_effective_dates` behavior tests.
//!
//! Mirrors TS-compatible effective-date fallback semantics:
//!   `SELECT p.slug, p.source_id, COALESCE(p.effective_date, p.updated_at, p.created_at) AS ts
//!    FROM pages p
//!    WHERE (p.slug, p.source_id) IN ((?1,?2), …) AND p.deleted_at IS NULL`
//! returning a `HashMap<String, String>` keyed by `format!("{source_id}::{slug}")`.
//!
//! PG mirror tests below this libsql block stay unchanged.

mod support;

use tempfile::NamedTempFile;
use zbrain_core::engine::{BrainEngine, EngineConfig, PageInput};
use zbrain_core::libsql::LibsqlEngine;
use zbrain_core::{EffectiveDateSource, PageRef};

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

fn note_input_with_effective_date(title: &str, body: &str, effective_date: &str) -> PageInput {
    PageInput {
        effective_date: Some(effective_date.to_string()),
        effective_date_source: Some(EffectiveDateSource::Filename),
        ..note_input(title, body)
    }
}

#[tokio::test]
async fn libsql_get_effective_dates_prefers_effective_date_before_row_timestamps() {
    let _guard = libsql_test_guard();
    let (engine, tmp) = init_clean_engine().await;
    libsql_seed_source(&tmp, "src-1").await;
    engine
        .put_page(
            "dated-slug",
            Some("src-1"),
            &note_input_with_effective_date("Dated title", "body", "2026-01-15"),
        )
        .await
        .expect("seed page");

    let dates = engine
        .get_effective_dates(&[PageRef {
            slug: "dated-slug".to_string(),
            source_id: "src-1".to_string(),
        }])
        .await
        .expect("get_effective_dates");

    assert_eq!(
        dates.get("src-1::dated-slug").map(String::as_str),
        Some("2026-01-15"),
        "effective_date must win over updated_at/created_at when present"
    );
    engine.disconnect().await.expect("disconnect");
}

#[tokio::test]
async fn libsql_get_effective_dates_returns_compound_key_for_each_ref() {
    let _guard = libsql_test_guard();
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
    let _guard = libsql_test_guard();
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
    let _guard = libsql_test_guard();
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
// Locks TS-compatible `get_effective_dates` fallback semantics:
//   SELECT p.slug, p.source_id,
//          COALESCE(p.effective_date, p.updated_at::text, p.created_at::text) AS ts
//   FROM pages p
//   JOIN unnest($1::text[], $2::text[]) AS u(slug, source_id)
//     ON p.slug = u.slug AND p.source_id = u.source_id
//   WHERE p.deleted_at IS NULL;
// keyed by `format!("{source_id}::{slug}")`. The unnest join enforces
// per-(slug, source_id) precision so cross-source slug collisions are
// disambiguated. Uses pg-embed via PgFixture for ephemeral, isolated
// databases. No serial gating needed — each test gets its own database.
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
async fn postgres_get_effective_dates_prefers_effective_date_before_row_timestamps() {
    let _guard = libsql_test_guard();
    let fix = support::pg_fixture::PgFixture::start().await;
    let engine = &fix.engine;
    pg_seed_source(&fix.url, "src-1").await;
    engine
        .put_page(
            "dated-slug",
            Some("src-1"),
            &note_input_with_effective_date("Dated title", "body", "2026-01-15"),
        )
        .await
        .expect("seed page");

    let dates = engine
        .get_effective_dates(&[PageRef {
            slug: "dated-slug".to_string(),
            source_id: "src-1".to_string(),
        }])
        .await
        .expect("get_effective_dates");

    assert_eq!(
        dates.get("src-1::dated-slug").map(String::as_str),
        Some("2026-01-15"),
        "effective_date must win over updated_at/created_at when present"
    );
}

#[tokio::test]
async fn postgres_get_effective_dates_returns_compound_key_for_each_ref() {
    let _guard = libsql_test_guard();
    let fix = support::pg_fixture::PgFixture::start().await;
    let engine = &fix.engine;
    pg_seed_source(&fix.url, "src-1").await;
    pg_seed_source(&fix.url, "src-2").await;
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
}

#[tokio::test]
async fn postgres_get_effective_dates_disambiguates_same_slug_across_sources() {
    let _guard = libsql_test_guard();
    // Two sources both have a page with slug `shared`. The unnest join MUST
    // return BOTH and key them by `{source_id}::{slug}` so callers can tell
    // them apart.
    let fix = support::pg_fixture::PgFixture::start().await;
    let engine = &fix.engine;
    pg_seed_source(&fix.url, "src-1").await;
    pg_seed_source(&fix.url, "src-2").await;
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
}

#[tokio::test]
async fn postgres_get_effective_dates_excludes_soft_deleted_rows() {
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
}

#[tokio::test]
async fn postgres_get_effective_dates_returns_empty_map_for_empty_input() {
    let _guard = libsql_test_guard();
    let fix = support::pg_fixture::PgFixture::start().await;
    let engine = &fix.engine;

    let dates = engine
        .get_effective_dates(&[])
        .await
        .expect("get_effective_dates on empty input");
    assert!(dates.is_empty(), "empty refs slice → empty map");
}
