# Slice #110-d Libsql TS Contract Alignment Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** Align the libsql backend with the four TS/PG #110-c page-contract changes and lock the behavior with libsql-specific contract tests.

**Architecture:** Treat TS PGLite as the source of truth, PG #110-c tests as the template, and libsql as a SQLite-adapted implementation. Prefer characterization/regression tests first because the current libsql implementation already appears to satisfy most contract points; only change production code after observing a RED failure for a missing contract.

**Tech Stack:** Rust, Tokio, libsql, SQLite `PRAGMA table_info`, `zbrain-core` `BrainEngine`, `cargo test`, `cargo clippy`.

---

## Current baseline

Branch:

```text
rust-rewrite
```

Known working-tree state before #110-d execution:

```text
 M crates/zbrain-core/src/libsql.rs
?? .codegraph/
```

Important: the `libsql.rs` modification is the user-requested TODO comment. Do not accidentally add `.codegraph/` to any commit.

Existing #110-d-relevant files:

- PG contract template: `crates/zbrain-core/tests/postgres_engine_full_columns.rs`
- Existing libsql CRUD tests: `crates/zbrain-core/tests/libsql_engine_page_crud.rs`
- Existing libsql lifecycle/schema tests: `crates/zbrain-core/tests/libsql_engine_lifecycle.rs`
- Libsql production implementation: `crates/zbrain-core/src/libsql.rs`
- SQLite migration: `crates/zbrain-core/migrations-sqlite/0002_pages_full_columns.sql`
- Upstream #110-c notes: `docs/plans/20260526/17-session-state-110c.md`
- Flake isolation notes: `docs/plans/20260526/18-session-state-110h-init-schema-flake.md`
- Slice conventions: `docs/plans/20260526/16-slice-index-and-conventions.md`

## Source-of-truth contract

TS PGLite `putPage` behavior:

```ts
const frontmatter = page.frontmatter || {};
const sourceKind = page.source_kind ?? null;
const sourceUri = page.source_uri ?? null;
const ingestedVia = page.ingested_via ?? null;
const ingestedAt = (sourceKind || sourceUri || ingestedVia)
  ? new Date().toISOString()
  : null;
```

TS schema shape:

```sql
frontmatter   JSONB   NOT NULL DEFAULT '{}',
last_retrieved_at     TIMESTAMPTZ,
corpus_generation          TEXT,
```

Libsql SQLite adaptations:

```sql
ALTER TABLE pages ADD COLUMN frontmatter            TEXT    NOT NULL DEFAULT '{}';
ALTER TABLE pages ADD COLUMN last_retrieved_at      TEXT;
ALTER TABLE pages ADD COLUMN corpus_generation      TEXT;
ALTER TABLE pages ADD COLUMN embedding              BLOB;
```

Target #110-d assertions:

1. `put_page` must not persist caller-provided `embedding`.
2. `put_page` must not persist caller-provided `last_retrieved_at`.
3. `ingested_at` must be server-stamped only when any of `source_kind` / `source_uri` / `ingested_via` is present, and caller-provided `input.ingested_at` must be ignored.
4. Without ingestion metadata, `ingested_at` must remain `None`.
5. Omitted `frontmatter` must round-trip as `{}` and the underlying SQLite column must be `TEXT NOT NULL DEFAULT '{}'`.
6. `corpus_generation` must be a SQLite `TEXT` column and Rust must decode it as `Option<String>`.

## Known nuance: PG template has one suspicious legacy assertion

`postgres_engine_full_columns.rs::roundtrip_all_full_columns` currently contains:

```rust
assert_eq!(
    page.ingested_at.as_deref(),
    Some("2026-05-30T00:00:00+00:00"),
    "ingested_at (client-provided)"
);
```

Do not copy this assertion into libsql. It conflicts with the TS PGLite source-of-truth and with the explicit server-stamp tests that say caller-provided `input.ingested_at` must be ignored.

