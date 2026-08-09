//! Replay captured eval candidates against the current brain (`zbrain eval-replay`).
//!
//! Ports `src/commands/eval-replay.ts` (deleted under `3c09a69f`; recovered
//! from `3c09a69f^`). Replay is the contributor-facing half of BrainBench-Real:
//!
//!   1. capture real traffic         (lands in `eval_candidates`)
//!   2. snapshot it                  (`zbrain eval-export --since 7d > baseline.ndjson`)
//!   3. make a code change           (tune RRF_K, edit hybrid, swap an embed model)
//!   4. replay against the snapshot  (`zbrain eval-replay --against baseline.ndjson`)
//!
//! Outputs three numbers a contributor can read at a glance:
//!   * mean Jaccard@k between captured `retrieved_slugs` and the current run's slugs
//!   * top-1 stability rate (was the #1 result the same?)
//!   * mean latency delta (current − captured, positive = slower now)
//!
//! Best-effort by design — replay is NOT pure (the brain has more pages than
//! at capture, embeddings may drift). The metrics answer "did this change hurt
//! retrieval on the queries you actually serve", not "do these match the
//! baseline byte-for-byte".
//!
//! Faithful behavior preserved:
//!   * Retrieval is injected via a generic async [`ReplayQueryFn`] so the core
//!     stays pure and testable (mirrors `eval::gate`). The CLI wires the real
//!     engine: `tool_name == "search"` → keyword path, `"query"` → hybrid path.
//!   * Set-Jaccard over slugs: order ignored, dupes collapsed; both empty → 1.0.
//!   * A per-row exception is recorded as `errored: true` and excluded from the
//!     aggregate rather than crashing the whole replay.
//!   * Empty queries are `skipped`, not counted as replayed.
//!   * NDJSON parsing skips the `_kind: "baseline_metadata"` header that
//!     `zbrain bench publish` writes, and validates `schema_version == 1`.
//!   * `--compare-limit` forces a constant K across modes so Jaccard@k measures
//!     quality drift, not K-drift.

use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::future::Future;

use crate::Result;

/// Which retrieval path a captured row exercised at capture time.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ReplayTool {
    /// Bare keyword search (`searchKeyword`).
    Search,
    /// Hybrid path (vector + keyword + RRF).
    Query,
}

impl Default for ReplayTool {
    fn default() -> Self {
        ReplayTool::Query
    }
}

/// A single captured row as written by `zbrain eval-export` (schema v1).
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CapturedRow {
    /// Optional, may be `absent`/`null` in the JSON. When `_kind` is
    /// `"baseline_metadata"` the row is a bench header and must be skipped.
    #[serde(default, rename = "_kind")]
    pub kind: Option<String>,
    #[serde(default)]
    pub schema_version: Option<u32>,
    #[serde(default)]
    pub id: i64,
    #[serde(default)]
    pub tool_name: ReplayTool,
    #[serde(default)]
    pub query: String,
    #[serde(default)]
    pub retrieved_slugs: Vec<String>,
    #[serde(default)]
    pub detail: Option<String>,
    #[serde(default)]
    pub expand_enabled: Option<bool>,
    #[serde(default)]
    pub embedding_column: Option<String>,
    #[serde(default)]
    pub latency_ms: i64,
}

/// Per-row replay result.
#[derive(Clone, Debug, Serialize)]
pub struct RowResult {
    pub id: i64,
    pub tool_name: ReplayTool,
    pub query: String,
    /// Set-overlap score in [0, 1]. 1.0 = identical retrieved set.
    pub jaccard: f64,
    /// True when the current top result matches the captured top result.
    pub top1_match: bool,
    /// Captured `retrieved_slugs` (as-is from NDJSON).
    pub captured_slugs: Vec<String>,
    /// Current run's slugs (deduped, in result order).
    pub current_slugs: Vec<String>,
    /// Wall-clock latency (ms) of the current re-run.
    pub current_latency_ms: i64,
    /// latency delta = current − captured. Positive = slower now.
    pub latency_delta_ms: i64,
    /// True if the row was skipped (e.g. captured query was empty).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub skipped: Option<bool>,
    /// Reason the row was skipped, if any.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub skip_reason: Option<String>,
    /// True if the row threw during replay; `current_slugs` is empty.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub errored: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error_message: Option<String>,
}

