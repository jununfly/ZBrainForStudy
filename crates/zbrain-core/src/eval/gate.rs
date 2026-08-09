//! Correctness gate for `zbrain eval-gate` (the qrels half of the TS
//! `eval-gate` command).
//!
//! Ports `src/core/bench/correctness-gate.ts` + `src/core/bench/qrels-file.ts`
//! (deleted under `bcafcafd`; recovered from `bcafcafd^`). The regression half
//! (`--baseline`, replay) is tracked separately (1-1-4 stage 4) and is NOT
//! implemented here.
//!
//! Faithful behavior preserved:
//!   * Compares on `${source_id}::${slug}` keys (eng-D5) so multi-source
//!     brains don't false-pass via wrong-source hits.
//!   * `recall@k` reuses [`zbrain_core::search::eval::recall_at_k`].
//!   * A per-query exception (Finding 2D) is recorded as `errored: true` and
//!     surfaces as a `queries_errored` breach rather than being silently
//!     dropped from the aggregate.
//!   * `expected_top1` is only enforced when at least one query sets it.
//!   * Thresholds fall back to [`DEFAULT_QRELS_THRESHOLDS`] when CLI flags are
//!     absent (CLI > embedded > defaults — but this Rust port has no embedded
//!     baseline, so CLI > defaults).

use crate::engine::SearchOpts;
use crate::Error;
use crate::Result;
use crate::search::eval::recall_at_k;
use serde::{Deserialize, Serialize};
use std::collections::HashSet;

// ── Qrels file types (federated, eng-D5) ──────────────────────────

/// Canonical `${source_id}::${slug}` reference used for all comparisons.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct QrelsRef {
    pub source_id: String,
    pub slug: String,
}

impl QrelsRef {
    /// Build the canonical compare key.
    pub fn key(&self) -> String {
        format!("{}::{}", self.source_id, self.slug)
    }
}

/// Wire schema version for the qrels file (`{ "schema_version": 1, ... }`).
pub const QRELS_FILE_SCHEMA_VERSION: u8 = 1;

/// A single qrels entry (normalized). Plain `relevant_slugs` strings are
/// promoted to `source_id: "default"`; `expected_top1` mirrors `first_relevant_slug`.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct QrelsEntry {
    pub query_id: String,
    pub query: String,
    pub relevant: Vec<QrelsRef>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expected_top1: Option<QrelsRef>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
}

/// A parsed qrels file (object shape, NOT a bare array).
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct QrelsFile {
    pub schema_version: u8,
    pub queries: Vec<QrelsEntry>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub _description: Option<String>,
}

/// Parse error for [`parse_qrels_file`].
#[derive(Debug)]
pub struct QrelsParseError(pub String);

impl std::fmt::Display for QrelsParseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "qrels parse error: {}", self.0)
    }
}

impl std::error::Error for QrelsParseError {}

impl From<QrelsParseError> for Error {
    fn from(e: QrelsParseError) -> Self {
        Error::new("EvalError", "parse_qrels_file", &e.0)
    }
}

