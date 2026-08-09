//! `eval run-all` orchestration types + aggregation.
//!
//! Redesign (2026-08-09, 1-1-4 stage 6): the TS `eval run-all` was a
//! v0.32.3 orchestrator *stub* that only wrote `status: "skipped"` audit rows
//! — its per-suite sweep depended on TS-only `SearchMode`s (conservative /
//! balanced / tokenmax) that were never ported to Rust. Instead of porting
//! the stub, `run-all` now genuinely orchestrates the verdict-producing eval
//! gates that already exist in Rust — `gate`, `replay`, `whoknows` — and
//! aggregates their verdicts into a single run report.
//!
//! The data-lifecycle commands (`export` / `prune`) are intentionally *not*
//! wrapped: they are not quality gates and have no pass/fail verdict, so they
//! don't belong in a gate-aggregation report. The [`RunAllCheck`] shape is
//! open enough to add more gates later.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// Verdict of a single check inside a run-all report.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RunAllStatus {
    Passed,
    Failed,
    Errored,
    Skipped,
}

/// One gate's outcome within a run-all report.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RunAllCheck {
    pub name: String,
    pub status: RunAllStatus,
    /// Free-form metrics surfaced for the report (hit rates, Jaccard, etc.).
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub metrics: BTreeMap<String, serde_json::Value>,
    /// Optional human note (e.g. a skip reason).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
    /// Present when the check itself threw (status == `Errored`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// The aggregated report produced by `eval run-all`.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RunAllReport {
    pub schema_version: u8,
    pub run_id: String,
    /// RFC3339 timestamp (best-effort; set by the caller).
    pub ran_at: String,
    /// Short git commit the run was produced against (best-effort).
    pub commit: String,
    pub overall_passed: bool,
    pub checks: Vec<RunAllCheck>,
    pub duration_ms: u64,
}

/// Aggregate a list of per-check results into a single run report.
///
/// `overall_passed` is true iff every check is `Passed` or `Skipped`. A
/// `Skipped` check neither passes nor fails the run (it needs inputs the
/// operator didn't supply). `Failed` / `Errored` checks fail the run.
pub fn assemble_run_all_report(
    run_id: String,
    ran_at: String,
    commit: String,
    checks: Vec<RunAllCheck>,
    duration_ms: u64,
) -> RunAllReport {
    let overall_passed = checks
        .iter()
        .all(|c| matches!(c.status, RunAllStatus::Passed | RunAllStatus::Skipped));
    RunAllReport {
        schema_version: 1,
        run_id,
        ran_at,
        commit,
        overall_passed,
        checks,
        duration_ms,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn check(name: &str, status: RunAllStatus) -> RunAllCheck {
        RunAllCheck {
            name: name.to_string(),
            status,
            metrics: BTreeMap::new(),
            detail: None,
            error: None,
        }
    }

    #[test]
    fn all_passed_is_overall_pass() {
        let r = assemble_run_all_report(
            "r1".into(),
            "t".into(),
            "abc".into(),
            vec![check("gate", RunAllStatus::Passed), check("replay", RunAllStatus::Passed)],
            10,
        );
        assert!(r.overall_passed);
    }

    #[test]
    fn one_failed_fails_overall() {
        let r = assemble_run_all_report(
            "r1".into(),
            "t".into(),
            "abc".into(),
            vec![check("gate", RunAllStatus::Passed), check("replay", RunAllStatus::Failed)],
            10,
        );
        assert!(!r.overall_passed);
    }

    #[test]
    fn errored_fails_overall_but_skipped_is_tolerated() {
        let r = assemble_run_all_report(
            "r1".into(),
            "t".into(),
            "abc".into(),
            vec![check("gate", RunAllStatus::Errored), check("whoknows", RunAllStatus::Skipped)],
            10,
        );
        assert!(!r.overall_passed);

        let ok = assemble_run_all_report(
            "r2".into(),
            "t".into(),
            "abc".into(),
            vec![check("gate", RunAllStatus::Skipped), check("whoknows", RunAllStatus::Skipped)],
            10,
        );
        assert!(ok.overall_passed);
    }

    #[test]
    fn report_round_trips_through_json() {
        let r = assemble_run_all_report(
            "r1".into(),
            "2026-08-09T00:00:00Z".into(),
            "abc1234".into(),
            vec![check("gate", RunAllStatus::Passed)],
            42,
        );
        let json = serde_json::to_string(&r).unwrap();
        let back: RunAllReport = serde_json::from_str(&json).unwrap();
        assert_eq!(back.run_id, "r1");
        assert_eq!(back.checks.len(), 1);
        assert_eq!(back.checks[0].status, RunAllStatus::Passed);
    }
}
