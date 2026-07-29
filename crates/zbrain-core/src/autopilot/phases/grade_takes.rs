//! Part12 1-1-5 — port of `src/core/cycle/grade-takes.ts` →
//! `autopilot/phases/grade_takes.rs`.
//!
//! Walks unresolved active takes that are old enough to have outcome data,
//! asks a judge model to verdict each one (`correct`/`incorrect`/`partial`/
//! `unresolvable`), and writes the verdicts to the `take_grade_cache` verdict
//! cache. When the operator has flipped the opt-in `auto_resolve` flag,
//! high-confidence verdicts are also applied to the canonical `takes` table via
//! `engine.resolve_take` (D17 — auto-resolve OFF by default).
//!
//! Porting notes vs the TS source:
//! - **Judge IS the injected `&dyn ChatProvider`.** TS `defaultJudge` calls
//!   `gateway.chat`; this port routes the same prompt through `chat` (mirrors
//!   `extract_atoms.rs` / `propose_takes.rs`). Tests install `MockChatProvider`.
//! - **Idempotency via typed engine methods, not raw SQL.** TS uses
//!   `engine.executeRaw` against `take_grade_cache`. Per the zbrain-core design
//!   intent (no `execute_raw` escapes — see `engine.rs`), this port uses
//!   [`BrainEngine::take_grade_cache_exists`] + [`BrainEngine::add_take_grade_cache`]
//!   (implemented on all three engines). The composite PK
//!   `(take_id, prompt_version, judge_model_id, evidence_signature)` keeps the
//!   INSERT conflict-safe.
//! - **Evidence retriever is a placeholder.** TS v0.36.1.0 ship-state documents
//!   that hybrid evidence retrieval is "not yet wired"; the default retriever
//!   returns the take's own claim text as the only evidence. The seam (`opts
//!   .evidence_retriever`) is kept so real retrieval can land later.
//! - **gstack-learnings coupling is intentionally NOT ported.** It is an opt-in
//!   external side effect (`cycle.grade_takes.write_gstack_learnings`, default
//!   false) and the downstream `gstack` binary is not part of zbrain-core; the
//!   verdict + auto-apply loop is the faithful core.
//! - **Budget is a lightweight cumulative estimate** (same token-price model as
//!   `extract_atoms`/`propose_takes`), not the full `BaseCyclePhase.checkBudget`
//!   harness. It only gates whether we stop early; it does not block writes.

use chrono::{DateTime, Utc};
use sha2::{Digest, Sha256};

use crate::ai::chat::{ChatMessage, ChatOpts, ChatProvider, ChatRole};
use crate::engine::{BrainEngine, TakeGradeCacheInput};
use crate::error::Result as ZbResult;
use crate::types::{Take, TakeResolution, TakesListOpts};

/// Bump when the judge prompt or the JSON output shape changes. Old verdicts in
/// `take_grade_cache` (composite key includes prompt_version) stay valid as
/// audit history; new runs re-spend LLM tokens.
pub const GRADE_TAKES_PROMPT_VERSION: &str = "v0.36.1.0-stub";

const GRADE_TAKE_PROMPT: &str = r#"[v0.36.1.0-stub] You are grading a single forecasting take. The author
made this claim on the given date. Based on the evidence provided, did the
claim turn out to be:
- correct        (the world plays out as predicted)
- incorrect      (the world clearly contradicts the prediction)
- partial        (some aspects right, some wrong; or right direction wrong magnitude)
- unresolvable   (insufficient evidence; outcome still pending)

Output ONLY one JSON object with these fields:
- verdict        ('correct' | 'incorrect' | 'partial' | 'unresolvable')
- confidence     (number in [0,1]) — your self-reported confidence in this verdict.
- reasoning      (string, <=400 chars) — one short paragraph explaining what evidence drove the verdict.

If the evidence is sparse or ambiguous, return verdict='unresolvable' with
confidence reflecting the lack of evidence (NOT certainty of unresolvable).

TAKE:
  Claim:    {CLAIM}
  Kind:     {KIND}
  Holder:   {HOLDER}
  Made on:  {SINCE_DATE}
  Weight:   {WEIGHT}

EVIDENCE:
{EVIDENCE_BLOCK}
"#;

const VALID_VERDICTS: &[&str] = &["correct", "incorrect", "partial", "unresolvable"];