/// Parse a qrels file (object shape). Faithful to `parseQrelsFile`:
/// requires `schema_version == 1`, a non-empty `queries` array, and per-entry
/// `query` + non-empty relevant set. Accepts BOTH the federated shape
/// (`relevant` + `expected_top1`) and the legacy slug-only shape
/// (`relevant_slugs` + `first_relevant_slug`, auto-defaulted to source_id
/// `'default'`).
pub fn parse_qrels_file(content: &str) -> std::result::Result<QrelsFile, QrelsParseError> {
    let v: serde_json::Value = serde_json::from_str(content)
        .map_err(|e| QrelsParseError(format!("malformed JSON: {e}")))?;

    if !v.is_object() {
        return Err(QrelsParseError(
            "qrels file must be a JSON object (got array or non-object). \
             Expected shape: {\"schema_version\":1,\"queries\":[...]}"
                .into(),
        ));
    }
    let schema_version = v.get("schema_version").and_then(|x| x.as_u64());
    if schema_version != Some(QRELS_FILE_SCHEMA_VERSION as u64) {
        return Err(QrelsParseError(format!(
            "unsupported schema_version {:?} (this zbrain build expects {})",
            schema_version, QRELS_FILE_SCHEMA_VERSION
        )));
    }
    let queries = v.get("queries").and_then(|x| x.as_array());
    let Some(queries) = queries else {
        return Err(QrelsParseError("qrels file missing \"queries\" array".into()));
    };
    if queries.is_empty() {
        return Err(QrelsParseError(
            "qrels file has empty \"queries\" array — at least one entry required".into(),
        ));
    }

    let mut out_queries = Vec::with_capacity(queries.len());
    for (i, entry) in queries.iter().enumerate() {
        if !entry.is_object() {
            return Err(QrelsParseError(format!("entry {i} is not a JSON object")));
        }
        let query_id = entry
            .get("query_id")
            .and_then(|x| x.as_str())
            .map(|s| s.to_string())
            .unwrap_or_else(|| format!("entry-{i}"));
        let query = entry
            .get("query")
            .and_then(|x| x.as_str())
            .filter(|s| !s.trim().is_empty())
            .ok_or_else(|| QrelsParseError(format!("entry {i} ({query_id}) missing or empty \"query\"")))?
            .to_string();

        // relevant: prefer federated `relevant`, fall back to legacy `relevant_slugs`.
        let relevant = if let Some(arr) = entry.get("relevant").and_then(|x| x.as_array()) {
            let mut refs = Vec::with_capacity(arr.len());
            for (j, r) in arr.iter().enumerate() {
                let obj = r.as_object().ok_or_else(|| {
                    QrelsParseError(format!("entry {i} ({query_id}) relevant[{j}] is not an object"))
                })?;
                let source_id = obj
                    .get("source_id")
                    .and_then(|x| x.as_str())
                    .ok_or_else(|| {
                        QrelsParseError(format!(
                            "entry {i} ({query_id}) relevant[{j}] missing source_id"
                        ))
                    })?
                    .to_string();
                let slug = obj
                    .get("slug")
                    .and_then(|x| x.as_str())
                    .ok_or_else(|| {
                        QrelsParseError(format!(
                            "entry {i} ({query_id}) relevant[{j}] missing slug"
                        ))
                    })?
                    .to_string();
                refs.push(QrelsRef { source_id, slug });
            }
            refs
        } else if let Some(arr) = entry.get("relevant_slugs").and_then(|x| x.as_array()) {
            let mut refs = Vec::with_capacity(arr.len());
            for (j, s) in arr.iter().enumerate() {
                let slug = s.as_str().ok_or_else(|| {
                    QrelsParseError(format!(
                        "entry {i} ({query_id}) relevant_slugs[{j}] is not a string"
                    ))
                })?;
                refs.push(QrelsRef {
                    source_id: "default".into(),
                    slug: slug.to_string(),
                });
            }
            refs
        } else {
            return Err(QrelsParseError(format!(
                "entry {i} ({query_id}) missing \"relevant\" or \"relevant_slugs\""
            )));
        };
        if relevant.is_empty() {
            return Err(QrelsParseError(format!(
                "entry {i} ({query_id}) has empty relevant set"
            )));
        }

        // expected_top1: prefer federated `expected_top1`, fall back to legacy `first_relevant_slug`.
        let expected_top1 = if let Some(e) = entry.get("expected_top1") {
            let obj = e.as_object().ok_or_else(|| {
                QrelsParseError(format!("entry {i} ({query_id}) expected_top1 is not an object"))
            })?;
            let source_id = obj
                .get("source_id")
                .and_then(|x| x.as_str())
                .ok_or_else(|| {
                    QrelsParseError(format!(
                        "entry {i} ({query_id}) expected_top1 missing source_id"
                    ))
                })?
                .to_string();
            let slug = obj
                .get("slug")
                .and_then(|x| x.as_str())
                .ok_or_else(|| {
                    QrelsParseError(format!(
                        "entry {i} ({query_id}) expected_top1 missing slug"
                    ))
                })?
                .to_string();
            Some(QrelsRef { source_id, slug })
        } else if let Some(s) = entry.get("first_relevant_slug").and_then(|x| x.as_str()) {
            Some(QrelsRef {
                source_id: "default".into(),
                slug: s.to_string(),
            })
        } else {
            None
        };

        let label = entry
            .get("label")
            .and_then(|x| x.as_str())
            .map(|s| s.to_string());

        out_queries.push(QrelsEntry {
            query_id,
            query,
            relevant,
            expected_top1,
            label,
        });
    }

    let description = v
        .get("_description")
        .and_then(|x| x.as_str())
        .map(|s| s.to_string());

    Ok(QrelsFile {
        schema_version: QRELS_FILE_SCHEMA_VERSION,
        queries: out_queries,
        _description: description,
    })
}