/// Aggregate summary across all rows.
#[derive(Clone, Debug, Serialize)]
pub struct ReplaySummary {
    pub rows_total: usize,
    pub rows_replayed: usize,
    pub rows_skipped: usize,
    pub rows_errored: usize,
    /// Mean Jaccard across non-skipped, non-errored rows.
    pub mean_jaccard: f64,
    pub top1_stability_rate: f64,
    pub mean_latency_delta_ms: f64,
    /// Rows where current latency is more than 2× captured (regression alarm).
    pub rows_over_2x_latency: usize,
}

/// Retrieval injected into the replay. `tool_name` lets the caller dispatch the
/// same logic that produced the original retrieval (keyword vs hybrid).
///
/// Generic over `F`/`Fut` (mirrors `eval::gate::run_correctness_gate`) so
/// callers can pass a plain closure returning an async block without boxing
/// to a trait object.
pub type ReplayQueryFn<'a> = dyn Fn(&str, ReplayTool, usize) -> Fut<'a> + 'a;
type Fut<'a> = std::pin::Pin<Box<dyn Future<Output = Result<Vec<String>>> + Send + 'a>>;

/// Parse NDJSON. One object per non-blank line; a single bad line throws —
/// it's a corrupt export and silently dropping rows would mask real bugs.
///
/// Skips the `_kind: "baseline_metadata"` header line that `zbrain bench
/// publish` writes, and rejects anything without `schema_version == 1`.
pub fn parse_ndjson(content: &str) -> Result<Vec<CapturedRow>> {
    let mut rows: Vec<CapturedRow> = Vec::new();
    for (i, raw) in content.lines().enumerate() {
        let line = raw.trim();
        if line.is_empty() {
            continue;
        }
        let row: CapturedRow = serde_json::from_str(line).map_err(|e| {
            crate::Error::new(
                "EvalReplay",
                "parse_ndjson",
                &format!("NDJSON parse error on line {}: {}", i + 1, e),
            )
        })?;
        // Drop bench metadata headers before they can pollute counts.
        if row.kind.as_deref() == Some("baseline_metadata") {
            continue;
        }
        match row.schema_version {
            None => {
                return Err(crate::Error::new(
                    "EvalReplay",
                    "parse_ndjson",
                    &format!(
                        "Line {} missing schema_version — not from `zbrain eval-export`?",
                        i + 1
                    ),
                ));
            }
            Some(1) => {}
            Some(v) => {
                return Err(crate::Error::new(
                    "EvalReplay",
                    "parse_ndjson",
                    &format!(
                        "Line {} has schema_version={v}; this replay only supports v1. \
                         Upgrade zbrain or re-export.",
                        i + 1
                    ),
                ));
            }
        }
        rows.push(row);
    }
    Ok(rows)
}

/// Set-Jaccard between two slug arrays. Order ignored, dupes collapsed.
/// Both empty → 1.0 (identical empty sets, no information lost).
pub fn jaccard_slugs(a: &[String], b: &[String]) -> f64 {
    let set_a: HashSet<&str> = a.iter().map(String::as_str).collect();
    let set_b: HashSet<&str> = b.iter().map(String::as_str).collect();
    if set_a.is_empty() && set_b.is_empty() {
        return 1.0;
    }
    let intersection = set_a.intersection(&set_b).count();
    let union = set_a.len() + set_b.len() - intersection;
    if union == 0 {
        1.0
    } else {
        intersection as f64 / union as f64
    }
}

/// Replay a single captured row, returning its per-row result.
async fn replay_row<F, Fut>(
    query_fn: &F,
    row: &CapturedRow,
    compare_limit: Option<usize>,
) -> RowResult
where
    F: Fn(&str, ReplayTool, usize) -> Fut,
    Fut: Future<Output = Result<Vec<String>>>,
{
    let captured_slugs = row.retrieved_slugs.clone();
    let started = std::time::Instant::now();

    // Default replay limit matches hybridSearch's default (20). When
    // `--compare-limit` is set it forces a constant K across modes and
    // overrides the captured K.
    let limit = compare_limit.unwrap_or_else(|| captured_slugs.len().max(20));

    let current: Vec<String> = match query_fn(&row.query, row.tool_name, limit).await {
        Ok(slugs) => slugs,
        Err(e) => {
            let elapsed_ms = started.elapsed().as_millis() as i64;
            return RowResult {
                id: row.id,
                tool_name: row.tool_name,
                query: row.query.clone(),
                jaccard: 0.0,
                top1_match: false,
                captured_slugs,
                current_slugs: Vec::new(),
                current_latency_ms: elapsed_ms,
                latency_delta_ms: elapsed_ms - row.latency_ms,
                skipped: None,
                skip_reason: None,
                errored: Some(true),
                error_message: Some(e.to_string()),
            };
        }
    };

    let current_latency_ms = started.elapsed().as_millis() as i64;
    // Dedup slugs while preserving order — same convention as search results.
    let mut seen: HashSet<String> = HashSet::new();
    let mut current_slugs: Vec<String> = Vec::new();
    for s in current {
        if seen.insert(s.clone()) {
            current_slugs.push(s);
        }
    }

    RowResult {
        id: row.id,
        tool_name: row.tool_name,
        query: row.query.clone(),
        jaccard: jaccard_slugs(&captured_slugs, &current_slugs),
        top1_match: captured_slugs
            .first()
            .map(|c| current_slugs.first() == Some(c))
            .unwrap_or(false),
        captured_slugs,
        current_slugs,
        current_latency_ms,
        latency_delta_ms: current_latency_ms - row.latency_ms,
        skipped: None,
        skip_reason: None,
        errored: None,
        error_message: None,
    }
}

