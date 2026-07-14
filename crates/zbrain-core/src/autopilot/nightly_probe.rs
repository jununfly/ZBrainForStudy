//! Nightly quality probe — once per 24h, runs canonical quality pipeline.
//!
//! Ported from `src/core/cycle/nightly-quality-probe.ts` (v0.40.1.0 T6).

use chrono::{DateTime, Duration, Utc};

// ── Constants ───────────────────────────────────────────────────────────

/// 24‑hour window — the probe runs at most once per day.
const NIGHTLY_WINDOW: Duration = Duration::hours(24);

/// Default max spend per run; matches eval-cross-modal --max-usd default.
const DEFAULT_MAX_USD: f64 = 5.0;

/// Committed fixture path relative to the repo root.
const NIGHTLY_FIXTURE_REL_PATH: &str = "tests/unit/fixtures/longmemeval-nightly.jsonl";

/// Convert a `DateTime<Utc>` to an ISO 8601 string for audit storage.
fn dt_to_iso(dt: DateTime<Utc>) -> String {
    dt.to_rfc3339()
}

/// Parse an ISO 8601 string into a `DateTime<Utc>`, returning `None` on
/// failure (mirrors TS silently skipping corrupt timestamps).
fn parse_iso_utc(s: &str) -> Option<DateTime<Utc>> {
    chrono::DateTime::parse_from_rfc3339(s)
        .ok()
        .map(|d| d.with_timezone(&Utc))
}

// ── shouldRunNightly ────────────────────────────────────────────────────

/// Outcome of the nightly rate-limit check.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RateLimitDecision {
    /// The probe may run.
    Run,
    /// The probe must be skipped — already ran within the window.
    RateLimited,
}

/// Pure function: decide whether the probe should run given the audit
/// history. Returns [`RateLimitDecision::RateLimited`] when any recent
/// event falls within the 24 h window.
pub fn should_run_nightly(
    now: DateTime<Utc>,
    recent_timestamps: &[DateTime<Utc>],
    window: Duration,
) -> RateLimitDecision {
    let cutoff = now - window;
    for ts in recent_timestamps {
        if *ts >= cutoff {
            return RateLimitDecision::RateLimited;
        }
    }
    RateLimitDecision::Run
}

// ── NightlyProbeResult ──────────────────────────────────────────────────

