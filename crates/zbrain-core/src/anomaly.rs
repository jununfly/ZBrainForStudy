//! v0.29 — Anomaly detection: statistical helpers for `find_anomalies`.
//!
//! Pure functions over densified daily-count buckets. Mirrors the TS module
//! `src/core/cycle/anomaly.ts`. The engine layer runs the dialect-specific SQL
//! (Postgres `generate_series`/`date_trunc`/`array_agg`, Libsql recursive
//! date-series CTE + `strftime` + `json_group_array`, InMemory in-Rust) and
//! hands the denified rows here. Keeping the stats pure is what makes
//! `find_anomalies` testable without a database.
//!
//! Cohort kinds: `tag`, `type`. The year cohort is deferred to v0.30 pending
//! proper frontmatter date-field detection (same as TS).

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// One row of the densified daily-count series for a single cohort.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CohortDayRow {
    pub cohort_kind: CohortKind,
    pub cohort_value: String,
    /// ISO date (YYYY-MM-DD).
    pub day: String,
    /// Distinct pages touched in this cohort on `day`. Zero if no activity.
    pub count: i64,
}

/// "Today" current-window count per cohort plus the page slugs that drove it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CohortTodayRow {
    pub cohort_kind: CohortKind,
    pub cohort_value: String,
    pub count: i64,
    pub page_slugs: Vec<String>,
}

/// Which facet a cohort is grouped by.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CohortKind {
    Tag,
    Type,
}

impl CohortKind {
    /// Stable string form used in the SQL `cohort_kind` column and `cohort_key`.
    pub fn as_str(self) -> &'static str {
        match self {
            CohortKind::Tag => "tag",
            CohortKind::Type => "type",
        }
    }
}

/// A detected anomaly row, ready to serialize for the CLI / tool output.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct AnomalyResult {
    pub cohort_kind: CohortKind,
    pub cohort_value: String,
    pub count: i64,
    pub baseline_mean: f64,
    pub baseline_stddev: f64,
    pub sigma_observed: f64,
    pub page_slugs: Vec<String>,
}

/// Mean and (sample) stddev of a number slice. Returns `(0.0, 0.0)` for empty
/// input. Uses the sample stddev (n-1 denominator) so a single-sample baseline
/// doesn't claim zero variance.
pub fn mean_stddev(samples: &[f64]) -> (f64, f64) {
    if samples.is_empty() {
        return (0.0, 0.0);
    }
    let sum: f64 = samples.iter().copied().sum();
    let mean = sum / samples.len() as f64;
    if samples.len() == 1 {
        return (mean, 0.0);
    }
    let sq_sum: f64 = samples.iter().map(|b| (b - mean) * (b - mean)).sum();
    let variance = sq_sum / (samples.len() - 1) as f64;
    (mean, variance.sqrt())
}

