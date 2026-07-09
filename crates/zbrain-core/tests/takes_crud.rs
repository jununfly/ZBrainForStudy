//! Phase 7A: Takes CRUD integration tests.
//!
//! Covers `add_takes_batch`, `get_takes_for_page`, `resolve_take`, and
//! `CalibrationQueries::get_scorecard` across InMemory and Libsql backends.
//!
//! Fence round-trip tests live in `crates/zbrain-core/src/takes_fence.rs`;
//! salience-with-takes behavioural tests live in
//! `page_methods_salience_scores_with_takes.rs`.

mod support;

use tempfile::NamedTempFile;
use zbrain_core::engine::{BrainEngine, EngineConfig, PageInput};
use zbrain_core::libsql::LibsqlEngine;
use zbrain_core::{CalibrationQueries, InMemoryEngine, TakeInput, TakeResolution, TakesScorecard};

// ---------------------------------------------------------------------------
// InMemoryEngine tests
// ---------------------------------------------------------------------------

fn ti(weight: f64, claim: &str, row_num: i32) -> TakeInput {
    TakeInput {
        page_id: 1,
        row_num: Some(row_num),
        claim: claim.to_string(),
        kind: "take".to_string(),
        holder: "alice".to_string(),
        weight,
        since_date: None,
        until_date: None,
        source: None,
        superseded_by: None,
        active: None,
    }
}

// ── add_takes_batch + get_takes_for_page round-trip ─────────────────────

#[tokio::test]
async fn inmem_roundtrip_single_take() {
    let engine = InMemoryEngine::new();
    let res = engine
        .add_takes_batch(1, &[ti(0.8, "claim-1", 0)])
        .await
        .expect("add_takes_batch");
    assert_eq!(res.upserted, 1);
    assert_eq!(res.weight_clamped, 0);

    let takes = engine.get_takes_for_page(1).await.expect("get_takes");
    assert_eq!(takes.len(), 1);
    let t = &takes[0];
    assert_eq!(t.page_id, 1);
    assert_eq!(t.row_num, 0);
    assert_eq!(t.claim, "claim-1");
    assert_eq!(t.kind, "take");
    assert_eq!(t.holder, "alice");
    assert!((t.weight - 0.8).abs() < 1e-9);
    assert!(t.active);
    assert!(t.resolved_at.is_none());
    assert!(t.resolved_outcome.is_none());
    assert!(!t.created_at.is_empty());
    assert!(!t.updated_at.is_empty());
}

#[tokio::test]
async fn inmem_roundtrip_multiple_takes_ordered_by_row_num() {
    let engine = InMemoryEngine::new();
    let res = engine
        .add_takes_batch(
            1,
            &[
                ti(0.9, "third", 3),
                ti(0.5, "first", 1),
                ti(0.7, "second", 2),
            ],
        )
        .await
        .expect("add_takes_batch");
    assert_eq!(res.upserted, 3);

    let takes = engine.get_takes_for_page(1).await.expect("get_takes");
    assert_eq!(takes.len(), 3);
    // Must be sorted by row_num
    assert_eq!(takes[0].row_num, 1);
    assert_eq!(takes[0].claim, "first");
    assert_eq!(takes[1].row_num, 2);
    assert_eq!(takes[1].claim, "second");
    assert_eq!(takes[2].row_num, 3);
    assert_eq!(takes[2].claim, "third");
}

#[tokio::test]
async fn inmem_roundtrip_multi_page_isolation() {
    let engine = InMemoryEngine::new();
    engine
        .add_takes_batch(1, &[ti(0.5, "page-1-claim", 0)])
        .await
        .expect("add for page 1");
    engine
        .add_takes_batch(2, &[ti(0.8, "page-2-claim", 0)])
        .await
        .expect("add for page 2");

    let p1 = engine.get_takes_for_page(1).await.expect("get p1");
    let p2 = engine.get_takes_for_page(2).await.expect("get p2");
    assert_eq!(p1.len(), 1);
    assert_eq!(p1[0].claim, "page-1-claim");
    assert_eq!(p2.len(), 1);
    assert_eq!(p2[0].claim, "page-2-claim");
}

#[tokio::test]
async fn inmem_get_takes_for_nonexistent_page_returns_empty() {
    let engine = InMemoryEngine::new();
    let takes = engine.get_takes_for_page(999).await.expect("get_takes");
    assert!(takes.is_empty());
}

