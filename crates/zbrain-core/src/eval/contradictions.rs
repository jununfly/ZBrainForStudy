//! Rust port of TS `src/core/eval-contradictions/` — MVP probe.
//!
//! The TS source is a full subsystem (18 files): an orchestrator runner, a
//! custom query-conditioned judge, calibration-join, cost-tracker,
//! severity-classify, trends (run-row + ASCII chart), cache, cross-source,
//! date-filter, auto-supersession, judge-errors. The Rust port here is the
//! **MVP slice** of that surface — faithful to the eval *contract* (verdict /
//! severity taxonomy, query-conditioned one-call-one-pair judge, judge-errors
//! counted as first-class) but deliberately narrower:
//!
//! - Pair discovery uses the **existing takes corpus** (enumerate candidate
//!   pairs from sampled takes), NOT `engine.hybridSearch` retrieval. The
//!   retrieval-based discovery path is deferred (it needs `hybrid_search` /
//!   `embed_query` engine methods that are not yet ported — see roadmap node
//!   1-1-5-4).
//! - Only the `run` probe is implemented. The `trend` (run-row ASCII chart)
//!   and `review` (surface latest findings) subcommands are present in the CLI
//!   but return an informative "deferred in MVP" error.
//! - The judge is a single utility-tier model (one-call-one-pair), not the
//!   3-model cross-modal panel. The verdict/severity taxonomy is the same as
//!   TS, so findings remain comparable.
//!
//! Honest degradation: when the corpus is empty, [`run`] returns `Err` rather
//! than a fake clean report. When the judge call fails to parse, the failure
//! is counted in [`JudgeErrorsCounts`] (part of the denominator), never
//! silently skipped.

use std::collections::HashMap;
use std::path::PathBuf;

use anyhow::Result;
use serde::{Deserialize, Serialize};

use crate::engine::BrainEngine;
use crate::eval::cross_modal::ChatRequest;
use crate::types::{Take, TakesListOpts};

/// The six-member verdict taxonomy, faithful to
/// `src/core/eval-contradictions/types.ts` (v0.34 / Lane A2).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Verdict {
    /// Drop from findings (not surfaced).
    NoContradiction,
    /// Genuine conflict at the same point in time.
    Contradiction,
    /// Newer claim updates/replaces older; not an error.
    TemporalSupersession,
    /// Metric/status went backwards over time.
    TemporalRegression,
    /// Legitimate change over time, neither of the above.
    TemporalEvolution,
    /// Judge misread an explicit negation in one chunk.
    NegationArtifact,
}

impl Verdict {
    /// Whether this verdict should be surfaced as a finding (i.e. not
    /// `no_contradiction`).
    pub fn is_finding(&self) -> bool {
        !matches!(self, Verdict::NoContradiction)
    }
}

/// Severity rank, faithful to TS. `info` is non-error-class (v0.34 Lane A2).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Severity {
    Info,
    Low,
    Medium,
    High,
}

/// Resolution kinds, faithful to TS `ResolutionKind`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ResolutionKind {
    TakesSupersede,
    DreamSynthesize,
    TakesMarkDebate,
    ManualReview,
    TemporalSupersede,
    FlagForReview,
    LogTimelineChange,
}

/// One end of a pair (unified shape across kinds).
#[derive(Debug, Clone)]
pub struct PairMember {
    pub page_id: u64,
    pub take_id: u64,
    pub claim: String,
    pub kind: String,
    pub holder: String,
    pub since: Option<String>,
}

/// A candidate pair to judge.
#[derive(Debug, Clone)]
pub struct ContradictionPair {
    pub kind: PairKind,
    pub query: String,
    pub a: PairMember,
    pub b: PairMember,
}

/// How the pair was constructed in the MVP (without hybrid_search retrieval).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PairKind {
    /// Takes from two different pages.
    CrossPage,
    /// Two takes within the same page.
    IntraPage,
}

/// The judge's parsed verdict for a single pair.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JudgeVerdict {
    pub verdict: Verdict,
    pub severity: Severity,
    /// One-line description of what they disagree about.
    #[serde(default)]
    pub axis: String,
    pub confidence: f64,
    #[serde(default)]
    pub resolution_kind: Option<ResolutionKind>,
}

/// Error classes counted toward the run's denominator (NOT silent skips).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum JudgeErrorKind {
    ParseFail,
    Refusal,
    Timeout,
    Http5xx,
    Unknown,
}

