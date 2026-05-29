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

use serde_json::json;
use tempfile::NamedTempFile;
use zbrain_core::engine::{
    BrainEngine, EngineConfig, GetPageOpts, PageFilters, PageInput,
};
use zbrain_core::libsql::LibsqlEngine;
use zbrain_core::{EffectiveDateSource, PageKind};

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

// -- put_page (S5 stub) ---------------------------------------------------

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

// -- put_page (S6-T6 19-col upsert) ----------------------------------------
//
// 12 test cases covering INSERT / UPDATE / COALESCE-preserve / excluded
// overwrite / ingested_at server-stamp / updated_at monotonic /
// chunker_version default / frontmatter JSON roundtrip / generation trigger /
// RETURNING 30-col projection / source_id default 'default'.

/// Build a `PageInput` with every optional field populated, giving a
/// deterministic fixture that exercises the full 19-column INSERT path.
fn full_input() -> PageInput {
    PageInput {
        page_type: "note".to_string(),
        title: "Full Input".to_string(),
        compiled_truth: "body".to_string(),
        timeline: Some("T1 → T2".to_string()),
        frontmatter: Some(json!({"key": "value"})),
        content_hash: Some("sha256:abcdef".to_string()),
        page_kind: Some(PageKind::Markdown),
        effective_date: Some("2026-01-15".to_string()),
        effective_date_source: Some(EffectiveDateSource::Filename),
        import_filename: Some("test.md".to_string()),
        chunker_version: Some(2),
        source_path: Some("/path/to/test.md".to_string()),
        source_kind: Some("file".to_string()),
        source_uri: Some("file:///path/to/test.md".to_string()),
        ingested_via: Some("cli".to_string()),
        ingested_at: None, // server-stamp; None so we verify engine sets it
        last_retrieved_at: None, // S5 — put_page does not write
        embedding: None,         // S5 — put_page does not write
    }
}

#[tokio::test]
async fn s6t6_insert_new_page_writes_19_columns() {
    // INSERT path: all 19 columns must land in the row and be readable
    // via get_page (30-col projection). Source_id defaults to 'default'.
    let (engine, _tmp) = init_clean_engine().await;
    let result = engine
        .put_page("full-insert", &full_input())
        .await
        .expect("put_page full insert");

    // Identity + content
    assert_eq!(result.slug, "full-insert");
    assert_eq!(result.page_type, "note");
    assert_eq!(result.page_kind, PageKind::Markdown);
    assert_eq!(result.title, "Full Input");
    assert_eq!(result.compiled_truth, "body");
    assert_eq!(result.timeline, "T1 → T2");
    assert_eq!(result.content_hash.as_deref(), Some("sha256:abcdef"));
    assert_eq!(result.source_id, "default");

    // Effective-date chain
    assert_eq!(result.effective_date.as_deref(), Some("2026-01-15"));
    assert_eq!(
        result.effective_date_source,
        Some(EffectiveDateSource::Filename)
    );
    assert_eq!(result.import_filename.as_deref(), Some("test.md"));

    // Provenance
    assert_eq!(result.source_path.as_deref(), Some("/path/to/test.md"));
    assert_eq!(result.source_kind.as_deref(), Some("file"));
    assert_eq!(
        result.source_uri.as_deref(),
        Some("file:///path/to/test.md")
    );
    assert_eq!(result.ingested_via.as_deref(), Some("cli"));
    assert!(result.ingested_at.is_some(), "ingested_at must be server-stamped when source_kind is set");

    // Chunker
    assert_eq!(result.chunker_version, 2);

    // Timestamps
    assert!(!result.created_at.is_empty());
    assert!(!result.updated_at.is_empty());

    // Cross-check via get_page
    let got = engine
        .get_page("full-insert", &GetPageOpts::default())
        .await
        .expect("get_page")
        .expect("Some(page)");
    assert_eq!(got.title, result.title);
    assert_eq!(got.source_kind, result.source_kind);
    engine.disconnect().await.expect("disconnect");
}