// ── Weight clamping ────────────────────────────────────────────────────

#[tokio::test]
async fn inmem_weight_clamped_to_0_1_range() {
    let engine = InMemoryEngine::new();
    let res = engine
        .add_takes_batch(
            1,
            &[
                ti(1.5, "over", 0),   // clamped to 1.0
                ti(-0.3, "under", 1), // clamped to 0.0
                ti(0.6, "valid", 2),  // no change
            ],
        )
        .await
        .expect("add_takes_batch");
    assert_eq!(res.upserted, 3);
    assert_eq!(res.weight_clamped, 2);

    let takes = engine.get_takes_for_page(1).await.expect("get_takes");
    assert!((takes[0].weight - 1.0).abs() < 1e-9, "over -> 1.0");
    assert!((takes[1].weight - 0.0).abs() < 1e-9, "under -> 0.0");
    assert!((takes[2].weight - 0.6).abs() < 1e-9, "valid unchanged");
}

// ── resolve_take ───────────────────────────────────────────────────────

#[tokio::test]
async fn inmem_resolve_take_updates_fields() {
    let engine = InMemoryEngine::new();
    engine
        .add_takes_batch(1, &[ti(0.7, "resolve-me", 0)])
        .await
        .expect("add");

    let res = TakeResolution {
        page_id: 1,
        row_num: 0,
        quality: Some("high".to_string()),
        outcome: Some(true),
        evidence: Some("confirmed by source".to_string()),
        value: Some(42.0),
        unit: Some("count".to_string()),
        by: Some("bob".to_string()),
    };
    engine
        .resolve_take(1, 0, &res)
        .await
        .expect("resolve_take");

    let takes = engine.get_takes_for_page(1).await.expect("get_takes");
    let t = &takes[0];
    assert_eq!(t.resolved_quality.as_deref(), Some("high"));
    assert_eq!(t.resolved_outcome, Some(true));
    assert_eq!(t.resolved_evidence.as_deref(), Some("confirmed by source"));
    assert!((t.resolved_value.unwrap() - 42.0).abs() < 1e-9);
    assert_eq!(t.resolved_unit.as_deref(), Some("count"));
    assert_eq!(t.resolved_by.as_deref(), Some("bob"));
    assert!(t.resolved_at.is_some());
    assert!(!t.resolved_at.as_ref().unwrap().is_empty());
    // updated_at should be bumped (at least not empty)
    assert!(!t.updated_at.is_empty());
}

#[tokio::test]
async fn inmem_resolve_nonexistent_returns_error() {
    let engine = InMemoryEngine::new();
    let res = TakeResolution {
        page_id: 1,
        row_num: 0,
        quality: None,
        outcome: None,
        evidence: None,
        value: None,
        unit: None,
        by: None,
    };
    let err = engine
        .resolve_take(999, 0, &res)
        .await
        .expect_err("should fail");
    let msg = format!("{err:?}");
    assert!(msg.contains("not_found") || msg.contains("Not Found"), "error should mention not_found: {msg}");
}

// ── Scorecard (InMemory CalibrationQueries) ────────────────────────────

#[tokio::test]
async fn inmem_scorecard_empty_returns_zeros() {
    let engine = InMemoryEngine::new();
    let sc: TakesScorecard = engine.get_scorecard("alice").await.expect("get_scorecard");
    assert_eq!(sc.resolved, 0);
    assert_eq!(sc.brier, 0.0);
    assert_eq!(sc.accuracy, 0.0);
    assert_eq!(sc.correct, 0);
    assert_eq!(sc.incorrect, 0);
}

#[tokio::test]
async fn inmem_scorecard_single_resolved_correct() {
    let engine = InMemoryEngine::new();
    engine
        .add_takes_batch(1, &[ti(0.8, "pred-1", 0)])
        .await
        .expect("add");
    engine
        .resolve_take(
            1,
            0,
            &TakeResolution {
                page_id: 1,
                row_num: 0,
                quality: Some("high".to_string()),
                outcome: Some(true),
                evidence: None,
                value: None,
                unit: None,
                by: None,
            },
        )
        .await
        .expect("resolve");

    let sc: TakesScorecard = engine.get_scorecard("alice").await.expect("get_scorecard");
    assert_eq!(sc.resolved, 1);
    assert_eq!(sc.correct, 1);
    assert_eq!(sc.incorrect, 0);
    assert!((sc.accuracy - 1.0).abs() < 1e-9);
    // Brier: (0.8 - 1.0)² = 0.04
    assert!((sc.brier - 0.04).abs() < 1e-9, "Brier should be 0.04, got {}", sc.brier);
}