/// A single judge verdict. Mirrors TS `JudgeVerdict`.
#[derive(Debug, Clone, PartialEq)]
pub struct JudgeVerdict {
    pub verdict: String,
    pub confidence: f64,
    pub reasoning: String,
}

/// A single model's ensemble verdict contribution. Mirrors TS
/// `EnsembleVerdict['modelVerdicts']`.
#[derive(Debug, Clone)]
pub struct EnsembleModelVerdict {
    pub model_id: String,
    pub verdict: String,
    pub confidence: f64,
    pub failed: bool,
}

/// Aggregate per-model verdicts into a single ensemble verdict. Pure function.
/// Mirrors TS `aggregateEnsemble`.
///
/// Algorithm: tally non-failed verdicts; winner = most-voted label with
/// tie-break preferring non-unresolvable then alphabetical; `agreement` = count
/// of models returning the winning label; `min_confidence` = MIN confidence
/// across the models that returned the winning label.
#[derive(Debug, Clone)]
pub struct EnsembleVerdict {
    pub verdict: String,
    pub min_confidence: f64,
    pub agreement: u32,
    pub model_verdicts: Vec<EnsembleModelVerdict>,
}

pub fn aggregate_ensemble(
    results: &[(String, Option<JudgeVerdict>)],
) -> EnsembleVerdict {
    let model_verdicts: Vec<EnsembleModelVerdict> = results
        .iter()
        .map(|(model_id, v)| match v {
            Some(jv) => EnsembleModelVerdict {
                model_id: model_id.clone(),
                verdict: jv.verdict.clone(),
                confidence: jv.confidence,
                failed: false,
            },
            None => EnsembleModelVerdict {
                model_id: model_id.clone(),
                verdict: "unresolvable".to_string(),
                confidence: 0.0,
                failed: true,
            },
        })
        .collect();

    // Tally only the non-failed verdicts.
    let mut tally: std::collections::HashMap<String, u32> = std::collections::HashMap::new();
    for (_, v) in results {
        if let Some(jv) = v {
            *tally.entry(jv.verdict.clone()).or_insert(0) += 1;
        }
    }

    // Pick the winner. Tie-break: prefer non-unresolvable, then alphabetical.
    let mut winner = "unresolvable".to_string();
    let mut best_count: u32 = 0;
    for (v, n) in &tally {
        if *n > best_count {
            winner = v.clone();
            best_count = *n;
        } else if *n == best_count {
            if winner == "unresolvable" && v != "unresolvable" {
                winner = v.clone();
            } else if v != "unresolvable" && winner != "unresolvable" && v < &winner {
                winner = v.clone();
            }
        }
    }

    // min_confidence + agreement: across models that returned the winning label.
    let mut min_confidence = 1.0f64;
    let mut agreement: u32 = 0;
    for (_, v) in results {
        if let Some(jv) = v {
            if jv.verdict == winner {
                agreement += 1;
                if jv.confidence < min_confidence {
                    min_confidence = jv.confidence;
                }
            }
        }
    }
    if agreement == 0 {
        min_confidence = 0.0;
    }

    EnsembleVerdict {
        verdict: winner,
        min_confidence,
        agreement,
        model_verdicts,
    }
}

/// Compute the evidence_signature for the cache. SHA-256 of
/// `judge_model_id + '|' + evidence` keeps cache invalidation honest: a re-run
/// with new evidence OR a different judge produces a fresh row.
pub fn evidence_signature(evidence: &str, judge_model_id: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(judge_model_id.as_bytes());
    hasher.update(b"|");
    hasher.update(evidence.as_bytes());
    let digest = hasher.finalize();
    digest.iter().map(|b| format!("{:02x}", b)).collect()
}