#[tokio::test]
async fn s6t6_update_same_slug_uses_update_branch() {
    // Second put_page with the same slug must UPDATE (reuse id), not INSERT.
    let (engine, _tmp) = init_clean_engine().await;
    let first = engine
        .put_page("upsert-target", &full_input())
        .await
        .expect("first put");
    let mut second_input = full_input();
    second_input.title = "Updated Title".to_string();
    let second = engine
        .put_page("upsert-target", &second_input)
        .await
        .expect("second put (update)");

    assert_eq!(first.id, second.id, "upsert must reuse row id");
    assert_eq!(second.title, "Updated Title");
    engine.disconnect().await.expect("disconnect");
}

#[tokio::test]
async fn s6t6_coalesce_preserve_effective_date() {
    // On UPDATE, null input for effective_date must preserve the old value
    // (COALESCE-preserve). Conversely, a non-null input must overwrite.
    let (engine, _tmp) = init_clean_engine().await;
    let mut input = full_input();
    input.effective_date = Some("2026-03-01".to_string());
    engine
        .put_page("coalesce-eff", &input)
        .await
        .expect("first put");

    // UPDATE with null effective_date → old value preserved
    let mut update = full_input();
    update.effective_date = None;
    let updated = engine
        .put_page("coalesce-eff", &update)
        .await
        .expect("second put (coalesce)");
    assert_eq!(
        updated.effective_date.as_deref(),
        Some("2026-03-01"),
        "null input must COALESCE-preserve old effective_date"
    );

    // UPDATE with new value → overwrites
    let mut overwrite = full_input();
    overwrite.effective_date = Some("2026-06-15".to_string());
    let overwritten = engine
        .put_page("coalesce-eff", &overwrite)
        .await
        .expect("third put (overwrite)");
    assert_eq!(
        overwritten.effective_date.as_deref(),
        Some("2026-06-15"),
        "non-null input must overwrite effective_date"
    );
    engine.disconnect().await.expect("disconnect");
}

#[tokio::test]
async fn s6t6_coalesce_preserve_import_filename() {
    // Null import_filename on UPDATE must not blank a previously set value.
    let (engine, _tmp) = init_clean_engine().await;
    let mut input = full_input();
    input.import_filename = Some("original.md".to_string());
    engine
        .put_page("coalesce-impf", &input)
        .await
        .expect("first put");

    let mut update = full_input();
    update.import_filename = None;
    let updated = engine
        .put_page("coalesce-impf", &update)
        .await
        .expect("second put");
    assert_eq!(
        updated.import_filename.as_deref(),
        Some("original.md"),
        "null import_filename must COALESCE-preserve old value"
    );
    engine.disconnect().await.expect("disconnect");
}

#[tokio::test]
async fn s6t6_coalesce_preserve_ingested_at() {
    // Null provenance on UPDATE must preserve the old ingested_at.
    let (engine, _tmp) = init_clean_engine().await;
    let mut input = full_input();
    input.source_kind = Some("file".to_string());
    engine
        .put_page("coalesce-ingat", &input)
        .await
        .expect("first put with provenance → ingested_at stamped");
    let first_ingested = engine
        .get_page("coalesce-ingat", &GetPageOpts::default())
        .await
        .expect("get")
        .expect("page")
        .ingested_at
        .clone();
    assert!(first_ingested.is_some(), "first put must stamp ingested_at");

    // UPDATE without provenance → ingested_at preserved (COALESCE)
    let mut update = full_input();
    update.source_kind = None;
    update.source_uri = None;
    update.ingested_via = None;
    let updated = engine
        .put_page("coalesce-ingat", &update)
        .await
        .expect("second put");
    assert_eq!(
        updated.ingested_at, first_ingested,
        "COALESCE must preserve old ingested_at when no provenance fields"
    );
    engine.disconnect().await.expect("disconnect");
}

