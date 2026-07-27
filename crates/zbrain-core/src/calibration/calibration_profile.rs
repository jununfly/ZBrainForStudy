//! 1-3-3-7 — `runPhaseCalibrationProfile`: calibration-profile cycle phase port.
//!
//! Port of `src/core/cycle/calibration-profile.ts` (v0.36.1.0, T6 / D11 / D24).
//! Aggregates a holder's resolved-takes subset into one calibration profile:
//!   - quantitative: `TakesScorecard` (Brier, accuracy, partial_rate, per-domain)
//!   - qualitative: 2-4 pattern statements via the voice gate (`gateVoice`)
//!   - bias tags: short kebab-case labels (e.g. `over-confident-geography`)
//!
//! Faithful to the canonical function, with the cluster-wide grill decisions
//! applied:
//!   - LLM deps injected via `PatternStatementsGenerator` / `BiasTagsGenerator`
//!     async traits (continues the Q4 trait-DI shape); production impls wrap
//!     `Arc<dyn ChatProvider>` (Sonnet for patterns, Haiku for the judge).
//!   - `pattern_statement_template` / `PatternStatementSlots` are reused from
//!     `crate::calibration` (not re-ported).
//!   - budget gate decomposed into a `BudgetGate` trait (standalone function
//!     has no per-cycle spend accumulator); default is permissive.
//!   - `calibration_profiles` write uses the typed `CalibrationQueries::
//!     insert_calibration_profile` (Q3: no `execute_raw` escape hatch).

use std::fmt;
use std::sync::Arc;

use async_trait::async_trait;
use serde_json::json;

use crate::ai::chat::{ChatMessage, ChatOpts, ChatProvider, ChatRole};
use crate::calibration::voice_gate::{
    gate_voice, LlmBackedVoiceJudge, VoiceGateError, VoiceGateMode, VoiceGateOpts, VoiceGenerator,
    VoiceJudge,
};
use crate::calibration::{aggregate_domain_scorecards, pattern_statement_template, CalibrationDomain, PatternStatementSlots};
use crate::calibration_queries::{CalibrationProfileInsert, ScorecardQuery, TakesScorecard};
use crate::engine::BrainEngine;
use crate::error::{Error, Result as ZbResult};

pub const CALIBRATION_PROFILE_PROMPT_VERSION: &str = "v0.36.1.0-stub";

/// Default model for pattern statements (mirrors TS `claude-sonnet-4-6`).
const DEFAULT_PATTERNS_MODEL: &str = "claude-sonnet-4-6";
/// Default model for the voice-gate judge (mirrors TS `defaultJudge` Haiku).
const DEFAULT_JUDGE_MODEL: &str = "claude-haiku-4-5-20251001";
/// Below this many resolved takes a brain is too cold for a profile (TS `if (scorecard.resolved < 5)`).
const COLD_BRAIN_MIN_RESOLVED: i64 = 5;

const PATTERN_STATEMENTS_PROMPT: &str = r#"[v0.36.1.0-stub] You are summarizing a forecaster's track record so they
can see their patterns. Below is a JSON snapshot of how they performed —
per-domain scorecards over the resolved subset.

Write 2 to 4 short pattern statements, ONE per line. Each statement:
- Names a domain (e.g. "macro tech", "geography", "hiring decisions").
- States the direction (right / wrong / late / early / over-confident /
  under-calibrated).
- Includes ONE concrete number a reader can verify ("2 of 5 missed").
- Sounds like a smart friend recapping the record, not a doctor or HR.
- Under 25 words.

EXAMPLES of the voice we want:
- "You called early-stage tactics well — 8 of 10 held up."
- "Geography is your blind spot. High-conviction calls missed 4 of 6."
- "On macro tech you tend to be ~18 months early; calls land, just later."

DO NOT use phrases like "the data shows", "our analysis indicates", "Brier
score", or "conviction bucket". DO NOT preach. Be plain.

Output the 2-4 pattern statements only, one per line. No numbering, no
prose around them.

SCORECARD:
{SCORECARD_JSON}
"#;

