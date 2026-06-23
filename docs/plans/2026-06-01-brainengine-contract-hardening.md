# BrainEngine Contract Hardening Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** Harden the `BrainEngine` Slice 6a S6 page-method contract by removing trait-level unsupported fallbacks and making every backend, including `InMemoryEngine`, implement the full method surface explicitly.

**Architecture:** Keep production backend behavior unchanged for `LibsqlEngine` and `PostgresEngine`, both of which already implement the full S6 surface. Add explicit `InMemoryEngine` test-double implementations under TDD, using the same observable contracts as the libsql/postgres page-method tests where the in-memory store has enough state, and conservative no-op/empty semantics where persistent link/tag side tables do not exist. Only after `InMemoryEngine` covers the full surface, remove the default `Err(Error::unsupported("pending slice 6a"))` implementations from `BrainEngine` so future backend drift fails at compile time.

**Tech Stack:** Rust, `async_trait`, `serde_json::Value`, `std::sync::Mutex`, `HashMap`, `HashSet`, Cargo workspace tests, rustfmt, clippy.

---

## Non-negotiable constraints

- TDD rule: **NO PRODUCTION CODE WITHOUT A FAILING TEST FIRST**.
- Worktree root:
  - `/Users/bilibili/Documents/workspace/jununfly/zbrain-rust`
- Cargo manifest:
  - `/Users/bilibili/Documents/workspace/jununfly/zbrain-rust/Cargo.toml`
- Do not amend prior implementation commits. If committing this work, create a new commit.
- Do not silently treat PG integration skips as PG infra completion:
  - `skipping: ZBRAIN_TEST_PG_URL unset` is a skip, not a pass for PG infra readiness.

## Current known backend coverage

S6 methods to harden:

```rust
find_duplicate_page
soft_delete_page
restore_page
purge_deleted_pages
add_tag
remove_tag
get_tags
refresh_page_body
update_page_contextual_retrieval_state
get_all_slugs
list_all_page_refs
find_orphan_pages
get_page_timestamps
get_effective_dates
get_salience_scores
```

Known status before this plan:

- `LibsqlEngine`: 15/15 implemented.
- `PostgresEngine`: 15/15 implemented.
- `InMemoryEngine`: 2/15 implemented:
  - `find_duplicate_page`
  - `soft_delete_page`

## Files

Primary production file:

- Modify: `/Users/bilibili/Documents/workspace/jununfly/zbrain-rust/crates/zbrain-core/src/engine.rs`

Existing behavior-reference tests:

- Read/reference: `/Users/bilibili/Documents/workspace/jununfly/zbrain-rust/crates/zbrain-core/tests/page_methods_restore_page.rs`
- Read/reference: `/Users/bilibili/Documents/workspace/jununfly/zbrain-rust/crates/zbrain-core/tests/page_methods_purge_deleted_pages.rs`
- Read/reference: `/Users/bilibili/Documents/workspace/jununfly/zbrain-rust/crates/zbrain-core/tests/libsql_engine_tag_crud.rs`
- Read/reference: `/Users/bilibili/Documents/workspace/jununfly/zbrain-rust/crates/zbrain-core/tests/page_methods_refresh_page_body.rs`
- Read/reference: `/Users/bilibili/Documents/workspace/jununfly/zbrain-rust/crates/zbrain-core/tests/page_methods_update_cr_state.rs`
- Read/reference: `/Users/bilibili/Documents/workspace/jununfly/zbrain-rust/crates/zbrain-core/tests/page_methods_get_all_slugs.rs`
- Read/reference: `/Users/bilibili/Documents/workspace/jununfly/zbrain-rust/crates/zbrain-core/tests/page_methods_list_all_page_refs.rs`
- Read/reference: `/Users/bilibili/Documents/workspace/jununfly/zbrain-rust/crates/zbrain-core/tests/page_methods_find_orphan_pages.rs`
- Read/reference: `/Users/bilibili/Documents/workspace/jununfly/zbrain-rust/crates/zbrain-core/tests/page_methods_get_page_timestamps.rs`
- Read/reference: `/Users/bilibili/Documents/workspace/jununfly/zbrain-rust/crates/zbrain-core/tests/page_methods_get_effective_dates.rs`
- Read/reference: `/Users/bilibili/Documents/workspace/jununfly/zbrain-rust/crates/zbrain-core/tests/page_methods_get_salience_scores.rs`

New contract tests:

- Create: `/Users/bilibili/Documents/workspace/jununfly/zbrain-rust/crates/zbrain-core/tests/in_memory_engine_contract.rs`

Cleanup tests/comments:

- Modify: `/Users/bilibili/Documents/workspace/jununfly/zbrain-rust/crates/zbrain-core/tests/page_methods_list_all_page_refs.rs`
- Modify: `/Users/bilibili/Documents/workspace/jununfly/zbrain-rust/crates/zbrain-core/tests/page_methods_get_page_timestamps.rs`
- Modify: `/Users/bilibili/Documents/workspace/jununfly/zbrain-rust/crates/zbrain-core/tests/page_methods_get_effective_dates.rs`

---

## Task 1: Add InMemory lifecycle RED tests

**Files:**

- Create: `/Users/bilibili/Documents/workspace/jununfly/zbrain-rust/crates/zbrain-core/tests/in_memory_engine_contract.rs`
- Modify later: `/Users/bilibili/Documents/workspace/jununfly/zbrain-rust/crates/zbrain-core/src/engine.rs`

**Step 1: Write the failing lifecycle tests**

Create `in_memory_engine_contract.rs` with lifecycle tests for `restore_page` and `purge_deleted_pages`.

Use imports shaped like:

```rust
use std::collections::HashSet;

use serde_json::json;
use zbrain_core::{BrainEngine, InMemoryEngine, PageInput, PurgeResult};
```

Use local helpers:

