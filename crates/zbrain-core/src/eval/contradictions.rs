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
//! Pair discovery has two strategies, selectable via [`PairingMode`]:
//!   * `Corpus` (MVP default): enumerate candidate pairs from sampled takes
//!     (cross-page + intra-page takes).
//!   * `Retrieval` (extension): run `crate::search::hybrid_search` per query,
//!     take the top-K pages, and pair them cross-page (page `compiled_truth`
//!     vs page) and intra-page (page `compiled_truth` vs that page's active
//!     takes, fetched in one batch via `TakesListOpts.page_ids` — the Rust
//!     port of TS `listActiveTakesForPages`). This is the retrieval-discovery
//!     path that was deferred at MVP time; it no longer depends on unported
//!     engine methods (see roadmap node 1-1-5-4).
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

use crate::engine::{BrainEngine, SearchResult};
use crate::eval::cross_modal::ChatRequest;
use crate::search::{hybrid_search, HybridSearchOpts};
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

/// How the pair was constructed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PairKind {
    /// Takes from two different pages (Corpus mode).
    CrossPage,
    /// Two takes within the same page (Corpus mode).
    IntraPage,
    /// Two different pages' `compiled_truth` (Retrieval mode, cross-page).
    RetrievalCross,
    /// A page's `compiled_truth` vs one of that page's active takes
    /// (Retrieval mode, intra-page). Mirrors TS `intra_page_chunk_take`.
    RetrievalIntra,
}

/// Which discovery strategy [`run`] uses to build candidate pairs.
#[derive(Debug, Clone)]
pub enum PairingMode {
    /// Sample takes from the corpus and pair them (MVP default).
    Corpus,
    /// Retrieval-based discovery: run `hybrid_search` for each query, take the
    /// top-K pages, and pair them cross/intra. `top_k` is the per-query limit.
    Retrieval {
        /// One or more retrieval queries; each yields its own candidate pairs.
        queries: Vec<String>,
        /// Max pages (`limit`) passed to `hybrid_search` per query.
        top_k: usize,
    },
}

/// A lightweight, testable projection of a `SearchResult` page used by the
/// retrieval pairing builder. Decouples [`build_retrieval_pairs`] from the
/// full `SearchResult`/`Page` shape so it can be exercised without standing up
/// a search engine.
#[derive(Debug, Clone)]
pub struct RetrievalHit {
    pub page_id: u64,
    pub compiled_truth: String,
    pub effective_date: Option<String>,
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
    /// Stable run id (`new_run_id`) so the run can be persisted and trended.
    pub run_id: String,
    /// Judge model string used for this run.
    pub judge_model: String,
    /// Number of queries evaluated (Rust MVP: 1 probe run = 1 query).
    pub queries_evaluated: u64,
    /// Queries whose findings contain at least one `contradiction` verdict.
    pub queries_with_contradiction: u64,
    /// Total findings flagged across all pairs (== `findings.len()`).
    pub total_contradictions_flagged: u64,
    /// Wilson 95% CI lower bound on the contradiction rate.
    pub wilson_ci_lower: f64,
    /// Wilson 95% CI upper bound on the contradiction rate.
    pub wilson_ci_upper: f64,
    /// Total judge-call cost in USD (Rust MVP: 0.0, not metered yet).
    pub cost_usd_total: f64,
    /// Wall-clock duration of the probe in milliseconds (injected by CLI).
    pub duration_ms: u64,
    /// Per-source-tier pair counts (all zero in the MVP).
    pub source_tier_breakdown: SourceTierBreakdown,
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
    /// Pair discovery strategy (Corpus default, or Retrieval extension).
    pub pairing: PairingMode,
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

/// Eval schema version for the `eval_contradictions_runs` row (mirrors TS
/// `SCHEMA_VERSION`). Bumped when the run-row shape changes incompatibly.
pub const SCHEMA_VERSION: u8 = 1;

/// Prompt version stamped on every run-row (mirrors TS `PROMPT_VERSION`).
pub const PROMPT_VERSION: &str = "2";

/// Per-pair truncation policy label (mirrors TS `TRUNCATION_POLICY`).
pub const TRUNCATION_POLICY: &str = "1500-chars-utf8-safe";

/// Per-source-tier pair counts that fed a probe run (mirrors TS
/// `SourceTierBreakdown`). The Rust MVP does not separate corpus tiers yet,
/// so all counts are zero — recorded honestly rather than fabricated.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SourceTierBreakdown {
    pub curated_vs_curated: u64,
    pub curated_vs_bulk: u64,
    pub bulk_vs_bulk: u64,
    pub other: u64,
}