/// Summarize per-row results into the aggregate [`ReplaySummary`].
pub fn summarize(results: &[RowResult]) -> ReplaySummary {
    let eligible: Vec<&RowResult> = results
        .iter()
        .filter(|r| r.skipped != Some(true) && r.errored != Some(true))
        .collect();
    let n = eligible.len();
    let mean = |sum: f64| if n == 0 { 0.0 } else { sum / n as f64 };
    let mean_jaccard = mean(eligible.iter().map(|r| r.jaccard).sum());
    let top1_rate = mean(eligible.iter().map(|r| if r.top1_match { 1.0 } else { 0.0 }).sum());
    let mean_latency_delta =
        mean(eligible.iter().map(|r| r.latency_delta_ms as f64).sum());
    // Rows where current latency is more than 2× captured latency
    // (current_latency_ms − latency_delta_ms == captured latency).
    let over_2x = eligible
        .iter()
        .filter(|r| {
            let captured = r.current_latency_ms - r.latency_delta_ms;
            captured >= 0 && r.current_latency_ms > 2 * captured
        })
        .count();

    ReplaySummary {
        rows_total: results.len(),
        rows_replayed: eligible.len(),
        rows_skipped: results.iter().filter(|r| r.skipped == Some(true)).count(),
        rows_errored: results.iter().filter(|r| r.errored == Some(true)).count(),
        mean_jaccard,
        top1_stability_rate: top1_rate,
        mean_latency_delta_ms: mean_latency_delta,
        rows_over_2x_latency: over_2x,
    }
}