```rust
fn page_input(slug: &str, source_id: &str) -> PageInput {
    PageInput {
        source_id: source_id.to_string(),
        slug: slug.to_string(),
        title: slug.to_string(),
        domain: None,
        frontmatter: json!({}),
        body: format!("body for {slug}"),
        compiled_truth: format!("truth for {slug}"),
        timeline: json!([]),
        metadata: json!({}),
        content_hash: Some(format!("hash-{slug}")),
        effective_date: None,
        emotional_weight: None,
        salience_override: None,
        contextual_retrieval_mode: None,
        corpus_generation: None,
    }
}

fn assert_purge(actual: &PurgeResult, expected_sorted: &[&str]) {
    let mut got = actual.slugs.clone();
    got.sort();
    let want: Vec<String> = expected_sorted.iter().map(ToString::to_string).collect();
    assert_eq!(got, want);
    assert_eq!(actual.count, expected_sorted.len() as u64);
}
```

Add tests:

```rust
#[tokio::test]
async fn in_memory_restore_page_restores_soft_deleted_exact_source() {
    let engine = InMemoryEngine::default();
    engine.upsert_page(page_input("to-restore", "src-1")).await.unwrap();
    assert_eq!(engine.soft_delete_page("to-restore", Some("src-1")).await.unwrap(), Some("to-restore".to_string()));

    let restored = engine.restore_page("to-restore", Some("src-1")).await.unwrap();

    assert!(restored);
    let page = engine.get_page("to-restore", None, Some("src-1")).await.unwrap().unwrap();
    assert!(page.deleted_at.is_none());
}

#[tokio::test]
async fn in_memory_restore_page_returns_false_for_live_missing_or_wrong_source() {
    let engine = InMemoryEngine::default();
    engine.upsert_page(page_input("live", "src-1")).await.unwrap();
    engine.upsert_page(page_input("deleted", "src-1")).await.unwrap();
    engine.soft_delete_page("deleted", Some("src-1")).await.unwrap();

    assert!(!engine.restore_page("live", Some("src-1")).await.unwrap());
    assert!(!engine.restore_page("missing", Some("src-1")).await.unwrap());
    assert!(!engine.restore_page("deleted", Some("src-2")).await.unwrap());
}

#[tokio::test]
async fn in_memory_purge_deleted_pages_removes_deleted_rows_and_keeps_live_rows() {
    let engine = InMemoryEngine::default();
    engine.upsert_page(page_input("live", "src-1")).await.unwrap();
    engine.upsert_page(page_input("deleted-a", "src-1")).await.unwrap();
    engine.upsert_page(page_input("deleted-b", "src-2")).await.unwrap();
    engine.soft_delete_page("deleted-a", Some("src-1")).await.unwrap();
    engine.soft_delete_page("deleted-b", Some("src-2")).await.unwrap();

    let result = engine.purge_deleted_pages(0).await.unwrap();

    assert_purge(&result, &["deleted-a", "deleted-b"]);
    assert!(engine.get_page("live", None, Some("src-1")).await.unwrap().is_some());
    assert!(engine.get_page("deleted-a", None, Some("src-1")).await.unwrap().is_none());
    assert!(engine.get_page("deleted-b", None, Some("src-2")).await.unwrap().is_none());
}
```

**Step 2: Run lifecycle tests to verify RED**

Run:

```bash
cargo test --manifest-path "/Users/bilibili/Documents/workspace/jununfly/zbrain-rust/Cargo.toml" -p zbrain-core --test in_memory_engine_contract in_memory_restore
cargo test --manifest-path "/Users/bilibili/Documents/workspace/jununfly/zbrain-rust/Cargo.toml" -p zbrain-core --test in_memory_engine_contract in_memory_purge
```

Expected: fail because `InMemoryEngine` currently inherits default trait fallback for `restore_page` / `purge_deleted_pages`, returning `Unsupported("pending slice 6a")`.

**Step 3: Implement minimal lifecycle code**

Modify `engine.rs` inside `impl BrainEngine for InMemoryEngine`:

```rust
async fn restore_page(&self, slug: &str, source_id: Option<&str>) -> crate::Result<bool> {
    let mut store = self
        .store
        .lock()
        .expect("InMemoryEngine store mutex poisoned");
    let Some(page) = store.iter_mut().find(|p| {
        p.slug == slug
            && p.deleted_at.is_some()
            && source_id.is_none_or(|source_id| p.source_id == source_id)
    }) else {
        return Ok(false);
    };

    page.deleted_at = None;
    page.updated_at = current_utc_iso8601();
    Ok(true)
}

async fn purge_deleted_pages(&self, _older_than_hours: u32) -> crate::Result<PurgeResult> {
    let mut store = self
        .store
        .lock()
        .expect("InMemoryEngine store mutex poisoned");
    let mut slugs = Vec::new();

    store.retain(|page| {
        if page.deleted_at.is_some() {
            slugs.push(page.slug.clone());
            false
        } else {
            true
        }
    });

    Ok(PurgeResult {
        count: slugs.len() as u64,
        slugs,
    })
}
```

Note: in-memory currently only stores current rows with runtime-generated timestamps. Without a controllable clock, `older_than_hours` cannot be meaningfully exercised. For the test double, `0` purges all deleted rows; future clock injection can harden age filtering separately if needed.

**Step 4: Run lifecycle tests to verify GREEN**

Run:

```bash
cargo test --manifest-path "/Users/bilibili/Documents/workspace/jununfly/zbrain-rust/Cargo.toml" -p zbrain-core --test in_memory_engine_contract in_memory_restore
cargo test --manifest-path "/Users/bilibili/Documents/workspace/jununfly/zbrain-rust/Cargo.toml" -p zbrain-core --test in_memory_engine_contract in_memory_purge
```

Expected: PASS.

**Step 5: Commit optional checkpoint**

If keeping frequent commits:

```bash
git add crates/zbrain-core/src/engine.rs crates/zbrain-core/tests/in_memory_engine_contract.rs
git commit -m "test(core): cover in-memory page lifecycle contract"
```

---

## Task 2: Add InMemory tag RED tests

**Files:**

