//! takes-quality-eval/receipt — stable JSON shape for one eval run, plus the
//! 4-sha receipt-naming contract and DB-authoritative persistence.
//!
//! Faithful port of TS `receipt.ts` + `receipt-name.ts` + `receipt-write.ts`.
//! `schema_version: 1` is a one-way-door contract: rename fields → bump
//! schema_version; adding optional fields is additive and compatible.

use std::collections::BTreeMap;
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::engine::BrainEngine;
use crate::eval::cross_modal::{DimensionRoll as CrossDimensionRoll, FailReason, Verdict};
use crate::eval::takes_quality::rubric::RUBRIC_VERSION;

pub const RECEIPT_SCHEMA_VERSION: u8 = 1;

/// The 4-sha receipt-naming contract. Two runs over the same corpus + same
/// rubric produce the same key, AND a future rubric tweak produces a different
/// key (no silent corruption of trend graphs).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReceiptIdentity {
    pub corpus_sha8: String,
    pub prompt_sha8: String,
    pub models_sha8: String,
    pub rubric_sha8: String,
}

/// Stable 8-char fingerprint over the joined corpus content.
pub fn corpus_sha8(takes_text: &str) -> String {
    crate::eval::cross_modal::sha8(takes_text)
}

/// Stable 8-char fingerprint over the model set. Sorted before hashing so
/// (`['a','b']`) and (`['b','a']`) produce the same sha — model order in the
/// slots array doesn't change identity.
pub fn model_set_sha8(model_ids: &[String]) -> String {
    let mut sorted: Vec<&str> = model_ids.iter().map(|s| s.as_str()).collect();
    sorted.sort();
    let canonical = serde_json::to_string(&sorted).unwrap_or_default();
    crate::eval::cross_modal::sha8(&canonical)
}

/// Build the receipt filename (no path, no extension stripping).
pub fn build_receipt_filename(id: &ReceiptIdentity) -> String {
    format!(
        "takes-quality-{}-{}-{}-{}.json",
        id.corpus_sha8, id.prompt_sha8, id.models_sha8, id.rubric_sha8
    )
}

/// Strip the receipt filename to recover identity components.
pub fn parse_receipt_filename(filename: &str) -> Option<ReceiptIdentity> {
    let name = filename.rsplit('/').next().unwrap_or(filename);
    let caps = regex_lite_safe(name)?;
    Some(ReceiptIdentity {
        corpus_sha8: caps.0,
        prompt_sha8: caps.1,
        models_sha8: caps.2,
        rubric_sha8: caps.3,
    })
}

/// `(corpus, prompt, models, rubric)` 8-hex groups from a filename.
fn regex_lite_safe(name: &str) -> Option<(String, String, String, String)> {
    let trimmed = name.strip_suffix(".json").unwrap_or(name);
    let parts: Vec<&str> = trimmed.split('-').collect();
    // takes-quality-<c>-<p>-<m>-<r>
    if parts.len() != 5 {
        return None;
    }
    let (c, p, m, r) = (parts[1], parts[2], parts[3], parts[4]);
    let is_sha = |s: &str| s.len() == 8 && s.chars().all(|c| c.is_ascii_hexdigit());
    if is_sha(c) && is_sha(p) && is_sha(m) && is_sha(r) {
        Some((c.to_string(), p.to_string(), m.to_string(), r.to_string()))
    } else {
        None
    }
}

/// One roll per declared rubric dimension (faithful to TS `aggregate.ts`
/// `DimensionRoll`: mean/min/max/scores/per_model/failReason).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DimensionRoll {
    pub mean: f64,
    pub min: f64,
    pub max: f64,
    pub scores: Vec<f64>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub per_model: BTreeMap<String, f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fail_reason: Option<String>,
}

impl DimensionRoll {
    /// Convert from the shared cross-modal [`DimensionRoll`] (which lacks
    /// `max`/`per_model`) into the takes-quality receipt shape.
    pub fn from_cross(roll: &CrossDimensionRoll) -> DimensionRoll {
        let max = roll.scores.iter().cloned().fold(f64::NEG_INFINITY, f64::max);
        let fail_reason = roll.fail_reason.map(|f| match f {
            FailReason::MeanBelow7 => "mean_below_7".to_string(),
            FailReason::MinBelow5 => "min_below_5".to_string(),
        });
        DimensionRoll {
            mean: roll.mean,
            min: roll.min,
            max: if max.is_finite() { max } else { roll.min },
            scores: roll.scores.clone(),
            per_model: BTreeMap::new(),
            fail_reason,
        }
    }
}

/// Corpus provenance embedded in the receipt.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CorpusMeta {
    pub source: String,
    pub n_takes: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub slug_prefix: Option<String>,
    pub corpus_sha8: String,
}

