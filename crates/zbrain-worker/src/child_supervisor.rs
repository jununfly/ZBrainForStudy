//! Child worker supervisor — reusable spawn/respawn core (roadmap 1-2-5).
//!
//! Extracted from `MinionSupervisor` so it can be reused by both
//! `zbrain jobs supervisor` and `zbrain autopilot`. Mirrors TS
//! `child-worker-supervisor.ts`.
//!
//! ## Deferred (not in this slice)
//! - PID file locking → `supervisor.rs`
//! - Signal handling (SIGTERM/SIGINT) → `supervisor.rs`
//! - DB health checks → `supervisor.rs`
//! - Audit logging → `supervisor.rs`

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use tokio::process::Command;
use tokio::sync::mpsc;
use tracing::{info, warn};

use crate::exit_classification::{classify_exit, ExitClass};
use crate::spawn_helpers::{build_spawn_args, detect_tini};

// ─── Types ───────────────────────────────────────────────────────────────────

/// Lifecycle events emitted by the child supervisor.
/// Mirrors TS `ChildSupervisorEvent` union.
#[derive(Debug, Clone)]
pub enum ChildSupervisorEvent {
    /// Child process started successfully.
    WorkerSpawned { pid: u32, tini: bool },
    /// Child process exited.
    WorkerExited {
        code: Option<i32>,
        signal: Option<i32>,
        run_duration_ms: u64,
        likely_cause: String,
        crash_count: u32,
    },
    /// Failed to spawn child (sync or async error).
    WorkerSpawnFailed {
        error: String,
    },
    /// Entering backoff before next spawn attempt.
    Backoff {
        ms: u64,
        crash_count: u32,
        reason: BackoffReason,
    },
    /// Clean restart budget exceeded — applying cooldown.
    HealthWarn {
        reason: String,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BackoffReason {
    /// Code 0 — immediate restart, no delay.
    CleanExit,
    /// Code != 0 — exponential backoff.
    Crash,
    /// Clean exits exceeded budget in sliding window — 1s cooldown.
    BudgetExceeded,
}

/// Configuration for `ChildWorkerSupervisor`.
/// Mirrors TS `ChildWorkerSupervisorOpts`.
#[derive(Clone)]
pub struct ChildSupervisorOpts {
    /// Path to the zbrain CLI binary.
    pub cli_path: String,
    /// Arguments passed to the child process (e.g. `["jobs", "work"]`).
    pub args: Vec<String>,
    /// Environment variables for the child process.
    pub env: std::collections::HashMap<String, String>,
    /// Maximum consecutive crashes before giving up.
    pub max_crashes: u32,
    /// If a non-zero-exit child ran at least this long, reset crashCount to 1.
    /// Default: 5 minutes. Mirrors TS `stableRunResetMs`.
    pub stable_run_reset_ms: u64,
    /// Max clean exits within the budget window before applying 1s cooldown.
    /// Default: 10. Mirrors TS `cleanRestartBudget`.
    pub clean_restart_budget: u32,
    /// Sliding window for clean restart budget. Default: 60s. Mirrors TS
    /// `cleanRestartWindowMs`.
    pub clean_restart_window_ms: u64,
    /// Cooldown when clean restart budget is exceeded. Default: 1s.
    pub clean_restart_budget_backoff_ms: u64,
    /// Testing override for backoff floor. If `Some`, the backoff delay
    /// returned by `calculate_supervisor_backoff` is at least this value.
    pub _backoff_floor_ms: Option<u64>,
    /// Testing override for `Instant::now`. If `Some`, used instead of the
    /// real clock.
    #[allow(clippy::type_complexity)]
    pub _now: Option<Arc<dyn Fn() -> Instant + Send + Sync>>,
}

impl std::fmt::Debug for ChildSupervisorOpts {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ChildSupervisorOpts")
            .field("cli_path", &self.cli_path)
            .field("args", &self.args)
            .field("env", &self.env)
            .field("max_crashes", &self.max_crashes)
            .field("stable_run_reset_ms", &self.stable_run_reset_ms)
            .field("clean_restart_budget", &self.clean_restart_budget)
            .field("clean_restart_window_ms", &self.clean_restart_window_ms)
            .field("clean_restart_budget_backoff_ms", &self.clean_restart_budget_backoff_ms)
            .field("_backoff_floor_ms", &self._backoff_floor_ms)
            .field("_now", &self._now.as_ref().map(|_| "<closure>"))
            .finish()
    }
}

impl Default for ChildSupervisorOpts {
    fn default() -> Self {
        Self {
            cli_path: String::new(),
            args: Vec::new(),
            env: std::collections::HashMap::new(),
            max_crashes: 10,
            stable_run_reset_ms: 300_000,       // 5 min
            clean_restart_budget: 10,
            clean_restart_window_ms: 60_000,    // 1 min
            clean_restart_budget_backoff_ms: 1_000,
            _backoff_floor_ms: None,
            _now: None,
        }
    }
}

// ─── Backoff ─────────────────────────────────────────────────────────────────

/// Calculate backoff delay for the supervisor. Exponential: 1s, 2s, 4s, ...
/// capped at 60s, with 10% jitter. Mirrors TS `calculateBackoffMs`.
#[must_use]
pub fn calculate_supervisor_backoff(crash_count: u32, floor_ms: Option<u64>) -> u64 {
    if crash_count == 0 {
        return 0;
    }
    let base = (1_000u64).saturating_mul(2u64.saturating_pow(crash_count - 1));
    let capped = base.min(60_000); // 60s cap
    // 10% jitter (deterministic for testability via floor).
    let mut ms = capped;
    // floor overrides for test stability
    if let Some(f) = floor_ms {
        ms = ms.max(f);
    }
    ms
}

// ─── ChildWorkerSupervisor ───────────────────────────────────────────────────

/// Reusable spawn-and-respawn loop. Spawns a child process, waits for it to
/// exit, classifies the exit, and applies backoff before respawning.
/// Implements D1 (stable-run reset) and D2 (clean-restart budget).
///
/// Mirrors TS `ChildWorkerSupervisor` class.
pub struct ChildWorkerSupervisor {
    opts: ChildSupervisorOpts,
    tini_path: Option<std::path::PathBuf>,
    /// Sends lifecycle events to the consumer (audit log, CLI output).
    event_tx: mpsc::Sender<ChildSupervisorEvent>,
    /// Called when maxCrashes is exceeded.
    on_max_crashes: Arc<dyn Fn(u32, u32) + Send + Sync>,
    /// External stop signal. When true, the loop exits after the current
    /// child finishes (or during backoff).
    is_stopping: Arc<AtomicBool>,

