//! Slice 4b - `PostgresEngine` Page CRUD integration tests.
//!
//! Gated on `ZBRAIN_TEST_PG_URL` (same pattern as the lifecycle suite).
//! Each test connects to a clean schema via `init_clean_engine()`, truncates
//! the `pages` table from prior test runs, then exercises one CRUD method.
//!
//! Slice #110-g: every test is `#[serial_test::serial]` because all of them
//! share the same `pages` table in the test database. Cargo runs tests
//! multi-threaded by default, which races TRUNCATE against sibling INSERTs
//! and causes flaky failures. `#[serial]` enforces a global mutex.
//!
//! Test groups:
//! - `get_page`: not-found / found / source scoping / `include_deleted`
//!   hides soft-deleted rows by default and returns them when set
//! - `put_page`: insert / upsert (same slug -> updated row, id may change)
//! - `delete_page`: row vanishes, no-op on missing
//! - `list_pages`: empty / `page_type` filter / limit truncation
//! - `resolve_slugs`: exact match only (fuzzy deferred to slice 6.5c)

use zbrain_core::engine::{
    BrainEngine, EngineConfig, GetPageOpts, PageFilters, PageInput, PageSort,
};
use zbrain_core::postgres::PostgresEngine;

fn pg_url() -> Option<String> {
    std::env::var("ZBRAIN_TEST_PG_URL").ok()
}

