//! Part12 1-1-2: `extract_atoms` cycle-phase integration tests (libsql).
//!
//! Covers the LLM → DB reconcile path end-to-end against a real libsql
//! backend:
//! - brain page with `compiled_truth` ≥ 500 chars + non-null `content_hash`
//!   is discovered, Haiku (mocked) returns atoms, and each is written as a
//!   `type: "atom"` page with correct frontmatter (`atom_type`,
//!   `source_hash`, `source_slug`).
//! - re-run idempotency: the NOT EXISTS subquery suppresses the already
//!   atomised page → 0 new atom pages.
//! - dry-run: atoms are counted but zero atom pages are written.
//! - empty brain: no chat call, zero work.

use std::sync::{Mutex, OnceLock};
use tempfile::NamedTempFile;
use zbrain_core::ai::chat::MockChatProvider;
use zbrain_core::autopilot::phases::extract_atoms::{run_extract_atoms, ExtractAtomsOpts};
use zbrain_core::engine::{BrainEngine, EngineConfig, GetPageOpts, PageFilters, PageInput};
use zbrain_core::libsql::LibsqlEngine;
use zbrain_core::types::PageType;

/// Serialize all libsql FFI access in this binary. The `libsql` native
/// library is not safe to drive from multiple OS threads concurrently on
/// Windows (parallel `cargo test` threads crash with STATUS_ACCESS_VIOLATION
/// 0xc0000005). Each test grabs this guard for its whole body so the suite
/// stays green under default parallelism; serial runs are unaffected.
static LIBSQL_TEST_LOCK: OnceLock<Mutex<()>> = OnceLock::new();
fn libsql_test_guard() -> std::sync::MutexGuard<'static, ()> {
    LIBSQL_TEST_LOCK
        .get_or_init(|| Mutex::new(()))
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// 64-char hash; discovery matches `substring(content_hash, 1, 16)` against the
/// atom `source_hash`, so a stable long hash keeps the test deterministic.
const HASH: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";

const SRC: &str = "default";
const PAGE_SLUG: &str = "jane-interview";

const VALID_ATOMS: &str = r#"[
  {"title":"Jane ships weekly","atom_type":"insight","body":"Jane commits to a weekly ship cadence.","source_quote":"we ship every week","lesson":"consistency beats intensity","virality_score":72,"emotional_register":"practical"},
  {"title":"Jane hates meetings","atom_type":"strategy","body":"Jane prefers async over sync.","source_quote":"async over meetings","lesson":"protect deep work"}
]"#;

fn extractable_page(title: &str) -> PageInput {
    // ≥500 chars so `length(compiled_truth) >= 500` passes discovery.
    let body = format!(
        "Long interview transcript.\n\n{}\n\nJane describes her weekly cadence and why async wins.",
        "Lorem ipsum dolor sit amet, consectetur adipiscing elit. ".repeat(12)
    );
    PageInput {
        page_type: PageType::from("article"),
        title: title.to_string(),
        compiled_truth: body,
        content_hash: Some(HASH.to_string()),
        ..Default::default()
    }
}

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

async fn list_atom_slugs(engine: &LibsqlEngine) -> Vec<String> {
    let pages = engine
        .list_pages(&PageFilters {
            page_type: Some(PageType::from("atom")),
            source_id: Some(SRC.to_string()),
            ..Default::default()
        })
        .await
        .expect("list atom pages");
    pages.iter().map(|p| p.slug.clone()).collect()
}

// ── discovery → LLM → atom page write ──────────────────────────────────────

