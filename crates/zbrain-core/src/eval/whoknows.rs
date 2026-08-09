//! Two-layer whoknows eval gate (`zbrain eval-whoknows`).
//!
//! Ports `src/commands/eval-whoknows.ts` (deleted under `3c09a69f`; recovered
//! from `3c09a69f^`). v0.33 two-layer eval gate (ENG-D2) for naive zbrain
//! whoknows:
//!
//! **Layer 1 (PRIMARY, ship-blocking)**: hand-labeled fixture. For each
//! `{query, expected_top_3_slugs}`, run whoknows and check whether top-3
//! result slugs intersect with `expected_top_3_slugs`. Pass =
//! `HIT_RATE_THRESHOLD` (0.8) or higher.
//!
//! **Layer 2 (SECONDARY, ship-blocking when data exists)**: eval_candidates
//! replay. Stream rows where `tool_name == 'query'`; re-run whoknows and
//! compute set-Jaccard@3 between current output and captured `retrieved_slugs`.
//! Pass = `REGRESSION_THRESHOLD` (0.4) mean Jaccard. Sparseness fallback:
//! if fewer than `MIN_REPLAY_ROWS` (20) replay-eligible rows exist, the
//! regression gate auto-disables and the verdict is decided by Layer 1 alone.
//!
//! Faithful behavior preserved:
//!   * The whoknows callable is injected generically so the core stays pure
//!     and impl-agnostic (local `find_experts` vs thin-client remote op — the
//!     CLI wires the local path).
//!   * `jaccard_at_k` slices only the first k of each list; empty∩empty = 1.0
//!     (vacuously stable), empty∩nonempty = 0.
//!   * `top_k_hit` scans the first min(k, len(actual)) results.
//!   * Regression gate auto-skips (with reason) when < MIN_REPLAY_ROWS rows.

use serde::{Deserialize, Serialize};
use std::future::Future;

use crate::Result;

pub const HIT_RATE_THRESHOLD: f64 = 0.8;
pub const REGRESSION_THRESHOLD: f64 = 0.4;
pub const MIN_REPLAY_ROWS: usize = 20;

/// A hand-labeled fixture row (JSONL).
#[derive(Clone, Debug, Deserialize)]
pub struct FixtureRow {
    pub query: String,
    #[serde(default)]
    pub expected_top_3_slugs: Vec<String>,
    #[serde(default)]
    pub notes: Option<String>,
}

/// Per-query quality result.
#[derive(Clone, Debug, Serialize)]
pub struct QualityRowResult {
    pub query: String,
    pub expected: Vec<String>,
    pub actual_top_3: Vec<String>,
    pub hit: bool,
}

/// Layer-1 aggregate.
#[derive(Clone, Debug, Serialize)]
pub struct QualityReport {
    pub total: usize,
    pub hits: usize,
    pub hit_rate: f64,
    pub threshold: f64,
    pub passed: bool,
    pub rows: Vec<QualityRowResult>,
}

/// A captured replay row (query + retrieved slugs).
#[derive(Clone, Debug)]
pub struct ReplayRow {
    pub query: String,
    pub retrieved_slugs: Vec<String>,
}

/// Per-query regression result.
#[derive(Clone, Debug, Serialize)]
pub struct RegressionRowResult {
    pub query: String,
    pub captured: Vec<String>,
    pub current: Vec<String>,
    pub jaccard: f64,
}

/// Layer-2 aggregate.
#[derive(Clone, Debug, Serialize)]
pub struct RegressionReport {
    pub status: String, // "passed" | "failed" | "skipped"
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    pub total: usize,
    pub mean_jaccard: f64,
    pub threshold: f64,
    pub rows: Vec<RegressionRowResult>,
}

/// The full report envelope.
#[derive(Clone, Debug, Serialize)]
pub struct EvalWhoknowsReport {
    pub schema_version: u32,
    pub fixture_path: String,
    pub quality: QualityReport,
    pub regression: RegressionReport,
    pub overall_passed: bool,
    pub exit_code: u8,
}