const BIAS_TAGS_PROMPT: &str = r#"Based on the pattern statements below, emit 1-4
kebab-case bias tags. Each tag combines an axis (over-confident,
under-confident, early, late, hedged-correctly) with a domain
(tactics, macro, geography, hiring, market-timing, founder-behavior,
ai, other).

Examples: "over-confident-geography", "late-on-macro-tech",
"hedged-correctly-on-hiring".

Output ONLY a JSON array of strings. No prose. If no clear bias pattern
emerges, return [].

PATTERN STATEMENTS:
{PATTERNS_BULLETS}
"#;

/// Domain-level error for calibration-profile generation (generator / judge
/// failures). Kept distinct from [`VoiceGateError`] so the two layers report
/// separately.
#[derive(Debug, Clone)]
pub struct CalibrationProfileError(pub String);

impl fmt::Display for CalibrationProfileError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl std::error::Error for CalibrationProfileError {}

/// Parse a newline-separated pattern-statement block (port of TS
/// `parsePatternStatementsOutput`). Strips leading bullets (`-`/`*`/`•`) and
/// `N.`/`N)` numbering, drops empty/over-long lines, caps at 4.
pub fn parse_pattern_statements_output(raw: &str) -> Vec<String> {
    if raw.trim().is_empty() {
        return Vec::new();
    }
    let mut out: Vec<String> = Vec::new();
    for line in raw.split('\n') {
        let line = line.trim();
        // Strip EITHER a leading bullet OR a leading "N."/"N)" prefix (mutually
        // exclusive, mirroring the TS regex alternation `^[-*•]\s+|^\d+[.)]\s+`).
        let stripped = if let Some(s) = line
            .strip_prefix("- ")
            .or_else(|| line.strip_prefix("* "))
            .or_else(|| line.strip_prefix("• "))
        {
            s
        } else {
            strip_leading_number_prefix(line)
        };
        let stripped = stripped.trim();
        if !stripped.is_empty() && stripped.len() <= 200 {
            out.push(stripped.to_string());
        }
        if out.len() == 4 {
            break;
        }
    }
    out
}

/// Strip a leading `N.` or `N)` prefix (digits followed by `.`/`)` and a space).
fn strip_leading_number_prefix(s: &str) -> &str {
    let bytes = s.as_bytes();
    let mut i = 0;
    while i < bytes.len() && bytes[i].is_ascii_digit() {
        i += 1;
    }
    if i > 0 && i + 1 < bytes.len() && (bytes[i] == b'.' || bytes[i] == b')') && bytes[i + 1] == b' ' {
        &s[i + 2..]
    } else {
        s
    }
}

/// Parse a JSON-array bias-tags block, tolerant of fence wrapping (port of TS
/// `parseBiasTagsOutput`). Lowercases, validates kebab-case, caps at 4.
pub fn parse_bias_tags_output(raw: &str) -> Vec<String> {
    if raw.trim().is_empty() {
        return Vec::new();
    }
    let mut text = raw.trim().to_string();
    if let Some(inner) = strip_code_fence(&text) {
        text = inner;
    }
    let first_arr = match text.find('[') {
        Some(i) => i,
        None => return Vec::new(),
    };
    let parsed: serde_json::Value = match serde_json::from_str(&text[first_arr..]) {
        Ok(v) => v,
        Err(_) => return Vec::new(),
    };
    let arr = match parsed {
        serde_json::Value::Array(a) => a,
        _ => return Vec::new(),
    };
    arr.into_iter()
        .filter_map(|t| t.as_str().map(|s| s.trim().to_lowercase()))
        .filter(|t| is_kebab_tag(t))
        .take(4)
        .collect()
}

/// Strip a leading ```json ... ``` fence if present.
fn strip_code_fence(s: &str) -> Option<String> {
    let trimmed = s.trim_start();
    if !trimmed.starts_with("```") {
        return None;
    }
    let rest = &trimmed[3..];
    let rest = rest.strip_prefix("json").unwrap_or(rest);
    let rest = rest.strip_prefix('\n').unwrap_or(rest);
    rest.rfind("```").map(|end| rest[..end].trim().to_string())
}

