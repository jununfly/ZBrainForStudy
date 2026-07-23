//! Calibration algorithm layer (Part11 1-3, Phase 1 — zero-dependency pure
//! functions ported from `src/core/calibration/*.ts`).
//!
//! Phase 1 scope: the pure, side-effect-free formatters/parsers that need no
//! `BrainEngine` and no LLM. These mirror the TS templates verbatim so the
//! web admin / CLI can call Rust instead of the TS module. Engine-backed and
//! LLM-backed calibration functions (e.g. `forecastForTake`, `gateVoice`,
//! `runAbTrial`) are Phase 2 and may surface as KNOWN-GAPs.
//!
//! Note: no roadmap node number is referenced here on purpose — the Part11
//! roadmap JSON is a temporary working file and will be cleared on completion,
//! so comments must stay self-explanatory.

use crate::calibration_queries::{CalibrationProfileRow, CalibrationQueries, TakesScorecard};
use async_trait::async_trait;
use crate::engine::BrainEngine;
use serde::{Serialize, Deserialize};
use std::collections::HashMap;
use std::io::ErrorKind as IoErrorKind;
use std::process::Command;

// ── voice-gate fallback templates (templates.ts) ──────────────────────────

/// Voice-gate modes. Every mode MUST have a template in this module; the
/// web admin pins parity against this list.
pub const VOICE_GATE_MODES: &[&str] = &[
    "pattern_statement",
    "nudge",
    "forecast_blurb",
    "dashboard_caption",
    "morning_pulse",
];

#[derive(Debug, Clone, PartialEq)]
pub struct PatternStatementSlots {
    pub domain: String,
    pub n_right: u32,
    pub n_wrong: u32,
    /// Optional one-word direction tag e.g. "over-confident" / "late".
    pub direction: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct NudgeSlots {
    pub domain: String,
    pub conviction: f64,
    pub n_recent_misses: u32,
    pub n_recent_total: u32,
    pub hush_pattern: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ForecastBlurbSlots {
    pub domain: String,
    pub conviction: f64,
    pub bucket_brier: f64,
    pub overall_brier: f64,
    pub bucket_n: u32,
}

#[derive(Debug, Clone, PartialEq)]
pub struct DashboardCaptionSlots {
    /// e.g. "Brier trend" or "Per-domain accuracy".
    pub surface: String,
    /// Single short fact for the chart caption.
    pub fact: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MorningPulseTrend {
    Improving,
    Declining,
    Stable,
}

#[derive(Debug, Clone, PartialEq)]
pub struct MorningPulseSlots {
    pub brier: f64,
    pub trend: MorningPulseTrend,
    pub top_pattern: String,
}

/// Pattern statement template — what `calibration_profile` writes when the
/// voice gate fails on an LLM narrative.
pub fn pattern_statement_template(s: &PatternStatementSlots) -> String {
    let total = s.n_right + s.n_wrong;
    if total == 0 {
        return format!(
            "Not enough resolved {} calls yet to spot a pattern.",
            s.domain
        );
    }
    let direction = match &s.direction {
        Some(d) => d.clone(),
        None => {
            if s.n_wrong > s.n_right {
                "mixed"
            } else {
                "mostly right"
            }
            .to_string()
        }
    };
    format!(
        "Your {} calls have a {} record — {} of {} held up.",
        s.domain, direction, s.n_right, total
    )
}

/// Nudge template — stderr line on sync after a take is committed.
pub fn nudge_template(s: &NudgeSlots) -> String {
    format!(
        "[zbrain] You just committed a {} take at conviction {:.2}. \
         Recent record on similar calls: {} of {} missed. \
         Hush this pattern for 14 days: zbrain takes nudge --hush {}",
        s.domain, s.conviction, s.n_recent_misses, s.n_recent_total, s.hush_pattern
    )
}

/// Inline forecast on a new take (queue + takes show).
pub fn forecast_blurb_template(s: &ForecastBlurbSlots) -> String {
    if s.bucket_n < 5 {
        return format!(
            "Forecast unavailable: only {} resolved {} takes at this conviction yet.",
            s.bucket_n, s.domain
        );
    }
    let note = if s.bucket_brier > s.overall_brier {
        "worse than your average"
    } else {
        "on par with your average"
    };
    format!(
        "Predicted Brier in {} at conviction {:.2}: {:.2} ({}, n={}).",
        s.domain, s.conviction, s.bucket_brier, note, s.bucket_n
    )
}

/// Dashboard chart caption.
pub fn dashboard_caption_template(s: &DashboardCaptionSlots) -> String {
    format!("{}: {}", s.surface, s.fact)
}

/// Recall morning pulse Brier+pattern line.
pub fn morning_pulse_template(s: &MorningPulseSlots) -> String {
    let trend_word = match s.trend {
        MorningPulseTrend::Improving => "improving",
        MorningPulseTrend::Declining => "declining",
        MorningPulseTrend::Stable => "stable",
    };
    let tail = if !s.top_pattern.is_empty() {
        format!("Top pattern: {}.", s.top_pattern)
    } else {
        String::new()
    };
    format!("Brier {:.2} ({}). {}", s.brier, trend_word, tail)
}

// ── recall calibration footer (recall-footer.ts) ──────────────────────────

/// The three fields `build_recall_calibration_footer` actually reads from a
/// calibration profile. Kept separate from the DB-layer `CalibrationProfileRow`
/// (which lacks `total_resolved`) so this pure formatter has no engine
/// dependency; the Phase-2 caller constructs it from the DB row + a count.
#[derive(Debug, Clone, PartialEq)]
pub struct RecallFooterProfile {
    pub total_resolved: u32,
    pub brier: Option<f64>,
    pub pattern_statements: Vec<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct AbandonedThreadSummary {
    pub claim: String,
    pub months_silent: u32,
}

#[derive(Debug, Clone, PartialEq)]
pub struct RecallFooterOpts {
    pub profile: Option<RecallFooterProfile>,
    pub abandoned_threads: Vec<AbandonedThreadSummary>,
    /// Width hint for column alignment on threads. Pass 0 to use the default 50.
    pub thread_column_width: usize,
}

/// Pure formatter for the `zbrain recall` morning-pulse calibration block.
/// Cold-brain branch returns an empty string when no profile or insufficient
/// resolved takes.
pub fn build_recall_calibration_footer(opts: &RecallFooterOpts) -> String {
    let profile = match &opts.profile {
        Some(p) => p,
        None => return String::new(),
    };
    if profile.total_resolved < 5 {
        return String::new();
    }

    let mut lines: Vec<String> = Vec::new();
    lines.push("Calibration this quarter:".to_string());

    if let Some(brier) = profile.brier {
        lines.push(format!("  Brier {:.2} {}", brier, trend_note(brier)));
    }
    for p in profile.pattern_statements.iter().take(4) {
        lines.push(format!("  {}", p));
    }

    if !opts.abandoned_threads.is_empty() {
        lines.push(String::new());
        lines.push("Threads you opened and never came back to:".to_string());
        let col_width = if opts.thread_column_width == 0 {
            50
        } else {
            opts.thread_column_width
        };
        for t in opts.abandoned_threads.iter().take(5) {
            let claim = if t.claim.chars().count() > col_width {
                let mut truncated: String = t.claim.chars().take(col_width - 1).collect();
                truncated.push('…');
                truncated
            } else {
                t.claim.clone()
            };
            let padded = format!("{:<width$}", claim, width = col_width);
            lines.push(format!("  · {} ({} months silent)", padded, t.months_silent));
        }
    }

    lines.join("\n")
}

fn trend_note(brier: f64) -> &'static str {
    // Map Brier to a conversational anchor. No history yet so we describe the
    // absolute value rather than trend.
    if brier <= 0.1 {
        "(strong calibration)."
    } else if brier <= 0.2 {
        "(solid)."
    } else if brier <= 0.25 {
        "(near baseline)."
    } else {
        "(worse than always-50% baseline — review your high-conviction calls)."
    }
}

// ── voice-gate judge parsing (voice-gate.ts: parseJudgeOutput + DEFAULT_RUBRICS) ──

/// The five calibration UX surfaces that share one voice gate.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VoiceGateMode {
    PatternStatement,
    Nudge,
    ForecastBlurb,
    DashboardCaption,
    MorningPulse,
}

impl VoiceGateMode {
    pub fn as_str(self) -> &'static str {
        match self {
            VoiceGateMode::PatternStatement => "pattern_statement",
            VoiceGateMode::Nudge => "nudge",
            VoiceGateMode::ForecastBlurb => "forecast_blurb",
            VoiceGateMode::DashboardCaption => "dashboard_caption",
            VoiceGateMode::MorningPulse => "morning_pulse",
        }
    }
}

/// The Haiku judge's verdict on a candidate string.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VoiceGateVerdict {
    Conversational,
    Academic,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VoiceGateJudgeVerdict {
    pub verdict: VoiceGateVerdict,
    pub reason: String,
}

/// Per-mode rubric text the gate ships to its Haiku judge. Tuning the rubric
/// is the V1 lever; tuning the gate code is a later concern. Mirrors
/// `DEFAULT_RUBRICS` in voice-gate.ts verbatim.
pub fn default_rubric(mode: VoiceGateMode) -> &'static str {
    match mode {
        VoiceGateMode::PatternStatement => r#"Voice for a calibration pattern statement:
- Sounds like a smart friend recapping your record, not a doctor or HR.
- Uses second person ("your", "you").
- Names numbers grounded in actual takes ("2 of 3 missed"), not abstract
  metrics like "Brier 0.31" or "conviction-bucket 0.8-0.9".
- No preachy/clinical phrasing ("our analysis indicates", "the data shows").
- Short — under 25 words.
- NEVER mentions internal field names like 'Brier' or 'conviction-bucket'
  without translation."#,
        VoiceGateMode::Nudge => r#"Voice for a real-time nudge fired during sync after a take is committed:
- Sounds like a friend tapping you on the shoulder, not an alert system.
- Second person, contractions allowed, casual.
- Grounded in 1-2 concrete past data points the user can verify.
- Always closes with a concrete next step (a CLI command or a question).
- Under 30 words.
- NEVER preachy. NEVER "we recommend." NEVER "according to your data"."#,
        VoiceGateMode::ForecastBlurb => r#"Voice for an inline forecast blurb on a new take:
- One short factual line, ~12-20 words.
- Names the past data in concrete terms ("2 of 3 missed" beats "Brier 0.31").
- Acknowledges uncertainty when n is small.
- No "predicted Brier" jargon without translation.
- NEVER condescending."#,
        VoiceGateMode::DashboardCaption => r#"Voice for a chart caption on the admin dashboard:
- Single short sentence per caption.
- Names ONE concrete fact.
- No marketing copy, no "powerful insights", no "leverage".
- Plain language, no jargon."#,
        VoiceGateMode::MorningPulse => r#"Voice for a daily morning-pulse line:
- One sentence, sounds like a friend giving you a quick status check.
- Names the trend in plain words ("improving" beats "trending positive").
- Mentions ONE pattern when relevant; skip when no clear pattern.
- Under 25 words.
- NEVER clinical, NEVER preachy, NEVER hedged corporate language."#,
    }
}