#[tokio::test]
async fn extract_atoms_writes_atom_pages_from_brain_page() {
    let _guard = libsql_test_guard();
    let (engine, _tmp) = init_clean_engine().await;
    engine
        .put_page(PAGE_SLUG, Some(SRC), &extractable_page("Jane Interview"))
        .await
        .expect("put_page");

    let chat = MockChatProvider::new(VALID_ATOMS);
    let r = run_extract_atoms(
        &engine,
        &chat,
        &ExtractAtomsOpts {
            source_id: Some(SRC.to_string()),
            ..Default::default()
        },
    )
    .await
    .expect("run_extract_atoms");

    assert_eq!(r.pages_total, 1, "one extractable page discovered");
    assert_eq!(r.pages_processed, 1);
    assert_eq!(r.transcripts_total, 0, "transcript path not ported (G62)");
    assert_eq!(r.atoms_extracted, 2, "both atoms parsed and written");

    let slugs = list_atom_slugs(&engine).await;
    assert_eq!(slugs.len(), 2, "two atom pages persisted");

    // Read one atom page back and verify frontmatter wiring.
    let slug = slugs
        .iter()
        .find(|s| s.contains("jane-ships-weekly"))
        .expect("atom page for 'Jane ships weekly' exists");
    let page = engine
        .get_page(slug, &GetPageOpts {
            source_id: Some(SRC.to_string()),
            include_deleted: false,
        })
        .await
        .expect("get_page")
        .expect("atom page present");

    assert_eq!(page.page_type, "atom");
    assert_eq!(page.title, "Jane ships weekly");
    assert_eq!(page.compiled_truth, "Jane commits to a weekly ship cadence.");

    let fm = page.frontmatter.as_object().expect("frontmatter object");
    assert_eq!(fm.get("atom_type").and_then(|v| v.as_str()), Some("insight"));
    assert_eq!(
        fm.get("source_hash").and_then(|v| v.as_str()),
        Some(&HASH[..16]),
        "source_hash = first 16 chars of the page content_hash"
    );
    assert_eq!(
        fm.get("source_slug").and_then(|v| v.as_str()),
        Some(PAGE_SLUG),
        "source_slug points back at the originating brain page"
    );
    assert_eq!(
        fm.get("source_quote").and_then(|v| v.as_str()),
        Some("we ship every week")
    );

    engine.disconnect().await.expect("disconnect");
}

// ── re-run idempotency (NOT EXISTS suppresses atomised page) ───────────────

#[tokio::test]
async fn extract_atoms_rerun_is_idempotent() {
    let _guard = libsql_test_guard();
    let (engine, _tmp) = init_clean_engine().await;
    engine
        .put_page(PAGE_SLUG, Some(SRC), &extractable_page("Jane Interview"))
        .await
        .expect("put_page");

    let chat = MockChatProvider::new(VALID_ATOMS);
    let r1 = run_extract_atoms(
        &engine,
        &chat,
        &ExtractAtomsOpts {
            source_id: Some(SRC.to_string()),
            ..Default::default()
        },
    )
    .await
    .expect("first run");
    assert_eq!(r1.atoms_extracted, 2);

    let r2 = run_extract_atoms(
        &engine,
        &chat,
        &ExtractAtomsOpts {
            source_id: Some(SRC.to_string()),
            ..Default::default()
        },
    )
    .await
    .expect("second run");

    assert_eq!(r2.pages_total, 0, "already-atomised page is suppressed by NOT EXISTS");
    assert_eq!(r2.atoms_extracted, 0, "second run writes no new atoms");

    // Atom page count stays stable.
    let slugs = list_atom_slugs(&engine).await;
    assert_eq!(slugs.len(), 2, "no duplicate atom pages created");

    engine.disconnect().await.expect("disconnect");
}

// ── dry-run writes nothing ─────────────────────────────────────────────────

#[tokio::test]
async fn extract_atoms_dry_run_writes_nothing() {
    let _guard = libsql_test_guard();
    let (engine, _tmp) = init_clean_engine().await;
    engine
        .put_page(PAGE_SLUG, Some(SRC), &extractable_page("Jane Interview"))
        .await
        .expect("put_page");

    let chat = MockChatProvider::new(VALID_ATOMS);
    let r = run_extract_atoms(
        &engine,
        &chat,
        &ExtractAtomsOpts {
            dry_run: true,
            source_id: Some(SRC.to_string()),
            ..Default::default()
        },
    )
    .await
    .expect("dry run");

    assert_eq!(r.pages_total, 1, "page still discovered and counted");
    assert_eq!(r.pages_processed, 1);
    assert_eq!(r.atoms_extracted, 2, "atoms parsed and counted");

    let slugs = list_atom_slugs(&engine).await;
    assert!(slugs.is_empty(), "dry run must not write atom pages");

    engine.disconnect().await.expect("disconnect");
}

// ── empty brain ────────────────────────────────────────────────────────────

#[tokio::test]
async fn extract_atoms_empty_brain_is_ok() {
    let _guard = libsql_test_guard();
    let (engine, _tmp) = init_clean_engine().await;

    let chat = MockChatProvider::new(VALID_ATOMS);
    let r = run_extract_atoms(
        &engine,
        &chat,
        &ExtractAtomsOpts {
            source_id: Some(SRC.to_string()),
            ..Default::default()
        },
    )
    .await
    .expect("run_extract_atoms");

    assert_eq!(r.pages_total, 0);
    assert_eq!(r.atoms_extracted, 0);

    let slugs = list_atom_slugs(&engine).await;
    assert!(slugs.is_empty());

    engine.disconnect().await.expect("disconnect");
}