/// Typed, first-class error counters. Mirrors TS `JudgeErrorsCounts`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct JudgeErrorsCounts {
    pub parse_fail: u32,
    pub refusal: u32,
    pub timeout: u32,
    pub http_5xx: u32,
    pub unknown: u32,
    pub total: u32,
    /// Surfaced verbatim so users know errors are counted, not silent.
    pub note: String,
}

impl JudgeErrorsCounts {
    fn bump(&mut self, kind: JudgeErrorKind) {
        match kind {
            JudgeErrorKind::ParseFail => self.parse_fail += 1,
            JudgeErrorKind::Refusal => self.refusal += 1,
            JudgeErrorKind::Timeout => self.timeout += 1,
            JudgeErrorKind::Http5xx => self.http_5xx += 1,
            JudgeErrorKind::Unknown => self.unknown += 1,
        }
        self.total += 1;
    }
}

/// One surfaced finding (a non-`no_contradiction` verdict).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContradictionFinding {
    pub pair_id: String,
    pub kind: String,
    pub verdict: Verdict,
    pub severity: Severity,
    pub axis: String,
    pub confidence: f64,
    pub resolution_kind: Option<ResolutionKind>,
}

/// Aggregated result of a probe run.
#[derive(Debug, Serialize, Deserialize)]
pub struct ContradictionsResult {
    /// How many takes were sampled from the corpus.
    pub n_takes: usize,
    /// How many pairs were constructed and judged.
    pub n_pairs: usize,
    /// Pairs whose judge returned a parseable verdict.
    pub judged: u64,
    /// Count of pairs per verdict (includes no_contradiction).
    pub verdict_breakdown: HashMap<String, u64>,
    /// Count of findings per severity.
    pub severity_breakdown: HashMap<String, u64>,
    /// First-class judge errors (part of the denominator).
    pub judge_errors: JudgeErrorsCounts,
    /// Surfaced findings (non-`no_contradiction`).
    pub findings: Vec<ContradictionFinding>,
    /// Path to the written JSON summary receipt (if any).
    pub receipt_path: Option<String>,
}

/// Options for [`run`].
pub struct ContradictionOpts<'a> {
    pub engine: &'a dyn BrainEngine,
    /// Number of takes to sample from the corpus (pair pool).
    pub sample: usize,
    /// Hard cap on the number of pairs judged (cost guard).
    pub max_pairs: usize,
    /// Conditioning query string applied to every pair (query-conditioned judge).
    pub query: String,
    /// Judge model string (`provider:model`). Resolved by the CLI layer.
    pub judge_model: String,
    /// UTF-8-safe per-pair truncation budget.
    pub max_pair_chars: usize,
    /// Per-call max output tokens for the judge model.
    pub max_tokens: u32,
    /// Where the JSON summary receipt is written.
    pub receipt_dir: PathBuf,
    /// Receipt filename slug.
    pub slug: Option<String>,
}

/// Default conditioning query when the user does not supply one.
pub const DEFAULT_QUERY: &str = "General consistency audit across the brain's takes.";

/// Default utility-tier judge model (TS default: anthropic:claude-haiku-4-5).
pub const DEFAULT_JUDGE_MODEL: &str = "anthropic:claude-haiku-4-5";

/// UTF-8-safe truncation: cap at `max_chars` but never split a code point.
fn truncate_utf8(text: &str, max_chars: usize) -> String {
    if text.chars().count() <= max_chars {
        return text.to_string();
    }
    text.chars().take(max_chars).collect()
}

/// Render a pair member as the statement text the judge sees.
fn render_member(m: &PairMember, max_pair_chars: usize) -> String {
    let date = m.since.clone().unwrap_or_else(|| "date unknown".to_string());
    let body = truncate_utf8(&m.claim, max_pair_chars / 2);
    format!(
        "[{} | holder={} | since={}]\n  {}",
        m.kind, m.holder, date, body
    )
}

/// Build the system prompt for the contradiction judge.
fn judge_system_prompt() -> String {
    "You are a contradiction judge for a personal knowledge base ('brain'). \
     You receive a user query and two statements (A and B). Decide whether they \
     genuinely contradict, are temporally related, or are consistent. \
     Respond with ONLY a JSON object, no prose, no markdown fences:\n\
     {\n  \"verdict\": \"no_contradiction\" | \"contradiction\" | \"temporal_supersession\" | \"temporal_regression\" | \"temporal_evolution\" | \"negation_artifact\",\n  \"severity\": \"info\" | \"low\" | \"medium\" | \"high\",\n  \"axis\": \"<one-line description of what they disagree about, or empty>\",\n  \"confidence\": <float 0..1>,\n  \"resolution_kind\": null | \"takes_supersede\" | \"dream_synthesize\" | \"takes_mark_debate\" | \"manual_review\" | \"temporal_supersede\" | \"flag_for_review\" | \"log_timeline_change\"\n}\n\
     Use 'no_contradiction' when the two statements are consistent or simply \
     about different topics. Use 'contradiction' only for a genuine conflict \
     at the same point in time. Prefer the temporal_* verdicts when the \
     disagreement is explained by time passing."
        .to_string()
}