/// Parse the judge model's JSON output. Tolerant of fence wrapping and leading
/// prose; returns `None` on unrecoverable parse failure. Mirrors TS
/// `parseJudgeOutput`.
pub fn parse_judge_output(raw: &str) -> Option<JudgeVerdict> {
    if raw.trim().is_empty() {
        return None;
    }
    let mut text = raw.trim().to_string();
    if let Some(stripped) = text.strip_prefix("```") {
        let inner = stripped.trim_start_matches("json").trim_start();
        if let Some(end) = inner.find("```") {
            text = inner[..end].trim().to_string();
        }
    }
    let first_obj = text.find('{')?;
    let parsed: serde_json::Value = serde_json::from_str(&text[first_obj..]).ok()?;
    let obj = parsed.as_object()?;
    let verdict = obj.get("verdict").and_then(|v| v.as_str())?;
    if !VALID_VERDICTS.contains(&verdict) {
        return None;
    }
    let conf_raw = match obj.get("confidence") {
        Some(serde_json::Value::Number(n)) => n.as_f64(),
        Some(serde_json::Value::String(s)) => s.parse::<f64>().ok(),
        _ => None,
    }?;
    if !conf_raw.is_finite() {
        return None;
    }
    let confidence = conf_raw.clamp(0.0, 1.0);
    let reasoning = obj
        .get("reasoning")
        .and_then(|v| v.as_str())
        .map(|s| s.chars().take(400).collect())
        .unwrap_or_default();
    Some(JudgeVerdict {
        verdict: verdict.to_string(),
        confidence,
        reasoning,
    })
}

/// Default evidence retriever — v0.36.1.0 ship-state placeholder. Real
/// retrieval lands later via hybrid search over pages newer than the take's
/// `since_date`. Documented limitation per CDX-8 + D17.
pub fn default_evidence_retriever(take: &Take) -> String {
    format!(
        "[evidence retrieval not yet wired — v0.36.1.0 ship-state]\nTake claim text (the only \"evidence\" available pre-T-retrieval-impl):\n  {}\nMade on: {}",
        take.claim,
        take.since_date.as_deref().unwrap_or("unknown"),
    )
}

/// Determine whether a take is old enough to grade. Defaults to 6 months.
/// Takes without `since_date` are NOT graded (we'd be hallucinating context) —
/// callers count these as `too_recent`.
pub fn take_is_old_enough(take: &Take, min_age_months: u32, now: DateTime<Utc>) -> bool {
    let since = match &take.since_date {
        None => return false,
        Some(s) => s,
    };
    // Tolerant date parsing — since_date can be YYYY-MM-DD or YYYY-MM.
    let since_str = if since.len() == 7 {
        format!("{}-15", since)
    } else {
        since.clone()
    };
    let since_date = match DateTime::parse_from_rfc3339(&since_str)
        .map(|dt| dt.with_timezone(&Utc))
        .or_else(|_| {
            chrono::NaiveDate::parse_from_str(&since_str, "%Y-%m-%d").map(|d| {
                DateTime::<Utc>::from_naive_utc_and_offset(d.and_hms_opt(0, 0, 0).unwrap(), Utc)
            })
        }) {
        Ok(dt) => dt,
        Err(_) => return false,
    };
    let cutoff = now - chrono::Months::new(min_age_months as u32);
    since_date <= cutoff
}

fn verdict_to_resolution(verdict: &JudgeVerdict, resolved_by_label: &str, prompt_version: &str) -> Option<TakeResolution> {
    if verdict.verdict == "unresolvable" {
        return None;
    }
    Some(TakeResolution {
        page_id: 0,
        row_num: 0,
        quality: Some(verdict.verdict.clone()),
        outcome: None,
        evidence: Some(format!("grade_takes:{}", prompt_version)),
        value: None,
        unit: None,
        by: Some(resolved_by_label.to_string()),
    })
}

/// Options for [`run_grade_takes`]. Mirrors TS `GradeTakesOpts`.
#[derive(Debug, Clone)]
pub struct GradeTakesOpts {
    /// Minimum age in months before a take is eligible for grading. Default 6.
    pub min_age_months: u32,
    /// Limit takes processed in this cycle. Default 50.
    pub take_limit: u32,
    /// Auto-resolve verdicts above the confidence threshold. D17 default: false.
    pub auto_resolve: bool,
    /// Confidence threshold for auto-resolve. D12 default: 0.95.
    pub auto_resolve_threshold: f64,
    /// Judge model id; defaults to the configured chat model.
    pub model: Option<String>,
    /// Override prompt_version (tests).
    pub prompt_version: Option<String>,
    /// Identifier recorded as resolved_by when auto-applying. Default 'zbrain:grade_takes'.
    pub resolved_by_label: String,
    /// E2 ensemble (T5): when true, borderline single-model verdicts fire a
    /// multi-judge ensemble tiebreaker. Default false (single-model only).
    pub use_ensemble: bool,
    /// E2 ensemble auto-apply threshold. Default 0.85 (D12 conservative).
    pub ensemble_threshold: f64,
    /// E2 ensemble TRIGGER band [lower, upper). Single-model verdicts whose
    /// confidence falls in this band invoke the ensemble. Default [0.6, 0.95).
    pub ensemble_trigger_band: (f64, f64),
    /// E2 ensemble judge model ids. When `use_ensemble` is true and the
    /// single-model verdict is borderline, each model is judged in turn via the
    /// injected `chat` provider. Tests inject deterministic model ids.
    pub ensemble_judges: Vec<String>,
    /// Inject the evidence retriever (tests / future real retrieval).
    pub evidence_retriever: Option<fn(&Take) -> String>,
}

