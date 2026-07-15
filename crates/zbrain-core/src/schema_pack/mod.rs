//! Schema Pack — Schema Cathedral v3.
//!
//! Manages markdown frontmatter type/link-type definitions, aliases, prefixes,
//! schema pack loading/activation/mutation/lint.
//!
//! Ported from TS `src/core/schema-pack/` (G4).
//!
//! Sub-modules:
//! - `manifest` — SchemaPackManifest data model + validation + identity
//! - `primitives` — five composable primitive defaults
//! - `closure` — alias graph BFS closure
//! - `loader` — YAML/JSON file loading + mini-YAML parser

pub mod manifest;
pub mod primitives;
pub mod closure;
pub mod loader;
pub mod registry;
pub mod per_source;
pub mod pack_lock;
pub mod trust_gate;
pub mod load_active;
pub mod lint_rules;
pub mod mutate_audit;
pub mod mutate;
pub mod activate;
pub mod type_accessors;
pub mod candidate_audit;
pub mod redos_guard;
pub mod discovery;

// ---------------------------------------------------------------------------
// Test-serialization guard for `~/.zbrain` file-I/O tests
// ---------------------------------------------------------------------------
//
// Several `schema_pack` sub-module tests (`mutate`, `activate`, `mutate_audit`)
// mutate the process-global `HOME`/`USERPROFILE` env vars and read/write the
// shared `~/.zbrain` directory. `cargo test` runs tests in the same binary in
// parallel by default, so these tests race on the global env + filesystem and
// fail nondeterministically (e.g. `PackNotFound`, reading stray audit records).
//
// This mirrors the existing process-wide `SCHEMA_INIT_LOCK` pattern used by
// `LibsqlEngine::init_schema`: a single static mutex that every file-I/O test
// acquires for its whole duration, so at most one schema-pack test touches
// `~/.zbrain` at a time. Pure (in-memory) tests may also acquire it harmlessly.
//
// See also `crates/zbrain-core/src/paths.rs` note: the schema-pack suite is
// run single-threaded by design; this guard enforces that under any test
// configuration.

#[cfg(test)]
pub(crate) static SCHEMA_FS_LOCK: std::sync::LazyLock<std::sync::Mutex<()>> =
    std::sync::LazyLock::new(|| std::sync::Mutex::new(()));

/// Acquire the schema-pack filesystem serialization guard.
///
/// Returns a guard that releases the lock when dropped (end of the calling
/// test). Poison-tolerant: a panicking test leaves the mutex poisoned, but the
/// next acquirer recovers via `into_inner` rather than deadlocking the suite.
#[cfg(test)]
pub(crate) fn lock_schema_fs() -> std::sync::MutexGuard<'static, ()> {
    match SCHEMA_FS_LOCK.lock() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    }
}