#[tokio::test]
async fn s6t6_excluded_overwrites_title_and_compiled_truth() {
    // title and compiled_truth are on the excluded-override list — they
    // must always reflect the latest input, even if it differs from the
    // stored value.
    let (engine, _tmp) = init_clean_engine().await;
    let mut input = full_input();
    input.title = "Original Title".to_string();
    input.compiled_truth = "original body".to_string();
    engine
        .put_page("excluded-ovr", &input)
        .await
        .expect("first put");

    let mut update = full_input();
    update.title = "New Title".to_string();
    update.compiled_truth = "new body".to_string();
    let updated = engine
        .put_page("excluded-ovr", &update)
        .await
        .expect("second put");
    assert_eq!(updated.title, "New Title");
    assert_eq!(updated.compiled_truth, "new body");
    engine.disconnect().await.expect("disconnect");
}

#[tokio::test]
async fn s6t6_updated_at_monotonically_increases() {
    // updated_at on the second put must be >= the first. SQLite
    // CURRENT_TIMESTAMP has second granularity, so we only assert not-less.
    let (engine, _tmp) = init_clean_engine().await;
    let first = engine
        .put_page("mono-ts", &full_input())
        .await
        .expect("first put");
    let second = engine
        .put_page("mono-ts", &full_input())
        .await
        .expect("second put");
    assert!(
        second.updated_at >= first.updated_at,
        "updated_at must not go backwards: first={} second={}",
        first.updated_at,
        second.updated_at,
    );
    engine.disconnect().await.expect("disconnect");
}

#[tokio::test]
async fn s6t6_ingested_at_server_stamp_with_provenance() {
    // When any of source_kind / source_uri / ingested_via is non-null, the
    // engine must compute ingested_at server-side (ignoring PageInput.ingested_at).
    let (engine, _tmp) = init_clean_engine().await;
    let mut input = full_input();
    input.source_kind = Some("file".to_string());
    input.ingested_at = Some("1999-01-01T00:00:00Z".to_string()); // must be ignored
    let result = engine
        .put_page("stamp-prov", &input)
        .await
        .expect("put_page");
    assert!(
        result.ingested_at.is_some(),
        "provenance fields present → ingested_at must be server-stamped"
    );
    assert_ne!(
        result.ingested_at.as_deref(),
        Some("1999-01-01T00:00:00Z"),
        "server stamp must ignore input.ingested_at"
    );
    // Must be a plausible recent timestamp (starts with 202)
    assert!(
        result.ingested_at.as_ref().unwrap().starts_with("202"),
        "server stamp must be a recent timestamp, got {:?}",
        result.ingested_at,
    );
    engine.disconnect().await.expect("disconnect");
}

#[tokio::test]
async fn s6t6_ingested_at_no_stamp_without_provenance() {
    // When none of source_kind / source_uri / ingested_via are set, ingested_at
    // must remain null (server does not stamp).
    let (engine, _tmp) = init_clean_engine().await;
    let mut input = full_input();
    input.source_kind = None;
    input.source_uri = None;
    input.ingested_via = None;
    let result = engine
        .put_page("stamp-none", &input)
        .await
        .expect("put_page");
    assert!(
        result.ingested_at.is_none(),
        "no provenance fields → ingested_at must be null, got {:?}",
        result.ingested_at,
    );
    engine.disconnect().await.expect("disconnect");
}

#[tokio::test]
async fn s6t6_chunker_version_defaults_to_1() {
    // When chunker_version is None in PageInput, INSERT must use COALESCE
    // to default to 1 (matching TS COALESCE($13, 1)).
    let (engine, _tmp) = init_clean_engine().await;
    let mut input = full_input();
    input.chunker_version = None;
    let result = engine
        .put_page("chunker-def", &input)
        .await
        .expect("put_page");
    assert_eq!(result.chunker_version, 1, "null chunker_version must default to 1");

    // Explicit value must be stored
    let mut explicit = full_input();
    explicit.chunker_version = Some(3);
    let explicit_result = engine
        .put_page("chunker-explicit", &explicit)
        .await
        .expect("put_page explicit");
    assert_eq!(explicit_result.chunker_version, 3);
    engine.disconnect().await.expect("disconnect");
}

