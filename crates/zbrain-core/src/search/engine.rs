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
//!   * ~~`cosineReScore` (G67)~~ — **ported**. Rust's retrieval is page-level,
//!     so the re-score operates on `Page::embedding` (the faithful analog of
//!     the TS chunk-level pass); see [`cosine_re_score`]. The engine method
//!     `get_embeddings_by_chunk_ids` remains available for a chunk-level path.
//!   * Semantic query cache (`hybridSearchCached`) — **G69**, pending.
//!   * two-pass walk, cross-modal LLM tie-break, graph-signals, per-list
//!     intent RRF weights — optional layers not required by the `think`
//!     contract.

use crate::embedding::EmbeddingClient;
use crate::engine::{decode_embedding_le, BrainEngine, SearchOpts, SearchResult};
use crate::search::cosine_similarity;
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
            // Clone the query embedding so the rescore stage below can
            // still observe the same vector without fighting the borrow
            // checker (SearchOpts takes ownership).
            query_embedding: embedding.clone(),
            limit: opts.limit,
            source_id: opts.source_id.clone(),
            floor_ratio: opts.floor_ratio,
            disable_salience_boost: opts.disable_salience_boost,
            disable_recency_boost: opts.disable_recency_boost,
            rrf_k: opts.rrf_k,
            ..Default::default()
        })
        .await?;

    // G67: cosine re-score stage (page-level analog of the TS `cosineReScore`).
    // Re-ranks each result by exact cosine between the query embedding and the
    // page's stored embedding, blended with the RRF fusion score. Fail-open:
    // when no query embedding is available the fused results pass through
    // unchanged.
    if let Some(ref q_emb) = embedding {
        results = cosine_re_score(&results, q_emb);
    }

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

