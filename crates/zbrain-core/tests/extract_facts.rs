//! Part12 1-1-1: `extract_facts` cycle-phase integration tests (libsql).
//!
//! Covers the fence → DB reconcile path end-to-end against a real libsql
//! backend:
//! - page with a `## Facts` fence → facts inserted + read-back columns
//!   (`row_num`, `source_markdown_slug`, valid_from precedence)
//! - page without a fence → zero facts, no warnings
//! - empty brain → Ok envelope, guard untouched
//! - re-run idempotency → wipe-and-reinsert keeps the row count stable
//! - legacy v0.31 guard → rows with `row_num IS NULL AND entity_slug NOT
//!   NULL` block the destructive pass

use tempfile::NamedTempFile;
use zbrain_core::autopilot::phases::extract_facts::{run_extract_facts, ExtractFactsOpts};
use zbrain_core::engine::{BrainEngine, EngineConfig, PageInput};
use zbrain_core::libsql::LibsqlEngine;
use zbrain_core::types::{FactListOpts, NewFact};

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

fn page_with_fence(title: &str) -> PageInput {
    let body = r#"some intro

## Facts

<!--- zbrain:facts:begin -->
| # | claim | kind | confidence | visibility | notability | valid_from | valid_until | source | context |
|---|---|---|---|---|---|---|---|---|---|
| 1 | Founded Acme in 2017 | fact | 1.0 | world | high | 2017-01-01 |  | linkedin |  |
| 2 | Prefers async over meetings | preference | 0.85 | private | medium | 2026-04-29 |  | OH |  |
<!--- zbrain:facts:end -->

more content"#;
    PageInput {
        page_type: "note".to_string(),
        title: title.to_string(),
        compiled_truth: body.to_string(),
        ..Default::default()
    }
}

fn page_without_fence(title: &str) -> PageInput {
    PageInput {
        page_type: "note".to_string(),
        title: title.to_string(),
        compiled_truth: "plain body, no facts fence".to_string(),
        ..Default::default()
    }
}

// ── fence → DB reconcile ──────────────────────────────────────────────────

#[tokio::test]
async fn extract_facts_reconciles_fence_into_db() {
    let (engine, _tmp) = init_clean_engine().await;
    engine
        .put_page("alice", None, &page_with_fence("Alice"))
        .await
        .expect("put_page");

    let r = run_extract_facts(&engine, &ExtractFactsOpts::default())
        .await
        .expect("run_extract_facts");

    assert!(!r.guard_triggered, "guard must not trigger on a clean brain");
    assert!(r.warnings.is_empty(), "no warnings expected: {:?}", r.warnings);
    assert_eq!(r.pages_scanned, 1);
    assert_eq!(r.pages_with_facts, 1);
    assert_eq!(r.facts_inserted, 2);
    assert_eq!(r.facts_deleted, 0);

    let rows = engine
        .list_facts_by_entity("default", "alice", &FactListOpts::default())
        .await
        .expect("list_facts_by_entity");
    assert_eq!(rows.len(), 2);

    let founded = rows
        .iter()
        .find(|f| f.fact == "Founded Acme in 2017")
        .expect("fence row 1 present");
    assert_eq!(founded.row_num, Some(1));
    assert_eq!(founded.source_markdown_slug.as_deref(), Some("alice"));
    assert_eq!(founded.source, "linkedin");
    assert!(founded
        .valid_from
        .as_deref()
        .unwrap_or_default()
        .starts_with("2017-01-01"));

    let prefers = rows
        .iter()
        .find(|f| f.fact == "Prefers async over meetings")
        .expect("fence row 2 present");
    assert_eq!(prefers.row_num, Some(2));
    assert_eq!(prefers.source_markdown_slug.as_deref(), Some("alice"));

    engine.disconnect().await.expect("disconnect");
}

// ── page without fence ────────────────────────────────────────────────────

#[tokio::test]
async fn extract_facts_page_without_fence_yields_zero() {
    let (engine, _tmp) = init_clean_engine().await;
    engine
        .put_page("bob", None, &page_without_fence("Bob"))
        .await
        .expect("put_page");

    let r = run_extract_facts(&engine, &ExtractFactsOpts::default())
        .await
        .expect("run_extract_facts");

    assert_eq!(r.pages_scanned, 1);
    assert_eq!(r.pages_with_facts, 0);
    assert_eq!(r.facts_inserted, 0);
    assert!(r.warnings.is_empty());

    let rows = engine
        .list_facts_by_entity("default", "bob", &FactListOpts::default())
        .await
        .expect("list_facts_by_entity");
    assert!(rows.is_empty());

    engine.disconnect().await.expect("disconnect");
}