impl Default for GradeTakesOpts {
    fn default() -> Self {
        Self {
            min_age_months: 6,
            take_limit: 50,
            auto_resolve: false,
            auto_resolve_threshold: 0.95,
            model: None,
            prompt_version: None,
            resolved_by_label: "zbrain:grade_takes".to_string(),
            use_ensemble: false,
            ensemble_threshold: 0.85,
            ensemble_trigger_band: (0.6, 0.95),
            ensemble_judges: Vec::new(),
            evidence_retriever: None,
        }
    }
}

/// Result of a single `grade_takes` run. Mirrors TS `GradeTakesResult`.
#[derive(Debug, Clone, Default, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GradeTakesResult {
    pub takes_scanned: u64,
    pub cache_hits: u64,
    pub verdicts_written: u64,
    pub auto_applied: u64,
    pub too_recent: u64,
    pub budget_exhausted: bool,
    pub warnings: Vec<String>,
    /// E2 ensemble (T5): count of takes where the ensemble tiebreaker fired.
    pub ensemble_invoked: u64,
    /// E2 ensemble (T5): count of takes where ensemble produced 3/3 unanimous.
    pub ensemble_unanimous: u64,
}

/// Default per-cycle LLM budget in USD (mirrors TS `budgetUsdDefault = 3.0`).
const DEFAULT_BUDGET_USD: f64 = 3.0;

/// Build the judge prompt by substituting the take fields + evidence.
fn build_prompt(take: &Take, evidence: &str) -> String {
    GRADE_TAKE_PROMPT
        .replace("{CLAIM}", &take.claim)
        .replace("{KIND}", &take.kind)
        .replace("{HOLDER}", &take.holder)
        .replace("{SINCE_DATE}", take.since_date.as_deref().unwrap_or("unknown"))
        .replace("{WEIGHT}", &take.weight.to_string())
        .replace("{EVIDENCE_BLOCK}", evidence)
}

