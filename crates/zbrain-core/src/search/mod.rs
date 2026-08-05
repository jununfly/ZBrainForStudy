//! Post-fusion ranking math for zbrain hybrid search.
//!
//! Pure, DB-free functions ported 1:1 from `src/core/search/hybrid.ts` (the
//! TypeScript fusion core). Every function carries Rust `#[test]`s that
//! reproduce the TypeScript unit-test assertions, satisfying the project's
//! PARITY_GATE (Rust coverage must be non-stub before the TS source is
//! deleted).
//!
//! `apply_recency_boost` is intentionally NOT re-ported here — it already
//! lives in `crate::recency_decay` (faithfully ported with its own tests) and
//! is reused. The fusion orchestrator (`hybridSearch`, a later sub-node) will
//! call both this module's stages and `recency_decay::apply_recency_boost`.

mod fusion;

pub use fusion::*;
