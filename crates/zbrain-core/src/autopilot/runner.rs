//! 1-5-4: Autopilot main loop + mode resolution + reconnect classification.
//!
//! Ports `src/commands/autopilot.ts`. The main loop is decomposed into:
//! - Pure functions (classify_reconnect_error, mode resolution, adaptive
//!   interval, should_full_cycle / should_sleep, lock freshness, no-worker
//!   probe state, error counter) — all testable without an engine.
//! - `run_autopilot_tick` — single iteration of the loop, testable with
//!   InMemoryEngine. The actual infinite loop + signal handling + child
//!   process supervision are CLI concerns (1-5-6).
//!
//! Per grill Q4:
//! - no-worker probe + federated v2 freshness: included in this node
//! - nightly quality probe: skipped stub (1-5-7 deferred)

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::autopilot::brain_score::{
    compute_recommendations, BrainHealth, RecommendationContext, RemediationStatus,
};
use crate::autopilot::cycle::{run_cycle, CycleOpts, CycleReport, CycleStatus};
use crate::autopilot::fanout::{dispatch_per_source, resolve_fanout_max, FanoutOpts};
use crate::autopilot::nightly_probe::{
    run_nightly_quality_probe, NightlyProbeDeps, NightlyProbeOutcome, QualityProbeAuditEvent,
};
use crate::engine::{BrainEngine, EngineKind};
use crate::minions::queue::MinionQueue;

// ── Constants ─────────────────────────────────────────────────────────

/// Lock file TTL: if lock is older than this (minutes), take over.
const LOCK_TTL_MINUTES: f64 = 10.0;

/// Minimum minutes between full cycles for a healthy brain.
const FULL_CYCLE_FLOOR_MIN: i64 = 60;

/// Consecutive idle ticks before warning about missing worker.
const NO_WORKER_WARN_TICKS: u32 = 3;

/// Max consecutive cycle failures before stopping autopilot.
const MAX_CONSECUTIVE_ERRORS: u32 = 5;

// ── Reconnect error classification ────────────────────────────────────

/// Classification of autopilot reconnect-loop errors.
///
/// Mirrors TS `classifyReconnectError` return type. `Unrecoverable` errors
/// cause immediate exit; `Recoverable` errors retry up to
/// `max_reconnect_fails` before giving up.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ReconnectErrorClass {
    Recoverable,
    Unrecoverable,
}

/// Classify a reconnect error message as recoverable or unrecoverable.
///
/// Port of TS `classifyReconnectError`. Unrecoverable patterns:
/// - `database_url` + (`undefined` | `missing` | `empty` | `not set`)
/// - `invalid url` | `malformed` | `parse url`
/// - `password authentication failed` | `authentication failed`
/// - `role` + `does not exist`
/// - `no brain configured` | `config not found`
///
/// Everything else is recoverable (network blip, 503, pool saturated, etc.).
pub fn classify_reconnect_error(msg: &str) -> ReconnectErrorClass {
    let m = msg.to_lowercase();
    if m.contains("database_url")
        && (m.contains("undefined")
            || m.contains("missing")
            || m.contains("empty")
            || m.contains("not set"))
    {
        return ReconnectErrorClass::Unrecoverable;
    }
    if m.contains("invalid url") || m.contains("malformed") || m.contains("parse url") {
        return ReconnectErrorClass::Unrecoverable;
    }
    if m.contains("password authentication failed") || m.contains("authentication failed") {
        return ReconnectErrorClass::Unrecoverable;
    }
    if m.contains("role") && m.contains("does not exist") {
        return ReconnectErrorClass::Unrecoverable;
    }
    if m.contains("no brain configured") || m.contains("config not found") {
        return ReconnectErrorClass::Unrecoverable;
    }
    ReconnectErrorClass::Recoverable
}

// ── Worker spawn decision ─────────────────────────────────────────────

/// Whether the autopilot should spawn a managed worker child process.
///
/// Returns `false` when `--no-worker` is present in args.
/// Port of TS `shouldSpawnAutopilotWorker`.
pub fn should_spawn_autopilot_worker(args: &[String]) -> bool {
    !args.iter().any(|a| a == "--no-worker")
}

// ── Mode resolution ───────────────────────────────────────────────────

/// Resolved autopilot dispatch mode.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum AutopilotMode {
    /// Minions dispatch mode (postgres + minion_mode != off + !force_inline).
    /// `spawn_worker` is false when `--no-worker` is set (dispatch only,
    /// worker managed externally).
    #[serde(rename = "minions_dispatch")]
    MinionsDispatch { spawn_worker: bool },
    /// Inline fallback mode (run_cycle directly, no queue).
    #[serde(rename = "inline")]
    Inline { reason: InlineReason },
}

/// Why inline mode was selected.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InlineReason {
    /// `minion_mode=off` in preferences.
    MinionModeOff,
    /// Engine is not Postgres (PGLite/libsql/InMemory).
    EngineNotPostgres,
    /// `--inline` flag was passed.
    ForceInline,
}

impl std::fmt::Display for InlineReason {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            InlineReason::MinionModeOff => write!(f, "minion_mode=off"),
            InlineReason::EngineNotPostgres => write!(f, "engine=pglite"),
            InlineReason::ForceInline => write!(f, "flag=--inline"),
        }
    }
}