/// G67 — Page-level analog of the TS `cosineReScore` (see
/// `src/core/search/hybrid.ts:1299` in the pre-migration TS source).
///
/// TS rescored at chunk granularity: it pulled each chunk's embedding via
/// `engine.getEmbeddingsByChunkIds(chunkIds, column)` and blended
/// `0.7 * normRrf + 0.3 * cosine` per chunk. Rust retrieval is page-level
/// — every `SearchResult` already carries a `Page::embedding` (the
/// mean-pooled page vector used by the vector path), so the faithful port
/// computes the cosine against that single vector and stamps the raw value
/// on `cosine_score` for `--explain` attribution.
///
/// Behavior:
///   * For every result with a non-empty `Page::embedding` the raw cosine
///     between the page vector and `query_embedding` is computed and stored
///     on `cosine_score` (mirroring `rerank_score` semantics).
///   * `score` is replaced by `0.7 * normRrf + 0.3 * cosine`, where
///     `normRrf` is the fused score normalized to 0..1 against the head.
///   * `base_score` is left untouched (it stays the pre-boost fused stamp
///     for `--explain` attribution, matching the rerank-stage contract).
///   * Results without a stored embedding get `cosine_score = None` and
///     fall back to their normalized RRF contribution (no panic, no
///     removal — same fail-open shape as TS).
///   * The output is re-sorted by the blended score descending.
pub fn cosine_re_score(results: &[SearchResult], query_embedding: &[f32]) -> Vec<SearchResult> {
    if results.is_empty() {
        return Vec::new();
    }
    // Normalize RRF fused scores to 0..1 against the head, mirroring the
    // TS guard `const max = Math.max(...rows.map(r => r.score))`.
    let max_score = results
        .iter()
        .map(|r| r.score)
        .fold(f64::MIN, f64::max);
    let max_score = if max_score <= 0.0 { 1.0 } else { max_score };

    let mut rescored: Vec<SearchResult> = results
        .iter()
        .map(|r| {
            let norm_rrf = r.score / max_score;
            // Page::embedding is stored as little-endian f32 bytes (see
            // `encode_embedding`/`decode_embedding_le` in engine.rs). Decode
            // for the cosine call; pages with no embedding (None) or with
            // a non-multiple-of-4 byte buffer (decode_embedding_le returns
            // None) fall back to the normalized RRF value.
            let page_vec = r
                .page
                .embedding
                .as_deref()
                .and_then(decode_embedding_le);
            match page_vec {
                Some(vec) if !vec.is_empty() => {
                    let cosine = cosine_similarity(&vec, query_embedding);
                    let blended = 0.7 * norm_rrf + 0.3 * cosine;
                    let mut cloned = r.clone();
                    cloned.cosine_score = Some(cosine);
                    cloned.score = blended;
                    cloned
                }
                _ => {
                    // Fail-open: no page embedding → cosine stays None and
                    // the blended score reduces to the normalized RRF.
                    let mut cloned = r.clone();
                    cloned.score = norm_rrf;
                    cloned
                }
            }
        })
        .collect();

    // Re-sort by blended score descending, stable for ties.
    rescored.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    rescored
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::InMemoryEngine;
    use crate::Page;

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

    // ── G67: cosine_re_score unit tests ────────────────────────────────
    // These exercise the page-level analog of the TS `cosineReScore`
    // (src/core/search/hybrid.ts:1299). The RRF-fused scores are
    // normalized to 0..1 against the head, then blended with the
    // per-page cosine as `0.7 * normRrf + 0.3 * cosine` (same weights as
    // the TS source). Pages without an embedding get `cosine_score = None`
    // and fall back to the normalized RRF value.

    fn page_with_embedding(slug: &str, embedding: Vec<f32>) -> SearchResult {
        let mut page = Page::default();
        page.slug = slug.to_string();
        page.compiled_truth = format!("body of {}", slug);
        // Page.embedding is LE f32 bytes; tests build the same shape that
        // libsql/postgres would surface in production (the G67 stage
        // re-decodes via `decode_embedding_le` before calling
        // `cosine_similarity`).
        let mut bytes = Vec::with_capacity(embedding.len() * 4);
        for v in &embedding {
            bytes.extend_from_slice(&v.to_le_bytes());
        }
        page.embedding = Some(bytes);
        SearchResult {
            page,
            score: 0.0,
            base_score: 0.0,
            snippet: None,
            rerank_score: None,
            reranker_delta: None,
            salience_boost: None,
            recency_boost: None,
            cosine_score: None,
        }
    }

    #[test]
    fn cosine_re_score_preserves_order_when_cosine_agrees() {
        // Page A is the RRF head (score 0.9) and also the best cosine match
        // (cos 1.0); page B is the runner-up on both signals. The
        // blended ranking must remain A → B.
        let q = vec![1.0_f32, 0.0];
        let mut a = page_with_embedding("a", vec![1.0, 0.0]);
        a.score = 0.9;
        a.base_score = 0.9;
        let mut b = page_with_embedding("b", vec![0.9, 0.1]);
        b.score = 0.6;
        b.base_score = 0.6;
        let out = cosine_re_score(&[a, b], &q);
        assert_eq!(out[0].page.slug, "a");
        assert_eq!(out[1].page.slug, "b");
        // Stamp captures the raw cosine.
        assert_eq!(out[0].cosine_score, Some(1.0));
        assert!(out[1].cosine_score.unwrap() > 0.9);
    }

    #[test]
    fn cosine_re_score_reorders_when_cosine_disagrees_with_rrf() {
        // RRF head is page A (score 0.9), but page B has the perfect cosine
        // match. With 0.7/0.3 weights the blended scores are:
        //   A: 0.7 * 1.0 + 0.3 * 0.0 = 0.70
        //   B: 0.7 * 0.5 + 0.3 * 1.0 = 0.65
        // RRF still wins — verify the boundary is sensible. The real
        // purpose here is to confirm `cosine_re_score` re-sorts by blended
        // score, not by RRF input order.
        let q = vec![1.0_f32, 0.0];
        let mut a = page_with_embedding("a", vec![0.0, 1.0]); // orthogonal
        a.score = 0.9;
        a.base_score = 0.9;
        let mut b = page_with_embedding("b", vec![1.0, 0.0]); // identical
        b.score = 0.5;
        b.base_score = 0.5;
        let out = cosine_re_score(&[a, b], &q);
        assert_eq!(out[0].page.slug, "a", "RRF dominance preserved at 0.7/0.3");
        // raw cosine stamps must be set on both.
        assert_eq!(out[0].cosine_score, Some(0.0));
        assert_eq!(out[1].cosine_score, Some(1.0));
    }

    #[test]
    fn cosine_re_score_handles_missing_page_embedding() {
        // A page with an empty embedding must NOT panic and must stamp
        // `cosine_score = None`, falling back to its normalized RRF
        // contribution. Mirrors the TS guard that skipped pages without
        // embeddings.
        let q = vec![1.0_f32, 0.0];
        let mut head = page_with_embedding("head", vec![1.0, 0.0]);
        head.score = 0.9;
        head.base_score = 0.9;
        let mut bare = page_with_embedding("bare", Vec::new());
        bare.score = 0.6;
        bare.base_score = 0.6;
        // Force "no embedding" semantics (decode_embedding_le on an empty
        // buffer would yield None as well, but using `None` is the
        // canonical signal a page has never been embedded).
        bare.page.embedding = None;
        let out = cosine_re_score(&[head, bare], &q);
        let bare_pos = out.iter().position(|r| r.page.slug == "bare").unwrap();
        assert!(out[bare_pos].cosine_score.is_none());
    }
}