/// Validate a kebab-case tag: `^[a-z]+(?:-[a-z0-9]+)*$`.
fn is_kebab_tag(s: &str) -> bool {
    if s.is_empty() {
        return false;
    }
    let mut prev_dash = false;
    for (i, c) in s.char_indices() {
        if c == '-' {
            if i == 0 || prev_dash {
                return false;
            }
            prev_dash = true;
        } else if c.is_ascii_lowercase() || c.is_ascii_digit() {
            if i == 0 && !c.is_ascii_lowercase() {
                return false; // must start with a letter
            }
            prev_dash = false;
        } else {
            return false;
        }
    }
    !prev_dash
}

/// Pick the "loudest" pattern slot for the template fallback (port of TS
/// `pickFallbackSlots`).
pub fn pick_fallback_slots(scorecard: &TakesScorecard) -> PatternStatementSlots {
    if scorecard.resolved == 0 {
        return PatternStatementSlots {
            domain: "overall".to_string(),
            n_right: 0,
            n_wrong: 0,
            direction: None,
        };
    }
    let direction = if scorecard.brier.unwrap_or(0.0) > 0.25 {
        Some("over-confident".to_string())
    } else {
        Some("mostly right".to_string())
    };
    PatternStatementSlots {
        domain: "overall".to_string(),
        n_right: scorecard.correct as u32,
        n_wrong: scorecard.incorrect as u32,
        direction,
    }
}

// ── injected LLM dependencies (trait DI, Q4 shape) ──────────────────────

/// Input to a [`PatternStatementsGenerator`].
pub struct PatternStatementsGenInput {
    pub scorecard: TakesScorecard,
    pub holder: String,
    pub attempt: u32,
    pub feedback: Option<String>,
}

/// Produces 2-4 pattern statements for a holder's scorecard (LLM or stub).
#[async_trait]
pub trait PatternStatementsGenerator: Send + Sync {
    async fn generate(&self, input: PatternStatementsGenInput) -> Result<Vec<String>, CalibrationProfileError>;
}

/// Input to a [`BiasTagsGenerator`].
pub struct BiasTagsGenInput {
    pub patterns: Vec<String>,
}

/// Produces 1-4 kebab-case bias tags from the pattern statements (LLM or stub).
#[async_trait]
pub trait BiasTagsGenerator: Send + Sync {
    async fn generate(&self, input: BiasTagsGenInput) -> Result<Vec<String>, CalibrationProfileError>;
}

/// Budget decision returned by a [`BudgetGate`].
pub struct BudgetDecision {
    pub allowed: bool,
    pub budget_usd: f64,
}

/// Gate for the LLM-driven generation step. Port of TS `BaseCyclePhase.
/// checkBudget`. A standalone function has no per-cycle spend accumulator, so
/// this is injected; the default ([`PermissiveBudgetGate`]) always allows.
pub trait BudgetGate: Send + Sync {
    fn allowed(&self, est_input_tokens: u32, max_output_tokens: u32, model_id: &str) -> BudgetDecision;
}

/// Default budget gate: always allows (used when no real budget wiring is
/// injected). Mirrors a generous config cap.
pub struct PermissiveBudgetGate;

impl BudgetGate for PermissiveBudgetGate {
    fn allowed(&self, _est_input_tokens: u32, _max_output_tokens: u32, _model_id: &str) -> BudgetDecision {
        BudgetDecision { allowed: true, budget_usd: 0.5 }
    }
}

/// Production pattern-statements generator backed by a `ChatProvider`.
pub struct ChatBackedPatternStatementsGenerator {
    provider: Arc<dyn ChatProvider>,
    model_id: String,
    #[allow(dead_code)]
    prompt_version: String,
}

impl ChatBackedPatternStatementsGenerator {
    pub fn new(provider: Arc<dyn ChatProvider>, model_id: impl Into<String>, prompt_version: impl Into<String>) -> Self {
        Self {
            provider,
            model_id: model_id.into(),
            prompt_version: prompt_version.into(),
        }
    }
}

