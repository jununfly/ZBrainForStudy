//! 1-3-3-6 — `runAbTrial`: A/B harness for `zbrain think`.
//!
//! Port of `src/core/calibration/think-ab.ts` (v0.36.1.0, T18 / D19). Each
//! invocation runs think TWICE on the same question (baseline vs
//! `--with-calibration`), records the preference, and writes both answers to
//! `think_ab_results`. `build_ab_report` aggregates win/loss over a time window
//! and flags `calibration_net_negative` when the with-calibration variant loses
//! >55% of decisive trials (n >= 20).
//!
//! Design (locked via grill-me, 2026-07-27):
//! - `ThinkRunner` + `PreferenceResolver` are injected `async` traits. Production
//!   wires the real `runThink` into `ThinkRunner`; Rust has no `runThink` yet, so
//!   the production wiring is deferred — only the DI trait + orchestration + DB
//!   write port now.
//! - `source_id` is supplied by the caller (must reference an existing `sources`
//!   row). A FK violation returns an error — we never fabricate a source (G52).
//! - `build_ab_report` computes the time-window cutoff in Rust
//!   (`Utc::now() - days`) and binds `WHERE ran_at >= $1`. ISO8601 sorts
//!   lexicographically, so the same SQL works on both libsql and postgres.

use std::fmt;

use async_trait::async_trait;
use chrono::{Duration, Utc};
use serde::{Deserialize, Serialize};

use crate::calibration_queries::{CalibrationQueries, ThinkAbInsert};
use crate::error::{Error, Result as ZbResult};

/// Preference a user expressed after seeing both A/B answers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AbPreference {
    Baseline,
    WithCalibration,
    Neither,
    Tie,
}

impl AbPreference {
    /// DB string used by the `preferred` CHECK constraint.
    pub fn as_db(self) -> &'static str {
        match self {
            AbPreference::Baseline => "baseline",
            AbPreference::WithCalibration => "with_calibration",
            AbPreference::Neither => "neither",
            AbPreference::Tie => "tie",
        }
    }

    /// Parse a DB string back into the enum; unknown values default to Baseline.
    pub fn from_db(s: &str) -> AbPreference {
        match s {
            "with_calibration" => AbPreference::WithCalibration,
            "neither" => AbPreference::Neither,
            "tie" => AbPreference::Tie,
            _ => AbPreference::Baseline,
        }
    }
}

/// Answer returned by a single think run.
#[derive(Debug, Clone)]
pub struct ThinkRunAnswer {
    pub answer: String,
    pub model_used: Option<String>,
}

/// Runs `think` once. Production wires the real `runThink` here.
#[async_trait]
pub trait ThinkRunner: Send + Sync {
    async fn run(&self, question: &str, with_calibration: bool) -> Result<ThinkRunAnswer, ThinkAbError>;
}

/// Resolves which variant the user preferred. Production prompts via stdin;
/// tests inject a non-interactive resolver.
#[async_trait]
pub trait PreferenceResolver: Send + Sync {
    async fn resolve(&self, baseline: &str, with_calibration: &str) -> Result<AbPreference, ThinkAbError>;
}

/// Error type for the injected A/B traits. Converted to the crate `Error` at the
/// `run_ab_trial` boundary (the harness never swallows these).
#[derive(Debug, Clone)]
pub struct ThinkAbError(pub String);

impl fmt::Display for ThinkAbError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "think_ab_error: {}", self.0)
    }
}

impl std::error::Error for ThinkAbError {}

/// Inputs for [`run_ab_trial`].
pub struct AbRunInput<'a> {
    pub question: String,
    /// Must reference an existing `sources(id)` row. FK violation -> error (G52:
    /// we never fabricate a source).
    pub source_id: String,
    /// Typed store for the `think_ab_results` write (libsql / postgres impl
    /// `CalibrationQueries`; no `execute_raw` escape hatch — project rule).
    pub queries: &'a dyn CalibrationQueries,
    /// Runs think baseline + with-calibration. Production = real `runThink`.
    pub think_runner: std::sync::Arc<dyn ThinkRunner>,
    /// Records the user's preference. Production = stdin prompt.
    pub preference_resolver: std::sync::Arc<dyn PreferenceResolver>,
    pub notes: Option<String>,
}

/// Output of [`run_ab_trial`].
#[derive(Debug, Clone)]
pub struct AbRunResult {
    pub baseline_answer: String,
    pub with_calibration_answer: String,
    pub preferred: AbPreference,
    pub model_used: Option<String>,
    pub row_id: Option<i64>,
}

/// Run one A/B trial: think twice, get the preference, write the row.
pub async fn run_ab_trial(input: &AbRunInput<'_>) -> ZbResult<AbRunResult> {
    let baseline = input
        .think_runner
        .run(&input.question, false)
        .await
        .map_err(|e| Error::engine(format!("think_runner (baseline): {}", e.0)))?;
    let with_calibration = input
        .think_runner
        .run(&input.question, true)
        .await
        .map_err(|e| Error::engine(format!("think_runner (with_calibration): {}", e.0)))?;

    let preferred = input
        .preference_resolver
        .resolve(&baseline.answer, &with_calibration.answer)
        .await
        .map_err(|e| Error::engine(format!("preference_resolver: {}", e.0)))?;

    let model_id = baseline.model_used.clone().or(with_calibration.model_used.clone());

    let row_id = input
        .queries
        .insert_think_ab_result(&ThinkAbInsert {
            source_id: &input.source_id,
            question: &input.question,
            baseline_answer: &baseline.answer,
            with_calibration_answer: &with_calibration.answer,
            preferred: preferred.as_db(),
            model_id: model_id.as_deref(),
            notes: input.notes.as_deref(),
        })
        .await?;

    Ok(AbRunResult {
        baseline_answer: baseline.answer,
        with_calibration_answer: with_calibration.answer,
        preferred,
        model_used: model_id,
        row_id,
    })
}

