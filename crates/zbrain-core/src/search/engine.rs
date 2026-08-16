//! Thin `hybrid_search` orchestrator for zbrain.
//!
//! This is the Rust entry point that mirrors the TS `hybridSearch` core path
//! consumed by `think/gather.ts`. NOTE: Rust's retrieval + RRF fusion +
//! post-fusion metadata boosts (backlink / salience / recency) already live
//! inside `engine::fuse_and_boost` (called by `search_pages`), so this layer
//! is deliberately THIN — it resolves the query embedding, delegates to
//! `search_pages`, then applies the two tail stages the engine does NOT own:
//! page-level dedup and the token budget.
//!
//! Deferred (registered in docs/plans/MIGRATION.md):
//!   * `cosineReScore` — **G67**: needs a new engine method
//!     `get_embeddings_by_chunk_ids` (no backend yet); `cargo check` cannot
//!     validate a DB round-trip.
//!   * Semantic query cache (`hybridSearchCached`), two-pass walk, cross-modal
//!     LLM tie-break, graph-signals, per-list intent RRF weights — optional
//!     layers not required by the `think` contract.

use crate::embedding::EmbeddingClient;
use crate::engine::{BrainEngine, SearchOpts, SearchResult};
use crate::search::dedup::{dedup_results, DedupOpts};
use crate::token_budget::{enforce_token_budget, estimate_tokens};
use std::sync::Arc;

/// Options for the thin `hybrid_search` orchestrator. A subset of the TS
/// `HybridSearchOpts` surface — only the fields the Rust pipeline honors.
#[derive(Clone, Default)]
pub struct HybridSearchOpts {
    /// Max results to return (passed through to `search_pages`).
    pub limit: Option<usize>,
    /// Results to skip (applied AFTER dedup, mirroring TS `slice(offset, ..)`).
    pub offset: usize,
    /// Source scope (None = all sources).
    pub source_id: Option<String>,
    /// Floor-ratio gate for the metadata-axis boost stages inside
    /// `fuse_and_boost`. `None` = gate disabled.
    pub floor_ratio: Option<f64>,
    /// Skip the salience boost stage in `fuse_and_boost`.
    pub disable_salience_boost: bool,
    /// Skip the recency boost stage in `fuse_and_boost`.
    pub disable_recency_boost: bool,
    /// Page-level dedup knobs. `None` = defaults (cap 2/page, 60% type cap).
    pub dedup_opts: Option<DedupOpts>,
    /// Token budget in estimated tokens. `None` = no budget enforcement.
    pub token_budget: Option<u64>,
    /// Embedding client used to compute the query vector for the vector path.
    /// `None` → keyword-only (fusion degenerates to lexical, matching TS when
    /// no embedding provider is configured).
    pub embedding_client: Option<Arc<EmbeddingClient>>,
    /// Override the RRF K constant (default: `engine::RRF_K` = 60.0). Plumbed
    /// through to `SearchOpts::rrf_k` → `engine::fuse_and_boost` → `rrf_fuse`,
    /// so `zbrain eval --rrf-k` re-ranks without recompiling (KNOWN-GAPS G74b).
    pub rrf_k: Option<f64>,
}

impl HybridSearchOpts {
    /// Minimal opts used by `think/gather.ts` today: just a limit.
    pub fn with_limit(limit: usize) -> Self {
        HybridSearchOpts { limit: Some(limit), ..Default::default() }
    }
}

/// Run the hybrid search pipeline.
///
/// 1. Resolve the query embedding (fail-open: any embed error → keyword-only).
/// 2. Delegate to `engine.search_pages`, which performs lexical + vector
///    retrieval, RRF fusion, and the backlink/salience/recency boost stages.
/// 3. Page-level dedup.
/// 4. Offset slice, then token-budget enforcement.
pub async fn hybrid_search(
    engine: &dyn BrainEngine,
    query: &str,
    opts: &HybridSearchOpts,
) -> crate::Result<Vec<SearchResult>> {
    let embedding = match &opts.embedding_client {
        Some(client) => client.embed_query(query).await.ok(),
        None => None,
    };

    let mut results = engine
        .search_pages(&SearchOpts {
            keywords: vec![query.to_string()],
            query_embedding: embedding,
            limit: opts.limit,
            source_id: opts.source_id.clone(),
            floor_ratio: opts.floor_ratio,
            disable_salience_boost: opts.disable_salience_boost,
            disable_recency_boost: opts.disable_recency_boost,
            rrf_k: opts.rrf_k,
            ..Default::default()
        })
        .await?;

    results = dedup_results(&results, opts.dedup_opts.as_ref());

    // Offset slice (mirrors TS `.slice(offset, offset + limit)` after dedup).
    let offset = opts.offset.min(results.len());
    let sliced = if offset == 0 { results } else { results.split_off(offset) };

    match opts.token_budget {
        Some(budget) => {
            let (kept, _meta) =
                enforce_token_budget(&sliced, budget, |r| estimate_tokens(&r.page.compiled_truth));
            Ok(kept)
        }
        None => Ok(sliced),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::InMemoryEngine;

    #[tokio::test]
    async fn empty_brain_returns_empty() {
        let engine = InMemoryEngine::default();
        let out = hybrid_search(&engine, "anything", &HybridSearchOpts::with_limit(10))
            .await
            .unwrap();
        assert!(out.is_empty());
    }

    #[tokio::test]
    async fn keyword_only_when_no_embedding_client() {
        // Without an embedding client the pipeline must not panic and must
        // still return (empty) results — keyword-only degenerate path.
        let engine = InMemoryEngine::default();
        let opts = HybridSearchOpts {
            embedding_client: None,
            limit: Some(5),
            ..Default::default()
        };
        let out = hybrid_search(&engine, "needle", &opts).await.unwrap();
        assert!(out.is_empty());
    }
}
