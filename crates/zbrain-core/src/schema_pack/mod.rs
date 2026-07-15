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
// Test isolation for `~/.zbrain` file-I/O tests
// ---------------------------------------------------------------------------
//
// `schema_pack` sub-module tests (`mutate`, `activate`, `mutate_audit`) read and
// write the `~/.zbrain` directory. They used to isolate it by mutating the
// process-global `HOME`/`USERPROFILE` env vars, which forced the whole suite to
// run serially (behind a process-wide mutex) to avoid races.
//
// That serialization is gone: each test now injects a private `~/.zbrain` root
// on its own thread via `crate::paths::ScopedTestHome`, and audit verbosity via
// `mutate_audit::ScopedAuditVerbose`. Because cargo runs each test on its own
// thread, the thread-local overrides give every test a fully isolated home with
// zero cross-test interference — the suite runs in parallel with no lock.
