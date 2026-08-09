//! v0.34 pre-w0 — code-retrieval eval harness (Rust port of the TS
//! `src/eval/code-retrieval/harness.ts` + `strategies.ts`).
//!
//! The harness captures a retrieval-quality number against a curated question
//! set, then the `--compare` (gate) mode decides whether the v0.34 code-intel
//! path beat the pre-v0.34 baseline.
//!
//! Two capture modes:
//!   - `baseline`:          query + hybrid search only (today's zbrain).
//!   - `with-code-intel`:   code-intel MCP ops (code_blast / code_flow /
//!                          code_def / code_refs) — wired to the real Rust
//!                          code-intel ops, NOT the TS stub that returned [].
//!
//! Pure-function metrics + loader + gate logic live in this file and are fully
//! unit-testable without an engine. The engine-backed retrieval strategy is
//! injected as a closure by the CLI (mirroring `replay_core`'s `query_fn`),
//! so the runner stays hermetic.

use chrono::Utc;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::path::Path;

/// Question categories. Mirrors the TS `CodeQuestionKind` union (snake_case).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CodeQuestionKind {
    Callers,
    Callees,
    Definition,
    References,
    BlastRadius,
    ExecutionFlow,
    ClusterMembership,
}

impl CodeQuestionKind {
    /// Maps a question kind to the Rust code-intel op used by the
    /// `with-code-intel` strategy. `cluster_membership` has no Rust op yet
    /// (TS used `code_cluster_get`, which ZBrain has not ported) → `None`.
    pub fn code_intel_op(&self) -> Option<&'static str> {
        match self {
            CodeQuestionKind::Callers | CodeQuestionKind::BlastRadius => Some("code_blast"),
            CodeQuestionKind::Callees | CodeQuestionKind::ExecutionFlow => Some("code_flow"),
            CodeQuestionKind::Definition => Some("code_def"),
            CodeQuestionKind::References => Some("code_refs"),
            CodeQuestionKind::ClusterMembership => None,
        }
    }
}

/// A single curated retrieval question.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CodeQuestion {
    pub id: String,
    pub kind: CodeQuestionKind,
    /// Human-language query an agent would type.
    pub query: String,
    /// Canonical symbol to look up structurally (post-v0.34).
    pub symbol: String,
    /// Expected file paths that must appear in the retrieved set.
    pub expected_files: Vec<String>,
    /// Minimum recall@k (against `expected_files`) for this question to count
    /// as "answered".
    pub expected_min_recall: f64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

/// The curated question set file (version 1).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CodeQuestionFile {
    pub version: u8,
    pub schema: String,
    pub corpus: String,
    pub description: String,
    pub questions: Vec<CodeQuestion>,
}

/// Per-question retrieval result.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QuestionResult {
    pub id: String,
    pub kind: CodeQuestionKind,
    /// Files actually returned, in rank order (top-k).
    pub retrieved_files: Vec<String>,
    /// Top-1 file (the single most-confident answer).
    pub top_1: Option<String>,
    /// precision@k = |relevant ∩ retrieved| / |retrieved|.
    pub precision_at_k: f64,
    /// recall@k = |relevant ∩ retrieved| / |relevant|.
    pub recall_at_k: f64,
    /// Whether this question's bar (`expected_min_recall`) was cleared.
    pub answered: bool,
    /// Total latency for this question's tool calls, in ms.
    pub latency_ms: u64,
}

/// Capture mode for a run.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum EvalRunReportMode {
    Baseline,
    WithCodeIntel,
}

/// A single captured run report.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EvalRunReport {
    pub mode: EvalRunReportMode,
    pub schema_version: u8,
    pub corpus: String,
    pub k: usize,
    pub questions: Vec<QuestionResult>,
    /// Mean precision@k across all questions.
    pub mean_precision_at_k: f64,
    /// Fraction of questions that cleared their `expected_min_recall` bar.
    pub answered_rate: f64,
    /// Top-1 stability — set when comparing two runs; `None` for single-run.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub top_1_stability_rate: Option<f64>,
    /// Aggregate run latency, ms.
    pub total_latency_ms: u64,
    /// ISO-8601 capture time.
    pub captured_at: String,
    /// Git short-SHA at capture time.
    pub commit: String,
}

