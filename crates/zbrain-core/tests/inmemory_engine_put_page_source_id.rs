//! S6-T8 — `InMemoryEngine::put_page` `source_id` parameterisation tests.
//!
//! Companion to `libsql_engine_put_page_source_id.rs`; covers the in-memory
//! impl path at `engine.rs:498-560`:
//!   * line 506: `let source_id_norm = source_id.unwrap_or("default");`
//!   * line 522: `.find(|p| p.slug == slug && p.source_id == source_id_norm)`
//!
//! Test matrix (2 cases, `InMemory` side):
//!   * T5 — `Some("foo")` round-trips: row carries `source_id = "foo"`.
//!   * T6 — Same slug under two `source_ids` -> two independent rows
//!     (proves the compound-key match, not slug-only).
//!
//! NOTE: `get_page` / `delete_page` on `InMemory` still match slug-only (not yet
//! source-scoped); that gap is explicitly slated for S6-T9 — see the doc
//! comment at `engine.rs:517-519`. These tests deliberately avoid `get_page`
//! readback and instead use the `Page` returned by `put_page` itself, which
//! is the row authority in the `InMemory` impl.

use zbrain_core::engine::{BrainEngine, EngineConfig, InMemoryEngine, PageInput};

async fn init_inmemory() -> InMemoryEngine {
    let engine = InMemoryEngine::default();
    let cfg = EngineConfig {
        database_url: None,
        database_path: None,
    };
    engine.connect(&cfg).await.expect("connect");
    engine.init_schema().await.expect("init_schema");
    engine
}

fn note_input(title: &str, body: &str) -> PageInput {
    PageInput {
        page_type: "note".to_string(),
        title: title.to_string(),
        compiled_truth: body.to_string(),
        ..Default::default()
    }
}

// -- T5 — Some("foo") round-trip -----------------------------------------

#[tokio::test]
async fn inmemory_put_page_with_some_source_id_round_trips() {
    let engine = init_inmemory().await;
    let inserted = engine
        .put_page("alpha", Some("notion"), &note_input("Alpha", "body-1"))
        .await
        .expect("put_page");
    assert_eq!(
        inserted.source_id, "notion",
        "Some(\"notion\") must land verbatim in the in-memory row"
    );
    engine.disconnect().await.expect("disconnect");
}

// -- T6 — compound key under same slug yields two rows --------------------

#[tokio::test]
async fn inmemory_put_page_same_slug_different_source_ids_produces_two_rows() {
    let engine = init_inmemory().await;

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
        "InMemory compound match (slug, source_id) must NOT merge across sources"
    );
    assert_eq!(a.source_id, "src-a");
    assert_eq!(b.source_id, "src-b");
    assert_eq!(a.slug, "shared");
    assert_eq!(b.slug, "shared");

    // Re-put under src-a with new body — must in-place update row `a`, not row `b`.
    let a2 = engine
        .put_page(
            "shared",
            Some("src-a"),
            &note_input("Shared A v2", "body-a-v2"),
        )
        .await
        .expect("put_page src-a v2");
    assert_eq!(
        a2.id, a.id,
        "same (slug, source_id) must reuse existing row id"
    );
    assert_eq!(a2.title, "Shared A v2");

    engine.disconnect().await.expect("disconnect");
}