/// Correctness gate thresholds.
#[derive(Clone, Copy, Debug)]
pub struct QrelsThresholds {
    pub recall_at_k: f64,
    pub first_relevant_hit: f64,
    pub expected_top1: f64,
    /// Top-K cutoff carried for convenience (NOT a gate floor).
    pub k: usize,
}

/// Defaults when neither the qrels file nor CLI flags set them.
pub const DEFAULT_QRELS_THRESHOLDS: QrelsThresholds = QrelsThresholds {
    recall_at_k: 0.70,
    first_relevant_hit: 0.60,
    /// Lower default because exact top-1 is harder than any-relevant top-1.
    expected_top1: 0.50,
    /// k for recall@k unless overridden by CLI.
    k: 10,
};

/// Thresholds as serialized in the gate output envelope (excludes `k`).
#[derive(Clone, Debug, Serialize)]
pub struct CorrectnessGateThresholds {
    pub recall_at_k: f64,
    pub first_relevant_hit: f64,
    pub expected_top1: f64,
}

impl From<&QrelsThresholds> for CorrectnessGateThresholds {
    fn from(t: &QrelsThresholds) -> Self {
        CorrectnessGateThresholds {
            recall_at_k: t.recall_at_k,
            first_relevant_hit: t.first_relevant_hit,
            expected_top1: t.expected_top1,
        }
    }
}

/// Per-query correctness result.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PerQueryCorrectness {
    pub query_id: String,
    pub query: String,
    pub recall_at_k: f64,
    /// 1 if the first retrieved ref is relevant, else 0.
    pub first_relevant_hit: u8,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expected_top1_hit: Option<u8>,
    pub retrieved_count: usize,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub errored: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error_message: Option<String>,
}

/// Aggregate correctness summary across all queries.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CorrectnessSummary {
    pub k: usize,
    pub queries_total: usize,
    /// queries_total - queries_errored.
    pub queries_run: usize,
    pub queries_errored: usize,
    pub mean_recall_at_k: f64,
    pub first_relevant_hit_rate: f64,
    /// Denominator = queries with expected_top1 SET (not total queries).
    pub expected_top1_hit_rate: f64,
    pub expected_top1_denominator: usize,
}

/// Full correctness gate result (per-query + aggregate).
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CorrectnessResult {
    pub summary: CorrectnessSummary,
    pub per_query: Vec<PerQueryCorrectness>,
}

