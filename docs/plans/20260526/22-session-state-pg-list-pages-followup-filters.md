# Session State: PG list_pages follow-up filters

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans if continuing implementation from this state. Use superpowers:test-driven-development for any follow-up behavior changes. Use superpowers:verification-before-completion before claiming completion or committing.

**Goal:** Preserve the current project state for the PostgreSQL `list_pages` follow-up filters slice so a later session can resume, audit, or commit without re-discovering the context.

**Scope:** ZBrain Rust rewrite, branch `rust-rewrite`. This slice extends PostgreSQL `PostgresEngine::list_pages` beyond #74 source filters to cover the query-only subset:

```text
slug_prefix / updated_after / include_deleted / offset / sort
```

`tag` is intentionally split out because PostgreSQL does not yet have `page_tags` schema or tag CRUD behavior.

---

## Current repository state

Expected files for this slice:

```text
crates/zbrain-core/src/postgres.rs
crates/zbrain-core/tests/postgres_engine_page_crud.rs
docs/plans/20260526/22-session-state-pg-list-pages-followup-filters.md
```

Related but separate pending doc update from the previous #74 status request:

```text
docs/plans/20260526/16-slice-index-and-conventions.md
```

Non-slice formatting-only drift observed after `cargo fmt --all`:

```text
crates/zbrain-core/tests/libsql_engine_full_columns.rs
```

That file only had rustfmt layout changes. Prefer reverting it before commit if the goal is a narrow PG-only slice.

## Slice purpose

#74 already added PostgreSQL support for:

```rust
pub source_id: Option<String>,
pub source_ids: Option<Vec<String>>,
```

This follow-up slice adds PostgreSQL support for the remaining query-only `PageFilters` fields that can be implemented against the existing PG `pages` table:

```rust
pub slug_prefix: Option<String>,
pub updated_after: Option<String>,
pub include_deleted: bool,
pub sort: Option<PageSort>,
pub limit: Option<usize>,
pub offset: Option<usize>,
```

Existing behavior preserved:

1. `page_type` still filters by page type.
2. `source_id` still filters by exact source id.
3. `source_ids: Some(vec![])` still short-circuits to an empty result.
4. `source_ids` still uses PostgreSQL native array bind with `ANY($N::text[])`.

## Explicit tag split decision

`PageFilters::tag` is not part of this slice.

Reason: libsql implements tag filtering via `page_tags`:

```sql
JOIN page_tags AS pt ON pt.page_id = p.id
```

Current PostgreSQL migrations only provide `sources` and `pages` plus later `deleted_at` / full-column additions. There is no PG `page_tags` table and no PG tag CRUD override. Implementing `tag` here would silently mix a query-only list-pages slice with a schema and behavior slice.

Follow-up slice required:

```text
PG page_tags migration + tag CRUD/behavior + list_pages(tag)
```

Acceptance criteria for that future slice:

1. Add PG `page_tags` migration matching libsql semantics.
2. Implement PG tag CRUD behavior.
3. Add integration tests for tag add/remove/get and `list_pages(tag)`.
4. Only then add the `JOIN page_tags` path to PG `list_pages`.

## TDD path followed

### RED tests added first

New tests were added to:

```text
crates/zbrain-core/tests/postgres_engine_page_crud.rs
```

Tests added:

```rust
async fn list_pages_filters_by_slug_prefix()
async fn list_pages_filters_by_updated_after()
async fn list_pages_excludes_soft_deleted_by_default()
async fn list_pages_includes_soft_deleted_when_flag_set()
async fn list_pages_respects_offset()
async fn list_pages_sorts_by_slug_asc()
async fn list_pages_sorts_by_updated_desc_by_default()
```

Targeted RED command:

```bash
set -a; source "/Users/bilibili/Documents/workspace/jununfly/zbrain-rust/.env"; set +a; \
cargo test --manifest-path "/Users/bilibili/Documents/workspace/jununfly/zbrain-rust/Cargo.toml" \
  --test postgres_engine_page_crud list_pages_ -- --test-threads=1
```

Observed RED result before production change:

```text
running 12 tests
5 passed; 7 failed
```

Failing tests were the seven new follow-up filter tests, confirming PostgreSQL `list_pages` did not yet implement the required behavior.

### GREEN implementation

Production code changed in:

```text
crates/zbrain-core/src/postgres.rs
```