// ─────────────────────────────────────────────────────────────────
// Pure metrics (no engine dependency, fully unit-testable)
// ─────────────────────────────────────────────────────────────────

/// precision@k = relevant ∩ retrieved (top-k) / retrieved (top-k).
/// Returns 0 when `retrieved` is empty.
pub fn precision_at_k(retrieved: &[String], relevant: &HashSet<String>, k: usize) -> f64 {
    let top_k: Vec<&String> = retrieved.iter().take(k).collect();
    if top_k.is_empty() {
        return 0.0;
    }
    let hits = top_k.iter().filter(|r| relevant.contains(r.as_str())).count();
    hits as f64 / top_k.len() as f64
}

/// recall@k = relevant ∩ retrieved (top-k) / relevant.
/// Returns 1 when `relevant` is empty (degenerate case).
pub fn recall_at_k(retrieved: &[String], relevant: &HashSet<String>, k: usize) -> f64 {
    if relevant.is_empty() {
        return 1.0;
    }
    let top_k: HashSet<&String> = retrieved.iter().take(k).collect();
    let hits = relevant.iter().filter(|r| top_k.contains(r)).count();
    hits as f64 / relevant.len() as f64
}

/// Top-1 stability rate between two runs over the same question set.
/// `|{q : run1.top_1 === run2.top_1}| / comparable`. NaN/empty → 0.
pub fn top1_stability_rate(run1: &[QuestionResult], run2: &[QuestionResult]) -> f64 {
    if run1.is_empty() || run2.is_empty() {
        return 0.0;
    }
    let lookup: HashMap<&str, Option<&str>> =
        run2.iter().map(|q| (q.id.as_str(), q.top_1.as_deref())).collect();
    let mut stable = 0u64;
    let mut comparable = 0u64;
    for q in run1 {
        let Some(other) = lookup.get(q.id.as_str()) else {
            continue;
        };
        comparable += 1;
        if q.top_1.as_deref() == *other {
            stable += 1;
        }
    }
    if comparable == 0 {
        0.0
    } else {
        stable as f64 / comparable as f64
    }
}

/// Dedupe + drop empties; preserves order.
pub fn normalize_retrieved(retrieved_files: &[String]) -> Vec<String> {
    let mut seen = HashSet::new();
    let mut out = Vec::new();
    for f in retrieved_files {
        if f.is_empty() {
            continue;
        }
        if seen.contains(f) {
            continue;
        }
        seen.insert(f.clone());
        out.push(f.clone());
    }
    out
}

/// Split expected files into exact files vs directory prefixes (trailing
/// slash).
pub struct ExpectedRelevant {
    pub exact_files: HashSet<String>,
    pub dir_prefixes: Vec<String>,
}

pub fn expand_expected_to_relevant_set(expected: &[String]) -> ExpectedRelevant {
    let mut exact_files = HashSet::new();
    let mut dir_prefixes = Vec::new();
    for e in expected {
        if e.ends_with('/') {
            dir_prefixes.push(e.clone());
        } else {
            exact_files.insert(e.clone());
        }
    }
    ExpectedRelevant {
        exact_files,
        dir_prefixes,
    }
}

pub fn is_file_relevant(file: &str, expected: &ExpectedRelevant) -> bool {
    if expected.exact_files.contains(file) {
        return true;
    }
    for p in &expected.dir_prefixes {
        if file.starts_with(p.as_str()) {
            return true;
        }
    }
    false
}

// ─────────────────────────────────────────────────────────────────
// Loader
// ─────────────────────────────────────────────────────────────────

