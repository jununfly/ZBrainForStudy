//! Slice 6a PG-find-orphan-pages tests.
//!
//! Part 1 (libsql): mirror behavior tests for `find_orphan_pages`.
//!
//! Part 2 (Postgres): mirror integration tests gated on `ZBRAIN_TEST_PG_URL`.
//! These prove the PG `find_orphan_pages` override matches the TS contract
//! (codex C11: bilateral soft-delete filter, COALESCE title, domain
//! extraction, slug ordering).

use tempfile::NamedTempFile;
use zbrain_core::engine::{BrainEngine, EngineConfig, PageInput};
use zbrain_core::libsql::LibsqlEngine;

// ---------------------------------------------------------------------------
// LibsqlEngine mirror tests (slice 6a-libsql find-orphan-pages)
// ---------------------------------------------------------------------------

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

async fn libsql_page_id(tmp: &NamedTempFile, slug: &str) -> i64 {
    let path_str = tmp.path().to_string_lossy().into_owned();
    let db = ::libsql::Builder::new_local(&path_str)
        .build()
        .await
        .expect("raw db");
    let raw_conn = db.connect().expect("raw conn");
    let mut rows = raw_conn
        .query(
            "SELECT id FROM pages WHERE slug = ?1",
            ::libsql::params![slug],
        )
        .await
        .expect("page id query");
    let row = rows
        .next()
        .await
        .expect("page id row fetch")
        .expect("page must exist");
    row.get(0).expect("decode page id")
}

async fn libsql_insert_link(tmp: &NamedTempFile, from_slug: &str, to_slug: &str, link_type: &str) {
    let from_id = libsql_page_id(tmp, from_slug).await;
    let to_id = libsql_page_id(tmp, to_slug).await;
    let path_str = tmp.path().to_string_lossy().into_owned();
    let db = ::libsql::Builder::new_local(&path_str)
        .build()
        .await
        .expect("raw db");
    let raw_conn = db.connect().expect("raw conn");
    raw_conn
        .execute(
            "INSERT INTO links (from_page_id, to_page_id, link_type, link_source) \
             VALUES (?1, ?2, ?3, 'markdown')",
            ::libsql::params![from_id, to_id, link_type],
        )
        .await
        .expect("insert link");
}

async fn libsql_soft_delete(tmp: &NamedTempFile, slug: &str) {
    let path_str = tmp.path().to_string_lossy().into_owned();
    let db = ::libsql::Builder::new_local(&path_str)
        .build()
        .await
        .expect("raw db");
    let raw_conn = db.connect().expect("raw conn");
    raw_conn
        .execute(
            "UPDATE pages SET deleted_at = CURRENT_TIMESTAMP WHERE slug = ?1 AND deleted_at IS NULL",
            ::libsql::params![slug],
        )
        .await
        .expect("soft delete page");
}

#[tokio::test]
async fn libsql_find_orphan_pages_mirrors_ts_contract() {
    let (engine, tmp) = init_clean_engine().await;

    for (slug, title, domain) in [
        ("alpha", "Alpha", Some("research")),
        ("bravo", "Bravo", None),
        ("charlie", "Charlie", None),
        ("delta", "Delta", None),
        ("echo", "Echo", None),
        ("zulu", "Zulu", None),
    ] {
        let frontmatter = domain.map_or_else(|| json!({}), |d| json!({"domain": d}));
        engine
            .put_page(
                slug,
                Some("default"),
                &PageInput {
                    page_type: "note".to_string(),
                    title: title.to_string(),
                    compiled_truth: "body".to_string(),
                    frontmatter: Some(frontmatter),
                    ..PageInput::default()
                },
            )
            .await
            .expect("seed page");
    }

    // charlie -> bravo: bravo has live inbound and is not an orphan.
    libsql_insert_link(&tmp, "charlie", "bravo", "link").await;
    // echo -> delta, then soft-delete echo: deleted-source inbound is ignored,
    // and echo itself must not appear as a candidate.
    libsql_insert_link(&tmp, "echo", "delta", "link").await;
    libsql_soft_delete(&tmp, "echo").await;

    let orphans = engine.find_orphan_pages().await.expect("find_orphan_pages");
    let slugs: Vec<&str> = orphans.iter().map(|o| o.slug.as_str()).collect();
    assert_eq!(
        slugs,
        vec!["alpha", "charlie", "delta", "zulu"],
        "must filter live inbound links, ignore deleted-source inbound links, exclude deleted candidates, and order by slug"
    );

    let by_slug: std::collections::HashMap<&str, &OrphanPage> =
        orphans.iter().map(|o| (o.slug.as_str(), o)).collect();
    let alpha = by_slug.get("alpha").expect("alpha must be orphan");
    assert_eq!(alpha.title, "Alpha");
    assert_eq!(alpha.domain.as_deref(), Some("research"));

    engine.disconnect().await.expect("disconnect");
}

