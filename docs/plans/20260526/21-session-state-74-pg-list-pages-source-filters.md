# Session State: #74 PG list_pages source filters

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans if continuing implementation from this state. Use superpowers:test-driven-development for any follow-up behavior changes. Use superpowers:verification-before-completion before claiming completion or committing.

**Goal:** Preserve the current project state for slice #74 so a later session can resume or commit without re-discovering the context.

**Scope:** ZBrain Rust rewrite, branch `rust-rewrite`, slice #74. This handoff records the TDD path used to add PostgreSQL `list_pages` source filtering.

---

## Current repository state

Working tree snapshot at handoff creation:

```text
## rust-rewrite
 M crates/zbrain-core/src/postgres.rs
 M crates/zbrain-core/tests/postgres_engine_page_crud.rs
?? docs/plans/20260526/21-session-state-74-pg-list-pages-source-filters.md
```

Branch:

```text
rust-rewrite
```

Expected #74 files:

```text
crates/zbrain-core/src/postgres.rs
crates/zbrain-core/tests/postgres_engine_page_crud.rs
docs/plans/20260526/21-session-state-74-pg-list-pages-source-filters.md
```

Do not accidentally stage unrelated local files if present.

## #74 purpose

#74 adds PostgreSQL backend support for these existing `PageFilters` fields:

```rust
pub source_id: Option<String>,
pub source_ids: Option<Vec<String>>,
```

Target behavior mirrors the existing libsql backend:

1. `source_id: Some(id)` returns only pages whose `source_id` exactly matches `id`.
2. `source_ids: Some(ids)` returns pages whose `source_id` is included in `ids`.
3. `source_ids: Some(vec![])` returns an empty result immediately.
4. Existing `page_type` and `limit` behavior remains supported.
5. Existing PG ordering remains `ORDER BY id ASC` until a later slice migrates the full `sort/offset` surface.

## TDD path already followed

### RED tests added first

New tests were added to:

```text
crates/zbrain-core/tests/postgres_engine_page_crud.rs
```

Tests:

```rust
async fn list_pages_filters_by_source_id()
async fn list_pages_filters_by_source_ids()
```

Observed RED failure before production change showed PG `list_pages` ignored source filters:

```text
assertion `left == right` failed: only pg-alpha pages should appear
  left: 3
 right: 1
```

and for multi-source filtering:

```text
left: ["source-default", "source-alpha", "source-beta"]
right: ["source-default", "source-beta"]
```

### GREEN implementation

Production code changed in:

```text
crates/zbrain-core/src/postgres.rs
```

`PostgresEngine::list_pages` was migrated from a static SQL query with optional `page_type` / `limit` binds to a dynamic SQL builder that only emits active filter clauses.

Important implementation snippets:

```rust
if let Some(ids) = filters.source_ids.as_ref() {
    if ids.is_empty() {
        return Ok(Vec::new());
    }
}
```

```rust
let mut sql = format!("SELECT {FULL_PAGE_PROJECTION} FROM pages WHERE 1=1");
let mut param_idx: u32 = 1;
```

```rust
let source_eq_clause = if filters.source_id.is_some() {
    let frag = format!(" AND source_id = ${param_idx}");
    param_idx += 1;
    Some(frag)
} else {
    None
};
if let Some(ref frag) = source_eq_clause {
    sql.push_str(frag);
}
```

```rust
let source_any_clause = if filters.source_ids.is_some() {
    let frag = format!(" AND source_id = ANY(${param_idx}::text[])");
    param_idx += 1;
    Some(frag)
} else {
    None
};
if let Some(ref frag) = source_any_clause {
    sql.push_str(frag);
}
```

Bind order matches SQL fragment order:

```rust
if let Some(pt) = filters.page_type.as_deref() {
    query = query.bind(pt);
}
if let Some(sid) = filters.source_id.as_deref() {
    query = query.bind(sid);
}
if let Some(ref ids) = filters.source_ids {
    query = query.bind(ids.as_slice());
}
if let Some(limit) = filters.limit {
    query = query.bind(i64::try_from(limit).unwrap_or(i64::MAX));
}
```

## Issues encountered and resolved

### `unused_assignments` on `param_idx`

After introducing dynamic SQL, `limit` was the final optional bind. Incrementing `param_idx` after formatting `LIMIT $N` produced an unused assignment warning under strict verification.

Resolution:

```rust
let limit_param = if filters.limit.is_some() {
    Some(format!(" LIMIT ${param_idx}"))
} else {
    None
};
```

Do not increment `param_idx` after the final bind until another later filter is appended after `limit`.

### `clippy::similar_names`

Strict clippy rejected names that only differed by pluralization:

```text
source_id_param
source_ids_param
```

Resolution: rename to clearer SQL-clause names:

```rust
source_eq_clause
source_any_clause
```

### transient residual-row observation

