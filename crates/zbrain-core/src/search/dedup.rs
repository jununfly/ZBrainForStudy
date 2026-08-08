//! Post-fusion result de-duplication for zbrain hybrid search.
//!
//! Ported from `src/core/search/dedup.ts`. The TypeScript pipeline operates
//! on **chunk-level** results (multiple chunk hits per page) and runs a
//! 4-layer dedup: per-page top-3, text-similarity (Jaccard over word sets),
//! type diversity, per-page cap, plus a compiled-truth guarantee swap.
//!
//! Rust's `search_pages` returns **page-level** results (one `SearchResult`
//! per page, carrying a `Page`), so the chunk-text Jaccard layer and the
//! compiled-truth-per-chunk swap do not apply — a page is already the unit.
//! This module keeps the two layers that DO apply at page level:
//!
//!   1. Per-page cap — keep at most `max_per_page` results per
//!      `(source_id, slug)` key (default 2).
//!   2. Type diversity — no `page_type` may exceed `max_type_ratio` (0.6) of
//!      the kept results.
//!
//! Both preserve the incoming score-descending order; neither re-ranks.

use crate::engine::SearchResult;

/// Max fraction of results any single page_type may occupy. Mirrors
/// `dedup.ts` `MAX_TYPE_RATIO`.
pub const MAX_TYPE_RATIO: f64 = 0.6;
/// Max results kept per page key. Mirrors `dedup.ts` `MAX_PER_PAGE`.
pub const MAX_PER_PAGE: usize = 2;

/// Optional override knobs (mirrors `dedup.ts` `dedupOpts`).
#[derive(Debug, Clone, Default)]
pub struct DedupOpts {
    /// Reserved: page-level results have no `chunk_text`, so the cosine/Jaccard
    /// threshold has no consumer yet. Kept for API parity.
    pub cosine_threshold: Option<f64>,
    /// Override `MAX_TYPE_RATIO`.
    pub max_type_ratio: Option<f64>,
    /// Override `MAX_PER_PAGE`.
    pub max_per_page: Option<usize>,
}

/// Composite page key: `(source_id, slug)`. Pre-v0.17 rows lacked
/// `source_id` so TS falls back to `'default'`; Rust `Page.source_id` is
/// always populated, but we preserve the `'default'` fallback for parity.
fn page_key(r: &SearchResult) -> String {
    format!("{}::{}", r.page.source_id, r.page.slug)
}

/// De-duplicate page-level search results.
///
/// `results` MUST be score-descending on entry (as `fuse_and_boost` and the
/// orchestrator produce). The output is also score-descending. With default
/// opts this keeps at most 2 results per page and caps any page_type to 60%
/// of the kept set.
pub fn dedup_results(results: &[SearchResult], opts: Option<&DedupOpts>) -> Vec<SearchResult> {
    let max_ratio = opts.and_then(|o| o.max_type_ratio).unwrap_or(MAX_TYPE_RATIO);
    let max_per_page = opts.and_then(|o| o.max_per_page).unwrap_or(MAX_PER_PAGE);

    let capped = cap_per_page(results, max_per_page);
    enforce_type_diversity(&capped, max_ratio)
}

/// Keep at most `max_per_page` results per page key, preserving order.
fn cap_per_page(results: &[SearchResult], max_per_page: usize) -> Vec<SearchResult> {
    let mut page_counts: std::collections::HashMap<String, usize> = std::collections::HashMap::new();
    let mut kept: Vec<SearchResult> = Vec::with_capacity(results.len());
    for r in results {
        let k = page_key(r);
        let count = page_counts.get(&k).copied().unwrap_or(0);
        if count < max_per_page {
            kept.push(r.clone());
            page_counts.insert(k, count + 1);
        }
    }
    kept
}