/// Full replay over the NDJSON content. Returns the aggregate summary plus
/// every per-row result (mirrors the TS `replayCore` programmatic entrypoint).
pub async fn replay_core<F, Fut>(
    query_fn: &F,
    content: &str,
    limit: Option<usize>,
    compare_limit: Option<usize>,
) -> Result<(ReplaySummary, Vec<RowResult>)>
where
    F: Fn(&str, ReplayTool, usize) -> Fut,
    Fut: Future<Output = Result<Vec<String>>>,
{
    let rows = parse_ndjson(content)?;
    if rows.is_empty() {
        return Err(crate::Error::new(
            "EvalReplay",
            "replay_core",
            "NDJSON contains no rows (empty export)",
        ));
    }
    let capped: Vec<&CapturedRow> = match limit {
        Some(l) if l > 0 => rows.iter().take(l).collect(),
        _ => rows.iter().collect(),
    };

    let mut results: Vec<RowResult> = Vec::with_capacity(capped.len());
    for row in capped {
        if row.query.is_empty() {
            results.push(RowResult {
                id: row.id,
                tool_name: row.tool_name,
                query: row.query.clone(),
                jaccard: 0.0,
                top1_match: false,
                captured_slugs: row.retrieved_slugs.clone(),
                current_slugs: Vec::new(),
                current_latency_ms: 0,
                latency_delta_ms: 0,
                skipped: Some(true),
                skip_reason: Some("empty query".to_string()),
                errored: None,
                error_message: None,
            });
            continue;
        }
        let r = replay_row(query_fn, row, compare_limit).await;
        results.push(r);
    }

    Ok((summarize(&results), results))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn captured(over: serde_json::Value) -> serde_json::Value {
        let mut base = serde_json::json!({
            "schema_version": 1,
            "id": 1,
            "tool_name": "search",
            "query": "alice",
            "retrieved_slugs": ["people/alice", "people/alice-bio"],
            "retrieved_chunk_ids": [],
            "source_ids": [],
            "expand_enabled": null,
            "detail": null,
            "detail_resolved": null,
            "vector_enabled": false,
            "expansion_applied": false,
            "latency_ms": 50,
            "remote": true,
            "job_id": null,
            "subagent_id": null,
            "created_at": "2026-04-25T00:00:00Z"
        });
        let obj = base.as_object_mut().unwrap();
        if let Some(over_obj) = over.as_object() {
            for (k, v) in over_obj {
                obj.insert(k.clone(), v.clone());
            }
        }
        base
    }

    use std::collections::HashMap;

    #[test]
    fn empty_ndjson_is_rejected() {
        assert!(parse_ndjson("").unwrap().is_empty());
    }

    #[test]
    fn jaccard_both_empty_is_one() {
        assert_eq!(jaccard_slugs(&[], &[]), 1.0);
    }

    #[test]
    fn jaccard_identical_is_one() {
        let a = vec!["x".to_string(), "y".to_string()];
        let b = vec!["y".to_string(), "x".to_string()]; // order ignored, dupes
        assert_eq!(jaccard_slugs(&a, &b), 1.0);
    }

    #[test]
    fn jaccard_disjoint_is_zero() {
        let a = vec!["a".to_string()];
        let b = vec!["b".to_string()];
        assert_eq!(jaccard_slugs(&a, &b), 0.0);
    }

    #[test]
    fn jaccard_partial_overlap() {
        // {a,b} ∩ {a,c} = {a}(1), union {a,b,c}(3) → 1/3
        let a = vec!["a".to_string(), "b".to_string()];
        let b = vec!["a".to_string(), "c".to_string()];
        assert!((jaccard_slugs(&a, &b) - 1.0 / 3.0).abs() < 1e-9);
    }

    #[test]
    fn baseline_metadata_header_is_skipped() {
        let content = serde_json::json!({
            "_kind": "baseline_metadata",
            "label": "my-baseline"
        })
        .to_string();
        let rows = parse_ndjson(&content).unwrap();
        assert!(rows.is_empty());
    }

    #[test]
    fn missing_schema_version_rejected() {
        let content = "{\"id\":1,\"tool_name\":\"search\",\"query\":\"q\",\"retrieved_slugs\":[]}";
        let err = parse_ndjson(content).unwrap_err();
        assert!(err.to_string().contains("schema_version"));
    }

    #[test]
    fn future_schema_version_rejected() {
        let content = "{\"schema_version\":2,\"id\":1,\"tool_name\":\"search\",\"query\":\"q\",\"retrieved_slugs\":[],\"latency_ms\":0}";
        let err = parse_ndjson(content).unwrap_err();
        assert!(err.to_string().to_lowercase().contains("upgrade zbrain"));
    }

    #[test]
    fn malformed_line_reports_line_number() {
        let content = "{\"schema_version\":1,\"id\":1}\nthis is not json\n";
        let err = parse_ndjson(content).unwrap_err();
        assert!(err.to_string().contains("line 2"));
    }

    fn single_query_fn(returns: &'static [(&'static str, &'static [&'static str])]) -> impl Fn(&str, ReplayTool, usize) -> Fut<'static> + 'static {
        let map: HashMap<&'static str, Vec<&'static str>> = returns
            .iter()
            .map(|(k, v)| (*k, v.to_vec()))
            .collect();
        move |q: &str, _t: ReplayTool, _l: usize| {
            let q = q.to_string();
            let map = map.clone();
            Box::pin(async move {
                Ok(map.get(q.as_str()).cloned().unwrap_or_default().into_iter().map(|s| s.to_string()).collect())
            })
        }
    }

    /// Join captured-row JSON objects into proper NDJSON (one object per line),
    /// matching what `zbrain eval-export` actually emits.
    fn ndjson(rows: Vec<serde_json::Value>) -> String {
        rows.into_iter()
            .map(|r| r.to_string())
            .collect::<Vec<_>>()
            .join("\n")
            + "\n"
    }

    #[tokio::test]
    async fn identical_slugs_report_jaccard_one() {
        let content = ndjson(vec![captured(serde_json::json!({
            "tool_name": "search", "query": "alice",
            "retrieved_slugs": ["people/alice", "people/alice-bio"]
        }))]);
        let qf = single_query_fn(&[("alice", &["people/alice", "people/alice-bio"])]);
        let (summary, results) = replay_core(&qf, &content, None, None).await.unwrap();
        assert_eq!(summary.rows_replayed, 1);
        assert_eq!(summary.mean_jaccard, 1.0);
        assert_eq!(summary.top1_stability_rate, 1.0);
        assert_eq!(results[0].jaccard, 1.0);
    }

    #[tokio::test]
    async fn disjoint_slugs_report_jaccard_zero() {
        let content = ndjson(vec![captured(serde_json::json!({
            "tool_name": "search", "query": "bob",
            "retrieved_slugs": ["people/bob", "people/bob-bio"]
        }))]);
        let qf = single_query_fn(&[("bob", &["people/charlie", "people/charlie-bio"])]);
        let (summary, _) = replay_core(&qf, &content, None, None).await.unwrap();
        assert_eq!(summary.mean_jaccard, 0.0);
        assert_eq!(summary.top1_stability_rate, 0.0);
    }

    #[tokio::test]
    async fn partial_overlap_top1_stable() {
        // captured [a,b], current [a,c] → Jaccard 1/3, top-1 still matches.
        let content = ndjson(vec![captured(serde_json::json!({
            "retrieved_slugs": ["a", "b"], "query": "q"
        }))]);
        let qf = single_query_fn(&[("q", &["a", "c"])]);
        let (summary, _) = replay_core(&qf, &content, None, None).await.unwrap();
        assert!((summary.mean_jaccard - 1.0 / 3.0).abs() < 1e-9);
        assert_eq!(summary.top1_stability_rate, 1.0);
    }

    #[tokio::test]
    async fn top1_mismatch_reduces_stability() {
        // Same set but top-1 swapped a→b: jaccard 1.0, stability 0.
        let content = ndjson(vec![captured(serde_json::json!({
            "retrieved_slugs": ["a", "b", "c"], "query": "q"
        }))]);
        let qf = single_query_fn(&[("q", &["b", "a", "c"])]);
        let (summary, _) = replay_core(&qf, &content, None, None).await.unwrap();
        assert_eq!(summary.mean_jaccard, 1.0);
        assert_eq!(summary.top1_stability_rate, 0.0);
    }

    #[tokio::test]
    async fn multiple_rows_averaged() {
        let content = ndjson(vec![
            captured(serde_json::json!({"id": 1, "query": "q1", "retrieved_slugs": ["a"]})),
            captured(serde_json::json!({"id": 2, "query": "q2", "retrieved_slugs": ["b"]})),
        ]);
        let qf = single_query_fn(&[("q1", &["a"]), ("q2", &["z"])]);
        let (summary, _) = replay_core(&qf, &content, None, None).await.unwrap();
        assert!((summary.mean_jaccard - 0.5).abs() < 1e-9); // (1.0 + 0)/2
        assert!((summary.top1_stability_rate - 0.5).abs() < 1e-9);
        assert_eq!(summary.rows_replayed, 2);
    }

    #[tokio::test]
    async fn limit_caps_replay_count() {
        let content = ndjson([1, 2, 3, 4, 5].iter().map(|i| captured(
            serde_json::json!({"id": i, "query": format!("q{i}"), "retrieved_slugs": ["a"]})
        )).collect());
        let qf = single_query_fn(&[]);
        let (summary, _) = replay_core(&qf, &content, Some(2), None).await.unwrap();
        assert_eq!(summary.rows_total, 2);
    }

    #[tokio::test]
    async fn empty_query_is_skipped() {
        let content = ndjson(vec![captured(serde_json::json!({
            "query": "", "retrieved_slugs": []
        }))]);
        let qf = single_query_fn(&[]);
        let (summary, _) = replay_core(&qf, &content, None, None).await.unwrap();
        assert_eq!(summary.rows_skipped, 1);
        assert_eq!(summary.rows_replayed, 0);
    }

    #[tokio::test]
    async fn row_that_errors_is_recorded_not_crash() {
        let content = ndjson(vec![captured(serde_json::json!({
            "query": "boom"
        }))]);
        let qf = |_q: &str, _t: ReplayTool, _l: usize| {
            Box::pin(async move {
                Err(crate::Error::new("EvalReplay", "test", "engine offline"))
            })
        };
        let (summary, results) = replay_core(&qf, &content, None, None).await.unwrap();
        assert_eq!(summary.rows_errored, 1);
        assert_eq!(summary.rows_replayed, 0);
        assert_eq!(results[0].errored, Some(true));
    }
}