- Modify: `/Users/bilibili/Documents/workspace/jununfly/zbrain-rust/crates/zbrain-core/tests/in_memory_engine_contract.rs`
- Modify later: `/Users/bilibili/Documents/workspace/jununfly/zbrain-rust/crates/zbrain-core/src/engine.rs`

**Step 1: Write failing tag tests**

Append:

```rust
#[tokio::test]
async fn in_memory_tags_are_idempotent_sorted_and_source_scoped() {
    let engine = InMemoryEngine::default();
    engine.upsert_page(page_input("tagged", "src-1")).await.unwrap();
    engine.upsert_page(page_input("tagged", "src-2")).await.unwrap();

    engine.add_tag("tagged", "z-tag", Some("src-1")).await.unwrap();
    engine.add_tag("tagged", "a-tag", Some("src-1")).await.unwrap();
    engine.add_tag("tagged", "a-tag", Some("src-1")).await.unwrap();
    engine.add_tag("tagged", "src-2-tag", Some("src-2")).await.unwrap();

    assert_eq!(
        engine.get_tags("tagged", Some("src-1")).await.unwrap(),
        vec!["a-tag".to_string(), "z-tag".to_string()]
    );
    assert_eq!(
        engine.get_tags("tagged", Some("src-2")).await.unwrap(),
        vec!["src-2-tag".to_string()]
    );
}

#[tokio::test]
async fn in_memory_remove_tag_is_silent_for_absent_tag_and_missing_page() {
    let engine = InMemoryEngine::default();
    engine.upsert_page(page_input("tagged", "src-1")).await.unwrap();
    engine.add_tag("tagged", "keep", Some("src-1")).await.unwrap();
    engine.add_tag("tagged", "drop", Some("src-1")).await.unwrap();

    engine.remove_tag("tagged", "drop", Some("src-1")).await.unwrap();
    engine.remove_tag("tagged", "absent", Some("src-1")).await.unwrap();
    engine.remove_tag("missing", "absent", Some("src-1")).await.unwrap();

    assert_eq!(
        engine.get_tags("tagged", Some("src-1")).await.unwrap(),
        vec!["keep".to_string()]
    );
}

#[tokio::test]
async fn in_memory_add_tag_ignores_soft_deleted_pages() {
    let engine = InMemoryEngine::default();
    engine.upsert_page(page_input("deleted", "src-1")).await.unwrap();
    engine.soft_delete_page("deleted", Some("src-1")).await.unwrap();

    let result = engine.add_tag("deleted", "tag", Some("src-1")).await;

    assert!(result.is_err(), "soft-deleted page should not be taggable");
    assert!(engine.get_tags("deleted", Some("src-1")).await.unwrap().is_empty());
}
```

**Step 2: Run tag tests to verify RED**

Run:

```bash
cargo test --manifest-path "/Users/bilibili/Documents/workspace/jununfly/zbrain-rust/Cargo.toml" -p zbrain-core --test in_memory_engine_contract in_memory_tags
cargo test --manifest-path "/Users/bilibili/Documents/workspace/jununfly/zbrain-rust/Cargo.toml" -p zbrain-core --test in_memory_engine_contract in_memory_remove_tag
cargo test --manifest-path "/Users/bilibili/Documents/workspace/jununfly/zbrain-rust/Cargo.toml" -p zbrain-core --test in_memory_engine_contract in_memory_add_tag
```

Expected: fail with `Unsupported("pending slice 6a")` for tag methods.

**Step 3: Implement minimal tag support in existing `Page::metadata`**

Do not add new public test-only fields. Use the existing `metadata` JSON object to store an in-memory-only `tags` array. This keeps the test double self-contained and avoids changing `InMemoryEngine` struct shape.

Add private helpers near `InMemoryEngine` helpers in `engine.rs`:

```rust
fn page_tags(page: &Page) -> Vec<String> {
    page.metadata
        .get("tags")
        .and_then(Value::as_array)
        .map(|tags| {
            tags.iter()
                .filter_map(Value::as_str)
                .map(ToString::to_string)
                .collect()
        })
        .unwrap_or_default()
}

fn set_page_tags(page: &mut Page, mut tags: Vec<String>) {
    tags.sort();
    tags.dedup();
    let Some(metadata) = page.metadata.as_object_mut() else {
        page.metadata = json!({ "tags": tags });
        return;
    };
    metadata.insert("tags".to_string(), json!(tags));
}
```

If `json!` is not imported in `engine.rs`, either add `use serde_json::json;` or build the array through `Value::Array` manually. Prefer the smallest change consistent with existing imports.

Implement methods:

```rust
async fn add_tag(&self, slug: &str, tag: &str, source_id: Option<&str>) -> crate::Result<()> {
    let mut store = self
        .store
        .lock()
        .expect("InMemoryEngine store mutex poisoned");
    let Some(page) = store.iter_mut().find(|p| {
        p.slug == slug
            && p.deleted_at.is_none()
            && source_id.unwrap_or("default") == p.source_id
    }) else {
        return Err(Error::page_not_found(slug));
    };

    let mut tags = page_tags(page);
    tags.push(tag.to_string());
    set_page_tags(page, tags);
    page.updated_at = current_utc_iso8601();
    Ok(())
}

async fn remove_tag(&self, slug: &str, tag: &str, source_id: Option<&str>) -> crate::Result<()> {
    let mut store = self
        .store
        .lock()
        .expect("InMemoryEngine store mutex poisoned");
    let Some(page) = store.iter_mut().find(|p| {
        p.slug == slug
            && p.deleted_at.is_none()
            && source_id.unwrap_or("default") == p.source_id
    }) else {
        return Ok(());
    };

    let mut tags = page_tags(page);
    tags.retain(|existing| existing != tag);
    set_page_tags(page, tags);
    page.updated_at = current_utc_iso8601();
    Ok(())
}

async fn get_tags(&self, slug: &str, source_id: Option<&str>) -> crate::Result<Vec<String>> {
    let store = self
        .store
        .lock()
        .expect("InMemoryEngine store mutex poisoned");
    let Some(page) = store.iter().find(|p| {
        p.slug == slug
            && p.deleted_at.is_none()
            && source_id.unwrap_or("default") == p.source_id
    }) else {
        return Ok(Vec::new());
    };

    let mut tags = page_tags(page);
    tags.sort();
    tags.dedup();
    Ok(tags)
}
```