#[async_trait]
impl PatternStatementsGenerator for ChatBackedPatternStatementsGenerator {
    async fn generate(&self, input: PatternStatementsGenInput) -> Result<Vec<String>, CalibrationProfileError> {
        let scorecard_json = serde_json::to_string(&json!({
            "holder": input.holder,
            "total_bets": input.scorecard.total_bets,
            "resolved": input.scorecard.resolved,
            "correct": input.scorecard.correct,
            "incorrect": input.scorecard.incorrect,
            "partial": input.scorecard.partial,
            "accuracy": input.scorecard.accuracy,
            "brier": input.scorecard.brier,
            "partial_rate": input.scorecard.partial_rate,
            "unresolvable_count": input.scorecard.unresolvable_count,
            "unresolvable_rate": input.scorecard.unresolvable_rate,
        }))
        .map_err(|e| CalibrationProfileError(e.to_string()))?;
        let prompt = PATTERN_STATEMENTS_PROMPT.replace("{SCORECARD_JSON}", &scorecard_json);
        let feedback_suffix = input
            .feedback
            .as_ref()
            .map(|f| format!("\n\nPrior attempt was rejected for: {f}. Try again, more conversational."))
            .unwrap_or_default();
        let result = self
            .provider
            .chat(ChatOpts {
                model: Some(self.model_id.clone()),
                system: None,
                messages: vec![ChatMessage::text(ChatRole::User, format!("{prompt}{feedback_suffix}"))],
                tools: vec![],
                max_tokens: Some(500),
                cache_system: false,
            })
            .await
            .map_err(|e| CalibrationProfileError(e.to_string()))?;
        Ok(parse_pattern_statements_output(&result.text))
    }
}

/// Production bias-tags generator backed by a `ChatProvider`.
pub struct ChatBackedBiasTagsGenerator {
    provider: Arc<dyn ChatProvider>,
    model_id: String,
}

impl ChatBackedBiasTagsGenerator {
    pub fn new(provider: Arc<dyn ChatProvider>, model_id: impl Into<String>) -> Self {
        Self {
            provider,
            model_id: model_id.into(),
        }
    }
}

#[async_trait]
impl BiasTagsGenerator for ChatBackedBiasTagsGenerator {
    async fn generate(&self, input: BiasTagsGenInput) -> Result<Vec<String>, CalibrationProfileError> {
        let bullets = input
            .patterns
            .iter()
            .map(|p| format!("- {p}"))
            .collect::<Vec<_>>()
            .join("\n");
        let prompt = BIAS_TAGS_PROMPT.replace("{PATTERNS_BULLETS}", &bullets);
        let result = self
            .provider
            .chat(ChatOpts {
                model: Some(self.model_id.clone()),
                system: None,
                messages: vec![ChatMessage::text(ChatRole::User, prompt)],
                tools: vec![],
                max_tokens: Some(200),
                cache_system: false,
            })
            .await
            .map_err(|e| CalibrationProfileError(e.to_string()))?;
        Ok(parse_bias_tags_output(&result.text))
    }
}

/// Adapter: a [`PatternStatementsGenerator`] → [`VoiceGenerator`] producing a
/// single `\n`-joined string for the voice gate (port of the TS closure).
struct PatternStatementsVoiceAdapter {
    inner: Arc<dyn PatternStatementsGenerator>,
    holder: String,
    scorecard: TakesScorecard,
}

#[async_trait]
impl VoiceGenerator for PatternStatementsVoiceAdapter {
    async fn generate(&self, attempt: u32, feedback: Option<String>) -> Result<String, VoiceGateError> {
        let lines = self
            .inner
            .generate(PatternStatementsGenInput {
                scorecard: self.scorecard.clone(),
                holder: self.holder.clone(),
                attempt,
                feedback,
            })
            .await
            .map_err(|e| VoiceGateError(e.to_string()))?;
        Ok(lines.join("\n"))
    }
}

// ── orchestration ─────────────────────────────────────────────────────