#[tokio::test]
async fn inmem_scorecard_single_resolved_incorrect() {
    let engine = InMemoryEngine::new();
    engine
        .add_takes_batch(1, &[ti(0.9, "pred-1", 0)])
        .await
        .expect("add");
    engine
        .resolve_take(
            1,
            0,
            &TakeResolution {
                page_id: 1,
                row_num: 0,
                quality: None,
                outcome: Some(false),
                evidence: None,
                value: None,
                unit: None,
                by: None,
            },
        )
        .await
        .expect("resolve");

    let sc: TakesScorecard = engine.get_scorecard("alice").await.expect("get_scorecard");
    assert_eq!(sc.resolved, 1);
    assert_eq!(sc.correct, 0);
    assert_eq!(sc.incorrect, 1);
    assert!((sc.accuracy - 0.0).abs() < 1e-9);
    // Brier: (0.9 - 0.0)² = 0.81
    assert!((sc.brier - 0.81).abs() < 1e-9, "Brier should be 0.81, got {}", sc.brier);
}

#[tokio::test]
async fn inmem_scorecard_multiple_mixed() {
    let engine = InMemoryEngine::new();
    // Take 0: weight=0.6, resolved=true  → (0.6-1)² = 0.16, correct
    // Take 1: weight=0.8, resolved=false → (0.8-0)² = 0.64, incorrect
    // Take 2: weight=0.3, resolved=true  → (0.3-1)² = 0.49, correct
    engine
        .add_takes_batch(
            1,
            &[
                ti(0.6, "a", 0),
                ti(0.8, "b", 1),
                ti(0.3, "c", 2),
            ],
        )
        .await
        .expect("add");

    let resolve = |row_num: i32, outcome: bool| {
        TakeResolution {
            page_id: 1,
            row_num,
            quality: None,
            outcome: Some(outcome),
            evidence: None,
            value: None,
            unit: None,
            by: None,
        }
    };
    engine.resolve_take(1, 0, &resolve(0, true)).await.expect("resolve 0");
    engine.resolve_take(1, 1, &resolve(1, false)).await.expect("resolve 1");
    engine.resolve_take(1, 2, &resolve(2, true)).await.expect("resolve 2");

    let sc: TakesScorecard = engine.get_scorecard("alice").await.expect("get_scorecard");
    assert_eq!(sc.resolved, 3);
    assert_eq!(sc.correct, 2);
    assert_eq!(sc.incorrect, 1);
    let expected_acc = 2.0 / 3.0;
    assert!((sc.accuracy - expected_acc).abs() < 1e-9);
    let expected_brier = (0.16 + 0.64 + 0.49) / 3.0;
    assert!(
        (sc.brier - expected_brier).abs() < 1e-9,
        "Brier should be ~{expected_brier}, got {}",
        sc.brier
    );
}

#[tokio::test]
async fn inmem_scorecard_filters_by_holder() {
    let engine = InMemoryEngine::new();
    // alice: 2 takes
    engine
        .add_takes_batch(
            1,
            &[
                ti(0.5, "alice-1", 0),
                TakeInput {
                    holder: "bob".to_string(),
                    ..ti(0.9, "bob-1", 1)
                },
            ],
        )
        .await
        .expect("add");

    let resolve = |row_num: i32, outcome: bool| TakeResolution {
        page_id: 1,
        row_num,
        quality: None,
        outcome: Some(outcome),
        evidence: None,
        value: None,
        unit: None,
        by: None,
    };
    engine.resolve_take(1, 0, &resolve(0, true)).await.expect("resolve alice");
    engine.resolve_take(1, 1, &resolve(1, false)).await.expect("resolve bob");

    let sc_alice = engine.get_scorecard("alice").await.expect("alice");
    assert_eq!(sc_alice.resolved, 1, "alice should see 1 resolved take");
    assert_eq!(sc_alice.correct, 1);

    let sc_bob = engine.get_scorecard("bob").await.expect("bob");
    assert_eq!(sc_bob.resolved, 1, "bob should see 1 resolved take");
    assert_eq!(sc_bob.incorrect, 1);
}