One earlier filtered run temporarily showed an unexpected `soft-del-hidden` slug in the `source_ids` result. A later exact rerun passed:

```text
test list_pages_filters_by_source_ids ... ok
```

Given every PG test calls `init_clean_engine()` and truncates `pages`, the latest evidence supports treating the earlier observation as transient shared-test-db state rather than a stable #74 implementation bug.

## Verification already performed

Commands were run with:

```bash
set -a; source "/Users/bilibili/Documents/workspace/jununfly/zbrain-rust/.env"; set +a
```

### Exact filtered rerun

```bash
cargo test --manifest-path "/Users/bilibili/Documents/workspace/jununfly/zbrain-rust/Cargo.toml" \
  --test postgres_engine_page_crud list_pages_filters_by_source_ids -- --test-threads=1 --exact
```

Result:

```text
running 1 test
test list_pages_filters_by_source_ids ... ok

test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 16 filtered out
```

### Targeted source-filter tests

```bash
cargo test --manifest-path "/Users/bilibili/Documents/workspace/jununfly/zbrain-rust/Cargo.toml" \
  --test postgres_engine_page_crud list_pages_filters_by_source -- --test-threads=1
```

Result:

```text
running 2 tests
test list_pages_filters_by_source_id ... ok
test list_pages_filters_by_source_ids ... ok

test result: ok. 2 passed; 0 failed; 0 ignored; 0 measured; 15 filtered out
```

### Full PG CRUD integration file

```bash
cargo test --manifest-path "/Users/bilibili/Documents/workspace/jununfly/zbrain-rust/Cargo.toml" \
  --test postgres_engine_page_crud -- --test-threads=1
```

Result:

```text
running 17 tests
...
test list_pages_filters_by_source_id ... ok
test list_pages_filters_by_source_ids ... ok
...
test result: ok. 17 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out
```

### Workspace serial tests

```bash
cargo test --manifest-path "/Users/bilibili/Documents/workspace/jununfly/zbrain-rust/Cargo.toml" \
  --workspace -- --test-threads=1
```

Result summary:

```text
Status: completed
Duration: 32s
all displayed test groups ok
```

### Strict clippy

```bash
cargo clippy --manifest-path "/Users/bilibili/Documents/workspace/jununfly/zbrain-rust/Cargo.toml" \
  --workspace --all-targets -- -D warnings
```

Result:

```text
Checking zbrain-core v0.0.1 ...
Checking zbrain-cli v0.0.1 ...
Checking zbrain-web v0.0.1 ...
Checking zbrain-mcp v0.0.1 ...
Finished `dev` profile [unoptimized + debuginfo] target(s) in 2.40s
```

## Verification still recommended before commit

Before committing, rerun at least:

```bash
set -a; source "/Users/bilibili/Documents/workspace/jununfly/zbrain-rust/.env"; set +a; \
cargo build --manifest-path "/Users/bilibili/Documents/workspace/jununfly/zbrain-rust/Cargo.toml"
```

Then check final diff:

```bash
git -C "/Users/bilibili/Documents/workspace/jununfly/zbrain-rust" status --short --branch
git -C "/Users/bilibili/Documents/workspace/jununfly/zbrain-rust" diff --stat
git -C "/Users/bilibili/Documents/workspace/jununfly/zbrain-rust" diff -- \
  crates/zbrain-core/src/postgres.rs \
  crates/zbrain-core/tests/postgres_engine_page_crud.rs \
  docs/plans/20260526/21-session-state-74-pg-list-pages-source-filters.md
```

If code changed again after this handoff, also rerun:

```bash
set -a; source "/Users/bilibili/Documents/workspace/jununfly/zbrain-rust/.env"; set +a; \
cargo test --manifest-path "/Users/bilibili/Documents/workspace/jununfly/zbrain-rust/Cargo.toml" \
  --test postgres_engine_page_crud -- --test-threads=1

set -a; source "/Users/bilibili/Documents/workspace/jununfly/zbrain-rust/.env"; set +a; \
cargo test --manifest-path "/Users/bilibili/Documents/workspace/jununfly/zbrain-rust/Cargo.toml" \
  --workspace -- --test-threads=1

set -a; source "/Users/bilibili/Documents/workspace/jununfly/zbrain-rust/.env"; set +a; \
cargo clippy --manifest-path "/Users/bilibili/Documents/workspace/jununfly/zbrain-rust/Cargo.toml" \
  --workspace --all-targets -- -D warnings
```

## Notes for next slice selection

#74 covers only `source_id` and `source_ids` for PG `list_pages`.

Remaining PG `PageFilters` surface should stay explicit follow-up work, not be silently folded into #74:

```text
tag
offset
updated_after
slug_prefix
include_deleted
sort
```

Likely follow-up candidates from the plan pool:

```text
#75 — PG integration test isolation
#110-e — chunker_version TEXT historical-debt evaluation
#110-f — embedder / retrieval-tracker end-to-end contract
```