/// Run the correctness gate against a brain.
///
/// `query_fn(q, k)` returns the retrieved `${source_id}::${slug}` keys for
/// query `q` (retrieval depth `k`), or an error. A per-query error is recorded
/// as `errored: true` and surfaces as a `queries_errored` breach in the gate
/// verdict (Finding 2D) — it is never silently dropped from the aggregate.
///
/// Mirrors `runCorrectnessGate`: throws only if the qrels file is empty
/// (caller bug).
pub async fn run_correctness_gate<F, Fut>(
    qrels: &QrelsFile,
    k: usize,
    query_fn: F,
) -> Result<CorrectnessResult>
where
    F: Fn(&str, usize) -> Fut,
    Fut: std::future::Future<Output = Result<Vec<String>>>,
{
    if qrels.queries.is_empty() {
        return Err(Error::new(
            "EvalError",
            "run_correctness_gate",
            "qrels file has no queries",
        ));
    }

    let mut per_query = Vec::with_capacity(qrels.queries.len());
    for entry in &qrels.queries {
        let relevant: HashSet<String> = entry.relevant.iter().map(|r| r.key()).collect();

        let retrieved = match query_fn(&entry.query, k).await {
            Ok(keys) => keys,
            Err(e) => {
                per_query.push(PerQueryCorrectness {
                    query_id: entry.query_id.clone(),
                    query: entry.query.clone(),
                    recall_at_k: 0.0,
                    first_relevant_hit: 0,
                    expected_top1_hit: None,
                    retrieved_count: 0,
                    errored: Some(true),
                    error_message: Some(e.to_string()),
                });
                continue;
            }
        };

        let recall = recall_at_k(&retrieved, &relevant, k);
        let first_relevant = if retrieved.is_empty() {
            0
        } else {
            (relevant.contains(&retrieved[0]) as u8)
        };
        let mut out = PerQueryCorrectness {
            query_id: entry.query_id.clone(),
            query: entry.query.clone(),
            recall_at_k: recall,
            first_relevant_hit: first_relevant,
            expected_top1_hit: None,
            retrieved_count: retrieved.len(),
            errored: None,
            error_message: None,
        };
        if let Some(exp) = &entry.expected_top1 {
            out.expected_top1_hit = Some(if retrieved.is_empty() {
                0
            } else {
                ((retrieved[0] == exp.key()) as u8)
            });
        }
        per_query.push(out);
    }

    let errored = per_query.iter().filter(|p| p.errored == Some(true)).count();
    let run = per_query.len() - errored;
    let non_errored: Vec<&PerQueryCorrectness> =
        per_query.iter().filter(|p| p.errored != Some(true)).collect();

    let mean_recall = if non_errored.is_empty() {
        0.0
    } else {
        non_errored.iter().map(|p| p.recall_at_k).sum::<f64>() / non_errored.len() as f64
    };
    let first_relevant_rate = if non_errored.is_empty() {
        0.0
    } else {
        non_errored
            .iter()
            .map(|p| p.first_relevant_hit as f64)
            .sum::<f64>()
            / non_errored.len() as f64
    };
    let with_expected_top1: Vec<&PerQueryCorrectness> = non_errored
        .iter()
        .filter(|p| p.expected_top1_hit.is_some())
        .copied()
        .collect();
    let expected_top1_rate = if with_expected_top1.is_empty() {
        0.0
    } else {
        with_expected_top1
            .iter()
            .map(|p| p.expected_top1_hit.unwrap_or(0) as f64)
            .sum::<f64>()
            / with_expected_top1.len() as f64
    };

    Ok(CorrectnessResult {
        summary: CorrectnessSummary {
            k,
            queries_total: per_query.len(),
            queries_run: run,
            queries_errored: errored,
            mean_recall_at_k: mean_recall,
            first_relevant_hit_rate: first_relevant_rate,
            expected_top1_hit_rate: expected_top1_rate,
            expected_top1_denominator: with_expected_top1.len(),
        },
        per_query,
    })
}

/// Verdict of a gate (pass / fail).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum GateVerdict {
    Pass,
    Fail,
}

/// A single gate breach (metric that fell below its threshold, or a hard error).
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct GateBreach {
    pub metric: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub observed: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub threshold: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error_tail: Option<String>,
}