/// The whoknows callable injected into the gates. Takes a topic + limit and
/// returns the ranked slugs. Generic over `F`/`Fut` (mirrors `eval::gate` and
/// `eval::replay`).
type WhoknowsFn<'a> = dyn Fn(&str, usize) -> Fut<'a> + 'a;
type Fut<'a> = std::pin::Pin<Box<dyn Future<Output = Result<Vec<String>>> + Send + 'a>>;

/// Parse a fixture JSONL string. Blank lines and `//`/`#` comment lines are
/// skipped. A malformed line or a row missing `query`/`expected_top_3_slugs`
/// throws.
pub fn read_fixture(content: &str) -> Result<Vec<FixtureRow>> {
    let mut rows: Vec<FixtureRow> = Vec::new();
    for (i, raw) in content.lines().enumerate() {
        let line = raw.trim();
        if line.is_empty() || line.starts_with("//") || line.starts_with('#') {
            continue;
        }
        let obj: serde_json::Value = serde_json::from_str(line).map_err(|_| {
            crate::Error::new(
                "EvalWhoknows",
                "read_fixture",
                &format!("malformed JSONL line {}: {}", i + 1, &line[..line.len().min(80)]),
            )
        })?;
        let query = obj.get("query").and_then(|v| v.as_str());
        let expected = obj.get("expected_top_3_slugs").and_then(|v| v.as_array());
        match (query, expected) {
            (Some(q), Some(e)) => {
                let expected_slugs: Vec<String> = e
                    .iter()
                    .filter_map(|s| s.as_str().map(str::to_string))
                    .collect();
                let notes = obj
                    .get("notes")
                    .and_then(|v| v.as_str())
                    .map(str::to_string);
                rows.push(FixtureRow {
                    query: q.to_string(),
                    expected_top_3_slugs: expected_slugs,
                    notes,
                });
            }
            _ => {
                return Err(crate::Error::new(
                    "EvalWhoknows",
                    "read_fixture",
                    &format!(
                        "fixture row {} missing required fields (query, expected_top_3_slugs): {}",
                        i + 1,
                        &line[..line.len().min(80)]
                    ),
                ));
            }
        }
    }
    Ok(rows)
}

/// Set-Jaccard@k between two slug lists, treating only the first `k` items of
/// each as the set. Empty intersection over empty union = 1.0 (vacuously
/// stable); empty intersection over non-empty union = 0.
pub fn jaccard_at_k(a: &[String], b: &[String], k: usize) -> f64 {
    use std::collections::HashSet;
    let set_a: HashSet<&str> = a.iter().take(k).map(String::as_str).collect();
    let set_b: HashSet<&str> = b.iter().take(k).map(String::as_str).collect();
    if set_a.is_empty() && set_b.is_empty() {
        return 1.0;
    }
    let intersect = set_a.intersection(&set_b).count();
    let union = set_a.len() + set_b.len() - intersect;
    if union == 0 {
        1.0
    } else {
        intersect as f64 / union as f64
    }
}

/// True if any of the first `k` actual slugs appears in `expected`.
pub fn top_k_hit(actual: &[String], expected: &[String], k: usize) -> bool {
    use std::collections::HashSet;
    let expected_set: HashSet<&str> = expected.iter().map(String::as_str).collect();
    let n = actual.len().min(k);
    for i in 0..n {
        if expected_set.contains(actual[i].as_str()) {
            return true;
        }
    }
    false
}

/// Run Layer 1 (quality gate) over the fixture.
pub async fn run_quality_gate<F, Fut>(
    whoknows: &F,
    fixture: &[FixtureRow],
    limit: usize,
) -> QualityReport
where
    F: Fn(&str, usize) -> Fut,
    Fut: Future<Output = Result<Vec<String>>>,
{
    let mut rows: Vec<QualityRowResult> = Vec::with_capacity(fixture.len());
    for row in fixture {
        let results = match whoknows(&row.query, limit).await {
            Ok(slugs) => slugs,
            Err(_) => Vec::new(),
        };
        let actual_top_3: Vec<String> = results.into_iter().take(3).collect();
        let hit = top_k_hit(&actual_top_3, &row.expected_top_3_slugs, 3);
        rows.push(QualityRowResult {
            query: row.query.clone(),
            expected: row.expected_top_3_slugs.clone(),
            actual_top_3,
            hit,
        });
    }
    let hits = rows.iter().filter(|r| r.hit).count();
    let hit_rate = if rows.is_empty() { 0.0 } else { hits as f64 / rows.len() as f64 };
    QualityReport {
        total: rows.len(),
        hits,
        hit_rate,
        threshold: HIT_RATE_THRESHOLD,
        passed: hit_rate >= HIT_RATE_THRESHOLD,
        rows,
    }
}

