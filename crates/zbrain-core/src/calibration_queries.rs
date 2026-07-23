//! Admin calibration data-access trait.
//!
//! Separate from `BrainEngine` and `AdminQueries` because calibration reads
//! `calibration_profiles` and `takes` tables — a different concern from
//! OAuth/API-key management or brain content.
//!
//! Defined in zbrain-core so both the engine and zbrain-web can depend on
//! it without a circular dependency.

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use std::fmt::Debug;

use crate::error::Result;

// ── value types ───────────────────────────────────────────────────────────

/// Aggregated scoring statistics for a holder's resolved takes.
///
/// Field names + shape mirror the canonical TS `TakesScorecard`
/// (`src/core/engine.ts`) exactly — snake_case JSON, so NO `rename_all`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct TakesScorecard {
    /// Total `kind='bet'` rows in scope (resolved or not).
    pub total_bets: i64,
    /// Count of rows where `resolved_quality IN ('correct','incorrect','partial')`.
    /// Deliberately 3-state to preserve historical comparisons; unresolvable
    /// rows land in `unresolvable_count` instead.
    pub resolved: i64,
    pub correct: i64,
    pub incorrect: i64,
    pub partial: i64,
    /// `correct / (correct + incorrect)`. `None` when n=0.
    pub accuracy: Option<f64>,
    /// Brier over `resolved_quality IN ('correct','incorrect')` rows:
    /// `correct→1`, `incorrect→0`, `mean((weight − outcome)²)`. Excludes
    /// partial AND unresolvable. `None` when no correct+incorrect rows.
    pub brier: Option<f64>,
    /// `partial / resolved`. `None` when n=0.
    pub partial_rate: Option<f64>,
    /// Count of `resolved_quality = 'unresolvable'` rows. Sibling to
    /// `resolved` so historical comparisons stay valid. Optional in the TS
    /// SDK; always populated here.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub unresolvable_count: Option<i64>,
    /// `unresolvable_count / (resolved + unresolvable_count)`. `None` when
    /// both are 0. Optional in the TS SDK; always populated here.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub unresolvable_rate: Option<f64>,
}

/// Minimal per-take projection needed for scorecard aggregation. Backends
/// pull these rows already scoped (holder/domain/window/allow-list applied by
/// the query layer) then hand them to [`aggregate_scorecard`].
#[derive(Debug, Clone)]
pub struct ScorecardRow {
    pub kind: String,
    pub weight: f64,
    pub resolved_quality: Option<String>,
}

/// Backend-agnostic scorecard aggregation — the single source of truth for the
/// scorecard math across InMemory/Libsql/Postgres.
///
/// Mirrors the canonical TS pglite `getScorecard` FILTER counts + `POWER`
/// Brier term + `finalizeScorecard` degradation exactly:
///   - `total_bets`      = rows where `kind = 'bet'`
///   - `resolved`        = rows where `resolved_quality IN (correct,incorrect,partial)`
///   - Brier            = mean over `correct|incorrect` rows of `(weight − outcome)²`
///                        (`correct→1`, `incorrect→0`); `None` when that set is empty
///   - `accuracy`        = `correct / (correct+incorrect)`; `None` when n=0
///   - `partial_rate`    = `partial / resolved`; `None` when n=0
///   - `unresolvable_rate` = `unresolvable / (resolved+unresolvable)`; `None` when n=0
pub fn aggregate_scorecard<I: IntoIterator<Item = ScorecardRow>>(rows: I) -> TakesScorecard {
    let mut total_bets = 0i64;
    let mut correct = 0i64;
    let mut incorrect = 0i64;
    let mut partial = 0i64;
    let mut unresolvable = 0i64;
    let mut brier_sum = 0f64;
    for r in rows {
        if r.kind == "bet" {
            total_bets += 1;
        }
        match r.resolved_quality.as_deref() {
            Some("correct") => {
                correct += 1;
                brier_sum += (r.weight - 1.0).powi(2);
            }
            Some("incorrect") => {
                incorrect += 1;
                brier_sum += (r.weight - 0.0).powi(2);
            }
            Some("partial") => partial += 1,
            Some("unresolvable") => unresolvable += 1,
            _ => {}
        }
    }
    let resolved = correct + incorrect + partial;
    let binary = correct + incorrect;
    let unresolvable_denom = resolved + unresolvable;
    TakesScorecard {
        total_bets,
        resolved,
        correct,
        incorrect,
        partial,
        accuracy: if binary > 0 {
            Some(correct as f64 / binary as f64)
        } else {
            None
        },
        brier: if binary > 0 {
            Some(brier_sum / binary as f64)
        } else {
            None
        },
        partial_rate: if resolved > 0 {
            Some(partial as f64 / resolved as f64)
        } else {
            None
        },
        unresolvable_count: Some(unresolvable),
        unresolvable_rate: if unresolvable_denom > 0 {
            Some(unresolvable as f64 / unresolvable_denom as f64)
        } else {
            None
        },
    }
}

/// A single confidence-bucket entry for the calibration curve.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct CalibrationBucket {
    pub bucket_label: String,
    pub n: i64,
    pub accuracy: f64,
}

/// A single row from the `calibration_profiles` table.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct CalibrationProfileRow {
    pub id: i64,
    pub source_id: String,
    pub holder: String,
    pub wave_version: String,
    pub generated_at: String,
    pub published: bool,
    pub total_resolved: i32,
    pub brier: Option<f64>,
    pub accuracy: Option<f64>,
    pub partial_rate: Option<f64>,
    pub grade_completion: f64,
    pub domain_scorecards: serde_json::Value,
    pub pattern_statements: Vec<String>,
    pub voice_gate_passed: bool,
    pub voice_gate_attempts: i16,
    pub active_bias_tags: Vec<String>,
    pub model_id: String,
    pub cost_usd: Option<f64>,
    pub judge_model_agreement: Option<f64>,
}