/// Strip a leading ``` fence (optionally tagged `json`) from a judge string.
/// Mirrors the TS regex `/^```(?:json)?\s*\n?([\s\S]*?)\n?```$/`.
fn strip_judge_fence(raw: &str) -> &str {
    let t = raw.trim();
    if t.len() >= 6 && t.starts_with("```") && t.ends_with("```") {
        let inner = &t[3..t.len() - 3];
        let inner = inner.strip_prefix("json").unwrap_or(inner);
        return inner.trim();
    }
    t
}

/// Parse the Haiku judge's JSON output. Robust to fence wrapping + leading
/// prose. On unrecoverable parse failure, treat as 'academic' with
/// reason='parse_failed' so the gate falls back to the template rather than
/// silently passing bad voice. Pure mirror of `parseJudgeOutput`.
pub fn parse_judge_output(raw: &str) -> VoiceGateJudgeVerdict {
    if raw.trim().is_empty() {
        return VoiceGateJudgeVerdict {
            verdict: VoiceGateVerdict::Academic,
            reason: "empty_judge_output".to_string(),
        };
    }
    let text = strip_judge_fence(raw).to_string();
    let first_obj = match text.find('{') {
        Some(i) => i,
        None => {
            return VoiceGateJudgeVerdict {
                verdict: VoiceGateVerdict::Academic,
                reason: "parse_failed".to_string(),
            }
        }
    };
    let parsed: serde_json::Value = match serde_json::from_str(&text[first_obj..]) {
        Ok(v) => v,
        Err(_) => {
            return VoiceGateJudgeVerdict {
                verdict: VoiceGateVerdict::Academic,
                reason: "parse_failed".to_string(),
            }
        }
    };
    if !parsed.is_object() {
        return VoiceGateJudgeVerdict {
            verdict: VoiceGateVerdict::Academic,
            reason: "parse_failed".to_string(),
        };
    }
    let verdict = match parsed.get("verdict").and_then(|v| v.as_str()) {
        Some("conversational") => VoiceGateVerdict::Conversational,
        _ => VoiceGateVerdict::Academic,
    };
    let reason = parsed
        .get("reason")
        .and_then(|v| v.as_str())
        .map(|s| s.chars().take(80).collect::<String>())
        .unwrap_or_else(|| "no_reason".to_string());
    VoiceGateJudgeVerdict { verdict, reason }
}

// ── Brier-trend forecast pure math (take-forecast.ts: resolveDomainPrefix + computeForecast) ──

/// Minimum bucket size before we report a forecast. Below this → None.
pub const MIN_BUCKET_N: u32 = 5;

/// Map a free-form domain hint to a `domainPrefix` the scorecard query
/// understands. Slug-prefix-looking values are kept as-is; free-form words
/// fall back to `None` (overall scorecard) for now. Pure mirror of
/// `resolveDomainPrefix`.
pub fn resolve_domain_prefix(domain: Option<&str>) -> Option<String> {
    let d = domain?.to_lowercase();
    let d = d.trim();
    if d.is_empty() {
        return None;
    }
    if d.ends_with('/') {
        return Some(d.to_string());
    }
    if d.starts_with("wiki/") || d.starts_with("companies/") || d.starts_with("people/") {
        return Some(d.to_string());
    }
    None
}

/// Pure output of `computeForecast`. The `conviction` dimension is carried by
/// the (Phase 2) engine wrapper `forecastForTake`, not used in the pure math.
#[derive(Debug, Clone, PartialEq)]
pub struct TakeForecast {
    pub predicted_brier: Option<f64>,
    pub bucket_n: u32,
    /// Holder's overall Brier for comparison. `None` when the holder has no
    /// resolved correct/incorrect bets (canonical TS `overall_brier: number | null`).
    pub overall_brier: Option<f64>,
    pub bucket_domain: String,
    pub insufficient_data: bool,
}

/// Pure math: given the holder's overall scorecard AND optional bucketed
/// scorecard, compute the forecast struct. Caller fetches scorecards via the
/// engine (Phase 2); this stays engine-free so tests drive it directly.
pub fn compute_forecast(
    overall: &TakesScorecard,
    bucket: Option<&TakesScorecard>,
    domain: Option<&str>,
) -> TakeForecast {
    let overall_brier = overall.brier;
    let bucket = bucket.unwrap_or(overall);
    let bucket_domain = domain.unwrap_or("overall").to_string();
    let bucket_n = bucket.resolved.max(0) as u32;
    let insufficient_data = bucket_n < MIN_BUCKET_N;
    let predicted_brier = if insufficient_data {
        None
    } else {
        bucket.brier
    };
    TakeForecast {
        predicted_brier,
        bucket_n,
        overall_brier,
        bucket_domain,
        insufficient_data,
    }
}

// ── engine-backed forecast wrappers (take-forecast.ts: forecastForTake + batchForecast) ──

/// Input to `forecast_for_take` / `batch_forecast`. Mirrors `TakeForecastInput`.
pub struct TakeForecastInput {
    /// Take's holder, e.g. 'garry' or 'people/charlie-example'.
    pub holder: String,
    /// Optional domain hint (e.g. 'macro', 'companies/foo'). When present and
    /// it resolves to a slug prefix, the forecast scopes to that domain's bucket.
    pub domain: Option<String>,
    /// The conviction-weight of the new take in [0,1].
    pub conviction: f64,
}

/// Zero scorecard used for the fail-open fallback when an engine read errors.
/// Delegates to `aggregate_scorecard` over no rows so it can never drift from
/// the canonical degradation semantics.
fn zero_scorecard() -> TakesScorecard {
    crate::calibration_queries::aggregate_scorecard(std::iter::empty())
}

/// Engine-backed forecast for a single take. Mirrors `forecastForTake`:
/// resolves the domain hint, fetches the overall + (optional) bucketed
/// scorecard via `CalibrationQueries`, then delegates the math to the pure
/// `compute_forecast`. Fail-open — an engine error falls back to a zero
/// scorecard so the surfaced blurb never hard-fails on a transient read error.
pub async fn forecast_for_take<C: CalibrationQueries + ?Sized>(
    engine: &C,
    input: &TakeForecastInput,
) -> TakeForecast {
    let overall = engine
        .get_scorecard(&crate::calibration_queries::ScorecardQuery::for_holder(
            &input.holder,
        ))
        .await
        .unwrap_or_else(|_| zero_scorecard());
    let bucket = match &input.domain {
        Some(domain) => match resolve_domain_prefix(Some(domain)) {
            Some(prefix) => Some(
                engine
                    .get_scorecard(&crate::calibration_queries::ScorecardQuery {
                        holder: Some(&input.holder),
                        domain_prefix: Some(&prefix),
                        ..Default::default()
                    })
                    .await
                    .unwrap_or_else(|_| zero_scorecard()),
            ),
            None => None,
        },
        None => None,
    };
    compute_forecast(&overall, bucket.as_ref(), input.domain.as_deref())
}

/// Batched forecast over many takes. Mirrors `batchForecast` — one engine
/// round-trip per input (memoization across `(holder, domain)` is a later
/// optimization; the output is identical either way). See `forecast_for_take`
/// for per-take semantics.
pub async fn batch_forecast<C: CalibrationQueries + ?Sized>(
    engine: &C,
    inputs: &[TakeForecastInput],
) -> Vec<TakeForecast> {
    let mut out = Vec::with_capacity(inputs.len());
    for input in inputs {
        out.push(forecast_for_take(engine, input).await);
    }
    out
}

// ── cross-brain calibration query semantics (cross-brain.ts: canReadMountsForCtx + attributionSuffix) ──

/// Combined trait for a mountable engine: BrainEngine + CalibrationQueries + Send + Sync.
pub trait MountableBrainEngine: BrainEngine + CalibrationQueries + Send + Sync + 'static {}
impl<T: BrainEngine + CalibrationQueries + Send + Sync + 'static> MountableBrainEngine for T {}

/// Resolver that yields mounted brain engines for cross-brain fallback lookup.
///
/// Matches TS `mountResolver` contract: the resolver returns an ordered list of
/// (brain_id, engine) pairs to query in priority order (first match wins).
#[async_trait]
pub trait MountResolver: Send + Sync {
    async fn resolve_mounts(&self) -> crate::error::Result<Vec<(String, Box<dyn MountableBrainEngine>)>>;
}

