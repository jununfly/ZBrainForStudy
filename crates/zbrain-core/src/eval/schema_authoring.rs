//! Faithful port of the pure `aggregateVerdict` function from the TS-era
//! `src/commands/eval-schema-authoring.ts` (retrieved from git history at
//! `3c09a69f^`).
//!
//! The TS `runEvalSchemaAuthoring` CLI harness was a stub
//! ("Full CLI harness lands in v0.39.1"); only `aggregateVerdict` was real.
//! That pure function is ported here, verbatim in behavior, so it can be
//! reused by the eventual hermetic harness or called directly in-process.

use serde::Serialize;

/// Verdict of a schema-authoring eval run.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum SchemaAuthoringVerdict {
    Pass,
    Fail,
    Inconclusive,
}

/// Result of aggregating a baseline vs post-suggest filing-accuracy delta.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct AggregateVerdict {
    pub verdict: SchemaAuthoringVerdict,
    pub delta: f64,
    pub reasoning: String,
}

/// Pure aggregator: given baseline + post-suggest filing-accuracy numbers,
/// decide pass/fail/inconclusive.
///
/// Pass requires non-trivial improvement (`delta >= 0.1`) AND no
/// high-confidence suggestion was silently auto-applied below the 0.6
/// threshold (`low_confidence_count` is informational only).
///
/// Faithful to TS
/// `aggregateVerdict(baseline, post_suggest, suggestion_count, low_confidence_count)`
/// in `src/commands/eval-schema-authoring.ts`.
pub fn aggregate_verdict(
    baseline: f64,
    post_suggest: f64,
    suggestion_count: u64,
    low_confidence_count: u64,
) -> AggregateVerdict {
    let delta = post_suggest - baseline;

    if suggestion_count == 0 && baseline >= 0.9 {
        return AggregateVerdict {
            verdict: SchemaAuthoringVerdict::Pass,
            delta,
            reasoning: "Active pack already matches brain shape; no suggestions needed.".to_string(),
        };
    }

    if suggestion_count == 0 {
        return AggregateVerdict {
            verdict: SchemaAuthoringVerdict::Inconclusive,
            delta,
            reasoning: format!(
                "Baseline {:.2} below 0.9 but runSuggest returned 0 suggestions. Check whether the brain has enough typed pages for detect to fire.",
                baseline
            ),
        };
    }

    if delta >= 0.1 {
        return AggregateVerdict {
            verdict: SchemaAuthoringVerdict::Pass,
            delta,
            reasoning: format!(
                "Filing accuracy improved {:.1}pp from {:.1}% → {:.1}%.",
                delta * 100.0,
                baseline * 100.0,
                post_suggest * 100.0
            ),
        };
    }

    if delta >= 0.0 {
        return AggregateVerdict {
            verdict: SchemaAuthoringVerdict::Inconclusive,
            delta,
            reasoning: format!(
                "Suggestions returned but filing accuracy delta is only {:.1}pp — below the 10pp pass threshold.",
                delta * 100.0
            ),
        };
    }

    AggregateVerdict {
        verdict: SchemaAuthoringVerdict::Fail,
        delta,
        reasoning: format!(
            "Filing accuracy REGRESSED {:.1}pp after applying suggestions. {} low-confidence suggestions were emitted; verify they were NOT auto-applied.",
            delta.abs() * 100.0,
            low_confidence_count
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pass_when_already_good_and_no_suggestions() {
        let v = aggregate_verdict(0.95, 0.95, 0, 0);
        assert_eq!(v.verdict, SchemaAuthoringVerdict::Pass);
        assert_eq!(v.delta, 0.0);
    }

    #[test]
    fn inconclusive_when_baseline_low_and_no_suggestions() {
        let v = aggregate_verdict(0.50, 0.50, 0, 0);
        assert_eq!(v.verdict, SchemaAuthoringVerdict::Inconclusive);
        assert!(v.reasoning.contains("0.50"));
    }

    #[test]
    fn pass_on_large_improvement() {
        let v = aggregate_verdict(0.60, 0.75, 3, 0);
        assert_eq!(v.verdict, SchemaAuthoringVerdict::Pass);
        assert!((v.delta - 0.15).abs() < 1e-9);
        assert!(v.reasoning.contains("15.0pp"));
        assert!(v.reasoning.contains("60.0%"));
        assert!(v.reasoning.contains("75.0%"));
    }

    #[test]
    fn inconclusive_on_small_positive_delta() {
        let v = aggregate_verdict(0.60, 0.65, 3, 0);
        assert_eq!(v.verdict, SchemaAuthoringVerdict::Inconclusive);
        assert!((v.delta - 0.05).abs() < 1e-9);
        assert!(v.reasoning.contains("5.0pp"));
    }

    #[test]
    fn fail_on_regression() {
        let v = aggregate_verdict(0.80, 0.70, 4, 2);
        assert_eq!(v.verdict, SchemaAuthoringVerdict::Fail);
        assert!((v.delta + 0.10).abs() < 1e-9);
        assert!(v.reasoning.contains("REGRESSED"));
        assert!(v.reasoning.contains("10.0pp"));
        assert!(v.reasoning.contains("2 low-confidence"));
    }

    #[test]
    fn exactly_tenth_delta_passes() {
        // delta == 0.1 is the inclusive pass threshold. Use 0.0/0.1 so the
        // f64 subtraction yields exactly 0.1 (0.6 - 0.7 artifacts to
        // 0.0999... and would fall through to Inconclusive — matching TS).
        let v = aggregate_verdict(0.0, 0.1, 1, 0);
        assert_eq!(v.verdict, SchemaAuthoringVerdict::Pass);
        assert!((v.delta - 0.10).abs() < 1e-12);
        assert!(v.reasoning.contains("10.0pp"));
    }
}
