# PG Embed Integration Test Infrastructure Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** Replace all `ZBRAIN_TEST_PG_URL` env-gated PG integration tests with pg-embed ephemeral instance fixtures, achieving zero-config PG contract testing on every machine and CI runner.

**Architecture:** Introduce `pg-embed` v1.0.0 as a dev-dependency in `zbrain-core`. Create a `tests/support/pg_fixture.rs` module that provides a `PgFixture` struct: it downloads (cached) and starts a PG instance per test, creates an isolated database, runs `init_schema()`, and returns both a `PostgresEngine` and the database URL. Each test gets its own PG process + datadir; RAII drop kills the process and deletes the datadir. Replace every `pg_url() / ZBRAIN_TEST_PG_URL` gate with a `PgFixture::start().await` call. Remove all `#[serial_test::serial]` annotations (114 hits across 16 files) since independent instances eliminate the shared-table contention that required serialization. Finally, remove `serial_test` from workspace dev-dependencies.

**Tech Stack:** Rust, `pg-embed` v1.0.0 (PG 10–18 supported, MSRV 1.88), `sqlx` 0.8, `tokio`, `tempfile`.

---

## Non-negotiable constraints

- TDD rule: **NO PRODUCTION CODE WITHOUT A FAILING TEST FIRST**. However, this plan is *test infrastructure*, not production code. The "RED" phase means: write the fixture code, wire it into a test, run the test, and confirm the test *passes against the real PG instance*. If a test previously skipped (no `ZBRAIN_TEST_PG_URL`), it now passes — that is the GREEN signal.
- Worktree root:
  - `/Users/bilibili/Documents/workspace/jununfly/zbrain-rust`
- Cargo manifest:
  - `/Users/bilibili/Documents/workspace/jununfly/zbrain-rust/Cargo.toml`
- Do not amend prior implementation commits. Create new commits.
- Do not silently treat PG integration skips as PG infra completion:
  - `skipping: ZBRAIN_TEST_PG_URL unset` is a skip, not a pass. After migration, tests must actually run and pass.
- `serial_test` removal: every `#[serial_test::serial]` annotation must be deleted; `serial_test` must be removed from workspace `Cargo.toml` `[workspace.dev-dependencies]`.
- `pg-embed` must be a dev-dependency only — never a production dependency.
- Each test must get its own PG instance (B2 strategy). No shared instance, no `serial_test`.

## Current state

- 16 test files contain `ZBRAIN_TEST_PG_URL` gating (151 total references).
- 16 test files contain `#[serial_test::serial]` annotations (114 total references).
- `serial_test = "3"` in workspace `Cargo.toml` `[workspace.dev-dependencies]` with Slice #110-g comment.
- Source file `src/postgres.rs` references `ZBRAIN_TEST_PG_URL` only in a comment (line 254) — no production code change needed.
- Local machine: `docker not found` — pg-embed is the correct zero-config choice.

## Files

### Dev-dependency changes

- Modify: `/Users/bilibili/Documents/workspace/jununfly/zbrain-rust/Cargo.toml` (add `pg-embed`, later remove `serial_test`)
- Modify: `/Users/bilibili/Documents/workspace/jununfly/zbrain-rust/crates/zbrain-core/Cargo.toml` (add `pg-embed` dev-dep, later remove `serial_test` dev-dep)

### New fixture module

- Create: `/Users/bilibili/Documents/workspace/jununfly/zbrain-rust/crates/zbrain-core/tests/support/mod.rs`
- Create: `/Users/bilibili/Documents/workspace/jununfly/zbrain-rust/crates/zbrain-core/tests/support/pg_fixture.rs`

### Migrated test files (16 files)

Phase A-1 (lifecycle PoC — 1 file):
- Modify: `/Users/bilibili/Documents/workspace/jununfly/zbrain-rust/crates/zbrain-core/tests/postgres_engine_lifecycle.rs`

Phase A-2 (remaining postgres_engine — 2 files):
- Modify: `/Users/bilibili/Documents/workspace/jununfly/zbrain-rust/crates/zbrain-core/tests/postgres_engine_page_crud.rs`
- Modify: `/Users/bilibili/Documents/workspace/jununfly/zbrain-rust/crates/zbrain-core/tests/postgres_engine_full_columns.rs`

