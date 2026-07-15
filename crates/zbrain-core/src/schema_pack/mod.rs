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