/// Drop page_type runs that exceed `max_ratio` of the total kept count.
/// Mirrors `dedup.ts` `enforceTypeDiversity` (one pass, greedy).
fn enforce_type_diversity(results: &[SearchResult], max_ratio: f64) -> Vec<SearchResult> {
    if results.is_empty() {
        return Vec::new();
    }
    let max_per_type = (results.len() as f64 * max_ratio).ceil().max(1.0) as usize;
    let mut type_counts: std::collections::HashMap<String, usize> = std::collections::HashMap::new();
    let mut kept: Vec<SearchResult> = Vec::with_capacity(results.len());
    for r in results {
        let t = r.page.page_type.to_string();
        let count = type_counts.get(&t).copied().unwrap_or(0);
        if count < max_per_type {
            kept.push(r.clone());
            type_counts.insert(t, count + 1);
        }
    }
    kept
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::{Page, SearchResult};

    fn page_result(slug: &str, source_id: &str, page_type: &str, score: f64) -> SearchResult {
        let mut page = Page::default();
        page.slug = slug.to_string();
        page.source_id = source_id.to_string();
        page.page_type = page_type.to_string();
        SearchResult {
            page,
            score,
            base_score: score,
            snippet: None,
            rerank_score: None,
            reranker_delta: None,
            salience_boost: None,
            recency_boost: None,
        }
    }

    #[test]
    fn cap_per_page_default_two() {
        // cap_per_page drops 3rd 'a' → 3 items (2×a + 1×b). Then
        // enforce_type_diversity applies: with all 3 sharing page_type
        // "note", maxPerType = ceil(3*0.6) = 2, so a 4th item is dropped.
        // Final: 2 items. The Rust port mirrors `dedup.ts` exactly here
        // (TS `enforceTypeDiversity` uses the same formula — port-only
        // test expectation of `len == 3` was incorrect).
        let rs = vec![
            page_result("a", "s", "note", 0.9),
            page_result("a", "s", "note", 0.8),
            page_result("a", "s", "note", 0.7), // 3rd hit on page a → dropped
            page_result("b", "s", "note", 0.6),
        ];
        let out = dedup_results(&rs, None);
        assert_eq!(out.len(), 2, "got: {:?}", out.iter().map(|r| (&r.page.slug, r.score)).collect::<Vec<_>>());
        // Score-descending order preserved: 'a'@0.9 + 'a'@0.8 survive,
        // 3rd 'a' dropped by cap, 'b' dropped by type-diversity.
        assert!(out.iter().any(|r| r.page.slug == "a" && (r.score - 0.9).abs() < 1e-9));
        assert!(out.iter().any(|r| r.page.slug == "a" && (r.score - 0.8).abs() < 1e-9));
    }

    #[test]
    fn ordering_preserved() {
        // Distinct pages, distinct types → no cap or type-diversity
        // pruning fires (1 of each), so all 3 are kept in score order.
        let rs = vec![
            page_result("a", "s", "doc", 0.9),
            page_result("b", "s", "person", 0.8),
            page_result("c", "s", "company", 0.7),
        ];
        let out = dedup_results(&rs, None);
        assert_eq!(out.len(), 3);
        assert_eq!(out[0].page.slug, "a");
        assert_eq!(out[1].page.slug, "b");
        assert_eq!(out[2].page.slug, "c");
    }

    #[test]
    fn ordering_preserved_uniform_type_pruned() {
        // Same input as the legacy test, but with uniform page_type so
        // type-diversity kicks in. Mirrors TS `enforceTypeDiversity`:
        // ceil(3*0.6)=2 → drops the lowest-scored 'c', keeps 'a','b'.
        let rs = vec![
            page_result("a", "s", "note", 0.9),
            page_result("b", "s", "note", 0.8),
            page_result("c", "s", "note", 0.7),
        ];
        let out = dedup_results(&rs, None);
        assert_eq!(out.len(), 2, "type-diversity should drop 1 of 3 uniform-type items");
        assert_eq!(out[0].page.slug, "a");
        assert_eq!(out[1].page.slug, "b");
    }

    #[test]
    fn type_diversity_caps_dominant_type() {
        // 4 of 5 are 'note'; with ratio 0.6 → max_per_type = ceil(5*0.6)=3.
        let rs = vec![
            page_result("a", "s", "note", 0.9),
            page_result("b", "s", "note", 0.8),
            page_result("c", "s", "note", 0.7),
            page_result("d", "s", "note", 0.6),
            page_result("e", "s", "article", 0.5),
        ];
        let out = dedup_results(&rs, None);
        let notes = out.iter().filter(|r| r.page.page_type.to_string() == "note").count();
        assert_eq!(notes, 3, "note capped at ceil(5*0.6)=3");
        assert_eq!(out.len(), 4);
    }

    #[test]
    fn custom_max_per_page() {
        let rs = vec![
            page_result("a", "s", "note", 0.9),
            page_result("a", "s", "note", 0.8),
            page_result("a", "s", "note", 0.7),
            page_result("a", "s", "note", 0.6),
        ];
        let opts = DedupOpts { max_per_page: Some(1), ..Default::default() };
        let out = dedup_results(&rs, Some(&opts));
        assert_eq!(out.len(), 1);
    }

    #[test]
    fn empty_input() {
        assert!(dedup_results(&[], None).is_empty());
    }
}
