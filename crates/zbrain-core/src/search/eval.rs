//! Retrieval Evaluation Harness (IR metrics).
//!
//! Ports `src/core/search/eval.ts` (deleted under `bcafcafd` — this is a
//! "delete-then-backfill" gap, **G73** in `docs/plans/KNOWN-GAPS.md`). The
//! four standard IR metrics (`precision@k`, `recall@k`, `mrr`, `ndcg@k`) are
//! pure functions with zero dependencies and are fully unit-tested; `run_eval`
//! orchestrates a search strategy against user-supplied ground truth (qrels)
//! and returns a structured [`EvalReport`].
//!
//! ## Design note vs the TS original
//!
//! The TS `runEval` took a `BrainEngine` and switched on `config.strategy`
//! (`keyword` | `vector` | `hybrid`), calling `engine.searchKeyword` /
//! `engine.searchVector` / `hybridSearch` internally. Rust uses a single
//! `engine.search_pages` entry point whose `query_embedding: Option<Vec<f32>>`
//! selects keyword (None) vs vector/hybrid (Some). To keep this module
//! **embedding-free and feature-gate-free** (the `EmbeddingClient` only exists
//! under the `embedding` feature), `run_eval` instead accepts an **async query
//! closure** `Fn(&str) -> Fut` that injects the strategy. Callers wire the
//! closure to whatever search path they want (keyword via
//! [`keyword_search_slugs`], or vector/hybrid with their own embedding client).
//! This is the idiomatic Rust equivalent of "the harness runs an arbitrary
//! search strategy" and keeps the metric math fully testable without any
//! backend.
//!
//! Faithful behavior preserved:
//!   * `precisionAtK` divides by `k` (not by `min(hits, k)`), matching TS.
//!   * `recallAtK` divides by `relevant.size`.
//!   * `ndcgAtK` uses `log2(rank + 1)` with 1-indexed rank, ideal DCG from the
//!     top-`k` grades sorted descending.
//!   * `runEval` retrieves up to `limit = config.limit ?? max(k*2, 10)` per
//!     query, then computes the @k metrics over that retrieved set.

use crate::engine::{BrainEngine, PageInput, SearchOpts, SearchResult};
use crate::Error;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::path::Path;

// ─────────────────────────────────────────────────────────────────
// Ground truth types
// ─────────────────────────────────────────────────────────────────

/// A single query + its judged-relevant slugs (binary relevance).
///
/// Mirrors `EvalQrel` in the deleted TS. `grades` provides optional graded
/// relevance for nDCG (1–3 typical); when omitted all relevant slugs grade 1.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct EvalQrel {
    /// Optional stable identifier for the query.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    pub query: String,
    /// Slugs considered relevant (binary relevance).
    pub relevant: Vec<String>,
    /// Optional graded relevance for nDCG. `slug -> grade`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub grades: Option<HashMap<String, f64>>,
}

/// File wrapper accepted by [`parse_qrels`] (the `{ version, queries }` form).
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct EvalQrelFile {
    pub version: u8,
    pub queries: Vec<EvalQrel>,
}

// ─────────────────────────────────────────────────────────────────
// Config types
// ─────────────────────────────────────────────────────────────────

/// Search strategy evaluated by the harness.
///
/// The actual execution is injected by the caller (see module docs), but we
/// carry the enum so reports stay self-describing and the serialized config
/// round-trips with the TS `strategy` literal union.
#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum EvalStrategy {
    Keyword,
    Vector,
    /// Default, matching TS `hybrid`.
    #[default]
    Hybrid,
}

/// Evaluation configuration. Mirrors `EvalConfig` in the deleted TS.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct EvalConfig {
    /// Human-readable label for this configuration (shown in A/B output).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub strategy: Option<EvalStrategy>,
    /// Override RRF K constant (default: 60).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rrf_k: Option<f64>,
    /// Enable multi-query expansion (hybrid only, default false for eval stability).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expand: Option<bool>,
    /// Override cosine dedup threshold (default: 0.85).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dedup_cosine_threshold: Option<f64>,
    /// Override type ratio cap (default: 0.6).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dedup_type_ratio: Option<f64>,
    /// Override max chunks per page (default: 2).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dedup_max_per_page: Option<usize>,
    /// Max results to retrieve per query (default: max(k*2, 10)).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<usize>,
}