/// Result of cross-brain query: the calibration profile plus attribution.
#[derive(Debug, Clone, PartialEq)]
pub struct CrossBrainProfileResult {
    pub profile: CalibrationProfileRow,
    pub source_brain_id: String,
    pub from_mount: bool,
}

/// Operation-context gate for whether mounted brains may be read.
/// Local CLI / MCP read-scope → true; subagent loop (the OAuth-token leak
/// surface) → only when `allowed_slug_prefixes` is non-empty. Pure mirror of
/// `canReadMountsForCtx`.
pub fn can_read_mounts_for_ctx(
    remote: bool,
    via_subagent: Option<bool>,
    allowed_slug_prefixes: Option<&[String]>,
) -> bool {
    if !remote {
        return true;
    }
    if via_subagent == Some(true) {
        return allowed_slug_prefixes
            .map(|p| !p.is_empty())
            .unwrap_or(false);
    }
    true
}

/// Render the attribution suffix consumers MUST surface so the user sees
/// which brain answered. Local hits need no suffix. Pure mirror of
/// `attributionSuffix`.
pub fn attribution_suffix(result: &CrossBrainProfileResult) -> String {
    if !result.from_mount {
        return String::new();
    }
    format!(" (from mounted brain: {})", result.source_brain_id)
}

/// Query calibration profile across local + mounted brains per D18 4-rule contract.
///
/// 1. Local-first: if a profile exists locally, return it immediately (no mount query).
/// 2. Mount-fallback: only when local has no profile AND `can_read_mounts` is true.
/// 3. Query mounts in priority order; first published profile wins.
/// 4. Returns None when no reachable profile found.
pub async fn query_across_brains(
    local_engine: &dyn CalibrationQueries,
    local_brain_id: String,
    holder: &str,
    can_read_mounts: bool,
    mount_resolver: &dyn MountResolver,
    source_id: Option<&str>,
    source_ids: Option<&[String]>,
) -> crate::error::Result<Option<CrossBrainProfileResult>> {
    // 1. Local-first: check local engine first
    let local_profile = local_engine.get_latest_profile(holder, source_id, source_ids).await?;
    if let Some(profile) = local_profile {
        return Ok(Some(CrossBrainProfileResult {
            profile,
            source_brain_id: local_brain_id,
            from_mount: false,
        }));
    }

    // 4. If can't read mounts, stop here
    if !can_read_mounts {
        return Ok(None);
    }

    // 2. Mount fallback: iterate priority order
    let mounts = mount_resolver.resolve_mounts().await?;
    for (brain_id, engine) in mounts {
        let mount_profile = engine.get_latest_profile(holder, source_id, source_ids).await?;
        if let Some(profile) = mount_profile {
            if !profile.published {
                continue;
            }
            // Found first matching published profile → return
            return Ok(Some(CrossBrainProfileResult {
                profile,
                source_brain_id: brain_id,
                from_mount: true,
            }));
        }
    }

    // No profile found anywhere
    Ok(None)
}

// ── domain scorecard aggregation (aggregateDomainScorecards.ts) ──

/// Aggregator algorithm kind (closed enum).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum AggregatorKind {
    /// Standard Brier score over resolved binary takes.
    ScalarBrier,
    /// Brier weighted by take conviction (ABS(weight - 0.5) * 2).
    WeightedBrier,
    /// Simple accuracy ratio (correct / resolved) for binary without probability semantics.
    CountBased,
    /// Descriptive rollup (tier counts) for domains without binary outcomes.
    ClusterSummary,
}

impl std::fmt::Display for AggregatorKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            AggregatorKind::ScalarBrier => write!(f, "scalar_brier"),
            AggregatorKind::WeightedBrier => write!(f, "weighted_brier"),
            AggregatorKind::CountBased => write!(f, "count_based"),
            AggregatorKind::ClusterSummary => write!(f, "cluster_summary"),
        }
    }
}

impl TryFrom<&str> for AggregatorKind {
    type Error = crate::error::Error;

    fn try_from(value: &str) -> Result<Self, Self::Error> {
        match value {
            "scalar_brier" => Ok(AggregatorKind::ScalarBrier),
            "weighted_brier" => Ok(AggregatorKind::WeightedBrier),
            "count_based" => Ok(AggregatorKind::CountBased),
            "cluster_summary" => Ok(AggregatorKind::ClusterSummary),
            _ => Err(crate::error::StructuredError::new(
                "InvalidAggregatorKind",
                "invalid_aggregator_kind",
                &format!("unknown aggregator kind '{}', expected one of: scalar_brier, weighted_brier, count_based, cluster_summary", value),
            ).into()),
        }
    }
}

/// A single calibration domain declared in the pack manifest.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CalibrationDomain {
    /// Domain name (lowercase snake_case).
    pub name: String,
    /// Aggregation algorithm to use.
    pub aggregator: AggregatorKind,
    /// Page types whose takes feed this domain.
    pub page_types: Vec<String>,
}

/// Per-domain aggregated scorecard.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DomainScorecard {
    /// Number of resolved takes contributing to this scorecard.
    pub n: i32,
    /// Brier score (lower = better), null when n = 0 or aggregator doesn't compute it.
    pub brier: Option<f64>,
    /// Accuracy fraction in [0, 1], null when n = 0 or aggregator doesn't compute it.
    pub accuracy: Option<f64>,
    /// Aggregator algorithm used (debugging).
    pub aggregator: AggregatorKind,
    /// Page types filtered for this domain.
    pub page_types: Vec<String>,
    /// Aggregator-specific extra data (tier_counts for cluster_summary).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub extras: Option<serde_json::Value>,
}

/// Map from domain name to scorecard.
pub type DomainScorecards = HashMap<String, DomainScorecard>;

/// Result row for scalar_brier / weighted_brier / count_brier queries.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
struct AggregationRow {
    n: i32,
    brier: Option<f64>,
    accuracy: Option<f64>,
}

/// Result row for cluster_summary query.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
struct ClusterSummaryRow {
    n: i32,
    t1: i32,
    t2: i32,
    t3: i32,
    t4: i32,
}

/// Aggregate every declared calibration domain for the given holder + source.
///
/// - Fail-soft per domain: any error returns {n: 0, brier: null, accuracy: null, extras: {error: msg}}
/// - Empty domains (n=0) are still included so consumers can distinguish
///   "declared but no data" from "not declared".
pub async fn aggregate_domain_scorecards(
    engine: &dyn BrainEngine,
    holder: &str,
    domains: &[CalibrationDomain],
    source_id: &str,
) -> crate::error::Result<DomainScorecards> {
    let mut out = DomainScorecards::new();
    for domain in domains {
        match aggregate_one_domain(engine, holder, domain, source_id).await {
            Ok(scorecard) => {
                out.insert(domain.name.clone(), scorecard);
            }
            Err(err) => {
                out.insert(domain.name.clone(), DomainScorecard {
                    n: 0,
                    brier: None,
                    accuracy: None,
                    aggregator: domain.aggregator.clone(),
                    page_types: domain.page_types.clone(),
                    extras: Some(serde_json::json!({
                        "error": err.to_string()
                    })),
                });
            }
        }
    }
    Ok(out)
}

async fn aggregate_one_domain(
    engine: &dyn BrainEngine,
    holder: &str,
    domain: &CalibrationDomain,
    source_id: &str,
) -> crate::error::Result<DomainScorecard> {
    match domain.aggregator {
        AggregatorKind::ScalarBrier => aggregate_scalar_brier(engine, holder, domain, source_id).await,
        AggregatorKind::WeightedBrier => aggregate_weighted_brier(engine, holder, domain, source_id).await,
        AggregatorKind::CountBased => aggregate_count_based(engine, holder, domain, source_id).await,
        AggregatorKind::ClusterSummary => aggregate_cluster_summary(engine, domain, source_id).await,
    }
}

/// Standard Brier score: mean((p - outcome)^2).
async fn aggregate_scalar_brier(
    engine: &dyn BrainEngine,
    holder: &str,
    domain: &CalibrationDomain,
    source_id: &str,
) -> crate::error::Result<DomainScorecard> {
    let sql = r#"
        SELECT
            COUNT(*)::int AS n,
            AVG(POWER(t.weight - (t.resolved_outcome::int)::real, 2))::real AS brier,
            (SUM(CASE WHEN (t.weight >= 0.5) = t.resolved_outcome THEN 1 ELSE 0 END)::real
                / NULLIF(COUNT(*), 0))::real AS accuracy
         FROM takes t
         JOIN take_domain_assignments a ON a.take_id = t.id
         JOIN pages p ON p.id = t.page_id
         WHERE a.domain = $1
           AND t.holder = $2
           AND t.active = TRUE
           AND t.resolved_outcome IS NOT NULL
           AND p.type = ANY($3::text[])
           AND p.source_id = $4
    "#;
    let params: &[&(dyn erased_serde::Serialize + Sync)] = &[
        &domain.name,
        &holder,
        &domain.page_types,
        &source_id,
    ];
    let json_rows = engine.execute_raw(sql, params).await?;
    let mut rows = Vec::new();
    for json in json_rows {
        let row: AggregationRow = serde_json::from_value(json)
            .map_err(|e| crate::error::Error::engine(format!("deserialize aggregation row: {}", e)))?;
        rows.push(row);
    }
    let row = rows.first().unwrap_or(&AggregationRow { n: 0, brier: None, accuracy: None });
    Ok(DomainScorecard {
        n: row.n,
        brier: if row.n > 0 { row.brier } else { None },
        accuracy: if row.n > 0 { row.accuracy } else { None },
        aggregator: AggregatorKind::ScalarBrier,
        page_types: domain.page_types.clone(),
        extras: None,
    })
}