**Step 4: Run tag tests to verify GREEN**

Run:

```bash
cargo test --manifest-path "/Users/bilibili/Documents/workspace/jununfly/zbrain-rust/Cargo.toml" -p zbrain-core --test in_memory_engine_contract in_memory_tags
cargo test --manifest-path "/Users/bilibili/Documents/workspace/jununfly/zbrain-rust/Cargo.toml" -p zbrain-core --test in_memory_engine_contract in_memory_remove_tag
cargo test --manifest-path "/Users/bilibili/Documents/workspace/jununfly/zbrain-rust/Cargo.toml" -p zbrain-core --test in_memory_engine_contract in_memory_add_tag
```

Expected: PASS.

**Step 5: Commit optional checkpoint**

```bash
git add crates/zbrain-core/src/engine.rs crates/zbrain-core/tests/in_memory_engine_contract.rs
git commit -m "feat(core): implement in-memory page tags contract"
```

---

## Task 3: Add InMemory advanced-write RED tests

**Files:**

- Modify: `/Users/bilibili/Documents/workspace/jununfly/zbrain-rust/crates/zbrain-core/tests/in_memory_engine_contract.rs`
- Modify later: `/Users/bilibili/Documents/workspace/jununfly/zbrain-rust/crates/zbrain-core/src/engine.rs`

**Step 1: Write failing refresh / contextual retrieval tests**

Add imports if needed:

```rust
use zbrain_core::{CRMode, RefreshPageBodyArgs};
```

Append:

```rust
#[tokio::test]
async fn in_memory_refresh_page_body_updates_live_exact_source_only() {
    let engine = InMemoryEngine::default();
    engine.upsert_page(page_input("shared", "src-1")).await.unwrap();
    engine.upsert_page(page_input("shared", "src-2")).await.unwrap();
    engine.upsert_page(page_input("deleted", "src-1")).await.unwrap();
    engine.soft_delete_page("deleted", Some("src-1")).await.unwrap();

    let timeline = json!([{ "event": "updated" }]);
    engine
        .refresh_page_body(&RefreshPageBodyArgs {
            slug: "shared".to_string(),
            source_id: "src-1".to_string(),
            compiled_truth: "new truth".to_string(),
            timeline: timeline.clone(),
            content_hash: "new-hash".to_string(),
        })
        .await
        .unwrap();
    engine
        .refresh_page_body(&RefreshPageBodyArgs {
            slug: "deleted".to_string(),
            source_id: "src-1".to_string(),
            compiled_truth: "should not apply".to_string(),
            timeline: json!([]),
            content_hash: "deleted-hash".to_string(),
        })
        .await
        .unwrap();

    let updated = engine.get_page("shared", None, Some("src-1")).await.unwrap().unwrap();
    let untouched = engine.get_page("shared", None, Some("src-2")).await.unwrap().unwrap();

    assert_eq!(updated.compiled_truth, "new truth");
    assert_eq!(updated.timeline, timeline.to_string());
    assert_eq!(updated.content_hash.as_deref(), Some("new-hash"));
    assert_ne!(untouched.compiled_truth, "new truth");
}

#[tokio::test]
async fn in_memory_update_contextual_retrieval_state_updates_live_exact_source_only() {
    let engine = InMemoryEngine::default();
    engine.upsert_page(page_input("shared", "src-1")).await.unwrap();
    engine.upsert_page(page_input("shared", "src-2")).await.unwrap();
    engine.upsert_page(page_input("deleted", "src-1")).await.unwrap();
    engine.soft_delete_page("deleted", Some("src-1")).await.unwrap();

    engine
        .update_page_contextual_retrieval_state(
            "shared",
            "src-1",
            "per_chunk_synopsis",
            Some("corpus-v2"),
        )
        .await
        .unwrap();
    engine
        .update_page_contextual_retrieval_state("deleted", "src-1", "full_doc", None)
        .await
        .unwrap();

    let updated = engine.get_page("shared", None, Some("src-1")).await.unwrap().unwrap();
    let untouched = engine.get_page("shared", None, Some("src-2")).await.unwrap().unwrap();

    assert_eq!(updated.contextual_retrieval_mode, Some(CRMode::PerChunkSynopsis));
    assert_eq!(updated.corpus_generation.as_deref(), Some("corpus-v2"));
    assert!(untouched.contextual_retrieval_mode.is_none());
    assert!(untouched.corpus_generation.is_none());
}
```

**Step 2: Run advanced-write tests to verify RED**

Run:

```bash
cargo test --manifest-path "/Users/bilibili/Documents/workspace/jununfly/zbrain-rust/Cargo.toml" -p zbrain-core --test in_memory_engine_contract in_memory_refresh_page_body
cargo test --manifest-path "/Users/bilibili/Documents/workspace/jununfly/zbrain-rust/Cargo.toml" -p zbrain-core --test in_memory_engine_contract in_memory_update_contextual
```

Expected: fail with `Unsupported("pending slice 6a")`.

**Step 3: Implement minimal advanced-write methods**

```rust
async fn refresh_page_body(&self, args: &RefreshPageBodyArgs) -> crate::Result<()> {
    let mut store = self
        .store
        .lock()
        .expect("InMemoryEngine store mutex poisoned");
    if let Some(page) = store.iter_mut().find(|p| {
        p.slug == args.slug && p.source_id == args.source_id && p.deleted_at.is_none()
    }) {
        page.compiled_truth = args.compiled_truth.clone();
        page.timeline = args.timeline.to_string();
        page.content_hash = Some(args.content_hash.clone());
        page.updated_at = current_utc_iso8601();
    }
    Ok(())
}

async fn update_page_contextual_retrieval_state(
    &self,
    slug: &str,
    source_id: &str,
    mode: &str,
    corpus_generation: Option<&str>,
) -> crate::Result<()> {
    let mut store = self
        .store
        .lock()
        .expect("InMemoryEngine store mutex poisoned");
    if let Some(page) = store
        .iter_mut()
        .find(|p| p.slug == slug && p.source_id == source_id && p.deleted_at.is_none())
    {
        page.contextual_retrieval_mode = CRMode::from_str(mode);
        page.corpus_generation = corpus_generation.map(ToString::to_string);
        page.updated_at = current_utc_iso8601();
    }
    Ok(())
}
```