/// Load + validate a questions file (version 1).
pub fn load_questions(path: &Path) -> anyhow::Result<CodeQuestionFile> {
    let raw = std::fs::read_to_string(path)
        .map_err(|e| anyhow::anyhow!("questions file not found: {} ({})", path.display(), e))?;
    let parsed: CodeQuestionFile = serde_json::from_str(&raw)
        .map_err(|e| anyhow::anyhow!("failed to parse questions JSON: {}", e))?;
    if parsed.version != 1 {
        anyhow::bail!("unsupported questions file version {} (expected 1)", parsed.version);
    }
    if parsed.questions.is_empty() {
        anyhow::bail!("questions file contains no questions");
    }
    for q in &parsed.questions {
        if q.id.is_empty() || q.query.is_empty() || q.expected_files.is_empty() {
            anyhow::bail!("malformed question entry: {}", serde_json::to_string(q).unwrap_or_default());
        }
    }
    Ok(parsed)
}

/// Bundled default question set (verbatim from the TS-era v0.34 eval corpus).
/// Retrieval is string-matching against whatever brain the verb runs against,
/// so against a Rust brain the `.ts` paths simply yield an honest empty
/// baseline — the set must stay fixed so it cannot be retroactively tuned.
pub fn load_default_questions() -> anyhow::Result<CodeQuestionFile> {
    let parsed: CodeQuestionFile = serde_json::from_str(DEFAULT_QUESTIONS)
        .map_err(|e| anyhow::anyhow!("failed to parse bundled questions JSON: {}", e))?;
    Ok(parsed)
}

const DEFAULT_QUESTIONS: &str = include_str!("code_retrieval_questions.json");

// ─────────────────────────────────────────────────────────────────
// Runner
// ─────────────────────────────────────────────────────────────────

/// Outcome of one strategy retrieval call.
pub struct RetrievalOutcome {
    pub files: Vec<String>,
    pub latency_ms: u64,
}

/// Options for [`run_code_retrieval_eval`].
pub struct RunnerOpts {
    pub k: usize,
    pub corpus: String,
    /// Git short-SHA at capture time (filled by the caller).
    pub commit: String,
}

/// Run the eval over `questions` using the injected `retrieve` strategy.
///
/// `retrieve` is an async closure mapping a question to a ranked list of file
/// paths (it performs the actual engine / op calls). Retrieval errors are part
/// of the eval signal — they are recorded as an empty result, not propagated
/// (mirrors the TS runner).
pub async fn run_code_retrieval_eval<F, Fut>(
    mode: EvalRunReportMode,
    questions: &[CodeQuestion],
    retrieve: &F,
    opts: &RunnerOpts,
) -> anyhow::Result<EvalRunReport>
where
    F: Fn(&CodeQuestion, usize) -> Fut,
    Fut: std::future::Future<Output = anyhow::Result<RetrievalOutcome>>,
{
    let mut results: Vec<QuestionResult> = Vec::with_capacity(questions.len());
    let started = std::time::Instant::now();

    for q in questions {
        let t0 = std::time::Instant::now();
        let retrieved: Vec<String> = match retrieve(q, opts.k).await {
            Ok(r) => normalize_retrieved(&r.files),
            Err(e) => {
                eprintln!("[eval] retrieval error on {}: {}", q.id, e);
                Vec::new()
            }
        };
        let latency_ms = t0.elapsed().as_millis() as u64;

        let expected = expand_expected_to_relevant_set(&q.expected_files);
        let relevant_set: HashSet<String> =
            retrieved.iter().filter(|f| is_file_relevant(f, &expected)).cloned().collect();
        let precision_at_k = precision_at_k(&retrieved, &relevant_set, opts.k);
        let relevant_vec: Vec<String> = relevant_set.into_iter().collect();
        let recall_at_k = recall_at_k(&relevant_vec, &expected.exact_files, opts.k);
        let top_1 = retrieved.first().cloned();
        let answered = recall_at_k >= q.expected_min_recall;

        results.push(QuestionResult {
            id: q.id.clone(),
            kind: q.kind,
            retrieved_files: retrieved,
            top_1,
            precision_at_k,
            recall_at_k,
            answered,
            latency_ms,
        });
    }

    let total_latency_ms = started.elapsed().as_millis() as u64;
    let mean_precision_at_k =
        results.iter().map(|r| r.precision_at_k).sum::<f64>() / (results.len().max(1) as f64);
    let answered_rate =
        results.iter().filter(|r| r.answered).count() as f64 / (results.len().max(1) as f64);

    Ok(EvalRunReport {
        mode,
        schema_version: 1,
        corpus: opts.corpus.clone(),
        k: opts.k,
        questions: results,
        mean_precision_at_k,
        answered_rate,
        top_1_stability_rate: None,
        total_latency_ms,
        captured_at: Utc::now().to_rfc3339(),
        commit: opts.commit.clone(),
    })
}