/// Output of the correctness gate, as embedded in the gate envelope.
#[derive(Clone, Debug, Serialize)]
pub struct CorrectnessGateOutput {
    pub ran: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub qrels_path: Option<String>,
    pub summary: CorrectnessSummary,
    pub thresholds: CorrectnessGateThresholds,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub breaches: Vec<GateBreach>,
}

/// Output of the (not-yet-implemented) regression gate, for envelope symmetry.
#[derive(Clone, Debug, Default, Serialize)]
pub struct RegressionGateOutput {
    pub ran: bool,
}

/// Top-level gate envelope (mirrors the TS `GateResult`).
#[derive(Clone, Debug, Serialize)]
pub struct GateResult {
    pub schema_version: u8,
    pub verdict: GateVerdict,
    pub regression_gate: RegressionGateOutput,
    pub correctness_gate: CorrectnessGateOutput,
}

/// Evaluate the correctness result against thresholds, returning breaches.
///
/// Faithful to `runCorrectnessGateDispatch`: per-query errors are a breach;
/// `mean_recall_at_k` / `first_relevant_hit_rate` are always checked;
/// `expected_top1_hit_rate` is only checked when at least one query set
/// `expected_top1` (denominator > 0).
pub fn evaluate_correctness_gate(
    result: &CorrectnessResult,
    thresholds: &QrelsThresholds,
) -> Vec<GateBreach> {
    let mut breaches = Vec::new();
    if result.summary.queries_errored > 0 {
        breaches.push(GateBreach {
            metric: "queries_errored".into(),
            observed: Some(result.summary.queries_errored as f64),
            threshold: Some(0.0),
            reason: Some("one_or_more_qrels_queries_threw".into()),
            error_tail: None,
        });
    }
    if result.summary.mean_recall_at_k < thresholds.recall_at_k {
        breaches.push(GateBreach {
            metric: "mean_recall_at_k".into(),
            observed: Some(result.summary.mean_recall_at_k),
            threshold: Some(thresholds.recall_at_k),
            reason: None,
            error_tail: None,
        });
    }
    if result.summary.first_relevant_hit_rate < thresholds.first_relevant_hit {
        breaches.push(GateBreach {
            metric: "first_relevant_hit_rate".into(),
            observed: Some(result.summary.first_relevant_hit_rate),
            threshold: Some(thresholds.first_relevant_hit),
            reason: None,
            error_tail: None,
        });
    }
    if result.summary.expected_top1_denominator > 0
        && result.summary.expected_top1_hit_rate < thresholds.expected_top1
    {
        breaches.push(GateBreach {
            metric: "expected_top1_hit_rate".into(),
            observed: Some(result.summary.expected_top1_hit_rate),
            threshold: Some(thresholds.expected_top1),
            reason: None,
            error_tail: None,
        });
    }
    breaches
}