/// Create a connected, schema-initialized engine and wipe any leftover rows
/// from previous test runs. Returns None when `ZBRAIN_TEST_PG_URL` is unset so
/// callers can skip cleanly without panicking.
async fn init_clean_engine() -> Option<PostgresEngine> {
    let url = pg_url()?;
    let engine = PostgresEngine::new();
    let cfg = EngineConfig {
        database_url: Some(url.clone()),
        database_path: None,
    };
    engine.connect(&cfg).await.expect("connect");
    engine.init_schema().await.expect("init_schema");

    // Truncate via a side-channel pool so we do not need a trait method for it.
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

fn note_input(title: &str, body: &str) -> PageInput {
    PageInput {
        page_type: "note".to_string(),
        title: title.to_string(),
        compiled_truth: body.to_string(),
        ..Default::default()
    }
}

async fn seed_source(id: &str) {
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

async fn source_ids_for_slug(slug: &str) -> Vec<String> {
    let url = pg_url().expect("ZBRAIN_TEST_PG_URL set for source verification");
    let pool = sqlx::postgres::PgPoolOptions::new()
        .max_connections(1)
        .connect(&url)
        .await
        .expect("source verification pool");
    let rows = sqlx::query_scalar::<_, String>(
        "SELECT source_id FROM pages WHERE slug = $1 ORDER BY source_id ASC",
    )
    .bind(slug)
    .fetch_all(&pool)
    .await
    .expect("select page source ids");
    pool.close().await;
    rows
}

// -- get_page --------------------------------------------------------------

#[tokio::test]
#[serial_test::serial]
async fn get_page_returns_none_when_slug_missing() {
    let Some(engine) = init_clean_engine().await else {
        eprintln!("skipping: ZBRAIN_TEST_PG_URL unset");
        return;
    };
    let got = engine
        .get_page("does-not-exist", &GetPageOpts::default())
        .await
        .expect("get_page");
    assert!(got.is_none(), "missing slug must return None, got {got:?}");
    engine.disconnect().await.expect("disconnect");
}

#[tokio::test]
#[serial_test::serial]
async fn get_page_round_trips_after_put() {
    let Some(engine) = init_clean_engine().await else {
        eprintln!("skipping: ZBRAIN_TEST_PG_URL unset");
        return;
    };
    let inserted = engine
        .put_page("alpha", None, &note_input("Alpha", "body-1"))
        .await
        .expect("put_page");
    let got = engine
        .get_page("alpha", &GetPageOpts::default())
        .await
        .expect("get_page")
        .expect("Some(page)");
    assert_eq!(got.slug, "alpha");
    assert_eq!(got.title, "Alpha");
    assert_eq!(got.compiled_truth, "body-1");
    assert_eq!(got.page_type, "note");
    assert_eq!(got.id, inserted.id);
    engine.disconnect().await.expect("disconnect");
}

#[tokio::test]
#[serial_test::serial]
async fn get_page_respects_source_id_scope() {
    let Some(engine) = init_clean_engine().await else {
        eprintln!("skipping: ZBRAIN_TEST_PG_URL unset");
        return;
    };
    seed_source("pg-alt").await;

    let default_page = engine
        .put_page(
            "same-slug-get-source",
            None,
            &note_input("Default title", "default-body"),
        )
        .await
        .expect("put default source");
    let alt_page = engine
        .put_page(
            "same-slug-get-source",
            Some("pg-alt"),
            &note_input("Alt title", "alt-body"),
        )
        .await
        .expect("put alt source");

    let got = engine
        .get_page(
            "same-slug-get-source",
            &GetPageOpts {
                source_id: Some("pg-alt".to_string()),
                include_deleted: false,
            },
        )
        .await
        .expect("get_page")
        .expect("Some(page)");

    assert_eq!(got.id, alt_page.id);
    assert_ne!(got.id, default_page.id);
    assert_eq!(got.title, "Alt title");
    assert_eq!(got.compiled_truth, "alt-body");
    assert_eq!(got.source_id, "pg-alt");
    engine.disconnect().await.expect("disconnect");
}

#[tokio::test]
#[serial_test::serial]
async fn get_page_without_source_id_does_not_fall_back_to_non_default_source() {
    let Some(engine) = init_clean_engine().await else {
        eprintln!("skipping: ZBRAIN_TEST_PG_URL unset");
        return;
    };
    seed_source("pg-alt").await;

    engine
        .put_page(
            "alt-only-slug",
            Some("pg-alt"),
            &note_input("Alt title", "alt-body"),
        )
        .await
        .expect("put alt source");

    let got = engine
        .get_page("alt-only-slug", &GetPageOpts::default())
        .await
        .expect("get_page");

    assert!(
        got.is_none(),
        "GetPageOpts::default() must only search the default source, got {got:?}"
    );
    engine.disconnect().await.expect("disconnect");
}

/// Side-channel: soft-delete a page by stamping `deleted_at = now()` directly
/// via SQL. `BrainEngine::soft_delete_page` is still `unsupported` in PG until
/// a later slice, so the test sets the column itself to exercise the read path.
async fn soft_delete_via_sql(slug: &str) {
    let url = pg_url().expect("ZBRAIN_TEST_PG_URL set for soft-delete side-channel");
    let pool = sqlx::postgres::PgPoolOptions::new()
        .max_connections(1)
        .connect(&url)
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
        "soft_delete_via_sql expected to update at least one row for slug={slug}, got {rows}"
    );
    pool.close().await;
}

#[tokio::test]
#[serial_test::serial]
async fn get_page_hides_soft_deleted_row_by_default() {
    let Some(engine) = init_clean_engine().await else {
        eprintln!("skipping: ZBRAIN_TEST_PG_URL unset");
        return;
    };
    engine
        .put_page("soft-del-hidden", None, &note_input("X", "body"))
        .await
        .expect("put_page");
    soft_delete_via_sql("soft-del-hidden").await;

    let got = engine
        .get_page("soft-del-hidden", &GetPageOpts::default())
        .await
        .expect("get_page");
    assert!(
        got.is_none(),
        "default GetPageOpts must hide soft-deleted rows, got {got:?}"
    );
    engine.disconnect().await.expect("disconnect");
}

#[tokio::test]
#[serial_test::serial]
async fn get_page_returns_soft_deleted_row_when_include_deleted_true() {
    let Some(engine) = init_clean_engine().await else {
        eprintln!("skipping: ZBRAIN_TEST_PG_URL unset");
        return;
    };
    let inserted = engine
        .put_page("soft-del-visible", None, &note_input("X", "body"))
        .await
        .expect("put_page");
    soft_delete_via_sql("soft-del-visible").await;

    let opts = GetPageOpts {
        source_id: None,
        include_deleted: true,
    };
    let got = engine
        .get_page("soft-del-visible", &opts)
        .await
        .expect("get_page")
        .expect("Some(page) when include_deleted=true");
    assert_eq!(got.id, inserted.id);
    assert_eq!(got.slug, "soft-del-visible");
    // Slice #72-a: `Page.deleted_at` field fidelity — the PG SELECT must
    // include the column and `row_to_page` must decode it into the engine
    // struct. `soft_delete_via_sql` stamped `now()`, so this must be Some.
    assert!(
        got.deleted_at.is_some(),
        "include_deleted=true must surface the deleted_at timestamp, got {:?}",
        got.deleted_at
    );
    engine.disconnect().await.expect("disconnect");
}

#[tokio::test]
#[serial_test::serial]
async fn get_page_live_row_has_no_deleted_at() {
    // Slice #72-a guard: a never-deleted row must round-trip
    // `deleted_at == None`. Prevents an accidental projection that always
    // populates the field (e.g. defaulting to `now()` instead of NULL).
    let Some(engine) = init_clean_engine().await else {
        eprintln!("skipping: ZBRAIN_TEST_PG_URL unset");
        return;
    };
    engine
        .put_page("live-page", None, &note_input("Live", "body"))
        .await
        .expect("put_page");

    let got = engine
        .get_page("live-page", &GetPageOpts::default())
        .await
        .expect("get_page")
        .expect("Some(page)");
    assert!(
        got.deleted_at.is_none(),
        "live row must have deleted_at = None, got {:?}",
        got.deleted_at
    );
    engine.disconnect().await.expect("disconnect");
}

// -- put_page --------------------------------------------------------------

#[tokio::test]
#[serial_test::serial]
async fn put_page_upsert_updates_existing_row() {
    let Some(engine) = init_clean_engine().await else {
        eprintln!("skipping: ZBRAIN_TEST_PG_URL unset");
        return;
    };
    let first = engine
        .put_page("beta", None, &note_input("Beta v1", "body-v1"))
        .await
        .expect("first put");
    let second = engine
        .put_page("beta", None, &note_input("Beta v2", "body-v2"))
        .await
        .expect("second put");

    // Same slug must keep the same id (upsert, not insert-new).
    assert_eq!(first.id, second.id, "upsert must reuse the row id");
    assert_eq!(second.title, "Beta v2");
    assert_eq!(second.compiled_truth, "body-v2");

    let got = engine
        .get_page("beta", &GetPageOpts::default())
        .await
        .expect("get_page")
        .expect("Some(page)");
    assert_eq!(got.title, "Beta v2");
    assert_eq!(got.compiled_truth, "body-v2");
    engine.disconnect().await.expect("disconnect");
}

#[tokio::test]
#[serial_test::serial]
async fn put_page_respects_source_id_as_part_of_identity() {
    let Some(engine) = init_clean_engine().await else {
        eprintln!("skipping: ZBRAIN_TEST_PG_URL unset");
        return;
    };
    seed_source("pg-alt").await;

    let default_page = engine
        .put_page(
            "same-slug-different-source",
            None,
            &note_input("Default title", "default-body"),
        )
        .await
        .expect("put default source");
    let alt_page = engine
        .put_page(
            "same-slug-different-source",
            Some("pg-alt"),
            &note_input("Alt title", "alt-body"),
        )
        .await
        .expect("put alt source");

    assert_eq!(default_page.source_id, "default");
    assert_eq!(alt_page.source_id, "pg-alt");
    assert_ne!(
        default_page.id, alt_page.id,
        "same slug under distinct source_id values must be distinct rows"
    );
    assert_eq!(
        source_ids_for_slug("same-slug-different-source").await,
        vec!["default".to_string(), "pg-alt".to_string()]
    );
    engine.disconnect().await.expect("disconnect");
}

// -- delete_page -----------------------------------------------------------

#[tokio::test]
#[serial_test::serial]
async fn delete_page_removes_row() {
    let Some(engine) = init_clean_engine().await else {
        eprintln!("skipping: ZBRAIN_TEST_PG_URL unset");
        return;
    };
    engine
        .put_page("gamma", None, &note_input("Gamma", "body"))
        .await
        .expect("put_page");
    engine.delete_page("gamma").await.expect("delete_page");
    let got = engine
        .get_page("gamma", &GetPageOpts::default())
        .await
        .expect("get_page");
    assert!(got.is_none(), "deleted row must vanish");
    engine.disconnect().await.expect("disconnect");
}

#[tokio::test]
#[serial_test::serial]
async fn delete_page_is_noop_on_missing_slug() {
    let Some(engine) = init_clean_engine().await else {
        eprintln!("skipping: ZBRAIN_TEST_PG_URL unset");
        return;
    };
    // Must not error - matches TS behavior and InMemoryEngine.
    engine
        .delete_page("never-existed")
        .await
        .expect("delete_page on missing slug must be a no-op");
    engine.disconnect().await.expect("disconnect");
}

// -- list_pages ------------------------------------------------------------

#[tokio::test]
#[serial_test::serial]
async fn list_pages_empty_when_no_rows() {
    let Some(engine) = init_clean_engine().await else {
        eprintln!("skipping: ZBRAIN_TEST_PG_URL unset");
        return;
    };
    let pages = engine
        .list_pages(&PageFilters::default())
        .await
        .expect("list_pages");
    assert!(pages.is_empty(), "empty table must yield empty Vec");
    engine.disconnect().await.expect("disconnect");
}

#[tokio::test]
#[serial_test::serial]
async fn list_pages_filters_by_page_type() {
    let Some(engine) = init_clean_engine().await else {
        eprintln!("skipping: ZBRAIN_TEST_PG_URL unset");
        return;
    };
    engine
        .put_page("n1", None, &note_input("N1", "x"))
        .await
        .expect("put n1");
    engine
        .put_page("n2", None, &note_input("N2", "y"))
        .await
        .expect("put n2");
    engine
        .put_page(
            "p1",
            None,
            &PageInput {
                page_type: "person".to_string(),
                title: "P1".to_string(),
                compiled_truth: "z".to_string(),
                ..Default::default()
            },
        )
        .await
        .expect("put p1");

    let notes = engine
        .list_pages(&PageFilters {
            page_type: Some("note".to_string()),
            limit: None,
            source_id: None,
            ..Default::default()
        })
        .await
        .expect("list_pages note");
    assert_eq!(notes.len(), 2, "exactly two note rows expected");
    assert!(notes.iter().all(|p| p.page_type == "note"));

    let people = engine
        .list_pages(&PageFilters {
            page_type: Some("person".to_string()),
            limit: None,
            source_id: None,
            ..Default::default()
        })
        .await
        .expect("list_pages person");
    assert_eq!(people.len(), 1);
    assert_eq!(people[0].slug, "p1");
    engine.disconnect().await.expect("disconnect");
}

#[tokio::test]
#[serial_test::serial]
async fn list_pages_respects_limit() {
    let Some(engine) = init_clean_engine().await else {
        eprintln!("skipping: ZBRAIN_TEST_PG_URL unset");
        return;
    };
    for i in 0..5 {
        engine
            .put_page(
                &format!("slug-{i}"),
                None,
                &note_input(&format!("T{i}"), "body"),
            )
            .await
            .expect("put_page");
    }
    let pages = engine
        .list_pages(&PageFilters {
            page_type: None,
            limit: Some(3),
            source_id: None,
            ..Default::default()
        })
        .await
        .expect("list_pages");
    assert_eq!(pages.len(), 3, "limit must truncate result set");
    engine.disconnect().await.expect("disconnect");
}

#[tokio::test]
#[serial_test::serial]
async fn list_pages_filters_by_source_id() {
    let Some(engine) = init_clean_engine().await else {
        eprintln!("skipping: ZBRAIN_TEST_PG_URL unset");
        return;
    };
    seed_source("pg-alpha").await;
    seed_source("pg-beta").await;

    engine
        .put_page(
            "source-default",
            None,
            &note_input("Default", "default-body"),
        )
        .await
        .expect("put default source");
    engine
        .put_page(
            "source-alpha",
            Some("pg-alpha"),
            &note_input("Alpha", "alpha-body"),
        )
        .await
        .expect("put alpha source");
    engine
        .put_page(
            "source-beta",
            Some("pg-beta"),
            &note_input("Beta", "beta-body"),
        )
        .await
        .expect("put beta source");

    let pages = engine
        .list_pages(&PageFilters {
            source_id: Some("pg-alpha".to_string()),
            ..Default::default()
        })
        .await
        .expect("list_pages source_id=pg-alpha");

    assert_eq!(pages.len(), 1, "only pg-alpha pages should appear");
    assert_eq!(pages[0].slug, "source-alpha");
    assert_eq!(pages[0].source_id, "pg-alpha");
    engine.disconnect().await.expect("disconnect");
}

#[tokio::test]
#[serial_test::serial]
async fn list_pages_filters_by_source_ids() {
    let Some(engine) = init_clean_engine().await else {
        eprintln!("skipping: ZBRAIN_TEST_PG_URL unset");
        return;
    };
    seed_source("pg-alpha").await;
    seed_source("pg-beta").await;

    engine
        .put_page(
            "source-default",
            None,
            &note_input("Default", "default-body"),
        )
        .await
        .expect("put default source");
    engine
        .put_page(
            "source-alpha",
            Some("pg-alpha"),
            &note_input("Alpha", "alpha-body"),
        )
        .await
        .expect("put alpha source");
    engine
        .put_page(
            "source-beta",
            Some("pg-beta"),
            &note_input("Beta", "beta-body"),
        )
        .await
        .expect("put beta source");

    let pages = engine
        .list_pages(&PageFilters {
            source_ids: Some(vec!["default".to_string(), "pg-beta".to_string()]),
            sort: Some(PageSort::Slug),
            ..Default::default()
        })
        .await
        .expect("list_pages source_ids=[default,pg-beta]");

    let slugs: Vec<&str> = pages.iter().map(|p| p.slug.as_str()).collect();
    assert_eq!(
        slugs,
        vec!["source-beta", "source-default"],
        "source_ids filter must include only selected sources with requested slug ordering"
    );
    assert!(
        pages
            .iter()
            .all(|p| p.source_id == "default" || p.source_id == "pg-beta"),
        "all results must belong to selected source ids"
    );
    engine.disconnect().await.expect("disconnect");
}

#[tokio::test]
#[serial_test::serial]
async fn list_pages_filters_by_slug_prefix() {
    let Some(engine) = init_clean_engine().await else {
        eprintln!("skipping: ZBRAIN_TEST_PG_URL unset");
        return;
    };

    engine
        .put_page("docs/alpha", None, &note_input("Docs Alpha", "body"))
        .await
        .expect("put docs alpha");
    engine
        .put_page("docs/beta", None, &note_input("Docs Beta", "body"))
        .await
        .expect("put docs beta");
    engine
        .put_page("notes/gamma", None, &note_input("Notes Gamma", "body"))
        .await
        .expect("put notes gamma");

    let pages = engine
        .list_pages(&PageFilters {
            slug_prefix: Some("docs/".to_string()),
            sort: Some(PageSort::Slug),
            ..Default::default()
        })
        .await
        .expect("list_pages slug_prefix=docs/");

    let slugs: Vec<&str> = pages.iter().map(|p| p.slug.as_str()).collect();
    assert_eq!(slugs, vec!["docs/alpha", "docs/beta"]);
    engine.disconnect().await.expect("disconnect");
}

#[tokio::test]
#[serial_test::serial]
async fn list_pages_filters_by_updated_after() {
    let Some(engine) = init_clean_engine().await else {
        eprintln!("skipping: ZBRAIN_TEST_PG_URL unset");
        return;
    };

    let old = engine
        .put_page("updated-old", None, &note_input("Old", "body"))
        .await
        .expect("put old");
    let new = engine
        .put_page("updated-new", None, &note_input("New", "body"))
        .await
        .expect("put new");

    let pages = engine
        .list_pages(&PageFilters {
            updated_after: Some(old.updated_at.clone()),
            sort: Some(PageSort::Slug),
            ..Default::default()
        })
        .await
        .expect("list_pages updated_after=old.updated_at");

    let slugs: Vec<&str> = pages.iter().map(|p| p.slug.as_str()).collect();
    assert_eq!(
        slugs,
        vec!["updated-new"],
        "updated_after must be a strict greater-than filter; cutoff={:?}, new={:?}",
        old.updated_at,
        new.updated_at
    );
    engine.disconnect().await.expect("disconnect");
}

#[tokio::test]
#[serial_test::serial]
async fn list_pages_excludes_soft_deleted_by_default() {
    let Some(engine) = init_clean_engine().await else {
        eprintln!("skipping: ZBRAIN_TEST_PG_URL unset");
        return;
    };

    engine
        .put_page("visible-page", None, &note_input("Visible", "body"))
        .await
        .expect("put visible");
    engine
        .put_page("soft-deleted-page", None, &note_input("Deleted", "body"))
        .await
        .expect("put deleted");
    soft_delete_via_sql("soft-deleted-page").await;

    let pages = engine
        .list_pages(&PageFilters {
            sort: Some(PageSort::Slug),
            ..Default::default()
        })
        .await
        .expect("list_pages default");

    let slugs: Vec<&str> = pages.iter().map(|p| p.slug.as_str()).collect();
    assert_eq!(slugs, vec!["visible-page"]);
    assert!(pages.iter().all(|p| p.deleted_at.is_none()));
    engine.disconnect().await.expect("disconnect");
}

#[tokio::test]
#[serial_test::serial]
async fn list_pages_includes_soft_deleted_when_flag_set() {
    let Some(engine) = init_clean_engine().await else {
        eprintln!("skipping: ZBRAIN_TEST_PG_URL unset");
        return;
    };

    engine
        .put_page("visible-page", None, &note_input("Visible", "body"))
        .await
        .expect("put visible");
    engine
        .put_page("soft-deleted-page", None, &note_input("Deleted", "body"))
        .await
        .expect("put deleted");
    soft_delete_via_sql("soft-deleted-page").await;

    let pages = engine
        .list_pages(&PageFilters {
            include_deleted: true,
            sort: Some(PageSort::Slug),
            ..Default::default()
        })
        .await
        .expect("list_pages include_deleted=true");

    let slugs: Vec<&str> = pages.iter().map(|p| p.slug.as_str()).collect();
    assert_eq!(slugs, vec!["soft-deleted-page", "visible-page"]);
    let deleted = pages
        .iter()
        .find(|p| p.slug == "soft-deleted-page")
        .expect("soft-deleted page should be present");
    assert!(deleted.deleted_at.is_some());
    engine.disconnect().await.expect("disconnect");
}

#[tokio::test]
#[serial_test::serial]
async fn list_pages_respects_offset() {
    let Some(engine) = init_clean_engine().await else {
        eprintln!("skipping: ZBRAIN_TEST_PG_URL unset");
        return;
    };

    for slug in ["offset-a", "offset-b", "offset-c", "offset-d"] {
        engine
            .put_page(slug, None, &note_input(slug, "body"))
            .await
            .expect("put offset page");
    }

    let pages = engine
        .list_pages(&PageFilters {
            offset: Some(2),
            sort: Some(PageSort::Slug),
            ..Default::default()
        })
        .await
        .expect("list_pages offset=2");

    let slugs: Vec<&str> = pages.iter().map(|p| p.slug.as_str()).collect();
    assert_eq!(slugs, vec!["offset-c", "offset-d"]);
    engine.disconnect().await.expect("disconnect");
}

#[tokio::test]
#[serial_test::serial]
async fn list_pages_sorts_by_slug_asc() {
    let Some(engine) = init_clean_engine().await else {
        eprintln!("skipping: ZBRAIN_TEST_PG_URL unset");
        return;
    };

    for slug in ["slug-c", "slug-a", "slug-b"] {
        engine
            .put_page(slug, None, &note_input(slug, "body"))
            .await
            .expect("put slug sort page");
    }

    let pages = engine
        .list_pages(&PageFilters {
            sort: Some(PageSort::Slug),
            ..Default::default()
        })
        .await
        .expect("list_pages sort=Slug");

    let slugs: Vec<&str> = pages.iter().map(|p| p.slug.as_str()).collect();
    assert_eq!(slugs, vec!["slug-a", "slug-b", "slug-c"]);
    engine.disconnect().await.expect("disconnect");
}

#[tokio::test]
#[serial_test::serial]
async fn list_pages_sorts_by_updated_desc_by_default() {
    let Some(engine) = init_clean_engine().await else {
        eprintln!("skipping: ZBRAIN_TEST_PG_URL unset");
        return;
    };

    let first = engine
        .put_page("updated-default-first", None, &note_input("First", "body"))
        .await
        .expect("put first");
    let second = engine
        .put_page(
            "updated-default-second",
            None,
            &note_input("Second", "body"),
        )
        .await
        .expect("put second");

    let pages = engine
        .list_pages(&PageFilters::default())
        .await
        .expect("list_pages default sort");

    let slugs: Vec<&str> = pages.iter().map(|p| p.slug.as_str()).collect();
    assert_eq!(
        slugs,
        vec!["updated-default-second", "updated-default-first"],
        "default list_pages sort should be updated_at DESC with slug tie-breaker; first={:?}, second={:?}",
        first.updated_at,
        second.updated_at
    );
    engine.disconnect().await.expect("disconnect");
}

// -- tag CRUD / list_pages(tag) --------------------------------------------

#[tokio::test]
#[serial_test::serial]
async fn add_tag_round_trips_via_get_tags() {
    let Some(engine) = init_clean_engine().await else {
        eprintln!("skipping: ZBRAIN_TEST_PG_URL unset");
        return;
    };

    engine
        .put_page("tag-alpha", None, &note_input("Tag Alpha", "body"))
        .await
        .expect("put tag-alpha");
    engine
        .add_tag("tag-alpha", "rust", None)
        .await
        .expect("add_tag must succeed on an existing live page");

    let tags = engine.get_tags("tag-alpha", None).await.expect("get_tags");
    assert_eq!(tags, vec!["rust"]);
    engine.disconnect().await.expect("disconnect");
}

#[tokio::test]
#[serial_test::serial]
async fn add_tag_is_idempotent_for_duplicate_tag() {
    let Some(engine) = init_clean_engine().await else {
        eprintln!("skipping: ZBRAIN_TEST_PG_URL unset");
        return;
    };

    engine
        .put_page("tag-beta", None, &note_input("Tag Beta", "body"))
        .await
        .expect("put tag-beta");
    engine
        .add_tag("tag-beta", "ai", None)
        .await
        .expect("first add_tag");
    engine
        .add_tag("tag-beta", "ai", None)
        .await
        .expect("second add_tag must be idempotent");

    let tags = engine.get_tags("tag-beta", None).await.expect("get_tags");
    assert_eq!(tags, vec!["ai"], "duplicate tag must not create rows");
    engine.disconnect().await.expect("disconnect");
}

#[tokio::test]
#[serial_test::serial]
async fn add_tag_missing_page_returns_page_not_found() {
    let Some(engine) = init_clean_engine().await else {
        eprintln!("skipping: ZBRAIN_TEST_PG_URL unset");
        return;
    };

    let err = engine
        .add_tag("tag-ghost", "rust", None)
        .await
        .expect_err("add_tag on a missing page must fail");
    assert_eq!(err.class, "PageNotFound");
    assert_eq!(err.code, "page_not_found");
    assert!(err.message.contains("tag-ghost"), "msg={}", err.message);
    assert!(
        err.message.contains("(source=default)"),
        "None source_id must be normalised to default; msg={}",
        err.message
    );
    engine.disconnect().await.expect("disconnect");
}

#[tokio::test]
#[serial_test::serial]
async fn remove_tag_deletes_existing_tag() {
    let Some(engine) = init_clean_engine().await else {
        eprintln!("skipping: ZBRAIN_TEST_PG_URL unset");
        return;
    };

    engine
        .put_page("tag-gamma", None, &note_input("Tag Gamma", "body"))
        .await
        .expect("put tag-gamma");
    engine
        .add_tag("tag-gamma", "rust", None)
        .await
        .expect("add_tag");
    engine
        .remove_tag("tag-gamma", "rust", None)
        .await
        .expect("remove_tag");

    let tags = engine.get_tags("tag-gamma", None).await.expect("get_tags");
    assert!(tags.is_empty(), "tag must be gone after remove");
    engine.disconnect().await.expect("disconnect");
}

#[tokio::test]
#[serial_test::serial]
async fn get_tags_returns_sorted_tags() {
    let Some(engine) = init_clean_engine().await else {
        eprintln!("skipping: ZBRAIN_TEST_PG_URL unset");
        return;
    };

    engine
        .put_page("tag-delta", None, &note_input("Tag Delta", "body"))
        .await
        .expect("put tag-delta");
    for tag in ["zinc", "alpha", "mid"] {
        engine
            .add_tag("tag-delta", tag, None)
            .await
            .expect("add_tag");
    }

    let tags = engine.get_tags("tag-delta", None).await.expect("get_tags");
    assert_eq!(tags, vec!["alpha", "mid", "zinc"]);
    engine.disconnect().await.expect("disconnect");
}

#[tokio::test]
#[serial_test::serial]
async fn list_pages_filters_by_tag() {
    let Some(engine) = init_clean_engine().await else {
        eprintln!("skipping: ZBRAIN_TEST_PG_URL unset");
        return;
    };

    engine
        .put_page("tag-list-rust", None, &note_input("Rust", "body"))
        .await
        .expect("put rust page");
    engine
        .put_page("tag-list-ai", None, &note_input("AI", "body"))
        .await
        .expect("put ai page");
    engine
        .put_page("tag-list-both", None, &note_input("Both", "body"))
        .await
        .expect("put both page");

    engine
        .add_tag("tag-list-rust", "rust", None)
        .await
        .expect("tag rust page");
    engine
        .add_tag("tag-list-ai", "ai", None)
        .await
        .expect("tag ai page");
    engine
        .add_tag("tag-list-both", "rust", None)
        .await
        .expect("tag both page with rust");
    engine
        .add_tag("tag-list-both", "ai", None)
        .await
        .expect("tag both page with ai");

    let pages = engine
        .list_pages(&PageFilters {
            tag: Some("rust".to_string()),
            sort: Some(PageSort::Slug),
            ..Default::default()
        })
        .await
        .expect("list_pages tag=rust");

    let slugs: Vec<&str> = pages.iter().map(|p| p.slug.as_str()).collect();
    assert_eq!(slugs, vec!["tag-list-both", "tag-list-rust"]);
    engine.disconnect().await.expect("disconnect");
}

// -- resolve_slugs ---------------------------------------------------------

#[tokio::test]
#[serial_test::serial]
async fn resolve_slugs_exact_match_only_in_slice_4b() {
    let Some(engine) = init_clean_engine().await else {
        eprintln!("skipping: ZBRAIN_TEST_PG_URL unset");
        return;
    };
    engine
        .put_page("alpha-beta", None, &note_input("AB", "x"))
        .await
        .expect("put_page");
    engine
        .put_page("alpha-gamma", None, &note_input("AG", "x"))
        .await
        .expect("put_page");

    // Exact match returns the one slug.
    let exact = engine
        .resolve_slugs("alpha-beta")
        .await
        .expect("resolve_slugs exact");
    assert_eq!(exact, vec!["alpha-beta".to_string()]);

    // Substring "alpha" must NOT match - fuzzy is deferred to slice 6.5c.
    let partial = engine
        .resolve_slugs("alpha")
        .await
        .expect("resolve_slugs partial");
    assert!(
        partial.is_empty(),
        "slice 4b resolve_slugs is exact-only, got {partial:?}"
    );
    engine.disconnect().await.expect("disconnect");
}
