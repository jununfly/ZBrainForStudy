//! takes-quality-eval/trend — DB-backed quality-over-time view.
//!
//! Faithful port of TS `trend.ts`. Reads `eval_takes_quality_runs` ordered by
//! `created_at` DESC, optionally filtered by `rubric_version` (segregates
//! rubric epochs so a v1.0 → v1.1 transition doesn't lie about quality moving).
//! Plain text table for stdout; JSON for programmatic consumers.

use crate::engine::BrainEngine;
use crate::eval::takes_quality::receipt::TakesQualityRunRow;
use serde::Serialize;

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct TrendRow {
    pub id: String,
    pub ts: String,
    pub rubric_version: String,
    pub verdict: String,
    pub overall_score: Option<f64>,
    pub cost_usd: f64,
    pub corpus_sha8: String,
}

#[derive(Debug, Clone, Default)]
pub struct TrendOpts {
    /// Number of days to look back. Default 30.
    pub days: Option<i64>,
    /// Filter to a specific rubric version (default: all).
    pub rubric_version: Option<String>,
    /// Hard cap on rows returned. Default 20, max 200.
    pub limit: Option<usize>,
}

/// Load recent runs from the DB, newest first.
pub async fn load_trend(
    engine: &dyn BrainEngine,
    opts: &TrendOpts,
) -> crate::Result<Vec<TrendRow>> {
    let days = opts.days.unwrap_or(30);
    let rows: Vec<TakesQualityRunRow> = engine.load_takes_quality_trend(days).await?;
    let rows = if let Some(rv) = &opts.rubric_version {
        rows.into_iter().filter(|r| &r.rubric_version == rv).collect()
    } else {
        rows
    };
    let limit = opts.limit.unwrap_or(20).min(200);
    Ok(rows.into_iter().take(limit).map(TrendRow::from).collect())
}

impl From<TakesQualityRunRow> for TrendRow {
    fn from(r: TakesQualityRunRow) -> Self {
        TrendRow {
            id: r.run_id,
            ts: r.ran_at,
            rubric_version: r.rubric_version,
            verdict: r.verdict,
            overall_score: r.overall_score,
            cost_usd: r.cost_usd,
            corpus_sha8: r.corpus_sha8,
        }
    }
}

/// Render the trend table as plain text for stdout.
pub fn render_trend_table(rows: &[TrendRow]) -> String {
    if rows.is_empty() {
        return "No takes-quality runs recorded yet. Run `zbrain eval takes-quality run` to get started."
            .to_string();
    }
    let header = ["ts", "rubric", "verdict", "overall", "cost", "corpus"].join("  ");
    let sep = "─".repeat(header.len() + 8);
    let lines: Vec<String> = rows
        .iter()
        .map(|r| {
            [
                r.ts.chars().take(19).collect::<String>(),
                pad(&r.rubric_version, 6),
                pad(&r.verdict, 12),
                pad(&format!("{:.1}", r.overall_score.unwrap_or(0.0)), 6),
                pad(&format!("${:.2}", r.cost_usd), 7),
                r.corpus_sha8.clone(),
            ]
            .join("  ")
        })
        .collect();
    [header, sep, lines.join("\n")].join("\n")
}

fn pad(s: &str, width: usize) -> String {
    format!("{s:<width$}", s = s)
}