/// One persisted row of a `eval-suspected-contradictions` probe run (mirrors
/// TS `ContradictionsRunRow` / `TrendRow`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContradictionsRunRow {
    pub run_id: String,
    pub ran_at: String,
    pub schema_version: u8,
    pub judge_model: String,
    pub prompt_version: String,
    pub queries_evaluated: u64,
    pub queries_with_contradiction: u64,
    pub total_contradictions_flagged: u64,
    pub wilson_ci_lower: f64,
    pub wilson_ci_upper: f64,
    pub judge_errors_total: u64,
    pub cost_usd_total: f64,
    pub duration_ms: u64,
    pub source_tier_breakdown: SourceTierBreakdown,
    pub report_json: serde_json::Value,
}

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

/// Build retrieval-based candidate pairs from a single query's top-K results.
///
/// Faithful to TS `generateCrossSlugPairs` + `generateIntraPagePairs` but
/// adapted to Rust's **page-level** retrieval surface (Rust `hybrid_search`
/// returns pages, not chunks):
///   * `RetrievalCross`: every distinct-page pair of hits (page
///     `compiled_truth` vs page `compiled_truth`).
///   * `RetrievalIntra`: each hit's `compiled_truth` paired with every active
///     take on that page (`takes_by_page`), mirroring TS `intra_page_chunk_take`.
///
/// `max_pairs` is a hard cap applied in cross-first then intra-page order. Each
/// pair inherits `query` as its conditioning string (TS conditions the judge
/// on the originating query).
pub fn build_retrieval_pairs(
    query: &str,
    hits: &[RetrievalHit],
    takes_by_page: &HashMap<u64, Vec<Take>>,
    max_pairs: usize,
) -> Vec<ContradictionPair> {
    let mut pairs: Vec<ContradictionPair> = Vec::new();
    let member_from_hit = |h: &RetrievalHit| PairMember {
        page_id: h.page_id,
        take_id: 0,
        claim: h.compiled_truth.clone(),
        kind: "page".to_string(),
        holder: String::new(),
        since: h.effective_date.clone(),
    };

    // Cross-page pairs (distinct page ids only).
    for i in 0..hits.len() {
        for j in (i + 1)..hits.len() {
            if hits[i].page_id == hits[j].page_id {
                continue;
            }
            pairs.push(ContradictionPair {
                kind: PairKind::RetrievalCross,
                query: query.to_string(),
                a: member_from_hit(&hits[i]),
                b: member_from_hit(&hits[j]),
            });
            if pairs.len() >= max_pairs {
                return pairs;
            }
        }
    }

    // Intra-page pairs (page compiled_truth vs that page's active takes).
    for h in hits {
        let Some(takes) = takes_by_page.get(&h.page_id) else {
            continue;
        };
        let page_member = member_from_hit(h);
        for t in takes {
            pairs.push(ContradictionPair {
                kind: PairKind::RetrievalIntra,
                query: query.to_string(),
                a: page_member.clone(),
                b: PairMember {
                    page_id: t.page_id,
                    take_id: t.id,
                    claim: t.claim.clone(),
                    kind: t.kind.clone(),
                    holder: t.holder.clone(),
                    since: t.since_date.clone(),
                },
            });
            if pairs.len() >= max_pairs {
                return pairs;
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
    let (pairs, n_takes) = match &opts.pairing {
        PairingMode::Corpus => {
            let takes = opts
                .engine
                .list_takes(&TakesListOpts {
                    limit: Some(opts.sample as u32),
                    ..Default::default()
                })
                .await?;
            let n = takes.len();
            if n == 0 {
                anyhow::bail!(
                    "eval-suspected-contradictions: no takes to probe (empty corpus). Seed takes first (e.g. `zbrain extract takes`)."
                );
            }
            (build_pairs(&takes, opts.max_pairs, &opts.query), n)
        }
        PairingMode::Retrieval { queries, top_k } => {
            let mut pairs: Vec<ContradictionPair> = Vec::new();
            let mut n_takes_total: usize = 0;
            for q in queries {
                if pairs.len() >= opts.max_pairs {
                    break;
                }
                // Retrieval (page-level). hybrid_search fails open to
                // keyword-only when no embedding provider is configured, so
                // this works offline and improves once embeddings are wired.
                let results: Vec<SearchResult> =
                    hybrid_search(opts.engine, q, &HybridSearchOpts::with_limit(*top_k)).await?;
                let page_ids: Vec<u64> = results.iter().map(|r| r.page.id).collect();
                let fetched = if page_ids.is_empty() {
                    Vec::new()
                } else {
                    // Batch fetch of all retrieved pages' active takes — the
                    // Rust port of TS `listActiveTakesForPages`.
                    opts.engine
                        .list_takes(&TakesListOpts {
                            page_ids: Some(page_ids),
                            active: Some(true),
                            ..Default::default()
                        })
                        .await?
                };
                n_takes_total += fetched.len();
                let mut takes_by_page: HashMap<u64, Vec<Take>> = HashMap::new();
                for t in fetched {
                    takes_by_page.entry(t.page_id).or_default().push(t);
                }
                let hits: Vec<RetrievalHit> = results
                    .iter()
                    .map(|r| RetrievalHit {
                        page_id: r.page.id,
                        compiled_truth: r.page.compiled_truth.clone(),
                        effective_date: r.page.effective_date.clone(),
                    })
                    .collect();
                let remaining = opts.max_pairs - pairs.len();
                pairs.extend(build_retrieval_pairs(q, &hits, &takes_by_page, remaining));
            }
            (pairs, n_takes_total)
        }
    };
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

    // --- Trend-row bookkeeping (mirrors TS runner.ts) ---
    let run_id = new_run_id();
    let queries_evaluated: u64 = if n_pairs > 0 { 1 } else { 0 };
    let queries_with_contradiction: u64 =
        if findings.iter().any(|f| f.verdict == Verdict::Contradiction) {
            1
        } else {
            0
        };
    let total_contradictions_flagged: u64 = findings.len() as u64;
    let (wilson_ci_lower, wilson_ci_upper, _ci_point) =
        wilson_ci(queries_with_contradiction, queries_evaluated);
    let cost_usd_total = 0.0_f64; // MVP: judge cost not metered yet.
    let source_tier_breakdown = SourceTierBreakdown::default();

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
        run_id,
        judge_model: opts.judge_model.clone(),
        queries_evaluated,
        queries_with_contradiction,
        total_contradictions_flagged,
        wilson_ci_lower,
        wilson_ci_upper,
        cost_usd_total,
        duration_ms: 0,
        source_tier_breakdown,
    })
}

/// Wilson score interval (95%) — faithful port of TS `calibration.ts`
/// `wilsonCI(num, den)`. Returns `(lower, upper, point)`.
///
/// With `den <= 0` returns all zeros (no data). `num` is clamped to `[0, den]`.
pub fn wilson_ci(numerator: u64, denominator: u64) -> (f64, f64, f64) {
    let z = 1.959963984540054_f64; // Z_{0.975}
    let n = denominator as f64;
    let k = (numerator as f64).clamp(0.0, n);
    if n <= 0.0 {
        return (0.0, 0.0, 0.0);
    }
    let p = k / n;
    let z2 = z * z;
    let center = (p + z2 / (2.0 * n)) / (1.0 + z2 / n);
    let margin = (z * (p * (1.0 - p) / n + z2 / (4.0 * n * n)).sqrt()) / (1.0 + z2 / n);
    let lower = if k == 0.0 { 0.0 } else { (center - margin).max(0.0) };
    let upper = if k == n { 1.0 } else { (center + margin).min(1.0) };
    (lower, upper, center)
}

/// Build a stable run id. Faithful to TS `runner.ts` (ISO timestamp with
/// `:`/`.` replaced by `-`), e.g. `2026-08-13T12-34-56-789Z`.
pub fn new_run_id() -> String {
    crate::time::current_utc_iso8601()
        .replace([':', '.'], "-")
}

/// Assemble a persistable run-row from a finished [`ContradictionsResult`].
/// `duration_ms` is supplied by the CLI (which wraps `run` with a timer); the
/// result's own `duration_ms` is the in-probe placeholder.
pub fn build_contradictions_run_row(
    result: &ContradictionsResult,
    duration_ms: u64,
) -> ContradictionsRunRow {
    ContradictionsRunRow {
        run_id: result.run_id.clone(),
        ran_at: crate::time::current_utc_iso8601(),
        schema_version: SCHEMA_VERSION,
        judge_model: result.judge_model.clone(),
        prompt_version: PROMPT_VERSION.to_string(),
        queries_evaluated: result.queries_evaluated,
        queries_with_contradiction: result.queries_with_contradiction,
        total_contradictions_flagged: result.total_contradictions_flagged,
        wilson_ci_lower: result.wilson_ci_lower,
        wilson_ci_upper: result.wilson_ci_upper,
        judge_errors_total: result.judge_errors.total as u64,
        cost_usd_total: result.cost_usd_total,
        duration_ms,
        source_tier_breakdown: result.source_tier_breakdown.clone(),
        report_json: serde_json::json!({
            "n_takes": result.n_takes,
            "n_pairs": result.n_pairs,
            "judged": result.judged,
            "verdict_breakdown": result.verdict_breakdown,
            "severity_breakdown": result.severity_breakdown,
            "judge_errors": {
                "total": result.judge_errors.total,
                "parse_fail": result.judge_errors.parse_fail,
                "unknown": result.judge_errors.unknown,
                "note": result.judge_errors.note,
            },
            "findings_count": result.findings.len(),
            "truncation_policy": TRUNCATION_POLICY,
        }),
    }
}

/// Render the trend as a fixed-width ASCII table (faithful port of TS
/// `trends.ts` `renderTrendChart`). Columns: ran_at / judge_model /
/// queries_evaluated / queries_with_contradiction / total_contradictions_flagged
/// / Wilson CI / bar; the most recent run's `verdict_breakdown` is appended.
pub fn render_trend_chart(rows: &[ContradictionsRunRow]) -> String {
    if rows.is_empty() {
        return "No probe runs recorded yet. Run `zbrain eval suspected-contradictions run` first."
            .to_string();
    }
    let mut out = String::new();
    out.push_str(
        "Date (ran_at)            Model                      Q    WithCx Flag  CI95                      Bar\n",
    );
    out.push_str(
        "------------------------ -------------------------- ----- ------- ----- ------------------------- --------------\n",
    );
    for r in rows {
        let date = trunc_display(&r.ran_at, 24);
        let model = trunc_display(&r.judge_model, 26);
        let q = format!("{:>5}", r.queries_evaluated);
        let with_cx = format!("{:>7}", r.queries_with_contradiction);
        let flag = format!("{:>5}", r.total_contradictions_flagged);
        let ci = format!("[{:.3}, {:.3}]", r.wilson_ci_lower, r.wilson_ci_upper);
        let rate = if r.queries_evaluated > 0 {
            r.queries_with_contradiction as f64 / r.queries_evaluated as f64
        } else {
            0.0
        };
        let bar_len = (rate * 20.0).round() as usize;
        let bar = "#".repeat(bar_len.min(20));
        out.push_str(&format!(
            "{:<24} {:<26} {:>5} {:>7} {:>5} {:<25} {}\n",
            date, model, q, with_cx, flag, ci, bar
        ));
    }
    out.push_str("\nLatest run verdict breakdown:\n");
    if let Some(vb) = rows.first().and_then(|r| r.report_json.get("verdict_breakdown")) {
        if let Some(map) = vb.as_object() {
            for (k, v) in map {
                out.push_str(&format!("  {:<24} {}\n", k, v));
            }
        }
    }
    out
}

/// Right-truncate a string to `max` chars for fixed-width display.
fn trunc_display(s: &str, max: usize) -> String {
    let mut out = String::new();
    let mut n = 0;
    for ch in s.chars() {
        if n >= max {
            break;
        }
        out.push(ch);
        n += 1;
    }
    out
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
    use crate::engine::{InMemoryEngine, PageInput};
    use crate::search::{hybrid_search, HybridSearchOpts};
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
            pairing: PairingMode::Corpus,
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

    #[test]
    fn build_retrieval_pairs_builds_cross_and_intra() {
        // Two distinct pages -> one cross-page pair.
        let hits = vec![
            RetrievalHit {
                page_id: 1,
                compiled_truth: "Markets show rising valuation concerns.".to_string(),
                effective_date: Some("2024-01-01".to_string()),
            },
            RetrievalHit {
                page_id: 2,
                compiled_truth: "Valuation is fair and markets are efficient.".to_string(),
                effective_date: None,
            },
        ];
        // Two active takes on page 1 only.
        let takes_by_page = HashMap::from([
            (
                1u64,
                vec![
                    Take {
                        id: 100,
                        page_id: 1,
                        row_num: 1,
                        claim: "valuation looks stretched".to_string(),
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
                    },
                    Take {
                        id: 101,
                        page_id: 1,
                        row_num: 2,
                        claim: "valuation is reasonable".to_string(),
                        kind: "bet".to_string(),
                        holder: "bob".to_string(),
                        weight: 0.6,
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
                    },
                ],
            ),
        ]);

        // Cap of 20 should keep all 3 pairs (1 cross + 2 intra).
        let pairs = build_retrieval_pairs("valuation", &hits, &takes_by_page, 20);
        assert_eq!(pairs.len(), 3);
        let cross = pairs.iter().filter(|p| matches!(p.kind, PairKind::RetrievalCross)).count();
        let intra = pairs.iter().filter(|p| matches!(p.kind, PairKind::RetrievalIntra)).count();
        assert_eq!(cross, 1);
        assert_eq!(intra, 2);
        // Every pair inherits the conditioning query.
        assert!(pairs.iter().all(|p| p.query == "valuation"));
        // Intra pair b is the actual take (take_id set, holder preserved).
        let intra_pair = pairs.iter().find(|p| matches!(p.kind, PairKind::RetrievalIntra)).unwrap();
        assert_eq!(intra_pair.b.take_id, 100);
        assert_eq!(intra_pair.b.holder, "alice");
        // Page member uses page effective_date as its date anchor.
        assert_eq!(intra_pair.a.since.as_deref(), Some("2024-01-01"));

        // Hard cap is respected.
        let capped = build_retrieval_pairs("valuation", &hits, &takes_by_page, 1);
        assert_eq!(capped.len(), 1);
    }

    #[tokio::test]
    async fn run_retrieval_pairs_without_api_key() {
        let engine = InMemoryEngine::new();
        // Two pages whose compiled_truth shares the retrieval keyword.
        engine
            .put_page(
                "valuation-notes",
                Some("default"),
                &PageInput {
                    page_type: "note".to_string(),
                    title: "Valuation notes".to_string(),
                    compiled_truth: "Markets show rising valuation concerns this quarter.".to_string(),
                    ..Default::default()
                },
            )
            .await
            .unwrap();
        engine
            .put_page(
                "markets-notes",
                Some("default"),
                &PageInput {
                    page_type: "note".to_string(),
                    title: "Markets notes".to_string(),
                    compiled_truth: "Valuation is fair and markets are efficient.".to_string(),
                    ..Default::default()
                },
            )
            .await
            .unwrap();

        // Resolve the retrieved page ids so takes attach to the right pages.
        let results = hybrid_search(&engine, "valuation", &HybridSearchOpts::with_limit(5))
            .await
            .unwrap();
        assert!(results.len() >= 2, "expected both pages to match 'valuation'");
        let pid1 = results[0].page.id;
        // One active take on page 1 -> an intra-page pair candidate.
        engine.add_take(mk_take(900, pid1, "valuation looks stretched here"));

        let receipt_dir =
            std::env::temp_dir().join(format!("sc_test_retrieval_{}", std::process::id()));
        std::fs::create_dir_all(&receipt_dir).ok();

        let opts = ContradictionOpts {
            engine: &engine,
            sample: 50,
            max_pairs: 20,
            query: DEFAULT_QUERY.to_string(),
            pairing: PairingMode::Retrieval {
                queries: vec!["valuation".to_string()],
                top_k: 5,
            },
            judge_model: DEFAULT_JUDGE_MODEL.to_string(),
            max_pair_chars: 1500,
            max_tokens: 1000,
            receipt_dir,
            slug: Some("test-sc-retrieval".to_string()),
        };
        let res = run(&opts, &fake_clean_judge).await.unwrap();
        // 2 pages -> 1 cross pair; page 1 has 1 take -> 1 intra pair.
        assert_eq!(res.n_pairs, 2, "expected 1 cross + 1 intra pair");
        assert_eq!(res.judged, 2);
        assert_eq!(res.n_takes, 1, "one active take fetched for the page");
        assert!(res
            .findings
            .iter()
            .all(|f| f.kind == "RetrievalCross" || f.kind == "RetrievalIntra"));
        assert!(res.receipt_path.is_some());
    }

    #[test]
    fn wilson_ci_matches_calibration_contract() {
        // den=0 -> all zeros (no data).
        assert_eq!(wilson_ci(0, 0), (0.0, 0.0, 0.0));
        // k==0 -> lower bound clamped to 0.
        let (lo, hi, _pt) = wilson_ci(0, 1);
        assert_eq!(lo, 0.0);
        assert!(hi > 0.0 && hi <= 1.0);
        // k==n -> upper bound clamped to 1.
        let (lo2, hi2, _pt2) = wilson_ci(1, 1);
        assert_eq!(hi2, 1.0);
        assert!(lo2 >= 0.0 && lo2 < 1.0);
    }

    #[test]
    fn new_run_id_is_timestamp_like_and_fs_safe() {
        let id = new_run_id();
        // No ':' or '.' remain after the dash substitution.
        assert!(!id.contains(':') && !id.contains('.'));
        assert!(id.starts_with("20")); // ISO year prefix
    }

    #[tokio::test]
    async fn build_contradictions_run_row_round_trips_scalars() {
        let engine = InMemoryEngine::new();
        for i in 0..4u64 {
            engine.add_take(mk_take(i, i % 2, &format!("take {i}")));
        }
        let receipt_dir =
            std::env::temp_dir().join(format!("sc_test_row_{}", std::process::id()));
        std::fs::create_dir_all(&receipt_dir).ok();
        let opts = base_opts(&engine, receipt_dir);
        let res = run(&opts, &fake_contradiction_judge).await.unwrap();
        let row = build_contradictions_run_row(&res, 1234);
        assert_eq!(row.run_id, res.run_id);
        assert_eq!(row.queries_evaluated, 1);
        assert_eq!(row.queries_with_contradiction, 1);
        assert_eq!(row.total_contradictions_flagged, 6);
        assert_eq!(row.judge_errors_total, 0);
        assert_eq!(row.duration_ms, 1234);
        assert_eq!(row.schema_version, SCHEMA_VERSION);
        assert!(row.report_json.get("findings_count").is_some());
    }

    #[test]
    fn render_trend_chart_handles_empty_and_rows() {
        let empty = render_trend_chart(&[]);
        assert!(empty.contains("No probe runs recorded yet"));
        let rows = vec![
            ContradictionsRunRow {
                run_id: "r1".to_string(),
                ran_at: "2026-08-13T00:00:00Z".to_string(),
                schema_version: 1,
                judge_model: "anthropic:claude-haiku-4-5".to_string(),
                prompt_version: "2".to_string(),
                queries_evaluated: 1,
                queries_with_contradiction: 1,
                total_contradictions_flagged: 3,
                wilson_ci_lower: 0.1,
                wilson_ci_upper: 0.9,
                judge_errors_total: 0,
                cost_usd_total: 0.0,
                duration_ms: 10,
                source_tier_breakdown: SourceTierBreakdown::default(),
                report_json: serde_json::json!({"verdict_breakdown": {"contradiction": 3}}),
            },
        ];
        let chart = render_trend_chart(&rows);
        assert!(chart.contains("Date (ran_at)"));
        assert!(chart.contains("Latest run verdict breakdown"));
        assert!(chart.contains("contradiction"));
    }

    #[tokio::test]
    async fn inmemory_write_and_load_trend_round_trip() {
        let engine = InMemoryEngine::new();
        let row = ContradictionsRunRow {
            run_id: "r1".to_string(),
            ran_at: "2026-08-13T00:00:00Z".to_string(),
            schema_version: 1,
            judge_model: "anthropic:claude-haiku-4-5".to_string(),
            prompt_version: "2".to_string(),
            queries_evaluated: 1,
            queries_with_contradiction: 1,
            total_contradictions_flagged: 3,
            wilson_ci_lower: 0.1,
            wilson_ci_upper: 0.9,
            judge_errors_total: 0,
            cost_usd_total: 0.0,
            duration_ms: 10,
            source_tier_breakdown: SourceTierBreakdown::default(),
            report_json: serde_json::json!({"verdict_breakdown": {"contradiction": 3}}),
        };
        // First write records it.
        assert!(engine.write_contradictions_run(&row).await.unwrap());
        // Idempotent replay does not double-count.
        assert!(!engine.write_contradictions_run(&row).await.unwrap());
        let trend = engine.load_contradictions_trend(30).await.unwrap();
        assert_eq!(trend.len(), 1);
        assert_eq!(trend[0].run_id, "r1");
    }
}