/// Run the grade-takes phase.
///
/// - Lists unresolved active takes (`list_takes`, oldest-first by `since_date`).
/// - For each take: checks `take_is_old_enough`; retrieves evidence; checks the
///   idempotency cache via `take_grade_cache_exists`; on a miss, calls the
///   judge, parses the verdict, and writes it via `add_take_grade_cache`
///   (conflict-safe). A single-take judge error logs a warning and continues.
/// - When `auto_resolve` is on and the verdict clears the threshold, the verdict
///   is applied to the canonical take via `engine.resolve_take`.
pub async fn run_grade_takes(
    engine: &dyn BrainEngine,
    chat: &dyn ChatProvider,
    opts: &GradeTakesOpts,
) -> ZbResult<GradeTakesResult> {
    let prompt_version = opts
        .prompt_version
        .clone()
        .unwrap_or_else(|| GRADE_TAKES_PROMPT_VERSION.to_string());
    let judge_model_id = opts
        .model
        .clone()
        .unwrap_or_else(|| "claude-sonnet-4-6".to_string());
    let resolved_by_label = &opts.resolved_by_label;

    let mut result = GradeTakesResult::default();
    let mut estimated_spend = 0.0f64;

    // Load unresolved active takes. TS sorts by since_date ascending
    // (oldest first); list_takes has no sort param, so we sort post-fetch.
    let mut takes = engine
        .list_takes(&TakesListOpts {
            active: Some(true),
            resolved: Some(false),
            limit: Some(opts.take_limit),
            ..Default::default()
        })
        .await?;
    takes.sort_by(|a, b| a.since_date.cmp(&b.since_date));

    let now = Utc::now();
    let evidence_retriever = opts.evidence_retriever.unwrap_or(default_evidence_retriever);

    for take in &takes {
        result.takes_scanned += 1;

        if !take_is_old_enough(take, opts.min_age_months, now) {
            result.too_recent += 1;
            continue;
        }

        // Retrieve evidence first — the signature depends on it.
        let evidence = evidence_retriever(take);
        let sig = evidence_signature(&evidence, &judge_model_id);

        // Idempotency: skip when the exact cache tuple exists.
        match engine
            .take_grade_cache_exists(take.id, &prompt_version, &judge_model_id, &sig)
            .await
        {
            Ok(true) => {
                result.cache_hits += 1;
                continue;
            }
            Ok(false) => {}
            Err(e) => {
                result.warnings.push(format!(
                    "take_grade_cache_exists failed on take {}: {}",
                    take.id, e
                ));
            }
        }

        // Budget pre-check (lightweight cumulative estimate, mirrors TS).
        if estimated_spend >= DEFAULT_BUDGET_USD {
            result.budget_exhausted = true;
            result.warnings.push(format!(
                "budget exhausted at take {}/{} (cumulative ${:.4} / cap ${:.2})",
                result.takes_scanned,
                takes.len(),
                estimated_spend,
                DEFAULT_BUDGET_USD
            ));
            break;
        }

        // Call the single-model judge. Errors on a single take log warning + continue.
        let mut verdict = match judge_call(chat, &judge_model_id, take, &evidence).await {
            Ok(v) => v,
            Err(e) => {
                result
                    .warnings
                    .push(format!("judge failed on take {}: {}", take.id, e));
                continue;
            }
        };
        estimated_spend += PER_VERDICT_COST;

        // T5 — ensemble tiebreaker for borderline single-model verdicts.
        let mut recorded_judge_model_id = judge_model_id.clone();
        let mut ensemble_apply_eligible = false;
        let (band_lo, band_hi) = opts.ensemble_trigger_band;
        let in_borderline_band = verdict.confidence >= band_lo
            && verdict.confidence < band_hi
            && verdict.verdict != "unresolvable";

        if opts.use_ensemble
            && in_borderline_band
            && !opts.ensemble_judges.is_empty()
        {
            result.ensemble_invoked += 1;
            let mut collected: Vec<(String, Option<JudgeVerdict>)> = Vec::new();
            for model_id in &opts.ensemble_judges {
                let v = judge_call(chat, model_id, take, &evidence).await.ok();
                collected.push((model_id.clone(), v));
            }
            let ensemble = aggregate_ensemble(&collected);

            // Record the ensemble verdict in the cache row instead of the
            // single-model verdict. The judge_model_id becomes
            // 'ensemble:<a>+<b>+<c>' so a future re-run with different ensemble
            // membership doesn't collide.
            recorded_judge_model_id = format!("ensemble:{}", opts.ensemble_judges.join("+"));
            verdict = JudgeVerdict {
                verdict: ensemble.verdict.clone(),
                confidence: ensemble.min_confidence,
                reasoning: format!(
                    "ensemble agreement {}/3; per-model: {}",
                    ensemble.agreement,
                    ensemble
                        .model_verdicts
                        .iter()
                        .map(|m| format!(
                            "{}={}{}{}",
                            m.model_id,
                            m.verdict,
                            if m.confidence.is_finite() {
                                format!("@{:.2}", m.confidence)
                            } else {
                                String::new()
                            },
                            if m.failed { "(failed)" } else { "" }
                        ))
                        .collect::<Vec<_>>()
                        .join(", ")
                ),
            };
            if ensemble.agreement == 3 {
                result.ensemble_unanimous += 1;
            }

            ensemble_apply_eligible = ensemble.agreement == 3
                && ensemble.min_confidence >= opts.ensemble_threshold
                && ensemble.verdict != "unresolvable";
        }

        // Decide auto-resolve eligibility BEFORE writing to cache so `applied`
        // reflects the decision. Two paths:
        //   - Ensemble path: requires 3/3 unanimous + min conf >= ensembleThreshold
        //   - Single-model path: requires confidence >= autoResolveThreshold
        // 'unresolvable' NEVER auto-applies either way.
        let resolution = verdict_to_resolution(&verdict, resolved_by_label, &prompt_version);
        let mut should_apply = false;
        if opts.auto_resolve && resolution.is_some() {
            if recorded_judge_model_id.starts_with("ensemble:") {
                should_apply = ensemble_apply_eligible;
            } else {
                should_apply = verdict.confidence >= opts.auto_resolve_threshold;
            }
        }

        // Compute a NEW evidence_signature when ensemble fires, since the cache
        // composite key includes judge_model_id.
        let recorded_sig = if recorded_judge_model_id == judge_model_id {
            sig.clone()
        } else {
            evidence_signature(&evidence, &recorded_judge_model_id)
        };

        // Write the verdict to the cache (conflict-safe).
        let written = engine
            .add_take_grade_cache(&TakeGradeCacheInput {
                take_id: take.id,
                prompt_version: prompt_version.clone(),
                judge_model_id: recorded_judge_model_id.clone(),
                evidence_signature: recorded_sig,
                wave_version: "v0.36.1.0".to_string(),
                verdict: verdict.verdict.clone(),
                confidence: verdict.confidence,
                applied: should_apply,
                cost_usd: Some(PER_VERDICT_COST),
            })
            .await?;
        if written > 0 {
            result.verdicts_written += 1;
        }

        // Apply to canonical takes if eligible.
        if should_apply {
            if let Some(mut res) = resolution {
                res.page_id = take.page_id;
                res.row_num = take.row_num;
                match engine.resolve_take(take.page_id, take.row_num, &res).await {
                    Ok(()) => {
                        result.auto_applied += 1;
                    }
                    Err(e) => result
                        .warnings
                        .push(format!("auto-apply failed on take {}: {}", take.id, e)),
                }
            }
        }
    }

    Ok(result)
}