/// Resolve autopilot dispatch mode from configuration + CLI flags.
///
/// Port of TS mode resolution logic (lines 183-187 of autopilot.ts):
/// ```text
/// useMinionsDispatch = mode != "off" && engineType == "postgres" && !forceInline
/// spawnManagedWorker = useMinionsDispatch && !noWorker
/// ```
pub fn resolve_autopilot_mode(
    mode: &str,
    engine_type: &str,
    force_inline: bool,
    no_worker: bool,
) -> AutopilotMode {
    let use_minions = mode != "off" && engine_type == "postgres" && !force_inline;
    if use_minions {
        AutopilotMode::MinionsDispatch {
            spawn_worker: !no_worker,
        }
    } else {
        let reason = if mode == "off" {
            InlineReason::MinionModeOff
        } else if engine_type != "postgres" {
            InlineReason::EngineNotPostgres
        } else {
            InlineReason::ForceInline
        };
        AutopilotMode::Inline { reason }
    }
}

// ── Lock file freshness ───────────────────────────────────────────────

/// Lock file status.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LockStatus {
    /// Lock age < 10 min — another instance is likely running.
    Fresh,
    /// Lock age >= 10 min — safe to take over.
    Stale,
}

/// Check whether a lock file is fresh or stale.
///
/// Port of TS lock file TTL logic (lines 166-174 of autopilot.ts).
pub fn check_lock_freshness(lock_age_minutes: f64) -> LockStatus {
    if lock_age_minutes < LOCK_TTL_MINUTES {
        LockStatus::Fresh
    } else {
        LockStatus::Stale
    }
}

// ── Adaptive interval ─────────────────────────────────────────────────

/// Compute adaptive sleep interval based on brain score.
///
/// Port of TS adaptive interval logic (lines 640-645):
/// - score >= 90: `base * 2` (healthy brain, back off)
/// - score < 70: `max(base / 2, 60)` (degraded brain, speed up)
/// - else: `base`
pub fn compute_adaptive_interval(base_interval: u64, brain_score: u32) -> u64 {
    if brain_score >= 90 {
        base_interval * 2
    } else if brain_score < 70 {
        std::cmp::max(base_interval / 2, 60)
    } else {
        base_interval
    }
}

// ── Full cycle vs. targeted submit decision ───────────────────────────

/// Decide whether to run a full autopilot-cycle this tick.
///
/// Port of TS `shouldFullCycle` logic (lines 521-525):
/// ```text
/// (score >= 95 && plan.length === 0 && minutesSinceLastFull >= 60) ||
/// plan.length > 3 ||
/// estTotal >= 300 ||
/// score < 70
/// ```
pub fn should_full_cycle(
    score: u32,
    plan_len: usize,
    est_total_secs: u64,
    minutes_since_last_full: i64,
) -> bool {
    (score >= 95 && plan_len == 0 && minutes_since_last_full >= FULL_CYCLE_FLOOR_MIN)
        || plan_len > 3
        || est_total_secs >= 300
        || score < 70
}

/// Decide whether to skip this tick entirely (healthy brain, recently cycled).
///
/// Port of TS `shouldSleep` logic (line 527):
/// ```text
/// score >= 95 && plan.length === 0 && minutesSinceLastFull < 60
/// ```
pub fn should_sleep(score: u32, plan_len: usize, minutes_since_last_full: i64) -> bool {
    score >= 95 && plan_len == 0 && minutes_since_last_full < FULL_CYCLE_FLOOR_MIN
}

// ── No-worker probe state ─────────────────────────────────────────────

/// Update no-worker probe state based on live worker signal.
///
/// Returns `(new_consecutive_idle, should_warn)`.
/// - If `live_worker_signal > 0`: reset counter to 0, no warning.
/// - If `live_worker_signal == 0`: increment counter; warn when reaching
///   `NO_WORKER_WARN_TICKS` (3). Does NOT repeat the warning on subsequent
///   ticks — re-arms once a live worker is seen.
///
/// Port of TS no-worker probe logic (lines 366-398).
pub fn update_no_worker_probe(
    consecutive_idle: u32,
    live_worker_signal: u32,
) -> (u32, bool) {
    if live_worker_signal > 0 {
        (0, false)
    } else {
        let new_count = consecutive_idle + 1;
        let should_warn = new_count == NO_WORKER_WARN_TICKS;
        (new_count, should_warn)
    }
}

// ── Consecutive error tracking ────────────────────────────────────────

/// Update consecutive error counter and decide whether to stop.
///
/// Returns `(new_count, should_stop)`.
/// - `cycle_ok = true`: reset counter to 0.
/// - `cycle_ok = false`: increment; stop when reaching `MAX_CONSECUTIVE_ERRORS` (5).
///
/// Port of TS circuit breaker logic (lines 656-665).
pub fn update_error_counter(consecutive_errors: u32, cycle_ok: bool) -> (u32, bool) {
    if cycle_ok {
        (0, false)
    } else {
        let new_count = consecutive_errors + 1;
        (new_count, new_count >= MAX_CONSECUTIVE_ERRORS)
    }
}

// ── Reconnect failure tracking ────────────────────────────────────────

/// Handle a reconnect failure and decide whether to stop.
///
/// Returns `(new_fails, should_stop)`.
/// - `Unrecoverable`: stop immediately regardless of fail count.
/// - `Recoverable` + `fails >= max_fails`: stop.
/// - `Recoverable` + `fails < max_fails`: continue.
///
/// Port of TS reconnect failure logic (lines 339-360).
pub fn handle_reconnect_failure(
    fails: u32,
    error_class: ReconnectErrorClass,
    max_fails: u32,
) -> (u32, bool) {
    let new_fails = fails + 1;
    match error_class {
        ReconnectErrorClass::Unrecoverable => (new_fails, true),
        ReconnectErrorClass::Recoverable => (new_fails, new_fails >= max_fails),
    }
}

// ── Autopilot state + tick ────────────────────────────────────────────

/// Mutable state carried across autopilot loop iterations.
#[derive(Debug, Clone)]
pub struct AutopilotState {
    pub consecutive_errors: u32,
    pub reconnect_fails: u32,
    pub no_worker_consecutive_idle: u32,
    pub last_full_cycle_at: Option<DateTime<Utc>>,
    pub stopping: bool,
    pub stop_reason: Option<String>,
}