/// Run Layer 2 (regression gate) over captured replay rows. If fewer than
/// `MIN_REPLAY_ROWS` rows are provided, the gate is skipped.
pub async fn run_regression_gate<F, Fut>(
    whoknows: &F,
    captured: &[ReplayRow],
    limit: usize,
) -> RegressionReport
where
    F: Fn(&str, usize) -> Fut,
    Fut: Future<Output = Result<Vec<String>>>,
{
    if captured.len() < MIN_REPLAY_ROWS {
        return RegressionReport {
            status: "skipped".to_string(),
            reason: Some(format!(
                "only {} replay-eligible eval_candidates rows (< {} threshold)",
                captured.len(),
                MIN_REPLAY_ROWS
            )),
            total: captured.len(),
            mean_jaccard: 0.0,
            threshold: REGRESSION_THRESHOLD,
            rows: Vec::new(),
        };
    }

    let mut rows: Vec<RegressionRowResult> = Vec::with_capacity(captured.len());
    for r in captured {
        let current = match whoknows(&r.query, limit).await {
            Ok(slugs) => slugs,
            Err(_) => Vec::new(),
        };
        let current_slugs: Vec<String> = current.into_iter().take(3).collect();
        let captured_slugs: Vec<String> = r.retrieved_slugs.iter().take(3).cloned().collect();
        let j = jaccard_at_k(&current_slugs, &captured_slugs, 3);
        rows.push(RegressionRowResult {
            query: r.query.clone(),
            captured: captured_slugs,
            current: current_slugs,
            jaccard: j,
        });
    }
    let mean_jaccard =
        rows.iter().map(|r| r.jaccard).sum::<f64>() / rows.len().max(1) as f64;
    RegressionReport {
        status: if mean_jaccard >= REGRESSION_THRESHOLD {
            "passed".to_string()
        } else {
            "failed".to_string()
        },
        reason: None,
        total: rows.len(),
        mean_jaccard,
        threshold: REGRESSION_THRESHOLD,
        rows,
    }
}

