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
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct TakesScorecard {
    pub resolved: i64,
    pub brier: f64,
    pub accuracy: f64,
    pub correct: i64,
    pub incorrect: i64,
    pub partial_rate: f64,
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
    pub id: String,
    pub source_id: String,
    pub holder: String,
    pub generated_at: String,
    pub brier: Option<f64>,
    pub accuracy: Option<f64>,
    pub pattern_statements: Option<Vec<String>>,
    pub active_bias_tags: Option<Vec<String>>,
    pub domain_scorecards: Option<serde_json::Value>,
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

// ── trait ────────────────────────────────────────────────────────────────

/// Calibration-oriented queries against calibration_profiles and takes tables.
#[async_trait]
pub trait CalibrationQueries: Debug + Send + Sync {
    /// Aggregated scoring stats from resolved takes.
    ///
    /// `domain_prefix` scopes the scorecard to a calibration domain (via
    /// `take_domain_assignments`); `None` returns the holder's overall
    /// scorecard. Mirrors the TS `getScorecard({ holder, domainPrefix })`
    /// surface so `forecastForTake`/`batchForecast` can fetch bucketed
    /// scorecards.
    async fn get_scorecard(&self, holder: &str, domain_prefix: Option<&str>) -> Result<TakesScorecard>;

    /// Confidence-bucket accuracy curve.
    async fn get_calibration_curve(&self, holder: &str) -> Result<Vec<CalibrationBucket>>;

    /// Latest calibration profile for a holder.
    /// Returns None when the table does not exist or no profiles exist.
    async fn get_latest_profile(&self, holder: &str) -> Result<Option<CalibrationProfileRow>>;

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
        let result = engine.get_latest_profile("garry").await.unwrap();
        assert_eq!(result, None);
    }

    #[tokio::test]
    async fn contract_get_scorecard_returns_zeros() {
        let engine = make_engine();
        let result = engine.get_scorecard("garry", None).await.unwrap();
        assert_eq!(result.resolved, 0);
        assert_eq!(result.brier, 0.0);
        assert_eq!(result.accuracy, 0.0);
        assert_eq!(result.correct, 0);
        assert_eq!(result.incorrect, 0);
        assert_eq!(result.partial_rate, 0.0);
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