/// Weighted Brier: each prediction weighted by conviction (ABS(weight - 0.5) * 2).
async fn aggregate_weighted_brier(
    engine: &dyn BrainEngine,
    holder: &str,
    domain: &CalibrationDomain,
    source_id: &str,
) -> crate::error::Result<DomainScorecard> {
    let sql = r#"
        WITH scored AS (
            SELECT
                POWER(t.weight - (t.resolved_outcome::int)::real, 2) AS sq_err,
                ABS(t.weight - 0.5) * 2.0 AS conviction,
                (t.weight >= 0.5) = t.resolved_outcome AS hit
            FROM takes t
            JOIN take_domain_assignments a ON a.take_id = t.id
            JOIN pages p ON p.id = t.page_id
            WHERE a.domain = $1
              AND t.holder = $2
              AND t.active = TRUE
              AND t.resolved_outcome IS NOT NULL
              AND p.type = ANY($3::text[])
              AND p.source_id = $4
        )
        SELECT
            COUNT(*)::int AS n,
            (SUM(sq_err * conviction) / NULLIF(SUM(conviction), 0))::real AS brier,
            (SUM(CASE WHEN hit THEN 1 ELSE 0 END)::real / NULLIF(COUNT(*), 0))::real AS accuracy
        FROM scored
    "#;
    let params: &[&(dyn erased_serde::Serialize + Sync)] = &[
        &domain.name,
        &holder,
        &domain.page_types,
        &source_id,
    ];
    let json_rows = engine.execute_raw(sql, params).await?;
    let mut rows = Vec::new();
    for json in json_rows {
        let row: AggregationRow = serde_json::from_value(json)
            .map_err(|e| crate::error::Error::engine(format!("deserialize aggregation row: {}", e)))?;
        rows.push(row);
    }
    let row = rows.first().unwrap_or(&AggregationRow { n: 0, brier: None, accuracy: None });
    Ok(DomainScorecard {
        n: row.n,
        brier: if row.n > 0 { row.brier } else { None },
        accuracy: if row.n > 0 { row.accuracy } else { None },
        aggregator: AggregatorKind::WeightedBrier,
        page_types: domain.page_types.clone(),
        extras: None,
    })
}

/// Simple accuracy: (correct / resolved), no Brier.
async fn aggregate_count_based(
    engine: &dyn BrainEngine,
    holder: &str,
    domain: &CalibrationDomain,
    source_id: &str,
) -> crate::error::Result<DomainScorecard> {
    let sql = r#"
        SELECT
            COUNT(*)::int AS n,
            (SUM(CASE WHEN (t.weight >= 0.5) = t.resolved_outcome THEN 1 ELSE 0 END)::real
                / NULLIF(COUNT(*), 0))::real AS accuracy
         FROM takes t
         JOIN take_domain_assignments a ON a.take_id = t.id
         JOIN pages p ON p.id = t.page_id
         WHERE a.domain = $1
           AND t.holder = $2
           AND t.active = TRUE
           AND t.resolved_outcome IS NOT NULL
           AND p.type = ANY($3::text[])
           AND p.source_id = $4
    "#;
    let params: &[&(dyn erased_serde::Serialize + Sync)] = &[
        &domain.name,
        &holder,
        &domain.page_types,
        &source_id,
    ];
    let json_rows = engine.execute_raw(sql, params).await?;
    let mut rows = Vec::new();
    for json in json_rows {
        let row: AggregationRow = serde_json::from_value(json)
            .map_err(|e| crate::error::Error::engine(format!("deserialize aggregation row: {}", e)))?;
        rows.push(row);
    }
    let row = rows.first().unwrap_or(&AggregationRow { n: 0, brier: None, accuracy: None });
    Ok(DomainScorecard {
        n: row.n,
        brier: None,
        accuracy: if row.n > 0 { row.accuracy } else { None },
        aggregator: AggregatorKind::CountBased,
        page_types: domain.page_types.clone(),
        extras: None,
    })
}

/// Cluster summary: descriptive rollup (tier counts) for domains without binary outcomes.
async fn aggregate_cluster_summary(
    engine: &dyn BrainEngine,
    domain: &CalibrationDomain,
    source_id: &str,
) -> crate::error::Result<DomainScorecard> {
    let sql = r#"
        SELECT
            COUNT(*)::int AS n,
            SUM(CASE WHEN frontmatter->>'tier' = 'T1' OR frontmatter->>'tier' = '1' THEN 1 ELSE 0 END)::int AS t1,
            SUM(CASE WHEN frontmatter->>'tier' = 'T2' OR frontmatter->>'tier' = '2' THEN 1 ELSE 0 END)::int AS t2,
            SUM(CASE WHEN frontmatter->>'tier' = 'T3' OR frontmatter->>'tier' = '3' THEN 1 ELSE 0 END)::int AS t3,
            SUM(CASE WHEN frontmatter->>'tier' = 'T4' OR frontmatter->>'tier' = '4' THEN 1 ELSE 0 END)::int AS t4
         FROM pages
         WHERE type = ANY($1::text[])
           AND source_id = $2
           AND deleted_at IS NULL
    "#;
    let params: &[&(dyn erased_serde::Serialize + Sync)] = &[
        &domain.page_types,
        &source_id,
    ];
    let json_rows = engine.execute_raw(sql, params).await?;
    let mut rows = Vec::new();
    for json in json_rows {
        let row: ClusterSummaryRow = serde_json::from_value(json)
            .map_err(|e| crate::error::Error::engine(format!("deserialize cluster row: {}", e)))?;
        rows.push(row);
    }
    let row = rows.first().unwrap_or(&ClusterSummaryRow { n: 0, t1: 0, t2: 0, t3: 0, t4: 0 });
    Ok(DomainScorecard {
        n: row.n,
        brier: None,
        accuracy: None,
        aggregator: AggregatorKind::ClusterSummary,
        page_types: domain.page_types.clone(),
        extras: Some(serde_json::json!({
            "tier_counts": {
                "T1": row.t1,
                "T2": row.t2,
                "T3": row.t3,
                "T4": row.t4,
            }
        })),
    })
}

// ── real-time pattern surfacing on take commit (nudge.ts: takeDomainHint + evaluateNudgeRule) ──

pub const NUDGE_COOLDOWN_DAYS: u32 = 14;
pub const NUDGE_CONVICTION_THRESHOLD: f64 = 0.7;

/// The minimal take projection `evaluateNudgeRule` / `takeDomainHint` need.
/// The full `Take` type has no `page_slug` field on the Rust side, so this
/// decoupled struct keeps the pure port engine-free.
#[derive(Debug, Clone, PartialEq)]
pub struct NudgeTake {
    pub page_slug: String,
    pub holder: String,
    pub weight: f64,
}

/// Why a nudge rule did / didn't match. `CooldownActive` is produced by the
/// engine-backed cooldown probe (Phase 2), not by this pure rule.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NudgeReason {
    NoProfile,
    BelowConvictionThreshold,
    NoMatchingBiasTag,
    CooldownActive,
    WrongHolder,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NudgeRuleResult {
    pub matched: bool,
    pub reason: Option<NudgeReason>,
    pub matched_tag: Option<String>,
}

/// Map a take's page slug to a domain hint that joins against bias tags.
/// Same heuristic as eval-contradictions/calibration-join.ts. Pure mirror of
/// `takeDomainHint`.
pub fn take_domain_hint(take: &NudgeTake) -> String {
    let slug = take.page_slug.to_lowercase();
    if slug.contains("/companies/") || slug.starts_with("companies/") {
        return "hiring".to_string();
    }
    if slug.contains("/people/") || slug.starts_with("people/") {
        return "founder-behavior".to_string();
    }
    if slug.contains("/deals/") || slug.starts_with("deals/") {
        return "market-timing".to_string();
    }
    if slug.contains("macro") {
        return "macro".to_string();
    }
    if slug.contains("geography") {
        return "geography".to_string();
    }
    if slug.contains("tactics") {
        return "tactics".to_string();
    }
    if slug.contains("/ai/") || slug.contains("-ai-") {
        return "ai".to_string();
    }
    String::new()
}

/// Pure decision: should a take fire a nudge given the active profile?
/// Cooldown is a separate engine-backed probe (Phase 2). Pure mirror of
/// `evaluateNudgeRule`.
pub fn evaluate_nudge_rule(
    take: &NudgeTake,
    profile: Option<&CalibrationProfileRow>,
) -> NudgeRuleResult {
    let profile = match profile {
        Some(p) => p,
        None => {
            return NudgeRuleResult {
                matched: false,
                reason: Some(NudgeReason::NoProfile),
                matched_tag: None,
            }
        }
    };
    if take.holder != profile.holder {
        return NudgeRuleResult {
            matched: false,
            reason: Some(NudgeReason::WrongHolder),
            matched_tag: None,
        };
    }
    if take.weight <= NUDGE_CONVICTION_THRESHOLD {
        return NudgeRuleResult {
            matched: false,
            reason: Some(NudgeReason::BelowConvictionThreshold),
            matched_tag: None,
        };
    }
    let hint = take_domain_hint(take);
    if hint.is_empty() {
        return NudgeRuleResult {
            matched: false,
            reason: Some(NudgeReason::NoMatchingBiasTag),
            matched_tag: None,
        };
    }
    let tags = profile.active_bias_tags.as_slice();
    for tag in tags {
        if tag.to_lowercase().contains(&hint) {
            return NudgeRuleResult {
                matched: true,
                reason: None,
                matched_tag: Some(tag.clone()),
            };
        }
    }
    NudgeRuleResult {
        matched: false,
        reason: Some(NudgeReason::NoMatchingBiasTag),
        matched_tag: None,
    }
}

// ── gstack-learnings coupling (gstack-coupling.ts: buildLearningEntry + defaultGstackWriter) ──