/// Result reported back to the cycle dispatcher.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NightlyProbeOutcome {
    Pass,
    Fail,
    Inconclusive,
    Error,
    BudgetExceeded,
    RateLimited,
    NoEmbeddingKey,
    Disabled,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct NightlyProbeResult {
    pub outcome: NightlyProbeOutcome,
    pub exit_code: i32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

// ── QualityProbeAuditEvent ──────────────────────────────────────────────

/// One audit row recorded for every probe run (including short-circuits).
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct QualityProbeAuditEvent {
    /// ISO 8601 timestamp string (e.g. "2026-07-14T12:00:00Z").
    pub ts: String,
    pub outcome: NightlyProbeOutcome,
    pub exit_code: i32,
    pub pass_count: u32,
    pub fail_count: u32,
    pub inconclusive_count: u32,
    pub error_count: u32,
    pub est_cost_usd: f64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fixture_sha8: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

// ── NightlyProbeDeps ────────────────────────────────────────────────────

/// Dependency injection trait — every external effect is abstracted behind
/// this trait so tests can stub slow / expensive paths.
#[async_trait::async_trait]
pub trait NightlyProbeDeps: Send + Sync {
    /// Returns true when the feature config flag is on.
    async fn is_enabled(&self) -> bool;
    /// Returns true when an embedding provider is configured + reachable.
    async fn has_embedding_provider(&self) -> bool;
    /// Resolves the cost cap (config override or DEFAULT_MAX_USD).
    async fn resolve_max_usd(&self) -> f64;
    /// Resolves the repo root so we can find the committed fixture.
    async fn resolve_repo_root(&self) -> String;
    /// Runs the long‑mem‑eval command, writing per‑question hypotheses to
    /// `output_path`.
    async fn run_long_mem_eval(&self, fixture_path: &str, output_path: &str) -> Result<(), String>;
    /// Runs the cross‑modal batch. Returns exit code and optional summary.
    async fn run_cross_modal_batch(
        &self,
        batch_path: &str,
        summary_path: &str,
        max_usd: f64,
    ) -> Result<(i32, Option<CrossModalSummary>), String>;
    /// Reads recent audit events for rate-limit decisions.
    fn read_recent_events(&self, days: u32) -> Vec<QualityProbeAuditEvent>;
    /// Appends one audit event. Best-effort; panics/bugs here must not
    /// crash the probe.
    fn log_event(&self, event: QualityProbeAuditEvent);
    /// Now provider — overridable for tests of the 24h rate limit.
    fn now(&self) -> DateTime<Utc>;
}

// ── runNightlyQualityProbe ──────────────────────────────────────────────

/// Cross-modal batch summary parsed from the eval output.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct CrossModalSummary {
    pub pass_count: u32,
    pub fail_count: u32,
    pub inconclusive_count: u32,
    pub error_count: u32,
    pub est_cost_usd: f64,
    pub verdict: String,
}

/// Run the nightly probe. All external effects go through `deps` so tests
/// can stub long-running paths (eval / embedding).
pub async fn run_nightly_quality_probe(deps: &dyn NightlyProbeDeps) -> NightlyProbeResult {
    // 1. Feature flag check.
    if !deps.is_enabled().await {
        return NightlyProbeResult {
            outcome: NightlyProbeOutcome::Disabled,
            exit_code: 0,
            detail: Some("feature flag off".into()),
        };
    }

    let now = deps.now();

    // 2. 24h rate limit.
    let recent = deps.read_recent_events(2);
    let recent_ts: Vec<DateTime<Utc>> = recent
        .iter()
        .filter_map(|e| parse_iso_utc(&e.ts))
        .collect();
    let decision = should_run_nightly(now, &recent_ts, NIGHTLY_WINDOW);
    if decision == RateLimitDecision::RateLimited {
        deps.log_event(QualityProbeAuditEvent {
            ts: dt_to_iso(now),
            outcome: NightlyProbeOutcome::RateLimited,
            exit_code: 0,
            pass_count: 0,
            fail_count: 0,
            inconclusive_count: 0,
            error_count: 0,
            est_cost_usd: 0.0,
            fixture_sha8: None,
            detail: Some("already ran within 24h window".into()),
        });
        return NightlyProbeResult {
            outcome: NightlyProbeOutcome::RateLimited,
            exit_code: 0,
            detail: Some("already ran within 24h".into()),
        };
    }

    // 3. Embedding provider check.
    if !deps.has_embedding_provider().await {
        eprintln!(
            "[nightly-quality-probe] no embedding provider configured; skipping. \
             Configure OPENAI_API_KEY / VOYAGE_API_KEY / ZEROENTROPY_API_KEY and re-enable."
        );
        deps.log_event(QualityProbeAuditEvent {
            ts: dt_to_iso(now),
            outcome: NightlyProbeOutcome::NoEmbeddingKey,
            exit_code: 0,
            pass_count: 0,
            fail_count: 0,
            inconclusive_count: 0,
            error_count: 0,
            est_cost_usd: 0.0,
            fixture_sha8: None,
            detail: Some("no embedding provider configured".into()),
        });
        return NightlyProbeResult {
            outcome: NightlyProbeOutcome::NoEmbeddingKey,
            exit_code: 0,
            detail: Some("no embedding provider".into()),
        };
    }

    // 4. Resolve repo root + fixture path.
    let repo_root = deps.resolve_repo_root().await;
    let fixture_path = format!("{}/{}", repo_root, NIGHTLY_FIXTURE_REL_PATH);
    let fixture_sha8 = None; // sha8 computed by filesystem in production adapter
    let max_usd = deps.resolve_max_usd().await;

    // 5. Run evaluations.
    // Placeholder work_dir — production adapter creates tempdir.
    let lme_out = format!("{}/lme-output.jsonl", "tmp");
    let summary_path = format!("{}/summary.json", "tmp");

    match deps.run_long_mem_eval(&fixture_path, &lme_out).await {
        Ok(()) => {}
        Err(detail) => {
            eprintln!("[nightly-quality-probe] runtime error: {}", detail);
            deps.log_event(QualityProbeAuditEvent {
                ts: dt_to_iso(now),
                outcome: NightlyProbeOutcome::Error,
                exit_code: 1,
                pass_count: 0,
                fail_count: 0,
                inconclusive_count: 0,
                error_count: 0,
                est_cost_usd: 0.0,
                fixture_sha8,
                detail: Some(detail.clone()),
            });
            return NightlyProbeResult {
                outcome: NightlyProbeOutcome::Error,
                exit_code: 1,
                detail: Some(detail),
            };
        }
    };

    let (exit_code, summary) = match deps
        .run_cross_modal_batch(&lme_out, &summary_path, max_usd)
        .await
    {
        Ok(result) => result,
        Err(detail) => {
            eprintln!("[nightly-quality-probe] runtime error: {}", detail);
            deps.log_event(QualityProbeAuditEvent {
                ts: dt_to_iso(now),
                outcome: NightlyProbeOutcome::Error,
                exit_code: 1,
                pass_count: 0,
                fail_count: 0,
                inconclusive_count: 0,
                error_count: 0,
                est_cost_usd: 0.0,
                fixture_sha8,
                detail: Some(detail.clone()),
            });
            return NightlyProbeResult {
                outcome: NightlyProbeOutcome::Error,
                exit_code: 1,
                detail: Some(detail),
            };
        }
    };

    // 6. Determine outcome.
    let outcome = if let Some(ref s) = summary {
        match s.verdict.as_str() {
            "pass" => NightlyProbeOutcome::Pass,
            "fail" => NightlyProbeOutcome::Fail,
            "inconclusive" => NightlyProbeOutcome::Inconclusive,
            "error" => NightlyProbeOutcome::Error,
            _ => {
                if exit_code == 1 {
                    NightlyProbeOutcome::BudgetExceeded
                } else {
                    NightlyProbeOutcome::Error
                }
            }
        }
    } else if exit_code == 1 {
        NightlyProbeOutcome::BudgetExceeded
    } else {
        NightlyProbeOutcome::Error
    };

    // 7. Audit.
    deps.log_event(QualityProbeAuditEvent {
        ts: dt_to_iso(now),
        outcome: outcome.clone(),
        exit_code,
        pass_count: summary.as_ref().map_or(0, |s| s.pass_count),
        fail_count: summary.as_ref().map_or(0, |s| s.fail_count),
        inconclusive_count: summary.as_ref().map_or(0, |s| s.inconclusive_count),
        error_count: summary.as_ref().map_or(0, |s| s.error_count),
        est_cost_usd: summary.as_ref().map_or(0.0, |s| s.est_cost_usd),
        fixture_sha8,
        detail: None,
    });

    NightlyProbeResult {
        outcome,
        exit_code,
        detail: None,
    }
}

// ── Tests ───────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn dt(s: &str) -> DateTime<Utc> {
        Utc.datetime_from_str(s, "%Y-%m-%dT%H:%M:%SZ").unwrap()
    }

    // ── S2: types ──────────────────────────────────────────────────────────

    #[test]
    fn nightly_probe_result_serde_roundtrip() {
        let r = NightlyProbeResult {
            outcome: NightlyProbeOutcome::Pass,
            exit_code: 0,
            detail: None,
        };
        let json = serde_json::to_string(&r).unwrap();
        assert!(json.contains("\"pass\""));
        let back: NightlyProbeResult = serde_json::from_str(&json).unwrap();
        assert_eq!(back.outcome, NightlyProbeOutcome::Pass);
    }

    #[test]
    fn outcome_variants_serialize_snake_case() {
        assert_eq!(
            serde_json::to_string(&NightlyProbeOutcome::NoEmbeddingKey).unwrap(),
            "\"no_embedding_key\""
        );
        assert_eq!(
            serde_json::to_string(&NightlyProbeOutcome::BudgetExceeded).unwrap(),
            "\"budget_exceeded\""
        );
        assert_eq!(
            serde_json::to_string(&NightlyProbeOutcome::RateLimited).unwrap(),
            "\"rate_limited\""
        );
    }

    #[test]
    fn result_detail_skipped_when_none() {
        let r = NightlyProbeResult {
            outcome: NightlyProbeOutcome::Disabled,
            exit_code: 0,
            detail: None,
        };
        let json = serde_json::to_string(&r).unwrap();
        assert!(!json.contains("detail"));
    }

    #[test]
    fn result_detail_present_when_some() {
        let r = NightlyProbeResult {
            outcome: NightlyProbeOutcome::Error,
            exit_code: 1,
            detail: Some("fixture not found".into()),
        };
        let json = serde_json::to_string(&r).unwrap();
        assert!(json.contains("fixture not found"));
    }

    // ── Stub deps for S3 tests ───────────────────────────────────────────

    struct StubDeps {
        enabled: bool,
        has_embedding: bool,
        max_usd: f64,
        repo_root: String,
        now: DateTime<Utc>,
        long_mem_eval_result: Result<(), String>,
        cross_modal_result: Result<(i32, Option<super::CrossModalSummary>), String>,
        recent_events: Vec<super::QualityProbeAuditEvent>,
        logged_events: std::sync::Mutex<Vec<super::QualityProbeAuditEvent>>,
    }

    impl StubDeps {
        fn new() -> Self {
            Self {
                enabled: true,
                has_embedding: true,
                max_usd: super::DEFAULT_MAX_USD,
                repo_root: "/tmp/brain".into(),
                now: dt("2026-07-14T12:00:00Z"),
                long_mem_eval_result: Ok(()),
                cross_modal_result: Ok((
                    0,
                    Some(super::CrossModalSummary {
                        pass_count: 10,
                        fail_count: 0,
                        inconclusive_count: 0,
                        error_count: 0,
                        est_cost_usd: 1.5,
                        verdict: "pass".into(),
                    }),
                )),
                recent_events: vec![],
                logged_events: std::sync::Mutex::new(vec![]),
            }
        }

        fn logged_events(&self) -> Vec<super::QualityProbeAuditEvent> {
            self.logged_events.lock().unwrap().clone()
        }
    }

    #[async_trait::async_trait]
    impl super::NightlyProbeDeps for StubDeps {
        async fn is_enabled(&self) -> bool {
            self.enabled
        }
        async fn has_embedding_provider(&self) -> bool {
            self.has_embedding
        }
        async fn resolve_max_usd(&self) -> f64 {
            self.max_usd
        }
        async fn resolve_repo_root(&self) -> String {
            self.repo_root.clone()
        }
        async fn run_long_mem_eval(
            &self,
            _fixture_path: &str,
            _output_path: &str,
        ) -> Result<(), String> {
            self.long_mem_eval_result.clone()
        }
        async fn run_cross_modal_batch(
            &self,
            _batch_path: &str,
            _summary_path: &str,
            _max_usd: f64,
        ) -> Result<(i32, Option<super::CrossModalSummary>), String> {
            self.cross_modal_result.clone()
        }
        fn read_recent_events(&self, _days: u32) -> Vec<super::QualityProbeAuditEvent> {
            self.recent_events.clone()
        }
        fn log_event(&self, event: super::QualityProbeAuditEvent) {
            self.logged_events.lock().unwrap().push(event);
        }
        fn now(&self) -> DateTime<Utc> {
            self.now
        }
    }

    // ── S3: run_nightly_quality_probe ────────────────────────────────────

    #[tokio::test]
    async fn probe_disabled_returns_disabled_no_audit() {
        let mut deps = StubDeps::new();
        deps.enabled = false;
        let result = super::run_nightly_quality_probe(&deps).await;
        assert_eq!(result.outcome, NightlyProbeOutcome::Disabled);
        assert_eq!(result.exit_code, 0);
        assert!(deps.logged_events().is_empty());
    }

    #[tokio::test]
    async fn probe_rate_limited_logs_and_returns_rate_limited() {
        let mut deps = StubDeps::new();
        deps.recent_events = vec![super::QualityProbeAuditEvent {
            ts: "2026-07-13T18:00:00Z".into(),
            outcome: NightlyProbeOutcome::Pass,
            exit_code: 0,
            pass_count: 10,
            fail_count: 0,
            inconclusive_count: 0,
            error_count: 0,
            est_cost_usd: 1.5,
            fixture_sha8: None,
            detail: None,
        }];
        let result = super::run_nightly_quality_probe(&deps).await;
        assert_eq!(result.outcome, NightlyProbeOutcome::RateLimited);
        assert_eq!(result.exit_code, 0);
        let events = deps.logged_events();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].outcome, NightlyProbeOutcome::RateLimited);
    }

    #[tokio::test]
    async fn probe_no_embedding_logs_and_skips() {
        let mut deps = StubDeps::new();
        deps.has_embedding = false;
        let result = super::run_nightly_quality_probe(&deps).await;
        assert_eq!(result.outcome, NightlyProbeOutcome::NoEmbeddingKey);
        let events = deps.logged_events();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].outcome, NightlyProbeOutcome::NoEmbeddingKey);
    }

    #[tokio::test]
    async fn probe_passes_when_verdict_is_pass() {
        let deps = StubDeps::new();
        let result = super::run_nightly_quality_probe(&deps).await;
        assert_eq!(result.outcome, NightlyProbeOutcome::Pass);
        assert_eq!(result.exit_code, 0);
        let events = deps.logged_events();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].pass_count, 10);
        assert_eq!(events[0].est_cost_usd, 1.5);
    }

    #[tokio::test]
    async fn probe_fails_when_verdict_is_fail() {
        let mut deps = StubDeps::new();
        deps.cross_modal_result = Ok((
            1,
            Some(super::CrossModalSummary {
                pass_count: 3,
                fail_count: 7,
                inconclusive_count: 0,
                error_count: 0,
                est_cost_usd: 2.0,
                verdict: "fail".into(),
            }),
        ));
        let result = super::run_nightly_quality_probe(&deps).await;
        assert_eq!(result.outcome, NightlyProbeOutcome::Fail);
        let events = deps.logged_events();
        assert_eq!(events[0].fail_count, 7);
    }

    #[tokio::test]
    async fn probe_returns_error_when_long_mem_eval_fails() {
        let mut deps = StubDeps::new();
        deps.long_mem_eval_result = Err("fixture not found".into());
        let result = super::run_nightly_quality_probe(&deps).await;
        assert_eq!(result.outcome, NightlyProbeOutcome::Error);
        assert_eq!(result.exit_code, 1);
    }

    #[tokio::test]
    async fn probe_returns_error_when_cross_modal_fails() {
        let mut deps = StubDeps::new();
        deps.cross_modal_result = Err("batch eval crashed".into());
        let result = super::run_nightly_quality_probe(&deps).await;
        assert_eq!(result.outcome, NightlyProbeOutcome::Error);
        assert_eq!(result.exit_code, 1);
    }

    #[tokio::test]
    async fn probe_no_summary_exit_code_1_is_budget_exceeded() {
        let mut deps = StubDeps::new();
        deps.cross_modal_result = Ok((1, None));
        let result = super::run_nightly_quality_probe(&deps).await;
        assert_eq!(result.outcome, NightlyProbeOutcome::BudgetExceeded);
        assert_eq!(result.exit_code, 1);
    }

    #[tokio::test]
    async fn probe_no_summary_exit_code_2_is_error() {
        let mut deps = StubDeps::new();
        deps.cross_modal_result = Ok((2, None));
        let result = super::run_nightly_quality_probe(&deps).await;
        assert_eq!(result.outcome, NightlyProbeOutcome::Error);
        assert_eq!(result.exit_code, 2);
    }

    #[tokio::test]
    async fn probe_inconclusive_verdict() {
        let mut deps = StubDeps::new();
        deps.cross_modal_result = Ok((
            0,
            Some(super::CrossModalSummary {
                pass_count: 5,
                fail_count: 3,
                inconclusive_count: 2,
                error_count: 0,
                est_cost_usd: 1.0,
                verdict: "inconclusive".into(),
            }),
        ));
        let result = super::run_nightly_quality_probe(&deps).await;
        assert_eq!(result.outcome, NightlyProbeOutcome::Inconclusive);
    }

    // ── S1: should_run_nightly ──────────────────────────────────────────

    #[test]
    fn empty_timestamps_returns_run() {
        let now = dt("2026-07-14T12:00:00Z");
        assert_eq!(
            should_run_nightly(now, &[], NIGHTLY_WINDOW),
            RateLimitDecision::Run
        );
    }

    #[test]
    fn event_within_24h_returns_rate_limited() {
        let now = dt("2026-07-14T12:00:00Z");
        let recent = vec![dt("2026-07-13T18:00:00Z")]; // 18h ago
        assert_eq!(
            should_run_nightly(now, &recent, NIGHTLY_WINDOW),
            RateLimitDecision::RateLimited
        );
    }

    #[test]
    fn event_at_cutoff_boundary_returns_rate_limited() {
        let now = dt("2026-07-14T12:00:00Z");
        let recent = vec![dt("2026-07-13T12:00:00Z")]; // exactly 24h ago
        assert_eq!(
            should_run_nightly(now, &recent, NIGHTLY_WINDOW),
            RateLimitDecision::RateLimited
        );
    }

    #[test]
    fn event_older_than_24h_returns_run() {
        let now = dt("2026-07-14T12:00:00Z");
        let recent = vec![dt("2026-07-13T11:59:59Z")]; // 24h + 1s ago
        assert_eq!(
            should_run_nightly(now, &recent, NIGHTLY_WINDOW),
            RateLimitDecision::Run
        );
    }

    #[test]
    fn first_event_within_window_short_circuits() {
        let now = dt("2026-07-14T12:00:00Z");
        let recent = vec![
            dt("2026-07-13T18:00:00Z"), // 18h ago — within
            dt("2026-07-10T12:00:00Z"), // 4d ago
            dt("2026-07-09T12:00:00Z"), // 5d ago
        ];
        assert_eq!(
            should_run_nightly(now, &recent, NIGHTLY_WINDOW),
            RateLimitDecision::RateLimited
        );
    }

    #[test]
    fn custom_window_is_respected() {
        let now = dt("2026-07-14T12:00:00Z");
        // 1h window — event 30 min ago should block
        let recent = vec![dt("2026-07-14T11:30:00Z")];
        assert_eq!(
            should_run_nightly(now, &recent, Duration::hours(1)),
            RateLimitDecision::RateLimited
        );
        // 1h window — event 90 min ago should NOT block
        let recent = vec![dt("2026-07-14T10:30:00Z")];
        assert_eq!(
            should_run_nightly(now, &recent, Duration::hours(1)),
            RateLimitDecision::Run
        );
    }
}