## Execution tasks

### Task 1: Create libsql full-column contract test module

**Files:**
- Create: `crates/zbrain-core/tests/libsql_engine_full_columns.rs`
- Reference: `crates/zbrain-core/tests/postgres_engine_full_columns.rs`
- Reference: `crates/zbrain-core/tests/libsql_engine_page_crud.rs`
- Reference: `crates/zbrain-core/tests/libsql_engine_lifecycle.rs`

**Step 1: Write the failing/characterization test file**

Create a libsql-specific test module with helpers:

```rust
use libsql::Builder;
use serde_json::json;
use tempfile::NamedTempFile;
use zbrain_core::engine::{BrainEngine, EffectiveDateSource, EngineConfig, PageInput, PageKind};
use zbrain_core::libsql::LibsqlEngine;

async fn init_clean_engine() -> (LibsqlEngine, NamedTempFile, String) {
    let path = NamedTempFile::new().expect("alloc temp db file");
    let path_str = path.path().to_string_lossy().into_owned();
    let engine = LibsqlEngine::new();
    engine
        .connect(&EngineConfig {
            database_url: None,
            database_path: Some(path_str.clone()),
        })
        .await
        .expect("connect");
    engine.init_schema().await.expect("init_schema");
    (engine, path, path_str)
}

fn base_input() -> PageInput {
    PageInput {
        page_type: "note".to_string(),
        title: "Libsql Full Columns".to_string(),
        compiled_truth: "body".to_string(),
        timeline: Some("T1 -> T2".to_string()),
        frontmatter: Some(json!({"key": "value"})),
        content_hash: Some("sha256:abcdef".to_string()),
        page_kind: Some(PageKind::Markdown),
        effective_date: Some("2026-05-30".to_string()),
        effective_date_source: Some(EffectiveDateSource::Filename),
        import_filename: Some("contract.md".to_string()),
        chunker_version: Some(2),
        source_path: Some("/tmp/contract.md".to_string()),
        source_kind: Some("file".to_string()),
        source_uri: Some("file:///tmp/contract.md".to_string()),
        ingested_via: Some("cli".to_string()),
        ingested_at: None,
        last_retrieved_at: None,
        embedding: None,
    }
}
```

Add test names:

```rust
#[tokio::test]
async fn put_page_does_not_persist_embedding_or_last_retrieved_at() { /* ... */ }

#[tokio::test]
async fn ingested_at_server_stamped_when_any_ingestion_metadata_present() { /* ... */ }

#[tokio::test]
async fn ingested_at_remains_none_without_ingestion_metadata() { /* ... */ }

#[tokio::test]
async fn frontmatter_defaults_to_empty_object_when_omitted() { /* ... */ }

#[tokio::test]
async fn frontmatter_column_is_text_not_null_default_empty_object() { /* ... */ }

#[tokio::test]
async fn corpus_generation_column_is_text() { /* ... */ }
```

**Step 2: Run the new test module to verify RED or characterization GREEN**

Run:

```bash
cargo test -p zbrain-core --test libsql_engine_full_columns -- --test-threads=1
```

Expected outcomes:

- Preferred/likely: tests compile and pass immediately. Treat as characterization/regression coverage because production already matches TS.
- If a test fails for a real contract gap, keep it RED and move to Task 2.
- If a test errors because of test setup, fix the test setup and rerun until it either fails for the intended contract reason or passes.

### Task 2: Minimal production fix only if Task 1 exposes RED

**Files:**
- Modify only if needed: `crates/zbrain-core/src/libsql.rs`
- Modify only if schema shape is wrong: `crates/zbrain-core/migrations-sqlite/0002_pages_full_columns.sql` or a later immutable migration strategy agreed separately

**Step 1: Diagnose exact failing assertion**

Classify the failure:

- `embedding` / `last_retrieved_at` persisted unexpectedly: remove those columns and binds from `put_page` INSERT/UPDATE path.
- `ingested_at` accepts caller value: ensure `input.ingested_at` is not used and server-stamp uses only provenance fields.
- `frontmatter` omitted returns `null`: ensure `frontmatter.unwrap_or_else(|| json!({}))` before JSON serialization.
- `frontmatter` schema not `NOT NULL DEFAULT '{}'`: stop and reassess because current 0002 says it is correct.
- `corpus_generation` schema not `TEXT`: stop and reassess because current 0002 says it is correct.

**Step 2: Implement the smallest fix**

Current expected production shape already has the key behavior:

```rust
let ingested_at = if input.source_kind.is_some()
    || input.source_uri.is_some()
    || input.ingested_via.is_some()
{
    Some(current_utc_iso8601())
} else {
    None
};
```

Current `put_page` INSERT columns should remain 19 columns and should not include `embedding` or `last_retrieved_at`:

```rust
source_path, source_kind, source_uri, ingested_via, ingested_at
```

Do not add unrelated refactors.

**Step 3: Run the failing test again**

Run:

```bash
cargo test -p zbrain-core --test libsql_engine_full_columns -- --test-threads=1
```

Expected: PASS.

### Task 3: Verify no overlap/regression against existing libsql CRUD tests

**Files:**
- Existing test: `crates/zbrain-core/tests/libsql_engine_page_crud.rs`
- Existing test: `crates/zbrain-core/tests/libsql_engine_lifecycle.rs`

**Step 1: Run focused libsql suites**

Run:

```bash
cargo test -p zbrain-core --test libsql_engine_full_columns -- --test-threads=1 && \
cargo test -p zbrain-core --test libsql_engine_page_crud -- --test-threads=1 && \
cargo test -p zbrain-core --test libsql_engine_lifecycle -- --test-threads=1
```

Expected: PASS.

**Step 2: If `init_schema` native flake appears**

Do not silently treat it as a #110-d failure. Record it against #110-h follow-up unless the failing test is deterministically tied to new #110-d code.

### Task 4: Workspace verification

**Files:**
- No direct edits expected.

**Step 1: Run build/test/clippy three-green**

Run:

```bash
cargo build && \
cargo test --workspace && \
cargo clippy --workspace --all-targets -- -D warnings
```

Expected: all PASS, no warnings.

**Step 2: If clippy fails on the new test module**

Fix only style/lint issues in the test module unless clippy points at a real production issue introduced by this slice.

### Task 5: Commit #110-d safely

**Files to consider adding:**
- `docs/plans/20260526/19-session-state-110d-libsql-ts-contract-alignment.md`
- `crates/zbrain-core/tests/libsql_engine_full_columns.rs`
- `crates/zbrain-core/src/libsql.rs` only if the TODO comment should be included in this commit or if Task 2 required production changes

**Files not to add:**
- `.codegraph/`

**Step 1: Inspect status and diff**

Run:

```bash
git status --short
git diff -- crates/zbrain-core/tests/libsql_engine_full_columns.rs crates/zbrain-core/src/libsql.rs docs/plans/20260526/19-session-state-110d-libsql-ts-contract-alignment.md
```

**Step 2: Stage exact paths only**

Run:

```bash
git add docs/plans/20260526/19-session-state-110d-libsql-ts-contract-alignment.md \
  crates/zbrain-core/tests/libsql_engine_full_columns.rs
```

If and only if agreed, also stage:

```bash
git add crates/zbrain-core/src/libsql.rs
```

**Step 3: Commit**

Run:

```bash
git commit -m "test: align libsql page contracts with TS"
```

Expected: one focused #110-d commit, no `.codegraph/`.

## Recommended immediate next action

Proceed with Task 1: create `crates/zbrain-core/tests/libsql_engine_full_columns.rs` and run the focused test module first. This respects TDD: tests are written before any production change, and if they pass immediately they serve as characterization tests proving the libsql implementation is already aligned.