// ── empty brain ───────────────────────────────────────────────────────────

#[tokio::test]
async fn extract_facts_empty_brain_is_ok() {
    let (engine, _tmp) = init_clean_engine().await;

    let r = run_extract_facts(&engine, &ExtractFactsOpts::default())
        .await
        .expect("run_extract_facts");

    assert_eq!(r.pages_scanned, 0);
    assert_eq!(r.facts_inserted, 0);
    assert!(!r.guard_triggered);
    assert!(r.warnings.is_empty());

    engine.disconnect().await.expect("disconnect");
}

// ── re-run idempotency (wipe-and-reinsert) ────────────────────────────────

#[tokio::test]
async fn extract_facts_rerun_is_idempotent() {
    let (engine, _tmp) = init_clean_engine().await;
    engine
        .put_page("alice", None, &page_with_fence("Alice"))
        .await
        .expect("put_page");

    let r1 = run_extract_facts(&engine, &ExtractFactsOpts::default())
        .await
        .expect("first run");
    assert_eq!(r1.facts_inserted, 2);
    assert_eq!(r1.facts_deleted, 0);

    let r2 = run_extract_facts(&engine, &ExtractFactsOpts::default())
        .await
        .expect("second run");
    assert_eq!(r2.facts_deleted, 2, "second pass wipes the fence-scoped rows");
    assert_eq!(r2.facts_inserted, 2, "…then reinserts them");

    let rows = engine
        .list_facts_by_entity("default", "alice", &FactListOpts::default())
        .await
        .expect("list_facts_by_entity");
    assert_eq!(rows.len(), 2, "row count stays stable across reruns");

    engine.disconnect().await.expect("disconnect");
}

// ── dry-run ───────────────────────────────────────────────────────────────

#[tokio::test]
async fn extract_facts_dry_run_writes_nothing() {
    let (engine, _tmp) = init_clean_engine().await;
    engine
        .put_page("alice", None, &page_with_fence("Alice"))
        .await
        .expect("put_page");

    let r = run_extract_facts(
        &engine,
        &ExtractFactsOpts {
            dry_run: true,
            ..Default::default()
        },
    )
    .await
    .expect("dry run");

    assert_eq!(r.pages_with_facts, 1, "fence is still parsed and counted");
    assert_eq!(r.facts_inserted, 0);
    assert_eq!(r.facts_deleted, 0);

    let rows = engine
        .list_facts_by_entity("default", "alice", &FactListOpts::default())
        .await
        .expect("list_facts_by_entity");
    assert!(rows.is_empty(), "dry run must not write facts");

    engine.disconnect().await.expect("disconnect");
}

// ── legacy v0.31 guard ────────────────────────────────────────────────────

#[tokio::test]
async fn extract_facts_legacy_guard_blocks_reconcile() {
    let (engine, _tmp) = init_clean_engine().await;
    engine
        .put_page("alice", None, &page_with_fence("Alice"))
        .await
        .expect("put_page");

    // Seed a legacy v0.31-shaped row: entity_slug set, row_num NULL.
    let legacy = NewFact {
        fact: "legacy row without fence coords".to_string(),
        kind: None,
        entity_slug: Some("alice".to_string()),
        visibility: None,
        context: None,
        valid_from: None,
        valid_until: None,
        source: "test".to_string(),
        source_session: None,
        confidence: None,
        notability: None,
        claim_metric: None,
        claim_value: None,
        claim_unit: None,
        claim_period: None,
        event_type: None,
        row_num: None,
        source_markdown_slug: None,
    };
    engine
        .insert_fact("default", "alice", &legacy)
        .await
        .expect("insert legacy fact");

    let r = run_extract_facts(&engine, &ExtractFactsOpts::default())
        .await
        .expect("run_extract_facts");

    assert!(r.guard_triggered, "legacy rows must trip the guard");
    assert_eq!(r.legacy_rows_pending, 1);
    assert!(!r.warnings.is_empty());
    assert_eq!(r.pages_scanned, 0, "guard aborts before the page walk");
    assert_eq!(r.facts_inserted, 0);

    // The legacy row must survive untouched.
    let rows = engine
        .list_facts_by_entity("default", "alice", &FactListOpts::default())
        .await
        .expect("list_facts_by_entity");
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].fact, "legacy row without fence coords");

    engine.disconnect().await.expect("disconnect");
}