impl Default for AutopilotState {
    fn default() -> Self {
        Self {
            consecutive_errors: 0,
            reconnect_fails: 0,
            no_worker_consecutive_idle: 0,
            // None = "long ago" so the first tick on a healthy brain still
            // runs the full cycle (phase-coupling exercise) before settling
            // into targeted-submit mode.
            last_full_cycle_at: None,
            stopping: false,
            stop_reason: None,
        }
    }
}

/// Options for a single autopilot tick.
#[derive(Debug, Clone)]
pub struct AutopilotOpts {
    pub repo_path: String,
    /// Base interval in seconds (default 300).
    pub base_interval: u64,
    pub json_mode: bool,
    pub mode: AutopilotMode,
    /// Max consecutive reconnect failures before exit (default 30).
    pub max_reconnect_fails: u32,
    pub engine_kind: EngineKind,
    /// Feature flag: nightly quality probe (default false).
    pub nightly_quality_probe_enabled: bool,
    /// Max USD per nightly probe run (default 5.0).
    pub nightly_probe_max_usd: f64,
    /// Directory for quality-probe audit JSONL (`~/.zbrain/audit` or
    /// `ZBRAIN_AUDIT_DIR`). CLI resolves + passes it in so the writer/reader
    /// share the same layout as the TS runtime. `None` = audit disabled
    /// (rate limit never fires, no rows written) — used by unit tests.
    pub audit_dir: Option<std::path::PathBuf>,
}

impl Default for AutopilotOpts {
    fn default() -> Self {
        Self {
            repo_path: "/tmp/brain".into(),
            base_interval: 300,
            json_mode: false,
            mode: AutopilotMode::Inline {
                reason: InlineReason::EngineNotPostgres,
            },
            max_reconnect_fails: 30,
            engine_kind: EngineKind::InMemory,
            nightly_quality_probe_enabled: false,
            nightly_probe_max_usd: 5.0,
            audit_dir: None,
        }
    }
}

/// Events emitted during a tick (for logging / JSON output).
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "event")]
pub enum TickEvent {
    #[serde(rename = "skip_healthy")]
    SkipHealthy { score: u32, plan_size: usize },
    #[serde(rename = "fanout_summary")]
    FanoutSummary {
        dispatched: Vec<String>,
        skipped_fresh: Vec<String>,
        skipped_cap: Vec<String>,
        legacy_fallback: bool,
        fanout_max: usize,
        score: u32,
    },
    #[serde(rename = "cycle_inline")]
    CycleInline {
        status: String,
        duration_ms: u64,
    },
    #[serde(rename = "cycle")]
    Cycle {
        brain_score: u32,
        elapsed_s: u64,
        next_s: u64,
    },
    #[serde(rename = "no_worker_warn")]
    NoWorkerWarn { consecutive_idle: u32 },
    #[serde(rename = "nightly_probe")]
    NightlyProbeResult {
        outcome: String,
        exit_code: i32,
        #[serde(skip_serializing_if = "Option::is_none")]
        detail: Option<String>,
    },
}

/// Result of a single autopilot tick.
#[derive(Debug, Clone)]
pub struct TickResult {
    pub cycle_ok: bool,
    pub next_interval: u64,
    pub events: Vec<TickEvent>,
}

/// Execute one iteration of the autopilot loop.
///
/// This is the testable core of `runAutopilot`. The actual infinite loop,
/// signal handling, lock file management, and child process supervision
/// are CLI concerns (1-5-6).
///
/// Per grill Q4: nightly quality probe is a skipped stub (1-5-7 deferred).
pub async fn run_autopilot_tick(
    engine: &dyn BrainEngine,
    state: &mut AutopilotState,
    opts: &AutopilotOpts,
) -> TickResult {
    let mut events = Vec::new();
    let mut cycle_ok = true;
    let mut next_interval = opts.base_interval;

    match &opts.mode {
        AutopilotMode::MinionsDispatch { .. } => {
            match dispatch_minions_path(engine, state, opts, &mut events).await {
                Ok(score) => {
                    next_interval = compute_adaptive_interval(opts.base_interval, score);
                    events.push(TickEvent::Cycle {
                        brain_score: score,
                        elapsed_s: 0,
                        next_s: next_interval,
                    });
                }
                Err(_) => {
                    cycle_ok = false;
                }
            }
        }
        AutopilotMode::Inline { .. } => {
            let report = run_cycle(
                engine,
                &CycleOpts {
                    brain_dir: opts.repo_path.clone(),
                    pull: true,
                    ..Default::default()
                },
            )
            .await;

            // Only 'failed' (every attempted phase failed) trips the
            // circuit breaker. 'partial' is a soft signal.
            if report.status == CycleStatus::Failed {
                cycle_ok = false;
            }

            let score = engine
                .get_health()
                .await
                .map(|h| h.brain_score)
                .unwrap_or(50);
            next_interval = compute_adaptive_interval(opts.base_interval, score);

            events.push(TickEvent::CycleInline {
                status: format!("{:?}", report.status),
                duration_ms: report.duration_ms,
            });
            events.push(TickEvent::Cycle {
                brain_score: score,
                elapsed_s: report.duration_ms / 1000,
                next_s: next_interval,
            });
        }
    }

    // ── Nightly quality probe ──────────────────────────────────────────
    // Runs only when enabled in config. Wrapped in catch — probe failure
    // must never crash the autopilot cycle.
    if opts.nightly_quality_probe_enabled {
        let probe_deps = NightlyProbeRunnerDeps {
            enabled: true,
            repo_root: opts.repo_path.clone(),
            max_usd: opts.nightly_probe_max_usd,
            audit_dir: opts.audit_dir.clone(),
        };
        let probe_result = run_nightly_quality_probe(&probe_deps).await;
        events.push(TickEvent::NightlyProbeResult {
            outcome: format!("{:?}", probe_result.outcome).to_lowercase(),
            exit_code: probe_result.exit_code,
            detail: probe_result.detail,
        });
    }

    TickResult {
        cycle_ok,
        next_interval,
        events,
    }
}