/// Options for [`run_calibration_profile`]. Every LLM dependency is optional;
/// when omitted, a `chat` provider is used to build the production defaults.
#[derive(Clone, Default)]
pub struct CalibrationProfileOpts {
    /// Holder to generate the profile for. Default `'garry'`.
    pub holder: Option<String>,
    /// Source id for the profile row. Default `'default'`.
    pub source_id: Option<String>,
    /// `grade_completion` from a same-cycle `grade_takes` phase. Default 1.0.
    pub grade_completion: Option<f64>,
    /// Override prompt version (informational; prompts are versioned by const).
    pub prompt_version: Option<String>,
    /// Override model id for pattern statements / bias tags. Default Sonnet.
    pub model_id: Option<String>,
    /// Production LLM handle. When set, unspecified generators/judge default
    /// to chat-backed impls. Required (directly or via injection) for a run.
    pub chat: Option<Arc<dyn ChatProvider>>,
    /// Inject the pattern-statements generator (tests).
    pub patterns_generator: Option<Arc<dyn PatternStatementsGenerator>>,
    /// Inject the bias-tags generator (tests).
    pub bias_tags_generator: Option<Arc<dyn BiasTagsGenerator>>,
    /// Inject the voice-gate judge (tests).
    pub voice_gate_judge: Option<Arc<dyn VoiceJudge>>,
    /// Inject the budget gate. Default [`PermissiveBudgetGate`].
    pub budget_gate: Option<Arc<dyn BudgetGate>>,
    /// Active-pack calibration domains for `domain_scorecards` widening.
    /// `None`/empty → write `{}` (R1 byte-identical regression).
    pub domains: Option<Vec<CalibrationDomain>>,
}

impl std::fmt::Debug for CalibrationProfileOpts {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CalibrationProfileOpts")
            .field("holder", &self.holder)
            .field("source_id", &self.source_id)
            .field("grade_completion", &self.grade_completion)
            .field("prompt_version", &self.prompt_version)
            .field("model_id", &self.model_id)
            .field("chat", &self.chat.as_ref().map(|_| "Arc<dyn ChatProvider>"))
            .field(
                "patterns_generator",
                &self.patterns_generator.as_ref().map(|_| "Arc<dyn PatternStatementsGenerator>"),
            )
            .field(
                "bias_tags_generator",
                &self.bias_tags_generator.as_ref().map(|_| "Arc<dyn BiasTagsGenerator>"),
            )
            .field(
                "voice_gate_judge",
                &self.voice_gate_judge.as_ref().map(|_| "Arc<dyn VoiceJudge>"),
            )
            .field(
                "budget_gate",
                &self.budget_gate.as_ref().map(|_| "Arc<dyn BudgetGate>"),
            )
            .field("domains", &self.domains)
            .finish()
    }
}

/// Outcome status of a calibration-profile run.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum CalibrationProfileStatus {
    #[default]
    Ok,
    Warn,
    Skipped,
}

/// Result of [`run_calibration_profile`], mirroring the canonical
/// `CalibrationProfileResult`.
#[derive(Debug, Clone, Default)]
pub struct CalibrationProfileResult {
    pub profile_written: bool,
    pub voice_gate_passed: bool,
    pub voice_gate_attempts: u32,
    pub pattern_statements: Vec<String>,
    pub active_bias_tags: Vec<String>,
    pub total_resolved: i64,
    pub brier: Option<f64>,
    pub warnings: Vec<String>,
    /// Set when the run short-circuits: `"insufficient_data"` (cold brain) or
    /// `"budget_exhausted"`.
    pub skipped: Option<String>,
    pub status: CalibrationProfileStatus,
}