// ─────────────────────────────────────────────────────────────────
// Report types
// ─────────────────────────────────────────────────────────────────

/// Per-query evaluation result.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct QueryResult {
    pub query: String,
    /// Returned slugs in rank order (already capped at the configured `limit`).
    pub hits: Vec<String>,
    pub precision_at_k: f64,
    pub recall_at_k: f64,
    pub mrr: f64,
    pub ndcg_at_k: f64,
}

/// Aggregate evaluation report across all queries.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct EvalReport {
    pub config: EvalConfig,
    /// The k cutoff used for P@k, R@k, nDCG@k.
    pub k: usize,
    pub queries: Vec<QueryResult>,
    pub mean_precision: f64,
    pub mean_recall: f64,
    pub mean_mrr: f64,
    pub mean_ndcg: f64,
}

// ─────────────────────────────────────────────────────────────────
// Pure metric functions
// ─────────────────────────────────────────────────────────────────

/// Precision@k: fraction of top-k hits that are relevant.
///
/// Divides by `k` (not by `min(hits.len(), k)`), matching the TS original.
pub fn precision_at_k(hits: &[String], relevant: &HashSet<String>, k: usize) -> f64 {
    if k == 0 || hits.is_empty() || relevant.is_empty() {
        return 0.0;
    }
    let top_k: HashSet<String> = hits.iter().take(k).cloned().collect();
    let relevant_hits = top_k.iter().filter(|h| relevant.contains(*h)).count();
    relevant_hits as f64 / k as f64
}

/// Recall@k: fraction of all relevant docs found in top-k hits.
pub fn recall_at_k(hits: &[String], relevant: &HashSet<String>, k: usize) -> f64 {
    if k == 0 || hits.is_empty() || relevant.is_empty() {
        return 0.0;
    }
    let top_k: HashSet<String> = hits.iter().take(k).cloned().collect();
    let relevant_hits = top_k.iter().filter(|h| relevant.contains(*h)).count();
    relevant_hits as f64 / relevant.len() as f64
}

/// Mean Reciprocal Rank: 1/rank of the first relevant hit (0 if none found).
pub fn mrr(hits: &[String], relevant: &HashSet<String>) -> f64 {
    if hits.is_empty() || relevant.is_empty() {
        return 0.0;
    }
    for (i, h) in hits.iter().enumerate() {
        if relevant.contains(h) {
            return 1.0 / (i as f64 + 1.0);
        }
    }
    0.0
}

/// nDCG@k: Normalized Discounted Cumulative Gain.
///
/// Uses `grades` for graded relevance (binary relevance = all relevant slugs
/// grade 1). `DCG = Σ grade_i / log2(rank_i + 1)` over the top-k; ideal DCG is
/// the DCG of the perfect ranking (all positive grades at the top);
/// `nDCG = DCG / IDCG`.
pub fn ndcg_at_k(hits: &[String], grades: &HashMap<String, f64>, k: usize) -> f64 {
    if k == 0 || hits.is_empty() || grades.is_empty() {
        return 0.0;
    }

    let top_k: Vec<&String> = hits.iter().take(k).collect();
    let mut dcg = 0.0;
    for (i, h) in top_k.iter().enumerate() {
        let grade = grades.get(*h).copied().unwrap_or(0.0);
        dcg += grade / ((i as f64 + 2.0).log2()); // log2(rank+1), rank is 1-indexed
    }

    // Ideal DCG: sort all positive grades descending, take top-k.
    let mut ideal: Vec<f64> = grades
        .values()
        .copied()
        .filter(|g| *g > 0.0)
        .collect();
    ideal.sort_by(|a, b| b.partial_cmp(a).unwrap_or(std::cmp::Ordering::Equal));

    let mut idcg = 0.0;
    for (i, g) in ideal.iter().take(k).enumerate() {
        idcg += g / ((i as f64 + 2.0).log2());
    }

    if idcg == 0.0 {
        return 0.0;
    }
    dcg / idcg
}

// ─────────────────────────────────────────────────────────────────
// Helpers
// ─────────────────────────────────────────────────────────────────

/// Build the grades map for nDCG. If the qrel has explicit grades, use them;
/// otherwise assign grade 1 to every relevant slug (binary relevance).
pub fn build_grades_map(qrel: &EvalQrel) -> HashMap<String, f64> {
    if let Some(grades) = &qrel.grades {
        if !grades.is_empty() {
            return grades.clone();
        }
    }
    qrel.relevant.iter().map(|s| (s.clone(), 1.0)).collect()
}

