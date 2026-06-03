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
//! NOTE: `get_page` / `delete_page` on `InMemory` are now source-scoped. These
//! tests still use the `Page` returned by `put_page` itself as the row authority
//! for the put-path assertions.

use zbrain_core::engine::{
    BrainEngine, EngineConfig, GetPageOpts, InMemoryEngine, PageFilters, PageInput, PageSort,
};

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

// -- S6a follow-up — get_page source scope --------------------------------

#[tokio::test]
async fn inmemory_get_page_respects_source_id_filter_for_same_slug() {
    let engine = init_inmemory().await;

    let default_page = engine
        .put_page(
            "shared-get",
            None,
            &note_input("Default title", "default-body"),
        )
        .await
        .expect("put default source");
    let alt_page = engine
        .put_page(
            "shared-get",
            Some("alt-source"),
            &note_input("Alt title", "alt-body"),
        )
        .await
        .expect("put alt source");

    let default_lookup = engine
        .get_page("shared-get", &GetPageOpts::default())
        .await
        .expect("get default source")
        .expect("default page exists");
    assert_eq!(
        default_lookup.id, default_page.id,
        "GetPageOpts::default() must read only the default source"
    );

    let alt_lookup = engine
        .get_page(
            "shared-get",
            &GetPageOpts {
                source_id: Some("alt-source".to_string()),
                include_deleted: false,
            },
        )
        .await
        .expect("get alt source")
        .expect("alt page exists");
    assert_eq!(
        alt_lookup.id, alt_page.id,
        "explicit source_id must read the matching source row"
    );

    let missing_source_lookup = engine
        .get_page(
            "shared-get",
            &GetPageOpts {
                source_id: Some("missing-source".to_string()),
                include_deleted: false,
            },
        )
        .await
        .expect("get missing source");
    assert!(
        missing_source_lookup.is_none(),
        "get_page must not fall back across sources when source_id is explicit"
    );

    engine.disconnect().await.expect("disconnect");
}

// -- S6a follow-up — delete_page source scope -----------------------------

#[tokio::test]
async fn inmemory_delete_page_with_source_id_only_removes_matching_source_row() {
    let engine = init_inmemory().await;

    let default_page = engine
        .put_page(
            "shared-delete",
            None,
            &note_input("Default title", "default-body"),
        )
        .await
        .expect("put default source");
    let alt_page = engine
        .put_page(
            "shared-delete",
            Some("alt-source"),
            &note_input("Alt title", "alt-body"),
        )
        .await
        .expect("put alt source");

    engine
        .delete_page("shared-delete", Some("alt-source"))
        .await
        .expect("delete alt source");

    let default_lookup = engine
        .get_page("shared-delete", &GetPageOpts::default())
        .await
        .expect("get default source after delete")
        .expect("default page must remain");
    assert_eq!(
        default_lookup.id, default_page.id,
        "source-scoped delete must leave same-slug default row intact"
    );

    let alt_lookup = engine
        .get_page(
            "shared-delete",
            &GetPageOpts {
                source_id: Some("alt-source".to_string()),
                include_deleted: false,
            },
        )
        .await
        .expect("get alt source after delete");
    assert!(
        alt_lookup.is_none(),
        "delete_page with explicit source_id must remove the matching source row"
    );
    assert_ne!(default_page.id, alt_page.id);

    engine.disconnect().await.expect("disconnect");
}

// -- S6a follow-up — list_pages source scope -------------------------------

#[tokio::test]
async fn inmemory_list_pages_filters_by_source_id() {
    let engine = init_inmemory().await;

    engine
        .put_page(
            "source-default",
            None,
            &note_input("Default", "default-body"),
        )
        .await
        .expect("put default source");
    engine
        .put_page(
            "source-alpha",
            Some("alpha"),
            &note_input("Alpha", "alpha-body"),
        )
        .await
        .expect("put alpha source");
    engine
        .put_page(
            "source-beta",
            Some("beta"),
            &note_input("Beta", "beta-body"),
        )
        .await
        .expect("put beta source");

    let pages = engine
        .list_pages(&PageFilters {
            source_id: Some("alpha".to_string()),
            sort: Some(PageSort::Slug),
            ..Default::default()
        })
        .await
        .expect("list_pages source_id=alpha");

    let slugs: Vec<&str> = pages.iter().map(|p| p.slug.as_str()).collect();
    assert_eq!(slugs, vec!["source-alpha"]);
    assert!(pages.iter().all(|p| p.source_id == "alpha"));

    engine.disconnect().await.expect("disconnect");
}

#[tokio::test]
async fn inmemory_list_pages_source_ids_take_precedence_over_source_id() {
    let engine = init_inmemory().await;

    engine
        .put_page(
            "source-default",
            None,
            &note_input("Default", "default-body"),
        )
        .await
        .expect("put default source");
    engine
        .put_page(
            "source-alpha",
            Some("alpha"),
            &note_input("Alpha", "alpha-body"),
        )
        .await
        .expect("put alpha source");
    engine
        .put_page(
            "source-beta",
            Some("beta"),
            &note_input("Beta", "beta-body"),
        )
        .await
        .expect("put beta source");

    let pages = engine
        .list_pages(&PageFilters {
            source_id: Some("alpha".to_string()),
            source_ids: Some(vec!["default".to_string(), "beta".to_string()]),
            sort: Some(PageSort::Slug),
            ..Default::default()
        })
        .await
        .expect("list_pages source_ids precedence");

    let slugs: Vec<&str> = pages.iter().map(|p| p.slug.as_str()).collect();
    assert_eq!(slugs, vec!["source-beta", "source-default"]);
    assert!(
        pages
            .iter()
            .all(|p| p.source_id == "default" || p.source_id == "beta"),
        "source_ids must take precedence over source_id"
    );

    engine.disconnect().await.expect("disconnect");
}

#[tokio::test]
async fn inmemory_list_pages_empty_source_ids_falls_back_to_source_id() {
    let engine = init_inmemory().await;

    engine
        .put_page(
            "source-default",
            None,
            &note_input("Default", "default-body"),
        )
        .await
        .expect("put default source");
    engine
        .put_page(
            "source-alpha",
            Some("alpha"),
            &note_input("Alpha", "alpha-body"),
        )
        .await
        .expect("put alpha source");
    engine
        .put_page(
            "source-beta",
            Some("beta"),
            &note_input("Beta", "beta-body"),
        )
        .await
        .expect("put beta source");

    let pages = engine
        .list_pages(&PageFilters {
            source_id: Some("alpha".to_string()),
            source_ids: Some(vec![]),
            sort: Some(PageSort::Slug),
            ..Default::default()
        })
        .await
        .expect("list_pages empty source_ids fallback");

    let slugs: Vec<&str> = pages.iter().map(|p| p.slug.as_str()).collect();
    assert_eq!(slugs, vec!["source-alpha"]);
    assert!(pages.iter().all(|p| p.source_id == "alpha"));

    engine.disconnect().await.expect("disconnect");
}