/// Namespace prefix. A later `--undo-wave` prunes these via
/// `gstack-learnings-prune`.
pub const GSTACK_LEARNING_NAMESPACE: &str = "zbrain:calibration:v0.36.1.0:";

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GstackQuality {
    Incorrect,
    Partial,
}

impl GstackQuality {
    fn as_str(&self) -> &'static str {
        match self {
            GstackQuality::Incorrect => "incorrect",
            GstackQuality::Partial => "partial",
        }
    }
}

/// The resolution event that seeds a gstack learning entry.
#[derive(Debug, Clone, PartialEq)]
pub struct IncorrectResolutionEvent {
    pub take_id: i64,
    pub page_slug: String,
    pub row_num: i64,
    pub holder: String,
    pub claim: String,
    pub quality: GstackQuality,
    pub weight: f64,
    pub active_bias_tags: Option<Vec<String>>,
    pub confidence: Option<f64>,
    pub reasoning: Option<String>,
}

/// Wire shape sent to gstack-learnings-log. Derives Serialize so
/// `default_gstack_writer` can JSON-encode it for the binary's argv.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GstackLearningEntry {
    pub skill: String,
    #[serde(rename = "type")]
    pub r#type: String,
    pub key: String,
    pub insight: String,
    pub confidence: f64,
    pub source: String,
    pub files: Option<Vec<String>>,
}

/// Build the learning entry from a resolution event. Pure mirror of
/// `buildLearningEntry`.
pub fn build_learning_entry(event: &IncorrectResolutionEvent) -> GstackLearningEntry {
    let truncated_claim = if event.claim.chars().count() > 200 {
        let mut c: String = event.claim.chars().take(200).collect();
        c.push('…');
        c
    } else {
        event.claim.clone()
    };
    let tag_suffix = match &event.active_bias_tags {
        Some(t) if !t.is_empty() => format!(":{}", t[0]),
        _ => String::new(),
    };
    let insight_lead = match event.quality {
        GstackQuality::Incorrect => "was wrong",
        GstackQuality::Partial => "was partially wrong",
    };
    let reasoning_tail = match &event.reasoning {
        Some(r) => {
            let capped: String = r.chars().take(200).collect();
            format!(" Reasoning: {}", capped)
        }
        None => String::new(),
    };
    let tag_tail = match &event.active_bias_tags {
        Some(t) if !t.is_empty() => format!(". Pattern: {}.", t.join(", ")),
        _ => String::new(),
    };
    GstackLearningEntry {
        skill: "zbrain-calibration".to_string(),
        r#type: "observation".to_string(),
        key: format!("{}take-{}{}", GSTACK_LEARNING_NAMESPACE, event.take_id, tag_suffix),
        insight: format!(
            "{} {} on \"{}\" (conviction {:.2}, graded {}).{}{}",
            event.holder,
            insight_lead,
            truncated_claim,
            event.weight,
            event.quality.as_str(),
            tag_tail,
            reasoning_tail
        ),
        confidence: event.confidence.unwrap_or(0.8),
        source: "observed".to_string(),
        files: Some(vec![event.page_slug.clone()]),
    }
}

/// Error from the production gstack writer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GstackWriteErrorKind {
    BinaryNotFound,
    WriteFailed,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GstackWriteError {
    pub kind: GstackWriteErrorKind,
    pub message: String,
}

/// Production writer: shell out to `gstack-learnings-log` if it's on PATH.
/// This is the I/O boundary of the gstack coupling (NOT a pure function) —
/// the unit-tested pure core is `build_learning_entry`. Mirrors
/// `defaultGstackWriter`. Best-effort: callers decide how to log failures.
pub fn default_gstack_writer(entry: &GstackLearningEntry) -> Result<(), GstackWriteError> {
    let json = serde_json::to_string(entry).map_err(|e| GstackWriteError {
        kind: GstackWriteErrorKind::WriteFailed,
        message: e.to_string(),
    })?;
    match Command::new("gstack-learnings-log").arg(&json).output() {
        Ok(o) if o.status.success() => Ok(()),
        Ok(o) => Err(GstackWriteError {
            kind: GstackWriteErrorKind::WriteFailed,
            message: format!("gstack-learnings-log exited with status {}", o.status),
        }),
        Err(e) if e.kind() == IoErrorKind::NotFound => Err(GstackWriteError {
            kind: GstackWriteErrorKind::BinaryNotFound,
            message: "gstack-learnings-log not on PATH".to_string(),
        }),
        Err(e) => Err(GstackWriteError {
            kind: GstackWriteErrorKind::WriteFailed,
            message: e.to_string(),
        }),
    }
}

// ── A/B harness report formatting (think-ab.ts: formatAbReport) ──

/// Aggregated win/loss breakdown over the last N days. Pure mirror of
/// `AbReportResult`.
#[derive(Debug, Clone, PartialEq)]
pub struct AbReportResult {
    pub total_trials: u64,
    pub baseline_wins: u64,
    pub with_calibration_wins: u64,
    pub ties: u64,
    pub neither: u64,
    pub with_calibration_win_rate: Option<f64>,
    pub net_negative: bool,
    pub decisive_trials: u64,
}

