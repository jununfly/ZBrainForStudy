//! 1-6-3-2: `BaseCyclePhase` abstraction for the cycle orchestration.
//!
//! Faithful Rust port of `src/core/cycle/base-phase.ts`. Several cycle phases
//! (drift, and future calibration phases) share enough structure that the
//! duplication-vs-abstraction trade tips toward a shared base. The base
//! enforces five cross-cutting concerns so each subclass only writes its
//! domain work:
//!
//! 1. Uniform phase signature: `run(engine, ctx, opts) -> PhaseResult`.
//! 2. Source-isolation: the engine is passed to `process` *explicitly* (NOT on
//!    the context). `scope` is the only sanctioned way to read source
//!    scoping. Mirrors TS `sourceScopeOpts(ctx)` + the v0.34.1 isolation fix —
//!    forgetting to thread source scope becomes a compile error, not a
//!    runtime leak.
//! 3. Budget metering wraps `run()` automatically. The subclass declares
//!    `budget_usd_key` + `budget_usd_default`; the base builds a `BudgetMeter`
//!    and passes it into `process`. The subclass calls `check_budget()` before
//!    each LLM submit; budget-exhausted work returns `status: Ok` with
//!    `details.budget_exhausted: true` (clean partial abort).
//! 4. Uniform error envelope: errors returned from `process` are caught and
//!    converted to `status: Fail` with a phase-specific `error.code`/`class`.
//! 5. Progress reporter integration via `tick()`.
//!
//! Existing pre-v0.36 phases (synthesize, patterns, …) deliberately do NOT
//! retrofit to this base yet — future phases use it by default.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;

use async_trait::async_trait;
use serde_json::json;

use crate::ai::chat::ChatProvider;
use crate::autopilot::budget_meter::{BudgetCheckResult, BudgetMeter, BudgetMeterOpts, SubmitEstimate};
use crate::autopilot::cycle::{CyclePhase, PhaseError, PhaseResult, PhaseStatus};
use crate::engine::BrainEngine;

/// Minimal progress reporter, mirrors TS `ProgressReporter` (object-safe).
pub trait ProgressReporter: std::fmt::Debug + Send + Sync {
    /// Advance the reporter by `delta` units with an optional message.
    fn tick(&self, delta: u64, message: Option<&str>);
}

/// Source-scoped read options threaded through every engine call inside a
/// `BaseCyclePhase`. Mirrors TS `ScopedReadOpts`.
#[derive(Debug, Clone, Default)]
pub struct ScopedReadOpts {
    pub source_id: Option<String>,
}

/// Non-engine context for a base phase run.
///
/// Holds everything except the engine — mirroring TS `OperationContext` minus
/// `engine`. The engine is passed to `process` explicitly to enforce
/// source-isolation: the subclass cannot read the engine off the context, so
/// forgetting to thread source scope is a compile error, not a runtime leak.
#[derive(Debug, Clone)]
pub struct BasePhaseCtx {
    /// Source id for per-source scoping (mirrors TS `ctx.auth.allowedSources`).
    pub source_id: Option<String>,
    /// LLM chat provider for LLM-heavy phases. `None` → phases skip LLM work.
    pub chat: Option<Arc<dyn ChatProvider>>,
    /// Dry-run mode propagated from cycle opts.
    pub dry_run: bool,
    /// Directory for the ISO-week `dream-budget-*.jsonl` ledger.
    pub audit_dir: PathBuf,
}

impl BasePhaseCtx {
    #[must_use]
    pub fn new(
        source_id: Option<String>,
        chat: Option<Arc<dyn ChatProvider>>,
        dry_run: bool,
        audit_dir: PathBuf,
    ) -> Self {
        Self { source_id, chat, dry_run, audit_dir }
    }
}

/// Options for a `BaseCyclePhase` run.
#[derive(Clone)]
pub struct BasePhaseOpts {
    /// Optional progress reporter. Phases call `tick()` through the base.
    pub reporter: Option<Arc<dyn ProgressReporter>>,
    /// Optional explicit budget override in USD. Otherwise the base uses
    /// `budget_usd_default()`. (Rust has no global config store yet; the TS
    /// `budgetUsdKey` resolution is recorded on the trait for future wiring.)
    pub budget_usd: Option<f64>,
    /// Optional injected `BudgetMeter` (tests). When set, replaces the default
    /// constructed one.
    pub meter: Option<Arc<BudgetMeter>>,
}