If `CRMode::from_str` is not the exact helper name, inspect existing libsql/postgres conversion code and use the same conversion path.

**Step 4: Run advanced-write tests to verify GREEN**

Run:

```bash
cargo test --manifest-path "/Users/bilibili/Documents/workspace/jununfly/zbrain-rust/Cargo.toml" -p zbrain-core --test in_memory_engine_contract in_memory_refresh_page_body
cargo test --manifest-path "/Users/bilibili/Documents/workspace/jununfly/zbrain-rust/Cargo.toml" -p zbrain-core --test in_memory_engine_contract in_memory_update_contextual
```

Expected: PASS.

**Step 5: Commit optional checkpoint**

```bash
git add crates/zbrain-core/src/engine.rs crates/zbrain-core/tests/in_memory_engine_contract.rs
git commit -m "feat(core): implement in-memory page update contract"
```

---

## Task 4: Add InMemory advanced-read RED tests

**Files:**

- Modify: `/Users/bilibili/Documents/workspace/jununfly/zbrain-rust/crates/zbrain-core/tests/in_memory_engine_contract.rs`
- Modify later: `/Users/bilibili/Documents/workspace/jununfly/zbrain-rust/crates/zbrain-core/src/engine.rs`

**Step 1: Write failing advanced-read tests**

Add imports if needed:

```rust
use zbrain_core::PageRef;
```

Append:

```rust
#[tokio::test]
async fn in_memory_get_all_slugs_includes_soft_deleted_and_filters_source() {
    let engine = InMemoryEngine::default();
    engine.upsert_page(page_input("alpha", "src-1")).await.unwrap();
    engine.upsert_page(page_input("beta", "src-1")).await.unwrap();
    engine.upsert_page(page_input("gamma", "src-2")).await.unwrap();
    engine.soft_delete_page("beta", Some("src-1")).await.unwrap();

    assert_eq!(
        engine.get_all_slugs(Some("src-1")).await.unwrap(),
        HashSet::from(["alpha".to_string(), "beta".to_string()])
    );
    assert_eq!(
        engine.get_all_slugs(None).await.unwrap(),
        HashSet::from(["alpha".to_string(), "beta".to_string(), "gamma".to_string()])
    );
}

#[tokio::test]
async fn in_memory_list_all_page_refs_returns_live_refs_ordered_by_source_then_slug() {
    let engine = InMemoryEngine::default();
    engine.upsert_page(page_input("b", "src-2")).await.unwrap();
    engine.upsert_page(page_input("a", "src-1")).await.unwrap();
    engine.upsert_page(page_input("c", "src-1")).await.unwrap();
    engine.soft_delete_page("c", Some("src-1")).await.unwrap();

    let refs = engine.list_all_page_refs().await.unwrap();

    assert_eq!(
        refs,
        vec![
            PageRef { source_id: "src-1".to_string(), slug: "a".to_string() },
            PageRef { source_id: "src-2".to_string(), slug: "b".to_string() },
        ]
    );
}

#[tokio::test]
async fn in_memory_get_page_timestamps_excludes_deleted_and_missing_rows() {
    let engine = InMemoryEngine::default();
    engine.upsert_page(page_input("live", "src-1")).await.unwrap();
    engine.upsert_page(page_input("deleted", "src-1")).await.unwrap();
    engine.soft_delete_page("deleted", Some("src-1")).await.unwrap();

    let stamps = engine
        .get_page_timestamps(&["live".to_string(), "deleted".to_string(), "missing".to_string()])
        .await
        .unwrap();

    assert!(stamps.contains_key("live"));
    assert!(!stamps.contains_key("deleted"));
    assert!(!stamps.contains_key("missing"));
}

#[tokio::test]
async fn in_memory_get_effective_dates_uses_source_slug_keys_and_fallback_dates() {
    let engine = InMemoryEngine::default();
    engine.upsert_page(page_input("live", "src-1")).await.unwrap();
    engine.upsert_page(page_input("deleted", "src-1")).await.unwrap();
    engine.soft_delete_page("deleted", Some("src-1")).await.unwrap();

    let dates = engine
        .get_effective_dates(&[
            PageRef { source_id: "src-1".to_string(), slug: "live".to_string() },
            PageRef { source_id: "src-1".to_string(), slug: "deleted".to_string() },
            PageRef { source_id: "src-1".to_string(), slug: "missing".to_string() },
        ])
        .await
        .unwrap();

    assert!(dates.contains_key("src-1::live"));
    assert!(!dates.contains_key("src-1::deleted"));
    assert!(!dates.contains_key("src-1::missing"));
}

#[tokio::test]
async fn in_memory_get_salience_scores_uses_emotional_weight_times_five() {
    let engine = InMemoryEngine::default();
    let mut weighted = page_input("weighted", "src-1");
    weighted.emotional_weight = Some(0.7);
    let mut neutral = page_input("neutral", "src-1");
    neutral.emotional_weight = None;
    engine.upsert_page(weighted).await.unwrap();
    engine.upsert_page(neutral).await.unwrap();

    let scores = engine
        .get_salience_scores(&[
            PageRef { source_id: "src-1".to_string(), slug: "weighted".to_string() },
            PageRef { source_id: "src-1".to_string(), slug: "neutral".to_string() },
            PageRef { source_id: "src-1".to_string(), slug: "missing".to_string() },
        ])
        .await
        .unwrap();

    assert_eq!(scores.get("src-1::weighted"), Some(&3.5));
    assert_eq!(scores.get("src-1::neutral"), Some(&0.0));
    assert!(!scores.contains_key("src-1::missing"));
}

#[tokio::test]
async fn in_memory_find_orphan_pages_returns_empty_until_link_graph_exists() {
    let engine = InMemoryEngine::default();
    engine.upsert_page(page_input("page", "src-1")).await.unwrap();

    let orphans = engine.find_orphan_pages(Some("src-1")).await.unwrap();

    assert!(orphans.is_empty());
}
```