/// Build the per-pair user prompt.
fn judge_user_prompt(pair: &ContradictionPair, max_pair_chars: usize) -> String {
    format!(
        "Query: {}\n\nStatement A:\n{}\n\nStatement B:\n{}\n\nRespond with JSON only.",
        pair.query,
        render_member(&pair.a, max_pair_chars),
        render_member(&pair.b, max_pair_chars)
    )
}

/// Robust JSON parse: strip ```json fences, then strict parse. Returns the
/// parsed `JudgeVerdict` or an error kind to count.
fn parse_judge_json(text: &str) -> Result<JudgeVerdict, JudgeErrorKind> {
    let trimmed = text.trim();
    let body = if let Some(stripped) = trimmed
        .strip_prefix("```json")
        .or_else(|| trimmed.strip_prefix("```"))
    {
        stripped
            .trim_start_matches("```json")
            .trim_start_matches("```")
            .trim_end_matches("```")
            .trim()
            .to_string()
    } else {
        trimmed.to_string()
    };
    // Extract the first {...} block if there is surrounding prose.
    let json_candidate = if let Some(start) = body.find('{') {
        if let Some(end) = body.rfind('}') {
            &body[start..=end]
        } else {
            &body[..]
        }
    } else {
        &body[..]
    };
    match serde_json::from_str::<JudgeVerdict>(json_candidate) {
        Ok(v) => Ok(v),
        Err(_) => Err(JudgeErrorKind::ParseFail),
    }
}

/// Sample takes and build a bounded set of candidate pairs.
///
/// Without `engine.hybridSearch` (deferred), we enumerate pairs from the
/// sampled takes directly: cross-page pairs (different `page_id`) and
/// intra-page pairs (same `page_id`). The list is capped at `max_pairs`.
fn build_pairs(takes: &[Take], max_pairs: usize, query: &str) -> Vec<ContradictionPair> {
    let mut pairs: Vec<ContradictionPair> = Vec::new();
    'outer: for i in 0..takes.len() {
        for j in (i + 1)..takes.len() {
            let a = &takes[i];
            let b = &takes[j];
            let kind = if a.page_id == b.page_id {
                PairKind::IntraPage
            } else {
                PairKind::CrossPage
            };
            let mk_member = |t: &Take| PairMember {
                page_id: t.page_id,
                take_id: t.id,
                claim: t.claim.clone(),
                kind: t.kind.clone(),
                holder: t.holder.clone(),
                since: t.since_date.clone(),
            };
            pairs.push(ContradictionPair {
                kind,
                query: query.to_string(),
                a: mk_member(a),
                b: mk_member(b),
            });
            if pairs.len() >= max_pairs {
                break 'outer;
            }
        }
    }
    pairs
}