/// A summarized take for pattern drill-down.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct TakeSummary {
    pub slug: String,
    pub claim: String,
    pub resolution: Option<String>,
    pub brier: Option<f64>,
}

/// Detailed drill-down for a calibration pattern.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct PatternDetail {
    pub pattern_text: String,
    pub top_takes: Vec<TakeSummary>,
}

// ── query scope ────────────────────────────────────────────────────────────

/// Query scope for [`CalibrationQueries::get_scorecard`].
///
/// Mirrors the canonical TS `TakesScorecardOpts` (`holder`/`domainPrefix`/
/// `since`/`until`) plus the server-side holder allow-list that TS passes as
/// `getScorecard(opts, allowList)`'s second argument. Bundled into one struct
/// so the trait signature stays stable as scoping grows.
#[derive(Debug, Clone, Default)]
pub struct ScorecardQuery<'a> {
    /// Holder to scope to (`world|garry|brain|<slug>`). `None` aggregates over
    /// all holders, matching canonical TS where `opts.holder === undefined`
    /// omits the `AND holder = $N` clause entirely.
    pub holder: Option<&'a str>,
    /// Slug-prefix domain scope (e.g. `"companies/"`), applied via
    /// `EXISTS(pages p WHERE p.id = takes.page_id AND p.slug LIKE prefix%)`.
    /// `None` returns the holder's overall scorecard.
    pub domain_prefix: Option<&'a str>,
    /// Inclusive lower bound on `since_date` (ISO `'YYYY-MM-DD'`). `None` = no lower bound.
    pub since: Option<&'a str>,
    /// Inclusive upper bound on `since_date` (ISO `'YYYY-MM-DD'`). `None` = no upper bound.
    pub until: Option<&'a str>,
    /// Server-side holder allow-list. When `Some`, only rows whose holder is
    /// in the list are counted (`AND holder = ANY($list)` parity, D4
    /// defense-in-depth for remote callers). `None` disables the filter
    /// (trusted local callers).
    pub holders_allow_list: Option<&'a [String]>,
}

impl<'a> ScorecardQuery<'a> {
    /// Convenience: overall scorecard for a single holder, no scoping/allow-list.
    pub fn for_holder(holder: &'a str) -> Self {
        Self { holder: Some(holder), ..Default::default() }
    }
}

// ── trait ────────────────────────────────────────────────────────────────

/// Calibration-oriented queries against calibration_profiles and takes tables.
#[async_trait]
pub trait CalibrationQueries: Debug + Send + Sync {
    /// Aggregated scoring stats from resolved takes.
    ///
    /// Scoped by [`ScorecardQuery`]. Mirrors the canonical TS
    /// `getScorecard({ holder, domainPrefix, since, until }, allowList)`
    /// surface so `forecastForTake`/`batchForecast` and the `takes_scorecard`
    /// operation share one path.
    async fn get_scorecard(&self, query: &ScorecardQuery<'_>) -> Result<TakesScorecard>;

    /// Confidence-bucket accuracy curve.
    async fn get_calibration_curve(&self, holder: &str) -> Result<Vec<CalibrationBucket>>;

    /// Latest calibration profile for a holder.
    /// Returns None when the table does not exist or no profiles exist.
    /// Optionally filter by source (cross-brain query uses this).
    async fn get_latest_profile(&self, holder: &str, source_id: Option<&str>, source_ids: Option<&[String]>) -> Result<Option<CalibrationProfileRow>>;

    /// Pattern text + top-25 resolved takes for drill-down.
    /// `pattern_index` is 1-based.
    async fn get_pattern_detail(
        &self,
        holder: &str,
        pattern_index: usize,
    ) -> Result<Option<PatternDetail>>;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::InMemoryEngine;

    fn make_engine() -> InMemoryEngine {
        InMemoryEngine::default()
    }

    #[tokio::test]
    async fn contract_get_latest_profile_returns_none() {
        let engine = make_engine();
        let result = engine.get_latest_profile("garry", None, None).await.unwrap();
        assert_eq!(result, None);
    }

    #[tokio::test]
    async fn contract_get_scorecard_returns_zeros() {
        let engine = make_engine();
        let result = engine
            .get_scorecard(&ScorecardQuery::for_holder("garry"))
            .await
            .unwrap();
        assert_eq!(result.total_bets, 0);
        assert_eq!(result.resolved, 0);
        assert_eq!(result.correct, 0);
        assert_eq!(result.incorrect, 0);
        assert_eq!(result.partial, 0);
        // n=0 → all rate/aggregate fields degrade to None.
        assert_eq!(result.accuracy, None);
        assert_eq!(result.brier, None);
        assert_eq!(result.partial_rate, None);
        assert_eq!(result.unresolvable_count, Some(0));
        assert_eq!(result.unresolvable_rate, None);
    }

    #[tokio::test]
    async fn contract_get_calibration_curve_returns_empty() {
        let engine = make_engine();
        let result = engine.get_calibration_curve("garry").await.unwrap();
        assert!(result.is_empty());
    }

    #[tokio::test]
    async fn contract_get_pattern_detail_returns_none() {
        let engine = make_engine();
        let result = engine.get_pattern_detail("garry", 1).await.unwrap();
        assert_eq!(result, None);
    }
}