/// Run the calibration-profile phase for a holder (port of TS
/// `runPhaseCalibrationProfile`). Reads the scorecard, applies the cold-brain
/// and budget gates, generates pattern statements through the voice gate,
/// derives bias tags, aggregates domain scorecards (fail-soft), and writes the
/// `calibration_profiles` row via the typed [`CalibrationQueries`] method.
pub async fn run_calibration_profile(
    engine: &dyn BrainEngine,
    opts: &CalibrationProfileOpts,
) -> ZbResult<CalibrationProfileResult> {
    let holder = opts.holder.clone().unwrap_or_else(|| "garry".to_string());
    let model_id = opts
        .model_id
        .clone()
        .unwrap_or_else(|| DEFAULT_PATTERNS_MODEL.to_string());
    let grade_completion = opts.grade_completion.unwrap_or(1.0);

    let mut result = CalibrationProfileResult::default();

    // Resolve generators + judge. Production wiring passes `chat`; tests inject
    // stubs. Misconfiguration (no chat, no injection) is a hard error.
    let patterns_generator: Arc<dyn PatternStatementsGenerator> = match &opts.patterns_generator {
        Some(g) => g.clone(),
        None => match &opts.chat {
            Some(p) => Arc::new(ChatBackedPatternStatementsGenerator::new(
                p.clone(),
                model_id.clone(),
                opts.prompt_version
                    .clone()
                    .unwrap_or_else(|| CALIBRATION_PROFILE_PROMPT_VERSION.to_string()),
            )),
            None => {
                return Err(Error::engine(
                    "run_calibration_profile: no patterns_generator and no chat provider",
                ))
            }
        },
    };
    let bias_tags_generator: Arc<dyn BiasTagsGenerator> = match &opts.bias_tags_generator {
        Some(g) => g.clone(),
        None => match &opts.chat {
            Some(p) => Arc::new(ChatBackedBiasTagsGenerator::new(p.clone(), model_id.clone())),
            None => {
                return Err(Error::engine(
                    "run_calibration_profile: no bias_tags_generator and no chat provider",
                ))
            }
        },
    };
    let judge: Arc<dyn VoiceJudge> = match &opts.voice_gate_judge {
        Some(j) => j.clone(),
        None => match &opts.chat {
            Some(p) => Arc::new(LlmBackedVoiceJudge::new(p.clone(), DEFAULT_JUDGE_MODEL)),
            None => {
                return Err(Error::engine(
                    "run_calibration_profile: no voice_gate_judge and no chat provider",
                ))
            }
        },
    };
    let budget_gate: Arc<dyn BudgetGate> =
        opts.budget_gate.clone().unwrap_or_else(|| Arc::new(PermissiveBudgetGate));

    // Load the holder's scorecard.
    let scorecard = engine.get_scorecard(&ScorecardQuery::for_holder(&holder)).await?;
    result.total_resolved = scorecard.resolved;
    result.brier = scorecard.brier;

    // Cold-brain branch: not enough resolved takes for a profile yet.
    if scorecard.resolved < COLD_BRAIN_MIN_RESOLVED {
        result.skipped = Some("insufficient_data".to_string());
        result.status = CalibrationProfileStatus::Skipped;
        return Ok(result);
    }

    // Budget gate before invoking the LLM-driven gate.
    let budget = budget_gate.allowed(800, 500, &model_id);
    if !budget.allowed {
        result
            .warnings
            .push(format!("budget exhausted before profile generation (cap ${:.2})", budget.budget_usd));
        result.skipped = Some("budget_exhausted".to_string());
        result.status = CalibrationProfileStatus::Warn;
        return Ok(result);
    }

    // Generate pattern statements via the voice gate.
    let adapter = PatternStatementsVoiceAdapter {
        inner: patterns_generator.clone(),
        holder: holder.clone(),
        scorecard: scorecard.clone(),
    };
    let gated = gate_voice::<PatternStatementSlots>(VoiceGateOpts {
        mode: VoiceGateMode::PatternStatement,
        generator: Arc::new(adapter),
        judge,
        template_fallback: (Box::new(pattern_statement_template), pick_fallback_slots(&scorecard)),
        max_attempts: 2,
        rubric: None,
    })
    .await;
    result.voice_gate_passed = gated.passed;
    result.voice_gate_attempts = gated.attempts;
    result.pattern_statements = gated
        .text
        .split('\n')
        .map(|l| l.trim().to_string())
        .filter(|l| !l.is_empty())
        .collect();

    // Bias tags from the patterns (best-effort; failure is non-fatal).
    match bias_tags_generator
        .generate(BiasTagsGenInput {
            patterns: result.pattern_statements.clone(),
        })
        .await
    {
        Ok(tags) => result.active_bias_tags = tags,
        Err(e) => result.warnings.push(format!("bias_tags_generator failed: {}", e)),
    }

    // Domain scorecards (fail-soft). Empty {} when no active pack declares
    // calibration_domains (R1 byte-identical regression).
    let source_id = opts.source_id.clone().unwrap_or_else(|| "default".to_string());
    let domain_scorecards: serde_json::Value = match &opts.domains {
        Some(domains) if !domains.is_empty() => {
            match aggregate_domain_scorecards(engine, &holder, domains, &source_id).await {
                Ok(d) => serde_json::to_value(&d).unwrap_or_else(|_| json!({})),
                Err(e) => {
                    result
                        .warnings
                        .push(format!("domain_scorecards_aggregation_failed: {}", e));
                    json!({})
                }
            }
        }
        _ => json!({}),
    };

    // Write the profile row (typed CalibrationQueries method, no execute_raw).
    let insert = CalibrationProfileInsert {
        source_id: &source_id,
        holder: &holder,
        total_resolved: scorecard.resolved as i32,
        brier: scorecard.brier,
        accuracy: scorecard.accuracy,
        partial_rate: scorecard.partial_rate,
        grade_completion,
        domain_scorecards,
        pattern_statements: result.pattern_statements.clone(),
        voice_gate_passed: result.voice_gate_passed,
        voice_gate_attempts: result.voice_gate_attempts as i16,
        active_bias_tags: result.active_bias_tags.clone(),
        model_id: &model_id,
    };
    match engine.insert_calibration_profile(&insert).await {
        Ok(_) => result.profile_written = true,
        Err(e) => return Err(Error::engine(format!("insert_calibration_profile failed: {}", e))),
    }

    result.status = CalibrationProfileStatus::Ok;
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_pattern_statements_output_variants() {
        assert!(parse_pattern_statements_output("").is_empty());
        assert!(parse_pattern_statements_output("   ").is_empty());
        let out = parse_pattern_statements_output(
            "- You called tactics well.\n* Geography is your blind spot.\n3. Hiring was a miss.",
        );
        assert_eq!(
            out,
            vec![
                "You called tactics well.",
                "Geography is your blind spot.",
                "Hiring was a miss.",
            ]
        );
        // >4 lines truncated.
        let out = parse_pattern_statements_output("a\nb\nc\nd\ne");
        assert_eq!(out.len(), 4);
        // Over-long line dropped.
        let out = parse_pattern_statements_output(&"x".repeat(201));
        assert!(out.is_empty());
    }

    #[test]
    fn parse_bias_tags_output_variants() {
        assert!(parse_bias_tags_output("").is_empty());
        let out = parse_bias_tags_output("```json\n[\"over-confident-geography\", \"late-on-macro-tech\"]\n```");
        assert_eq!(out, vec!["over-confident-geography", "late-on-macro-tech"]);
        // Number and invalid-tag entries filtered; case-normalized.
        let out = parse_bias_tags_output("[\"Over-Confident-Geography\", 5, \"bad tag!\"]");
        assert_eq!(out, vec!["over-confident-geography"]);
        let out = parse_bias_tags_output("no array here");
        assert!(out.is_empty());
        // >4 truncated.
        let out = parse_bias_tags_output("[\"a-b\",\"c-d\",\"e-f\",\"g-h\",\"i-j\"]");
        assert_eq!(out.len(), 4);
    }

    #[test]
    fn pick_fallback_slots_variants() {
        let empty = TakesScorecard {
            total_bets: 0,
            resolved: 0,
            correct: 0,
            incorrect: 0,
            partial: 0,
            accuracy: None,
            brier: None,
            partial_rate: None,
            unresolvable_count: None,
            unresolvable_rate: None,
        };
        let s = pick_fallback_slots(&empty);
        assert_eq!(s.domain, "overall");
        assert_eq!(s.n_right, 0);
        assert_eq!(s.direction, None);

        let high = TakesScorecard {
            total_bets: 10,
            resolved: 10,
            correct: 7,
            incorrect: 3,
            partial: 0,
            accuracy: Some(0.7),
            brier: Some(0.3),
            partial_rate: Some(0.0),
            unresolvable_count: None,
            unresolvable_rate: None,
        };
        let s = pick_fallback_slots(&high);
        assert_eq!(s.direction, Some("over-confident".to_string()));

        let low = TakesScorecard {
            total_bets: 10,
            resolved: 10,
            correct: 8,
            incorrect: 2,
            partial: 0,
            accuracy: Some(0.8),
            brier: Some(0.1),
            partial_rate: Some(0.0),
            unresolvable_count: None,
            unresolvable_rate: None,
        };
        let s = pick_fallback_slots(&low);
        assert_eq!(s.direction, Some("mostly right".to_string()));
    }
}
