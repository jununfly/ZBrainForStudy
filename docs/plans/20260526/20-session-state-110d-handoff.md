# Session State: #110-d Libsql TS Contract Alignment Handoff

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans if continuing implementation from this state. Use superpowers:verification-before-completion before claiming completion or committing.

**Goal:** Preserve the current project state so the next session can resume without re-discovering #110-d context.

**Scope:** ZBrain Rust rewrite, branch `rust-rewrite`, slice #110-d. This document is a handoff/state snapshot, not a new implementation plan.

---

## Current repository state

Working tree snapshot at handoff:

```text
 M crates/zbrain-core/src/libsql.rs
A  crates/zbrain-core/tests/libsql_engine_full_columns.rs
A  docs/plans/20260526/19-session-state-110d-libsql-ts-contract-alignment.md
?? .codegraph/
```

Branch:

```text
rust-rewrite
```

Important staging state:

```text
staged:
  crates/zbrain-core/tests/libsql_engine_full_columns.rs
  docs/plans/20260526/19-session-state-110d-libsql-ts-contract-alignment.md

unstaged:
  crates/zbrain-core/src/libsql.rs

untracked:
  .codegraph/
  docs/plans/20260526/20-session-state-110d-handoff.md
```

Do not accidentally stage `.codegraph/`.

## Completed before this handoff

### #110-h

Completed and committed:

```text
35eeb33 slice #110-h: isolate libsql init_schema flake
```

Context:

- Native SIGSEGV/SIGABRT appeared around libsql `init_schema` under libtest multi-runtime concurrency.
- The selected mitigation was test-level serial isolation with `#[serial_test::serial]` for the affected libsql tests.
- Treat future `init_schema` native flake as #110-h follow-up unless it is deterministically tied to #110-d changes.

### User-requested TODO comment

User requested adding this Chinese TODO to `crates/zbrain-core/src/libsql.rs`:

```rust
/// TODO：单例中单线程，借助线程的消息循环序列化所有对数据库的读写操作，避免竞态问题。
```

Current state:

- The TODO exists in `libsql.rs`.
- It is not staged for #110-d.
- Decide separately whether to commit it alone, include it with #110-d, or leave it for a later slice. It is not part of the #110-d contract-test change unless explicitly agreed.

## #110-d purpose

#110-d aligns the libsql backend with PG #110-c / TS PGLite page-contract semantics.

Source-of-truth TS semantics:

```ts
const frontmatter = page.frontmatter || {};
const sourceKind = page.source_kind ?? null;
const sourceUri = page.source_uri ?? null;
const ingestedVia = page.ingested_via ?? null;
const ingestedAt = (sourceKind || sourceUri || ingestedVia)
  ? new Date().toISOString()
  : null;
```

Target contracts locked by the new libsql tests:

1. `put_page` does not persist caller-provided `embedding`.
2. `put_page` does not persist caller-provided `last_retrieved_at`.
3. `ingested_at` is server-stamped when any one of `source_kind` / `source_uri` / `ingested_via` exists.
4. Caller-provided `input.ingested_at` is ignored when provenance exists.
5. Without provenance, `ingested_at` remains `None`.
6. Omitted `frontmatter` round-trips as `{}`.
7. SQLite `frontmatter` column is `TEXT NOT NULL DEFAULT '{}'`.
8. SQLite `corpus_generation` column is `TEXT` and Rust decodes it as `Option<String>`.

## New #110-d files

### `docs/plans/20260526/19-session-state-110d-libsql-ts-contract-alignment.md`

Status: staged.

Purpose:

- TDD implementation plan for #110-d.
- Captures TS source-of-truth contract, PG template caveat, task breakdown, test commands, and commit discipline.

### `crates/zbrain-core/tests/libsql_engine_full_columns.rs`

Status: staged.

Purpose:

- Libsql-specific characterization/regression test module for full page-column contracts.
- The implementation was mostly already aligned, so this slice is primarily test locking rather than production change.

Important review-driven strengthening:

- Initial test only covered all provenance fields together.
- Code review identified that TS contract is `OR`, not `AND`.
- The test was strengthened to cover each single provenance field independently:

```rust
let cases = [
    ("source-kind-only", Some("file".to_string()), None, None),
    ("source-uri-only", None, Some("file:///tmp/contract.md".to_string()), None),
    ("ingested-via-only", None, None, Some("cli".to_string())),
];
```