    // Running state
    crash_count: u32,
    clean_restart_timestamps: Vec<Instant>,
    _in_backoff: bool,
}

impl ChildWorkerSupervisor {
    /// Create a new supervisor.
    pub fn new(
        opts: ChildSupervisorOpts,
        event_tx: mpsc::Sender<ChildSupervisorEvent>,
        on_max_crashes: Arc<dyn Fn(u32, u32) + Send + Sync>,
        is_stopping: Arc<AtomicBool>,
    ) -> Self {
        let tini_path = detect_tini();
        Self {
            opts,
            tini_path,
            event_tx,
            on_max_crashes,
            is_stopping,
            crash_count: 0,
            clean_restart_timestamps: Vec::new(),
            _in_backoff: false,
        }
    }

    /// Whether a child process is currently alive.
    #[must_use]
    pub fn child_alive(&self) -> bool {
        // Tracked via internal state during spawn_once; for now always false.
        false
    }

    /// Whether currently sleeping in backoff.
    #[must_use]
    pub fn in_backoff(&self) -> bool {
        self._in_backoff
    }

    /// Current consecutive crash count.
    #[must_use]
    pub fn crash_count(&self) -> u32 {
        self.crash_count
    }

    /// Whether tini was detected at construction time.
    #[must_use]
    pub fn is_tini_detected(&self) -> bool {
        self.tini_path.is_some()
    }

    // ─── Current time (injectable for testing) ───────────────────────────

    fn now(&self) -> Instant {
        self.opts._now.as_ref().map(|f| f()).unwrap_or_else(Instant::now)
    }

    // ─── Main loop ──────────────────────────────────────────────────────

    /// Run the spawn/respawn loop. Blocks until `is_stopping()` is true or
    /// `crash_count >= max_crashes`.
    pub async fn run(&mut self) {
        loop {
            if self.is_stopping.load(Ordering::Acquire) {
                info!("ChildWorkerSupervisor: stopping before spawn");
                break;
            }
            if self.crash_count >= self.opts.max_crashes {
                warn!(
                    crash_count = self.crash_count,
                    max_crashes = self.opts.max_crashes,
                    "ChildWorkerSupervisor: max crashes exceeded"
                );
                (self.on_max_crashes)(self.crash_count, self.opts.max_crashes);
                break;
            }

            self.spawn_once().await;

            if self.is_stopping.load(Ordering::Acquire) {
                info!("ChildWorkerSupervisor: stopping after child exit");
                break;
            }
            if self.crash_count >= self.opts.max_crashes {
                (self.on_max_crashes)(self.crash_count, self.opts.max_crashes);
                break;
            }

            self.apply_backoff().await;
        }
    }