// ─────────────────────────────────────────────────────────────────
// Comparison — used by the v0.34 ship gate
// ─────────────────────────────────────────────────────────────────

/// Gate verdict result.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GateResult {
    pub passed: bool,
    pub precision_delta_pp: f64,
    pub top_1_stability_rate: f64,
    pub questions_cleared_bar: usize,
    pub questions_total: usize,
    /// Free-form summary explaining pass/fail.
    pub summary: String,
}

/// Gate thresholds.
#[derive(Debug, Clone, Copy)]
pub struct GateOpts {
    /// Required precision@k delta (percentage points) to pass. Default 10.
    pub required_precision_delta_pp: f64,
    /// Required answered_rate delta to pass (alternative criterion). Default 0.15.
    pub required_top_1_stability_delta: f64,
    /// Minimum questions that must clear `expected_min_recall` in with-code-intel
    /// mode. Default 15.
    pub min_questions_cleared: usize,
}

pub const DEFAULT_GATE: GateOpts = GateOpts {
    required_precision_delta_pp: 10.0,
    required_top_1_stability_delta: 0.15,
    min_questions_cleared: 15,
};

/// Evaluate the ship gate: did `with_code_intel` beat `baseline`?
///
/// Pass iff `min_questions_cleared` is met AND (precision delta clears the bar
/// OR answered_rate delta clears the bar). Stability is measured as a delta in
/// `answered_rate` over the baseline (treating baseline as 1.0 self-stability),
/// so a reordering that lands more good answers is not punished.
pub fn evaluate_gate(
    baseline: &EvalRunReport,
    with_code_intel: &EvalRunReport,
    opts: GateOpts,
) -> GateResult {
    let precision_delta_pp = (with_code_intel.mean_precision_at_k - baseline.mean_precision_at_k) * 100.0;
    let top_1_stability_rate = top1_stability_rate(&baseline.questions, &with_code_intel.questions);
    let questions_cleared_bar = with_code_intel.questions.iter().filter(|q| q.answered).count();
    let questions_total = with_code_intel.questions.len();

    let precision_passes = precision_delta_pp >= opts.required_precision_delta_pp;
    let stability_passes =
        (with_code_intel.answered_rate - baseline.answered_rate) >= opts.required_top_1_stability_delta;
    let enough_cleared = questions_cleared_bar >= opts.min_questions_cleared;

    let passed = enough_cleared && (precision_passes || stability_passes);

    let mut reasons: Vec<String> = Vec::new();
    if !enough_cleared {
        reasons.push(format!(
            "only {}/{} questions cleared expected_min_recall (need >= {})",
            questions_cleared_bar, questions_total, opts.min_questions_cleared
        ));
    }
    if precision_passes {
        reasons.push(format!(
            "precision@{} +{:.1}pp (>= {})",
            baseline.k, precision_delta_pp, opts.required_precision_delta_pp
        ));
    } else {
        reasons.push(format!(
            "precision@{} delta {:.1}pp (< {})",
            baseline.k, precision_delta_pp, opts.required_precision_delta_pp
        ));
    }
    if stability_passes {
        reasons.push(format!(
            "answered_rate +{:.1}pp",
            (with_code_intel.answered_rate - baseline.answered_rate) * 100.0
        ));
    }

    let summary = if passed {
        format!("GATE PASS — {}", reasons.join("; "))
    } else {
        format!("GATE FAIL — {}", reasons.join("; "))
    };

    GateResult {
        passed,
        precision_delta_pp,
        top_1_stability_rate,
        questions_cleared_bar,
        questions_total,
        summary,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hs(strs: &[&str]) -> HashSet<String> {
        strs.iter().map(|s| s.to_string()).collect()
    }

    fn make_result(id: &str, top_1: Option<&str>) -> QuestionResult {
        QuestionResult {
            id: id.to_string(),
            kind: CodeQuestionKind::Callers,
            retrieved_files: top_1.map(|t| vec![t.to_string()]).unwrap_or_default(),
            top_1: top_1.map(|t| t.to_string()),
            precision_at_k: if top_1.is_some() { 1.0 } else { 0.0 },
            recall_at_k: if top_1.is_some() { 1.0 } else { 0.0 },
            answered: top_1.is_some(),
            latency_ms: 1,
        }
    }

    fn make_report(
        mode: EvalRunReportMode,
        mean_precision: f64,
        answered_rate: f64,
        total: usize,
    ) -> EvalRunReport {
        let answered = (answered_rate * total as f64).round() as usize;
        let questions: Vec<QuestionResult> = (0..total)
            .map(|i| QuestionResult {
                id: format!("q{i}"),
                kind: CodeQuestionKind::Callers,
                retrieved_files: vec!["fakefile.ts".to_string()],
                top_1: Some("fakefile.ts".to_string()),
                precision_at_k: mean_precision,
                recall_at_k: 0.5,
                answered: i < answered,
                latency_ms: 1,
            })
            .collect();
        EvalRunReport {
            mode,
            schema_version: 1,
            corpus: "fake".to_string(),
            k: 5,
            questions,
            mean_precision_at_k: mean_precision,
            answered_rate,
            top_1_stability_rate: None,
            total_latency_ms: total as u64,
            captured_at: "2026-05-10T00:00:00Z".to_string(),
            commit: "abc1234".to_string(),
        }
    }

    // ── precisionAtK ──
    #[test]
    fn precision_at_k_empty() {
        assert_eq!(precision_at_k(&[], &hs(&["a"]), 5), 0.0);
    }
    #[test]
    fn precision_at_k_all_relevant() {
        assert_eq!(precision_at_k(&["a".into(), "b".into(), "c".into()], &hs(&["a", "b", "c"]), 5), 1.0);
    }
    #[test]
    fn precision_at_k_none_relevant() {
        assert_eq!(precision_at_k(&["x".into(), "y".into()], &hs(&["a", "b"]), 5), 0.0);
    }
    #[test]
    fn precision_at_k_respects_k() {
        // top-3 = [a,b,c]; relevant = {a,d} → 1/3
        assert!((precision_at_k(&["a".into(), "b".into(), "c".into(), "d".into(), "e".into()], &hs(&["a", "d"]), 3) - 1.0 / 3.0).abs() < 1e-9);
    }
    #[test]
    fn precision_at_k_k_larger_than_retrieved() {
        // retrieved len 2, k=5 → divide by 2
        assert!((precision_at_k(&["a".into(), "b".into()], &hs(&["a"]), 5) - 1.0 / 2.0).abs() < 1e-9);
    }

    // ── recallAtK ──
    #[test]
    fn recall_at_k_empty_relevant() {
        assert_eq!(recall_at_k(&["a".into(), "b".into()], &hs(&[]), 5), 1.0);
    }
    #[test]
    fn recall_at_k_all_relevant() {
        assert_eq!(recall_at_k(&["a".into(), "b".into(), "c".into()], &hs(&["a", "b"]), 5), 1.0);
    }
    #[test]
    fn recall_at_k_none_relevant() {
        assert_eq!(recall_at_k(&["x".into(), "y".into(), "z".into()], &hs(&["a", "b"]), 5), 0.0);
    }
    #[test]
    fn recall_at_k_half() {
        assert!((recall_at_k(&["a".into(), "x".into()], &hs(&["a", "b"]), 5) - 1.0 / 2.0).abs() < 1e-9);
    }
    #[test]
    fn recall_at_k_respects_k() {
        // top-2 = [x,y]; relevant = {a,b} → recall=0
        assert_eq!(recall_at_k(&["x".into(), "y".into(), "a".into(), "b".into()], &hs(&["a", "b"]), 2), 0.0);
    }

    // ── top1StabilityRate ──
    #[test]
    fn top1_empty() {
        assert_eq!(top1_stability_rate(&[], &[]), 0.0);
    }
    #[test]
    fn top1_all_match() {
        let r1 = vec![make_result("q1", Some("a")), make_result("q2", Some("b"))];
        let r2 = vec![make_result("q1", Some("a")), make_result("q2", Some("b"))];
        assert_eq!(top1_stability_rate(&r1, &r2), 1.0);
    }
    #[test]
    fn top1_none_match() {
        let r1 = vec![make_result("q1", Some("a"))];
        let r2 = vec![make_result("q1", Some("x"))];
        assert_eq!(top1_stability_rate(&r1, &r2), 0.0);
    }
    #[test]
    fn top1_ignores_one_sided_questions() {
        let r1 = vec![make_result("q1", Some("a")), make_result("q2", Some("b"))];
        let r2 = vec![make_result("q1", Some("a"))];
        assert_eq!(top1_stability_rate(&r1, &r2), 1.0);
    }
    #[test]
    fn top1_null_counts_unstable() {
        let r1 = vec![make_result("q1", Some("a"))];
        let r2 = vec![make_result("q1", None)];
        assert_eq!(top1_stability_rate(&r1, &r2), 0.0);
    }

    // ── normalizeRetrieved ──
    #[test]
    fn normalize_dedupes_order() {
        assert_eq!(normalize_retrieved(&["a".into(), "b".into(), "a".into(), "c".into(), "b".into()]), vec!["a", "b", "c"]);
    }
    #[test]
    fn normalize_drops_empty() {
        assert_eq!(normalize_retrieved(&["a".into(), "".into(), "b".into()]), vec!["a", "b"]);
    }

    // ── expandExpectedToRelevantSet / isFileRelevant ──
    #[test]
    fn relevant_exact() {
        let exp = expand_expected_to_relevant_set(&["src/foo.ts".into(), "src/bar.ts".into()]);
        assert!(is_file_relevant("src/foo.ts", &exp));
        assert!(!is_file_relevant("src/baz.ts", &exp));
    }
    #[test]
    fn relevant_dir_prefix() {
        let exp = expand_expected_to_relevant_set(&["src/core/".into()]);
        assert!(is_file_relevant("src/core/foo.ts", &exp));
        assert!(is_file_relevant("src/core/sub/bar.ts", &exp));
        assert!(!is_file_relevant("src/other.ts", &exp));
    }
    #[test]
    fn relevant_mixed() {
        let exp = expand_expected_to_relevant_set(&["src/foo.ts".into(), "src/core/".into()]);
        assert!(is_file_relevant("src/foo.ts", &exp));
        assert!(is_file_relevant("src/core/bar.ts", &exp));
        assert!(!is_file_relevant("src/other.ts", &exp));
    }

    // ── loadQuestions ──
    #[test]
    fn load_default_questions_ok() {
        let file = load_default_questions().expect("bundled questions must parse");
        assert_eq!(file.version, 1);
        assert_eq!(file.corpus, "zbrain");
        assert!(file.questions.len() >= 12);
        for q in &file.questions {
            assert!(!q.id.is_empty());
            // kind deserialized successfully (valid enum variant)
            let _ = q.kind;
            assert!(!q.query.is_empty());
            assert!(!q.symbol.is_empty());
            assert!(q.expected_files.len() >= 1);
            assert!((0.0..=1.0).contains(&q.expected_min_recall));
        }
    }
    #[test]
    fn load_questions_missing_file_errors() {
        let err = load_questions(Path::new("/tmp/does-not-exist-XXXX.json")).unwrap_err();
        assert!(err.to_string().contains("not found"), "got: {err}");
    }

    // ── evaluateGate ──
    #[test]
    fn gate_pass_on_precision() {
        let baseline = make_report(EvalRunReportMode::Baseline, 0.4, 0.5, 20);
        let with_ci = make_report(EvalRunReportMode::WithCodeIntel, 0.55, 0.85, 20);
        let gate = evaluate_gate(&baseline, &with_ci, DEFAULT_GATE);
        assert!(gate.passed);
        assert!((gate.precision_delta_pp - 15.0).abs() < 1e-9);
    }
    #[test]
    fn gate_fail_insufficient_cleared() {
        let baseline = make_report(EvalRunReportMode::Baseline, 0.4, 0.5, 30);
        let with_ci = make_report(EvalRunReportMode::WithCodeIntel, 0.6, 0.4, 30);
        let gate = evaluate_gate(&baseline, &with_ci, DEFAULT_GATE);
        assert!(!gate.passed);
        assert!(gate.summary.contains("only "), "got: {}", gate.summary);
    }
    #[test]
    fn gate_pass_on_answered_rate() {
        let baseline = make_report(EvalRunReportMode::Baseline, 0.4, 0.5, 30);
        let with_ci = make_report(EvalRunReportMode::WithCodeIntel, 0.45, 0.7, 30);
        let gate = evaluate_gate(&baseline, &with_ci, DEFAULT_GATE);
        assert!(gate.passed);
    }
    #[test]
    fn default_gate_constants() {
        assert_eq!(DEFAULT_GATE.required_precision_delta_pp, 10.0);
        assert_eq!(DEFAULT_GATE.required_top_1_stability_delta, 0.15);
        assert_eq!(DEFAULT_GATE.min_questions_cleared, 15);
    }

    // ── run_code_retrieval_eval (hermetic fake strategy) ──
    #[tokio::test]
    async fn run_eval_baseline_like_fake() {
        let questions = vec![
            CodeQuestion {
                id: "q1".into(),
                kind: CodeQuestionKind::Callers,
                query: "what calls foo".into(),
                symbol: "foo".into(),
                expected_files: vec!["a.rs".into(), "b.rs".into()],
                expected_min_recall: 0.5,
                note: None,
            },
            CodeQuestion {
                id: "q2".into(),
                kind: CodeQuestionKind::Definition,
                query: "where is bar".into(),
                symbol: "bar".into(),
                expected_files: vec!["c.rs".into()],
                expected_min_recall: 1.0,
                note: None,
            },
        ];
        // Fake strategy: q1 returns [a.rs, x.rs], q2 returns [] (miss).
        let retrieve = |q: &CodeQuestion, _k: usize| {
            let q = q.clone();
            async move {
            if q.id == "q1" {
                Ok(RetrievalOutcome { files: vec!["a.rs".into(), "x.rs".into()], latency_ms: 2 })
            } else {
                Ok(RetrievalOutcome { files: vec![], latency_ms: 1 })
            }
            }
        };
        let opts = RunnerOpts { k: 5, corpus: "zbrain".into(), commit: "deadbeef".into() };
        let report = run_code_retrieval_eval(EvalRunReportMode::Baseline, &questions, &retrieve, &opts).await.unwrap();
        assert_eq!(report.mode, EvalRunReportMode::Baseline);
        assert_eq!(report.questions.len(), 2);
        // q1: a.rs relevant (exact), x.rs not. precision@5 = 1/2 = 0.5; recall@5 = 1/2 = 0.5 >= 0.5 → answered.
        let q1 = &report.questions[0];
        assert!((q1.precision_at_k - 0.5).abs() < 1e-9);
        assert!((q1.recall_at_k - 0.5).abs() < 1e-9);
        assert!(q1.answered);
        assert_eq!(q1.top_1.as_deref(), Some("a.rs"));
        // q2: no retrieved → recall 0 < 1.0 → not answered.
        assert!(!report.questions[1].answered);
        assert!((report.answered_rate - 0.5).abs() < 1e-9);
        assert_eq!(report.commit, "deadbeef");
        assert_eq!(report.schema_version, 1);
    }
}