/// Call the judge for one take and return the parsed verdict. A failed parse
/// surfaces as `unresolvable @ 0.0` (so the row still lands in the cache
/// rather than disappearing silently) — mirrors TS `defaultJudge`.
async fn judge_call(
    chat: &dyn ChatProvider,
    model_id: &str,
    take: &Take,
    evidence: &str,
) -> ZbResult<JudgeVerdict> {
    let prompt = build_prompt(take, evidence);
    let chat_result = chat
        .chat(ChatOpts {
            model: Some(model_id.to_string()),
            system: None,
            messages: vec![ChatMessage::text(ChatRole::User, prompt)],
            tools: vec![],
            max_tokens: Some(600),
            cache_system: false,
        })
        .await
        .map_err(|e| crate::error::Error::engine(format!("grade_takes judge chat: {e}")))?;
    match parse_judge_output(&chat_result.text) {
        Some(v) => Ok(v),
        None => Ok(JudgeVerdict {
            verdict: "unresolvable".to_string(),
            confidence: 0.0,
            reasoning: "judge_output_parse_failed".to_string(),
        }),
    }
}

/// Rough per-verdict token-cost estimate (mirrors propose_takes: ~1200 in /
/// ~400 out against the 0.8/4.0 USD-per-Mtok price model). Used for the
/// lightweight budget meter and recorded in `take_grade_cache.cost_usd`.
/// Per-verdict cost = (1200*0.8 + 400*4.0)/1e6 = 0.00256 USD.
const PER_VERDICT_COST: f64 = 0.00256;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ai::chat::MockChatProvider;
    use crate::engine::{EngineConfig, InMemoryEngine};
    use crate::types::TakeInput;

    async fn setup() -> InMemoryEngine {
        let e = InMemoryEngine::new();
        e.connect(&EngineConfig::default()).await.unwrap();
        e
    }

    async fn put_take(e: &InMemoryEngine, claim: &str, since_date: &str, row_num: i32) -> u64 {
        e.add_takes_batch(
            1,
            &[TakeInput {
                page_id: 1,
                row_num: Some(row_num),
                claim: claim.to_string(),
                kind: "take".to_string(),
                holder: "brain".to_string(),
                weight: 0.7,
                since_date: Some(since_date.to_string()),
                until_date: None,
                source: None,
                superseded_by: None,
                active: Some(true),
            }],
        )
        .await
        .unwrap();
        // InMemory upsert assigns sequential ids from 1; look up the just-inserted
        // take by (page_id, row_num) to return its real id.
        let takes = e
            .list_takes(&TakesListOpts {
                page_id: Some(1),
                ..Default::default()
            })
            .await
            .unwrap();
        takes.into_iter().find(|t| t.row_num == row_num).unwrap().id
    }

    // A judge that always returns `correct @ 0.99`.
    fn correct_judge_json() -> String {
        r#"{"verdict":"correct","confidence":0.99,"reasoning":"the market played out as predicted"}"#.to_string()
    }

    #[tokio::test]
    async fn run_grade_takes_empty_inmemory() {
        let engine = setup().await;
        let chat = MockChatProvider::new(correct_judge_json());
        let result = run_grade_takes(
            &engine,
            &chat,
            &GradeTakesOpts {
                ..Default::default()
            },
        )
        .await
        .unwrap();
        assert_eq!(result.takes_scanned, 0);
        assert_eq!(result.verdicts_written, 0);
        assert!(!result.budget_exhausted);
    }

    #[tokio::test]
    async fn run_grade_takes_grades_old_take_and_writes_cache() {
        let engine = setup().await;
        // A take made 24 months ago is old enough (minAgeMonths default 6).
        let id = put_take(&engine, "alice will hit ARR milestone by Q3", "2024-01-15", 1).await;
        let chat = MockChatProvider::new(correct_judge_json());
        let result = run_grade_takes(
            &engine,
            &chat,
            &GradeTakesOpts {
                ..Default::default()
            },
        )
        .await
        .unwrap();
        assert_eq!(result.takes_scanned, 1);
        assert_eq!(result.too_recent, 0);
        assert_eq!(result.cache_hits, 0);
        assert_eq!(result.verdicts_written, 1);
        assert_eq!(result.auto_applied, 0); // auto_resolve default OFF

        // Cache row exists now.
        let exists = engine
            .take_grade_cache_exists(
                id,
                GRADE_TAKES_PROMPT_VERSION,
                "claude-sonnet-4-6",
                &evidence_signature(
                    &default_evidence_retriever(&get_take(&engine, id).await),
                    "claude-sonnet-4-6",
                ),
            )
            .await
            .unwrap();
        assert!(exists);
    }

    #[tokio::test]
    async fn run_grade_takes_too_recent_not_graded() {
        let engine = setup().await;
        // A take made 1 month ago is too recent.
        put_take(&engine, "bob will ship v2 soon", "2026-06-15", 1).await;
        let chat = MockChatProvider::new(correct_judge_json());
        let result = run_grade_takes(
            &engine,
            &chat,
            &GradeTakesOpts {
                ..Default::default()
            },
        )
        .await
        .unwrap();
        assert_eq!(result.takes_scanned, 1);
        assert_eq!(result.too_recent, 1);
        assert_eq!(result.verdicts_written, 0);
    }

    #[tokio::test]
    async fn run_grade_takes_idempotent_on_rerun() {
        let engine = setup().await;
        put_take(&engine, "carol will raise a round", "2023-01-15", 1).await;
        let chat = MockChatProvider::new(correct_judge_json());
        let _ = run_grade_takes(&engine, &chat, &GradeTakesOpts::default()).await.unwrap();
        // Re-run with a cache-hitting chat (doesn't matter what it returns).
        let chat2 = MockChatProvider::new(correct_judge_json());
        let result = run_grade_takes(&engine, &chat2, &GradeTakesOpts::default())
            .await
            .unwrap();
        assert_eq!(result.cache_hits, 1);
        assert_eq!(result.verdicts_written, 0);
    }

    #[tokio::test]
    async fn run_grade_takes_auto_resolve_applies() {
        let engine = setup().await;
        let id = put_take(&engine, "dave will win the deal", "2023-01-15", 1).await;
        let chat = MockChatProvider::new(correct_judge_json());
        let result = run_grade_takes(
            &engine,
            &chat,
            &GradeTakesOpts {
                auto_resolve: true,
                auto_resolve_threshold: 0.95,
                ..Default::default()
            },
        )
        .await
        .unwrap();
        assert_eq!(result.verdicts_written, 1);
        assert_eq!(result.auto_applied, 1);

        // The canonical take is now resolved.
        let takes = engine
            .list_takes(&TakesListOpts {
                page_id: Some(1),
                ..Default::default()
            })
            .await
            .unwrap();
        let t = takes.iter().find(|t| t.id == id).unwrap();
        assert_eq!(t.resolved_quality.as_deref(), Some("correct"));
        assert_eq!(t.resolved_outcome, Some(true));
        assert_eq!(t.resolved_by.as_deref(), Some("zbrain:grade_takes"));
    }

    #[tokio::test]
    async fn run_grade_takes_auto_resolve_below_threshold_no_apply() {
        let engine = setup().await;
        put_take(&engine, "erin will expand to EU", "2023-01-15", 1).await;
        // verdict confidence 0.99 but threshold raised to 0.999 → no apply.
        let chat = MockChatProvider::new(correct_judge_json());
        let result = run_grade_takes(
            &engine,
            &chat,
            &GradeTakesOpts {
                auto_resolve: true,
                auto_resolve_threshold: 0.999,
                ..Default::default()
            },
        )
        .await
        .unwrap();
        assert_eq!(result.verdicts_written, 1);
        assert_eq!(result.auto_applied, 0);
    }

    async fn get_take(engine: &InMemoryEngine, id: u64) -> Take {
        let takes = engine
            .list_takes(&TakesListOpts {
                ..Default::default()
            })
            .await
            .unwrap();
        takes.into_iter().find(|t| t.id == id).unwrap()
    }

    #[test]
    fn parse_judge_output_variants() {
        assert!(parse_judge_output("").is_none());
        assert!(parse_judge_output("no json").is_none());
        let v = parse_judge_output(
            r#"{"verdict":"incorrect","confidence":0.8,"reasoning":"world contradicted"}"#,
        )
        .unwrap();
        assert_eq!(v.verdict, "incorrect");
        assert!((v.confidence - 0.8).abs() < 1e-9);
        // invalid verdict → None
        assert!(parse_judge_output(r#"{"verdict":"bogus","confidence":0.5}"#).is_none());
        // fenced wrapper tolerated
        let w = parse_judge_output("```json\n{\"verdict\":\"partial\",\"confidence\":0.6}\n```").unwrap();
        assert_eq!(w.verdict, "partial");
        // confidence clamped to [0,1]
        let c = parse_judge_output(r#"{"verdict":"correct","confidence":5.0}"#).unwrap();
        assert!((c.confidence - 1.0).abs() < 1e-9);
    }

    #[test]
    fn take_is_old_enough_logic() {
        let now = Utc::now();
        let old = Take {
            id: 1,
            page_id: 1,
            row_num: 1,
            claim: "x".into(),
            kind: "take".into(),
            holder: "brain".into(),
            weight: 0.5,
            since_date: Some("2020-01-15".into()),
            until_date: None,
            source: None,
            superseded_by: None,
            active: true,
            resolved_at: None,
            resolved_quality: None,
            resolved_outcome: None,
            resolved_evidence: None,
            resolved_value: None,
            resolved_unit: None,
            resolved_by: None,
            created_at: "2020-01-15T00:00:00Z".into(),
            updated_at: "2020-01-15T00:00:00Z".into(),
        };
        assert!(take_is_old_enough(&old, 6, now));
        let recent = Take {
            since_date: Some("2099-01-15".into()),
            ..old.clone()
        };
        assert!(!take_is_old_enough(&recent, 6, now));
        let no_date = Take {
            since_date: None,
            ..old.clone()
        };
        assert!(!take_is_old_enough(&no_date, 6, now));
    }

    #[test]
    fn aggregate_ensemble_logic() {
        // 3/3 unanimous correct @ min 0.9
        let r = aggregate_ensemble(&[
            ("a".into(), Some(JudgeVerdict { verdict: "correct".into(), confidence: 0.9, reasoning: "".into() })),
            ("b".into(), Some(JudgeVerdict { verdict: "correct".into(), confidence: 0.95, reasoning: "".into() })),
            ("c".into(), Some(JudgeVerdict { verdict: "correct".into(), confidence: 0.92, reasoning: "".into() })),
        ]);
        assert_eq!(r.verdict, "correct");
        assert_eq!(r.agreement, 3);
        assert!((r.min_confidence - 0.9).abs() < 1e-9);

        // One unresolvable drops agreement → winner still correct with agreement 2
        let r2 = aggregate_ensemble(&[
            ("a".into(), Some(JudgeVerdict { verdict: "correct".into(), confidence: 0.9, reasoning: "".into() })),
            ("b".into(), Some(JudgeVerdict { verdict: "correct".into(), confidence: 0.95, reasoning: "".into() })),
            ("c".into(), None),
        ]);
        assert_eq!(r2.verdict, "correct");
        assert_eq!(r2.agreement, 2);
        assert!(r2.model_verdicts[2].failed);
    }

    #[test]
    fn evidence_signature_stable_and_distinct() {
        assert_eq!(
            evidence_signature("ev", "m1"),
            evidence_signature("ev", "m1")
        );
        assert_ne!(
            evidence_signature("ev", "m1"),
            evidence_signature("ev", "m2")
        );
    }
}
