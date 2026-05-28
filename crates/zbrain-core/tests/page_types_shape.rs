//! Slice 6a S2 — `Page` / `PageInput` / `PageFilters` shape validation.
//!
//! Slice 6a S1 landed the full schema (0002 migration); this slice expands the
//! Rust value types to mirror the TS `Page` / `PageInput` / `PageFilters` shape
//! (`zbrain/src/core/types.ts:73|199|277`). S3-S9 then teach the libsql engine
//! to read/write the new columns; the trait method names do not change.
//!
//! These tests are pure compile-time + value-shape assertions: they prove the
//! struct fields exist, default sensibly, and that `PageSort` round-trips to
//! the same SQL fragments the TS `PAGE_SORT_SQL` table uses (parity check —
//! a divergence here would break order-stable pagination on the wire).

use serde_json::json;

use zbrain_core::engine::{
    page_sort_sql, BrainEngine, EngineConfig, GetPageOpts, InMemoryEngine, Page, PageFilters,
    PageInput, PageSort,
};
use zbrain_core::{CRMode, EffectiveDateSource, PageKind};

// ─── Page shape ───────────────────────────────────────────────────────────────

/// Page must carry the full S2 24-column projection plus the 5 columns added
/// by S5 (`salience_score`, `last_retrieved_at`, `generation`, `embedding`,
/// `chunker_version`, `source_path`) so that `rowToPage` equivalent code in
/// libsql/postgres engines has somewhere to land each column. NULL-able TS
/// fields map to `Option<…>`; required ones to owned values.
#[test]
fn page_has_full_column_projection() {
    let page = Page {
        id: 42,
        slug: "alpha".to_string(),
        page_type: "note".to_string(),
        page_kind: PageKind::Markdown,
        title: "T".to_string(),
        compiled_truth: "C".to_string(),
        timeline: String::new(),
        frontmatter: json!({"tags": ["a", "b"]}),
        content_hash: Some("sha256:deadbeef".to_string()),
        emotional_weight: Some(0.42_f64),
        created_at: "2026-05-28T00:00:00Z".to_string(),
        updated_at: "2026-05-28T01:00:00Z".to_string(),
        deleted_at: None,
        effective_date: Some("2026-05-28".to_string()),
        effective_date_source: Some(EffectiveDateSource::EventDate),
        import_filename: Some("2026-05-28-alpha".to_string()),
        salience_touched_at: None,
        source_id: "default".to_string(),
        source_kind: Some("capture-cli".to_string()),
        source_uri: Some("file:///tmp/a.md".to_string()),
        ingested_via: Some("capture-cli".to_string()),
        ingested_at: Some("2026-05-28T00:00:00Z".to_string()),
        contextual_retrieval_mode: Some(CRMode::Title),
        corpus_generation: Some("abc123".to_string()),
        // S5 additions — mirror types.ts:73 (salience_score + 5 others).
        salience_score: Some(0.42_f64),
        last_retrieved_at: Some("2026-05-28T02:00:00Z".to_string()),
        generation: 1,
        embedding: None,
        chunker_version: 1,
        source_path: Some("notes/alpha.md".to_string()),
    };

    // Spot-check round-trip + struct equality so the test fails loudly if any
    // field gets renamed.
    let clone = page.clone();
    assert_eq!(clone.id, 42);
    assert_eq!(clone.source_id, "default");
    assert_eq!(clone.contextual_retrieval_mode, Some(CRMode::Title));
    assert_eq!(
        clone.effective_date_source,
        Some(EffectiveDateSource::EventDate)
    );
    assert_eq!(clone.frontmatter, json!({"tags": ["a", "b"]}));
}

// ─── PageInput shape ──────────────────────────────────────────────────────────

/// `PageInput` mirrors the TS write-side shape (`types.ts:199`). All
/// non-required fields are `Option<…>` so callers can keep using the minimal
/// 3-field form (`Default::default()` plus `page_type`/`title`/`compiled_truth`).
#[test]
fn page_input_has_full_optional_surface() {
    let input = PageInput {
        page_type: "note".to_string(),
        title: "T".to_string(),
        compiled_truth: "C".to_string(),
        timeline: Some("…".to_string()),
        frontmatter: Some(json!({"k": "v"})),
        content_hash: Some("sha256:abc".to_string()),
        page_kind: Some(PageKind::Code),
        effective_date: Some("2026-05-28".to_string()),
        effective_date_source: Some(EffectiveDateSource::Filename),
        import_filename: Some("2026-05-28-t".to_string()),
        chunker_version: Some(2),
        source_path: Some("notes/t.md".to_string()),
        source_kind: Some("capture-cli".to_string()),
        source_uri: Some("file:///tmp/t.md".to_string()),
        ingested_via: Some("capture-cli".to_string()),
        ingested_at: Some("2026-05-28T00:00:00Z".to_string()),
        // S5 additions — mirror types.ts:199.
        last_retrieved_at: Some("2026-05-28T03:00:00Z".to_string()),
        embedding: Some(vec![1_u8, 2, 3, 4]),
    };

    assert_eq!(input.chunker_version, Some(2));
    assert_eq!(input.page_kind, Some(PageKind::Code));
    assert_eq!(input.source_path.as_deref(), Some("notes/t.md"));
}