**Step 2: Run advanced-read tests to verify RED**

Run:

```bash
cargo test --manifest-path "/Users/bilibili/Documents/workspace/jununfly/zbrain-rust/Cargo.toml" -p zbrain-core --test in_memory_engine_contract in_memory_get_all_slugs
cargo test --manifest-path "/Users/bilibili/Documents/workspace/jununfly/zbrain-rust/Cargo.toml" -p zbrain-core --test in_memory_engine_contract in_memory_list_all_page_refs
cargo test --manifest-path "/Users/bilibili/Documents/workspace/jununfly/zbrain-rust/Cargo.toml" -p zbrain-core --test in_memory_engine_contract in_memory_get_page_timestamps
cargo test --manifest-path "/Users/bilibili/Documents/workspace/jununfly/zbrain-rust/Cargo.toml" -p zbrain-core --test in_memory_engine_contract in_memory_get_effective_dates
cargo test --manifest-path "/Users/bilibili/Documents/workspace/jununfly/zbrain-rust/Cargo.toml" -p zbrain-core --test in_memory_engine_contract in_memory_get_salience_scores
cargo test --manifest-path "/Users/bilibili/Documents/workspace/jununfly/zbrain-rust/Cargo.toml" -p zbrain-core --test in_memory_engine_contract in_memory_find_orphan_pages
```

Expected: fail with `Unsupported("pending slice 6a")` for the newly covered methods.

**Step 3: Implement minimal advanced-read methods**

Add imports if needed in `engine.rs`:

```rust
use std::collections::{HashMap, HashSet};
```

Implement methods in `impl BrainEngine for InMemoryEngine`:

```rust
async fn get_all_slugs(&self, source_id: Option<&str>) -> crate::Result<HashSet<String>> {
    let store = self
        .store
        .lock()
        .expect("InMemoryEngine store mutex poisoned");
    Ok(store
        .iter()
        .filter(|p| source_id.is_none_or(|source_id| p.source_id == source_id))
        .map(|p| p.slug.clone())
        .collect())
}

async fn list_all_page_refs(&self) -> crate::Result<Vec<PageRef>> {
    let store = self
        .store
        .lock()
        .expect("InMemoryEngine store mutex poisoned");
    let mut refs: Vec<PageRef> = store
        .iter()
        .filter(|p| p.deleted_at.is_none())
        .map(|p| PageRef {
            slug: p.slug.clone(),
            source_id: p.source_id.clone(),
        })
        .collect();
    refs.sort_by(|a, b| {
        a.source_id
            .cmp(&b.source_id)
            .then_with(|| a.slug.cmp(&b.slug))
    });
    Ok(refs)
}

async fn find_orphan_pages(&self, _source_id: Option<&str>) -> crate::Result<Vec<OrphanPage>> {
    Ok(Vec::new())
}

async fn get_page_timestamps(&self, slugs: &[String]) -> crate::Result<HashMap<String, String>> {
    let wanted: HashSet<&str> = slugs.iter().map(String::as_str).collect();
    let store = self
        .store
        .lock()
        .expect("InMemoryEngine store mutex poisoned");
    Ok(store
        .iter()
        .filter(|p| p.deleted_at.is_none() && wanted.contains(p.slug.as_str()))
        .map(|p| (p.slug.clone(), p.updated_at.clone()))
        .collect())
}

async fn get_effective_dates(&self, pages: &[PageRef]) -> crate::Result<HashMap<String, String>> {
    let requested: HashSet<(&str, &str)> = pages
        .iter()
        .map(|page| (page.source_id.as_str(), page.slug.as_str()))
        .collect();
    let store = self
        .store
        .lock()
        .expect("InMemoryEngine store mutex poisoned");
    Ok(store
        .iter()
        .filter(|p| {
            p.deleted_at.is_none() && requested.contains(&(p.source_id.as_str(), p.slug.as_str()))
        })
        .map(|p| {
            let key = format!("{}::{}", p.source_id, p.slug);
            let value = p
                .effective_date
                .clone()
                .unwrap_or_else(|| p.updated_at.clone());
            (key, value)
        })
        .collect())
}

async fn get_salience_scores(&self, pages: &[PageRef]) -> crate::Result<HashMap<String, f64>> {
    let requested: HashSet<(&str, &str)> = pages
        .iter()
        .map(|page| (page.source_id.as_str(), page.slug.as_str()))
        .collect();
    let store = self
        .store
        .lock()
        .expect("InMemoryEngine store mutex poisoned");
    Ok(store
        .iter()
        .filter(|p| {
            p.deleted_at.is_none() && requested.contains(&(p.source_id.as_str(), p.slug.as_str()))
        })
        .map(|p| {
            let key = format!("{}::{}", p.source_id, p.slug);
            let score = p.emotional_weight.unwrap_or(0.0) * 5.0;
            (key, score)
        })
        .collect())
}
```

**Step 4: Run advanced-read tests to verify GREEN**

Run the six commands from Step 2 again.

Expected: PASS.

**Step 5: Commit optional checkpoint**

```bash
git add crates/zbrain-core/src/engine.rs crates/zbrain-core/tests/in_memory_engine_contract.rs
git commit -m "feat(core): implement in-memory page read contract"
```

---

## Task 5: Remove BrainEngine S6 default fallbacks

**Files:**

- Modify: `/Users/bilibili/Documents/workspace/jununfly/zbrain-rust/crates/zbrain-core/src/engine.rs`