/// Assemble the full report envelope and overall verdict. `regression` is
/// passed in pre-built; the caller decides skip semantics. Returns the report
/// (with `exit_code` 0/1 set from the overall verdict).
pub fn assemble_report(
    fixture_path: &str,
    quality: QualityReport,
    regression: RegressionReport,
) -> EvalWhoknowsReport {
    let regression_passed = regression.status != "failed";
    let overall = quality.passed && regression_passed;
    EvalWhoknowsReport {
        schema_version: 1,
        fixture_path: fixture_path.to_string(),
        quality,
        regression,
        overall_passed: overall,
        exit_code: if overall { 0 } else { 1 },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn read_fixture_parses_rows_and_skips_comments() {
        let content = "# a comment\n// another\n{\"query\":\"a\",\"expected_top_3_slugs\":[\"x\"]}\n\n{\"query\":\"b\",\"expected_top_3_slugs\":[\"y\",\"z\"],\"notes\":\"hi\"}\n";
        let rows = read_fixture(content).unwrap();
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].query, "a");
        assert_eq!(rows[0].expected_top_3_slugs, vec!["x"]);
        assert_eq!(rows[1].query, "b");
        assert_eq!(rows[1].expected_top_3_slugs, vec!["y", "z"]);
        assert_eq!(rows[1].notes.as_deref(), Some("hi"));
    }

    #[test]
    fn read_fixture_rejects_malformed_line() {
        let err = read_fixture("this is not json\n").unwrap_err();
        assert!(err.to_string().contains("malformed"));
    }

    #[test]
    fn read_fixture_rejects_missing_required_fields() {
        let err = read_fixture("{\"query\":\"a\"}\n").unwrap_err();
        assert!(err.to_string().contains("missing required fields"));
    }

    #[test]
    fn jaccard_at_k_slices_first_k() {
        // a = [x, y, z], b = [x, y, w]; k=2 → {x,y}∩{x,y}=2, union 2 → 1.0
        let a = vec!["x".to_string(), "y".to_string(), "z".to_string()];
        let b = vec!["x".to_string(), "y".to_string(), "w".to_string()];
        assert!((jaccard_at_k(&a, &b, 2) - 1.0).abs() < 1e-9);
        // k=3 → {x,y,z}∩{x,y,w}={x,y}(2), union {x,y,z,w}(4) → 0.5
        assert!((jaccard_at_k(&a, &b, 3) - 0.5).abs() < 1e-9);
    }

    #[test]
    fn jaccard_at_k_empty_both_is_one() {
        assert_eq!(jaccard_at_k(&[], &[], 3), 1.0);
        assert_eq!(jaccard_at_k(&["a".to_string()], &[], 3), 0.0);
    }

    #[test]
    fn top_k_hit_checks_first_k_only() {
        let actual = vec!["a".to_string(), "b".to_string(), "c".to_string()];
        let expected = vec!["c".to_string()];
        // k=2 → only a,b checked; c not hit
        assert!(!top_k_hit(&actual, &expected, 2));
        // k=3 → c hit
        assert!(top_k_hit(&actual, &expected, 3));
    }

    fn fake_whoknows(returns: &'static [(&'static str, &'static [&'static str])]) -> impl Fn(&str, usize) -> Fut<'static> + 'static {
        let map: std::collections::HashMap<&'static str, Vec<&'static str>> = returns
            .iter()
            .map(|(k, v)| (*k, v.to_vec()))
            .collect();
        move |topic: &str, _limit: usize| {
            let topic = topic.to_string();
            let map = map.clone();
            Box::pin(async move {
                Ok(map
                    .get(topic.as_str())
                    .cloned()
                    .unwrap_or_default()
                    .into_iter()
                    .map(|s| s.to_string())
                    .collect())
            })
        }
    }

    #[tokio::test]
    async fn quality_gate_counts_hits() {
        let fixture = read_fixture(
            "{\"query\":\"q1\",\"expected_top_3_slugs\":[\"a\"]}\n{\"query\":\"q2\",\"expected_top_3_slugs\":[\"z\"]}\n",
        )
        .unwrap();
        let wk = fake_whoknows(&[("q1", &["a", "b"]), ("q2", &["x"])]);
        let report = run_quality_gate(&wk, &fixture, 5).await;
        assert_eq!(report.total, 2);
        assert_eq!(report.hits, 1);
        assert!((report.hit_rate - 0.5).abs() < 1e-9);
        assert!(!report.passed); // 0.5 < 0.8
    }

    #[tokio::test]
    async fn regression_gate_skips_when_sparse() {
        let captured = vec![ReplayRow {
            query: "q".to_string(),
            retrieved_slugs: vec!["a".to_string()],
        }];
        let wk = fake_whoknows(&[]);
        let report = run_regression_gate(&wk, &captured, 5).await;
        assert_eq!(report.status, "skipped");
        assert!(report.reason.is_some());
    }

    #[tokio::test]
    async fn regression_gate_computes_mean_jaccard() {
        // 20 rows all matching exactly → jaccard 1.0 → passed
        let captured: Vec<ReplayRow> = (0..20)
            .map(|i| ReplayRow {
                query: format!("q{i}"),
                retrieved_slugs: vec!["a".to_string(), "b".to_string(), "c".to_string()],
            })
            .collect();
        // whoknows always returns [a,b,c] (exact match for every row).
        let wk = |_topic: &str, _limit: usize| {
            Box::pin(async move {
                Ok(vec![
                    "a".to_string(),
                    "b".to_string(),
                    "c".to_string(),
                ])
            })
        };
        let report = run_regression_gate(&wk, &captured, 5).await;
        assert_eq!(report.status, "passed");
        assert!((report.mean_jaccard - 1.0).abs() < 1e-9);
    }
}
