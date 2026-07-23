//! Trajectory derived-metric math.
//!
//! Faithful Rust port of `src/core/trajectory.ts` (`computeTrajectoryStats`,
//! `detectRegressions`, `computeDriftScore`, `cosineSim`). These run on the
//! points returned by `BrainEngine::find_trajectory` to produce the
//! `{ regressions, drift_score }` block surfaced by the `find-trajectory` op.

use crate::types::{TrajectoryPoint, TrajectoryRegression, TrajectoryStats};

/// Best-effort parse of a fact `embedding` column into `Vec<f32>`.
/// Accepts either `[0.1,0.2]` (JSON, sqlite) or `{0.1,0.2}` (postgres `REAL[]`
/// text cast) array literals, or whitespace-separated scalars. Returns `None`
/// when null / unparseable — graceful degradation means `drift_score` simply
/// comes back `null` (matches TS behavior when <3 points carry embeddings).
pub fn parse_embedding_text(raw: Option<String>) -> Option<Vec<f32>> {
    let s = raw?;
    let trimmed = s.trim();
    if trimmed.is_empty() || trimmed == "null" {
        return None;
    }
    let inner = trimmed
        .strip_prefix('[')
        .and_then(|s| s.strip_suffix(']'))
        .or_else(|| trimmed.strip_prefix('{').and_then(|s| s.strip_suffix('}')))
        .unwrap_or(trimmed);
    let parts: Vec<f32> = inner
        .split(',')
        .map(|t| t.trim())
        .filter(|t| !t.is_empty())
        .filter_map(|t| t.parse::<f32>().ok())
        .collect();
    if parts.is_empty() {
        None
    } else {
        Some(parts)
    }
}

/// Extract the `YYYY-MM-DD` prefix from an ISO timestamp string (or pass
/// through an already-short date). Mirrors TS `toISODate`.
pub fn iso_date_prefix(s: &str) -> String {
    if s.len() >= 10 {
        s[..10].to_string()
    } else {
        s.to_string()
    }
}

/// Schema version for the trajectory + scorecard JSON contract. Additive-only.
/// Mirrors TS `TRAJECTORY_SCHEMA_VERSION`.
pub const TRAJECTORY_SCHEMA_VERSION: u8 = 1;

/// Default regression threshold. A consecutive pair fires when
/// `delta <= -threshold`. Mirrors TS `DEFAULT_REGRESSION_THRESHOLD`.
pub const DEFAULT_REGRESSION_THRESHOLD: f64 = 0.10;

/// Read the regression threshold from `ZBRAIN_TRAJECTORY_REGRESSION_THRESHOLD`
/// with fallback to the locked default. Invalid input falls back silently —
/// the threshold is a soft tuning knob, not a correctness gate.
/// Mirrors TS `resolveRegressionThreshold`.
pub fn resolve_regression_threshold() -> f64 {
    match std::env::var("ZBRAIN_TRAJECTORY_REGRESSION_THRESHOLD") {
        Ok(raw) => match raw.trim().parse::<f64>() {
            Ok(n) if n > 0.0 && n < 1.0 => n,
            _ => DEFAULT_REGRESSION_THRESHOLD,
        },
        Err(_) => DEFAULT_REGRESSION_THRESHOLD,
    }
}

/// Cosine similarity between two equal-length vectors. Returns 0 when either
/// vector has length zero or lengths differ (defensive — never throws).
/// Mirrors TS `cosineSim`.
pub fn cosine_sim(a: &[f32], b: &[f32]) -> f64 {
    if a.is_empty() || b.is_empty() || a.len() != b.len() {
        return 0.0;
    }
    let mut dot = 0.0_f64;
    let mut na = 0.0_f64;
    let mut nb = 0.0_f64;
    for i in 0..a.len() {
        let av = a[i] as f64;
        let bv = b[i] as f64;
        dot += av * bv;
        na += av * av;
        nb += bv * bv;
    }
    if na == 0.0 || nb == 0.0 {
        return 0.0;
    }
    dot / (na.sqrt() * nb.sqrt())
}

/// Detect chronological regressions in a sorted trajectory.
///
/// Iterates per-metric so interleaved series (mrr + arr + team_size) don't
/// trip false regressions across metric boundaries. Within each metric, walks
/// consecutive value pairs; a pair fires when `(new - old) / old <= -threshold`.
/// Mirrors TS `detectRegressions`.
pub fn detect_regressions(points: &[TrajectoryPoint], threshold: f64) -> Vec<TrajectoryRegression> {
    let mut out: Vec<TrajectoryRegression> = Vec::new();

    // Group by metric; each metric's regression detection is independent.
    let mut by_metric: std::collections::BTreeMap<String, Vec<&TrajectoryPoint>> =
        std::collections::BTreeMap::new();
    for p in points {
        let metric = match &p.metric {
            Some(m) if !m.is_empty() => m,
            _ => continue,
        };
        by_metric.entry(metric.clone()).or_default().push(p);
    }

    for (metric, series) in by_metric {
        for i in 1..series.len() {
            let older = series[i - 1];
            let newer = series[i];
            let old_val = match older.value {
                Some(v) if v.is_finite() => v,
                _ => continue,
            };
            let new_val = match newer.value {
                Some(v) if v.is_finite() => v,
                _ => continue,
            };
            // Guard division-by-zero: a metric starting at exactly 0 can't
            // compute a relative delta. Skip.
            if old_val == 0.0 {
                continue;
            }
            let delta = (new_val - old_val) / old_val;
            if delta <= -threshold {
                out.push(TrajectoryRegression {
                    metric: metric.clone(),
                    from_value: old_val,
                    from_date: iso_date(older.valid_from.as_deref()),
                    to_value: new_val,
                    to_date: iso_date(newer.valid_from.as_deref()),
                    delta_pct: delta,
                });
            }
        }
    }
    out
}