/// Compute anomaly results from densified baseline buckets + today's counts.
///
/// For each cohort:
/// 1. Compute `(mean, stddev)` over the baseline daily counts.
/// 2. If `stddev > 0`: anomalous when `today.count > mean + sigma*stddev`.
///    `sigma_observed = (today.count - mean) / stddev`.
/// 3. If `stddev == 0`: small-sample fallback — anomalous when
///    `today.count > mean + 1`. `sigma_observed` is the finite proxy
///    `today.count - mean` so callers still get a usable sort key.
///
/// Cohorts with no baseline rows AND no today rows are skipped. Cohorts
/// appearing only in `today` (a brand-new cohort) get a `baseline_mean` of 0 —
/// they're surfaced as anomalies whenever `today.count >= 2` (mean+1 fallback).
///
/// Returns the top `limit` rows sorted by `sigma_observed` descending. Each row
/// caps `page_slugs` at 50 entries.
pub fn compute_anomalies_from_buckets(
    baseline: &[CohortDayRow],
    today: &[CohortTodayRow],
    sigma: f64,
    limit: usize,
) -> Vec<AnomalyResult> {
    let mut baseline_by_cohort: HashMap<String, Vec<f64>> = HashMap::new();
    for row in baseline {
        let key = cohort_key(row.cohort_kind, &row.cohort_value);
        baseline_by_cohort.entry(key).or_default().push(row.count as f64);
    }

    let mut out: Vec<AnomalyResult> = Vec::new();
    for t in today {
        let key = cohort_key(t.cohort_kind, &t.cohort_value);
        let samples = baseline_by_cohort.get(&key).cloned().unwrap_or_default();
        let (mean, stddev) = mean_stddev(&samples);

        let (is_anomaly, sigma_observed) = if stddev > 0.0 {
            let threshold = mean + sigma * stddev;
            (t.count as f64 > threshold, (t.count as f64 - mean) / stddev)
        } else {
            // Zero-stddev fallback (or empty baseline). Sigma is undefined; we
            // use (count - mean) as a finite sort proxy and require
            // count > mean + 1 to avoid surfacing every 1-page-touched cohort.
            (t.count as f64 > mean + 1.0, t.count as f64 - mean)
        };

        if !is_anomaly {
            continue;
        }
        out.push(AnomalyResult {
            cohort_kind: t.cohort_kind,
            cohort_value: t.cohort_value.clone(),
            count: t.count,
            baseline_mean: mean,
            baseline_stddev: stddev,
            sigma_observed,
            page_slugs: t.page_slugs.iter().take(50).cloned().collect(),
        });
    }

    out.sort_by(|a, b| {
        b.sigma_observed
            .partial_cmp(&a.sigma_observed)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    out.truncate(limit);
    out
}

/// Stable cohort key. Uses `\x1f` (unit separator) — a byte that can't appear
/// in tags or `PageType` values.
pub fn cohort_key(kind: CohortKind, value: &str) -> String {
    format!("{}\u{1f}{}", kind.as_str(), value)
}

/// Default cap on anomalies returned (matches TS `computeAnomaliesFromBuckets`
/// default `limit = 20`).
pub const DEFAULT_ANOMALY_LIMIT: usize = 20;

/// Options for `BrainEngine::find_anomalies`. Mirrors TS `AnomaliesOpts`.
#[derive(Debug, Clone, Default)]
pub struct AnomaliesOpts {
    /// Target day (YYYY-MM-DD). Defaults to today (UTC).
    pub since: Option<String>,
    /// Baseline window in days. Defaults to 30. Clamped to >= 1.
    pub lookback_days: Option<u32>,
    /// Sigma threshold multiplier. Defaults to 3.0.
    pub sigma: Option<f64>,
}

/// Resolved date windows for `find_anomalies`.
///
/// Returns, in order:
/// * `baseline_from` — full RFC3339 `YYYY-MM-DDTHH:MM:SSZ` lower bound
///   (inclusive) of the baseline window, matching the stored `updated_at` format.
/// * `baseline_to` — exclusive upper bound of the baseline window (= target day).
/// * `today_from` — inclusive lower bound of the target day.
/// * `today_to` — exclusive upper bound of the target day (= target day + 1).
/// * `window_days` — every `YYYY-MM-DD` in `[baseline_start, target_day)`,
///   used to zero-fill the densified baseline so rare cohorts don't get
///   sparse-day-biased baselines (codex C4#6, same as the TS SQL `CROSS JOIN days`).
/// * `sigma` — effective threshold (default 3.0).
/// * `limit` — effective cap (default [`DEFAULT_ANOMALY_LIMIT`]).
///
/// All three engine backends (Libsql, InMemory, Postgres) share this resolver
/// so the window semantics are identical across dialects.
pub fn resolve_anomaly_windows(
    opts: &AnomaliesOpts,
) -> crate::Result<(String, String, String, String, Vec<String>, f64, usize)> {
    use chrono::{Duration, NaiveDate, Utc};

    let sigma = opts.sigma.unwrap_or(3.0);
    let lookback = std::cmp::max(1, opts.lookback_days.unwrap_or(30)) as i64;
    let since = match &opts.since {
        Some(s) => NaiveDate::parse_from_str(s, "%Y-%m-%d").map_err(|e| {
            crate::Error::new(
                "InvalidArgument",
                "invalid_argument",
                format!("invalid --since {s}: {e}"),
            )
        })?,
        None => Utc::now().date_naive(),
    };
    let since_end = since + Duration::days(1);
    let baseline_start = since - Duration::days(lookback);

    let day_str = |d: NaiveDate| d.format("%Y-%m-%d").to_string();
    let iso_midnight = |d: NaiveDate| format!("{}T00:00:00Z", day_str(d));

    let baseline_from = iso_midnight(baseline_start);
    let baseline_to = iso_midnight(since);
    let today_from = iso_midnight(since);
    let today_to = iso_midnight(since_end);
    let window_days: Vec<String> = (0..lookback as u32)
        .map(|i| day_str(baseline_start + Duration::days(i as i64)))
        .collect();

    Ok((
        baseline_from,
        baseline_to,
        today_from,
        today_to,
        window_days,
        sigma,
        DEFAULT_ANOMALY_LIMIT,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn day(kind: CohortKind, value: &str, day: &str, count: i64) -> CohortDayRow {
        CohortDayRow {
            cohort_kind: kind,
            cohort_value: value.to_string(),
            day: day.to_string(),
            count,
        }
    }

    fn today(kind: CohortKind, value: &str, count: i64, slugs: &[&str]) -> CohortTodayRow {
        CohortTodayRow {
            cohort_kind: kind,
            cohort_value: value.to_string(),
            count,
            page_slugs: slugs.iter().map(|s| s.to_string()).collect(),
        }
    }

    #[test]
    fn mean_stddev_empty() {
        assert_eq!(mean_stddev(&[]), (0.0, 0.0));
    }

    #[test]
    fn mean_stddev_single_sample_has_zero_variance() {
        // Sample stddev (n-1) of a single point is 0, never "no variance".
        assert_eq!(mean_stddev(&[7.0]), (7.0, 0.0));
    }

    #[test]
    fn mean_stddev_known_population() {
        // [2,4,4,4,5,5,7,9] -> mean 5, sample stddev = sqrt(32/7) ≈ 2.13809.
        let (mean, std) = mean_stddev(&[2.0, 4.0, 4.0, 4.0, 5.0, 5.0, 7.0, 9.0]);
        assert!((mean - 5.0).abs() < 1e-9);
        assert!((std - (32.0_f64 / 7.0).sqrt()).abs() < 1e-9);
    }

    #[test]
    fn cohort_key_uses_unit_separator() {
        assert_eq!(cohort_key(CohortKind::Tag, "rust"), "tag\u{1f}rust");
        assert_eq!(cohort_key(CohortKind::Type, "person"), "type\u{1f}person");
    }

    #[test]
    fn cohort_kind_serde_lowercase() {
        assert_eq!(serde_json::to_string(&CohortKind::Tag).unwrap(), "\"tag\"");
        assert_eq!(serde_json::to_string(&CohortKind::Type).unwrap(), "\"type\"");
    }

    #[test]
    fn no_anomaly_when_count_within_baseline() {
        // Baseline ~5/day, today = 6, sigma 3 -> threshold 5 + 3*sqrt(var).
        let baseline: Vec<CohortDayRow> = (0..10)
            .map(|i| day(CohortKind::Tag, "x", &format!("2026-01-{:02}", i + 1), 5))
            .collect();
        let today = vec![today(CohortKind::Tag, "x", 6, &["a"])];
        let out = compute_anomalies_from_buckets(&baseline, &today, 3.0, 20);
        assert!(out.is_empty(), "6 is within 3σ of a flat-5 baseline");
    }

    #[test]
    fn spike_is_anomalous_with_positive_sigma() {
        let baseline: Vec<CohortDayRow> = (0..10)
            .map(|i| day(CohortKind::Tag, "x", &format!("2026-01-{:02}", i + 1), 5))
            .collect();
        // today = 50 -> far above mean+3σ.
        let today = vec![today(CohortKind::Tag, "x", 50, &["a", "b"])];
        let out = compute_anomalies_from_buckets(&baseline, &today, 3.0, 20);
        assert_eq!(out.len(), 1);
        let r = &out[0];
        assert_eq!(r.cohort_value, "x");
        assert_eq!(r.count, 50);
        assert!((r.baseline_mean - 5.0).abs() < 1e-9);
        assert!(r.sigma_observed > 0.0);
        assert_eq!(r.page_slugs, vec!["a", "b"]);
    }

    #[test]
    fn zero_stddev_fallback_requires_count_gt_mean_plus_one() {
        // Flat baseline of 5 every day -> stddev 0 -> fallback threshold = 6.
        let baseline: Vec<CohortDayRow> = (0..10)
            .map(|i| day(CohortKind::Tag, "x", &format!("2026-01-{:02}", i + 1), 5))
            .collect();
        // count == 6 is NOT > mean+1 (==6), so not an anomaly.
        let at_six = compute_anomalies_from_buckets(
            &baseline,
            &[today(CohortKind::Tag, "x", 6, &["a"])],
            3.0,
            20,
        );
        assert!(at_six.is_empty());
        // count == 7 IS > mean+1.
        let at_seven = compute_anomalies_from_buckets(
            &baseline,
            &[today(CohortKind::Tag, "x", 7, &["a"])],
            3.0,
            20,
        );
        assert_eq!(at_seven.len(), 1);
        // sigma_observed proxy = count - mean = 2.
        assert!((at_seven[0].sigma_observed - 2.0).abs() < 1e-9);
    }

    #[test]
    fn brand_new_cohort_surfaces_when_count_ge_2() {
        // No baseline rows at all -> baseline_mean 0, fallback threshold = 1.
        let baseline: Vec<CohortDayRow> = vec![];
        // count == 1 -> not > 1, suppressed.
        let one = compute_anomalies_from_buckets(
            &baseline,
            &[today(CohortKind::Type, "person", 1, &["a"])],
            3.0,
            20,
        );
        assert!(one.is_empty());
        // count == 2 -> > 1, surfaced.
        let two = compute_anomalies_from_buckets(
            &baseline,
            &[today(CohortKind::Type, "person", 2, &["a", "b"])],
            3.0,
            20,
        );
        assert_eq!(two.len(), 1);
        assert!((two[0].baseline_mean).abs() < 1e-9);
        assert!((two[0].baseline_stddev).abs() < 1e-9);
    }

    #[test]
    fn sorts_by_sigma_desc_and_truncates_to_limit() {
        // Build three cohorts with very different spikes; verify order + cap.
        let baseline: Vec<CohortDayRow> = (0..10)
            .map(|i| day(CohortKind::Tag, "shared", &format!("2026-01-{:02}", i + 1), 5))
            .collect();
        let today = vec![
            today(CohortKind::Tag, "small", 12, &["s"]),  // mild spike
            today(CohortKind::Tag, "big", 200, &["b"]),   // huge spike
            today(CohortKind::Tag, "mid", 40, &["m"]),    // medium spike
        ];
        let out = compute_anomalies_from_buckets(&baseline, &today, 3.0, 2);
        assert_eq!(out.len(), 2);
        // Highest sigma first.
        assert_eq!(out[0].cohort_value, "big");
        assert_eq!(out[1].cohort_value, "mid");
        // Limit caps the return set.
        let out_all = compute_anomalies_from_buckets(&baseline, &today, 3.0, 20);
        assert_eq!(out_all.len(), 3);
    }

    #[test]
    fn page_slugs_capped_at_50() {
        let baseline: Vec<CohortDayRow> = vec![];
        let slugs: Vec<String> = (0..120).map(|i| format!("slug-{i}")).collect();
        let today = vec![CohortTodayRow {
            cohort_kind: CohortKind::Tag,
            cohort_value: "burst".to_string(),
            count: 120,
            page_slugs: slugs,
        }];
        let out = compute_anomalies_from_buckets(&baseline, &today, 3.0, 20);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].page_slugs.len(), 50);
    }

    #[test]
    fn non_anomalous_cohort_is_skipped_not_zeroed() {
        let baseline: Vec<CohortDayRow> = (0..10)
            .map(|i| day(CohortKind::Tag, "calm", &format!("2026-01-{:02}", i + 1), 5))
            .collect();
        let today = vec![
            today(CohortKind::Tag, "calm", 5, &["a"]), // within baseline
            today(CohortKind::Tag, "loud", 90, &["b"]), // spike
        ];
        let out = compute_anomalies_from_buckets(&baseline, &today, 3.0, 20);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].cohort_value, "loud");
    }

    #[test]
    fn anomaly_result_serializes_snake_case() {
        let r = AnomalyResult {
            cohort_kind: CohortKind::Tag,
            cohort_value: "rust".to_string(),
            count: 42,
            baseline_mean: 5.0,
            baseline_stddev: 1.5,
            sigma_observed: 24.0,
            page_slugs: vec!["a".to_string()],
        };
        let json = serde_json::to_value(&r).unwrap();
        assert_eq!(json["cohort_kind"], "tag");
        assert_eq!(json["cohort_value"], "rust");
        assert_eq!(json["baseline_mean"], 5.0);
        assert_eq!(json["sigma_observed"], 24.0);
        assert_eq!(json["page_slugs"][0], "a");
    }
}