fn mean(values: &[f64]) -> f64 {
    if values.is_empty() {
        return 0.0;
    }
    values.iter().sum::<f64>() / values.len() as f64
}

/// Parse qrels from either an inline JSON string or a file path.
///
/// Inline JSON starts with `[` or `{`. Otherwise `input` is treated as a file
/// path and read from disk. Both the bare array form `[EvalQrel, ...]` and the
/// wrapped object form `{ "version": 1, "queries": [...] }` are accepted.
pub fn parse_qrels(input: &str) -> Result<Vec<EvalQrel>, Error> {
    let trimmed = input.trim_start();
    let raw = if trimmed.starts_with('[') || trimmed.starts_with('{') {
        input.to_string()
    } else {
        std::fs::read_to_string(Path::new(trimmed))
            .map_err(|e| Error::new("EvalError", "parse_qrels", &format!("failed to read qrels file '{}': {e}", trimmed)))?
    };

    let parsed: serde_json::Value = serde_json::from_str(&raw)
        .map_err(|e| Error::new("EvalError", "parse_qrels", &e.to_string()))?;

    if let Some(arr) = parsed.as_array() {
        return serde_json::from_value(parsed)
            .map_err(|e| Error::new("EvalError", "parse_qrels", &e.to_string()));
    }
    if let Some(queries) = parsed.get("queries").cloned() {
        return serde_json::from_value::<Vec<EvalQrel>>(queries)
            .map_err(|e| Error::new("EvalError", "parse_qrels", &e.to_string()));
    }

    Err(Error::new(
        "EvalError",
        "parse_qrels",
        "invalid qrels format: expected a JSON array or an object with a 'queries' field",
    ))
}

// ─────────────────────────────────────────────────────────────────
// Orchestrator
// ─────────────────────────────────────────────────────────────────

/// Resolve the per-query retrieval depth for an evaluation run.
///
/// Faithful to the TS `const limit = config.limit ?? Math.max(k * 2, 10)`:
/// the `max(…, 10)` floor applies ONLY to the derived default, never to an
/// explicit `config.limit`. (The earlier Rust form
/// `config.limit.unwrap_or(k * 2).max(10)` silently clamped an explicit
/// `limit: 3` up to 10, changing what the harness measured.)
///
/// Exposed because [`run_eval`] delegates the actual retrieval to a caller-
/// supplied `query_fn` — the caller must size its own backend request with the
/// *same* number, so this has to be one shared function rather than two copies
/// of the expression.
#[must_use]
pub fn resolve_eval_limit(config: &EvalConfig, k: usize) -> usize {
    config.limit.unwrap_or_else(|| (k * 2).max(10))
}

/// Run a full evaluation of one search configuration against all qrels.
///
/// `query_fn` injects the search strategy: given a query string it returns the
/// ranked list of slugs (see module docs for the rationale). `k` is the cutoff
/// used for the @k metrics. `on_progress`, if provided, is called once per
/// evaluated query.
///
/// Faithful to the TS `runEval`: retrieves up to
/// [`resolve_eval_limit`] (`config.limit ?? max(k*2, 10)`) hits per query, then
/// computes the per-query metrics and the mean aggregates.
pub async fn run_eval<F, Fut>(
    qrels: &[EvalQrel],
    config: &EvalConfig,
    k: usize,
    query_fn: F,
    on_progress: Option<&dyn Fn(usize, usize, &str)>,
) -> Result<EvalReport, Error>
where
    F: Fn(&str) -> Fut,
    Fut: std::future::Future<Output = Result<Vec<String>, Error>>,
{
    let limit = resolve_eval_limit(config, k);

    let mut query_results: Vec<QueryResult> = Vec::with_capacity(qrels.len());
    let total = qrels.len();
    let mut done = 0usize;

    for qrel in qrels {
        let hits: Vec<String> = query_fn(&qrel.query).await?;
        let hits: Vec<String> = hits.into_iter().take(limit).collect();

        let relevant_set: HashSet<String> = qrel.relevant.iter().cloned().collect();
        let grades_map = build_grades_map(qrel);

        query_results.push(QueryResult {
            query: qrel.query.clone(),
            hits: hits.clone(),
            precision_at_k: precision_at_k(&hits, &relevant_set, k),
            recall_at_k: recall_at_k(&hits, &relevant_set, k),
            mrr: mrr(&hits, &relevant_set),
            ndcg_at_k: ndcg_at_k(&hits, &grades_map, k),
        });

        done += 1;
        if let Some(cb) = on_progress {
            cb(done, total, &qrel.query);
        }
    }

    let mean_precision = mean(
        &query_results
            .iter()
            .map(|r| r.precision_at_k)
            .collect::<Vec<_>>(),
    );
    let mean_recall = mean(
        &query_results
            .iter()
            .map(|r| r.recall_at_k)
            .collect::<Vec<_>>(),
    );
    let mean_mrr = mean(&query_results.iter().map(|r| r.mrr).collect::<Vec<_>>());
    let mean_ndcg = mean(
        &query_results
            .iter()
            .map(|r| r.ndcg_at_k)
            .collect::<Vec<_>>(),
    );

    Ok(EvalReport {
        config: config.clone(),
        k,
        queries: query_results,
        mean_precision,
        mean_recall,
        mean_mrr,
        mean_ndcg,
    })
}