The PG list query now uses `pages AS p` and builds only active filter clauses.

Important helper shape:

```rust
fn push_filter_clause(sql: &mut String, param_idx: &mut u32, active: bool, clause: &str) {
    if active {
        let frag = format!(" AND {clause} ${param_idx}");
        sql.push_str(&frag);
        *param_idx += 1;
    }
}

fn build_list_pages_sql(filters: &PageFilters) -> Option<String> {
    if filters.source_ids.as_ref().is_some_and(Vec::is_empty) {
        return None;
    }

    let mut sql = format!("SELECT {FULL_PAGE_PROJECTION} FROM pages AS p WHERE 1=1");
    let mut param_idx: u32 = 1;

    push_filter_clause(&mut sql, &mut param_idx, filters.page_type.is_some(), "p.type =");
    push_filter_clause(&mut sql, &mut param_idx, filters.source_id.is_some(), "p.source_id =");
    push_filter_clause(&mut sql, &mut param_idx, filters.source_ids.is_some(), "p.source_id = ANY(");
    if filters.source_ids.is_some() {
        sql.push_str("::text[])");
    }
    push_filter_clause(&mut sql, &mut param_idx, filters.slug_prefix.is_some(), "p.slug LIKE");
    if filters.slug_prefix.is_some() {
        sql.push_str(" || '%'");
    }
    push_filter_clause(&mut sql, &mut param_idx, filters.updated_after.is_some(), "p.updated_at >");
    if filters.updated_after.is_some() {
        sql.push_str("::timestamptz");
    }
    if !filters.include_deleted {
        sql.push_str(" AND p.deleted_at IS NULL");
    }

    push_list_pages_sort(&mut sql, filters.sort.unwrap_or_default());
    push_list_pages_pagination(&mut sql, &mut param_idx, filters);
    Some(sql)
}
```

Sort behavior now mirrors the shared `PageSort` contract:

```rust
fn push_list_pages_sort(sql: &mut String, sort_mode: PageSort) {
    sql.push_str(" ORDER BY ");
    sql.push_str(page_sort_sql(sort_mode));
    if sort_mode != PageSort::Slug {
        sql.push_str(", p.slug ASC");
    }
}
```

Pagination behavior:

```rust
fn push_list_pages_pagination(sql: &mut String, param_idx: &mut u32, filters: &PageFilters) {
    if filters.limit.is_some() {
        let frag = format!(" LIMIT ${param_idx}");
        sql.push_str(&frag);
        *param_idx += 1;
    }
    if filters.offset.is_some() {
        let frag = format!(" OFFSET ${param_idx}");
        sql.push_str(&frag);
    }
}
```

Bind order remains an explicit contract:

```rust
// ORDER CONTRACT: bind order must match `param_idx` advancement in
// `build_list_pages_sql`: page_type → source_id → source_ids →
// slug_prefix → updated_after → limit → offset. Reordering either side
// silently misbinds PG `$N`.
```

Actual bind order:

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
if let Some(prefix) = filters.slug_prefix.as_deref() {
    query = query.bind(prefix);
}
if let Some(cutoff) = filters.updated_after.as_deref() {
    query = query.bind(cutoff);
}
if let Some(limit) = filters.limit {
    query = query.bind(i64::try_from(limit).unwrap_or(i64::MAX));
}
if let Some(offset) = filters.offset {
    query = query.bind(i64::try_from(offset).unwrap_or(i64::MAX));
}
```

## Semantics covered

### `slug_prefix`

SQL:

```sql
p.slug LIKE $N || '%'
```

Behavior: prefix match on `slug`.

### `updated_after`

SQL:

```sql
p.updated_at > $N::timestamptz
```

Behavior: strict greater-than cutoff, matching libsql semantics.

### `include_deleted`

Default SQL adds:

```sql
AND p.deleted_at IS NULL
```

When `include_deleted: true`, that clause is omitted and soft-deleted rows can appear in list results.

### `sort`

Default:

```rust
PageSort::UpdatedDesc
```

Shared helper:

```rust
page_sort_sql(sort_mode)
```

Tie-breaker: for non-`Slug` sort modes, append:

```sql
, p.slug ASC
```

### `offset`

PostgreSQL uses direct offset:

```sql
OFFSET $N
```

No SQLite/libsql `LIMIT -1` sentinel is needed.

## Issues encountered and resolved

### Existing source_ids test assumed old insertion ordering

After aligning default sort to `PageSort::UpdatedDesc`, the existing #74 `list_pages_filters_by_source_ids` test failed because it implicitly expected old insertion/id ordering.

Resolution: that test now explicitly requests `PageSort::Slug`, because its purpose is source filtering, not default-sort validation.

### `cargo fmt --manifest-path` is not the right command

The command:

```bash
cargo fmt --manifest-path "/Users/bilibili/Documents/workspace/jununfly/zbrain-rust/Cargo.toml"
```

reported:

```text
Failed to find targets
```

Resolution: use workspace formatting:

```bash
cargo fmt --all --manifest-path "/Users/bilibili/Documents/workspace/jununfly/zbrain-rust/Cargo.toml"
```

### Strict clippy rejected `list_pages` length

Targeted clippy failed with:

```text
error: this function has too many lines (112/100)
```

Resolution: no `#[allow]` escape hatch. SQL construction was extracted into small helpers while keeping behavior unchanged:

```rust
push_filter_clause
build_list_pages_sql
push_list_pages_sort
push_list_pages_pagination
```

## Verification already performed

Commands were run with:

```bash
set -a; source "/Users/bilibili/Documents/workspace/jununfly/zbrain-rust/.env"; set +a
```

### Targeted follow-up list_pages tests

```bash
cargo test --manifest-path "/Users/bilibili/Documents/workspace/jununfly/zbrain-rust/Cargo.toml" \
  --test postgres_engine_page_crud list_pages_ -- --test-threads=1
```

Latest targeted result after refactor:

```text
running 12 tests
12 passed; 0 failed
```

### Targeted zbrain-core clippy

```bash
cargo clippy --manifest-path "/Users/bilibili/Documents/workspace/jununfly/zbrain-rust/Cargo.toml" \
  -p zbrain-core --all-targets -- -D warnings
```

Latest targeted result:

```text
Finished `dev` profile
Exit Code: 0
```

### Full verification

Background task:

```text
task_id: stpaws
Status: completed
Duration: 36s
```

Command:

```bash
cargo build --manifest-path "/Users/bilibili/Documents/workspace/jununfly/zbrain-rust/Cargo.toml" && \
cargo test --manifest-path "/Users/bilibili/Documents/workspace/jununfly/zbrain-rust/Cargo.toml" --workspace -- --test-threads=1 && \
cargo clippy --manifest-path "/Users/bilibili/Documents/workspace/jununfly/zbrain-rust/Cargo.toml" --workspace --all-targets -- -D warnings
```

Filtered output showed:

1. build completed;
2. workspace tests completed with `test result: ok` groups;
3. clippy completed with `Finished dev profile`;
4. no `error`, `FAILED`, or `failures:` lines.

Before final completion/commit, run fresh verification again in the current turn.

## Commit checklist

Before committing:

1. Revert or consciously include the formatting-only diff in:

   ```text
   crates/zbrain-core/tests/libsql_engine_full_columns.rs
   ```

2. Inspect final diff:

   ```bash
   git -C "/Users/bilibili/Documents/workspace/jununfly/zbrain-rust" diff --stat
   git -C "/Users/bilibili/Documents/workspace/jununfly/zbrain-rust" diff -- crates/zbrain-core/src/postgres.rs crates/zbrain-core/tests/postgres_engine_page_crud.rs docs/plans/20260526/22-session-state-pg-list-pages-followup-filters.md
   ```

3. Run fresh verification:

   ```bash
   set -a; source "/Users/bilibili/Documents/workspace/jununfly/zbrain-rust/.env"; set +a; \
   cargo build --manifest-path "/Users/bilibili/Documents/workspace/jununfly/zbrain-rust/Cargo.toml" && \
   cargo test --manifest-path "/Users/bilibili/Documents/workspace/jununfly/zbrain-rust/Cargo.toml" --workspace -- --test-threads=1 && \
   cargo clippy --manifest-path "/Users/bilibili/Documents/workspace/jununfly/zbrain-rust/Cargo.toml" --workspace --all-targets -- -D warnings
   ```

4. Stage the intended files only.
5. Commit without tag unless the user explicitly asks for a tag.

Suggested commit message:

```text
slice: add PG list_pages follow-up filters
```
