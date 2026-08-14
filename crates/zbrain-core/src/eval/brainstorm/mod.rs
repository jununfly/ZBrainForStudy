//! Brainstorm generator — faithful Rust port of `src/core/{error-classify,
//! checkpoint, judges, domain-bank, orchestrator}.ts` + the `cmd/{brainstorm,
//! eval-brainstorm}.ts` CLI entry points.
//!
//! The generator bisociates "close" pages (from hybrid search of the user's
//! question) with "far" pages pulled from the brain's own domain structure
//! (prefix-stratified sampling), then scores the resulting `(close × far)`
//! idea crosses with an LLM judge. Two judge configs (`BRAINSTORM_JUDGE_CONFIG`
//! / `LSD_JUDGE_CONFIG`) flip the threshold + inversion rule.
//!
//! Modules:
//!   * [`error_classify`] — 57014 (query_canceled) → `brainstorm_timeout`.
//!   * [`checkpoint`]     — `compute_run_id` (Q3 MVP; resume playback TODO).
//!   * [`judges`]         — `run_judge` + two configs + pure scoring helpers.
//!   * [`domain_bank`]    — `fetch_far` prefix-stratified far-page retrieval.
//!   * [`orchestrator`]   — 4-phase pipeline wiring the above together.

pub mod checkpoint;
pub mod domain_bank;
pub mod error_classify;
pub mod judges;
pub mod orchestrator;
pub mod store;