// ---------------------------------------------------------------------------
// PostgresEngine mirror tests (slice 6a-pg PG-find-orphan-pages)
//
// Gated on `ZBRAIN_TEST_PG_URL` (same convention as other PG integration
// tests). Each test is `#[serial_test::serial]` because they share the
// `pages` + `links` tables in the configured test database.
// ---------------------------------------------------------------------------

use serde_json::json;
use zbrain_core::postgres::PostgresEngine;
use zbrain_core::types::OrphanPage;

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
    // TRUNCATE cascades into links via FK ON DELETE CASCADE.
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

/// Resolve a slug to its `pages.id` via direct query (test helper).
async fn pg_page_id(slug: &str) -> i64 {
    let url = pg_url().expect("ZBRAIN_TEST_PG_URL set for page id lookup");
    let pool = sqlx::postgres::PgPoolOptions::new()
        .max_connections(1)
        .connect(&url)
        .await
        .expect("page id pool");
    let row: (i64,) = sqlx::query_as("SELECT id FROM pages WHERE slug = $1")
        .bind(slug)
        .fetch_one(&pool)
        .await
        .expect("page must exist");
    pool.close().await;
    row.0
}

/// Insert a link row directly into the `links` table (test helper).
/// The `BrainEngine` trait has no `put_link` yet — that's a later slice — so
/// tests must write via raw SQL.
async fn pg_insert_link(from_slug: &str, to_slug: &str, link_type: &str) {
    let from_id = pg_page_id(from_slug).await;
    let to_id = pg_page_id(to_slug).await;
    let url = pg_url().expect("ZBRAIN_TEST_PG_URL set for link insert");
    let pool = sqlx::postgres::PgPoolOptions::new()
        .max_connections(1)
        .connect(&url)
        .await
        .expect("link insert pool");
    sqlx::query(
        "INSERT INTO links (from_page_id, to_page_id, link_type, link_source) \
         VALUES ($1, $2, $3, 'markdown')",
    )
    .bind(from_id)
    .bind(to_id)
    .bind(link_type)
    .execute(&pool)
    .await
    .expect("insert link");
    pool.close().await;
}

/// Soft-delete a page by slug directly (test helper for C11 verification).
async fn pg_soft_delete(slug: &str) {
    let url = pg_url().expect("ZBRAIN_TEST_PG_URL set for soft delete");
    let pool = sqlx::postgres::PgPoolOptions::new()
        .max_connections(1)
        .connect(&url)
        .await
        .expect("soft delete pool");
    sqlx::query("UPDATE pages SET deleted_at = now() WHERE slug = $1 AND deleted_at IS NULL")
        .bind(slug)
        .execute(&pool)
        .await
        .expect("soft delete page");
    pool.close().await;
}

// ---- Test cases ----

/// C11 baseline: a page with zero inbound links is an orphan.
#[tokio::test]
#[serial_test::serial]
async fn postgres_find_orphan_pages_returns_page_with_no_inbound_links() {
    let Some(engine) = pg_init_clean_engine().await else {
        eprintln!("skipping: ZBRAIN_TEST_PG_URL unset");
        return;
    };
    pg_seed_source("src-orphan").await;
    engine
        .put_page(
            "alpha",
            Some("src-orphan"),
            &PageInput {
                page_type: "note".to_string(),
                title: "Alpha".to_string(),
                compiled_truth: "body".to_string(),
                frontmatter: Some(json!({"domain": "research"})),
                ..PageInput::default()
            },
        )
        .await
        .expect("seed alpha");

    let orphans = engine.find_orphan_pages().await.expect("find_orphan_pages");
    assert_eq!(orphans.len(), 1);
    assert_eq!(orphans[0].slug, "alpha");
    assert_eq!(orphans[0].title, "Alpha");
    assert_eq!(orphans[0].domain.as_deref(), Some("research"));
    engine.disconnect().await.expect("disconnect");
}

/// A page that has an inbound link from a live page is NOT an orphan.
#[tokio::test]
#[serial_test::serial]
async fn postgres_find_orphan_pages_excludes_page_with_live_inbound_link() {
    let Some(engine) = pg_init_clean_engine().await else {
        eprintln!("skipping: ZBRAIN_TEST_PG_URL unset");
        return;
    };
    pg_seed_source("src-link").await;
    engine
        .put_page(
            "bravo",
            Some("src-link"),
            &PageInput {
                page_type: "note".to_string(),
                title: "Bravo".to_string(),
                compiled_truth: "body".to_string(),
                ..PageInput::default()
            },
        )
        .await
        .expect("seed bravo");
    engine
        .put_page(
            "charlie",
            Some("src-link"),
            &PageInput {
                page_type: "note".to_string(),
                title: "Charlie".to_string(),
                compiled_truth: "body".to_string(),
                ..PageInput::default()
            },
        )
        .await
        .expect("seed charlie");

    // charlie → bravo link: bravo is no longer an orphan.
    pg_insert_link("charlie", "bravo", "link").await;

    let orphans = engine.find_orphan_pages().await.expect("find_orphan_pages");
    // charlie has no inbound links → orphan; bravo has inbound from charlie → not orphan.
    assert_eq!(orphans.len(), 1);
    assert_eq!(orphans[0].slug, "charlie");
    engine.disconnect().await.expect("disconnect");
}