/// Win/loss breakdown over a recent window.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AbReportResult {
    pub total_trials: u32,
    pub baseline_wins: u32,
    pub with_calibration_wins: u32,
    pub ties: u32,
    pub neither: u32,
    /// Win rate for `--with-calibration` as a fraction of decisive trials
    /// (excludes neither/tie). `None` when there are no decisive trials.
    pub with_calibration_win_rate: Option<f64>,
    /// When true, the doctor surface flags `calibration_net_negative`.
    pub net_negative: bool,
    pub decisive_trials: u32,
}

/// Options for [`build_ab_report`].
#[derive(Debug, Clone, Default)]
pub struct AbReportOpts {
    /// Window length in days (default 30, matching TS).
    pub days: u32,
}

/// Aggregate `think_ab_results` over the last `opts.days` days. Pure aggregation
/// over the row set; the time-window cutoff is computed in Rust (ISO8601, which
/// sorts lexicographically so the same SQL runs on libsql + postgres).
pub async fn build_ab_report(
    queries: &dyn CalibrationQueries,
    opts: &AbReportOpts,
) -> ZbResult<AbReportResult> {
    let days = if opts.days == 0 { 30 } else { opts.days };
    let cutoff = (Utc::now() - Duration::days(days as i64))
        .format("%Y-%m-%dT%H:%M:%SZ")
        .to_string();

    let rows = queries.think_ab_preference_counts(&cutoff).await?;

    let mut baseline_wins = 0u32;
    let mut with_calibration_wins = 0u32;
    let mut ties = 0u32;
    let mut neither = 0u32;
    for (preferred, count) in &rows {
        let count = (*count).min(u32::MAX as u64) as u32;
        match AbPreference::from_db(preferred) {
            AbPreference::Baseline => baseline_wins += count,
            AbPreference::WithCalibration => with_calibration_wins += count,
            AbPreference::Tie => ties += count,
            AbPreference::Neither => neither += count,
        }
    }

    let total_trials = baseline_wins + with_calibration_wins + ties + neither;
    let decisive_trials = baseline_wins + with_calibration_wins;
    let with_calibration_win_rate = if decisive_trials > 0 {
        Some(with_calibration_wins as f64 / decisive_trials as f64)
    } else {
        None
    };
    // calibration_net_negative threshold (D19): with-calibration loses
    // >55% of decisive trials over a sample of n >= 20.
    let net_negative = decisive_trials >= 20
        && with_calibration_win_rate.map_or(false, |r| r < 0.45);

    Ok(AbReportResult {
        total_trials,
        baseline_wins,
        with_calibration_wins,
        ties,
        neither,
        with_calibration_win_rate,
        net_negative,
        decisive_trials,
    })
}

/// Human-readable report (mirrors TS `formatAbReport`).
pub fn format_ab_report(report: &AbReportResult, days: u32) -> String {
    let days = if days == 0 { 30 } else { days };
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
    if let Some(rate) = report.with_calibration_win_rate {
        lines.push(format!(
            "  With-calibration win rate (decisive trials only): {:.1}% (n={})",
            rate * 100.0,
            report.decisive_trials
        ));
    }
    if report.net_negative {
        lines.push(String::new());
        lines.push(
            "⚠ calibration_net_negative: with-calibration is losing more than half of decisive trials."
                .to_string(),
        );
        lines.push("  Consider tuning the anti-bias prompt rewrite or".to_string());
        lines.push("  disabling --with-calibration via config until you tune.".to_string());
    }
    lines.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn preference_db_round_trip() {
        for p in [
            AbPreference::Baseline,
            AbPreference::WithCalibration,
            AbPreference::Neither,
            AbPreference::Tie,
        ] {
            assert_eq!(AbPreference::from_db(p.as_db()), p);
        }
        // Unknown -> Baseline.
        assert_eq!(AbPreference::from_db("bogus"), AbPreference::Baseline);
    }

    #[test]
    fn format_ab_report_empty() {
        let r = AbReportResult {
            total_trials: 0,
            baseline_wins: 0,
            with_calibration_wins: 0,
            ties: 0,
            neither: 0,
            with_calibration_win_rate: None,
            net_negative: false,
            decisive_trials: 0,
        };
        let s = format_ab_report(&r, 30);
        assert!(s.contains("No data yet"));
    }

    #[test]
    fn format_ab_report_net_negative() {
        let r = AbReportResult {
            total_trials: 25,
            baseline_wins: 20,
            with_calibration_wins: 5,
            ties: 0,
            neither: 0,
            with_calibration_win_rate: Some(0.2),
            net_negative: true,
            decisive_trials: 25,
        };
        let s = format_ab_report(&r, 30);
        assert!(s.contains("calibration_net_negative"));
    }
}