#[tokio::test]
async fn s6t6_frontmatter_json_roundtrip() {
    // frontmatter must survive a full JSON roundtrip: write via put_page,
    // read back via get_page, compare structure.
    let (engine, _tmp) = init_clean_engine().await;
    let fm = json!({
        "string": "hello",
        "number": 42,
        "nested": { "a": true }
    });
    let mut input = full_input();
    input.frontmatter = Some(fm.clone());
    let result = engine
        .put_page("fm-roundtrip", &input)
        .await
        .expect("put_page");
    assert_eq!(result.frontmatter, fm, "frontmatter must roundtrip exactly");

    // Verify through get_page as well (full 30-col projection)
    let got = engine
        .get_page("fm-roundtrip", &GetPageOpts::default())
        .await
        .expect("get_page")
        .expect("Some(page)");
    assert_eq!(got.frontmatter, fm, "frontmatter via get_page must match");
    engine.disconnect().await.expect("disconnect");
}

#[tokio::test]
async fn s6t6_generation_bumps_on_watched_column_update() {
    // The bump_page_generation_fn trigger (migration 0002+0003) bumps
    // generation when a watched column (e.g. compiled_truth) changes.
    // Two puts with different compiled_truth values must show generation >= 2.
    let (engine, _tmp) = init_clean_engine().await;
    let first = engine
        .put_page("gen-bump", &full_input())
        .await
        .expect("first put");
    assert_eq!(first.generation, 1, "initial generation must be 1");

    // Change a watched column
    let mut update = full_input();
    update.compiled_truth = "changed body".to_string();
    let second = engine
        .put_page("gen-bump", &update)
        .await
        .expect("second put");
    assert!(
        second.generation > first.generation,
        "generation must bump on watched-column update, got first={} second={}",
        first.generation,
        second.generation,
    );
    engine.disconnect().await.expect("disconnect");
}

#[tokio::test]
async fn s6t6_returning_projection_covers_all_30_columns() {
    // The RETURNING clause must feed full_row_to_page so that every Page
    // field is populated (not defaulted). We assert a representative set
    // of columns that the 7-col stub would leave as schema defaults.
    let (engine, _tmp) = init_clean_engine().await;
    let mut input = full_input();
    input.effective_date = Some("2026-05-01".to_string());
    input.effective_date_source = Some(EffectiveDateSource::Date);
    input.import_filename = Some("doc.md".to_string());
    input.content_hash = Some("sha256:deadbeef".to_string());
    let result = engine
        .put_page("proj-30col", &input)
        .await
        .expect("put_page");

    // Fields the 7-col stub would fabricate — must come from the DB row
    assert_eq!(result.source_id, "default");
    assert_eq!(result.generation, 1);
    assert!(!result.created_at.is_empty(), "created_at from DB");
    assert!(!result.updated_at.is_empty(), "updated_at from DB");
    assert!(result.deleted_at.is_none(), "live row has no deleted_at");
    assert_eq!(result.effective_date.as_deref(), Some("2026-05-01"));
    assert_eq!(result.effective_date_source, Some(EffectiveDateSource::Date));
    assert_eq!(result.import_filename.as_deref(), Some("doc.md"));
    assert_eq!(result.content_hash.as_deref(), Some("sha256:deadbeef"));
    assert_eq!(result.chunker_version, 2); // set in full_input()
    assert!(result.frontmatter.is_object(), "frontmatter must be an object");
    assert_eq!(result.ingested_via.as_deref(), Some("cli"));
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