/// Assemble the full gate envelope from a correctness result + its breaches.
///
/// `verdict` is `Pass` iff `breaches` is empty. The regression half is always
/// `ran: false` here (it is tracked by 1-1-4 stage 4), so the envelope stays
/// honest about what was actually executed.
pub fn assemble_gate_result(
    qrels_path: Option<String>,
    result: &CorrectnessResult,
    thresholds: &QrelsThresholds,
    breaches: Vec<GateBreach>,
) -> GateResult {
    let verdict = if breaches.is_empty() {
        GateVerdict::Pass
    } else {
        GateVerdict::Fail
    };
    GateResult {
        schema_version: 1,
        verdict,
        regression_gate: RegressionGateOutput { ran: false },
        correctness_gate: CorrectnessGateOutput {
            ran: true,
            qrels_path,
            summary: result.summary.clone(),
            thresholds: thresholds.into(),
            breaches,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn qrels_entry(query_id: &str, query: &str, relevant: &[(&str, &str)]) -> QrelsEntry {
        QrelsEntry {
            query_id: query_id.into(),
            query: query.into(),
            relevant: relevant
                .iter()
                .map(|(sid, slug)| QrelsRef {
                    source_id: sid.to_string(),
                    slug: slug.to_string(),
                })
                .collect(),
            expected_top1: None,
            label: None,
        }
    }

    // ── parse_qrels_file ──────────────────────────────────────────

    #[test]
    fn parse_qrels_legacy_slug_shape_promotes_to_default_source() {
        let json = r#"{"schema_version":1,"queries":[{"query_id":"q1","query":"rust","relevant_slugs":["rust","go"],"first_relevant_slug":"rust"}]}"#;
        let f = parse_qrels_file(json).unwrap();
        assert_eq!(f.queries.len(), 1);
        assert_eq!(f.queries[0].relevant.len(), 2);
        assert_eq!(f.queries[0].relevant[0].source_id, "default");
        assert_eq!(f.queries[0].relevant[0].slug, "rust");
        let exp = f.queries[0].expected_top1.as_ref().unwrap();
        assert_eq!(exp.source_id, "default");
        assert_eq!(exp.slug, "rust");
    }

    #[test]
    fn parse_qrels_federated_shape_keeps_source_id() {
        let json = r#"{"schema_version":1,"queries":[{"query_id":"q1","query":"rust","relevant":[{"source_id":"foo","slug":"rust"}],"expected_top1":{"source_id":"foo","slug":"rust"}}]}"#;
        let f = parse_qrels_file(json).unwrap();
        assert_eq!(f.queries[0].relevant[0].source_id, "foo");
        assert_eq!(f.queries[0].expected_top1.as_ref().unwrap().source_id, "foo");
    }

    #[test]
    fn parse_qrels_rejects_array() {
        assert!(parse_qrels_file(r#"[{"query":"x","relevant_slugs":["a"]}]"#).is_err());
    }

    #[test]
    fn parse_qrels_rejects_bad_schema_version() {
        assert!(parse_qrels_file(r#"{"schema_version":2,"queries":[]}"#).is_err());
    }

    #[test]
    fn parse_qrels_rejects_empty_queries() {
        assert!(parse_qrels_file(r#"{"schema_version":1,"queries":[]}"#).is_err());
    }

    #[test]
    fn parse_qrels_rejects_missing_query() {
        assert!(parse_qrels_file(
            r#"{"schema_version":1,"queries":[{"query_id":"q1","relevant_slugs":["a"]}]}"#
        )
        .is_err());
    }

    #[test]
    fn parse_qrels_rejects_empty_relevant() {
        assert!(parse_qrels_file(
            r#"{"schema_version":1,"queries":[{"query_id":"q1","query":"x","relevant_slugs":[]}]}"#
        )
        .is_err());
    }

    // ── run_correctness_gate (stub query_fn) ──────────────────────

    #[tokio::test]
    async fn run_gate_perfect_recall_and_top1() {
        let qrels = QrelsFile {
            schema_version: 1,
            queries: vec![
                qrels_entry("q1", "rust", &[("default", "rust"), ("default", "go")]),
                qrels_entry("q2", "python", &[("default", "python")]),
            ],
            _description: None,
        };
        // Each query returns relevant refs at rank 1.
        let result = run_correctness_gate(&qrels, 10, |q, _k| {
            let hits: Vec<String> = if q == "rust" {
                vec!["default::rust".into(), "default::go".into()]
            } else {
                vec!["default::python".into()]
            };
            async move { Ok(hits) }
        })
            .await
            .unwrap();
        assert_eq!(result.summary.queries_total, 2);
        assert_eq!(result.summary.queries_errored, 0);
        assert!((result.summary.mean_recall_at_k - 1.0).abs() < 1e-9);
        assert!((result.summary.first_relevant_hit_rate - 1.0).abs() < 1e-9);
    }

    #[tokio::test]
    async fn run_gate_partial_recall_breaches_recall_floor() {
        let qrels = QrelsFile {
            schema_version: 1,
            queries: vec![qrels_entry("q1", "rust", &[("default", "rust"), ("default", "go")])],
            _description: None,
        };
        // Only one of two relevant is retrieved → recall = 0.5 < 0.70 floor.
        let result = run_correctness_gate(&qrels, 10, |_q, _k| {
            async move { Ok(vec!["default::rust".into()]) }
        })
            .await
            .unwrap();
        assert!((result.summary.mean_recall_at_k - 0.5).abs() < 1e-9);
        let thresholds = QrelsThresholds {
            recall_at_k: 0.70,
            first_relevant_hit: 0.60,
            expected_top1: 0.50,
            k: 10,
        };
        let breaches = evaluate_correctness_gate(&result, &thresholds);
        assert!(breaches.iter().any(|b| b.metric == "mean_recall_at_k"));
    }

    #[tokio::test]
    async fn run_gate_per_query_error_is_a_breach() {
        let qrels = QrelsFile {
            schema_version: 1,
            queries: vec![qrels_entry("q1", "rust", &[("default", "rust")])],
            _description: None,
        };
        let result = run_correctness_gate(&qrels, 10, |_q, _k| {
            async move { Err(Error::new("EvalError", "test", "boom")) }
        })
            .await
            .unwrap();
        assert_eq!(result.summary.queries_errored, 1);
        assert_eq!(result.summary.queries_run, 0);
        let thresholds = QrelsThresholds {
            recall_at_k: 0.70,
            first_relevant_hit: 0.60,
            expected_top1: 0.50,
            k: 10,
        };
        let breaches = evaluate_correctness_gate(&result, &thresholds);
        assert!(breaches.iter().any(|b| b.metric == "queries_errored"));
    }

    #[tokio::test]
    async fn run_gate_expected_top1_only_enforced_when_set() {
        let mut q1 = qrels_entry("q1", "rust", &[("default", "rust")]);
        q1.expected_top1 = Some(QrelsRef {
            source_id: "default".into(),
            slug: "rust".into(),
        });
        let q2 = qrels_entry("q2", "python", &[("default", "python")]); // no expected_top1
        let qrels = QrelsFile {
            schema_version: 1,
            queries: vec![q1, q2],
            _description: None,
        };
        // Both return the right top-1 for q1 (expected) and q2 (first relevant);
        // denominator for expected_top1 is 1.
        let result = run_correctness_gate(&qrels, 10, |q, _k| {
            let hits: Vec<String> = if q == "rust" {
                vec!["default::rust".into()]
            } else {
                vec!["default::python".into()]
            };
            async move { Ok(hits) }
        })
            .await
            .unwrap();
        assert_eq!(result.summary.expected_top1_denominator, 1);
        assert!((result.summary.expected_top1_hit_rate - 1.0).abs() < 1e-9);

        // Now make q1's top-1 WRONG but keep it relevant → first_relevant ok,
        // expected_top1 drops, and since denominator=1 the floor (0.5) is beaten.
        let result = run_correctness_gate(&qrels, 10, |q, _k| {
            let hits: Vec<String> = if q == "rust" {
                // relevant rust is present but NOT at rank 0
                vec!["default::go".into(), "default::rust".into()]
            } else {
                vec!["default::python".into()]
            };
            async move { Ok(hits) }
        })
            .await
            .unwrap();
        assert_eq!(result.summary.expected_top1_denominator, 1);
        assert!((result.summary.expected_top1_hit_rate - 0.0).abs() < 1e-9);
        let thresholds = QrelsThresholds {
            recall_at_k: 0.70,
            first_relevant_hit: 0.60,
            expected_top1: 0.50,
            k: 10,
        };
        let breaches = evaluate_correctness_gate(&result, &thresholds);
        assert!(breaches.iter().any(|b| b.metric == "expected_top1_hit_rate"));
    }
}