Phase A-3 (page_methods — 13 files):
- Modify: `/Users/bilibili/Documents/workspace/jununfly/zbrain-rust/crates/zbrain-core/tests/page_methods_find_duplicate_page.rs`
- Modify: `/Users/bilibili/Documents/workspace/jununfly/zbrain-rust/crates/zbrain-core/tests/page_methods_find_orphan_pages.rs`
- Modify: `/Users/bilibili/Documents/workspace/jununfly/zbrain-rust/crates/zbrain-core/tests/page_methods_get_all_slugs.rs`
- Modify: `/Users/bilibili/Documents/workspace/jununfly/zbrain-rust/crates/zbrain-core/tests/page_methods_get_effective_dates.rs`
- Modify: `/Users/bilibili/Documents/workspace/jununfly/zbrain-rust/crates/zbrain-core/tests/page_methods_get_page_timestamps.rs`
- Modify: `/Users/bilibili/Documents/workspace/jununfly/zbrain-rust/crates/zbrain-core/tests/page_methods_get_salience_scores.rs`
- Modify: `/Users/bilibili/Documents/workspace/jununfly/zbrain-rust/crates/zbrain-core/tests/page_methods_list_all_page_refs.rs`
- Modify: `/Users/bilibili/Documents/workspace/jununfly/zbrain-rust/crates/zbrain-core/tests/page_methods_purge_deleted_pages.rs`
- Modify: `/Users/bilibili/Documents/workspace/jununfly/zbrain-rust/crates/zbrain-core/tests/page_methods_refresh_page_body.rs`
- Modify: `/Users/bilibili/Documents/workspace/jununfly/zbrain-rust/crates/zbrain-core/tests/page_methods_restore_page.rs`
- Modify: `/Users/bilibili/Documents/workspace/jununfly/zbrain-rust/crates/zbrain-core/tests/page_methods_salience_scores_with_takes.rs`
- Modify: `/Users/bilibili/Documents/workspace/jununfly/zbrain-rust/crates/zbrain-core/tests/page_methods_soft_delete_page.rs`
- Modify: `/Users/bilibili/Documents/workspace/jununfly/zbrain-rust/crates/zbrain-core/tests/page_methods_update_cr_state.rs`

---

## Task 1: Add pg-embed dev-dependency + create PgFixture module (A-1 setup)

**Files:**

- Modify: `/Users/bilibili/Documents/workspace/jununfly/zbrain-rust/Cargo.toml`
- Modify: `/Users/bilibili/Documents/workspace/jununfly/zbrain-rust/crates/zbrain-core/Cargo.toml`
- Create: `/Users/bilibili/Documents/workspace/jununfly/zbrain-rust/crates/zbrain-core/tests/support/mod.rs`
- Create: `/Users/bilibili/Documents/workspace/jununfly/zbrain-rust/crates/zbrain-core/tests/support/pg_fixture.rs`

**Step 0: Bump workspace MSRV to 1.88**

pg-embed v1.0.0 requires MSRV 1.88. Update in workspace `Cargo.toml`:

```toml
rust-version = "1.88"
```

(Replace `rust-version = "1.82"`.)

**Step 1: Add pg-embed to workspace Cargo.toml**

Add to `[workspace.dev-dependencies]`:

```toml
pg-embed = { version = "1.0", features = ["rt_tokio_migrate"] }
```

> **Note:** Use `rt_tokio_migrate` (not `rt_tokio`) to get `PgEmbed::create_database` / `drop_database` / `migrate` methods. Only `rt_tokio` gives bare start/stop without DB management helpers.

**Step 2: Add pg-embed to zbrain-core dev-dependencies**

Add to `crates/zbrain-core/Cargo.toml` `[dev-dependencies]`:

```toml
pg-embed = { workspace = true }
```

**Step 3: Create `tests/support/mod.rs`**

```rust
pub mod pg_fixture;
```

**Step 4: Create `tests/support/pg_fixture.rs`**

