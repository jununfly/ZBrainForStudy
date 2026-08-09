//! `eval compare` — diff two `run-all` reports and surface regressions.
//!
//! Redesign (2026-08-09, 1-1-4 stage 6): the TS `eval compare` read the stub
//! `eval-results.jsonl` audit trail and only ever rendered "no metric data
//! yet" (because `run-all` never produced real results). Here `compare` reads
//! two genuine [`RunAllReport`]s from `eval run-all` and reports, per check,
//! whether the verdict changed and whether it regressed.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

use super::run_all::{RunAllReport, RunAllStatus};

/// Per-check diff between a baseline and a current run.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CheckDiff {
    pub name: String,
    pub baseline: RunAllStatus,
    pub current: RunAllStatus,
    pub changed: bool,
    /// True iff the check was passing in baseline and is no longer passing
    /// (`Failed` / `Errored`) in current. A check that dropped out of the
    /// current run (treated as `Skipped`) is *changed* but not a quality
    /// regression.
    pub regression: bool,
}

/// The full compare report.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CompareReport {
    pub schema_version: u8,
    pub baseline_run_id: String,
    pub current_run_id: String,
    pub checks: Vec<CheckDiff>,
    pub any_regression: bool,
}

/// Compare two run-all reports, matching checks by name over the union of
/// names from both reports.
pub fn compare_reports(baseline: &RunAllReport, current: &RunAllReport) -> CompareReport {
    let mut base_status: BTreeMap<&str, RunAllStatus> = BTreeMap::new();
    for c in &baseline.checks {
        base_status.insert(c.name.as_str(), c.status);
    }

    let mut names: Vec<String> = baseline.checks.iter().map(|c| c.name.clone()).collect();
    for c in &current.checks {
        if !names.iter().any(|n| n == &c.name) {
            names.push(c.name.clone());
        }
    }

    let mut diffs: Vec<CheckDiff> = Vec::with_capacity(names.len());
    for name in &names {
        let baseline_status = *base_status.get(name.as_str()).unwrap_or(&RunAllStatus::Skipped);
        let current_status = current
            .checks
            .iter()
            .find(|c| &c.name == name)
            .map(|c| c.status)
            .unwrap_or(RunAllStatus::Skipped);

        let changed = baseline_status != current_status;
        let regression = baseline_status == RunAllStatus::Passed
            && (current_status == RunAllStatus::Failed || current_status == RunAllStatus::Errored);

        diffs.push(CheckDiff {
            name: name.clone(),
            baseline: baseline_status,
            current: current_status,
            changed,
            regression,
        });
    }

    let any_regression = diffs.iter().any(|d| d.regression);
    CompareReport {
        schema_version: 1,
        baseline_run_id: baseline.run_id.clone(),
        current_run_id: current.run_id.clone(),
        checks: diffs,
        any_regression,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::eval::run_all::{assemble_run_all_report, RunAllCheck};
    use std::collections::BTreeMap;

    fn passed(name: &str) -> RunAllCheck {
        RunAllCheck {
            name: name.to_string(),
            status: RunAllStatus::Passed,
            metrics: BTreeMap::new(),
            detail: None,
            error: None,
        }
    }

    fn failed(name: &str) -> RunAllCheck {
        RunAllCheck {
            name: name.to_string(),
            status: RunAllStatus::Failed,
            metrics: BTreeMap::new(),
            detail: None,
            error: None,
        }
    }

    #[test]
    fn identical_reports_have_no_change_and_no_regression() {
        let a = assemble_run_all_report(
            "base".into(),
            "t".into(),
            "c".into(),
            vec![passed("gate"), passed("replay")],
            1,
        );
        let b = assemble_run_all_report(
            "cur".into(),
            "t".into(),
            "c".into(),
            vec![passed("gate"), passed("replay")],
            1,
        );
        let cmp = compare_reports(&a, &b);
        assert!(!cmp.any_regression);
        assert!(cmp.checks.iter().all(|d| !d.changed));
    }

    #[test]
    fn passed_to_failed_is_a_regression() {
        let a = assemble_run_all_report("base".into(), "t".into(), "c".into(), vec![passed("gate")], 1);
        let b = assemble_run_all_report("cur".into(), "t".into(), "c".into(), vec![failed("gate")], 1);
        let cmp = compare_reports(&a, &b);
        let gate = cmp.checks.iter().find(|d| d.name == "gate").unwrap();
        assert!(gate.changed);
        assert!(gate.regression);
        assert!(cmp.any_regression);
    }

    #[test]
    fn skipped_to_failed_is_changed_but_not_a_quality_regression() {
        let a = assemble_run_all_report("base".into(), "t".into(), "c".into(), vec![passed("gate")], 1);
        // current run dropped the check entirely (treated as Skipped).
        let b = assemble_run_all_report("cur".into(), "t".into(), "c".into(), vec![], 1);
        let cmp = compare_reports(&a, &b);
        let gate = cmp.checks.iter().find(|d| d.name == "gate").unwrap();
        assert!(gate.changed);
        assert!(!gate.regression, "a dropped (skipped) check is not a quality regression");
        assert!(!cmp.any_regression);
    }

    #[test]
    fn new_check_appearing_in_current_is_changed_not_regression() {
        let a = assemble_run_all_report("base".into(), "t".into(), "c".into(), vec![passed("gate")], 1);
        let b = assemble_run_all_report(
            "cur".into(),
            "t".into(),
            "c".into(),
            vec![passed("gate"), passed("whoknows")],
            1,
        );
        let cmp = compare_reports(&a, &b);
        let wk = cmp.checks.iter().find(|d| d.name == "whoknows").unwrap();
        assert!(wk.changed);
        assert!(!wk.regression);
    }
}