**Step 1: Run current targeted tests before hardening**

Run:

```bash
cargo test --manifest-path "/Users/bilibili/Documents/workspace/jununfly/zbrain-rust/Cargo.toml" -p zbrain-core --test in_memory_engine_contract
```

Expected: PASS.

**Step 2: Remove default method bodies from `BrainEngine` trait**

In `engine.rs`, replace each S6 fallback method body with required method signatures.

From pattern:

```rust
async fn restore_page(&self, _slug: &str, _source_id: Option<&str>) -> crate::Result<bool> {
    Err(Error::unsupported("pending slice 6a"))
}
```

To:

```rust
async fn restore_page(&self, slug: &str, source_id: Option<&str>) -> crate::Result<bool>;
```

Apply to all S6 methods that still have default fallback bodies:

```rust
async fn find_duplicate_page(
    &self,
    source_id: &str,
    opts: &FindDuplicatePageOpts,
) -> crate::Result<Option<Page>>;
async fn soft_delete_page(&self, slug: &str, source_id: Option<&str>) -> crate::Result<Option<String>>;
async fn restore_page(&self, slug: &str, source_id: Option<&str>) -> crate::Result<bool>;
async fn purge_deleted_pages(&self, older_than_hours: u32) -> crate::Result<PurgeResult>;
async fn add_tag(&self, slug: &str, tag: &str, source_id: Option<&str>) -> crate::Result<()>;
async fn remove_tag(&self, slug: &str, tag: &str, source_id: Option<&str>) -> crate::Result<()>;
async fn get_tags(&self, slug: &str, source_id: Option<&str>) -> crate::Result<Vec<String>>;
async fn refresh_page_body(&self, args: &RefreshPageBodyArgs) -> crate::Result<()>;
async fn update_page_contextual_retrieval_state(
    &self,
    slug: &str,
    source_id: &str,
    mode: &str,
    corpus_generation: Option<&str>,
) -> crate::Result<()>;
async fn get_all_slugs(&self, source_id: Option<&str>) -> crate::Result<HashSet<String>>;
async fn list_all_page_refs(&self) -> crate::Result<Vec<PageRef>>;
async fn find_orphan_pages(&self, source_id: Option<&str>) -> crate::Result<Vec<OrphanPage>>;
async fn get_page_timestamps(&self, slugs: &[String]) -> crate::Result<HashMap<String, String>>;
async fn get_effective_dates(&self, pages: &[PageRef]) -> crate::Result<HashMap<String, String>>;
async fn get_salience_scores(&self, pages: &[PageRef]) -> crate::Result<HashMap<String, f64>>;
```

**Step 3: Update stale group comment**

Replace stale comment:

```rust
// Default implementations return `Error::Unsupported("pending slice 6a")`
// so existing backends (postgres / libsql / in-memory) compile unchanged.
// The S6-T2 green phase overrides them per backend; postgres holds on
// `pending slice 6a-pg` until slice 6a-pg lands.
```

With:

```rust
// Backends must implement the full Slice 6a S6 method group explicitly.
// This keeps backend drift compile-visible instead of hiding missing methods
// behind `Error::Unsupported("pending slice 6a")` fallbacks.
```

**Step 4: Run build to expose any missing impl**

Run:

```bash
cargo build --manifest-path "/Users/bilibili/Documents/workspace/jununfly/zbrain-rust/Cargo.toml" -p zbrain-core
```

Expected: PASS. If it fails with missing trait methods, implement only the missing methods and add/adjust tests first if behavior is not already covered.

**Step 5: Run targeted tests**

Run:

```bash
cargo test --manifest-path "/Users/bilibili/Documents/workspace/jununfly/zbrain-rust/Cargo.toml" -p zbrain-core --test in_memory_engine_contract
cargo test --manifest-path "/Users/bilibili/Documents/workspace/jununfly/zbrain-rust/Cargo.toml" -p zbrain-core --test page_methods_restore_page --test page_methods_purge_deleted_pages
cargo test --manifest-path "/Users/bilibili/Documents/workspace/jununfly/zbrain-rust/Cargo.toml" -p zbrain-core --test page_methods_get_all_slugs --test page_methods_list_all_page_refs --test page_methods_get_page_timestamps --test page_methods_get_effective_dates --test page_methods_get_salience_scores
```

Expected: PASS, allowing PG-specific tests to skip when `ZBRAIN_TEST_PG_URL` is unset.

**Step 6: Commit optional checkpoint**

```bash
git add crates/zbrain-core/src/engine.rs crates/zbrain-core/tests/in_memory_engine_contract.rs
git commit -m "feat(core): harden BrainEngine page method contract"
```

---

## Task 6: Clean stale RED comments

**Files:**

- Modify: `/Users/bilibili/Documents/workspace/jununfly/zbrain-rust/crates/zbrain-core/tests/page_methods_list_all_page_refs.rs`
- Modify: `/Users/bilibili/Documents/workspace/jununfly/zbrain-rust/crates/zbrain-core/tests/page_methods_get_page_timestamps.rs`
- Modify: `/Users/bilibili/Documents/workspace/jununfly/zbrain-rust/crates/zbrain-core/tests/page_methods_get_effective_dates.rs`

**Step 1: Remove stale comments**

Remove comments that still say tests are RED until libsql replaces the default fallback, e.g.:

```rust
//! These tests are RED until S3 GREEN replaces the libsql default
//! `Err(Error::unsupported("pending slice 6a"))` with a real implementation.
```

Keep behavior comments that document current contracts.

**Step 2: Run comment-adjacent targeted tests**

Run:

```bash
cargo test --manifest-path "/Users/bilibili/Documents/workspace/jununfly/zbrain-rust/Cargo.toml" -p zbrain-core --test page_methods_list_all_page_refs --test page_methods_get_page_timestamps --test page_methods_get_effective_dates
```

Expected: PASS, allowing PG-specific tests to skip when `ZBRAIN_TEST_PG_URL` is unset.

**Step 3: Commit optional checkpoint**

