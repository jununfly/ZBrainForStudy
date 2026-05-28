//! Slice 4b - `PostgresEngine` Page CRUD integration tests.
//!
//! Gated on `ZBRAIN_TEST_PG_URL` (same pattern as the lifecycle suite).
//! Each test connects to a clean schema via `init_clean_engine()`, truncates
//! the `pages` table from prior test runs, then exercises one CRUD method.
//!
//! Test groups:
//! - `get_page`: not-found / found / `include_deleted` unsupported error
//! - `put_page`: insert / upsert (same slug -> updated row, id may change)
//! - `delete_page`: row vanishes, no-op on missing
//! - `list_pages`: empty / `page_type` filter / limit truncation
//! - `resolve_slugs`: exact match only (fuzzy deferred to slice 6.5c)

use zbrain_core::engine::{
    BrainEngine, EngineConfig, GetPageOpts, PageFilters, PageInput,
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

// -- get_page --------------------------------------------------------------

#[tokio::test]
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
async fn get_page_round_trips_after_put() {
    let Some(engine) = init_clean_engine().await else {
        eprintln!("skipping: ZBRAIN_TEST_PG_URL unset");
        return;
    };
    let inserted = engine
        .put_page("alpha", &note_input("Alpha", "body-1"))
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
async fn get_page_with_include_deleted_returns_unsupported() {
    let Some(engine) = init_clean_engine().await else {
        eprintln!("skipping: ZBRAIN_TEST_PG_URL unset");
        return;
    };
    let opts = GetPageOpts {
        source_id: None,
        include_deleted: true,
    };
    let err = engine
        .get_page("any-slug", &opts)
        .await
        .expect_err("include_deleted must error in slice 4b");
    assert_eq!(err.class, "Unsupported", "got class={}", err.class);
    assert_eq!(err.code, "unsupported", "got code={}", err.code);
    engine.disconnect().await.expect("disconnect");
}

// -- put_page --------------------------------------------------------------

#[tokio::test]
async fn put_page_upsert_updates_existing_row() {
    let Some(engine) = init_clean_engine().await else {
        eprintln!("skipping: ZBRAIN_TEST_PG_URL unset");
        return;
    };
    let first = engine
        .put_page("beta", &note_input("Beta v1", "body-v1"))
        .await
        .expect("first put");
    let second = engine
        .put_page("beta", &note_input("Beta v2", "body-v2"))
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

// -- delete_page -----------------------------------------------------------

#[tokio::test]
async fn delete_page_removes_row() {
    let Some(engine) = init_clean_engine().await else {
        eprintln!("skipping: ZBRAIN_TEST_PG_URL unset");
        return;
    };
    engine
        .put_page("gamma", &note_input("Gamma", "body"))
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
async fn list_pages_filters_by_page_type() {
    let Some(engine) = init_clean_engine().await else {
        eprintln!("skipping: ZBRAIN_TEST_PG_URL unset");
        return;
    };
    engine
        .put_page("n1", &note_input("N1", "x"))
        .await
        .expect("put n1");
    engine
        .put_page("n2", &note_input("N2", "y"))
        .await
        .expect("put n2");
    engine
        .put_page(
            "p1",
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
async fn list_pages_respects_limit() {
    let Some(engine) = init_clean_engine().await else {
        eprintln!("skipping: ZBRAIN_TEST_PG_URL unset");
        return;
    };
    for i in 0..5 {
        engine
            .put_page(
                &format!("slug-{i}"),
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

// -- resolve_slugs ---------------------------------------------------------

#[tokio::test]
async fn resolve_slugs_exact_match_only_in_slice_4b() {
    let Some(engine) = init_clean_engine().await else {
        eprintln!("skipping: ZBRAIN_TEST_PG_URL unset");
        return;
    };
    engine
        .put_page("alpha-beta", &note_input("AB", "x"))
        .await
        .expect("put_page");
    engine
        .put_page("alpha-gamma", &note_input("AG", "x"))
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
