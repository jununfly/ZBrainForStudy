//! Slice 5 - `LibsqlEngine` Page CRUD integration tests.
//!
//! Mirror of `postgres_engine_page_crud.rs` against the libsql backend.
//! Each test allocates its own `NamedTempFile`, so there is no cross-test
//! contamination and no need for a `TRUNCATE` reset between cases — the
//! file is born empty.
//!
//! Test groups (same shape as the PG side):
//! - `get_page`: not-found / found / soft-delete filter / `include_deleted`
//!   semantics / `source_id` scoping / 30-column projection defaults
//! - `put_page`: insert / upsert (same slug -> updated row, id reused)
//! - `delete_page`: row vanishes, no-op on missing
//! - `list_pages`: empty / `page_type` filter / limit truncation
//! - `resolve_slugs`: exact match only (fuzzy deferred to slice 6.5c)
//!
//! Slice 6a S6-T4: `get_page` upgraded from a 7-column stub to the full
//! 30-column projection backed by `full_row_to_page`, with `deleted_at`
//! filtering and `source_id` scoping mirroring `soft_delete_page` /
//! `find_duplicate_page`. The old "`include_deleted` returns Unsupported"
//! contract is dropped in favour of a real soft-deleted round trip.

use tempfile::NamedTempFile;
use zbrain_core::engine::{
    BrainEngine, EngineConfig, GetPageOpts, PageFilters, PageInput,
};
use zbrain_core::libsql::LibsqlEngine;
use zbrain_core::PageKind;

/// Build a connected, schema-initialized engine on a fresh temp file.
/// Returns `(engine, NamedTempFile)` so the caller can keep the temp file
/// alive for the duration of the test — dropping it deletes the DB.
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
    let (engine, _tmp) = init_clean_engine().await;
    let got = engine
        .get_page("does-not-exist", &GetPageOpts::default())
        .await
        .expect("get_page");
    assert!(got.is_none(), "missing slug must return None, got {got:?}");
    engine.disconnect().await.expect("disconnect");
}