/// Convenience keyword-only query function backed by `engine.search_pages`.
///
/// Runs the lexical path (no `query_embedding`, so fusion degenerates to
/// keyword-only), returning ranked slugs. Useful as the `query_fn` for
/// `run_eval` when evaluating the keyword strategy without an embedding client.
pub async fn keyword_search_slugs(
    engine: &(dyn BrainEngine + '_),
    query: &str,
    limit: usize,
) -> Result<Vec<String>, Error> {
    let results: Vec<SearchResult> = engine
        .search_pages(&SearchOpts {
            keywords: vec![query.to_string()],
            limit: Some(limit),
            ..Default::default()
        })
        .await?;
    Ok(results.into_iter().map(|r| r.page.slug).collect())
}

// ─────────────────────────────────────────────────────────────────
// Tests
// ─────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::InMemoryEngine;

    fn hs(items: &[&str]) -> HashSet<String> {
        items.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn precision_at_k_divides_by_k_not_by_hits_len() {
        // Top-2 of [a,b,c] with relevant {a,c}: 1 relevant / 2 = 0.5.
        let hits = vec!["a".into(), "b".into(), "c".into()];
        assert!((precision_at_k(&hits, &hs(&["a", "c"]), 2) - 0.5).abs() < 1e-9);
        // Top-3: 2 relevant / 3.
        assert!((precision_at_k(&hits, &hs(&["a", "c"]), 3) - 2.0 / 3.0).abs() < 1e-9);
    }

    #[test]
    fn precision_at_k_edge_cases() {
        let hits = vec!["a".into(), "b".into()];
        assert_eq!(precision_at_k(&hits, &hs(&["a"]), 0), 0.0);
        assert_eq!(precision_at_k(&[], &hs(&["a"]), 5), 0.0);
        assert_eq!(precision_at_k(&hits, &HashSet::new(), 5), 0.0);
    }

    #[test]
    fn recall_at_k_divides_by_relevant_size() {
        let hits = vec!["a".into(), "b".into(), "c".into()];
        let relevant = hs(&["a", "c", "d", "e"]); // size 4
        // top-2 = {a,b}: 1 relevant / 4.
        assert!((recall_at_k(&hits, &relevant, 2) - 0.25).abs() < 1e-9);
        // top-3 = {a,b,c}: 2 relevant / 4.
        assert!((recall_at_k(&hits, &relevant, 3) - 0.5).abs() < 1e-9);
    }

    #[test]
    fn mrr_finds_first_relevant_rank() {
        let hits = vec!["x".into(), "a".into(), "y".into()];
        assert!((mrr(&hits, &hs(&["a"])) - 0.5).abs() < 1e-9);
        // No relevant hit -> 0.
        assert_eq!(mrr(&hits, &hs(&["z"])), 0.0);
        // First hit relevant -> 1.0.
        assert_eq!(mrr(&vec!["a".into(), "x".into()], &hs(&["a"])), 1.0);
    }

    #[test]
    fn ndcg_at_k_binary_perfect_and_partial() {
        let hits = vec!["a".into(), "b".into()];
        // Both relevant (grade 1), ideal order -> nDCG = 1.0.
        let grades = hs_grades(&[("a", 1.0), ("b", 1.0)]);
        assert!((ndcg_at_k(&hits, &grades, 2) - 1.0).abs() < 1e-9);

        // Swapped order with grades a:3, b:1 -> partial.
        let hits2 = vec!["b".into(), "a".into()];
        let grades2 = hs_grades(&[("a", 3.0), ("b", 1.0)]);
        let dcg = 1.0 / 2f64.log2() + 3.0 / 3f64.log2(); // 1 + 1.8928
        let idcg = 3.0 / 2f64.log2() + 1.0 / 3f64.log2(); // 3 + 0.6309
        let expected = dcg / idcg;
        assert!((ndcg_at_k(&hits2, &grades2, 2) - expected).abs() < 1e-9);
    }

    #[test]
    fn ndcg_at_k_edge_cases() {
        let hits = vec!["a".into()];
        assert_eq!(ndcg_at_k(&hits, &HashMap::new(), 5), 0.0);
        assert_eq!(ndcg_at_k(&hits, &hs_grades(&[("a", 1.0)]), 0), 0.0);
    }

    fn hs_grades(pairs: &[(&str, f64)]) -> HashMap<String, f64> {
        pairs
            .iter()
            .map(|(s, g)| (s.to_string(), *g))
            .collect()
    }

    #[test]
    fn build_grades_map_binary_vs_explicit() {
        let q = EvalQrel {
            id: None,
            query: "q".into(),
            relevant: vec!["a".into(), "b".into()],
            grades: None,
        };
        let g = build_grades_map(&q);
        assert_eq!(g.get("a"), Some(&1.0));
        assert_eq!(g.get("b"), Some(&1.0));

        let q2 = EvalQrel {
            id: None,
            query: "q".into(),
            relevant: vec!["a".into()],
            grades: Some(HashMap::from([("a".into(), 3.0)])),
        };
        assert_eq!(build_grades_map(&q2).get("a"), Some(&3.0));
    }

    #[test]
    fn parse_qrels_inline_array() {
        let json = r#"[{"query":"rust","relevant":["a","b"]}]"#;
        let q = parse_qrels(json).unwrap();
        assert_eq!(q.len(), 1);
        assert_eq!(q[0].query, "rust");
        assert_eq!(q[0].relevant, vec!["a", "b"]);
    }

    #[test]
    fn parse_qrels_inline_object() {
        let json = r#"{"version":1,"queries":[{"query":"x","relevant":["a"]}]}"#;
        let q = parse_qrels(json).unwrap();
        assert_eq!(q.len(), 1);
        assert_eq!(q[0].query, "x");
    }

    #[test]
    fn parse_qrels_file_round_trip() {
        let dir = std::env::temp_dir();
        let path = dir.join(format!("zbrain_qrels_test_{}.json", std::process::id()));
        let json = r#"[{"query":"q1","relevant":["a"]},{"query":"q2","relevant":["b"]}]"#;
        std::fs::write(&path, json).unwrap();
        let q = parse_qrels(path.to_str().unwrap()).unwrap();
        assert_eq!(q.len(), 2);
        assert_eq!(q[1].query, "q2");
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn parse_qrels_rejects_garbage() {
        assert!(parse_qrels("not json at all").is_err());
    }

    #[tokio::test]
    async fn run_eval_with_fake_closure() {
        // query_fn mirrors a perfect engine: returns relevant slugs first.
        let qrels = vec![
            EvalQrel {
                id: None,
                query: "q1".into(),
                relevant: vec!["a".into(), "b".into()],
                grades: None,
            },
            EvalQrel {
                id: None,
                query: "q2".into(),
                relevant: vec!["c".into()],
                grades: None,
            },
        ];
        let config = EvalConfig {
            limit: Some(10),
            ..Default::default()
        };
        let query_fn = |q: &str| {
            let out: Vec<String> = if q == "q1" {
                vec!["a".into(), "b".into(), "x".into()]
            } else {
                vec!["c".into()]
            };
            async move { Ok(out) }
        };

        let report = run_eval(&qrels, &config, 5, query_fn, None).await.unwrap();
        assert_eq!(report.k, 5);
        assert_eq!(report.queries.len(), 2);

        // q1: top-5 = [a,b,x]; P@5 = 2/5; R@5 = 2/2; MRR = 1; nDCG = 1.
        let q1 = &report.queries[0];
        assert!((q1.precision_at_k - 0.4).abs() < 1e-9);
        assert!((q1.recall_at_k - 1.0).abs() < 1e-9);
        assert!((q1.mrr - 1.0).abs() < 1e-9);
        assert!((q1.ndcg_at_k - 1.0).abs() < 1e-9);

        // q2: single relevant hit at rank 1. P@5 = 1/5 (metric divides by k, not hits);
        // R@5 = 1/1; MRR = 1; nDCG = 1.
        let q2 = &report.queries[1];
        assert!((q2.precision_at_k - 0.2).abs() < 1e-9);
        assert!((q2.recall_at_k - 1.0).abs() < 1e-9);

        // Means: P = (0.4 + 0.2)/2 = 0.3; R/MRR/nDCG = 1.0.
        assert!((report.mean_precision - 0.3).abs() < 1e-9);
        assert!((report.mean_recall - 1.0).abs() < 1e-9);
        assert!((report.mean_mrr - 1.0).abs() < 1e-9);
        assert!((report.mean_ndcg - 1.0).abs() < 1e-9);
    }

    #[test]
    fn resolve_eval_limit_honors_explicit_limit_below_the_default_floor() {
        // Regression: the `max(…, 10)` floor belongs to the DERIVED default
        // only. TS: `config.limit ?? Math.max(k * 2, 10)`. An earlier Rust
        // form (`unwrap_or(k * 2).max(10)`) clamped an explicit `limit: 3`
        // up to 10, so `--limit 3` measured a 10-deep result set.
        let explicit = EvalConfig { limit: Some(3), ..Default::default() };
        assert_eq!(resolve_eval_limit(&explicit, 5), 3);

        // Derived default: k*2 when that clears the floor…
        let derived = EvalConfig::default();
        assert_eq!(resolve_eval_limit(&derived, 20), 40);
        // …and the floor when it does not.
        assert_eq!(resolve_eval_limit(&derived, 2), 10);
    }

    #[tokio::test]
    async fn run_eval_truncates_hits_to_an_explicit_small_limit() {
        // End-to-end guard for the same bug: with `limit: 2` the harness must
        // only score the top-2 hits, so the third (relevant) slug is invisible.
        let qrels = vec![EvalQrel {
            id: None,
            query: "q".into(),
            relevant: vec!["c".into()],
            grades: None,
        }];
        let config = EvalConfig { limit: Some(2), ..Default::default() };
        let query_fn = |_q: &str| async move {
            Ok(vec!["a".to_string(), "b".to_string(), "c".to_string()])
        };

        let report = run_eval(&qrels, &config, 5, query_fn, None).await.unwrap();
        assert_eq!(report.queries[0].hits, vec!["a".to_string(), "b".to_string()]);
        // "c" was truncated away, so nothing relevant was retrieved.
        assert!((report.mean_recall - 0.0).abs() < 1e-9);
    }

    #[tokio::test]
    async fn run_eval_on_real_inmemory_engine_keyword() {
        let engine = std::sync::Arc::new(InMemoryEngine::default());
        for (slug, body) in [
            ("rust", "Rust is a systems programming language"),
            ("python", "Python is a dynamic language"),
            ("go", "Go is another systems language"),
        ] {
            engine
                .put_page(
                    slug,
                    Some("default"),
                    &PageInput {
                        page_type: "note".to_string(),
                        title: slug.to_string(),
                        compiled_truth: body.to_string(),
                        ..Default::default()
                    },
                )
                .await
                .unwrap();
        }

        let qrels = vec![EvalQrel {
            id: None,
            query: "systems".into(),
            relevant: vec!["rust".into(), "go".into()],
            grades: None,
        }];
        let config = EvalConfig {
            strategy: Some(EvalStrategy::Keyword),
            limit: Some(10),
            ..Default::default()
        };

        let query_fn = move |q: &str| {
            let eng = std::sync::Arc::clone(&engine);
            let q = q.to_string();
            async move { keyword_search_slugs(&*eng, &q, 10).await }
        };

        let report = run_eval(&qrels, &config, 5, query_fn, None).await.unwrap();
        assert_eq!(report.queries.len(), 1);
        // Both relevant slugs should be retrieved by keyword search.
        assert!(!report.queries[0].hits.is_empty());
        // At least the two relevant slugs are present in the hits.
        let hits: HashSet<String> = report.queries[0].hits.iter().cloned().collect();
        assert!(hits.contains("rust"));
        assert!(hits.contains("go"));
        assert!((report.queries[0].recall_at_k - 1.0).abs() < 1e-9);
    }
}