// ── NightlyProbeRunnerDeps — production DI for autopilot ──────────────

struct NightlyProbeRunnerDeps {
    enabled: bool,
    repo_root: String,
    max_usd: f64,
    /// Directory for quality-probe audit JSONL. `None` = audit disabled
    /// (rate limit never fires, no rows written) — used by unit tests.
    audit_dir: Option<std::path::PathBuf>,
}

#[async_trait::async_trait]
impl NightlyProbeDeps for NightlyProbeRunnerDeps {
    async fn is_enabled(&self) -> bool {
        self.enabled
    }

    async fn has_embedding_provider(&self) -> bool {
        // Check for any embedding provider API key in the environment.
        // Mirrors the TS probe's embedding-key check (nightly-quality-probe.ts).
        std::env::var("OPENAI_API_KEY").is_ok()
            || std::env::var("VOYAGE_API_KEY").is_ok()
            || std::env::var("ZEROENTROPY_API_KEY").is_ok()
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
        // registered in docs/plans/KNOWN-GAPS.md (G58)
        //
        // The TS longmemeval command (`src/commands/eval-longmemeval.ts`) is
        // NOT runnable after the Phase11 minions teardown (commit 45fe955):
        // it imports the deleted `src/core/cli-options.ts`, so `bun test
        // tests/unit/eval-longmemeval-e2e.slow.test.ts` fails with
        // "Cannot find module '../core/cli-options.ts'". That breakage is a
        // deliberately-accepted baseline entry (tsc-baseline.txt froze it as
        // inherited debt), so spawning the TS via `bun` would only surface
        // module-not-found.
        //
        // Per the eval-disposition decision (KNOWN-GAPS G58): do NOT re-spawn
        // the dead TS. When the nightly probe genuinely needs to run, the
        // longmemeval pipeline must be ported to Rust natively (PGLite → an
        // embedded engine + hybrid search + the LLM answer step), not shelled
        // out to bun. Until then this returns a probe-level error; the caller
        // records an `error` outcome and never crashes autopilot.
        Err(
            "longmemeval not runnable: TS command broken by Phase11 minions \
             teardown (missing src/core/cli-options.ts); needs native Rust port"
                .into(),
        )
    }

    async fn run_cross_modal_batch(
        &self,
        _batch_path: &str,
        _summary_path: &str,
        _max_usd: f64,
    ) -> Result<(i32, Option<super::nightly_probe::CrossModalSummary>), String> {
        // registered in docs/plans/KNOWN-GAPS.md (G58)
        //
        // See `run_long_mem_eval` above. The cross-modal batch depends on the
        // longmemeval output that we can no longer produce, and its own TS
        // pipeline (`src/commands/eval-cross-modal.ts`, ~1543 lines, 15 LLM
        // calls) shares the same un-migrated-TS fate. Honest error until a
        // native Rust cross-modal batch runner exists.
        Err(
            "cross-modal batch not runnable: depends on longmemeval output \
             (broken by Phase11) + un-migrated TS; needs native Rust port"
                .into(),
        )
    }

    fn read_recent_events(&self, days: u32) -> Vec<QualityProbeAuditEvent> {
        let Some(ref audit_dir) = self.audit_dir else {
            return vec![];
        };
        super::nightly_probe::read_recent_quality_probe_events(
            audit_dir,
            days as i64,
            chrono::Utc::now(),
        )
    }

    fn log_event(&self, event: QualityProbeAuditEvent) {
        let Some(ref audit_dir) = self.audit_dir else {
            return;
        };
        super::nightly_probe::log_quality_probe_event(audit_dir, &event);
    }

    fn now(&self) -> chrono::DateTime<chrono::Utc> {
        chrono::Utc::now()
    }
}

// ── dispatch paths ────────────────────────────────────────────────────
///
/// Returns the brain score for adaptive interval computation.
async fn dispatch_minions_path(
    engine: &dyn BrainEngine,
    state: &mut AutopilotState,
    opts: &AutopilotOpts,
    events: &mut Vec<TickEvent>,
) -> Result<u32, String> {
    // Get health (cheap: single SQL count on real backends)
    let health = engine.get_health().await.map_err(|e| e.to_string())?;
    let score = health.brain_score;

    // Compute recommendations
    let ctx = RecommendationContext {
        repo_path: Some(opts.repo_path.clone()),
        ..Default::default()
    };
    let plan: Vec<_> = compute_recommendations(&health, &ctx)
        .into_iter()
        .filter(|r| r.status == RemediationStatus::Remediable)
        .collect();
    let est_total: u64 = plan.iter().map(|r| r.est_seconds).sum();

    // Track time since last full cycle
    // None = "long ago" (i64::MAX) so first tick runs full cycle
    let minutes_since_last_full = state
        .last_full_cycle_at
        .map(|t| (Utc::now() - t).num_minutes())
        .unwrap_or(i64::MAX);

    let do_full = should_full_cycle(score, plan.len(), est_total, minutes_since_last_full);
    let do_sleep = should_sleep(score, plan.len(), minutes_since_last_full);

    if do_sleep {
        events.push(TickEvent::SkipHealthy {
            score,
            plan_size: 0,
        });
        return Ok(score);
    }

    if do_full {
        // Full cycle via per-source fan-out
        let queue = MinionQueue::new(engine);
        let fanout_max = resolve_fanout_max(opts.engine_kind, None);
        let slot = Utc::now().format("%Y-%m-%dT%H:%M").to_string();
        let timeout_ms = std::cmp::max(opts.base_interval as i64 * 2 * 1000, 300_000);

        let fanout_opts = FanoutOpts {
            repo_path: opts.repo_path.clone(),
            slot,
            timeout_ms,
            fanout_max,
            json_mode: opts.json_mode,
        };

        let result = dispatch_per_source(engine, &queue, &fanout_opts)
            .await
            .map_err(|e| e.to_string())?;

        if !result.dispatched.is_empty() || result.legacy_fallback {
            state.last_full_cycle_at = Some(Utc::now());
        }

        events.push(TickEvent::FanoutSummary {
            dispatched: result.dispatched,
            skipped_fresh: result.skipped_fresh,
            skipped_cap: result.skipped_cap,
            legacy_fallback: result.legacy_fallback,
            fanout_max,
            score,
        });
    }
    // Small targeted plan path: would submit individual handler jobs per
    // RemediationStep. The actual queue.add calls are wired in the full
    // implementation; for now the score is returned for interval computation.

    Ok(score)
}

