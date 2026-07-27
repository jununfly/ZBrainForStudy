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

/// Minimal per-take projection needed for calibration-curve bucketing.
/// Backends pull these rows already scoped (holder/allow-list applied by the
/// query layer) then hand them to [`aggregate_calibration_curve`].
#[derive(Debug, Clone)]
pub struct CalibrationRow {
    pub weight: f64,
    pub resolved_quality: Option<String>,
}

/// Backend-agnostic calibration-curve aggregation — the single source of truth
/// for the curve math across InMemory/Libsql/Postgres.
///
/// Mirrors the canonical TS `getCalibrationCurve` binned CTE
/// (`src/core/pglite-engine.ts`):
///   - only `resolved_quality IN ('correct','incorrect')` rows count
///   - `bucket_idx = LEAST(floor(weight / bucketSize), maxIdx)` where
///     `maxIdx = floor(1/bucketSize) - 1`. We compute `floor(weight * scale)`
///     with `scale = round(1/bucketSize)` (an integer) so decimal weights like
///     `0.7` land in bucket 7 rather than 6 — this reproduces the TS Postgres
///     `weight::numeric / $1::numeric` exactness on SQLite/InMemory (double
///     `weight / bucketSize` would FP-round `0.7/0.1` to 6.999…→6).
///   - `observed = sum(correct)/n`, `predicted = mean(weight)`; both `None`
///     when `n == 0`.
pub fn aggregate_calibration_curve<I: IntoIterator<Item = CalibrationRow>>(
    rows: I,
    bucket_size: f64,
) -> Vec<CalibrationBucket> {
    let bucket_size = if bucket_size > 0.0 && bucket_size <= 1.0 {
        bucket_size
    } else {
        0.1
    };
    let scale = (1.0 / bucket_size).round() as i64;
    let max_idx = (1.0 / bucket_size).floor() as i64 - 1;
    // bucket_idx -> (n, sum_hit, sum_weight)
    let mut buckets: std::collections::BTreeMap<i64, (i64, f64, f64)> = Default::default();
    for r in rows {
        let hit = match r.resolved_quality.as_deref() {
            Some("correct") => 1.0,
            Some("incorrect") => 0.0,
            _ => continue,
        };
        let mut idx = (r.weight * scale as f64).floor() as i64;
        if idx > max_idx {
            idx = max_idx;
        }
        // Only an upper clamp (TS `LEAST`, not a lower clamp) — negative
        // weights are nonsensical but we mirror TS's single-sided clamp.
        let entry = buckets.entry(idx).or_insert((0, 0.0, 0.0));
        entry.0 += 1;
        entry.1 += hit;
        entry.2 += r.weight;
    }
    buckets
        .into_iter()
        .map(|(idx, (n, sum_hit, sum_weight))| CalibrationBucket {
            bucket_lo: idx as f64 * bucket_size,
            bucket_hi: (idx + 1) as f64 * bucket_size,
            n,
            observed: if n > 0 { Some(sum_hit / n as f64) } else { None },
            predicted: if n > 0 { Some(sum_weight / n as f64) } else { None },
        })
        .collect()
}

/// A single confidence-bucket entry for the calibration curve.
///
/// Field names + shape mirror the canonical TS `CalibrationBucket`
/// (`src/core/engine.ts`) exactly — snake_case/lowercase JSON, so NO
/// `rename_all`. `observed`/`predicted` are `None` only when `n == 0` (a
/// bucket can collapse to zero rows after scoping); otherwise they mirror
/// `correct/n` and `mean(weight)` respectively.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct CalibrationBucket {
    pub bucket_lo: f64,
    pub bucket_hi: f64,
    pub n: i64,
    pub observed: Option<f64>,
    pub predicted: Option<f64>,
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