/// Run the suspected-contradictions probe.
///
/// Samples `opts.sample` takes, builds candidate pairs, judges each with the
/// supplied `chat` closure, and aggregates verdict / severity breakdowns plus
/// first-class judge errors. Honest degradation: returns `Err` if the corpus
/// is empty.
pub async fn run<F, Fut>(opts: &ContradictionOpts<'_>, chat: &F) -> Result<ContradictionsResult>
where
    F: Fn(ChatRequest) -> Fut,
    Fut: std::future::Future<Output = Result<String>>,
{
    let takes = opts
        .engine
        .list_takes(&TakesListOpts {
            limit: Some(opts.sample as u32),
            ..Default::default()
        })
        .await?;
    let n_takes = takes.len();
    if n_takes == 0 {
        anyhow::bail!(
            "eval-suspected-contradictions: no takes to probe (empty corpus). Seed takes first (e.g. `zbrain extract takes`)."
        );
    }

    let pairs = build_pairs(&takes, opts.max_pairs, &opts.query);
    let n_pairs = pairs.len();

    let system = judge_system_prompt();
    let mut verdict_breakdown: HashMap<String, u64> = HashMap::new();
    let mut severity_breakdown: HashMap<String, u64> = HashMap::new();
    let mut judge_errors = JudgeErrorsCounts::default();
    let mut findings: Vec<ContradictionFinding> = Vec::new();
    let mut judged: u64 = 0;

    for (idx, pair) in pairs.iter().enumerate() {
        let pair_id = format!("pair-{:04}", idx);
        let prompt = judge_user_prompt(pair, opts.max_pair_chars);
        let req = ChatRequest {
            model: opts.judge_model.clone(),
            system: system.clone(),
            prompt,
            max_tokens: opts.max_tokens,
        };
        let raw = match chat(req).await {
            Ok(text) => text,
            Err(e) => {
                // Transport-level failure: count as unknown, keep going.
                judge_errors.bump(JudgeErrorKind::Unknown);
                let reason = format!("{e:?}");
                if judge_errors.note.is_empty() {
                    judge_errors.note = format!("pair {pair_id} chat error: {reason}");
                }
                continue;
            }
        };
        match parse_judge_json(&raw) {
            Ok(v) => {
                judged += 1;
                *verdict_breakdown
                    .entry(serde_json::to_string(&v.verdict).unwrap_or_default())
                    .or_insert(0) += 1;
                *severity_breakdown
                    .entry(serde_json::to_string(&v.severity).unwrap_or_default())
                    .or_insert(0) += 1;
                if v.verdict.is_finding() {
                    findings.push(ContradictionFinding {
                        pair_id,
                        kind: format!("{:?}", pair.kind),
                        verdict: v.verdict,
                        severity: v.severity,
                        axis: v.axis,
                        confidence: v.confidence,
                        resolution_kind: v.resolution_kind,
                    });
                }
            }
            Err(kind) => {
                judge_errors.bump(kind);
                if judge_errors.note.is_empty() {
                    judge_errors.note = format!("pair {pair_id} judge parse failure");
                }
            }
        }
    }

    // Persist a JSON summary receipt for inspection (mirrors the eval-receipts
    // convention used by cross_modal / takes_quality).
    let receipt_path = write_summary_receipt(opts, n_takes, n_pairs, judged, &verdict_breakdown, &severity_breakdown, &judge_errors, &findings)?;

    Ok(ContradictionsResult {
        n_takes,
        n_pairs,
        judged,
        verdict_breakdown,
        severity_breakdown,
        judge_errors,
        findings,
        receipt_path,
    })
}

/// Write a JSON summary to the receipt dir; returns the path (or None on failure).
fn write_summary_receipt(
    opts: &ContradictionOpts<'_>,
    n_takes: usize,
    n_pairs: usize,
    judged: u64,
    verdict_breakdown: &HashMap<String, u64>,
    severity_breakdown: &HashMap<String, u64>,
    judge_errors: &JudgeErrorsCounts,
    findings: &[ContradictionFinding],
) -> Result<Option<String>> {
    let slug = opts
        .slug
        .clone()
        .unwrap_or_else(|| "suspected-contradictions".to_string());
    let dir = &opts.receipt_dir;
    if let Err(e) = std::fs::create_dir_all(dir) {
        return Ok(Some(format!("(<receipt dir unwritable: {e}>)")));
    }
    let path = dir.join(format!("{slug}.json"));
    let summary = serde_json::json!({
        "schema_version": 1,
        "n_takes": n_takes,
        "n_pairs": n_pairs,
        "judged": judged,
        "verdict_breakdown": verdict_breakdown,
        "severity_breakdown": severity_breakdown,
        "judge_errors": judge_errors,
        "n_findings": findings.len(),
        "findings": findings,
    });
    match std::fs::write(&path, serde_json::to_string_pretty(&summary)?) {
        Ok(()) => Ok(Some(path.to_string_lossy().to_string())),
        Err(e) => Ok(Some(format!("(<receipt write failed: {e}>)"))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::InMemoryEngine;
    use crate::types::Take;

    /// A fake judge returning a clean `no_contradiction` verdict, so the full
    /// runner can be exercised without an API key.
    async fn fake_clean_judge(_req: ChatRequest) -> Result<String> {
        Ok(
            r#"{"verdict":"no_contradiction","severity":"info","axis":"","confidence":0.95,"resolution_kind":null}"#
                .to_string(),
        )
    }

    /// A fake judge returning a genuine contradiction.
    async fn fake_contradiction_judge(_req: ChatRequest) -> Result<String> {
        Ok(
            r#"{"verdict":"contradiction","severity":"medium","axis":"valuation","confidence":0.8,"resolution_kind":"manual_review"}"#
                .to_string(),
        )
    }

    /// A fake judge returning unparseable garbage — exercises the
    /// "judge-errors are first-class, not silent" path.
    async fn fake_garbage_judge(_req: ChatRequest) -> Result<String> {
        Ok("the model forgot to speak JSON today".to_string())
    }

    fn mk_take(id: u64, page_id: u64, claim: &str) -> Take {
        Take {
            id,
            page_id,
            row_num: id as i32,
            claim: claim.to_string(),
            kind: "bet".to_string(),
            holder: "alice".to_string(),
            weight: 0.7,
            since_date: None,
            until_date: None,
            source: Some("note".to_string()),
            superseded_by: None,
            active: true,
            resolved_at: None,
            resolved_quality: None,
            resolved_outcome: None,
            resolved_evidence: None,
            resolved_value: None,
            resolved_unit: None,
            resolved_by: None,
            created_at: "2026-01-01T00:00:00Z".to_string(),
            updated_at: "2026-01-01T00:00:00Z".to_string(),
        }
    }

    fn base_opts(engine: &dyn BrainEngine, receipt_dir: PathBuf) -> ContradictionOpts<'_> {
        ContradictionOpts {
            engine,
            sample: 50,
            max_pairs: 20,
            query: DEFAULT_QUERY.to_string(),
            judge_model: DEFAULT_JUDGE_MODEL.to_string(),
            max_pair_chars: 1500,
            max_tokens: 1000,
            receipt_dir,
            slug: Some("test-sc".to_string()),
        }
    }

    #[tokio::test]
    async fn run_scores_pairs_without_api_key() {
        let engine = InMemoryEngine::new();
        for i in 0..6u64 {
            engine.add_take(mk_take(i, i % 3, &format!("take {i}: markets are efficient")));
        }
        let receipt_dir =
            std::env::temp_dir().join(format!("sc_test_{}", std::process::id()));
        std::fs::create_dir_all(&receipt_dir).ok();

        let opts = base_opts(&engine, receipt_dir);
        let res = run(&opts, &fake_clean_judge).await.unwrap();
        assert_eq!(res.n_takes, 6);
        // 6 takes -> C(6,2)=15 pairs, capped at max_pairs=20 -> 15.
        assert_eq!(res.n_pairs, 15);
        assert_eq!(res.judged, 15);
        assert_eq!(res.judge_errors.total, 0);
        // All clean -> no findings.
        assert_eq!(res.findings.len(), 0);
        assert!(res
            .verdict_breakdown
            .get(&serde_json::to_string(&Verdict::NoContradiction).unwrap())
            .copied()
            .unwrap_or(0)
            == 15);
        assert!(res.receipt_path.is_some());
    }

    #[tokio::test]
    async fn run_surfaces_contradiction_findings() {
        let engine = InMemoryEngine::new();
        for i in 0..4u64 {
            engine.add_take(mk_take(i, i % 2, &format!("take {i}")));
        }
        let receipt_dir =
            std::env::temp_dir().join(format!("sc_test_find_{}", std::process::id()));
        std::fs::create_dir_all(&receipt_dir).ok();

        let opts = base_opts(&engine, receipt_dir);
        let res = run(&opts, &fake_contradiction_judge).await.unwrap();
        assert_eq!(res.judged, 6); // C(4,2)=6
        assert_eq!(res.findings.len(), 6);
        assert!(res
            .severity_breakdown
            .get(&serde_json::to_string(&Severity::Medium).unwrap())
            .copied()
            .unwrap_or(0)
            == 6);
    }

    #[tokio::test]
    async fn run_counts_judge_errors_as_first_class() {
        let engine = InMemoryEngine::new();
        for i in 0..4u64 {
            engine.add_take(mk_take(i, i % 2, &format!("take {i}")));
        }
        let receipt_dir =
            std::env::temp_dir().join(format!("sc_test_err_{}", std::process::id()));
        std::fs::create_dir_all(&receipt_dir).ok();

        let opts = base_opts(&engine, receipt_dir);
        let res = run(&opts, &fake_garbage_judge).await.unwrap();
        // No successful verdicts, but the run did NOT crash — errors counted.
        assert_eq!(res.judged, 0);
        assert_eq!(res.judge_errors.total, 6);
        assert_eq!(res.judge_errors.parse_fail, 6);
        assert_eq!(res.findings.len(), 0);
    }

    #[tokio::test]
    async fn run_errors_on_empty_corpus() {
        let engine = InMemoryEngine::new();
        let receipt_dir =
            std::env::temp_dir().join(format!("sc_test_empty_{}", std::process::id()));
        std::fs::create_dir_all(&receipt_dir).ok();

        let opts = base_opts(&engine, receipt_dir);
        let err = run(&opts, &fake_clean_judge).await.unwrap_err();
        assert!(err.to_string().contains("no takes to probe"));
    }
}