```bash
git add crates/zbrain-core/tests/page_methods_list_all_page_refs.rs crates/zbrain-core/tests/page_methods_get_page_timestamps.rs crates/zbrain-core/tests/page_methods_get_effective_dates.rs
git commit -m "docs(core): remove stale page method red comments"
```

---

## Task 7: Final verification and completion commit

**Files:**

- Verify all modified files.

**Step 1: Format**

Run:

```bash
cargo fmt --manifest-path "/Users/bilibili/Documents/workspace/jununfly/zbrain-rust/Cargo.toml" --all
cargo fmt --manifest-path "/Users/bilibili/Documents/workspace/jununfly/zbrain-rust/Cargo.toml" --all --check
```

Expected: PASS.

**Step 2: Build**

Run:

```bash
cargo build --manifest-path "/Users/bilibili/Documents/workspace/jununfly/zbrain-rust/Cargo.toml"
```

Expected: PASS.

**Step 3: Workspace tests**

Run:

```bash
cargo test --manifest-path "/Users/bilibili/Documents/workspace/jununfly/zbrain-rust/Cargo.toml" --workspace
```

Expected: PASS for runnable tests. Explicitly report any PG skips caused by missing `ZBRAIN_TEST_PG_URL` as skips, not as completed PG infra.

**Step 4: Clippy**

Run:

```bash
cargo clippy --manifest-path "/Users/bilibili/Documents/workspace/jununfly/zbrain-rust/Cargo.toml" --workspace --all-targets -- -D warnings
```

Expected: PASS.

**Step 5: Inspect diff**

Run:

```bash
git -C "/Users/bilibili/Documents/workspace/jununfly/zbrain-rust" diff -- crates/zbrain-core/src/engine.rs crates/zbrain-core/tests/in_memory_engine_contract.rs crates/zbrain-core/tests/page_methods_list_all_page_refs.rs crates/zbrain-core/tests/page_methods_get_page_timestamps.rs crates/zbrain-core/tests/page_methods_get_effective_dates.rs docs/plans/2026-06-01-brainengine-contract-hardening.md
```

Expected: diff only contains planned changes.

**Step 6: Commit final work if not already committed in checkpoints**

Use a new commit. Do not amend previous commits.

```bash
git add docs/plans/2026-06-01-brainengine-contract-hardening.md crates/zbrain-core/src/engine.rs crates/zbrain-core/tests/in_memory_engine_contract.rs crates/zbrain-core/tests/page_methods_list_all_page_refs.rs crates/zbrain-core/tests/page_methods_get_page_timestamps.rs crates/zbrain-core/tests/page_methods_get_effective_dates.rs
git commit -m "feat(core): harden BrainEngine page contract"
```

---

## Acceptance checklist

- [x] `in_memory_engine_contract.rs` exists and tests every missing S6 method on `InMemoryEngine`.
- [x] Each new behavior test was observed RED before implementation.
- [x] `InMemoryEngine` explicitly implements all 15 S6 methods.
- [x] `LibsqlEngine` and `PostgresEngine` still compile with their existing S6 implementations.
- [x] `BrainEngine` trait no longer provides default `Error::unsupported("pending slice 6a")` fallback bodies for S6 methods.
- [x] Stale fallback comments are removed or updated.
- [x] `cargo fmt --all --check` passes.
- [x] `cargo build --manifest-path .../Cargo.toml` passes.
- [x] `cargo test --manifest-path .../Cargo.toml --workspace` passes for runnable tests.
- [x] `cargo clippy --manifest-path .../Cargo.toml --workspace --all-targets -- -D warnings` passes.
- [x] Any PG skips due to missing `ZBRAIN_TEST_PG_URL` are reported as skips.
- [x] Final commit is a new commit, not an amend.

## Outcome

BrainEngine contract hardening completed as C1. The implementation removed all 15 Slice 6a S6 page-method default fallbacks from the `BrainEngine` trait and upgraded them to required backend contract methods. `InMemoryEngine` was brought to 15/15 parity; future backend drift now fails at compile time instead of falling through to `Error::unsupported("pending slice 6a")`.

Commit sequence:

| # | Commit | Plan task | Content |
|---|---|---|---|
| 1 | `67ebcce` | Task 1 + 2 | `InMemoryEngine` lifecycle + tag contract |
| 2 | `415d856` | Task 3 | `InMemoryEngine` advanced-write contract |
| 3 | `2654584` | Task 4 | `InMemoryEngine` advanced-read contract |
| 4 | `b456e6b` | hotfix | `in_memory_soft_delete_page_matches_libsql_contract` uses `include_deleted=true` for verification |
| 5 | `f7a647e` | Task 5 | Remove 15 S6 fallback bodies from `BrainEngine` trait |
| 6 | `6d434c4` | Task 6 | Clean stale RED comments in three `page_methods_*.rs` files |
| 7 | `f48f6bd` | Task 7 style follow-up | `cargo fmt --all` style-only layout changes required by final verification |

Final verification:

```text
cargo fmt --all --check                                  PASS
cargo build --workspace                                  PASS
cargo test --workspace                                   PASS
cargo clippy --workspace --all-targets -- -D warnings    PASS
```

PG suites ran as real tests rather than unset-url skips; `ZBRAIN_TEST_PG_URL` was configured during verification.

Note on `f48f6bd`: the plan required a new commit and prohibited amending previous commits. `cargo fmt --all` changed only formatting/layout in existing long signatures, closures, and macro arguments, so the style-only diff was kept as an independent commit instead of being amended into `f7a647e`.

## Execution handoff

Plan complete and saved to `docs/plans/2026-06-01-brainengine-contract-hardening.md`.

Two execution options:

1. **Subagent-Driven (this session)** - dispatch a fresh subagent per task, review between tasks, fast iteration.
2. **Parallel Session (separate)** - open a new session in the worktree and use `executing-plans` to execute task-by-task with checkpoints.

Recommended for this repo: **Subagent-Driven (this session)**, because the work is TDD-heavy and benefits from review after each batch.