/// The minimal write-form must still compile via `..Default::default()` so
/// the 30+ existing callers (Slice 4b/5 tests, in-memory mock) do not have
/// to be touched in this slice.
#[test]
fn page_input_default_keeps_minimal_callers_working() {
    let minimal = PageInput {
        page_type: "note".to_string(),
        title: "T".to_string(),
        compiled_truth: "C".to_string(),
        ..Default::default()
    };
    assert_eq!(minimal.page_kind, None);
    assert!(minimal.frontmatter.is_none());
    assert!(minimal.timeline.is_none());
}

// ─── PageFilters shape ────────────────────────────────────────────────────────

/// Default filter is a no-op: no `page_type` / no tag / no offset / no sort.
/// This anchors the "list everything" path the `InMemoryEngine` relies on.
#[test]
fn page_filters_default_is_unscoped() {
    let f = PageFilters::default();
    assert!(f.page_type.is_none());
    assert!(f.tag.is_none());
    assert!(f.limit.is_none());
    assert!(f.offset.is_none());
    assert!(f.updated_after.is_none());
    assert!(f.slug_prefix.is_none());
    assert!(!f.include_deleted);
    assert!(f.sort.is_none());
    assert!(f.source_id.is_none());
    assert!(f.source_ids.is_none());
}

/// Demonstrates the full set of filter fields so a refactor that drops one
/// fails compile here.
#[test]
fn page_filters_carries_full_surface() {
    let f = PageFilters {
        page_type: Some("note".to_string()),
        tag: Some("important".to_string()),
        limit: Some(20),
        offset: Some(40),
        updated_after: Some("2026-05-28".to_string()),
        slug_prefix: Some("daily/".to_string()),
        include_deleted: true,
        sort: Some(PageSort::CreatedDesc),
        source_id: Some("default".to_string()),
        source_ids: Some(vec!["a".to_string(), "b".to_string()]),
    };
    assert_eq!(f.page_type.as_deref(), Some("note"));
    assert_eq!(f.tag.as_deref(), Some("important"));
    assert_eq!(f.limit, Some(20));
    assert_eq!(f.offset, Some(40));
    assert_eq!(f.slug_prefix.as_deref(), Some("daily/"));
    assert!(f.include_deleted);
    assert_eq!(f.sort, Some(PageSort::CreatedDesc));
    assert_eq!(f.source_ids.as_deref().map(<[String]>::len), Some(2));
}

// ─── PageSort SQL parity ──────────────────────────────────────────────────────

/// `page_sort_sql` must produce the same fragments as the TS
/// `PAGE_SORT_SQL` table at `types.ts:332`. This is the wire-level parity
/// check — engines splice these literal fragments into prepared statements,
/// so any drift here changes pagination order.
#[test]
fn page_sort_sql_matches_typescript_table() {
    assert_eq!(page_sort_sql(PageSort::UpdatedDesc), "p.updated_at DESC");
    assert_eq!(page_sort_sql(PageSort::UpdatedAsc), "p.updated_at ASC");
    assert_eq!(page_sort_sql(PageSort::CreatedDesc), "p.created_at DESC");
    assert_eq!(page_sort_sql(PageSort::Slug), "p.slug ASC");
}

/// Default sort matches the TS default (pre-v0.29 behavior: updated DESC).
#[test]
fn page_sort_default_is_updated_desc() {
    assert_eq!(PageSort::default(), PageSort::UpdatedDesc);
}

// ─── InMemoryEngine compatibility ─────────────────────────────────────────────

/// Sanity: the mock engine still round-trips a minimal `PageInput` after the
/// struct grows. The mock only needs to populate the 3 caller-visible fields;
/// the rest may default to None / now-ish strings.
#[tokio::test]
async fn in_memory_engine_round_trips_after_struct_expansion() {
    let engine = InMemoryEngine::default();
    engine.connect(&EngineConfig::default()).await.unwrap();

    let input = PageInput {
        page_type: "note".to_string(),
        title: "Hi".to_string(),
        compiled_truth: "Body".to_string(),
        ..Default::default()
    };
    let stored = engine.put_page("hi", &input).await.expect("put ok");

    let fetched = engine
        .get_page("hi", &GetPageOpts::default())
        .await
        .expect("get ok")
        .expect("present");
    assert_eq!(fetched.id, stored.id);
    assert_eq!(fetched.title, "Hi");
    // Newly added fields land with safe defaults — mock does NOT need to
    // synthesise realistic timestamps.
    assert_eq!(fetched.source_id, "default");
    assert!(fetched.deleted_at.is_none());
}