/// C11 bilateral soft-delete filter: an inbound link from a soft-deleted
/// page does NOT disqualify a page from being an orphan.
#[tokio::test]
#[serial_test::serial]
async fn postgres_find_orphan_pages_treats_link_from_deleted_page_as_absent() {
    let Some(engine) = pg_init_clean_engine().await else {
        eprintln!("skipping: ZBRAIN_TEST_PG_URL unset");
        return;
    };
    pg_seed_source("src-c11").await;
    engine
        .put_page(
            "delta",
            Some("src-c11"),
            &PageInput {
                page_type: "note".to_string(),
                title: "Delta".to_string(),
                compiled_truth: "body".to_string(),
                ..PageInput::default()
            },
        )
        .await
        .expect("seed delta");
    engine
        .put_page(
            "echo",
            Some("src-c11"),
            &PageInput {
                page_type: "note".to_string(),
                title: "Echo".to_string(),
                compiled_truth: "body".to_string(),
                ..PageInput::default()
            },
        )
        .await
        .expect("seed echo");

    // echo → delta link, then soft-delete echo.
    pg_insert_link("echo", "delta", "link").await;
    pg_soft_delete("echo").await;

    let orphans = engine.find_orphan_pages().await.expect("find_orphan_pages");
    // delta is orphan (only inbound link is from deleted echo).
    // echo is excluded because it's soft-deleted itself (candidate-side filter).
    let orphan_slugs: Vec<&str> = orphans.iter().map(|o| o.slug.as_str()).collect();
    assert!(
        orphan_slugs.contains(&"delta"),
        "delta should be orphan: {orphan_slugs:?}"
    );
    assert!(
        !orphan_slugs.contains(&"echo"),
        "deleted echo must not appear: {orphan_slugs:?}"
    );
    engine.disconnect().await.expect("disconnect");
}

/// Domain extraction: `frontmatter->>'domain'` returns NULL when absent and
/// returns the string value when present. `title` is `TEXT NOT NULL` in the
/// canonical TS schema, so the `COALESCE(title, slug)` clause is defensive —
/// in practice an empty title stays empty (matches TS `findOrphanPages`).
#[tokio::test]
#[serial_test::serial]
async fn postgres_find_orphan_pages_title_coalesce_and_domain_extraction() {
    let Some(engine) = pg_init_clean_engine().await else {
        eprintln!("skipping: ZBRAIN_TEST_PG_URL unset");
        return;
    };
    pg_seed_source("src-title").await;
    engine
        .put_page(
            "no-title",
            Some("src-title"),
            &PageInput {
                page_type: "note".to_string(),
                title: String::new(),
                compiled_truth: "body".to_string(),
                frontmatter: Some(json!({})),
                ..PageInput::default()
            },
        )
        .await
        .expect("seed no-title");
    engine
        .put_page(
            "with-domain",
            Some("src-title"),
            &PageInput {
                page_type: "note".to_string(),
                title: "Titled".to_string(),
                compiled_truth: "body".to_string(),
                frontmatter: Some(json!({"domain": "engineering"})),
                ..PageInput::default()
            },
        )
        .await
        .expect("seed with-domain");

    let orphans = engine.find_orphan_pages().await.expect("find_orphan_pages");
    let by_slug: std::collections::HashMap<&str, &OrphanPage> =
        orphans.iter().map(|o| (o.slug.as_str(), o)).collect();

    let no_title = by_slug.get("no-title").expect("no-title must be orphan");
    // TS schema: title TEXT NOT NULL. Empty string stays empty after COALESCE.
    assert_eq!(no_title.title, "", "empty title remains empty (TS parity)");
    assert_eq!(no_title.domain, None, "no domain key → None");

    let with_domain = by_slug
        .get("with-domain")
        .expect("with-domain must be orphan");
    assert_eq!(with_domain.title, "Titled");
    assert_eq!(with_domain.domain.as_deref(), Some("engineering"));
    engine.disconnect().await.expect("disconnect");
}

/// Results must be ordered by slug ascending.
#[tokio::test]
#[serial_test::serial]
async fn postgres_find_orphan_pages_returns_results_ordered_by_slug() {
    let Some(engine) = pg_init_clean_engine().await else {
        eprintln!("skipping: ZBRAIN_TEST_PG_URL unset");
        return;
    };
    pg_seed_source("src-order").await;
    for slug in ["zulu", "mike", "alpha-2"] {
        engine
            .put_page(
                slug,
                Some("src-order"),
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

    let orphans = engine.find_orphan_pages().await.expect("find_orphan_pages");
    let slugs: Vec<&str> = orphans.iter().map(|o| o.slug.as_str()).collect();
    assert_eq!(
        slugs,
        vec!["alpha-2", "mike", "zulu"],
        "must be ORDER BY p.slug"
    );
    engine.disconnect().await.expect("disconnect");
}
