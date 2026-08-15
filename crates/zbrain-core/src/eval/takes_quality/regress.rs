//! takes-quality-eval/regress — compare a fresh run vs a prior receipt.
//!
//! Faithful port of TS `regress.ts`. Use case: after changing the takes
//! extraction prompt, run a fresh eval and compare against the last known-good
//! receipt. If `overall_score` or any dim mean dropped past a threshold, the
//! caller exits non-zero → CI gate fails the change.
//!
//! The current run reuses the same 4-sha identity as the prior receipt to keep
//! the comparison apples-to-apples. If they differ, `regress` reports the
//! inputs are dissimilar (informational — caller decides whether to treat as a
//! failure).

use crate::eval::takes_quality::receipt::TakesQualityReceipt;
use std::collections::BTreeMap;

#[derive(Debug, Clone, PartialEq)]
pub struct RegressionDelta {
    /// Per-dim mean delta (current − prior). Negative = regression.
    pub dim_deltas: BTreeMap<String, f64>,
    /// Overall score delta (current − prior). Negative = regression.
    pub overall_delta: f64,
    /// True when any dim regressed past `threshold`.
    pub regressed: bool,
    /// Threshold below which a dim drop counts as regression. Default 0.5.
    pub threshold: f64,
    /// Human-readable summary line.
    pub summary: String,
    /// True if any 4-sha component differs between current and prior.
    pub inputs_differ: bool,
    /// Specific 4-sha diffs when `inputs_differ`.
    pub input_diffs: Vec<String>,
}

#[derive(Debug, Clone, Default)]
pub struct RegressOpts {
    /// Per-dim mean drop threshold counting as regression. Default 0.5.
    pub threshold: Option<f64>,
}

fn round1(n: f64) -> f64 {
    (n * 10.0).round() / 10.0
}

/// Compare a fresh receipt against a prior receipt.
pub fn compare_receipts(
    current: &TakesQualityReceipt,
    prior: &TakesQualityReceipt,
    opts: &RegressOpts,
) -> RegressionDelta {
    let threshold = opts.threshold.unwrap_or(0.5);

    let cur = current.identity();
    let pri = prior.identity();
    let mut input_diffs = vec![];
    if cur.corpus_sha8 != pri.corpus_sha8 {
        input_diffs.push(format!("corpus_sha8 differs ({} → {})", pri.corpus_sha8, cur.corpus_sha8));
    }
    if cur.prompt_sha8 != pri.prompt_sha8 {
        input_diffs.push(format!("prompt_sha8 differs ({} → {})", pri.prompt_sha8, cur.prompt_sha8));
    }
    if cur.models_sha8 != pri.models_sha8 {
        input_diffs.push(format!("models_sha8 differs ({} → {})", pri.models_sha8, cur.models_sha8));
    }
    if cur.rubric_sha8 != pri.rubric_sha8 {
        input_diffs.push(format!("rubric_sha8 differs ({} → {})", pri.rubric_sha8, cur.rubric_sha8));
    }
    let inputs_differ = !input_diffs.is_empty();

    let mut dim_deltas: BTreeMap<String, f64> = BTreeMap::new();
    let mut regressed = false;
    for (dim, prior_roll) in &prior.scores {
        if let Some(cur_roll) = current.scores.get(dim) {
            let delta = round1(cur_roll.mean - prior_roll.mean);
            dim_deltas.insert(dim.clone(), delta);
            if delta < -threshold {
                regressed = true;
            }
        }
    }

    let c_overall = current.overall_score.unwrap_or(0.0);
    let p_overall = prior.overall_score.unwrap_or(0.0);
    let overall_delta = round1(c_overall - p_overall);
    if overall_delta < -threshold {
        regressed = true;
    }

    let failing: Vec<String> = dim_deltas
        .iter()
        .filter(|(_, d)| **d < -threshold)
        .map(|(k, d)| format!("{k}={d}"))
        .collect();
    let summary = if regressed {
        format!(
            "REGRESSION: overall {}{}{}",
            if overall_delta >= 0.0 { "+" } else { "" },
            overall_delta,
            if failing.is_empty() {
                String::new()
            } else {
                format!("; failing dims: {}", failing.join(", "))
            }
        )
    } else {
        format!(
            "OK: overall {}{} (no dim regressed past {})",
            if overall_delta >= 0.0 { "+" } else { "" },
            overall_delta,
            threshold
        )
    };

    RegressionDelta {
        dim_deltas,
        overall_delta,
        regressed,
        threshold,
        summary,
        inputs_differ,
        input_diffs,
    }
}