```rust
//! Ephemeral PostgreSQL fixture for integration tests.
//!
//! Each call to [`PgFixture::start`] launches a fresh `pg-embed` PostgreSQL
//! instance with an isolated data directory. On drop the process is killed
//! and the data directory is cleaned up (persistent=false). No external
//! PostgreSQL or Docker installation is required — pg-embed downloads a
//! pre-compiled binary on first use (cached thereafter).
//!
//! # Port allocation
//!
//! We bind a `TcpListener` to `127.0.0.1:0` to let the OS pick a free port,
//! then drop the listener and pass that port number to pg-embed. This avoids
//! the race condition where pg-embed's `port: 0` is passed literally to
//! `pg_ctl` without dynamic allocation.

use std::net::TcpListener;
use std::path::PathBuf;

use pg_embed::postgres::{PgEmbed, PgSettings};
use pg_embed::pg_fetch::{PgFetchSettings, PG_V17};
use sqlx::Executor;
use zbrain_core::engine::{BrainEngine, EngineConfig};
use zbrain_core::postgres::PostgresEngine;

/// RAII fixture that owns a running `pg-embed` PostgreSQL instance.
///
/// Provides a [`PostgresEngine`] that has already been `connect()`-ed and
/// `init_schema()`-ed, ready for test assertions.
pub struct PgFixture {
    /// The pg-embed instance. Kept alive so the PG process stays running.
    _pg: PgEmbed,
    /// The engine exposed to the test.
    pub engine: PostgresEngine,
    /// The database URL for direct SQL access if needed.
    pub url: String,
}

impl PgFixture {
    /// Start a fresh PostgreSQL instance, create an isolated database,
    /// connect a `PostgresEngine` to it, and run `init_schema()`.
    pub async fn start() -> Self {
        let db_name = format!(
            "zbrain_test_{}_{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_millis()
        );

        // Allocate a free port via the OS.
        let listener = TcpListener::bind("127.0.0.1:0")
            .expect("bind to find free port");
        let port = listener.local_addr().unwrap().port();
        drop(listener); // release before pg-embed binds

        // Temp dir for PG data; will be cleaned up by pg-embed (persistent=false).
        let database_dir = PathBuf::from(format!("/tmp/pg_embed_{db_name}"));

        let pg_settings = PgSettings {
            database_dir,
            port,
            user: "postgres".to_string(),
            password: "postgres".to_string(),
            auth_method: pg_embed::postgres::PgAuthMethod::Plain,
            persistent: false,
            timeout: Some(std::time::Duration::from_secs(30)),
            migration_dir: None,
        };

        let fetch_settings = PgFetchSettings {
            version: PG_V17,
            ..Default::default()
        };

        let mut pg = PgEmbed::new(pg_settings, fetch_settings)
            .await
            .expect("pg-embed init failed");

        // Download PG binary + run initdb (cached after first download).
        pg.setup().await.expect("pg-embed setup failed");

        pg.start_db().await.expect("pg-embed start_db failed");

        // Create the test database via pg-embed helper.
        pg.create_database(&db_name)
            .await
            .expect("create test database");

        let url = pg.full_db_uri(&db_name);

        // Connect PostgresEngine to the fresh database.
        let engine = PostgresEngine::new();
        let cfg = EngineConfig {
            database_url: Some(url.clone()),
            database_path: None,
        };
        engine.connect(&cfg).await.expect("PostgresEngine connect");
        engine.init_schema().await.expect("init_schema");

        Self {
            _pg: pg,
            engine,
            url,
        }
    }
}

impl Drop for PgFixture {
    fn drop(&mut self) {
        // Disconnect engine gracefully.
        // SAFETY: Drop may be called inside an async runtime. Using
        // `block_on` from the current handle would panic. Instead we
        // spawn a new minimal runtime — but only if we're NOT already
        // inside a tokio runtime. If we are, we rely on the engine's
        // own Drop (which should handle disconnect internally) or let
        // the connection drop naturally when the process exits.
        let engine = std::mem::replace(&mut self.engine, PostgresEngine::new());
        if tokio::runtime::Handle::try_current().is_err() {
            // Not inside a tokio runtime — safe to create one.
            let rt = tokio::runtime::Runtime::new().expect("create drop runtime");
            let _ = rt.block_on(engine.disconnect());
        }
        // If inside a tokio runtime, we skip block_on to avoid panic.
        // The engine connection will be cleaned up when the process exits.
        // pg-embed with persistent=false handles its own cleanup.
    }
}
```

**Step 5: Wire `support` module into the test harness**

Each integration test file that wants to use `PgFixture` will add:

```rust
mod support;
```

at the top of the file. This makes `support::pg_fixture::PgFixture` available.

**Step 6: Verify compilation**