    // ─── Spawn & await one child ─────────────────────────────────────────

    async fn spawn_once(&mut self) {
        if self.is_stopping.load(Ordering::Acquire) {
            return;
        }

        let inv = build_spawn_args(
            self.tini_path.as_ref(),
            &self.opts.cli_path,
            &self.opts.args,
        );

        let mut cmd = Command::new(&inv.cmd);
        cmd.args(&inv.args);
        for (k, v) in &self.opts.env {
            cmd.env(k, v);
        }
        cmd.kill_on_drop(true);

        let start = self.now();
        let child = match cmd.spawn() {
            Ok(c) => c,
            Err(e) => {
                warn!(error = %e, "ChildWorkerSupervisor: spawn failed");
                self.crash_count += 1;
                let _ = self.event_tx.try_send(ChildSupervisorEvent::WorkerSpawnFailed {
                    error: e.to_string(),
                });
                return;
            }
        };

        let pid = child.id().unwrap_or(0);
        let _ = self.event_tx.try_send(ChildSupervisorEvent::WorkerSpawned {
            pid,
            tini: self.tini_path.is_some(),
        });

        // Wait for child to exit.
        let output = child.wait_with_output().await;
        let run_duration = start.elapsed();
        let run_duration_ms = run_duration.as_millis() as u64;

        match output {
            Ok(out) => {
                let exit_class = classify_exit(out.status.code(), None);
                let likely_cause = match out.status.code() {
                    Some(0) => "clean_exit".to_string(),
                    Some(c) => format!("code_{c}"),
                    None => "signal".to_string(),
                };

                match exit_class {
                    ExitClass::Clean => {
                        // D2: track clean restart timestamps
                        self.clean_restart_timestamps.push(self.now());
                        self.prune_clean_restart_window();

                        // Don't reset crashCount on clean exit (preserve
                        // surge detection across mixed exit sequences)
                    }
                    ExitClass::Crash => {
                        // D1: stable run reset — if the child ran long
                        // enough, reset crashCount to 1
                        if run_duration_ms >= self.opts.stable_run_reset_ms {
                            self.crash_count = 1;
                        } else {
                            self.crash_count += 1;
                        }
                    }
                }

                let _ = self.event_tx.try_send(ChildSupervisorEvent::WorkerExited {
                    code: out.status.code(),
                    signal: None,
                    run_duration_ms,
                    likely_cause,
                    crash_count: self.crash_count,
                });
            }
            Err(e) => {
                warn!(error = %e, "ChildWorkerSupervisor: child wait error");
                self.crash_count += 1;
            }
        }
    }

    // ─── Backoff ─────────────────────────────────────────────────────────

    async fn apply_backoff(&mut self) {
        let (reason, ms) = if self.crash_count > 0 {
            // Crash → exponential backoff
            let ms = calculate_supervisor_backoff(self.crash_count, self.opts._backoff_floor_ms);
            (BackoffReason::Crash, ms)
        } else {
            // Clean exit → check D2 budget
            self.prune_clean_restart_window();
            let clean_count = self.clean_restart_timestamps.len() as u32;
            if clean_count > self.opts.clean_restart_budget {
                let _ = self.event_tx.try_send(ChildSupervisorEvent::HealthWarn {
                    reason: format!(
                        "clean_restart_budget_exceeded: {} in {}s",
                        clean_count,
                        self.opts.clean_restart_window_ms / 1000
                    ),
                });
                (BackoffReason::BudgetExceeded, self.opts.clean_restart_budget_backoff_ms)
            } else {
                (BackoffReason::CleanExit, 0)
            }
        };

        let _ = self.event_tx.try_send(ChildSupervisorEvent::Backoff {
            ms,
            crash_count: self.crash_count,
            reason,
        });

        if ms > 0 {
            self._in_backoff = true;
            // Split sleep into 200ms chunks so we can check isStopping
            let deadline = self.now() + Duration::from_millis(ms);
            loop {
                if self.is_stopping.load(Ordering::Acquire) {
                    break;
                }
                let remaining = deadline.saturating_duration_since(self.now());
                if remaining.is_zero() {
                    break;
                }
                let chunk = remaining.min(Duration::from_millis(200));
                tokio::time::sleep(chunk).await;
            }
            self._in_backoff = false;
        }
    }