/// Compute the drift score over the trajectory's existing embeddings.
///
/// `1 - mean(cosine(emb[i], emb[i-1]))` clamped to `[0, 1]`. Range: 0 = narrative
/// stable text-wise; 1 = every consecutive claim is unrelated to the prior.
/// Returns `None` when fewer than 3 points carry non-empty embeddings — the
/// statistic is meaningless on tiny samples. Mirrors TS `computeDriftScore`.
pub fn compute_drift_score(points: &[TrajectoryPoint]) -> Option<f64> {
    let with_emb: Vec<&Vec<f32>> = points
        .iter()
        .filter_map(|p| p.embedding.as_ref())
        .filter(|e| !e.is_empty())
        .collect();
    if with_emb.len() < 3 {
        return None;
    }
    let mut sum_cos = 0.0_f64;
    let mut pairs = 0;
    for i in 1..with_emb.len() {
        sum_cos += cosine_sim(with_emb[i - 1], with_emb[i]);
        pairs += 1;
    }
    if pairs == 0 {
        return None;
    }
    let mean_cos = sum_cos / pairs as f64;
    let drift = 1.0 - mean_cos;
    if drift < 0.0 {
        Some(0.0)
    } else if drift > 1.0 {
        Some(1.0)
    } else {
        Some(drift)
    }
}

/// Compose the two derived metrics into a single `TrajectoryStats`.
/// Mirrors TS `computeTrajectoryStats`.
pub fn compute_trajectory_stats(
    points: &[TrajectoryPoint],
    threshold: f64,
) -> TrajectoryStats {
    TrajectoryStats {
        regressions: detect_regressions(points, threshold),
        drift_score: compute_drift_score(points),
    }
}

/// Extract the `YYYY-MM-DD` prefix from an ISO timestamp string (or pass
/// through an already-short date). Mirrors TS `toISODate`.
fn iso_date(s: Option<&str>) -> String {
    match s {
        Some(s) if s.len() >= 10 => s[..10].to_string(),
        Some(s) => s.to_string(),
        None => String::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::TrajectoryPoint;

    fn point(fact_id: i64, metric: &str, value: f64, date: &str) -> TrajectoryPoint {
        TrajectoryPoint {
            fact_id,
            valid_from: Some(date.to_string()),
            metric: Some(metric.to_string()),
            value: Some(value),
            unit: None,
            period: None,
            event_type: None,
            text: String::new(),
            source_session: None,
            source_markdown_slug: None,
            embedding: None,
        }
    }

    #[test]
    fn detects_single_regression() {
        let pts = vec![
            point(1, "mrr", 100.0, "2024-01-01"),
            point(2, "mrr", 80.0, "2024-02-01"), // -20% <= -0.10
        ];
        let regs = detect_regressions(&pts, DEFAULT_REGRESSION_THRESHOLD);
        assert_eq!(regs.len(), 1);
        assert_eq!(regs[0].metric, "mrr");
        assert!((regs[0].delta_pct + 0.20).abs() < 1e-9);
        assert_eq!(regs[0].from_date, "2024-01-01");
        assert_eq!(regs[0].to_date, "2024-02-01");
    }

    #[test]
    fn ignores_small_drops_and_zero_base() {
        // -5% is below the 10% threshold
        let small = vec![point(1, "arr", 100.0, "2024-01-01"), point(2, "arr", 95.0, "2024-02-01")];
        assert!(detect_regressions(&small, DEFAULT_REGRESSION_THRESHOLD).is_empty());
        // base == 0 can't compute relative delta
        let zero = vec![point(1, "x", 0.0, "2024-01-01"), point(2, "x", 5.0, "2024-02-01")];
        assert!(detect_regressions(&zero, DEFAULT_REGRESSION_THRESHOLD).is_empty());
    }

    #[test]
    fn drift_score_needs_three_embeddings() {
        // no embeddings -> None
        let pts = vec![point(1, "mrr", 1.0, "2024-01-01"), point(2, "mrr", 2.0, "2024-02-01")];
        assert_eq!(compute_drift_score(&pts), None);
    }

    #[test]
    fn drift_score_computed_when_embeddings_present() {
        let mk = |id: i64, e: Vec<f32>| TrajectoryPoint {
            embedding: Some(e),
            ..point(id, "mrr", 1.0, "2024-01-01")
        };
        let pts = vec![mk(1, vec![1.0, 0.0]), mk(2, vec![1.0, 0.0]), mk(3, vec![1.0, 0.0])];
        // identical embeddings => cosine 1 => drift 0
        assert!((compute_drift_score(&pts).unwrap() - 0.0).abs() < 1e-9);
    }
}
