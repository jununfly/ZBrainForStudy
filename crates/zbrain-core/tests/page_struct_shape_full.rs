//! Slice 6a S5 — `Page` / `PageInput` field-completeness check.
//!
//! S2 expanded the structs to 24 / 14 fields, but six `Page` columns and
//! two `PageInput` columns that exist in the 0002 / 0003 schema are still
//! missing in the Rust value types. This slice closes that gap so S6 can
//! teach the libsql / postgres engines to read and write every column.
//!
//! Missing in `Page`:
//!   - `salience_score: Option<f64>` (added to SQL in S4; struct catch-up)
//!   - `last_retrieved_at: Option<String>`
//!   - `generation: i64`
//!   - `embedding: Option<Vec<u8>>` (BLOB; format deferred to 6e per C4)
//!   - `chunker_version: i32`
//!   - `source_path: Option<String>`
//!
//! Missing in `PageInput` (`chunker_version` + `source_path` already landed in S2):
//!   - `last_retrieved_at: Option<String>`
//!   - `embedding: Option<Vec<u8>>`
//!
//! Each test is a pure compile-time + value-shape assertion: it fails to
//! compile until the corresponding field exists on the struct.

use serde_json::json;

use zbrain_core::engine::{
    BrainEngine, EngineConfig, GetPageOpts, InMemoryEngine, Page, PageInput,
};
use zbrain_core::{CRMode, EffectiveDateSource, PageKind};

// ─── Page full-30-field projection ────────────────────────────────────────────

/// Build a `Page` with **all** columns set so that any missing field fails
/// compilation. This is the single most-load-bearing red assertion for S5.
#[test]
fn page_struct_has_all_30_fields_including_s5_additions() {
    let page = Page {
        // ── identity (unchanged from S2) ────────────────────────────────
        id: 7,
        slug: "s5".to_string(),
        page_type: "note".to_string(),
        page_kind: PageKind::Markdown,
        title: "T".to_string(),
        compiled_truth: "C".to_string(),
        timeline: String::new(),

        // ── content metadata ────────────────────────────────────────────
        frontmatter: json!({"k": "v"}),
        content_hash: Some("sha256:abc".to_string()),
        emotional_weight: Some(0.5),

        // ── timestamps ─────────────────────────────────────────────────
        created_at: "2026-05-28T00:00:00Z".to_string(),
        updated_at: "2026-05-28T01:00:00Z".to_string(),
        deleted_at: None,
        // NEW IN S5: tracks last read access; powers salience decay.
        last_retrieved_at: Some("2026-05-28T02:00:00Z".to_string()),

        // ── effective-date chain ───────────────────────────────────────
        effective_date: Some("2026-05-28".to_string()),
        effective_date_source: Some(EffectiveDateSource::EventDate),
        import_filename: Some("2026-05-28-s5".to_string()),

        // ── salience ───────────────────────────────────────────────────
        salience_touched_at: None,
        // NEW IN S5 (catch-up for S4's SQL column): per-page salience
        // score persisted alongside `salience_touched_at`. Excluded from
        // the bump_generation allow-list per S4 commit.
        salience_score: Some(0.75_f64),

        // ── generation / embedding (NEW IN S5) ─────────────────────────
        // generation: monotonic per-page revision counter, bumped by the
        // 0002/0003 triggers when watched columns change.
        generation: 3,
        // embedding: vector blob. Encoding deferred to slice 6e (C4).
        embedding: Some(vec![0x00, 0x01, 0x02, 0x03]),

        // ── chunker + source path (NEW IN S5) ──────────────────────────
        chunker_version: 2,
        source_path: Some("notes/s5.md".to_string()),

        // ── source / provenance (unchanged from S2) ────────────────────
        source_id: "default".to_string(),
        source_kind: Some("capture-cli".to_string()),
        source_uri: Some("file:///tmp/s5.md".to_string()),
        ingested_via: Some("capture-cli".to_string()),
        ingested_at: Some("2026-05-28T00:00:00Z".to_string()),

        // ── contextual retrieval (unchanged from S2) ───────────────────
        contextual_retrieval_mode: Some(CRMode::Title),
        corpus_generation: Some("gen-abc".to_string()),
    };

    // Spot-check the six new fields so a rename fails loudly.
    assert_eq!(page.salience_score, Some(0.75_f64));
    assert_eq!(
        page.last_retrieved_at.as_deref(),
        Some("2026-05-28T02:00:00Z")
    );
    assert_eq!(page.generation, 3);
    assert_eq!(
        page.embedding.as_deref(),
        Some(&[0x00, 0x01, 0x02, 0x03][..])
    );
    assert_eq!(page.chunker_version, 2);
    assert_eq!(page.source_path.as_deref(), Some("notes/s5.md"));
}

// ─── PageInput delta (only 2 new fields; chunker_version + source_path
//     already landed in S2) ──────────────────────────────────────────────────

/// `PageInput` must accept the two new optional write-side fields. Combined
/// with the S2-era 14 fields this brings the write surface to 16.
#[test]
fn page_input_accepts_last_retrieved_at_and_embedding() {
    let input = PageInput {
        page_type: "note".to_string(),
        title: "T".to_string(),
        compiled_truth: "C".to_string(),
        // NEW IN S5:
        last_retrieved_at: Some("2026-05-28T00:00:00Z".to_string()),
        embedding: Some(vec![0xDE, 0xAD, 0xBE, 0xEF]),
        ..Default::default()
    };

    assert_eq!(
        input.last_retrieved_at.as_deref(),
        Some("2026-05-28T00:00:00Z")
    );
    assert_eq!(
        input.embedding.as_deref(),
        Some(&[0xDE, 0xAD, 0xBE, 0xEF][..])
    );
}

/// Minimal-callers regression: `..Default::default()` must still yield
/// `None` for both new fields so existing call sites do not regress.
#[test]
fn page_input_default_leaves_new_fields_none() {
    let minimal = PageInput::default();
    assert!(minimal.last_retrieved_at.is_none());
    assert!(minimal.embedding.is_none());
}

// ─── InMemoryEngine default-value compatibility ───────────────────────────────

/// `InMemoryEngine::put_page` must initialise the six new `Page` fields to
/// safe defaults so existing tests that only set three input fields keep
/// passing. `generation` starts at `1` to mirror PG's `BIGINT DEFAULT 1`.
#[tokio::test]
async fn in_memory_engine_initialises_new_page_fields_with_safe_defaults() {
    let engine = InMemoryEngine::default();
    engine.connect(&EngineConfig::default()).await.unwrap();

    let input = PageInput {
        page_type: "note".to_string(),
        title: "Hi".to_string(),
        compiled_truth: "Body".to_string(),
        ..Default::default()
    };
    engine.put_page("hi", None, &input).await.expect("put ok");

    let fetched = engine
        .get_page("hi", &GetPageOpts::default())
        .await
        .expect("get ok")
        .expect("present");

    // generation starts at 1 (PG default), not 0.
    assert_eq!(fetched.generation, 1);
    // chunker_version defaults to 1 (PG `DEFAULT 1`).
    assert_eq!(fetched.chunker_version, 1);
    // Four optional fields default to None.
    assert!(fetched.salience_score.is_none());
    assert!(fetched.last_retrieved_at.is_none());
    assert!(fetched.embedding.is_none());
    assert!(fetched.source_path.is_none());
}