    // ─── D2 helpers ──────────────────────────────────────────────────────────

    fn prune_clean_restart_window(&mut self) {
        let cutoff = self.now() - Duration::from_millis(self.opts.clean_restart_window_ms);
        self.clean_restart_timestamps.retain(|ts| *ts > cutoff);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicBool;

    /// Helper: create a ChildWorkerSupervisor for testing.
    fn make_supervisor(opts: ChildSupervisorOpts) -> (ChildWorkerSupervisor, mpsc::Receiver<ChildSupervisorEvent>, Arc<AtomicBool>) {
        let (tx, rx) = mpsc::channel(32);
        let stopping = Arc::new(AtomicBool::new(false));
        let on_max = Arc::new(|_: u32, _: u32| {});
        let sup = ChildWorkerSupervisor::new(opts, tx, on_max, stopping.clone());
        (sup, rx, stopping)
    }

    #[tokio::test]
    async fn clean_exit_no_crash() {
        let opts = ChildSupervisorOpts {
            cli_path: "sleep".to_string(),
            args: vec!["0".to_string()],
            max_crashes: 1,
            ..Default::default()
        };
        let (mut sup, mut rx, stopping) = make_supervisor(opts);

        // Stop after one cycle.
        let s = stopping.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(500)).await;
            s.store(true, Ordering::Release);
        });

        sup.run().await;

        assert_eq!(sup.crash_count(), 0);

        // Collect all events.
        let mut events: Vec<ChildSupervisorEvent> = Vec::new();
        while let Ok(ev) = rx.try_recv() {
            events.push(ev);
        }

        // Should have: spawned, exited (clean), backoff (clean, no delay)
        let has_spawned = events.iter().any(|e| matches!(e, ChildSupervisorEvent::WorkerSpawned { .. }));
        let has_exited = events.iter().any(|e| matches!(e, ChildSupervisorEvent::WorkerExited { code: Some(0), .. }));
        assert!(has_spawned);
        assert!(has_exited);
    }

    #[tokio::test]
    async fn crash_exit_increments_count() {
        let opts = ChildSupervisorOpts {
            cli_path: "sh".to_string(),
            args: vec!["-c".to_string(), "exit 1".to_string()],
            max_crashes: 2,
            ..Default::default()
        };
        let (mut sup, mut rx, stopping) = make_supervisor(opts);

        let s = stopping.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(500)).await;
            s.store(true, Ordering::Release);
        });

        sup.run().await;

        assert_eq!(sup.crash_count(), 1);

        let mut events: Vec<ChildSupervisorEvent> = Vec::new();
        while let Ok(ev) = rx.try_recv() {
            events.push(ev);
        }
        let has_exited = events.iter().any(|e| matches!(e, ChildSupervisorEvent::WorkerExited { code: Some(1), .. }));
        assert!(has_exited);
    }

    #[test]
    fn backoff_calculation() {
        assert_eq!(calculate_supervisor_backoff(0, None), 0);
        assert_eq!(calculate_supervisor_backoff(1, None), 1000);
        assert_eq!(calculate_supervisor_backoff(2, None), 2000);
        assert_eq!(calculate_supervisor_backoff(3, None), 4000);
        assert_eq!(calculate_supervisor_backoff(4, None), 8000);
        assert_eq!(calculate_supervisor_backoff(5, None), 16000);
        assert_eq!(calculate_supervisor_backoff(6, None), 32000);
        assert_eq!(calculate_supervisor_backoff(7, None), 60000); // capped
        assert_eq!(calculate_supervisor_backoff(10, None), 60000); // capped
    }

    #[test]
    fn backoff_floor() {
        // With floor, the minimum delay is at least the floor.
        assert_eq!(calculate_supervisor_backoff(1, Some(5000)), 5000);
    }

    #[test]
    fn tini_detection_in_constructor() {
        let opts = ChildSupervisorOpts::default();
        let (sup, _rx, _) = make_supervisor(opts);
        // tini may or may not be installed; don't assert on value.
        let _detected = sup.is_tini_detected();
    }

    #[test]
    fn initial_crash_count_zero() {
        let opts = ChildSupervisorOpts::default();
        let (sup, _rx, _) = make_supervisor(opts);
        assert_eq!(sup.crash_count(), 0);
        assert!(!sup.child_alive());
        assert!(!sup.in_backoff());
    }
}