/// One eval run's full receipt (faithful to TS `TakesQualityReceipt`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TakesQualityReceipt {
    pub schema_version: u8,
    /// ISO 8601 UTC timestamp of run start.
    pub ts: String,
    pub rubric_version: String,
    pub rubric_sha8: String,
    pub corpus: CorpusMeta,
    pub prompt_sha8: String,
    pub models_sha8: String,
    pub models: Vec<String>,
    pub cycles_run: usize,
    /// One entry per cycle; the count of contributing models that cycle.
    pub successes_per_cycle: Vec<usize>,
    /// 'pass' | 'fail' | 'inconclusive'
    pub verdict: String,
    pub scores: BTreeMap<String, DimensionRoll>,
    /// Mean of dim means; null when verdict=inconclusive.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub overall_score: Option<f64>,
    pub cost_usd: f64,
    /// Top-10 deduped improvements; absent when verdict=inconclusive.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub improvements: Option<Vec<String>>,
    /// Per-slot errors carried through for debugging.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub errors: Option<Vec<ReceiptError>>,
    /// One-line human verdict prose.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub verdict_message: Option<String>,
}

/// Per-slot judge error.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ReceiptError {
    pub model_id: String,
    pub error: String,
}

impl TakesQualityReceipt {
    pub fn identity(&self) -> ReceiptIdentity {
        ReceiptIdentity {
            corpus_sha8: self.corpus.corpus_sha8.clone(),
            prompt_sha8: self.prompt_sha8.clone(),
            models_sha8: self.models_sha8.clone(),
            rubric_sha8: self.rubric_sha8.clone(),
        }
    }
}

/// Build the receipt's 4-sha identity filename.
pub fn receipt_filename(receipt: &TakesQualityReceipt) -> String {
    build_receipt_filename(&receipt.identity())
}

/// Best-effort disk artifact write. Returns the file path on success,
/// `None` on failure (caller logs but doesn't propagate). Mirrors TS
/// `writeReceiptArtifact` — the DB row is the durable artifact.
pub fn write_receipt_artifact(receipt: &TakesQualityReceipt, dir: &Path) -> Option<String> {
    let path = dir.join(receipt_filename(receipt));
    match std::fs::write(&path, serde_json::to_string_pretty(receipt).unwrap_or_default()) {
        Ok(()) => Some(path.to_string_lossy().to_string()),
        Err(e) => {
            eprintln!(
                "[eval takes-quality] disk receipt write failed ({}); DB row is the source of truth",
                e
            );
            None
        }
    }
}

/// One DB row backing `eval_takes_quality_runs` (mirrors the contradictions
/// `ContradictionsRunRow` pattern). The full receipt is stored as JSON; the
/// 4-sha columns form the idempotent unique key.
#[derive(Debug, Clone)]
pub struct TakesQualityRunRow {
    pub run_id: String,
    pub ran_at: String,
    pub schema_version: u8,
    pub rubric_version: String,
    pub verdict: String,
    pub overall_score: Option<f64>,
    pub cost_usd: f64,
    pub corpus_sha8: String,
    pub receipt_sha8_corpus: String,
    pub receipt_sha8_prompt: String,
    pub receipt_sha8_models: String,
    pub receipt_sha8_rubric: String,
    pub receipt_json: serde_json::Value,
}

impl TakesQualityRunRow {
    /// Build a run row from a receipt. `run_id` is the joined 4-sha key, which
    /// is exactly the DB idempotency key (ON CONFLICT DO NOTHING).
    pub fn from_receipt(receipt: &TakesQualityReceipt) -> TakesQualityRunRow {
        let id = receipt.identity();
        TakesQualityRunRow {
            run_id: format!(
                "{}-{}-{}-{}",
                id.corpus_sha8, id.prompt_sha8, id.models_sha8, id.rubric_sha8
            ),
            ran_at: receipt.ts.clone(),
            schema_version: receipt.schema_version,
            rubric_version: RUBRIC_VERSION.to_string(),
            verdict: receipt.verdict.clone(),
            overall_score: receipt.overall_score,
            cost_usd: receipt.cost_usd,
            corpus_sha8: receipt.corpus.corpus_sha8.clone(),
            receipt_sha8_corpus: id.corpus_sha8,
            receipt_sha8_prompt: id.prompt_sha8,
            receipt_sha8_models: id.models_sha8,
            receipt_sha8_rubric: id.rubric_sha8,
            receipt_json: serde_json::to_value(receipt).unwrap_or(serde_json::Value::Null),
        }
    }
}

/// Insert the full receipt into the DB. Returns `true` when a new row was
/// written, `false` on idempotent no-op (the 4-sha unique key already exists).
/// Throws on failure (DB is authoritative).
pub async fn write_takes_quality_run(
    engine: &dyn BrainEngine,
    receipt: &TakesQualityReceipt,
) -> crate::Result<bool> {
    let row = TakesQualityRunRow::from_receipt(receipt);
    engine.write_takes_quality_run(&row).await
}

/// Map a cross-modal [`Verdict`] to the TS receipt string form.
pub fn verdict_to_string(v: &Verdict) -> String {
    match v {
        Verdict::Pass => "pass".to_string(),
        Verdict::Fail => "fail".to_string(),
        Verdict::Inconclusive => "inconclusive".to_string(),
    }
}