#[tokio::test]
async fn inmem_scorecard_ignores_unresolved() {
    let engine = InMemoryEngine::new();
    engine
        .add_takes_batch(1, &[ti(0.5, "unresolved", 0)])
        .await
        .expect("add");
    // NOT resolved — should not appear in scorecard

    let sc: TakesScorecard = engine.get_scorecard("alice").await.expect("get_scorecard");
    assert_eq!(sc.resolved, 0);
}

// ---------------------------------------------------------------------------
// LibsqlEngine tests
// ---------------------------------------------------------------------------

fn libsql_note_input(title: &str, body: &str) -> PageInput {
    PageInput {
        page_type: "note".to_string(),
        title: title.to_string(),
        compiled_truth: body.to_string(),
        ..PageInput::default()
    }
}

async fn libsql_init() -> (LibsqlEngine, NamedTempFile) {
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

/// Look up page_id by (slug, source_id) for setting up takes via raw SQL.
async fn libsql_page_id(tmp: &NamedTempFile, slug: &str, source_id: &str) -> i64 {
    let path_str = tmp.path().to_string_lossy().into_owned();
    let db = ::libsql::Builder::new_local(&path_str)
        .build()
        .await
        .expect("raw db");
    let raw_conn = db.connect().expect("raw conn");
    let mut rows = raw_conn
        .query(
            "SELECT id FROM pages WHERE slug = ?1 AND source_id = ?2",
            ::libsql::params![slug, source_id],
        )
        .await
        .expect("page_id query");
    let row = rows.next().await.expect("page_id row").expect("page present");
    row.get::<i64>(0).expect("id value")
}

async fn libsql_seed_page(
    engine: &LibsqlEngine,
    tmp: &NamedTempFile,
    slug: &str,
    source: &str,
) -> u64 {
    libsql_seed_source(tmp, source).await;
    engine
        .put_page(slug, Some(source), &libsql_note_input(slug, "body"))
        .await
        .expect("seed page");
    let pid = libsql_page_id(tmp, slug, source).await;
    assert!(pid > 0);
    // put_page returns the old page version; we trust the raw SQL id.
    pid as u64
}

#[tokio::test]
async fn libsql_roundtrip_single_take() {
    let (engine, tmp) = libsql_init().await;
    let page_id = libsql_seed_page(&engine, &tmp, "test-page", "src-1").await;

    let res = engine
        .add_takes_batch(
            page_id,
            &[TakeInput {
                page_id,
                row_num: Some(0),
                claim: "libsql-claim".to_string(),
                kind: "take".to_string(),
                holder: "alice".to_string(),
                weight: 0.75,
                since_date: None,
                until_date: None,
                source: None,
                superseded_by: None,
                active: None,
            }],
        )
        .await
        .expect("add_takes_batch");
    assert_eq!(res.upserted, 1);
    assert_eq!(res.weight_clamped, 0);

    let takes = engine
        .get_takes_for_page(page_id)
        .await
        .expect("get_takes");
    assert_eq!(takes.len(), 1);
    let t = &takes[0];
    assert_eq!(t.page_id, page_id);
    assert_eq!(t.claim, "libsql-claim");
    assert_eq!(t.kind, "take");
    assert_eq!(t.holder, "alice");
    assert!((t.weight - 0.75).abs() < 1e-9);
    assert!(t.active);
    engine.disconnect().await.expect("disconnect");
}

#[tokio::test]
async fn libsql_multiple_takes_ordered() {
    let (engine, tmp) = libsql_init().await;
    let page_id = libsql_seed_page(&engine, &tmp, "multi", "src-1").await;

    engine
        .add_takes_batch(
            page_id,
            &[
                TakeInput {
                    page_id,
                    row_num: Some(2),
                    claim: "second".to_string(),
                    kind: "take".to_string(),
                    holder: "alice".to_string(),
                    weight: 0.5,
                    since_date: None,
                    until_date: None,
                    source: None,
                    superseded_by: None,
                    active: None,
                },
                TakeInput {
                    page_id,
                    row_num: Some(0),
                    claim: "first".to_string(),
                    kind: "take".to_string(),
                    holder: "alice".to_string(),
                    weight: 0.5,
                    since_date: None,
                    until_date: None,
                    source: None,
                    superseded_by: None,
                    active: None,
                },
            ],
        )
        .await
        .expect("add");

    let takes = engine
        .get_takes_for_page(page_id)
        .await
        .expect("get");
    assert_eq!(takes.len(), 2);
    assert_eq!(takes[0].claim, "first");
    assert_eq!(takes[1].claim, "second");
    engine.disconnect().await.expect("disconnect");
}

#[tokio::test]
async fn libsql_weight_clamping() {
    let (engine, tmp) = libsql_init().await;
    let page_id = libsql_seed_page(&engine, &tmp, "clamp", "src-1").await;

    let res = engine
        .add_takes_batch(
            page_id,
            &[TakeInput {
                page_id,
                row_num: Some(0),
                claim: "over".to_string(),
                kind: "take".to_string(),
                holder: "alice".to_string(),
                weight: 2.0, // clamped to 1.0
                since_date: None,
                until_date: None,
                source: None,
                superseded_by: None,
                active: None,
            }],
        )
        .await
        .expect("add");
    assert_eq!(res.weight_clamped, 1);

    let takes = engine
        .get_takes_for_page(page_id)
        .await
        .expect("get");
    assert!((takes[0].weight - 1.0).abs() < 1e-9);
    engine.disconnect().await.expect("disconnect");
}

#[tokio::test]
async fn libsql_resolve_take() {
    let (engine, tmp) = libsql_init().await;
    let page_id = libsql_seed_page(&engine, &tmp, "resolve", "src-1").await;

    engine
        .add_takes_batch(
            page_id,
            &[TakeInput {
                page_id,
                row_num: Some(0),
                claim: "resolve-me".to_string(),
                kind: "take".to_string(),
                holder: "alice".to_string(),
                weight: 0.5,
                since_date: None,
                until_date: None,
                source: None,
                superseded_by: None,
                active: None,
            }],
        )
        .await
        .expect("add");

    engine
        .resolve_take(
            page_id as u64,
            0,
            &TakeResolution {
                page_id: page_id as u64,
                row_num: 0,
                quality: Some("high".to_string()),
                outcome: Some(true),
                evidence: Some("solid".to_string()),
                value: Some(100.0),
                unit: Some("pct".to_string()),
                by: Some("bob".to_string()),
            },
        )
        .await
        .expect("resolve");

    let takes = engine
        .get_takes_for_page(page_id)
        .await
        .expect("get");
    let t = &takes[0];
    assert_eq!(t.resolved_quality.as_deref(), Some("high"));
    assert_eq!(t.resolved_outcome, Some(true));
    assert_eq!(t.resolved_evidence.as_deref(), Some("solid"));
    assert!((t.resolved_value.unwrap() - 100.0).abs() < 1e-9);
    assert_eq!(t.resolved_unit.as_deref(), Some("pct"));
    assert_eq!(t.resolved_by.as_deref(), Some("bob"));
    assert!(t.resolved_at.is_some());
    engine.disconnect().await.expect("disconnect");
}

#[tokio::test]
async fn libsql_resolve_nonexistent_errors() {
    let (engine, tmp) = libsql_init().await;
    let page_id = libsql_seed_page(&engine, &tmp, "no-resolve", "src-1").await;

    let err = engine
        .resolve_take(
            page_id,
            999, // nonexistent row_num
            &TakeResolution {
                page_id,
                row_num: 999,
                quality: None,
                outcome: None,
                evidence: None,
                value: None,
                unit: None,
                by: None,
            },
        )
        .await
        .expect_err("should fail");
    let msg = format!("{err:?}");
    assert!(
        msg.contains("not_found") || msg.contains("Not Found"),
        "error should mention not_found: {msg}"
    );
    engine.disconnect().await.expect("disconnect");
}

#[tokio::test]
async fn libsql_get_takes_for_nonexistent_page_returns_empty() {
    let (engine, _tmp) = libsql_init().await;
    let takes = engine
        .get_takes_for_page(99999)
        .await
        .expect("get_takes");
    assert!(takes.is_empty());
    engine.disconnect().await.expect("disconnect");
}