/// Query scope for [`CalibrationQueries::get_calibration_curve`].
///
/// Mirrors the canonical TS `CalibrationCurveOpts` (`holder`/`bucketSize`) plus
/// the server-side holder allow-list that TS passes as
/// `getCalibrationCurve(opts, allowList)`'s second argument. Bundled into one
/// struct so the trait signature stays stable as scoping grows.
#[derive(Debug, Clone, Default)]
pub struct CalibrationCurveQuery<'a> {
    /// Holder to scope to (`world|garry|brain|<slug>`). `None` aggregates over
    /// all holders, matching canonical TS where `opts.holder === undefined`
    /// omits the `AND holder = $N` clause entirely.
    pub holder: Option<&'a str>,
    /// Bucket width in `(0,1]` (default `0.1`). Canonical TS clamps to `0.1`
    /// when out of range.
    pub bucket_size: Option<f64>,
    /// Server-side holder allow-list. When `Some`, only rows whose holder is
    /// in the list are counted (`AND holder = ANY($list)` parity, D4
    /// defense-in-depth for remote callers). `None` disables the filter
    /// (trusted local callers).
    pub holders_allow_list: Option<&'a [String]>,
}

impl<'a> CalibrationCurveQuery<'a> {
    /// Convenience: curve for a single holder with default bucket size.
    pub fn for_holder(holder: &'a str) -> Self {
        Self {
            holder: Some(holder),
            ..Default::default()
        }
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

    /// Confidence-bucket accuracy curve (observed vs predicted per weight bucket).
    ///
    /// Scoped by [`CalibrationCurveQuery`]. Mirrors the canonical TS
    /// `getCalibrationCurve({ holder, bucketSize }, allowList)` surface.
    async fn get_calibration_curve(&self, query: &CalibrationCurveQuery<'_>) -> Result<Vec<CalibrationBucket>>;

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
        let result = engine
            .get_calibration_curve(&CalibrationCurveQuery::for_holder("garry"))
            .await
            .unwrap();
        assert!(result.is_empty());
    }

    #[tokio::test]
    async fn contract_get_pattern_detail_returns_none() {
        let engine = make_engine();
        let result = engine.get_pattern_detail("garry", 1).await.unwrap();
        assert_eq!(result, None);
    }

    // ── canonical bucketing math (backend-agnostic) ─────────────────────────

    #[test]
    fn aggregate_calibration_curve_buckets_correctly() {
        // `0.7` must land in bucket 7 — the FP-edge case where naive
        // `floor(weight / 0.1)` on doubles rounds `0.7/0.1` to 6.999…→6, while
        // the integer-scale trick reproduces TS Postgres `numeric` exactness (7).
        let rows = vec![
            CalibrationRow { weight: 0.05, resolved_quality: Some("correct".into()) }, // bucket 0
            CalibrationRow { weight: 0.10, resolved_quality: Some("correct".into()) }, // bucket 1
            CalibrationRow { weight: 0.70, resolved_quality: Some("correct".into()) }, // bucket 7
            CalibrationRow { weight: 0.70, resolved_quality: Some("incorrect".into()) }, // bucket 7
            CalibrationRow { weight: 0.70, resolved_quality: Some("partial".into()) }, // excluded
            CalibrationRow { weight: 0.95, resolved_quality: Some("incorrect".into()) }, // bucket 9 (maxIdx)
            CalibrationRow { weight: 1.00, resolved_quality: Some("correct".into()) }, // bucket 9 (clamp)
            CalibrationRow { weight: 0.30, resolved_quality: Some("incorrect".into()) }, // bucket 3
        ];
        let curve = aggregate_calibration_curve(rows, 0.1);

        let b0 = curve.iter().find(|b| b.bucket_lo == 0.0).expect("bucket 0");
        assert_eq!(b0.n, 1);
        assert_eq!(b0.observed, Some(1.0));
        assert_eq!(b0.predicted, Some(0.05));

        let b7 = curve
            .iter()
            .find(|b| (b.bucket_lo - 0.7).abs() < 1e-9)
            .expect("bucket 7");
        assert_eq!(b7.n, 2);
        assert_eq!(b7.observed, Some(0.5));
        assert_eq!(b7.predicted, Some(0.7));

        let b9 = curve
            .iter()
            .find(|b| (b.bucket_lo - 0.9).abs() < 1e-9)
            .expect("bucket 9");
        assert_eq!(b9.n, 2);
        assert_eq!(b9.observed, Some(0.5));
        assert!((b9.predicted.unwrap() - 0.975).abs() < 1e-9);

        // partial excluded; total resolved counted = 7 (8 input rows − 1 partial).
        let total: i64 = curve.iter().map(|b| b.n).sum();
        assert_eq!(total, 7);
    }

    #[test]
    fn aggregate_calibration_curve_clamps_bucket_size() {
        // bucket_size out of range falls back to 0.1.
        let rows = vec![CalibrationRow {
            weight: 0.55,
            resolved_quality: Some("correct".into()),
        }];
        let curve = aggregate_calibration_curve(rows, 0.0);
        assert_eq!(curve.len(), 1);
        assert!((curve[0].bucket_lo - 0.5).abs() < 1e-9);
        assert_eq!(curve[0].n, 1);
    }

    // ── InMemory engine end-to-end (scoping + math) ─────────────────────────

    use crate::types::Take;

    fn mk_take(id: u64, holder: &str, weight: f64, quality: Option<&str>) -> Take {
        let ts = "2026-01-01T00:00:00Z".to_string();
        Take {
            id,
            page_id: id,
            row_num: 1,
            claim: format!("take {id}"),
            kind: "bet".into(),
            holder: holder.into(),
            weight,
            since_date: None,
            until_date: None,
            source: None,
            superseded_by: None,
            active: true,
            resolved_at: Some(ts.clone()),
            resolved_quality: quality.map(|s| s.into()),
            resolved_outcome: None,
            resolved_evidence: None,
            resolved_value: None,
            resolved_unit: None,
            resolved_by: None,
            created_at: ts.clone(),
            updated_at: ts,
        }
    }

    #[tokio::test]
    async fn inmemory_get_calibration_curve_filters_holder_and_quality() {
        let engine = make_engine();
        engine.add_take(mk_take(1, "garry", 0.5, Some("correct")));
        engine.add_take(mk_take(2, "garry", 0.5, Some("correct")));
        engine.add_take(mk_take(3, "garry", 0.5, Some("incorrect")));
        engine.add_take(mk_take(4, "world", 0.5, Some("correct"))); // other holder
        engine.add_take(mk_take(5, "garry", 0.5, Some("partial"))); // excluded

        let curve = engine
            .get_calibration_curve(&CalibrationCurveQuery::for_holder("garry"))
            .await
            .unwrap();

        assert_eq!(curve.len(), 1, "all seeded takes fall in bucket 0.5");
        let b5 = &curve[0];
        assert_eq!(b5.n, 3); // 2 correct + 1 incorrect (partial excluded)
        assert_eq!(b5.observed, Some(2.0 / 3.0));
        assert!((b5.predicted.unwrap() - 0.5).abs() < 1e-9);
    }

    #[tokio::test]
    async fn inmemory_get_calibration_curve_allow_list_fail_closed() {
        let engine = make_engine();
        engine.add_take(mk_take(1, "garry", 0.5, Some("correct")));
        engine.add_take(mk_take(2, "world", 0.5, Some("correct")));

        // Empty allow-list → hard fail-closed (no rows).
        let empty: Vec<String> = vec![];
        let curve = engine
            .get_calibration_curve(&CalibrationCurveQuery {
                holder: None,
                bucket_size: None,
                holders_allow_list: Some(&empty),
            })
            .await
            .unwrap();
        assert!(curve.is_empty());

        // Non-empty allow-list restricts to the listed holder only.
        let list = vec!["garry".to_string()];
        let curve = engine
            .get_calibration_curve(&CalibrationCurveQuery {
                holder: None,
                bucket_size: None,
                holders_allow_list: Some(&list),
            })
            .await
            .unwrap();
        let total: i64 = curve.iter().map(|b| b.n).sum();
        assert_eq!(total, 1);
    }
}