// ── Federated v2 freshness check ──────────────────────────────────────

/// Check whether a source needs a freshness sync job.
///
/// Returns `true` when `last_sync_at` is older than `interval_secs`, or
/// when `last_sync_at` is `None` (never synced).
///
/// Port of TS federated v2 freshness logic (lines 446-452).
pub fn is_source_freshness_stale(
    last_sync_at: Option<DateTime<Utc>>,
    now: DateTime<Utc>,
    interval_secs: i64,
) -> bool {
    match last_sync_at {
        None => true,
        Some(last) => (now - last).num_seconds() >= interval_secs,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::{EngineConfig, EngineKind, InMemoryEngine};

    // ── classify_reconnect_error ───────────────────────────────────────

    #[test]
    fn classify_database_url_undefined_is_unrecoverable() {
        assert_eq!(
            classify_reconnect_error("config.database_url undefined"),
            ReconnectErrorClass::Unrecoverable
        );
    }

    #[test]
    fn classify_database_url_missing_is_unrecoverable() {
        assert_eq!(
            classify_reconnect_error("database_url is missing"),
            ReconnectErrorClass::Unrecoverable
        );
    }

    #[test]
    fn classify_database_url_empty_is_unrecoverable() {
        assert_eq!(
            classify_reconnect_error("database_url is empty"),
            ReconnectErrorClass::Unrecoverable
        );
    }

    #[test]
    fn classify_database_url_not_set_is_unrecoverable() {
        assert_eq!(
            classify_reconnect_error("database_url not set"),
            ReconnectErrorClass::Unrecoverable
        );
    }

    #[test]
    fn classify_invalid_url_is_unrecoverable() {
        assert_eq!(
            classify_reconnect_error("invalid url in connection string"),
            ReconnectErrorClass::Unrecoverable
        );
    }

    #[test]
    fn classify_malformed_is_unrecoverable() {
        assert_eq!(
            classify_reconnect_error("malformed connection string"),
            ReconnectErrorClass::Unrecoverable
        );
    }

    #[test]
    fn classify_parse_url_is_unrecoverable() {
        assert_eq!(
            classify_reconnect_error("failed to parse url"),
            ReconnectErrorClass::Unrecoverable
        );
    }

    #[test]
    fn classify_password_auth_failed_is_unrecoverable() {
        assert_eq!(
            classify_reconnect_error("password authentication failed for user"),
            ReconnectErrorClass::Unrecoverable
        );
    }

    #[test]
    fn classify_auth_failed_is_unrecoverable() {
        assert_eq!(
            classify_reconnect_error("authentication failed"),
            ReconnectErrorClass::Unrecoverable
        );
    }

    #[test]
    fn classify_role_does_not_exist_is_unrecoverable() {
        assert_eq!(
            classify_reconnect_error("role \"zbrain\" does not exist"),
            ReconnectErrorClass::Unrecoverable
        );
    }

    #[test]
    fn classify_no_brain_configured_is_unrecoverable() {
        assert_eq!(
            classify_reconnect_error("no brain configured"),
            ReconnectErrorClass::Unrecoverable
        );
    }

    #[test]
    fn classify_config_not_found_is_unrecoverable() {
        assert_eq!(
            classify_reconnect_error("config not found at path"),
            ReconnectErrorClass::Unrecoverable
        );
    }

    #[test]
    fn classify_network_error_is_recoverable() {
        assert_eq!(
            classify_reconnect_error("ECONNREFUSED 127.0.0.1:5432"),
            ReconnectErrorClass::Recoverable
        );
    }

    #[test]
    fn classify_503_is_recoverable() {
        assert_eq!(
            classify_reconnect_error("503 Service Unavailable"),
            ReconnectErrorClass::Recoverable
        );
    }

    #[test]
    fn classify_pool_saturated_is_recoverable() {
        assert_eq!(
            classify_reconnect_error("pool saturated, no connections available"),
            ReconnectErrorClass::Recoverable
        );
    }

    #[test]
    fn classify_empty_string_is_recoverable() {
        assert_eq!(
            classify_reconnect_error(""),
            ReconnectErrorClass::Recoverable
        );
    }

    #[test]
    fn classify_case_insensitive() {
        assert_eq!(
            classify_reconnect_error("DATABASE_URL UNDEFINED"),
            ReconnectErrorClass::Unrecoverable
        );
        assert_eq!(
            classify_reconnect_error("INVALID URL"),
            ReconnectErrorClass::Unrecoverable
        );
    }

    // ── should_spawn_autopilot_worker ──────────────────────────────────

    #[test]
    fn spawn_worker_true_without_no_worker_flag() {
        assert!(should_spawn_autopilot_worker(&[]));
        assert!(should_spawn_autopilot_worker(&[
            "--repo".into(),
            "/tmp/brain".into()
        ]));
    }

    #[test]
    fn spawn_worker_false_with_no_worker_flag() {
        assert!(!should_spawn_autopilot_worker(&["--no-worker".into()]));
        assert!(!should_spawn_autopilot_worker(&[
            "--repo".into(),
            "/tmp/brain".into(),
            "--no-worker".into()
        ]));
    }

    // ── resolve_autopilot_mode ─────────────────────────────────────────

    #[test]
    fn mode_minions_dispatch_when_postgres_and_not_off() {
        let mode = resolve_autopilot_mode("pain_triggered", "postgres", false, false);
        assert_eq!(
            mode,
            AutopilotMode::MinionsDispatch {
                spawn_worker: true
            }
        );
    }

    #[test]
    fn mode_inline_when_minion_mode_off() {
        let mode = resolve_autopilot_mode("off", "postgres", false, false);
        assert_eq!(
            mode,
            AutopilotMode::Inline {
                reason: InlineReason::MinionModeOff
            }
        );
    }

    #[test]
    fn mode_inline_when_engine_not_postgres() {
        let mode = resolve_autopilot_mode("pain_triggered", "pglite", false, false);
        assert_eq!(
            mode,
            AutopilotMode::Inline {
                reason: InlineReason::EngineNotPostgres
            }
        );
    }

    #[test]
    fn mode_inline_when_force_inline() {
        let mode = resolve_autopilot_mode("pain_triggered", "postgres", true, false);
        assert_eq!(
            mode,
            AutopilotMode::Inline {
                reason: InlineReason::ForceInline
            }
        );
    }

    #[test]
    fn mode_no_worker_sets_spawn_false() {
        let mode = resolve_autopilot_mode("always", "postgres", false, true);
        assert_eq!(
            mode,
            AutopilotMode::MinionsDispatch {
                spawn_worker: false
            }
        );
    }

    #[test]
    fn inline_reason_display() {
        assert_eq!(InlineReason::MinionModeOff.to_string(), "minion_mode=off");
        assert_eq!(
            InlineReason::EngineNotPostgres.to_string(),
            "engine=pglite"
        );
        assert_eq!(InlineReason::ForceInline.to_string(), "flag=--inline");
    }

    // ── check_lock_freshness ───────────────────────────────────────────

    #[test]
    fn lock_fresh_under_10_min() {
        assert_eq!(check_lock_freshness(0.0), LockStatus::Fresh);
        assert_eq!(check_lock_freshness(5.0), LockStatus::Fresh);
        assert_eq!(check_lock_freshness(9.9), LockStatus::Fresh);
    }

    #[test]
    fn lock_stale_at_or_over_10_min() {
        assert_eq!(check_lock_freshness(10.0), LockStatus::Stale);
        assert_eq!(check_lock_freshness(15.0), LockStatus::Stale);
        assert_eq!(check_lock_freshness(120.0), LockStatus::Stale);
    }

    // ── compute_adaptive_interval ──────────────────────────────────────

    #[test]
    fn interval_doubles_when_score_high() {
        assert_eq!(compute_adaptive_interval(300, 90), 600);
        assert_eq!(compute_adaptive_interval(300, 95), 600);
        assert_eq!(compute_adaptive_interval(300, 100), 600);
    }

    #[test]
    fn interval_halves_when_score_low() {
        assert_eq!(compute_adaptive_interval(300, 69), 150);
        assert_eq!(compute_adaptive_interval(300, 50), 150);
        assert_eq!(compute_adaptive_interval(300, 0), 150);
    }

    #[test]
    fn interval_floor_60_when_score_low() {
        assert_eq!(compute_adaptive_interval(100, 50), 60);
        assert_eq!(compute_adaptive_interval(80, 0), 60);
    }

    #[test]
    fn interval_base_when_score_mid() {
        assert_eq!(compute_adaptive_interval(300, 70), 300);
        assert_eq!(compute_adaptive_interval(300, 89), 300);
        assert_eq!(compute_adaptive_interval(300, 75), 300);
    }

    // ── should_full_cycle ──────────────────────────────────────────────

    #[test]
    fn full_cycle_when_score_high_plan_empty_and_old() {
        assert!(should_full_cycle(95, 0, 0, 60));
        assert!(should_full_cycle(100, 0, 0, 120));
    }

    #[test]
    fn no_full_cycle_when_score_high_plan_empty_but_recent() {
        assert!(!should_full_cycle(95, 0, 0, 30));
        assert!(!should_full_cycle(99, 0, 0, 59));
    }

    #[test]
    fn full_cycle_when_plan_large() {
        assert!(should_full_cycle(90, 4, 100, 10));
        assert!(should_full_cycle(80, 10, 500, 5));
    }

    #[test]
    fn full_cycle_when_est_total_high() {
        assert!(should_full_cycle(85, 2, 300, 10));
        assert!(should_full_cycle(80, 3, 600, 5));
    }

    #[test]
    fn full_cycle_when_score_low() {
        assert!(should_full_cycle(69, 0, 0, 5));
        assert!(should_full_cycle(50, 1, 10, 5));
    }

    #[test]
    fn no_full_cycle_when_score_ok_plan_small_est_low() {
        assert!(!should_full_cycle(85, 2, 100, 10));
        assert!(!should_full_cycle(80, 3, 200, 30));
    }

    // ── should_sleep ───────────────────────────────────────────────────

    #[test]
    fn sleep_when_score_high_plan_empty_recent_cycle() {
        assert!(should_sleep(95, 0, 30));
        assert!(should_sleep(100, 0, 0));
    }

    #[test]
    fn no_sleep_when_score_below_threshold() {
        assert!(!should_sleep(94, 0, 10));
        assert!(!should_sleep(80, 0, 10));
    }

    #[test]
    fn no_sleep_when_plan_non_empty() {
        assert!(!should_sleep(95, 1, 10));
        assert!(!should_sleep(99, 5, 10));
    }

    #[test]
    fn no_sleep_when_old_enough_for_full_cycle() {
        assert!(!should_sleep(95, 0, 60));
        assert!(!should_sleep(95, 0, 120));
    }

    // ── update_no_worker_probe ─────────────────────────────────────────

    #[test]
    fn probe_resets_on_live_signal() {
        let (count, warn) = update_no_worker_probe(2, 5);
        assert_eq!(count, 0);
        assert!(!warn);
    }

    #[test]
    fn probe_increments_on_no_signal() {
        let (count, warn) = update_no_worker_probe(0, 0);
        assert_eq!(count, 1);
        assert!(!warn);
    }

    #[test]
    fn probe_warns_on_third_consecutive_idle() {
        let (_, warn1) = update_no_worker_probe(0, 0);
        assert!(!warn1);

        let (_, warn2) = update_no_worker_probe(1, 0);
        assert!(!warn2);

        let (count3, warn3) = update_no_worker_probe(2, 0);
        assert_eq!(count3, 3);
        assert!(warn3);
    }

    #[test]
    fn probe_does_not_repeat_warning() {
        let (_, warn) = update_no_worker_probe(3, 0);
        assert!(!warn); // 4th tick — no repeat

        let (_, warn) = update_no_worker_probe(4, 0);
        assert!(!warn);
    }

    #[test]
    fn probe_rearms_after_live_signal() {
        // 3 idle ticks → warn
        let (c1, _) = update_no_worker_probe(0, 0);
        let (c2, _) = update_no_worker_probe(c1, 0);
        let (c3, w3) = update_no_worker_probe(c2, 0);
        assert!(w3);

        // Live signal → reset
        let (c4, _) = update_no_worker_probe(c3, 1);
        assert_eq!(c4, 0);

        // 3 more idle ticks → warn again
        let (c5, _) = update_no_worker_probe(c4, 0);
        let (c6, _) = update_no_worker_probe(c5, 0);
        let (c7, w7) = update_no_worker_probe(c6, 0);
        assert!(w7);
    }

    // ── update_error_counter ───────────────────────────────────────────

    #[test]
    fn error_counter_resets_on_success() {
        let (count, stop) = update_error_counter(3, true);
        assert_eq!(count, 0);
        assert!(!stop);
    }

    #[test]
    fn error_counter_increments_on_failure() {
        let (count, stop) = update_error_counter(0, false);
        assert_eq!(count, 1);
        assert!(!stop);
    }

    #[test]
    fn error_counter_stops_at_5() {
        let (count, stop) = update_error_counter(4, false);
        assert_eq!(count, 5);
        assert!(stop);
    }

    #[test]
    fn error_counter_does_not_stop_below_5() {
        let (count, stop) = update_error_counter(3, false);
        assert_eq!(count, 4);
        assert!(!stop);
    }

    // ── handle_reconnect_failure ───────────────────────────────────────

    #[test]
    fn reconnect_unrecoverable_stops_immediately() {
        let (fails, stop) = handle_reconnect_failure(0, ReconnectErrorClass::Unrecoverable, 30);
        assert_eq!(fails, 1);
        assert!(stop);
    }

    #[test]
    fn reconnect_recoverable_continues_below_max() {
        let (fails, stop) = handle_reconnect_failure(5, ReconnectErrorClass::Recoverable, 30);
        assert_eq!(fails, 6);
        assert!(!stop);
    }

    #[test]
    fn reconnect_recoverable_stops_at_max() {
        let (fails, stop) = handle_reconnect_failure(29, ReconnectErrorClass::Recoverable, 30);
        assert_eq!(fails, 30);
        assert!(stop);
    }

    // ── is_source_freshness_stale ──────────────────────────────────────

    #[test]
    fn freshness_stale_when_never_synced() {
        assert!(is_source_freshness_stale(None, Utc::now(), 300));
    }

    #[test]
    fn freshness_stale_when_older_than_interval() {
        let now = Utc::now();
        let old = now - chrono::Duration::seconds(600);
        assert!(is_source_freshness_stale(Some(old), now, 300));
    }

    #[test]
    fn freshness_not_stale_when_within_interval() {
        let now = Utc::now();
        let recent = now - chrono::Duration::seconds(100);
        assert!(!is_source_freshness_stale(Some(recent), now, 300));
    }

    // ── run_autopilot_tick (inline path) ───────────────────────────────

    async fn setup_engine() -> InMemoryEngine {
        let engine = InMemoryEngine::new();
        engine.connect(&EngineConfig::default()).await.unwrap();
        engine
    }

    #[tokio::test]
    async fn tick_inline_runs_cycle() {
        let engine = setup_engine().await;
        let mut state = AutopilotState::default();
        let opts = AutopilotOpts {
            repo_path: "/tmp/brain".into(),
            mode: AutopilotMode::Inline {
                reason: InlineReason::EngineNotPostgres,
            },
            ..Default::default()
        };

        let result = run_autopilot_tick(&engine, &mut state, &opts).await;

        assert!(result.cycle_ok);
        // Should have cycle_inline + cycle events (nightly probe disabled by default)
        let has_cycle_inline = result
            .events
            .iter()
            .any(|e| matches!(e, TickEvent::CycleInline { .. }));
        assert!(has_cycle_inline);

        let has_cycle = result
            .events
            .iter()
            .any(|e| matches!(e, TickEvent::Cycle { .. }));
        assert!(has_cycle);

        // No probe event when disabled (default)
        let has_probe = result
            .events
            .iter()
            .any(|e| matches!(e, TickEvent::NightlyProbeResult { .. }));
        assert!(!has_probe);
    }

    #[tokio::test]
    async fn tick_inline_empty_brain_score_is_100() {
        let engine = setup_engine().await;
        let mut state = AutopilotState::default();
        let opts = AutopilotOpts {
            repo_path: "/tmp/brain".into(),
            mode: AutopilotMode::Inline {
                reason: InlineReason::EngineNotPostgres,
            },
            base_interval: 300,
            ..Default::default()
        };

        let result = run_autopilot_tick(&engine, &mut state, &opts).await;

        // Empty brain → score 100 → interval doubles
        assert_eq!(result.next_interval, 600);
    }

    #[tokio::test]
    async fn tick_inline_cycle_ok_does_not_set_errors() {
        let engine = setup_engine().await;
        let mut state = AutopilotState::default();
        state.consecutive_errors = 3;

        let opts = AutopilotOpts {
            repo_path: "/tmp/brain".into(),
            mode: AutopilotMode::Inline {
                reason: InlineReason::EngineNotPostgres,
            },
            ..Default::default()
        };

        let result = run_autopilot_tick(&engine, &mut state, &opts).await;
        assert!(result.cycle_ok);
        // Caller resets errors using update_error_counter
        let (new_errors, stop) = update_error_counter(state.consecutive_errors, result.cycle_ok);
        assert_eq!(new_errors, 0);
        assert!(!stop);
    }

    // ── run_autopilot_tick (minions dispatch path) ─────────────────────

    #[tokio::test]
    async fn tick_minions_dispatch_empty_brain_full_cycle() {
        let engine = setup_engine().await;
        let mut state = AutopilotState::default();
        let opts = AutopilotOpts {
            repo_path: "/tmp/brain".into(),
            mode: AutopilotMode::MinionsDispatch {
                spawn_worker: true,
            },
            engine_kind: EngineKind::InMemory,
            base_interval: 300,
            ..Default::default()
        };

        let result = run_autopilot_tick(&engine, &mut state, &opts).await;

        // Empty brain → score 100 → should_sleep (None last_full_cycle =
        // i64::MAX minutes, but 100 >= 95 && plan empty && minutes >= 60)
        // → should_sleep = false (because minutes >= 60)
        // → should_full_cycle = true (score >= 95 && plan empty && min >= 60)
        // → fanout dispatch (legacy fallback for InMemory with no sources)
        assert!(result.cycle_ok);

        let has_fanout = result
            .events
            .iter()
            .any(|e| matches!(e, TickEvent::FanoutSummary { .. }));
        assert!(has_fanout);

        // After fanout, last_full_cycle_at should be set
        assert!(state.last_full_cycle_at.is_some());
    }

    #[tokio::test]
    async fn tick_minions_dispatch_sleeps_when_recently_cycled() {
        let engine = setup_engine().await;
        let mut state = AutopilotState::default();
        // Set last_full_cycle_at to recent → should_sleep
        state.last_full_cycle_at = Some(Utc::now());

        let opts = AutopilotOpts {
            repo_path: "/tmp/brain".into(),
            mode: AutopilotMode::MinionsDispatch {
                spawn_worker: true,
            },
            engine_kind: EngineKind::InMemory,
            base_interval: 300,
            ..Default::default()
        };

        let result = run_autopilot_tick(&engine, &mut state, &opts).await;

        // score=100, plan=0, minutes_since_last_full=0 → should_sleep=true
        let has_skip = result
            .events
            .iter()
            .any(|e| matches!(e, TickEvent::SkipHealthy { .. }));
        assert!(has_skip);
    }

    #[tokio::test]
    async fn tick_minions_dispatch_no_nightly_probe_when_disabled() {
        let engine = setup_engine().await;
        let mut state = AutopilotState::default();
        let opts = AutopilotOpts {
            repo_path: "/tmp/brain".into(),
            mode: AutopilotMode::MinionsDispatch {
                spawn_worker: true,
            },
            engine_kind: EngineKind::InMemory,
            nightly_quality_probe_enabled: false, // default
            ..Default::default()
        };

        let result = run_autopilot_tick(&engine, &mut state, &opts).await;

        let has_probe = result
            .events
            .iter()
            .any(|e| matches!(e, TickEvent::NightlyProbeResult { .. }));
        assert!(!has_probe);
    }

    #[tokio::test]
    async fn tick_minions_dispatch_emits_nightly_probe_when_enabled() {
        let engine = setup_engine().await;
        let mut state = AutopilotState::default();
        let opts = AutopilotOpts {
            repo_path: "/tmp/brain".into(),
            mode: AutopilotMode::MinionsDispatch {
                spawn_worker: true,
            },
            engine_kind: EngineKind::InMemory,
            nightly_quality_probe_enabled: true,
            ..Default::default()
        };

        let result = run_autopilot_tick(&engine, &mut state, &opts).await;

        let has_probe = result
            .events
            .iter()
            .any(|e| matches!(e, TickEvent::NightlyProbeResult { .. }));
        assert!(has_probe);
    }

    // ── AutopilotState default ─────────────────────────────────────────

    #[test]
    fn state_default_last_full_cycle_none() {
        let state = AutopilotState::default();
        assert!(state.last_full_cycle_at.is_none());
        assert_eq!(state.consecutive_errors, 0);
        assert_eq!(state.reconnect_fails, 0);
        assert_eq!(state.no_worker_consecutive_idle, 0);
        assert!(!state.stopping);
    }
}