- A temporary incorrect `&&` implementation in `libsql.rs` was used to prove the strengthened test goes RED.
- The production code was restored to the correct `||` implementation afterward.

## Production code status

### `crates/zbrain-core/src/libsql.rs`

Status: modified, unstaged.

Relevant correct production logic remains:

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

Do not change this to `&&`. The new #110-d tests are expected to catch that regression.

Current unstaged diff should be only the user-requested TODO comment, not a #110-d semantic change.

## Verification already performed before handoff

The session performed focused and workspace-level validation before this handoff. Key evidence recorded in conversation:

- New libsql full-column test module passed.
- Focused libsql suites passed.
- Workspace `cargo build`, `cargo test --workspace`, and `cargo clippy --workspace --all-targets -- -D warnings` passed after fixing clippy `doc_markdown` comments.
- Review gap around single-provenance `ingested_at` coverage was fixed.
- A deliberate temporary `&&` bug proved the single-provenance tests fail for the wrong implementation, then production was restored to `||`.

Before committing in a later session, rerun verification rather than relying only on this handoff.

Recommended command set from repo root:

```bash
cargo test -p zbrain-core --test libsql_engine_full_columns -- --test-threads=1
cargo test -p zbrain-core --test libsql_engine_page_crud -- --test-threads=1
cargo test -p zbrain-core --test libsql_engine_lifecycle -- --test-threads=1
cargo build
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
```

If running outside repo root, use:

```bash
cargo test --manifest-path /Users/bilibili/Documents/workspace/jununfly/zbrain-rust/Cargo.toml -p zbrain-core --test libsql_engine_full_columns -- --test-threads=1
cargo build --manifest-path /Users/bilibili/Documents/workspace/jununfly/zbrain-rust/Cargo.toml
cargo test --manifest-path /Users/bilibili/Documents/workspace/jununfly/zbrain-rust/Cargo.toml --workspace
cargo clippy --manifest-path /Users/bilibili/Documents/workspace/jununfly/zbrain-rust/Cargo.toml --workspace --all-targets -- -D warnings
```

## PG template caveat

Do not blindly copy `postgres_engine_full_columns.rs::roundtrip_all_full_columns` for `ingested_at`.

Known suspicious legacy assertion:

```rust
assert_eq!(
    page.ingested_at.as_deref(),
    Some("2026-05-30T00:00:00+00:00"),
    "ingested_at (client-provided)"
);
```

This conflicts with the TS PGLite source-of-truth and with the explicit server-stamp behavior. #110-d intentionally does not copy it.

## Immediate next actions

### Option A: Commit #110-d only

Use this if keeping the user-requested TODO separate.

```bash
git status --short
git diff --cached --stat
git diff -- crates/zbrain-core/src/libsql.rs
cargo test -p zbrain-core --test libsql_engine_full_columns -- --test-threads=1
cargo clippy --workspace --all-targets -- -D warnings
git commit -m "test: align libsql page contracts with TS"
```

Expected commit contents:

```text
crates/zbrain-core/tests/libsql_engine_full_columns.rs
docs/plans/20260526/19-session-state-110d-libsql-ts-contract-alignment.md
```

Do not stage:

```text
.codegraph/
crates/zbrain-core/src/libsql.rs
```

unless explicitly choosing Option B.

### Option B: Include TODO in the #110-d commit

Only use if user explicitly agrees that the TODO comment belongs in the same commit.

```bash
git add crates/zbrain-core/src/libsql.rs
git commit -m "test: align libsql page contracts with TS"
```

Risk:

- This mixes a concurrency TODO note into a TS contract-test slice. It is small, but it weakens slice purity.

### Option C: Commit TODO separately

Use this if the TODO should be preserved as a separate housekeeping/documentation commit.

```bash
git add crates/zbrain-core/src/libsql.rs
git commit -m "docs: note libsql single-thread serialization TODO"
```

## Non-negotiable guardrails

- Keep TDD discipline: no production semantic change without a RED test.
- Do not silently accept critical drift as an accepted deviation.
- Do not stage `.codegraph/`.
- Keep #110-d focused on libsql TS contract alignment.
- If `init_schema` native flake reappears, isolate it under #110-h unless #110-d changes are the deterministic trigger.
