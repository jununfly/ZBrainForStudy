//! zbrain `facts` core subsystem — Rust port of `src/core/facts/*`.
//!
//! Mirrors the TS module's responsibilities:
//!   - `decay`        — per-kind confidence decay (`effective_confidence`).
//!   - `eligibility`  — backstop eligibility predicate.
//!   - `classify`     — contradiction classifier (cosine fast-path + LLM).
//!   - `absorb_log`   — best-effort `facts:absorb` failure writer.
//!   - `extract`      — extraction kill-switch + model resolution + JSON parse.
//!
//! Fence parsing/rendering/upsert was ported earlier under
//! [`crate::facts_fence`] (no `facts/` directory then existed); it is
//! re-exported here so callers get one unified `facts` namespace.

pub mod absorb_log;
pub mod classify;
pub mod decay;
pub mod eligibility;
pub mod extract;

// Re-export the fence surface under the unified `facts` namespace.
pub use crate::facts_fence::{
    parse_facts_fence, render_facts_fence, strip_facts_fence, upsert_fact_row, FenceFact,
    FenceFactInput,
};
