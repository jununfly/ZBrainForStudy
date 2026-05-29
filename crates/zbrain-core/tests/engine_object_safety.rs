//! Slice 3 — verifies the `BrainEngine` trait shape with a minimal in-memory
//! mock. Object-safety is the contract the postgres / libsql implementations
//! will rely on (so that operations / CLI can take `Arc<dyn BrainEngine>`).
//!
//! TDD red→green driver: write the assertions first, then carve the trait to
//! satisfy them. Coverage at this slice is intentionally narrow — lifecycle +
//! Page CRUD subset — wider methods land in later slices.

use std::sync::Arc;

use zbrain_core::engine::{
    BrainEngine, EngineConfig, EngineKind, GetPageOpts, InMemoryEngine, Page, PageFilters,
    PageInput,
};

fn page_input(title: &str, body: &str) -> PageInput {
    PageInput {
        page_type: "note".to_string(),
        title: title.to_string(),
        compiled_truth: body.to_string(),
        ..Default::default()
    }
}

fn boxed_engine() -> Arc<dyn BrainEngine> {
    Arc::new(InMemoryEngine::default())
}

/// Trait must be object-safe; if this compiles the assertion is satisfied.
#[test]
fn trait_is_object_safe() {
    let engine: Arc<dyn BrainEngine> = boxed_engine();
    assert_eq!(engine.kind(), EngineKind::InMemory);
}

#[tokio::test]
async fn lifecycle_connects_and_disconnects() {
    let engine = InMemoryEngine::default();
    engine
        .connect(&EngineConfig::default())
        .await
        .expect("connect ok");
    engine.init_schema().await.expect("init_schema ok");
    engine.disconnect().await.expect("disconnect ok");
}

#[tokio::test]
async fn put_then_get_page_round_trip() {
    let engine = InMemoryEngine::default();
    engine.connect(&EngineConfig::default()).await.unwrap();

    let stored: Page = engine
        .put_page("hello-world", None, &page_input("Hello", "compiled body"))
        .await
        .expect("put_page ok");

    assert_eq!(stored.slug, "hello-world");
    assert_eq!(stored.title, "Hello");
    assert_eq!(stored.compiled_truth, "compiled body");
    assert!(stored.id >= 1, "id must be assigned");

    let fetched = engine
        .get_page("hello-world", &GetPageOpts::default())
        .await
        .expect("get_page ok")
        .expect("page exists");
    assert_eq!(fetched.id, stored.id);
    assert_eq!(fetched.title, "Hello");
}

#[tokio::test]
async fn get_missing_page_returns_none() {
    let engine = InMemoryEngine::default();
    engine.connect(&EngineConfig::default()).await.unwrap();

    let missing = engine
        .get_page("nope", &GetPageOpts::default())
        .await
        .expect("get_page ok");
    assert!(missing.is_none());
}

#[tokio::test]
async fn list_pages_returns_inserted_in_insertion_order() {
    let engine = InMemoryEngine::default();
    engine.connect(&EngineConfig::default()).await.unwrap();

    engine
        .put_page("alpha", None, &page_input("A", "body-a"))
        .await
        .unwrap();
    engine
        .put_page("beta", None, &page_input("B", "body-b"))
        .await
        .unwrap();

    let listed = engine
        .list_pages(&PageFilters::default())
        .await
        .expect("list_pages ok");
    let slugs: Vec<&str> = listed.iter().map(|p| p.slug.as_str()).collect();
    assert_eq!(slugs, vec!["alpha", "beta"]);
}

#[tokio::test]
async fn delete_page_removes_row() {
    let engine = InMemoryEngine::default();
    engine.connect(&EngineConfig::default()).await.unwrap();
    engine
        .put_page("to-delete", None, &page_input("X", "y"))
        .await
        .unwrap();
    engine.delete_page("to-delete").await.expect("delete ok");
    let after = engine
        .get_page("to-delete", &GetPageOpts::default())
        .await
        .expect("get ok");
    assert!(after.is_none(), "row should be gone");
}

#[tokio::test]
async fn resolve_slugs_does_substring_match() {
    let engine = InMemoryEngine::default();
    engine.connect(&EngineConfig::default()).await.unwrap();
    for slug in ["alpha-one", "alpha-two", "beta-one"] {
        engine.put_page(slug, None, &page_input(slug, "body")).await.unwrap();
    }

    let mut hits = engine.resolve_slugs("alpha").await.expect("resolve ok");
    hits.sort();
    assert_eq!(hits, vec!["alpha-one".to_string(), "alpha-two".to_string()]);
}

#[tokio::test]
async fn put_page_upserts_existing_slug() {
    let engine = InMemoryEngine::default();
    engine.connect(&EngineConfig::default()).await.unwrap();
    let v1 = engine
        .put_page("doc", None, &page_input("v1", "old body"))
        .await
        .unwrap();
    let v2 = engine
        .put_page("doc", None, &page_input("v2", "new body"))
        .await
        .unwrap();
    assert_eq!(v1.id, v2.id, "upsert keeps the same id");

    let fetched = engine
        .get_page("doc", &GetPageOpts::default())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(fetched.title, "v2");
    assert_eq!(fetched.compiled_truth, "new body");
}
