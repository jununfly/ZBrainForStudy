//! S6-T8 — `LibsqlEngine::put_page` `source_id` parameterisation tests.
//!
//! TS parity reference: `zbrain/src/core/pglite-engine.ts:834-887`.
//!   * line 838:  `const sourceId = opts?.sourceId ?? 'default';`
//!   * line 868:  `ON CONFLICT (source_id, slug) DO UPDATE SET ...`
//!
//! Before S6-T8, the `?1` bind slot was hardcoded to the literal `"default"`
//! at `libsql.rs:283` (`let source_id = "default";`). This file pins the
//! upgraded behaviour: callers may pass an explicit `source_id`, and `None`
//! still normalises to `"default"` for backwards-compat with every call site
//! migrated by the S6-T8 sed sweep (113 occurrences -> `.put_page(slug, None, &input)`).
//!
//! Test matrix (4 cases, libsql side):
//!   * T1 — `Some("foo")` writes `source_id = "foo"` to the row.
//!   * T2 — `None` normalises to `"default"` (parity with old hardcoded literal).
//!   * T3 — Same slug under two distinct `source_ids` -> two independent rows
//!     (proves `ON CONFLICT(source_id, slug)` compound key, not slug-only).
//!   * T4 — Same slug + same `source_id` -> row updated in place (id reused).

use tempfile::NamedTempFile;
use zbrain_core::engine::{BrainEngine, EngineConfig, PageFilters, PageInput};
use zbrain_core::libsql::LibsqlEngine;

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

/// Seed an additional `sources` row via a raw connection on the same temp
/// file. Required because `pages.source_id` carries a FK to `sources(id)`
/// and only the `"default"` seed exists after `init_schema`. Mirrors the
/// pattern used in `libsql_engine_list_pages.rs`. Idempotent via `OR IGNORE`.
async fn seed_source(tmp: &NamedTempFile, id: &str) {
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

// -- T1 — Some("foo") writes source_id = "foo" ----------------------------

#[tokio::test]
async fn put_page_with_some_source_id_writes_that_value() {
    let (engine, tmp) = init_clean_engine().await;
    seed_source(&tmp, "notion").await;
    let inserted = engine
        .put_page("alpha", Some("notion"), &note_input("Alpha", "body-1"))
        .await
        .expect("put_page");
    assert_eq!(
        inserted.source_id, "notion",
        "Some(\"notion\") must land verbatim in the row's source_id column"
    );
    engine.disconnect().await.expect("disconnect");
}

// -- T2 — None normalises to "default" ------------------------------------

#[tokio::test]
async fn put_page_with_none_source_id_normalises_to_default() {
    let (engine, _tmp) = init_clean_engine().await;
    let inserted = engine
        .put_page("beta", None, &note_input("Beta", "body-2"))
        .await
        .expect("put_page");
    assert_eq!(
        inserted.source_id, "default",
        "None must normalise to \"default\" — mirrors TS opts?.sourceId ?? 'default'"
    );
    engine.disconnect().await.expect("disconnect");
}

// -- T3 — same slug + different source_ids -> two rows --------------------

#[tokio::test]
async fn put_page_same_slug_different_source_ids_produces_two_rows() {
    let (engine, tmp) = init_clean_engine().await;
    seed_source(&tmp, "src-a").await;
    seed_source(&tmp, "src-b").await;

    let a = engine
        .put_page("shared", Some("src-a"), &note_input("Shared A", "body-a"))
        .await
        .expect("put_page src-a");
    let b = engine
        .put_page("shared", Some("src-b"), &note_input("Shared B", "body-b"))
        .await
        .expect("put_page src-b");

    assert_ne!(
        a.id, b.id,
        "compound key (source_id, slug) must NOT merge rows under different sources"
    );
    assert_eq!(a.source_id, "src-a");
    assert_eq!(b.source_id, "src-b");
    assert_eq!(a.slug, "shared");
    assert_eq!(b.slug, "shared");

    // Sanity-check: list_pages sees both rows.
    let all = engine
        .list_pages(&PageFilters::default())
        .await
        .expect("list_pages");
    let shared_rows: Vec<_> = all.iter().filter(|p| p.slug == "shared").collect();
    assert_eq!(
        shared_rows.len(),
        2,
        "expected two `shared` rows (one per source), got {shared_rows:?}"
    );

    engine.disconnect().await.expect("disconnect");
}

// -- T4 — same slug + same source_id -> in-place update -------------------

#[tokio::test]
async fn put_page_same_slug_same_source_id_updates_in_place() {
    let (engine, tmp) = init_clean_engine().await;
    seed_source(&tmp, "notion").await;

    let v1 = engine
        .put_page("gamma", Some("notion"), &note_input("Gamma v1", "body-v1"))
        .await
        .expect("put_page v1");
    let v2 = engine
        .put_page("gamma", Some("notion"), &note_input("Gamma v2", "body-v2"))
        .await
        .expect("put_page v2");

    assert_eq!(
        v1.id, v2.id,
        "ON CONFLICT(source_id, slug) DO UPDATE must reuse the existing row id"
    );
    assert_eq!(v2.title, "Gamma v2", "title must reflect the second write");
    assert_eq!(
        v2.compiled_truth, "body-v2",
        "compiled_truth must reflect the second write"
    );

    let all = engine
        .list_pages(&PageFilters::default())
        .await
        .expect("list_pages");
    let gamma_rows: Vec<_> = all.iter().filter(|p| p.slug == "gamma").collect();
    assert_eq!(
        gamma_rows.len(),
        1,
        "same (source_id, slug) must collapse to ONE row, got {gamma_rows:?}"
    );

    engine.disconnect().await.expect("disconnect");
}
