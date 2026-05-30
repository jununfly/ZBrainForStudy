//! Slice #110-h H1 — concurrent `init_schema` flake reproducer.
//!
//! Background
//! ----------
//! Slice #110-g serialised PG integration tests but left libsql tests on the
//! default multi-threaded runner because "each test owns its own `NamedTempFile`,
//! so no cross-test contention". Empirically that still flakes at ~1.5% on
//! `tests/libsql_engine_list_pages.rs:45` (`engine.init_schema().await.expect("init_schema")`).
//!
//! Hypothesised culprits (see `docs/plans/20260526/18-session-state-110h-init-schema-flake.md`):
//!   1. `NamedTempFile` re-uses pid-based suffix → libsql open races on the
//!      same path when several tests start in the same wall-clock millisecond.
//!   2. libsql `Builder::new_local(...).build()` lazily provisions a global
//!      `OnceCell` that is not concurrency-safe on cold start.
//!   3. `SQLite` journal/WAL file creation race when N processes touch the
//!      same parent directory at once.
//!   4. `PRAGMA user_version` read-then-write window inside `init_schema`
//!      races with itself (multiple connections on the same file).
//!   5. `execute_batch` on the migration SQL touches an FS path that another
//!      task is still holding open for write.
//!
//! Goal of H1
//! ----------
//! Reproduce the flake at >=10% so subsequent steps (H2: surface real error
//! string; H3: cross-check against the 5-item candidate list) have a reliable
//! signal. We intentionally do NOT fix anything here — only amplify.
//!
//! Test strategy
//! -------------
//! - `N = 32` parallel `tokio::spawn` tasks.
//! - Each task owns its own `NamedTempFile`, its own `LibsqlEngine`, calls
//!   `connect()` + `init_schema()`.
//! - We collect every task's `Result<(), String>` (stringified to dodge the
//!   `Error` cloning question) via `JoinSet`.
//! - On any failure we print a per-task report and panic with the failure
//!   count. On all-success we simply pass.
//!
//! Expected behaviour
//! ------------------
//! - Pre-fix: this test SHOULD fail intermittently (>=1 of 32 tasks panics
//!   or returns `Err`).
//! - Post-fix (later steps): this test must pass 100/100 consecutive runs.
//!
//! Running locally
//! ---------------
//! ```bash
//! cd crates/zbrain-core
//! # single run
//! cargo test --test libsql_init_schema_flake_reproduce -- --nocapture
//! # 20-round amplifier (bash)
//! for i in $(seq 1 20); do \
//!   cargo test --test libsql_init_schema_flake_reproduce -- --nocapture || break; \
//! done
//! ```

use tempfile::NamedTempFile;
use tokio::task::JoinSet;
use zbrain_core::engine::{BrainEngine, EngineConfig};
use zbrain_core::libsql::LibsqlEngine;

/// Number of parallel `init_schema` invocations.
/// 32 is chosen to saturate a typical 8-12 core dev machine while staying
/// well below the default fd limit. Adjust upward if the flake doesn't
/// surface; downward if CI is too noisy.
const CONCURRENT_INITS: usize = 32;

/// One independent attempt: own temp file, own engine, full connect + init.
/// Returns `Err(stringified)` so the caller can aggregate without `Error: Clone`.
async fn init_schema_once() -> Result<(), String> {
    let path = NamedTempFile::new().map_err(|e| format!("alloc temp db file: {e}"))?;
    let engine = LibsqlEngine::new();
    let cfg = EngineConfig {
        database_url: None,
        database_path: Some(path.path().to_string_lossy().into_owned()),
    };
    engine
        .connect(&cfg)
        .await
        .map_err(|e| format!("connect: {e}"))?;
    engine
        .init_schema()
        .await
        .map_err(|e| format!("init_schema: {e}"))?;
    // Keep `path` alive until init_schema returns; dropping the
    // NamedTempFile would unlink the file under libsql's nose.
    drop(path);
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 8)]
async fn init_schema_survives_high_concurrency() {
    // Spawn N independent attempts. Each runs to completion regardless of
    // siblings — we want to maximise the chance of two tasks hitting the
    // hot path of init_schema (PRAGMA read/write, migration apply, journal
    // file create) at the same instant.
    let mut set: JoinSet<Result<(), String>> = JoinSet::new();
    for _ in 0..CONCURRENT_INITS {
        set.spawn(init_schema_once());
    }

    let mut failures: Vec<String> = Vec::new();
    let mut joined: usize = 0;
    while let Some(res) = set.join_next().await {
        joined += 1;
        match res {
            Ok(Ok(())) => {}
            Ok(Err(msg)) => failures.push(format!("task#{joined}: {msg}")),
            Err(join_err) => failures.push(format!("task#{joined}: join error: {join_err}")),
        }
    }

    assert_eq!(joined, CONCURRENT_INITS, "JoinSet lost tasks");

    if !failures.is_empty() {
        eprintln!(
            "init_schema flake reproduced: {}/{} failed",
            failures.len(),
            CONCURRENT_INITS
        );
        for f in &failures {
            eprintln!("  - {f}");
        }
        panic!(
            "{} of {} concurrent init_schema attempts failed (see stderr)",
            failures.len(),
            CONCURRENT_INITS
        );
    }
}