/// Human-format the A/B report. NOTE: the TS original renders the
/// net-negative flag with a `⚠` emoji; this port uses a plain `WARNING:`
/// prefix to follow project conventions (no emojis in source).
pub fn format_ab_report(report: &AbReportResult, days: u64) -> String {
    let mut lines: Vec<String> = Vec::new();
    lines.push(format!("A/B report (last {} days):", days));
    lines.push(format!("  Total trials: {}", report.total_trials));
    if report.total_trials == 0 {
        lines.push("  No data yet. Try: zbrain think --ab \"<question>\"".to_string());
        return lines.join("\n");
    }
    lines.push(format!("  Baseline wins:           {}", report.baseline_wins));
    lines.push(format!("  With-calibration wins:   {}", report.with_calibration_wins));
    lines.push(format!("  Ties:                    {}", report.ties));
    lines.push(format!("  Neither:                 {}", report.neither));
    if let Some(wr) = report.with_calibration_win_rate {
        lines.push(format!(
            "  With-calibration win rate (decisive trials only): {:.1}% (n={})",
            wr * 100.0,
            report.decisive_trials
        ));
    }
    if report.net_negative {
        lines.push(String::new());
        lines.push(
            "WARNING: calibration_net_negative: with-calibration is losing more than half of decisive trials."
                .to_string(),
        );
        lines.push("  Consider tuning the anti-bias prompt rewrite (src/core/think/prompt.ts) or".to_string());
        lines.push("  disabling --with-calibration via config until you tune.".to_string());
    }
    lines.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pattern_statement_zero_total() {
        let s = PatternStatementSlots {
            domain: "market".into(),
            n_right: 0,
            n_wrong: 0,
            direction: None,
        };
        assert_eq!(
            pattern_statement_template(&s),
            "Not enough resolved market calls yet to spot a pattern."
        );
    }

    #[test]
    fn pattern_statement_default_direction() {
        // n_wrong (3) > n_right (2) -> "mixed"
        let s = PatternStatementSlots {
            domain: "market".into(),
            n_right: 2,
            n_wrong: 3,
            direction: None,
        };
        assert_eq!(
            pattern_statement_template(&s),
            "Your market calls have a mixed record — 2 of 5 held up."
        );
        // n_right > n_wrong -> "mostly right"
        let s2 = PatternStatementSlots {
            domain: "market".into(),
            n_right: 4,
            n_wrong: 1,
            direction: None,
        };
        assert_eq!(
            pattern_statement_template(&s2),
            "Your market calls have a mostly right record — 4 of 5 held up."
        );
    }

    #[test]
    fn pattern_statement_explicit_direction() {
        let s = PatternStatementSlots {
            domain: "macro".into(),
            n_right: 5,
            n_wrong: 1,
            direction: Some("late".into()),
        };
        assert_eq!(
            pattern_statement_template(&s),
            "Your macro calls have a late record — 5 of 6 held up."
        );
    }

    #[test]
    fn nudge_template_exact() {
        let s = NudgeSlots {
            domain: "team-exec".into(),
            conviction: 0.83,
            n_recent_misses: 2,
            n_recent_total: 9,
            hush_pattern: "team-exec".into(),
        };
        assert_eq!(
            nudge_template(&s),
            "[zbrain] You just committed a team-exec take at conviction 0.83. \
             Recent record on similar calls: 2 of 9 missed. \
             Hush this pattern for 14 days: zbrain takes nudge --hush team-exec"
        );
    }

    #[test]
    fn forecast_blurb_unavailable_when_small_bucket() {
        let s = ForecastBlurbSlots {
            domain: "market".into(),
            conviction: 0.7,
            bucket_brier: 0.2,
            overall_brier: 0.18,
            bucket_n: 4,
        };
        assert_eq!(
            forecast_blurb_template(&s),
            "Forecast unavailable: only 4 resolved market takes at this conviction yet."
        );
    }

    #[test]
    fn forecast_blurb_worse_and_on_par() {
        let worse = ForecastBlurbSlots {
            domain: "market".into(),
            conviction: 0.7,
            bucket_brier: 0.25,
            overall_brier: 0.18,
            bucket_n: 12,
        };
        assert_eq!(
            forecast_blurb_template(&worse),
            "Predicted Brier in market at conviction 0.70: 0.25 (worse than your average, n=12)."
        );
        let par = ForecastBlurbSlots {
            domain: "market".into(),
            conviction: 0.7,
            bucket_brier: 0.18,
            overall_brier: 0.18,
            bucket_n: 12,
        };
        assert_eq!(
            forecast_blurb_template(&par),
            "Predicted Brier in market at conviction 0.70: 0.18 (on par with your average, n=12)."
        );
    }

    #[test]
    fn dashboard_caption() {
        let s = DashboardCaptionSlots {
            surface: "Brier trend".into(),
            fact: "down 0.04 this quarter".into(),
        };
        assert_eq!(dashboard_caption_template(&s), "Brier trend: down 0.04 this quarter");
    }

    #[test]
    fn morning_pulse_trends() {
        let improving = MorningPulseSlots {
            brier: 0.18,
            trend: MorningPulseTrend::Improving,
            top_pattern: "over-confident on execution".into(),
        };
        assert_eq!(
            morning_pulse_template(&improving),
            "Brier 0.18 (improving). Top pattern: over-confident on execution."
        );
        let stable = MorningPulseSlots {
            brier: 0.22,
            trend: MorningPulseTrend::Stable,
            top_pattern: String::new(),
        };
        assert_eq!(morning_pulse_template(&stable), "Brier 0.22 (stable). ");
        let declining = MorningPulseSlots {
            brier: 0.3,
            trend: MorningPulseTrend::Declining,
            top_pattern: String::new(),
        };
        assert_eq!(morning_pulse_template(&declining), "Brier 0.30 (declining). ");
    }

    #[test]
    fn recall_footer_null_profile() {
        let opts = RecallFooterOpts {
            profile: None,
            abandoned_threads: vec![],
            thread_column_width: 0,
        };
        assert_eq!(build_recall_calibration_footer(&opts), "");
    }

    #[test]
    fn recall_footer_too_few_resolved() {
        let opts = RecallFooterOpts {
            profile: Some(RecallFooterProfile {
                total_resolved: 3,
                brier: Some(0.18),
                pattern_statements: vec![],
            }),
            abandoned_threads: vec![],
            thread_column_width: 0,
        };
        assert_eq!(build_recall_calibration_footer(&opts), "");
    }

    #[test]
    fn recall_footer_full_render() {
        // Use deterministic claim lengths so column padding/truncation is
        // exactly assertable: thread1 = 10 chars (pads to 50), thread2 = 60
        // chars (truncates to 49 + ellipsis = 50, no pad).
        let opts = RecallFooterOpts {
            profile: Some(RecallFooterProfile {
                total_resolved: 42,
                brier: Some(0.18),
                pattern_statements: vec![
                    "Right on early-stage tactics".into(),
                    "Late on macro by 18 months".into(),
                ],
            }),
            abandoned_threads: vec![
                AbandonedThreadSummary {
                    claim: "a".repeat(10),
                    months_silent: 17,
                },
                AbandonedThreadSummary {
                    claim: "b".repeat(60),
                    months_silent: 12,
                },
            ],
            thread_column_width: 50,
        };
        let out = build_recall_calibration_footer(&opts);
        // Build the expected string with the same padding/truncation logic so
        // the assertion stays exact without hand-counting column spaces.
        let thread1 = format!("  · {:<50} (17 months silent)", "a".repeat(10));
        let truncated2 = format!("{}{}", "b".repeat(49), "…"); // 49 + ellipsis = 50 chars
        let thread2 = format!("  · {:<50} (12 months silent)", truncated2);
        let expected = format!(
            "Calibration this quarter:\n  Brier 0.18 (solid).\n  Right on early-stage tactics\n  Late on macro by 18 months\n\nThreads you opened and never came back to:\n{}\n{}",
            thread1, thread2
        );
        assert_eq!(out, expected);
    }

    #[test]
    fn voice_gate_modes_parity() {
        // Every mode in VOICE_GATE_MODES must have a template function; this
        // guards the "mode parity" contract from the TS module.
        assert_eq!(VOICE_GATE_MODES.len(), 5);
        assert!(VOICE_GATE_MODES.contains(&"pattern_statement"));
        assert!(VOICE_GATE_MODES.contains(&"nudge"));
        assert!(VOICE_GATE_MODES.contains(&"forecast_blurb"));
        assert!(VOICE_GATE_MODES.contains(&"dashboard_caption"));
        assert!(VOICE_GATE_MODES.contains(&"morning_pulse"));
    }

    // ── voice-gate judge parsing ──

    #[test]
    fn parse_judge_empty_and_whitespace() {
        let v = parse_judge_output("");
        assert_eq!(
            v,
            VoiceGateJudgeVerdict { verdict: VoiceGateVerdict::Academic, reason: "empty_judge_output".into() }
        );
        let v2 = parse_judge_output("   \n  ");
        assert_eq!(v2.verdict, VoiceGateVerdict::Academic);
        assert_eq!(v2.reason, "empty_judge_output");
    }

    #[test]
    fn parse_judge_no_brace_is_parse_failed() {
        let v = parse_judge_output("totally not json");
        assert_eq!(
            v,
            VoiceGateJudgeVerdict { verdict: VoiceGateVerdict::Academic, reason: "parse_failed".into() }
        );
    }

    #[test]
    fn parse_judge_valid_conversational() {
        let v = parse_judge_output(r#"{"verdict":"conversational","reason":"sounds friendly"}"#);
        assert_eq!(v.verdict, VoiceGateVerdict::Conversational);
        assert_eq!(v.reason, "sounds friendly");
    }

    #[test]
    fn parse_judge_defaults_to_academic() {
        let v = parse_judge_output(r#"{"verdict":"something-else","reason":"x"}"#);
        assert_eq!(v.verdict, VoiceGateVerdict::Academic);
    }

    #[test]
    fn parse_judge_missing_verdict_field_is_academic() {
        let v = parse_judge_output(r#"{"reason":"no verdict key"}"#);
        assert_eq!(v.verdict, VoiceGateVerdict::Academic);
        assert_eq!(v.reason, "no verdict key");
    }

    #[test]
    fn parse_judge_missing_reason_defaults() {
        let v = parse_judge_output(r#"{"verdict":"conversational"}"#);
        assert_eq!(v.verdict, VoiceGateVerdict::Conversational);
        assert_eq!(v.reason, "no_reason");
    }

    #[test]
    fn parse_judge_fenced_json() {
        let raw = "```json\n{\"verdict\":\"conversational\",\"reason\":\"ok\"}\n```";
        let v = parse_judge_output(raw);
        assert_eq!(v.verdict, VoiceGateVerdict::Conversational);
        assert_eq!(v.reason, "ok");
    }

    #[test]
    fn parse_judge_leading_prose() {
        let raw = "Here is my verdict: {\"verdict\":\"conversational\",\"reason\":\"fine\"}";
        let v = parse_judge_output(raw);
        assert_eq!(v.verdict, VoiceGateVerdict::Conversational);
        assert_eq!(v.reason, "fine");
    }

    #[test]
    fn parse_judge_reason_truncated_to_80_chars() {
        let long = "a".repeat(120);
        let raw = format!("{{\"verdict\":\"academic\",\"reason\":\"{}\"}}", long);
        let v = parse_judge_output(&raw);
        assert_eq!(v.verdict, VoiceGateVerdict::Academic);
        assert_eq!(v.reason.chars().count(), 80);
    }

    #[test]
    fn parse_judge_invalid_json_is_parse_failed() {
        let v = parse_judge_output("{not valid json}");
        assert_eq!(v.reason, "parse_failed");
    }

    #[test]
    fn default_rubric_covers_all_modes() {
        let modes = [
            VoiceGateMode::PatternStatement,
            VoiceGateMode::Nudge,
            VoiceGateMode::ForecastBlurb,
            VoiceGateMode::DashboardCaption,
            VoiceGateMode::MorningPulse,
        ];
        for m in modes {
            let r = default_rubric(m);
            assert!(!r.is_empty(), "{:?} rubric empty", m);
        }
        assert!(default_rubric(VoiceGateMode::Nudge).contains("friend tapping"));
        // Mode strings stay in sync with the VOICE_GATE_MODES parity list.
        for m in modes {
            assert!(VOICE_GATE_MODES.contains(&m.as_str()));
        }
        assert_eq!(modes.len(), VOICE_GATE_MODES.len());
    }

    // ── take-forecast pure math ──

    fn scorecard(resolved: i64, brier: f64) -> TakesScorecard {
        TakesScorecard {
            total_bets: resolved,
            resolved,
            correct: 0,
            incorrect: 0,
            partial: 0,
            accuracy: None,
            brier: Some(brier),
            partial_rate: None,
            unresolvable_count: Some(0),
            unresolvable_rate: None,
        }
    }

    #[test]
    fn resolve_domain_prefix_cases() {
        assert_eq!(resolve_domain_prefix(None), None);
        assert_eq!(resolve_domain_prefix(Some("")), None);
        assert_eq!(resolve_domain_prefix(Some("  ")), None);
        // Free-form words fall back to None (overall scorecard).
        assert_eq!(resolve_domain_prefix(Some("Macro")), None);
        assert_eq!(resolve_domain_prefix(Some("geography")), None);
        // Slug-prefix-looking values are kept as-is (lower-cased, trimmed).
        assert_eq!(resolve_domain_prefix(Some("companies/")), Some("companies/".to_string()));
        assert_eq!(resolve_domain_prefix(Some("  Wiki/foo  ")), Some("wiki/foo".to_string()));
        assert_eq!(resolve_domain_prefix(Some("People/Acme")), Some("people/acme".to_string()));
        // Only wiki/companies/people/ are kept as-is; other slug-like hints
        // (deals/) fall back to None, matching TS resolveDomainPrefix.
        assert_eq!(resolve_domain_prefix(Some("deals/2024")), None);
        // Any trailing-slash value is kept as-is (the `ends_with('/')`
        // branch), NOT only the whitelisted wiki/companies/people prefixes.
        assert_eq!(resolve_domain_prefix(Some("deals/2024/")), Some("deals/2024/".to_string()));
        assert_eq!(resolve_domain_prefix(Some("companies")), None);
    }

    #[test]
    fn compute_forecast_overall_only() {
        let overall = scorecard(42, 0.18);
        let f = compute_forecast(&overall, None, None);
        assert_eq!(f.overall_brier, Some(0.18));
        assert_eq!(f.bucket_n, 42);
        assert_eq!(f.predicted_brier, Some(0.18));
        assert_eq!(f.bucket_domain, "overall");
        assert!(!f.insufficient_data);
    }

    #[test]
    fn compute_forecast_insufficient_bucket() {
        let overall = scorecard(42, 0.18);
        let bucket = scorecard(3, 0.30);
        let f = compute_forecast(&overall, Some(&bucket), Some("market"));
        assert!(f.insufficient_data);
        assert_eq!(f.predicted_brier, None);
        assert_eq!(f.bucket_n, 3);
        assert_eq!(f.bucket_domain, "market");
        assert_eq!(f.overall_brier, Some(0.18));
    }

    #[test]
    fn compute_forecast_sufficient_bucket() {
        let overall = scorecard(42, 0.18);
        let bucket = scorecard(12, 0.25);
        let f = compute_forecast(&overall, Some(&bucket), Some("market"));
        assert!(!f.insufficient_data);
        assert_eq!(f.predicted_brier, Some(0.25));
        assert_eq!(f.bucket_n, 12);
    }

    // ── cross-brain ──

    #[test]
    fn can_read_mounts_local_cli_always_yes() {
        assert!(can_read_mounts_for_ctx(false, None, None));
        assert!(can_read_mounts_for_ctx(false, Some(true), None));
        assert!(can_read_mounts_for_ctx(false, Some(true), Some(&[])));
    }

    #[test]
    fn can_read_mounts_mcp_read_scope_yes() {
        // remote but not a subagent loop → yes
        assert!(can_read_mounts_for_ctx(true, Some(false), None));
        assert!(can_read_mounts_for_ctx(true, None, None));
    }

    #[test]
    fn can_read_mounts_subagent_gate() {
        // subagent defaults to no
        assert!(!can_read_mounts_for_ctx(true, Some(true), None));
        assert!(!can_read_mounts_for_ctx(true, Some(true), Some(&[])));
        // trusted subagent (allowed_slug_prefixes non-empty) → yes
        let prefixes = vec!["companies/".to_string()];
        assert!(can_read_mounts_for_ctx(true, Some(true), Some(&prefixes)));
    }

    #[test]
    fn attribution_suffix_local_empty_mount_has_suffix() {
        let dummy_profile = CalibrationProfileRow {
            id: 1,
            source_id: "test".into(),
            holder: "test".into(),
            wave_version: "1".into(),
            generated_at: "2024-01-01T00:00:00Z".into(),
            published: false,
            total_resolved: 0,
            brier: None,
            accuracy: None,
            partial_rate: None,
            grade_completion: 0.0,
            domain_scorecards: serde_json::json!({}),
            pattern_statements: vec![],
            voice_gate_passed: false,
            voice_gate_attempts: 0,
            active_bias_tags: vec![],
            model_id: "test".into(),
            cost_usd: None,
            judge_model_agreement: None,
        };
        let local = CrossBrainProfileResult {
            profile: dummy_profile.clone(),
            source_brain_id: "garry".into(),
            from_mount: false,
        };
        assert_eq!(attribution_suffix(&local), "");
        let mount = CrossBrainProfileResult {
            profile: dummy_profile,
            source_brain_id: "team-x".into(),
            from_mount: true,
        };
        assert_eq!(attribution_suffix(&mount), " (from mounted brain: team-x)");
    }

    // ── nudge ──

    fn nudge_take(slug: &str, holder: &str, weight: f64) -> NudgeTake {
        NudgeTake {
            page_slug: slug.into(),
            holder: holder.into(),
            weight,
        }
    }

    fn nudge_profile(holder: &str, tags: Vec<String>) -> CalibrationProfileRow {
        CalibrationProfileRow {
            id: 1,
            source_id: "s".into(),
            holder: holder.into(),
            wave_version: "v0.36.1.0".into(),
            generated_at: "t".into(),
            published: false,
            total_resolved: 0,
            brier: None,
            accuracy: None,
            partial_rate: None,
            grade_completion: 1.0,
            domain_scorecards: serde_json::json!({}),
            pattern_statements: vec![],
            voice_gate_passed: false,
            voice_gate_attempts: 0,
            active_bias_tags: tags,
            model_id: "".into(),
            cost_usd: None,
            judge_model_agreement: None,
        }
    }

    #[test]
    fn take_domain_hint_cases() {
        assert_eq!(take_domain_hint(&nudge_take("companies/google", "g", 0.9)), "hiring");
        assert_eq!(take_domain_hint(&nudge_take("/people/charlie", "g", 0.9)), "founder-behavior");
        assert_eq!(take_domain_hint(&nudge_take("deals/series-a", "g", 0.9)), "market-timing");
        assert_eq!(take_domain_hint(&nudge_take("macro-outlook", "g", 0.9)), "macro");
        assert_eq!(take_domain_hint(&nudge_take("geography-europe", "g", 0.9)), "geography");
        assert_eq!(take_domain_hint(&nudge_take("growth-tactics", "g", 0.9)), "tactics");
        assert_eq!(take_domain_hint(&nudge_take("note-about-ai-models", "g", 0.9)), "ai");
        assert_eq!(take_domain_hint(&nudge_take("random-thought", "g", 0.9)), "");
    }

    #[test]
    fn evaluate_nudge_no_profile() {
        let t = nudge_take("companies/x", "garry", 0.9);
        let r = evaluate_nudge_rule(&t, None);
        assert!(!r.matched);
        assert_eq!(r.reason, Some(NudgeReason::NoProfile));
    }

    #[test]
    fn evaluate_nudge_wrong_holder() {
        let t = nudge_take("companies/x", "garry", 0.9);
        let p = nudge_profile("other", vec!["hiring".into()]);
        let r = evaluate_nudge_rule(&t, Some(&p));
        assert_eq!(r.reason, Some(NudgeReason::WrongHolder));
    }

    #[test]
    fn evaluate_nudge_below_threshold() {
        let t = nudge_take("companies/x", "garry", 0.5);
        let p = nudge_profile("garry", vec!["hiring".into()]);
        let r = evaluate_nudge_rule(&t, Some(&p));
        assert_eq!(r.reason, Some(NudgeReason::BelowConvictionThreshold));
    }

    #[test]
    fn evaluate_nudge_no_matching_tag() {
        let t = nudge_take("random-thought", "garry", 0.9);
        let p = nudge_profile("garry", vec!["hiring".into()]);
        let r = evaluate_nudge_rule(&t, Some(&p));
        assert_eq!(r.reason, Some(NudgeReason::NoMatchingBiasTag));
    }

    #[test]
    fn evaluate_nudge_matched_case_insensitive() {
        let t = nudge_take("companies/google", "garry", 0.9);
        let p = nudge_profile("garry", vec!["HIRING".into(), "macro".into()]);
        let r = evaluate_nudge_rule(&t, Some(&p));
        assert!(r.matched);
        assert_eq!(r.matched_tag, Some("HIRING".into()));
    }

    // ── gstack ──

    #[test]
    fn build_learning_entry_incorrect_with_tags_and_reasoning() {
        let e = IncorrectResolutionEvent {
            take_id: 42,
            page_slug: "companies/google".into(),
            row_num: 3,
            holder: "garry".into(),
            claim: "Google will hire 1000 people".into(),
            quality: GstackQuality::Incorrect,
            weight: 0.83,
            active_bias_tags: Some(vec!["hiring".into()]),
            confidence: Some(0.9),
            reasoning: Some("missed the hiring freeze".into()),
        };
        let entry = build_learning_entry(&e);
        assert_eq!(entry.skill, "zbrain-calibration");
        assert_eq!(entry.r#type, "observation");
        assert_eq!(entry.key, "zbrain:calibration:v0.36.1.0:take-42:hiring");
        assert!(entry.insight.contains("garry was wrong on \"Google will hire 1000 people\""));
        assert!(entry.insight.contains("(conviction 0.83, graded incorrect)"));
        assert!(entry.insight.contains("Pattern: hiring."));
        assert!(entry.insight.contains("Reasoning: missed the hiring freeze"));
        assert_eq!(entry.confidence, 0.9);
        assert_eq!(entry.files, Some(vec!["companies/google".to_string()]));
    }

    #[test]
    fn build_learning_entry_partial_defaults_confidence_no_tags() {
        let e = IncorrectResolutionEvent {
            take_id: 1,
            page_slug: "p".into(),
            row_num: 1,
            holder: "g".into(),
            claim: "c".into(),
            quality: GstackQuality::Partial,
            weight: 0.5,
            active_bias_tags: None,
            confidence: None,
            reasoning: None,
        };
        let entry = build_learning_entry(&e);
        // No tag suffix → key has no extra colon segment.
        assert_eq!(entry.key, "zbrain:calibration:v0.36.1.0:take-1");
        // confidence defaults to 0.8 when omitted.
        assert_eq!(entry.confidence, 0.8);
        assert!(entry.insight.contains("was partially wrong on \"c\""));
        assert!(!entry.insight.contains("Pattern:"));
    }

    #[test]
    fn build_learning_entry_truncates_long_claim() {
        let e = IncorrectResolutionEvent {
            take_id: 9,
            page_slug: "p".into(),
            row_num: 1,
            holder: "g".into(),
            claim: "a".repeat(300),
            quality: GstackQuality::Incorrect,
            weight: 0.7,
            active_bias_tags: None,
            confidence: None,
            reasoning: None,
        };
        let entry = build_learning_entry(&e);
        // TS buildLearningEntry: `claim.slice(0, 200) + '…'` — the 300-char
        // claim is truncated to EXACTLY 200 chars followed by the ellipsis.
        // Pin both bounds so a regression (over/under-truncating) fails.
        let run_200 = format!("{}…", "a".repeat(200));
        assert!(
            entry.insight.contains(&run_200),
            "claim should be truncated to exactly 200 'a's + ellipsis"
        );
        assert!(
            !entry.insight.contains(&"a".repeat(201)),
            "claim must not retain 201+ consecutive 'a's"
        );
    }

    #[test]
    fn gstack_entry_serializes_to_camel_case() {
        let e = IncorrectResolutionEvent {
            take_id: 7,
            page_slug: "p".into(),
            row_num: 1,
            holder: "g".into(),
            claim: "c".into(),
            quality: GstackQuality::Incorrect,
            weight: 0.7,
            active_bias_tags: None,
            confidence: None,
            reasoning: None,
        };
        let entry = build_learning_entry(&e);
        let json = serde_json::to_string(&entry).unwrap();
        assert!(json.contains("\"type\":\"observation\""));
        assert!(json.contains("\"skill\":\"zbrain-calibration\""));
        assert!(json.contains("\"source\":\"observed\""));
    }

    #[test]
    fn default_gstack_writer_missing_binary_is_binary_not_found() {
        // The gstack binary is not installed in this environment; the writer
        // must surface BinaryNotFound rather than panic.
        let entry = GstackLearningEntry {
            skill: "zbrain-calibration".into(),
            r#type: "observation".into(),
            key: "k".into(),
            insight: "i".into(),
            confidence: 0.8,
            source: "observed".into(),
            files: None,
        };
        let err = default_gstack_writer(&entry);
        assert!(err.is_err());
        assert_eq!(err.unwrap_err().kind, GstackWriteErrorKind::BinaryNotFound);
    }

    // ── think-ab ──

    fn empty_ab_report() -> AbReportResult {
        AbReportResult {
            total_trials: 0,
            baseline_wins: 0,
            with_calibration_wins: 0,
            ties: 0,
            neither: 0,
            with_calibration_win_rate: None,
            net_negative: false,
            decisive_trials: 0,
        }
    }

    #[test]
    fn format_ab_report_empty() {
        let out = format_ab_report(&empty_ab_report(), 30);
        assert!(out.contains("A/B report (last 30 days):"));
        assert!(out.contains("No data yet. Try: zbrain think --ab"));
    }

    #[test]
    fn format_ab_report_normal_no_warning() {
        let r = AbReportResult {
            total_trials: 20,
            baseline_wins: 8,
            with_calibration_wins: 12,
            ties: 0,
            neither: 0,
            with_calibration_win_rate: Some(12.0 / 20.0),
            net_negative: false,
            decisive_trials: 20,
        };
        let out = format_ab_report(&r, 30);
        assert!(out.contains("Baseline wins:           8"));
        assert!(out.contains("With-calibration wins:   12"));
        assert!(out.contains("With-calibration win rate (decisive trials only): 60.0% (n=20)"));
        assert!(!out.contains("WARNING"));
    }

    #[test]
    fn format_ab_report_net_negative_warns() {
        let r = AbReportResult {
            total_trials: 40,
            baseline_wins: 30,
            with_calibration_wins: 10,
            ties: 0,
            neither: 0,
            with_calibration_win_rate: Some(10.0 / 40.0),
            net_negative: true,
            decisive_trials: 40,
        };
        let out = format_ab_report(&r, 7);
        assert!(out.contains("A/B report (last 7 days):"));
        assert!(out.contains("WARNING: calibration_net_negative:"));
    }

    #[test]
    fn format_ab_report_no_win_rate_when_decisive_zero() {
        let r = AbReportResult {
            total_trials: 5,
            baseline_wins: 0,
            with_calibration_wins: 0,
            ties: 5,
            neither: 0,
            with_calibration_win_rate: None,
            net_negative: false,
            decisive_trials: 0,
        };
        let out = format_ab_report(&r, 30);
        assert!(!out.contains("win rate"));
    }

    // ── engine-backed forecast wrappers ──────────────────────────────────

    use crate::calibration_queries::{CalibrationBucket, PatternDetail};
    use std::collections::HashMap;

    /// Minimal mock implementing `CalibrationQueries`. Keyed on
    /// `(holder, domain_or_empty)` so tests can seed overall + bucketed
    /// scorecards independently. Only `get_scorecard` is meaningful; the
    /// other trait methods return empty/None.
    #[derive(Debug, Default)]
    struct FakeCalibrationEngine {
        scorecards: HashMap<(String, String), TakesScorecard>,
    }

    impl FakeCalibrationEngine {
        fn seed(&mut self, holder: &str, domain: Option<&str>, sc: TakesScorecard) {
            self.scorecards
                .insert((holder.to_string(), domain.unwrap_or("").to_string()), sc);
        }
    }

    #[async_trait]
    impl CalibrationQueries for FakeCalibrationEngine {
        async fn get_scorecard(
            &self,
            query: &crate::calibration_queries::ScorecardQuery<'_>,
        ) -> crate::error::Result<TakesScorecard> {
            Ok(self
                .scorecards
                .get(&(
                    query.holder.unwrap_or("").to_string(),
                    query.domain_prefix.unwrap_or("").to_string(),
                ))
                .cloned()
                .unwrap_or_else(zero_scorecard))
        }

        async fn get_calibration_curve(
            &self,
            _holder: &str,
        ) -> crate::error::Result<Vec<CalibrationBucket>> {
            Ok(vec![])
        }

        async fn get_latest_profile(
            &self,
            _holder: &str,
            _source_id: Option<&str>,
            _source_ids: Option<&[String]>,
        ) -> crate::error::Result<Option<CalibrationProfileRow>> {
            Ok(None)
        }

        async fn get_pattern_detail(
            &self,
            _holder: &str,
            _pattern_index: usize,
        ) -> crate::error::Result<Option<PatternDetail>> {
            Ok(None)
        }
    }

    fn sc(resolved: i64, brier: f64) -> TakesScorecard {
        TakesScorecard {
            total_bets: resolved,
            resolved,
            correct: 0,
            incorrect: 0,
            partial: 0,
            accuracy: None,
            brier: Some(brier),
            partial_rate: None,
            unresolvable_count: Some(0),
            unresolvable_rate: None,
        }
    }

    #[tokio::test]
    async fn forecast_for_take_no_domain_uses_overall() {
        let mut eng = FakeCalibrationEngine::default();
        eng.seed("garry", None, sc(8, 0.15));
        let f = forecast_for_take(
            &eng,
            &TakeForecastInput { holder: "garry".into(), domain: None, conviction: 0.7 },
        )
        .await;
        // n=8 ≥ MIN_BUCKET_N so we get a forecast; bucket falls back to overall.
        assert_eq!(f.bucket_domain, "overall");
        assert_eq!(f.bucket_n, 8);
        assert!(!f.insufficient_data);
        assert_eq!(f.predicted_brier, Some(0.15));
        assert_eq!(f.overall_brier, Some(0.15));
    }

    #[tokio::test]
    async fn forecast_for_take_domain_prefix_uses_bucket() {
        let mut eng = FakeCalibrationEngine::default();
        eng.seed("garry", None, sc(20, 0.20));
        // A slug-prefix domain ("companies/") IS resolved → bucketed lookup.
        eng.seed("garry", Some("companies/"), sc(6, 0.10));
        let f = forecast_for_take(
            &eng,
            &TakeForecastInput {
                holder: "garry".into(),
                domain: Some("companies/".into()),
                conviction: 0.9,
            },
        )
        .await;
        assert_eq!(f.bucket_domain, "companies/");
        assert_eq!(f.bucket_n, 6);
        assert_eq!(f.predicted_brier, Some(0.10));
        // overall_brier still comes from the unbucketed scorecard.
        assert_eq!(f.overall_brier, Some(0.20));
    }

    #[tokio::test]
    async fn forecast_for_take_freeform_domain_falls_back_to_overall() {
        let mut eng = FakeCalibrationEngine::default();
        eng.seed("garry", None, sc(9, 0.22));
        // 'macro' is a free-form hint (no slug prefix, no trailing '/') →
        // resolve_domain_prefix returns None → no bucketed lookup, but the
        // bucket_domain label still reflects the caller's hint.
        let f = forecast_for_take(
            &eng,
            &TakeForecastInput {
                holder: "garry".into(),
                domain: Some("macro".into()),
                conviction: 0.5,
            },
        )
        .await;
        assert_eq!(f.bucket_domain, "macro");
        assert_eq!(f.bucket_n, 9);
        assert_eq!(f.predicted_brier, Some(0.22));
    }

    #[tokio::test]
    async fn forecast_for_take_insufficient_data_when_below_min() {
        let mut eng = FakeCalibrationEngine::default();
        eng.seed("garry", None, sc(4, 0.30)); // n=4 < MIN_BUCKET_N(5)
        let f = forecast_for_take(
            &eng,
            &TakeForecastInput { holder: "garry".into(), domain: None, conviction: 0.6 },
        )
        .await;
        assert!(f.insufficient_data);
        assert_eq!(f.predicted_brier, None);
        assert_eq!(f.bucket_n, 4);
    }

    #[tokio::test]
    async fn batch_forecast_maps_each_input() {
        let mut eng = FakeCalibrationEngine::default();
        eng.seed("garry", None, sc(10, 0.12));
        eng.seed("alice", None, sc(3, 0.40));
        let inputs = vec![
            TakeForecastInput { holder: "garry".into(), domain: None, conviction: 0.8 },
            TakeForecastInput { holder: "alice".into(), domain: None, conviction: 0.5 },
        ];
        let out = batch_forecast(&eng, &inputs).await;
        assert_eq!(out.len(), 2);
        assert_eq!(out[0].predicted_brier, Some(0.12));
        assert!(!out[0].insufficient_data);
        assert_eq!(out[1].predicted_brier, None); // n=3 < 5
        assert!(out[1].insufficient_data);
    }
}
