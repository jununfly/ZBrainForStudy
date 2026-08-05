//! Hybrid search: fusion math, intent classification, dedup, and the thin
//! `hybrid_search` orchestrator.
//!
//! Ported from `src/core/search/` (the TypeScript fusion core). Every pure
//! function carries Rust `#[test]`s that reproduce the TypeScript unit-test
//! assertions, satisfying the project's PARITY_GATE (Rust coverage must be
//! non-stub before the TS source is deleted).
//!
//! Architecture note: Rust internalized retrieval + RRF fusion + the
//! post-fusion metadata boosts (backlink / salience / recency) into
//! `engine::fuse_and_boost` (called by `search_pages`). So the orchestrator
//! (`engine::hybrid_search`) is THIN: it resolves the query embedding,
//! delegates to `search_pages`, then applies the two tail stages the engine
//! does not own — page-level dedup and the token budget. `apply_recency_boost`
//! lives in `crate::recency_decay` and is reused.

pub mod dedup;
pub mod engine;
pub mod fusion;
pub mod intent;

pub use dedup::{dedup_results, DedupOpts, MAX_PER_PAGE, MAX_TYPE_RATIO};
pub use engine::{hybrid_search, HybridSearchOpts};
pub use fusion::{
    apply_backlink_boost, apply_salience_boost, compute_floor_threshold, cosine_similarity,
    rrf_fusion, rrf_fusion_weighted, BACKLINK_BOOST_COEF, SalienceStrength, COMPILED_TRUTH_BOOST,
};
pub use intent::{
    auto_detect_detail, classify_query, classify_query_intent, is_ambiguous_modality_query,
    ModalityMode, QueryIntent, QuerySuggestions, RecencyMode, SalienceMode, SearchDetail,
};