#[tokio::test]
async fn get_page_round_trips_after_put() {
    let (engine, _tmp) = init_clean_engine().await;
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
async fn get_page_default_excludes_soft_deleted() {
    // Default GetPageOpts has include_deleted=false. A row that has been
    // soft-deleted must vanish from the default read path, mirroring the
    // trait doc: "Returns None if not found or soft-deleted (unless
    // opts.include_deleted is true)".
    let (engine, _tmp) = init_clean_engine().await;
    engine
        .put_page("doomed", &note_input("Doomed", "body"))
        .await
        .expect("put_page");
    let hit = engine
        .soft_delete_page("doomed", None)
        .await
        .expect("soft_delete_page");
    assert_eq!(hit.as_deref(), Some("doomed"), "soft_delete must hit");

    let got = engine
        .get_page("doomed", &GetPageOpts::default())
        .await
        .expect("get_page");
    assert!(
        got.is_none(),
        "default get_page must skip soft-deleted rows, got {got:?}"
    );
    engine.disconnect().await.expect("disconnect");
}

#[tokio::test]
async fn get_page_with_include_deleted_returns_soft_deleted_row() {
    // With include_deleted=true the row must be visible AND carry a
    // non-empty deleted_at marker, proving the column is actually projected
    // (not just defaulted by the stub `row_to_page`).
    let (engine, _tmp) = init_clean_engine().await;
    engine
        .put_page("zombie", &note_input("Zombie", "body"))
        .await
        .expect("put_page");
    engine
        .soft_delete_page("zombie", None)
        .await
        .expect("soft_delete_page");

    let opts = GetPageOpts {
        source_id: None,
        include_deleted: true,
    };
    let got = engine
        .get_page("zombie", &opts)
        .await
        .expect("get_page")
        .expect("Some(page) with include_deleted=true");
    assert_eq!(got.slug, "zombie");
    assert!(
        got.deleted_at.is_some(),
        "soft-deleted row must surface deleted_at, got {:?}",
        got.deleted_at
    );
    engine.disconnect().await.expect("disconnect");
}

#[tokio::test]
async fn get_page_with_matching_source_id_returns_row() {
    // source_id filter must match the stored value. `put_page` uses the
    // schema default ('default') when PageInput leaves source_id None, so
    // requesting that exact source must succeed.
    let (engine, _tmp) = init_clean_engine().await;
    engine
        .put_page("scoped", &note_input("Scoped", "body"))
        .await
        .expect("put_page");
    let opts = GetPageOpts {
        source_id: Some("default".to_string()),
        include_deleted: false,
    };
    let got = engine
        .get_page("scoped", &opts)
        .await
        .expect("get_page")
        .expect("Some(page) with matching source_id");
    assert_eq!(got.slug, "scoped");
    assert_eq!(got.source_id, "default");
    engine.disconnect().await.expect("disconnect");
}

#[tokio::test]
async fn get_page_with_mismatched_source_id_returns_none() {
    // source_id scoping must filter out rows that belong to a different
    // source, even if the slug matches.
    let (engine, _tmp) = init_clean_engine().await;
    engine
        .put_page("scoped", &note_input("Scoped", "body"))
        .await
        .expect("put_page");
    let opts = GetPageOpts {
        source_id: Some("other-source".to_string()),
        include_deleted: false,
    };
    let got = engine
        .get_page("scoped", &opts)
        .await
        .expect("get_page");
    assert!(
        got.is_none(),
        "wrong source_id must yield None, got {got:?}"
    );
    engine.disconnect().await.expect("disconnect");
}

#[tokio::test]
async fn get_page_returns_full_column_projection() {
    // Lock the 30-column projection: after S6-T4, get_page must hydrate
    // every documented Page field from the row instead of synthesising
    // defaults in `row_to_page`. We assert the schema-default values
    // returned by an unannotated put_page so that any regression to the
    // 7-column stub fails immediately.
    let (engine, _tmp) = init_clean_engine().await;
    engine
        .put_page("full-cols", &note_input("Full Cols", "body"))
        .await
        .expect("put_page");
    let got = engine
        .get_page("full-cols", &GetPageOpts::default())
        .await
        .expect("get_page")
        .expect("Some(page)");

    // Identity / content
    assert_eq!(got.slug, "full-cols");
    assert_eq!(got.title, "Full Cols");
    assert_eq!(got.compiled_truth, "body");
    assert_eq!(got.page_type, "note");
    // PageInput leaves page_kind unset, so put_page falls back to the
    // schema/engine default of Markdown (see engine.rs:476).
    assert_eq!(got.page_kind, PageKind::Markdown);

    // 0002 / 0003 columns that the stub used to fabricate.
    assert_eq!(
        got.source_id, "default",
        "source_id must come from the row, not a literal default"
    );
    assert_eq!(
        got.generation, 1,
        "generation must hydrate from the row (DB default 1)"
    );
    assert_eq!(
        got.chunker_version, 1,
        "chunker_version must hydrate from the row (DB default 1)"
    );
    assert!(
        got.deleted_at.is_none(),
        "live row must report deleted_at=None"
    );
    assert!(
        got.frontmatter.is_object(),
        "frontmatter must decode to an object, got {:?}",
        got.frontmatter
    );

    // Timestamps must come from the DB, not be empty.
    assert!(!got.created_at.is_empty(), "created_at must be populated");
    assert!(!got.updated_at.is_empty(), "updated_at must be populated");
    engine.disconnect().await.expect("disconnect");
}

// -- put_page --------------------------------------------------------------

#[tokio::test]
async fn put_page_upsert_updates_existing_row() {
    let (engine, _tmp) = init_clean_engine().await;
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
    let (engine, _tmp) = init_clean_engine().await;
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
    let (engine, _tmp) = init_clean_engine().await;
    engine
        .delete_page("never-existed")
        .await
        .expect("delete_page on missing slug must be a no-op");
    engine.disconnect().await.expect("disconnect");
}

// -- list_pages ------------------------------------------------------------

#[tokio::test]
async fn list_pages_empty_when_no_rows() {
    let (engine, _tmp) = init_clean_engine().await;
    let pages = engine
        .list_pages(&PageFilters::default())
        .await
        .expect("list_pages");
    assert!(pages.is_empty(), "empty table must yield empty Vec");
    engine.disconnect().await.expect("disconnect");
}

#[tokio::test]
async fn list_pages_filters_by_page_type() {
    let (engine, _tmp) = init_clean_engine().await;
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
    let (engine, _tmp) = init_clean_engine().await;
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
async fn resolve_slugs_exact_match_only_in_slice_5() {
    let (engine, _tmp) = init_clean_engine().await;
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
        "slice 5 resolve_slugs is exact-only, got {partial:?}"
    );
    engine.disconnect().await.expect("disconnect");
}