impl Default for BasePhaseOpts {
    fn default() -> Self {
        Self { reporter: None, budget_usd: None, meter: None }
    }
}

/// Output of a subclass's `process`. Mirrors the TS `process` return shape;
/// `status: None` means the base defaults to `PhaseStatus::Ok`.
#[derive(Debug, Clone, Default)]
pub struct BasePhaseOutput {
    pub summary: String,
    pub details: serde_json::Value,
    pub status: Option<PhaseStatus>,
}

/// Shared base for cycle phases. See module docs for the five enforced concerns.
#[async_trait]
pub trait BaseCyclePhase {
    /// Phase name; matches a `CyclePhase` variant.
    fn name(&self) -> CyclePhase;

    /// Config key for the budget-USD override (mirrors TS `budgetUsdKey`).
    /// Recorded for future config wiring; not yet resolved in Rust.
    fn budget_usd_key(&self) -> &str;

    /// Default budget cap in USD if no override is present.
    fn budget_usd_default(&self) -> f64;

    /// Optional error-code mapper for errors returned from `process`.
    /// Subclasses specialize via `err.downcast_ref::<T>()`. Default: `"UNKNOWN"`.
    fn map_error_code(&self, _err: &(dyn std::error::Error + Send + Sync + 'static)) -> String {
        "UNKNOWN".to_string()
    }

    /// Optional error-class mapper. Default: `"InternalError"`.
    fn map_error_class(&self, _err: &(dyn std::error::Error + Send + Sync + 'static)) -> String {
        "InternalError".to_string()
    }

    /// Tick the progress reporter for this phase. Subclass calls this instead of
    /// reaching for `opts.reporter` directly so the phase name is always correct.
    fn tick(&self, opts: &BasePhaseOpts, message: Option<&str>, delta: u64) {
        if let Some(r) = &opts.reporter {
            r.tick(delta, message);
        }
    }

    /// Check the budget for a planned LLM submit. Subclass calls this before
    /// every gateway call. When `allowed == false` the subclass MUST abort the
    /// planned submit and continue with what it has accumulated (clean partial
    /// completion path).
    fn check_budget(&self, meter: &BudgetMeter, estimate: &SubmitEstimate) -> BudgetCheckResult {
        meter.check(estimate)
    }

    /// The phase's actual work. Subclass implements this; `run` wraps it with
    /// source-scope enforcement, budget metering, error catching, and progress
    /// accounting. `scope` is the only sanctioned way to read source-scoped data.
    async fn process(
        &self,
        engine: &dyn BrainEngine,
        scope: &ScopedReadOpts,
        ctx: &BasePhaseCtx,
        opts: &BasePhaseOpts,
        meter: &BudgetMeter,
    ) -> Result<BasePhaseOutput, Box<dyn std::error::Error + Send + Sync>>;

    /// Public entry point. Wraps `process` with all cross-cutting concerns and
    /// returns a `PhaseResult` ready to slot into `CycleReport.phases`.
    async fn run(
        &self,
        engine: &dyn BrainEngine,
        ctx: &BasePhaseCtx,
        opts: &BasePhaseOpts,
    ) -> PhaseResult {
        let t0 = Instant::now();

        // Source-scope discipline — required by every base-phase subclass.
        // Forgetting to thread this would have been the v0.34.1 leak class.
        let scope = ScopedReadOpts { source_id: ctx.source_id.clone() };

        // Budget meter construction. Tests inject; otherwise build from default.
        let meter: Arc<BudgetMeter> = match &opts.meter {
            Some(m) => m.clone(),
            None => {
                let budget = opts.budget_usd.unwrap_or_else(|| self.budget_usd_default());
                Arc::new(BudgetMeter::new(BudgetMeterOpts {
                    budget_usd: budget,
                    phase: self.name().label().to_string(),
                    audit_dir: ctx.audit_dir.clone(),
                    audit_path: None,
                }))
            }
        };

        match self.process(engine, &scope, ctx, opts, &meter).await {
            Ok(out) => PhaseResult {
                phase: self.name().label().to_string(),
                status: out.status.unwrap_or(PhaseStatus::Ok),
                duration_ms: t0.elapsed().as_millis() as u64,
                summary: out.summary,
                details: out.details,
                error: None,
            },
            Err(e) => {
                let code = self.map_error_code(e.as_ref());
                let class = self.map_error_class(e.as_ref());
                let message = e.to_string();
                PhaseResult {
                    phase: self.name().label().to_string(),
                    status: PhaseStatus::Fail,
                    duration_ms: t0.elapsed().as_millis() as u64,
                    summary: format!("{} failed: {}", self.name().label(), message),
                    details: json!({ "error_code": code }),
                    error: Some(PhaseError {
                        class,
                        code,
                        message,
                        hint: None,
                        docs_url: None,
                    }),
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    use crate::engine::InMemoryEngine;

    #[derive(Debug)]
    struct MockError {
        code: &'static str,
    }
    impl std::fmt::Display for MockError {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            write!(f, "mock error: {}", self.code)
        }
    }
    impl std::error::Error for MockError {}

    #[derive(Clone, Copy)]
    enum Mode {
        Ok,
        Err,
        BudgetGuard,
    }

    struct MockPhase {
        name: CyclePhase,
        budget_key: &'static str,
        budget_default: f64,
        mode: Mode,
        scope_seen: Arc<Mutex<Option<Option<String>>>>,
        budget_allowed: Arc<Mutex<Option<bool>>>,
    }

    #[async_trait]
    impl BaseCyclePhase for MockPhase {
        fn name(&self) -> CyclePhase {
            self.name
        }
        fn budget_usd_key(&self) -> &str {
            self.budget_key
        }
        fn budget_usd_default(&self) -> f64 {
            self.budget_default
        }
        fn map_error_code(&self, err: &(dyn std::error::Error + Send + Sync + 'static)) -> String {
            if let Some(e) = err.downcast_ref::<MockError>() {
                e.code.to_string()
            } else {
                "UNKNOWN".to_string()
            }
        }
        fn map_error_class(&self, _err: &(dyn std::error::Error + Send + Sync + 'static)) -> String {
            "MockClass".to_string()
        }

        async fn process(
            &self,
            _engine: &dyn BrainEngine,
            scope: &ScopedReadOpts,
            _ctx: &BasePhaseCtx,
            _opts: &BasePhaseOpts,
            meter: &BudgetMeter,
        ) -> Result<BasePhaseOutput, Box<dyn std::error::Error + Send + Sync>> {
            *self.scope_seen.lock().unwrap() = Some(scope.source_id.clone());
            match self.mode {
                Mode::Ok => Ok(BasePhaseOutput {
                    summary: "did work".into(),
                    details: json!({ "n": 3 }),
                    status: None,
                }),
                Mode::Err => Err(Box::new(MockError { code: "MOCK_BOOM" })),
                Mode::BudgetGuard => {
                    // openai:gpt-5.2 priced: 200k in + 32k out ≈ $0.57, so a
                    // $0.01 cap is exceeded (denied) and a $100 cap fits (allowed).
                    let est = SubmitEstimate {
                        model_id: "openai:gpt-5.2".into(),
                        estimated_input_tokens: 200_000,
                        max_output_tokens: 32_000,
                        label: Some("verdict".into()),
                    };
                    let res = self.check_budget(meter, &est);
                    *self.budget_allowed.lock().unwrap() = Some(res.allowed);
                    if !res.allowed {
                        // clean partial abort
                        return Ok(BasePhaseOutput {
                            summary: "budget exhausted; partial".into(),
                            details: json!({ "budget_exhausted": true }),
                            status: Some(PhaseStatus::Ok),
                        });
                    }
                    Ok(BasePhaseOutput {
                        summary: "under budget".into(),
                        details: json!({}),
                        status: None,
                    })
                }
            }
        }
    }

    fn ctx_with(source_id: Option<&str>) -> BasePhaseCtx {
        BasePhaseCtx::new(
            source_id.map(str::to_string),
            None,
            false,
            std::env::temp_dir(),
        )
    }

    #[tokio::test]
    async fn run_happy_ok_and_threads_scope() {
        let p = MockPhase {
            name: CyclePhase::CalibrationProfile,
            budget_key: "cycle.drift.budget_usd",
            budget_default: 1.0,
            mode: Mode::Ok,
            scope_seen: Arc::new(Mutex::new(None)),
            budget_allowed: Arc::new(Mutex::new(None)),
        };
        let r = p
            .run(&InMemoryEngine::new(), &ctx_with(Some("src-1")), &BasePhaseOpts::default())
            .await;
        assert_eq!(r.phase, "calibration-profile");
        assert_eq!(r.status, PhaseStatus::Ok);
        assert_eq!(r.details["n"], 3);
        assert!(r.error.is_none());
        assert_eq!(*p.scope_seen.lock().unwrap(), Some(Some("src-1".to_string())));
    }

    #[tokio::test]
    async fn run_maps_error_to_fail_with_code_and_class() {
        let p = MockPhase {
            name: CyclePhase::CalibrationProfile,
            budget_key: "x",
            budget_default: 1.0,
            mode: Mode::Err,
            scope_seen: Arc::new(Mutex::new(None)),
            budget_allowed: Arc::new(Mutex::new(None)),
        };
        let r = p
            .run(&InMemoryEngine::new(), &ctx_with(None), &BasePhaseOpts::default())
            .await;
        assert_eq!(r.status, PhaseStatus::Fail);
        let err = r.error.as_ref().unwrap();
        assert_eq!(err.code, "MOCK_BOOM");
        assert_eq!(err.class, "MockClass");
        assert_eq!(r.details["error_code"], "MOCK_BOOM");
        assert!(err.message.contains("MOCK_BOOM"));
    }

    #[tokio::test]
    async fn run_budget_exhausted_clean_abort() {
        let p = MockPhase {
            name: CyclePhase::CalibrationProfile,
            budget_key: "x",
            budget_default: 0.01,
            mode: Mode::BudgetGuard,
            scope_seen: Arc::new(Mutex::new(None)),
            budget_allowed: Arc::new(Mutex::new(None)),
        };
        let r = p
            .run(&InMemoryEngine::new(), &ctx_with(None), &BasePhaseOpts::default())
            .await;
        assert_eq!(r.status, PhaseStatus::Ok);
        assert_eq!(r.details["budget_exhausted"], true);
        assert_eq!(*p.budget_allowed.lock().unwrap(), Some(false));
    }

    #[tokio::test]
    async fn run_budget_override_allows_submit() {
        let p = MockPhase {
            name: CyclePhase::CalibrationProfile,
            budget_key: "x",
            budget_default: 0.01,
            mode: Mode::BudgetGuard,
            scope_seen: Arc::new(Mutex::new(None)),
            budget_allowed: Arc::new(Mutex::new(None)),
        };
        let opts = BasePhaseOpts {
            budget_usd: Some(100.0),
            ..Default::default()
        };
        let r = p.run(&InMemoryEngine::new(), &ctx_with(None), &opts).await;
        assert_eq!(*p.budget_allowed.lock().unwrap(), Some(true));
        assert_eq!(r.status, PhaseStatus::Ok);
        assert!(r.details.get("budget_exhausted").is_none());
    }

    #[tokio::test]
    async fn run_threads_none_scope_when_no_source() {
        let p = MockPhase {
            name: CyclePhase::CalibrationProfile,
            budget_key: "x",
            budget_default: 1.0,
            mode: Mode::Ok,
            scope_seen: Arc::new(Mutex::new(None)),
            budget_allowed: Arc::new(Mutex::new(None)),
        };
        let _ = p.run(&InMemoryEngine::new(), &ctx_with(None), &BasePhaseOpts::default()).await;
        assert_eq!(*p.scope_seen.lock().unwrap(), Some(None));
    }
}