Run: `cargo build -p zbrain-core --tests 2>&1 | tail -20`
Expected: Compiles successfully (may have unused import warnings — that's fine).

**Step 7: Commit**

```bash
git add Cargo.toml crates/zbrain-core/Cargo.toml crates/zbrain-core/tests/support/
git commit -m "feat(test): add pg-embed dev-dep and PgFixture module (A-1 setup)"
```

---

## Task 2: Migrate `postgres_engine_lifecycle.rs` to PgFixture (A-1 PoC)

**Files:**

- Modify: `/Users/bilibili/Documents/workspace/jununfly/zbrain-rust/crates/zbrain-core/tests/postgres_engine_lifecycle.rs`

This is the proof-of-concept file. It has 6 `ZBRAIN_TEST_PG_URL` references and no `#[serial_test::serial]` annotations. After this task, all PG tests in this file will run against a real PG instance without any env-var gating.

**Step 1: Replace `pg_url()` gate with `PgFixture`**

The current pattern in each PG-dependent test is:

```rust
let Some(url) = pg_url() else {
    eprintln!("skipping: ZBRAIN_TEST_PG_URL unset");
    return;
};
```

Replace with:

```rust
let fix = support::pg_fixture::PgFixture::start().await;
let url = fix.url.clone();
let engine = &fix.engine;
```

For `kind_reports_postgres()` — no PG needed, keep as-is.

For `connect_without_url_errors()` — no PG needed, keep as-is.

For `connect_succeeds_against_live_postgres()`, `init_schema_creates_pages_and_sources_tables()`, `init_schema_is_idempotent()` — rewrite to use `PgFixture`.

**New file content for `postgres_engine_lifecycle.rs`:**

```rust
//! Slice 4a — `PostgresEngine` lifecycle integration tests.
//!
//! Uses `pg-embed` to launch an ephemeral PostgreSQL instance per test.
//! No external PostgreSQL or Docker installation is required.

mod support;

use zbrain_core::engine::{BrainEngine, EngineConfig, EngineKind};
use zbrain_core::postgres::PostgresEngine;

#[tokio::test]
async fn kind_reports_postgres() {
    let engine = PostgresEngine::new();
    assert_eq!(engine.kind(), EngineKind::Postgres);
}

#[tokio::test]
async fn connect_succeeds_against_live_postgres() {
    let fix = support::pg_fixture::PgFixture::start().await;
    // PgFixture already connected and init_schema'd.
    // Verify disconnect works.
    let engine = std::mem::replace(&mut fix.engine, PostgresEngine::new());
    engine.disconnect().await.expect("disconnect should succeed");
}

#[tokio::test]
async fn connect_without_url_errors() {
    let engine = PostgresEngine::new();
    let cfg = EngineConfig::default();
    let result = engine.connect(&cfg).await;
    assert!(
        result.is_err(),
        "connect without database_url must error, got {result:?}"
    );
}

#[tokio::test]
async fn init_schema_creates_pages_and_sources_tables() {
    let fix = support::pg_fixture::PgFixture::start().await;
    // init_schema already called by PgFixture. Verify tables exist.
    let pool = sqlx::postgres::PgPoolOptions::new()
        .max_connections(1)
        .connect(&fix.url)
        .await
        .expect("verification pool");

    let pages_exists: (bool,) = sqlx::query_as(
        "SELECT EXISTS (SELECT 1 FROM information_schema.tables WHERE table_name = 'pages')",
    )
    .fetch_one(&pool)
    .await
    .expect("pages table existence check");
    assert!(pages_exists.0, "pages table must exist after init_schema");

    let sources_exists: (bool,) = sqlx::query_as(
        "SELECT EXISTS (SELECT 1 FROM information_schema.tables WHERE table_name = 'sources')",
    )
    .fetch_one(&pool)
    .await
    .expect("sources table existence check");
    assert!(
        sources_exists.0,
        "sources table must exist after init_schema"
    );

    let default_source: (String,) = sqlx::query_as("SELECT id FROM sources WHERE id = 'default'")
        .fetch_one(&pool)
        .await
        .expect("default source row must be seeded");
    assert_eq!(default_source.0, "default");

    pool.close().await;
}

#[tokio::test]
async fn init_schema_is_idempotent() {
    let fix = support::pg_fixture::PgFixture::start().await;
    fix.engine
        .init_schema()
        .await
        .expect("second init_schema must be a no-op");
}
```

**Step 2: Run the tests**

Run: `cargo test -p zbrain-core --test postgres_engine_lifecycle 2>&1 | tail -30`
Expected: All 5 tests pass (first run will be slow as pg-embed downloads PG binary; subsequent runs use cache).

**Step 3: Commit**

```bash
git add crates/zbrain-core/tests/postgres_engine_lifecycle.rs
git commit -m "refactor(test): migrate postgres_engine_lifecycle to pg-embed fixture (A-1 PoC)"
```

---

## Task 3: Migrate `postgres_engine_page_crud.rs` to PgFixture + remove `#[serial]` (A-2a)

**Files:**

- Modify: `/Users/bilibili/Documents/workspace/jununfly/zbrain-rust/crates/zbrain-core/tests/postgres_engine_page_crud.rs`

This is the largest single file (36 `ZBRAIN_TEST_PG_URL` references, ~30 `#[serial_test::serial]` annotations).

**Step 1: Add `mod support;` at top of file**

**Step 2: Remove `pg_url()` helper function entirely**

**Step 3: Replace every PG-gated block**

Current pattern:

```rust
#[serial_test::serial]
#[tokio::test]
async fn some_test() {
    let Some(url) = pg_url() else {
        eprintln!("skipping: ZBRAIN_TEST_PG_URL unset");
        return;
    };
    // ... test body using `url` ...
}
```

New pattern:

```rust
#[tokio::test]
async fn some_test() {
    let fix = support::pg_fixture::PgFixture::start().await;
    // ... test body using `fix.engine` and `fix.url` ...
}
```

Key changes per test:
- Delete `#[serial_test::serial]` line
- Replace `let Some(url) = pg_url() else { ... return; };` with `let fix = support::pg_fixture::PgFixture::start().await;`
- Replace `PostgresEngine::new()` + `connect(url)` + `init_schema()` with `fix.engine` (already connected + schema'd)
- Replace `engine` references with `fix.engine` or rebind: `let engine = &fix.engine;`
- Remove explicit `disconnect()` calls at end (handled by `PgFixture::drop`)

**Step 4: Run the tests**

Run: `cargo test -p zbrain-core --test postgres_engine_page_crud 2>&1 | tail -30`
Expected: All tests pass, no `serial_test` involved.

**Step 5: Commit**

```bash
git add crates/zbrain-core/tests/postgres_engine_page_crud.rs
git commit -m "refactor(test): migrate postgres_engine_page_crud to pg-embed, remove serial (A-2a)"
```

---

## Task 4: Migrate `postgres_engine_full_columns.rs` to PgFixture + remove `#[serial]` (A-2b)

**Files:**

- Modify: `/Users/bilibili/Documents/workspace/jununfly/zbrain-rust/crates/zbrain-core/tests/postgres_engine_full_columns.rs`

**Step 1: Same pattern as Task 3**

- Add `mod support;`
- Remove `pg_url()` helper
- Replace every `#[serial_test::serial]` + `pg_url()` gate with `PgFixture::start().await`
- Remove explicit `disconnect()` calls

**Step 2: Run the tests**

Run: `cargo test -p zbrain-core --test postgres_engine_full_columns 2>&1 | tail -30`
Expected: All tests pass.

**Step 3: Commit**

```bash
git add crates/zbrain-core/tests/postgres_engine_full_columns.rs
git commit -m "refactor(test): migrate postgres_engine_full_columns to pg-embed, remove serial (A-2b)"
```

---

## Task 5: Migrate all 13 `page_methods_*.rs` files to PgFixture + remove `#[serial]` (A-3)

**Files:**

- Modify all 13 files listed in the Files section above under Phase A-3.

These files have a mixed structure: each file contains both **libsql** tests (using `init_clean_engine()` which returns a `LibsqlEngine`) and **PG** tests (using `pg_url()` gate). Only the PG tests need migration; libsql tests are untouched.

**Step 1: Apply the same migration pattern to each file**

For each of the 13 files:

1. Add `mod support;` at top (if not already present)
2. Remove `pg_url()` helper function (only the one in this file)
3. For each PG test function:
   - Delete `#[serial_test::serial]` annotation
   - Replace `let Some(url) = pg_url() else { ... return; };` with `let fix = support::pg_fixture::PgFixture::start().await;`
   - Replace manual `PostgresEngine::new()` + `connect(url)` + `init_schema()` with `fix.engine` usage
   - Remove explicit `disconnect()` calls at end

**Important:** Each file defines its own local `pg_url()` function. Remove the function definition from each file. The `mod support;` declaration gives access to the shared fixture.

**Step 2: Run all page_methods tests**

Run: `cargo test -p zbrain-core --test page_methods 2>&1 | tail -30`
Expected: All tests pass (both libsql and PG variants).

**Step 3: Commit**

```bash
git add crates/zbrain-core/tests/page_methods_*.rs
git commit -m "refactor(test): migrate all page_methods_* to pg-embed, remove serial (A-3)"
```

---

## Task 6: Remove `serial_test` from workspace + cleanup (A-4)

**Files:**

- Modify: `/Users/bilibili/Documents/workspace/jununfly/zbrain-rust/Cargo.toml`
- Modify: `/Users/bilibili/Documents/workspace/jununfly/zbrain-rust/crates/zbrain-core/Cargo.toml`

**Step 1: Verify no remaining `serial_test` references**

Run: `grep -rn "serial_test\|#\[serial\]" crates/zbrain-core/tests/`
Expected: No output (zero hits).

If hits remain, fix them before proceeding.

**Step 2: Verify no remaining `ZBRAIN_TEST_PG_URL` in tests**

Run: `grep -rn "ZBRAIN_TEST_PG_URL" crates/zbrain-core/tests/`
Expected: No output (zero hits).

**Step 3: Remove `serial_test` from workspace Cargo.toml**

Remove this line from `[workspace.dev-dependencies]`:

```toml
serial_test = "3"  # Slice #110-g: forces PG tests to run serially (shared `pages` table)
```

**Step 4: Remove `serial_test` from zbrain-core Cargo.toml**

Remove from `[dev-dependencies]`:

```toml
serial_test = { workspace = true }
```

**Step 5: Verify clean build + all tests**

Run: `cargo build -p zbrain-core --tests 2>&1 | tail -10`
Expected: Compiles without errors.

Run: `cargo test -p zbrain-core 2>&1 | tail -20`
Expected: All tests pass (both libsql and PG).

**Step 6: Commit**

```bash
git add Cargo.toml crates/zbrain-core/Cargo.toml
git commit -m "chore: remove serial_test dependency after pg-embed migration (A-4)"
```

---

## Task 7: Update docs + final validation (A-4 continued)

**Files:**

- Modify: `/Users/bilibili/Documents/workspace/jununfly/zbrain-rust/crates/zbrain-core/src/postgres.rs` (update comment on line 254)

**Step 1: Update stale `ZBRAIN_TEST_PG_URL` comment in source**

In `src/postgres.rs`, update the comment referencing `ZBRAIN_TEST_PG_URL` to reflect the new pg-embed fixture approach.

**Step 2: Run full three-green**

Run: `cargo build -p zbrain-core && cargo test -p zbrain-core && cargo clippy -p zbrain-core --tests 2>&1 | tail -20`
Expected: Build OK, all tests pass, clippy clean.

**Step 3: Commit**

```bash
git add crates/zbrain-core/src/postgres.rs
git commit -m "docs: update PG test reference comment to pg-embed fixture (A-4)"
```

---

## Acceptance checklist

- [ ] `cargo build -p zbrain-core --tests` succeeds with zero errors
- [ ] `cargo test -p zbrain-core` — all tests pass (no skips)
- [ ] `cargo clippy -p zbrain-core --tests` — zero warnings
- [ ] Zero `ZBRAIN_TEST_PG_URL` references in `tests/` directory
- [ ] Zero `#[serial_test::serial]` annotations in `tests/` directory
- [ ] Zero `serial_test` references in any `Cargo.toml`
- [ ] `pg-embed` appears only in `[dev-dependencies]`, never in `[dependencies]`
- [ ] Each PG test gets its own `PgFixture::start().await` instance
- [ ] `PgFixture::drop` reliably kills PG process and cleans datadir
- [ ] Fresh clone + `cargo test -p zbrain-core` passes without any env-var setup

## Execution handoff

After saving this plan, proceed with Subagent-Driven execution (using `superpowers:subagent-driven-development`), dispatching one subagent per task. Review each task's result before proceeding to the next.
